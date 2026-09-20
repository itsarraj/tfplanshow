//! Renders `terraform show -json` plan output as a grouped, readable
//! summary. The input shape here is Terraform's own documented JSON
//! plan representation (`resource_changes[].change.actions`, an array
//! like `["create"]`, `["update"]`, `["delete"]`, or the two two-element
//! forms Terraform uses for a replace: `["delete","create"]` for the
//! default destroy-then-create order, `["create","delete"]` when
//! `create_before_destroy` is set) — see
//! <https://developer.hashicorp.com/terraform/internals/json-format>.
//! Nothing here shells out to `terraform`; it only ever reads JSON
//! already produced by `terraform show -json <planfile>`.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

// ---------------------------------------------------------------------
// The plan schema — just the fields this tool reads.
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub terraform_version: Option<String>,
    #[serde(default)]
    pub resource_changes: Vec<ResourceChange>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ResourceChange {
    pub address: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(rename = "type")]
    pub resource_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub provider_name: Option<String>,
    pub change: Change,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Change {
    pub actions: Vec<String>,
    #[serde(default)]
    pub before: Option<Value>,
    #[serde(default)]
    pub after: Option<Value>,
    /// Attribute paths that forced a replacement, e.g. `[["ami"]]` or
    /// `[["ingress", 0, "from_port"]]`. Documented but optional in real
    /// plan output — absent entirely for non-replace changes, and not
    /// every provider/Terraform version populates it even for a replace.
    #[serde(default)]
    pub replace_paths: Option<Vec<Vec<Value>>>,
}

pub fn parse_plan(json: &str) -> anyhow::Result<Plan> {
    Ok(serde_json::from_str(json)?)
}

// ---------------------------------------------------------------------
// Action categorization
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionKind {
    Create,
    Update,
    Delete,
    /// A replace. `create_before_destroy` is `true` for the
    /// `["create","delete"]` ordering, `false` for the default
    /// `["delete","create"]`.
    Replace {
        create_before_destroy: bool,
    },
    Read,
    NoOp,
    /// Any actions array that isn't one of the five documented shapes
    /// above — reported plainly rather than guessed at, since a new
    /// Terraform version could in principle add a shape this tool
    /// doesn't know about yet.
    Unknown(Vec<String>),
}

pub fn categorize(actions: &[String]) -> ActionKind {
    let as_strs: Vec<&str> = actions.iter().map(String::as_str).collect();
    match as_strs.as_slice() {
        ["create"] => ActionKind::Create,
        ["update"] => ActionKind::Update,
        ["delete"] => ActionKind::Delete,
        ["read"] => ActionKind::Read,
        ["no-op"] => ActionKind::NoOp,
        ["delete", "create"] => ActionKind::Replace {
            create_before_destroy: false,
        },
        ["create", "delete"] => ActionKind::Replace {
            create_before_destroy: true,
        },
        _ => ActionKind::Unknown(actions.to_vec()),
    }
}

/// Renders a `replace_paths`/JSON-Pointer-ish attribute path
/// (`["ingress", 0, "from_port"]`) the way a human would write it:
/// `ingress[0].from_port`.
pub fn render_path(path: &[Value]) -> String {
    let mut out = String::new();
    for seg in path {
        match seg {
            Value::Number(n) => {
                out.push('[');
                out.push_str(&n.to_string());
                out.push(']');
            }
            Value::String(s) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(s);
            }
            other => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(&other.to_string());
            }
        }
    }
    out
}

