//! Cancellable directory-entry scanner. Sizes are logical file lengths, not allocated
//! disk space: hard links count once per entry, and sparse/compressed files can differ.
use workflows::workflow::CancelToken;
use std::{
    ffi::OsString,
    fs::{self, Metadata},
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[cfg(windows)]
#[path = "disk_usage/ntfs.rs"]
mod ntfs;
#[cfg(windows)]
#[path = "disk_usage/windows_directory.rs"]
mod windows_directory;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScanMethod {
    #[default]
    Directory,
    NtfsMft,
}
impl ScanMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::NtfsMft => "NTFS MFT",
            Self::Directory if cfg!(windows) => "Windows directory batches",
            Self::Directory => "Directory traversal",
        }
    }
}

#[derive(Debug)]
pub struct DiskNode {
    pub name: OsString,
    pub parent: Option<usize>,
    /// Child indices, largest logical size first after scanning.
    pub children: Vec<usize>,
    pub is_directory: bool,
    pub bytes: u64,
    pub files: u64,
    /// Descendant directories, excluding this node itself.
    pub directories: u64,
    pub skipped: u64,
    pub errors: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScanProgress {
    pub method: ScanMethod,
    pub records: u64,
    pub total_records: u64,
    pub stage: &'static str,
    pub bytes: u64,
    pub files: u64,
    pub directories: u64,
    pub skipped: u64,
    pub errors: u64,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug)]
pub struct DiskUsage {
    pub method: ScanMethod,
    pub fallback_reason: Option<String>,
    pub root_path: PathBuf,
    /// Root is index zero. Parents always precede their children.
    pub nodes: Vec<DiskNode>,
    pub cancelled: bool,
    pub elapsed: Duration,
    /// First 32 failures; the root node retains the complete failure count.
    pub errors: Vec<ScanError>,
}

impl DiskUsage {
    /// Reconstruct paths only on demand rather than retaining a full path per file.
    pub fn path(&self, mut id: usize) -> PathBuf {
        let mut names = Vec::new();
        while let Some(parent) = self.nodes[id].parent {
            names.push(self.nodes[id].name.as_os_str());
            id = parent;
        }
        let mut path = self.root_path.clone();
        for name in names.into_iter().rev() {
            path.push(name);
        }
        path
    }

    fn failure(&mut self, parent: usize, path: PathBuf, error: io::Error) {
        self.nodes[parent].errors += 1;
        if self.errors.len() < 32 {
            self.errors.push(ScanError {
                path,
                message: error.to_string(),
            });
        }
    }
}

fn skip_metadata(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Reparse points (including junctions) and offline/recall files must not
        // recurse or trigger cloud downloads just to measure a folder.
        const SKIP_ATTRIBUTES: u32 = 0x400 | 0x1000 | 0x40000 | 0x400000;
        if metadata.file_attributes() & SKIP_ATTRIBUTES != 0 {
            return true;
        }
    }
    !metadata.is_dir() && !metadata.is_file()
}

/// Scan on a background thread. Progress is emitted initially, at most every
/// 100 ms while traversing, and once after completion. Cancellation returns the
/// partial tree; unreadable descendants are counted and do not abort siblings.
///
/// This portable traversal never intentionally follows links. As with ordinary
/// filesystem tools, concurrent changes can produce an incomplete snapshot.
pub fn scan(
    root: impl AsRef<Path>,
    cancel: &CancelToken,
    mut progress: impl FnMut(ScanProgress),
) -> io::Result<DiskUsage> {
    let started = Instant::now();
    let root = root.as_ref();
    #[cfg(windows)]
    let fallback_reason = if cancel.is_cancelled() {
        None
    } else {
        match ntfs::scan(root, cancel, &mut progress) {
            Ok(result) => return Ok(result),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => (root.has_root()
                && root.parent().is_none())
            .then(|| format!("NTFS fast scan unavailable: {error}")),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                Some("Run as administrator for fast NTFS volume scans".to_owned())
            }
            Err(error) => Some(format!("NTFS fast scan unavailable: {error}")),
        }
    };
    let mut result = scan_directories(root, cancel, &mut progress)?;
    result.elapsed = started.elapsed();
    #[cfg(windows)]
    {
        result.fallback_reason = fallback_reason;
    }
    Ok(result)
}

