//! Plan first, then revalidate each target immediately before removing it.
//! Only named, regenerable artifacts and clean, remotely backed linked worktrees
//! are eligible. Roots are explicit; discovery never follows filesystem links.
use crate::{CancelToken, disk_usage};
use std::{
    collections::HashSet,
    fs::{self, Metadata},
    io,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, SystemTime},
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    NodeModules,
    RustTarget,
    Cache,
    Worktree,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::NodeModules => "node_modules",
            Self::RustTarget => "rust-target",
            Self::Cache => "cache",
            Self::Worktree => "worktree",
        }
    }
}

#[derive(Debug)]
pub struct Options {
    pub roots: Vec<PathBuf>,
    pub older_than: Duration,
    /// Empty selects every supported kind.
    pub kinds: Vec<Kind>,
}

#[derive(Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub kind: Kind,
    /// Logical bytes, not a promise of physical disk space reclaimed.
    pub bytes: u64,
}

#[derive(Debug)]
pub struct Notice {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug)]
pub struct Plan {
    candidates: Vec<Candidate>,
    roots: Vec<PathBuf>,
    cutoff: SystemTime,
    pub skipped: Vec<Notice>,
    pub errors: Vec<Notice>,
}

impl Plan {
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    pub fn bytes(&self) -> u64 {
        self.candidates.iter().map(|c| c.bytes).sum()
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub removed: Vec<Candidate>,
    pub errors: Vec<Notice>,
}

fn error(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn check_cancel(cancel: &CancelToken) -> io::Result<()> {
    if cancel.is_cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "cleanup cancelled",
        ))
    } else {
        Ok(())
    }
}

fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Also skip offline/recall files so cleanup does not hydrate cloud data.
        if metadata.file_attributes() & (0x400 | 0x1000 | 0x40000 | 0x400000) != 0 {
            return true;
        }
    }
    false
}

fn is_offline(_metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        _metadata.file_attributes() & (0x1000 | 0x40000 | 0x400000) != 0
    }
    #[cfg(not(windows))]
    false
}

/// Check every component, including the root, to reject junctions and link escapes.
fn checked_path(path: &Path) -> io::Result<PathBuf> {
    for ancestor in path.ancestors() {
        if is_link(&fs::symlink_metadata(ancestor)?) {
            return Err(error("links, junctions and offline paths are excluded"));
        }
    }
    fs::canonicalize(path)
}

