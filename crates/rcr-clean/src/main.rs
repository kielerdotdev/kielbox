use clap::{Parser, ValueEnum};
use rcr_disk::{
    CancelToken,
    cleanup::{self, Kind, Notice},
};
use std::{io, path::PathBuf, time::Duration};

/// Reclaim space from old developer artifacts and clean Git worktrees.
#[derive(Debug, Parser)]
#[command(
    version,
    after_help = "Examples:\n  rcr-clean C:\\src C:\\Users\\me\\.codex\\worktrees\n  rcr-clean C:\\src --apply\n  rcr-clean . --kind node-modules,rust-target --older-than 7 --apply\n\nPreview is the default. --apply permanently deletes eligible directories.\nAges use modification times, not last access. Stop agents/builds before applying.\nWorktrees must be clean, unlocked, outside the current directory, and have HEAD\ncontained in existing local remote-tracking refs. No network fetch is performed."
)]
struct Cli {
    /// Directories to search recursively (default: current directory)
    #[arg(default_value = ".")]
    roots: Vec<PathBuf>,
    /// Delete eligible targets; without this flag only preview
    #[arg(long, conflicts_with = "dry_run")]
    apply: bool,
    /// Explicitly preview without deleting (the default)
    #[arg(long)]
    dry_run: bool,
    /// Require every entry to be at least this many days old
    #[arg(long, default_value_t = 30, value_name = "DAYS", value_parser = clap::value_parser!(u64).range(1..=36500))]
    older_than: u64,
    /// Restrict cleanup kinds (comma separated; default: all)
    #[arg(long, value_enum, value_delimiter = ',')]
    kind: Vec<KindArg>,
    /// Emit a machine-readable report
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum KindArg {
    NodeModules,
    RustTarget,
    Cache,
    Worktree,
}

impl From<KindArg> for Kind {
    fn from(value: KindArg) -> Self {
        match value {
            KindArg::NodeModules => Self::NodeModules,
            KindArg::RustTarget => Self::RustTarget,
            KindArg::Cache => Self::Cache,
            KindArg::Worktree => Self::Worktree,
        }
    }
}

fn size(bytes: u64) -> String {
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    format!("{value:.1} {unit}")
}

fn notices(values: &[Notice]) -> Vec<serde_json::Value> {
    values
        .iter()
        .map(|n| serde_json::json!({"path": n.path, "reason": n.reason}))
        .collect()
}

fn run(cli: &Cli, cancel: &CancelToken) -> io::Result<bool> {
    if !cli.json {
        eprintln!("Inspecting targets older than {} days…", cli.older_than);
    }
    let plan = cleanup::plan(
        &cleanup::Options {
            roots: cli.roots.clone(),
            older_than: Duration::from_secs(cli.older_than * 86400),
            kinds: cli.kind.iter().copied().map(Into::into).collect(),
        },
        cancel,
    )?;
    if !cli.json {
        for c in plan.candidates() {
            println!(
                "{:>12}  {:<12}  {}",
                size(c.bytes),
                c.kind.label(),
                c.path.display()
            );
        }
        for n in &plan.skipped {
            eprintln!("Preserved {}: {}", n.path.display(), n.reason);
        }
        for n in &plan.errors {
            eprintln!("Error {}: {}", n.path.display(), n.reason);
        }
        println!(
            "{} eligible directories, {} logical bytes.",
            plan.candidates().len(),
            size(plan.bytes())
        );
    }
    let outcome = if cli.apply {
        Some(cleanup::apply(&plan, cancel, |candidate| {
            if !cli.json {
                println!("Removed {}", candidate.path.display());
            }
        })?)
    } else {
        None
    };
    let success = plan.errors.is_empty() && outcome.as_ref().is_none_or(|o| o.errors.is_empty());
    if cli.json {
        let candidates: Vec<_> = plan
            .candidates()
            .iter()
            .map(|c| {
                serde_json::json!({
                    "path": c.path, "kind": c.kind.label(), "bytes": c.bytes,
                })
            })
            .collect();
        let removed: Vec<_> = outcome
            .as_ref()
            .map(|o| {
                o.removed
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "path": c.path, "kind": c.kind.label(), "bytes": c.bytes,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "applied": cli.apply, "success": success, "candidates": candidates,
                "logical_bytes": plan.bytes(), "skipped": notices(&plan.skipped),
                "scan_errors": notices(&plan.errors), "removed": removed,
                "remove_errors": outcome.as_ref().map(|o| notices(&o.errors)).unwrap_or_default(),
            }))?
        );
    } else if let Some(outcome) = outcome {
        for n in outcome.errors {
            eprintln!("Error {}: {}", n.path.display(), n.reason);
        }
        println!(
            "Removed {} directories ({} logical bytes).",
            outcome.removed.len(),
            size(outcome.removed.iter().map(|c| c.bytes).sum())
        );
    } else {
        println!("Preview only. Add --apply to permanently remove these directories.");
    }
    Ok(success)
}

fn main() {
    let cli = Cli::parse();
    let cancel = CancelToken::new();
    let handler = cancel.clone();
    if let Err(e) = ctrlc::set_handler(move || handler.cancel()) {
        eprintln!("error: could not install cancellation handler: {e}");
        std::process::exit(1);
    }
    let code = match run(&cli, &cancel) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({"success": false, "error": e.to_string()})
                );
            } else {
                eprintln!("error: {e}");
            }
            if e.kind() == io::ErrorKind::Interrupted {
                3
            } else {
                1
            }
        }
    };
    std::process::exit(code);
}
