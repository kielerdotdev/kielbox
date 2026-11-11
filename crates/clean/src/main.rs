use clap::{Parser, ValueEnum};
use disk::{
    CancelToken,
    cleanup::{self, Kind, Notice},
};
use std::{
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};

/// Reclaim space from old developer artifacts and clean Git worktrees.
#[derive(Debug, Parser)]
#[command(
    version,
    after_help = "Examples:\n  clean C:\\src C:\\Users\\me\\.codex\\worktrees\n  clean C:\\src --apply\n  clean . --kind node-modules,rust-target --older-than 7 --apply\n\nPreview is the default. --apply permanently deletes eligible directories.\nAges use modification times, not last access. Stop agents/builds before applying.\nWorktrees must be clean, unlocked, outside the current directory, and have HEAD\ncontained in existing local remote-tracking refs. No network fetch is performed.\nWorktrees containing ignored data are preserved unless --allow-ignored-worktree-data\nis supplied. This opt-in permanently deletes ignored local settings/data too."
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
    /// Allow deleting ignored worktree contents, including local settings/data
    #[arg(long)]
    allow_ignored_worktree_data: bool,
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

fn run(cli: &Cli, cancel: &CancelToken) -> io::Result<i32> {
    run_with_progress(cli, cancel, |_| {}, &mut io::stdout().lock())
}

fn run_with_progress(
    cli: &Cli,
    cancel: &CancelToken,
    mut removed: impl FnMut(&cleanup::Candidate),
    json_output: &mut impl Write,
) -> io::Result<i32> {
    if !cli.json {
        eprintln!("Inspecting targets older than {} days…", cli.older_than);
    }
    let plan = cleanup::plan(
        &cleanup::Options {
            roots: cli.roots.clone(),
            allow_ignored_worktree_data: cli.allow_ignored_worktree_data,
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
            removed(candidate);
            if !cli.json {
                println!("Removed {}", candidate.path.display());
            }
        })?)
    } else {
        None
    };
    let cancelled = outcome.as_ref().is_some_and(|o| o.cancelled);
    let success = !cancelled
        && plan.errors.is_empty()
        && outcome.as_ref().is_none_or(|o| o.errors.is_empty());
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
        writeln!(
            json_output,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "applied": cli.apply, "success": success, "cancelled": cancelled, "candidates": candidates,
                "logical_bytes": plan.bytes(), "skipped": notices(&plan.skipped),
                "scan_errors": notices(&plan.errors), "removed": removed,
                "remove_errors": outcome.as_ref().map(|o| notices(&o.errors)).unwrap_or_default(),
            }))?
        )?;
    } else if let Some(outcome) = outcome {
        if outcome.cancelled {
            eprintln!("Cleanup cancelled; completed removals are listed below.");
        }
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
    Ok(if cancelled {
        3
    } else if success {
        0
    } else {
        1
    })
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
        Ok(code) => code,
        Err(e) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "success": false, "error": e.to_string(),
                        "cancelled": e.kind() == io::ErrorKind::Interrupted,
                    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::SystemTime};

    #[test]
    fn json_cancellation_after_first_delete_retains_ledger_and_returns_exit_three() {
        let temp = tempfile::tempdir().unwrap();
        let old = filetime::FileTime::from_system_time(
            SystemTime::now() - Duration::from_secs(40 * 86400),
        );
        for name in ["a", "b"] {
            let root = temp.path().join(name);
            fs::create_dir(&root).unwrap();
            fs::write(root.join("package.json"), "{}").unwrap();
            let modules = root.join("node_modules");
            fs::create_dir(&modules).unwrap();
            let file = modules.join("fixture.js");
            fs::write(&file, "fixture").unwrap();
            filetime::set_file_mtime(&file, old).unwrap();
            filetime::set_file_mtime(&modules, old).unwrap();
        }
        let cli = Cli::parse_from([
            std::ffi::OsStr::new("clean"),
            temp.path().as_os_str(),
            std::ffi::OsStr::new("--apply"),
            std::ffi::OsStr::new("--json"),
        ]);
        let cancel = CancelToken::new();
        let mut output = Vec::new();
        let code = run_with_progress(&cli, &cancel, |_| cancel.cancel(), &mut output).unwrap();
        assert_eq!(code, 3);
        let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["success"], false);
        assert_eq!(report["cancelled"], true);
        assert_eq!(report["applied"], true);
        assert_eq!(report["removed"].as_array().unwrap().len(), 1);
        assert_eq!(report["removed"][0]["bytes"], 7);
        assert_eq!(report["remove_errors"].as_array().unwrap().len(), 0);
        let candidates = report["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(!std::path::Path::new(candidates[0]["path"].as_str().unwrap()).exists());
        assert!(std::path::Path::new(candidates[1]["path"].as_str().unwrap()).exists());
    }
}