fn git(path: &Path, args: &[&str]) -> io::Result<Output> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !output.status.success() {
        return Err(error(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output)
}

fn git_path(path: &Path, arg: &str) -> io::Result<PathBuf> {
    let output = git(path, &["rev-parse", "--path-format=absolute", arg])?;
    let value = std::str::from_utf8(&output.stdout)
        .map_err(|_| error("Git returned a non-UTF-8 path; preserving worktree"))?
        .trim_end_matches(['\r', '\n']);
    checked_path(Path::new(value))
}

fn worktree_admin(path: &Path) -> io::Result<PathBuf> {
    if is_link(&fs::symlink_metadata(path.join(".git"))?) {
        return Err(error("linked .git paths are excluded"));
    }
    let admin = git_path(path, "--absolute-git-dir")?;
    let common = git_path(path, "--git-common-dir")?;
    if admin == common || !admin.join("commondir").is_file() {
        return Err(error("primary repositories are never removed"));
    }
    if admin.join("locked").exists() {
        return Err(error("worktree is locked"));
    }
    Ok(admin)
}

fn worktree_safe(path: &Path) -> io::Result<PathBuf> {
    let admin = worktree_admin(path)?;
    if !git(
        path,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?
    .stdout
    .is_empty()
    {
        return Err(error("worktree has modified or untracked files"));
    }
    // No network fetch: local remote-tracking refs must already contain HEAD.
    // This also preserves detached commits and repositories without remotes.
    if !git(
        path,
        &["rev-list", "--max-count=1", "HEAD", "--not", "--remotes"],
    )?
    .stdout
    .is_empty()
    {
        return Err(error(
            "worktree has commits absent from local remote-tracking refs",
        ));
    }
    if path.join(".gitmodules").exists() {
        return Err(error("worktrees containing submodules are preserved"));
    }
    Ok(admin)
}

fn classify(path: &Path) -> Option<Kind> {
    let parent = path.parent()?;
    match path.file_name()?.to_str()? {
        "node_modules" if parent.join("package.json").is_file() => Some(Kind::NodeModules),
        "target" if parent.join("Cargo.toml").is_file() => Some(Kind::RustTarget),
        ".next" | ".nuxt" | ".svelte-kit" if parent.join("package.json").is_file() => {
            Some(Kind::Cache)
        }
        "__pycache__" => Some(Kind::Cache),
        ".pytest_cache" | ".mypy_cache" | ".ruff_cache"
            if ["pyproject.toml", "setup.py", "requirements.txt"]
                .iter()
                .any(|name| parent.join(name).is_file()) =>
        {
            Some(Kind::Cache)
        }
        _ if path.join(".git").is_file() => Some(Kind::Worktree),
        _ => None,
    }
}

fn measure(path: &Path, cutoff: SystemTime, cancel: &CancelToken) -> io::Result<u64> {
    // The scanner skips links. Their own timestamps still count toward freshness;
    // offline/recall entries are rejected rather than downloaded or deleted.
    let mut pending = vec![path.to_owned()];
    while let Some(entry) = pending.pop() {
        check_cancel(cancel)?;
        let metadata = fs::symlink_metadata(&entry)?;
        if metadata.modified()? > cutoff {
            return Err(error("directory contains recently modified entries"));
        }
        if is_offline(&metadata) {
            return Err(error("directory contains an offline/recall entry"));
        }
        // Internal package links (including pnpm) are unlinked by remove_dir_all,
        // never traversed or measured as part of this directory.
        if is_link(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            for child in fs::read_dir(&entry)? {
                pending.push(child?.path());
            }
        }
    }
    let tree = disk_usage::scan_directories(path, cancel, |_| {})?;
    check_cancel(cancel)?;
    if tree.nodes[0].errors != 0 {
        return Err(error("could not inspect every entry; preserving directory"));
    }
    Ok(tree.nodes[0].bytes)
}

fn validate(
    path: &Path,
    kind: Kind,
    roots: &[PathBuf],
    cutoff: SystemTime,
    cancel: &CancelToken,
) -> io::Result<u64> {
    check_cancel(cancel)?;
    let canonical = checked_path(path)?;
    if canonical != path || !roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(error("target changed or escaped the selected roots"));
    }
    if fs::canonicalize(std::env::current_dir()?)?.starts_with(&canonical) {
        return Err(error(
            "target contains the running command's working directory",
        ));
    }
    if classify(path) != Some(kind) {
        return Err(error("target is no longer a recognized cleanup directory"));
    }
    if kind == Kind::Worktree {
        let admin = worktree_safe(path)?;
        // Reflog/HEAD/index changes indicate Git activity even if source mtimes are old.
        measure(&admin, cutoff, cancel)?;
    }
    measure(path, cutoff, cancel)
}

pub fn plan(options: &Options, cancel: &CancelToken) -> io::Result<Plan> {
    if options.roots.is_empty() {
        return Err(error("at least one scan root is required"));
    }
    let cutoff = SystemTime::now()
        .checked_sub(options.older_than)
        .ok_or_else(|| error("age is too large"))?;
    let mut roots = Vec::new();
    for root in &options.roots {
        let absolute = if root.is_absolute() {
            root.clone()
        } else {
            std::env::current_dir()?.join(root)
        };
        let canonical = checked_path(&absolute)?;
        if !canonical.is_dir() {
            return Err(error(format!("{} is not a directory", root.display())));
        }
        if !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }
    let mut result = Plan {
        candidates: Vec::new(),
        roots,
        cutoff,
        skipped: Vec::new(),
        errors: Vec::new(),
    };
    let mut seen = HashSet::new();
    // Broad roots first, so overlapping explicit roots cannot produce nested deletions.
    let mut pending = result.roots.clone();
    pending.sort_by_key(|root| std::cmp::Reverse(root.components().count()));
    while let Some(path) = pending.pop() {
        check_cancel(cancel)?;
        if path
            .components()
            .any(|component| component.as_os_str() == ".git")
        {
            continue;
        }
        if !seen.insert(path.clone()) || result.candidates.iter().any(|c| path.starts_with(&c.path))
        {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                result.errors.push(Notice {
                    path,
                    reason: e.to_string(),
                });
                continue;
            }
        };
        if is_link(&metadata) || !metadata.is_dir() {
            continue;
        }
        if let Some(kind) = classify(&path) {
            if options.kinds.is_empty() || options.kinds.contains(&kind) {
                match validate(&path, kind, &result.roots, cutoff, cancel) {
                    Ok(bytes) => result.candidates.push(Candidate { path, kind, bytes }),
                    Err(e) => result.skipped.push(Notice {
                        path,
                        reason: e.to_string(),
                    }),
                }
                // Never descend into a recognized target, including protected worktrees.
                continue;
            }
            if kind != Kind::Worktree {
                continue;
            }
        }
        match fs::read_dir(&path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) if entry.file_name() != ".git" => pending.push(entry.path()),
                        Ok(_) => {}
                        Err(e) => result.errors.push(Notice {
                            path: path.clone(),
                            reason: e.to_string(),
                        }),
                    }
                }
            }
            Err(e) => result.errors.push(Notice {
                path,
                reason: e.to_string(),
            }),
        }
    }
    result
        .candidates
        .sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    Ok(result)
}

/// Apply only a plan produced by this module. Recheck scope, type, age and Git
/// state for every target. No force removal, fetch, branch deletion or pruning.
pub fn apply(
    plan: &Plan,
    cancel: &CancelToken,
    mut removed: impl FnMut(&Candidate),
) -> io::Result<Outcome> {
    let mut outcome = Outcome::default();
    for candidate in &plan.candidates {
        check_cancel(cancel)?;
        let result = (|| {
            let bytes = validate(
                &candidate.path,
                candidate.kind,
                &plan.roots,
                plan.cutoff,
                cancel,
            )?;
            check_cancel(cancel)?;
            if candidate.kind == Kind::Worktree {
                // Run from the common Git directory, outside the worktree being
                // removed (particularly important for Windows directory handles).
                let common = git_path(&candidate.path, "--git-common-dir")?;
                let output = Command::new("git")
                    .arg("--no-optional-locks")
                    .arg("-C")
                    .arg(common)
                    .args(["worktree", "remove", "--"])
                    .arg(&candidate.path)
                    .env("GIT_TERMINAL_PROMPT", "0")
                    .output()?;
                if !output.status.success() {
                    return Err(error(
                        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                    ));
                }
            } else {
                fs::remove_dir_all(&candidate.path)?;
            }
            Ok::<_, io::Error>(bytes)
        })();
        match result {
            Ok(bytes) => {
                outcome.removed.push(Candidate {
                    path: candidate.path.clone(),
                    kind: candidate.kind,
                    bytes,
                });
                removed(outcome.removed.last().unwrap());
            }
            Err(e) => outcome.errors.push(Notice {
                path: candidate.path.clone(),
                reason: e.to_string(),
            }),
        }
    }
    Ok(outcome)
}
