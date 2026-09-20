# tfplanshow

Renders `terraform show -json` plan output as a clean, grouped summary,
with replacements — the destroy-and-recreate changes that are usually
the ones that actually hurt — called out first and separately from a
plain create/update/delete. `terraform plan`'s own text output already
shows this per-resource, but scanning a real plan with 80 resources for
the handful of `-/+` replacements buried among routine `~` updates is
exactly the kind of thing worth a dedicated view for, especially in a
CI job's log output where the full plan text scrolls by unread.

## Usage

```bash
terraform show -json tfplan.binary > plan.json   # produced by terraform itself
tfplanshow plan.json
tfplanshow plan.json --show-noop                 # also list untouched/data-source reads
tfplanshow plan.json --json                      # machine-readable
tfplanshow plan.json --fail-on-replace           # exit 1 if anything would be destroyed and recreated
tfplanshow plan.json --fail-on-destroy           # exit 1 on any delete, including replacements
```

```
$ tfplanshow plan.json

REPLACEMENTS (destroy and recreate) — 2
  ~ aws_instance.api  (destroy then create)
      forced by: ami
  ~ aws_launch_template.app  (create then destroy)
      forced by: name

DELETE — 1
  - aws_s3_bucket.legacy_assets

CREATE — 1
  + aws_instance.web

UPDATE — 1
  ~ aws_db_instance.primary  (changed: engine_version)

Summary: 1 to add, 1 to change, 1 to destroy, 2 to replace
```

## Input: Terraform's own documented JSON plan format

This reads `resource_changes[]` from the plan representation documented
at
[developer.hashicorp.com/terraform/internals/json-format](https://developer.hashicorp.com/terraform/internals/json-format) —
specifically each entry's `address`, `type`, and `change.actions`. Every
change in a real plan is one of five action-array shapes, and this
tool's whole categorization is just naming them:

| `actions` | Shown as |
|---|---|
| `["create"]` | CREATE |
| `["update"]` | UPDATE |
| `["delete"]` | DELETE |
| `["delete","create"]` | REPLACEMENT — destroy then create (the default order) |
| `["create","delete"]` | REPLACEMENT — create then destroy (`create_before_destroy = true`) |
| `["read"]` | data source read, hidden unless `--show-noop` |
| `["no-op"]` | unchanged, hidden unless `--show-noop` |

Anything outside those six shapes lands in its own `UNRECOGNIZED
ACTIONS` section with the raw actions array printed rather than being
silently mis-bucketed — a defensive fallback in case a future Terraform
version adds a verb this tool doesn't know about yet, not something
observed against a real plan.

When a replacement's `change.replace_paths` is present (Terraform's own
documented field naming exactly which attribute paths forced the
replace), it's rendered as `forced by: <path>` using the same dotted/
bracket notation you'd write by hand — `tags.Name`, `ingress[0].
from_port`. For `UPDATE`, since there's no equivalent field, this does
its own shallow top-level diff of `before`/`after` and lists which
top-level attribute names differ (added, removed, or changed) — not a
deep diff, just enough to say "engine_version changed" without also
repeating every attribute that stayed the same.

## Status: built, and verified against a hand-built realistic plan fixture with all five real action shapes present at once

- **18 unit tests** (`cargo test --lib`): every one of the five
  documented action shapes categorized correctly, including telling
  `["delete","create"]` apart from `["create","delete"]` — the actual
  distinction that decides whether a replacement is destroy-then-create
  or create-then-destroy; an unrecognized actions combination (and the
  edge case of an empty actions array) correctly falling into `Unknown`
  rather than being silently miscategorized as something else;
  `replace_paths` rendering for a bare top-level attribute, a nested
  object path, and an array-index path (`ingress[0].from_port`); the
  shallow update diff finding only the genuinely differing top-level
  keys (and catching keys added or removed entirely, not just changed);
  a full realistic 7-resource plan (one of each action shape) parsed
  and bucketed with the right count in every bucket; and the rendered
  text output specifically asserting **replacements appear before
  deletes, which appear before creates** (not just that the right
  strings are present somewhere), that no-op/read resources are hidden
  by default but appear under `--show-noop`, and that the summary
  line's four counts are each individually correct.
- **Live-verified against a hand-built plan JSON fixture** shaped like
  real `terraform show -json` output (including realistic noise fields
  this tool doesn't use — `after_unknown`, `before_sensitive`,
  `module_address`, `provider_name` — to confirm they're silently
  ignored rather than causing a parse failure), run through the actual
  built `tfplanshow` binary: 7 resources mixing all five shapes —
  `aws_instance.web` created, `aws_db_instance.primary` updated (only
  `engine_version` actually changed, out of four attributes present —
  confirmed the other three unchanged ones were correctly *not* listed),
  `aws_s3_bucket.legacy_assets` deleted, `aws_instance.api` replaced
  destroy-then-create with `replace_paths` correctly rendering `forced
  by: ami`, `aws_launch_template.app` replaced create-then-destroy with
  `forced by: name`, plus a `read` data source and a `no-op` role. The
  real rendered output put both replacements first under their own
  section (one correctly labeled "destroy then create", the other
  "create then destroy"), then delete, then create, then update, with a
  correct `Summary: 1 to add, 1 to change, 1 to destroy, 2 to replace`
  line — and the read/no-op resources were absent from the default
  output and present under `--show-noop`.
- **CI-gating flags verified live**: `--fail-on-replace` and
  `--fail-on-destroy` both exited `1` against that same fixture (which
  has both), and a second, separate clean fixture with only a `create`
  exited `0` against both flags — confirming the gate is keyed to
  actual plan content, not just always tripping.
- Both fixture files were scratch JSON, deleted after the run.

**Not done / deliberately deferred**: does not shell out to `terraform`
itself (`terraform show -json` has to be run separately and piped/saved
to a file first) — this only ever reads plan JSON that already exists,
on purpose, so it never needs a Terraform install, provider
credentials, or `.terraform` state lock in this sandbox or anyone
else's CI; only a *shallow* diff of updated attributes — the update
diff correctly notices when a nested value changes (comparing `tags`
as a whole `Value` catches an edit anywhere inside it, since the two
maps simply won't be equal), it just reports "`tags` changed" rather
than drilling in to say which specific key inside that map moved; and
no notion of resources inside `module_address`-qualified child modules
being grouped or labeled any differently from root-module ones — they're
diffed and shown exactly the same way, just under their own (already
module-qualified) `address` string.