/// Directory-only mode is also useful for measuring the fallback independently.
pub fn scan_directories(
    root: impl AsRef<Path>,
    cancel: &CancelToken,
    mut progress: impl FnMut(ScanProgress),
) -> io::Result<DiskUsage> {
    let started = Instant::now();
    let root = root.as_ref();
    let root_path = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()?.join(root)
    };
    let metadata = fs::symlink_metadata(&root_path)?;
    if skip_metadata(&metadata) || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose a directory, not a file, link or offline folder.",
        ));
    }
    let mut result = DiskUsage {
        root_path,
        method: ScanMethod::Directory,
        fallback_reason: None,
        nodes: vec![DiskNode {
            name: root.as_os_str().to_owned(),
            parent: None,
            children: Vec::new(),
            is_directory: true,
            bytes: 0,
            files: 0,
            directories: 0,
            skipped: 0,
            errors: 0,
        }],
        cancelled: false,
        elapsed: Duration::ZERO,
        errors: Vec::new(),
    };
    let mut totals = ScanProgress {
        stage: "Scanning folders",
        ..Default::default()
    };
    progress(totals);
    let mut last_update = started;
    let mut pending = vec![0];
    while let Some(parent) = pending.pop() {
        if cancel.is_cancelled() {
            break;
        }
        let path = result.path(parent);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if skip_metadata(&metadata) || !metadata.is_dir() => {
                result.nodes[parent].skipped += 1;
                totals.skipped += 1;
                continue;
            }
            Err(error) => {
                result.failure(parent, path, error);
                totals.errors += 1;
                continue;
            }
            Ok(_) => {}
        }
        let entries = visit_directory(&path, cancel, |entry| {
            if last_update.elapsed() >= Duration::from_millis(100) {
                totals.elapsed = started.elapsed();
                progress(totals);
                last_update = Instant::now();
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    result.failure(parent, path.clone(), error);
                    totals.errors += 1;
                    return;
                }
            };
            if entry.skip {
                result.nodes[parent].skipped += 1;
                totals.skipped += 1;
                return;
            }
            let id = result.nodes.len();
            result.nodes.push(DiskNode {
                name: entry.name,
                parent: Some(parent),
                children: Vec::new(),
                is_directory: entry.directory,
                bytes: entry.bytes,
                files: u64::from(!entry.directory),
                directories: 0,
                skipped: 0,
                errors: 0,
            });
            result.nodes[parent].children.push(id);
            if entry.directory {
                pending.push(id);
                totals.directories += 1;
            } else {
                totals.files += 1;
                totals.bytes = totals.bytes.saturating_add(entry.bytes);
            }
        });
        if let Err(error) = entries
            && (error.kind() != io::ErrorKind::Interrupted || !cancel.is_cancelled())
        {
            result.failure(parent, path, error);
            totals.errors += 1;
        }
    }
    finish(&mut result, cancel);
    result.cancelled = cancel.is_cancelled();
    result.elapsed = started.elapsed();
    totals.elapsed = result.elapsed;
    totals.stage = "Complete";
    progress(totals);
    Ok(result)
}

struct Entry {
    name: OsString,
    directory: bool,
    bytes: u64,
    skip: bool,
}
fn visit_directory(
    path: &Path,
    cancel: &CancelToken,
    mut visit: impl FnMut(io::Result<Entry>),
) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_directory::read(path, cancel, |entry| {
            visit(Ok(Entry {
                name: entry.name,
                directory: entry.is_directory,
                bytes: if entry.is_directory { 0 } else { entry.bytes },
                skip: entry.attributes & (0x400 | 0x1000 | 0x40000 | 0x400000) != 0,
            }))
        })
    }
    #[cfg(not(windows))]
    {
        for entry in fs::read_dir(path)? {
            if cancel.is_cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            visit(entry.and_then(|entry| {
                let metadata = fs::symlink_metadata(entry.path())?;
                Ok(Entry {
                    name: entry.file_name(),
                    directory: metadata.is_dir(),
                    bytes: if metadata.is_dir() { 0 } else { metadata.len() },
                    skip: skip_metadata(&metadata),
                })
            }));
        }
        Ok(())
    }
}