/// The top-level object keys whose value differs between `before` and
/// `after` — a shallow, address-level "what changed" hint, not a deep
/// structural diff. Good enough to say "instance_type, tags changed"
/// without trying to render a nested attribute's own internal diff.
pub fn changed_top_level_keys(before: &Value, after: &Value) -> Vec<String> {
    let mut keys = BTreeSet::new();
    if let Some(obj) = before.as_object() {
        keys.extend(obj.keys().cloned());
    }
    if let Some(obj) = after.as_object() {
        keys.extend(obj.keys().cloned());
    }
    let mut changed: Vec<String> = keys
        .into_iter()
        .filter(|k| {
            let bv = before.get(k).unwrap_or(&Value::Null);
            let av = after.get(k).unwrap_or(&Value::Null);
            bv != av
        })
        .collect();
    changed.sort();
    changed
}

// ---------------------------------------------------------------------
// Summary
// ---------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct PlanSummary<'a> {
    pub creates: Vec<&'a ResourceChange>,
    pub updates: Vec<&'a ResourceChange>,
    pub deletes: Vec<&'a ResourceChange>,
    pub replaces: Vec<(&'a ResourceChange, bool)>, // (change, create_before_destroy)
    pub reads: Vec<&'a ResourceChange>,
    pub no_ops: Vec<&'a ResourceChange>,
    pub unknown: Vec<(&'a ResourceChange, Vec<String>)>,
}

pub fn summarize(plan: &Plan) -> PlanSummary<'_> {
    let mut summary = PlanSummary::default();
    for rc in &plan.resource_changes {
        match categorize(&rc.change.actions) {
            ActionKind::Create => summary.creates.push(rc),
            ActionKind::Update => summary.updates.push(rc),
            ActionKind::Delete => summary.deletes.push(rc),
            ActionKind::Replace {
                create_before_destroy,
            } => summary.replaces.push((rc, create_before_destroy)),
            ActionKind::Read => summary.reads.push(rc),
            ActionKind::NoOp => summary.no_ops.push(rc),
            ActionKind::Unknown(actions) => summary.unknown.push((rc, actions)),
        }
    }
    summary.creates.sort_by(|a, b| a.address.cmp(&b.address));
    summary.updates.sort_by(|a, b| a.address.cmp(&b.address));
    summary.deletes.sort_by(|a, b| a.address.cmp(&b.address));
    summary
        .replaces
        .sort_by(|a, b| a.0.address.cmp(&b.0.address));
    summary.reads.sort_by(|a, b| a.address.cmp(&b.address));
    summary.no_ops.sort_by(|a, b| a.address.cmp(&b.address));
    summary
        .unknown
        .sort_by(|a, b| a.0.address.cmp(&b.0.address));
    summary
}

impl<'a> PlanSummary<'a> {
    pub fn has_replacements(&self) -> bool {
        !self.replaces.is_empty()
    }
    pub fn has_deletes(&self) -> bool {
        !self.deletes.is_empty()
    }
    pub fn total_actionable(&self) -> usize {
        self.creates.len() + self.updates.len() + self.deletes.len() + self.replaces.len()
    }
}

// ---------------------------------------------------------------------
// Text rendering
// ---------------------------------------------------------------------

fn empty_value() -> Value {
    Value::Null
}

fn describe_replace_reason(change: &Change) -> Option<String> {
    let paths = change.replace_paths.as_ref()?;
    if paths.is_empty() {
        return None;
    }
    let rendered: Vec<String> = paths.iter().map(|p| render_path(p)).collect();
    Some(rendered.join(", "))
}

