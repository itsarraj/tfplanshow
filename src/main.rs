use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use tfplanshow::{parse_plan, render_text, summarize};

/// Renders `terraform show -json` plan output as a grouped, readable summary.
#[derive(Parser)]
#[command(name = "tfplanshow", version, about)]
struct Cli {
    /// Path to a plan JSON file, produced by `terraform show -json <planfile> > plan.json`.
    plan: PathBuf,

    /// Also show read (data source) and no-op resources.
    #[arg(long)]
    show_noop: bool,

    /// Print machine-readable JSON instead of the text report.
    #[arg(long)]
    json: bool,

    /// Exit non-zero if the plan contains any replacements.
    #[arg(long)]
    fail_on_replace: bool,

    /// Exit non-zero if the plan contains any deletes (including replacements).
    #[arg(long)]
    fail_on_destroy: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("tfplanshow: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let text = std::fs::read_to_string(&cli.plan)
        .with_context(|| format!("reading {}", cli.plan.display()))?;
    let plan = parse_plan(&text).with_context(|| format!("parsing {}", cli.plan.display()))?;
    let summary = summarize(&plan);

    if cli.json {
        let to_item = |rc: &tfplanshow::ResourceChange| {
            serde_json::json!({
                "address": rc.address,
                "type": rc.resource_type,
            })
        };
        let out = serde_json::json!({
            "creates": summary.creates.iter().map(|rc| to_item(rc)).collect::<Vec<_>>(),
            "updates": summary.updates.iter().map(|rc| to_item(rc)).collect::<Vec<_>>(),
            "deletes": summary.deletes.iter().map(|rc| to_item(rc)).collect::<Vec<_>>(),
            "replaces": summary.replaces.iter().map(|(rc, cbd)| {
                serde_json::json!({ "address": rc.address, "type": rc.resource_type, "create_before_destroy": cbd })
            }).collect::<Vec<_>>(),
            "reads": summary.reads.iter().map(|rc| to_item(rc)).collect::<Vec<_>>(),
            "no_ops": summary.no_ops.iter().map(|rc| to_item(rc)).collect::<Vec<_>>(),
            "unknown": summary.unknown.iter().map(|(rc, actions)| {
                serde_json::json!({ "address": rc.address, "actions": actions })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", render_text(&summary, cli.show_noop));
        if !summary.unknown.is_empty() {
            eprintln!(
                "tfplanshow: warning: {} resource(s) had an actions combination this tool doesn't recognize; see UNRECOGNIZED ACTIONS above",
                summary.unknown.len()
            );
        }
    }

    let mut fail = false;
    if cli.fail_on_replace && summary.has_replacements() {
        fail = true;
    }
    if cli.fail_on_destroy && (summary.has_deletes() || summary.has_replacements()) {
        fail = true;
    }

    Ok(if fail {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