fn finish(result: &mut DiskUsage, cancel: &CancelToken) {
    for id in (1..result.nodes.len()).rev() {
        let (earlier, child) = result.nodes.split_at_mut(id);
        let node = &child[0];
        let parent = &mut earlier[node.parent.expect("non-root node")];
        parent.bytes = parent.bytes.saturating_add(node.bytes);
        parent.files += node.files;
        parent.directories += node.directories + u64::from(node.is_directory);
        parent.skipped += node.skipped;
        parent.errors += node.errors;
    }
    for id in 0..result.nodes.len() {
        if cancel.is_cancelled() {
            break;
        }
        let mut children = std::mem::take(&mut result.nodes[id].children);
        children.sort_unstable_by(|&a, &b| {
            result.nodes[b]
                .bytes
                .cmp(&result.nodes[a].bytes)
                .then_with(|| result.nodes[a].name.cmp(&result.nodes[b].name))
        });
        result.nodes[id].children = children;
    }
    result.cancelled |= cancel.is_cancelled();
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "disk-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn aggregates_nested_files_and_preserves_paths() {
        let temp = Temp::new();
        fs::create_dir_all(temp.0.join("nested/empty")).unwrap();
        fs::write(temp.0.join("top"), [0; 7]).unwrap();
        fs::write(temp.0.join("nested/file"), [0; 13]).unwrap();
        let mut updates = Vec::new();
        let tree = scan(&temp.0, &CancelToken::new(), |p| updates.push(p)).unwrap();
        let root = &tree.nodes[0];
        assert_eq!(
            (root.bytes, root.files, root.directories, root.errors),
            (20, 2, 2, 0)
        );
        assert!(!tree.cancelled);
        let nested = tree.nodes.iter().position(|n| n.name == "nested").unwrap();
        assert_eq!(tree.nodes[nested].bytes, 13);
        assert_eq!(tree.path(nested), temp.0.join("nested"));
        assert_eq!(updates.last().unwrap().bytes, 20);
        assert_eq!(tree.nodes[root.children[0]].name, "nested");
    }

    #[test]
    fn cancellation_returns_partial_tree_and_final_progress() {
        let temp = Temp::new();
        fs::write(temp.0.join("file"), [0; 10]).unwrap();
        let cancel = CancelToken::new();
        let mut calls = 0;
        let tree = scan(&temp.0, &cancel, |_| {
            calls += 1;
            cancel.cancel();
        })
        .unwrap();
        assert!(tree.cancelled);
        assert_eq!(tree.nodes.len(), 1);
        assert_eq!(calls, 2);
    }

    #[test]
    fn rejects_files_and_missing_roots() {
        let temp = Temp::new();
        fs::write(temp.0.join("file"), []).unwrap();
        assert_eq!(
            scan(temp.0.join("file"), &CancelToken::new(), |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(scan(temp.0.join("missing"), &CancelToken::new(), |_| {}).is_err());
    }

    #[test]
    fn hardlinks_count_logical_directory_entries() {
        let temp = Temp::new();
        fs::write(temp.0.join("original"), [0; 17]).unwrap();
        fs::hard_link(temp.0.join("original"), temp.0.join("alias")).unwrap();
        let tree = scan(&temp.0, &CancelToken::new(), |_| {}).unwrap();
        assert_eq!((tree.nodes[0].bytes, tree.nodes[0].files), (34, 2));
    }

    #[test]
    fn disappearing_directory_is_reported_without_panicking() {
        let temp = Temp::new();
        let mut first = true;
        let tree = scan(&temp.0, &CancelToken::new(), |_| {
            if first {
                first = false;
                fs::remove_dir(&temp.0).unwrap();
            }
        })
        .unwrap();
        assert_eq!(tree.nodes[0].errors, 1);
        assert_eq!(tree.errors.len(), 1);
        assert!(!tree.cancelled);
    }

    #[cfg(windows)]
    #[test]
    fn skips_windows_directory_symlinks_when_creation_is_permitted() {
        let temp = Temp::new();
        fs::write(temp.0.join("file"), [0; 11]).unwrap();
        match std::os::windows::fs::symlink_dir(&temp.0, temp.0.join("cycle")) {
            Ok(()) => {}
            // Windows requires Developer Mode or the symlink privilege.
            Err(error)
                if error.kind() == io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(1314) =>
            {
                return;
            }
            Err(error) => panic!("Could not create symlink fixture: {error}"),
        }
        // Wine reports success without creating the link; there is nothing to skip.
        if fs::symlink_metadata(temp.0.join("cycle")).is_err() {
            return;
        }
        let tree = scan(&temp.0, &CancelToken::new(), |_| {}).unwrap();
        assert_eq!(
            (
                tree.nodes[0].bytes,
                tree.nodes[0].files,
                tree.nodes[0].skipped
            ),
            (11, 1, 1)
        );
        assert!(scan(temp.0.join("cycle"), &CancelToken::new(), |_| {}).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn skips_symlinks_including_directory_cycles() {
        let temp = Temp::new();
        fs::write(temp.0.join("file"), [0; 11]).unwrap();
        std::os::unix::fs::symlink(&temp.0, temp.0.join("cycle")).unwrap();
        std::os::unix::fs::symlink(temp.0.join("file"), temp.0.join("linked-file")).unwrap();
        let tree = scan(&temp.0, &CancelToken::new(), |_| {}).unwrap();
        assert_eq!(
            (
                tree.nodes[0].bytes,
                tree.nodes[0].files,
                tree.nodes[0].skipped
            ),
            (11, 1, 2)
        );
        assert!(scan(temp.0.join("cycle"), &CancelToken::new(), |_| {}).is_err());
    }
}