pub fn render_text(summary: &PlanSummary, show_noop: bool) -> String {
    let mut out = String::new();

    if summary.has_replacements() {
        out.push_str(&format!(
            "REPLACEMENTS (destroy and recreate) — {}\n",
            summary.replaces.len()
        ));
        for (rc, create_before_destroy) in &summary.replaces {
            let order = if *create_before_destroy {
                "create then destroy"
            } else {
                "destroy then create"
            };
            out.push_str(&format!("  ~ {}  ({order})\n", rc.address));
            if let Some(reason) = describe_replace_reason(&rc.change) {
                out.push_str(&format!("      forced by: {reason}\n"));
            }
        }
        out.push('\n');
    }

    if summary.has_deletes() {
        out.push_str(&format!("DELETE — {}\n", summary.deletes.len()));
        for rc in &summary.deletes {
            out.push_str(&format!("  - {}\n", rc.address));
        }
        out.push('\n');
    }

    if !summary.creates.is_empty() {
        out.push_str(&format!("CREATE — {}\n", summary.creates.len()));
        for rc in &summary.creates {
            out.push_str(&format!("  + {}\n", rc.address));
        }
        out.push('\n');
    }

    if !summary.updates.is_empty() {
        out.push_str(&format!("UPDATE — {}\n", summary.updates.len()));
        for rc in &summary.updates {
            let empty = empty_value();
            let before = rc.change.before.as_ref().unwrap_or(&empty);
            let after = rc.change.after.as_ref().unwrap_or(&empty);
            let changed = changed_top_level_keys(before, after);
            if changed.is_empty() {
                out.push_str(&format!("  ~ {}\n", rc.address));
            } else {
                out.push_str(&format!(
                    "  ~ {}  (changed: {})\n",
                    rc.address,
                    changed.join(", ")
                ));
            }
        }
        out.push('\n');
    }

    if !summary.unknown.is_empty() {
        out.push_str(&format!(
            "UNRECOGNIZED ACTIONS — {}\n",
            summary.unknown.len()
        ));
        for (rc, actions) in &summary.unknown {
            out.push_str(&format!("  ? {}  (actions: {:?})\n", rc.address, actions));
        }
        out.push('\n');
    }

    if show_noop {
        if !summary.reads.is_empty() {
            out.push_str(&format!("READ (data sources) — {}\n", summary.reads.len()));
            for rc in &summary.reads {
                out.push_str(&format!("  = {}\n", rc.address));
            }
            out.push('\n');
        }
        if !summary.no_ops.is_empty() {
            out.push_str(&format!("NO-OP — {}\n", summary.no_ops.len()));
            for rc in &summary.no_ops {
                out.push_str(&format!("  = {}\n", rc.address));
            }
            out.push('\n');
        }
    }

    out.push_str(&format!(
        "Summary: {} to add, {} to change, {} to destroy, {} to replace\n",
        summary.creates.len(),
        summary.updates.len(),
        summary.deletes.len(),
        summary.replaces.len()
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn actions(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn categorize_single_actions() {
        assert_eq!(categorize(&actions(&["create"])), ActionKind::Create);
        assert_eq!(categorize(&actions(&["update"])), ActionKind::Update);
        assert_eq!(categorize(&actions(&["delete"])), ActionKind::Delete);
        assert_eq!(categorize(&actions(&["read"])), ActionKind::Read);
        assert_eq!(categorize(&actions(&["no-op"])), ActionKind::NoOp);
    }

    #[test]
    fn categorize_replace_destroy_then_create() {
        assert_eq!(
            categorize(&actions(&["delete", "create"])),
            ActionKind::Replace {
                create_before_destroy: false
            }
        );
    }

    #[test]
    fn categorize_replace_create_before_destroy() {
        assert_eq!(
            categorize(&actions(&["create", "delete"])),
            ActionKind::Replace {
                create_before_destroy: true
            }
        );
    }

    #[test]
    fn categorize_unrecognized_combo_is_unknown_not_a_guess() {
        assert_eq!(
            categorize(&actions(&["create", "read"])),
            ActionKind::Unknown(vec!["create".into(), "read".into()])
        );
        assert_eq!(categorize(&[]), ActionKind::Unknown(vec![]));
    }

    #[test]
    fn render_path_handles_plain_string_segments() {
        let path = vec![v("\"tags\""), v("\"Name\"")];
        assert_eq!(render_path(&path), "tags.Name");
    }

    #[test]
    fn render_path_handles_array_index_segments() {
        let path = vec![v("\"ingress\""), v("0"), v("\"from_port\"")];
        assert_eq!(render_path(&path), "ingress[0].from_port");
    }

    #[test]
    fn render_path_handles_bare_top_level_attribute() {
        let path = vec![v("\"ami\"")];
        assert_eq!(render_path(&path), "ami");
    }

    #[test]
    fn changed_top_level_keys_finds_only_the_differing_keys() {
        let before = v(r#"{"ami": "ami-1", "instance_type": "t3.micro", "tags": {"Name": "web"}}"#);
        let after = v(r#"{"ami": "ami-1", "instance_type": "t3.large", "tags": {"Name": "web"}}"#);
        assert_eq!(
            changed_top_level_keys(&before, &after),
            vec!["instance_type".to_string()]
        );
    }

    #[test]
    fn changed_top_level_keys_catches_added_and_removed_keys_too() {
        let before = v(r#"{"a": 1, "b": 2}"#);
        let after = v(r#"{"a": 1, "c": 3}"#);
        let mut changed = changed_top_level_keys(&before, &after);
        changed.sort();
        assert_eq!(changed, vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn changed_top_level_keys_is_empty_for_identical_objects() {
        let before = v(r#"{"a": 1}"#);
        let after = v(r#"{"a": 1}"#);
        assert!(changed_top_level_keys(&before, &after).is_empty());
    }

    fn sample_plan_json() -> String {
        r#"{
          "format_version": "1.2",
          "terraform_version": "1.7.0",
          "resource_changes": [
            {
              "address": "aws_instance.new_worker",
              "mode": "managed",
              "type": "aws_instance",
              "name": "new_worker",
              "provider_name": "registry.terraform.io/hashicorp/aws",
              "change": { "actions": ["create"], "before": null, "after": {"ami": "ami-2"} }
            },
            {
              "address": "aws_s3_bucket.old_logs",
              "mode": "managed",
              "type": "aws_s3_bucket",
              "name": "old_logs",
              "change": { "actions": ["delete"], "before": {"bucket": "old-logs"}, "after": null }
            },
            {
              "address": "aws_instance.app",
              "mode": "managed",
              "type": "aws_instance",
              "name": "app",
              "change": {
                "actions": ["update"],
                "before": {"instance_type": "t3.micro", "ami": "ami-1"},
                "after": {"instance_type": "t3.large", "ami": "ami-1"}
              }
            },
            {
              "address": "aws_instance.web",
              "mode": "managed",
              "type": "aws_instance",
              "name": "web",
              "change": {
                "actions": ["delete", "create"],
                "before": {"ami": "ami-1"},
                "after": {"ami": "ami-2"},
                "replace_paths": [["ami"]]
              }
            },
            {
              "address": "aws_launch_template.lt",
              "mode": "managed",
              "type": "aws_launch_template",
              "name": "lt",
              "change": {
                "actions": ["create", "delete"],
                "before": {"name": "lt-v1"},
                "after": {"name": "lt-v2"},
                "replace_paths": [["name"]]
              }
            },
            {
              "address": "data.aws_ami.ubuntu",
              "mode": "data",
              "type": "aws_ami",
              "name": "ubuntu",
              "change": { "actions": ["read"], "before": null, "after": null }
            },
            {
              "address": "aws_iam_role.unchanged",
              "mode": "managed",
              "type": "aws_iam_role",
              "name": "unchanged",
              "change": { "actions": ["no-op"], "before": {"name": "unchanged"}, "after": {"name": "unchanged"} }
            }
          ]
        }"#
        .to_string()
    }

    #[test]
    fn parses_a_realistic_plan_and_buckets_every_resource_correctly() {
        let plan = parse_plan(&sample_plan_json()).unwrap();
        assert_eq!(plan.resource_changes.len(), 7);
        let summary = summarize(&plan);

        assert_eq!(summary.creates.len(), 1);
        assert_eq!(summary.creates[0].address, "aws_instance.new_worker");

        assert_eq!(summary.deletes.len(), 1);
        assert_eq!(summary.deletes[0].address, "aws_s3_bucket.old_logs");

        assert_eq!(summary.updates.len(), 1);
        assert_eq!(summary.updates[0].address, "aws_instance.app");

        assert_eq!(summary.replaces.len(), 2);
        assert_eq!(summary.reads.len(), 1);
        assert_eq!(summary.no_ops.len(), 1);
        assert!(summary.unknown.is_empty());
    }

    #[test]
    fn distinguishes_destroy_then_create_from_create_then_destroy_in_the_same_plan() {
        let plan = parse_plan(&sample_plan_json()).unwrap();
        let summary = summarize(&plan);
        let web = summary
            .replaces
            .iter()
            .find(|(rc, _)| rc.address == "aws_instance.web")
            .unwrap();
        assert!(!web.1, "aws_instance.web should be destroy-then-create");
        let lt = summary
            .replaces
            .iter()
            .find(|(rc, _)| rc.address == "aws_launch_template.lt")
            .unwrap();
        assert!(lt.1, "aws_launch_template.lt should be create-then-destroy");
    }

    #[test]
    fn render_text_calls_out_replacements_prominently_and_before_other_sections() {
        let plan = parse_plan(&sample_plan_json()).unwrap();
        let summary = summarize(&plan);
        let text = render_text(&summary, false);

        let replace_pos = text.find("REPLACEMENTS").unwrap();
        let delete_pos = text.find("DELETE").unwrap();
        let create_pos = text.find("CREATE").unwrap();
        assert!(
            replace_pos < delete_pos,
            "replacements must be shown before deletes"
        );
        assert!(
            delete_pos < create_pos,
            "deletes must be shown before creates"
        );

        assert!(text.contains("aws_instance.web"));
        assert!(text.contains("forced by: ami"));
        assert!(text.contains("create then destroy"));
        assert!(text.contains("destroy then create"));
    }

    #[test]
    fn render_text_shows_changed_attribute_names_for_updates() {
        let plan = parse_plan(&sample_plan_json()).unwrap();
        let summary = summarize(&plan);
        let text = render_text(&summary, false);
        assert!(text.contains("aws_instance.app  (changed: instance_type)"));
    }

    #[test]
    fn render_text_hides_noop_and_reads_by_default_but_shows_them_when_asked() {
        let plan = parse_plan(&sample_plan_json()).unwrap();
        let summary = summarize(&plan);
        let hidden = render_text(&summary, false);
        assert!(!hidden.contains("data.aws_ami.ubuntu"));
        assert!(!hidden.contains("aws_iam_role.unchanged"));

        let shown = render_text(&summary, true);
        assert!(shown.contains("data.aws_ami.ubuntu"));
        assert!(shown.contains("aws_iam_role.unchanged"));
    }

    #[test]
    fn render_text_summary_line_has_correct_counts() {
        let plan = parse_plan(&sample_plan_json()).unwrap();
        let summary = summarize(&plan);
        let text = render_text(&summary, false);
        assert!(text.contains("Summary: 1 to add, 1 to change, 1 to destroy, 2 to replace"));
    }

    #[test]
    fn has_replacements_and_has_deletes_reflect_the_plan() {
        let plan = parse_plan(&sample_plan_json()).unwrap();
        let summary = summarize(&plan);
        assert!(summary.has_replacements());
        assert!(summary.has_deletes());
        assert_eq!(summary.total_actionable(), 1 + 1 + 1 + 2);
    }

    #[test]
    fn plan_with_no_replacements_or_deletes_reports_that_correctly() {
        let json = r#"{"resource_changes": [
            {"address": "aws_instance.a", "type": "aws_instance", "name": "a",
             "change": {"actions": ["create"], "before": null, "after": {}}}
        ]}"#;
        let plan = parse_plan(json).unwrap();
        let summary = summarize(&plan);
        assert!(!summary.has_replacements());
        assert!(!summary.has_deletes());
    }
}
