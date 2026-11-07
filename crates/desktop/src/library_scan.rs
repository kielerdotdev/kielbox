//! Latest-request-wins filesystem scanning. No OS filesystem call runs on GPUI.
use crate::recordings;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::SystemTime,
};

pub struct ScanRequest {
    pub root: PathBuf,
    pub toolkit_files: Vec<PathBuf>,
    pub imported_files: BTreeSet<PathBuf>,
}

struct Pending {
    revision: u64,
    request: ScanRequest,
}
struct ScanResult {
    revision: u64,
    outcome: ScanOutcome,
}

pub(crate) struct ScanOutcome {
    pub files: Vec<PathBuf>,
    /// Last-modified times of the scanned files that have one, for date grouping.
    pub modified: BTreeMap<PathBuf, SystemTime>,
    pub root_error: Option<recordings::RootScanError>,
}
impl ScanOutcome {
    /// A disconnected root must not erase the last successful catalog. Explicit
    /// import/removal actions still own references; this only applies a scan.
    pub fn apply(self, files: &mut Vec<PathBuf>) -> Option<recordings::RootScanError> {
        if self.root_error.is_some() {
            let mut known: BTreeSet<_> = files.iter().cloned().collect();
            files.extend(self.files.into_iter().filter(|path| known.insert(path.clone())));
        } else {
            *files = self.files;
        }
        self.root_error
    }
}

pub struct LibraryScanner {
    latest: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
    pending: Arc<Mutex<Option<Pending>>>,
    result: Arc<Mutex<Option<ScanResult>>>,
    wake: Option<mpsc::SyncSender<()>>,
    revision: u64,
    applied: u64,
}

impl LibraryScanner {
    pub fn new() -> Self {
        let latest = Arc::new(AtomicU64::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(Mutex::new(None::<Pending>));
        let result = Arc::new(Mutex::new(None::<ScanResult>));
        let (wake, receiver) = mpsc::sync_channel(1);
        let worker_latest = latest.clone();
        let worker_stopped = stopped.clone();
        let worker_pending = pending.clone();
        let worker_result = result.clone();
        std::thread::Builder::new()
            .name("rcr-library-scan".into())
            .spawn(move || {
                while receiver.recv().is_ok() {
                    loop {
                        if worker_stopped.load(Ordering::Acquire) {
                            return;
                        }
                        let Some(Pending { revision, request }) =
                            worker_pending.lock().unwrap().take()
                        else {
                            break;
                        };
                        let cancelled = || {
                            worker_stopped.load(Ordering::Acquire)
                                || worker_latest.load(Ordering::Acquire) != revision
                        };
                        if let Some(outcome) = scan(request, cancelled)
                            && !cancelled()
                        {
                            *worker_result.lock().unwrap() = Some(ScanResult { revision, outcome });
                        }
                    }
                }
            })
            .expect("start Library scan worker");
        // Intentionally detached: a disconnected network filesystem may block an
        // OS call. The worker owns only bounded request/result slots and no UI.
        Self {
            latest,
            stopped,
            pending,
            result,
            wake: Some(wake),
            revision: 0,
            applied: 0,
        }
    }

    pub fn request(&mut self, request: ScanRequest) {
        if self.stopped.load(Ordering::Acquire) { return; }
        self.revision = self.latest.fetch_add(1, Ordering::AcqRel) + 1;
        *self.pending.lock().unwrap() = Some(Pending {
            revision: self.revision,
            request,
        });
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }

    pub fn poll(&mut self) -> Option<ScanOutcome> {
        if self.stopped.load(Ordering::Acquire) { return None; }
        let result = self.result.lock().unwrap().take()?;
        if result.revision != self.revision {
            return None;
        }
        self.applied = result.revision;
        Some(result.outcome)
    }

    pub fn is_pending(&self) -> bool {
        self.revision != self.applied
    }

    pub fn cancel(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.latest.fetch_add(1, Ordering::AcqRel);
        self.pending.lock().unwrap().take();
        self.result.lock().unwrap().take();
        self.wake.take();
        self.applied = self.revision;
    }
}

impl Drop for LibraryScanner {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn scan(request: ScanRequest, cancelled: impl Fn() -> bool) -> Option<ScanOutcome> {
    let (captures, root_error) = match recordings::scan_in(request.root, &cancelled)? {
        Ok(captures) => (captures, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let mut found: BTreeMap<PathBuf, Option<SystemTime>> = captures
        .into_iter()
        .map(|capture| (capture.path, capture.modified))
        .collect();
    for path in request.toolkit_files {
        if cancelled() {
            return None;
        }
        if !recordings::is_image(&path) {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        if cancelled() {
            return None;
        }
        let path = path.canonicalize().unwrap_or(path);
        found.insert(path, metadata.modified().ok());
    }
    for path in request.imported_files {
        if cancelled() {
            return None;
        }
        if !recordings::is_supported(&path) {
            continue;
        }
        let metadata = std::fs::metadata(&path)
            .ok()
            .filter(|metadata| metadata.is_file());
        if cancelled() {
            return None;
        }
        // Keep saved identity even when its drive is disconnected. Available
        // references were canonicalized by import; no source-path migration here.
        found.insert(path, metadata.and_then(|metadata| metadata.modified().ok()));
    }
    if cancelled() {
        return None;
    }
    let mut captures: Vec<_> = found.into_iter().collect();
    captures.sort_by(|(a, a_time), (b, b_time)| b_time.cmp(a_time).then_with(|| a.cmp(b)));
    if cancelled() {
        return None;
    }
    let modified = captures
        .iter()
        .filter_map(|(path, time)| Some((path.clone(), (*time)?)))
        .collect();
    Some(ScanOutcome {
        files: captures.into_iter().map(|(path, _)| path).collect(),
        modified,
        root_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        time::{Duration, Instant, UNIX_EPOCH},
    };

    #[test]
    fn scan_merges_nested_mixed_case_media_without_trash_and_keeps_offline_imports() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("captures");
        for relative in [
            "nested/older.MP4",
            "nested/newer.MoV",
            "Screen.GIF",
            ".trash/hidden.mp4",
            ".hidden/skip.png",
            "notes.txt",
        ] {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"source").unwrap();
        }
        let older = root.join("nested/older.MP4").canonicalize().unwrap();
        let newer = root.join("nested/newer.MoV").canonicalize().unwrap();
        for (path, seconds) in [(&older, 10), (&newer, 20)] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
                .unwrap();
        }
        let image = root.join("Screen.GIF").canonicalize().unwrap();
        std::fs::File::options()
            .write(true)
            .open(&image)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(5))
            .unwrap();
        let offline = directory.path().join("offline.SSXE");
        let files = scan(
            ScanRequest {
                root,
                toolkit_files: vec![image.clone(), image.clone(), older.clone()],
                imported_files: BTreeSet::from([older.clone(), offline.clone()]),
            },
            || false,
        )
        .unwrap();
        assert!(files.root_error.is_none());
        assert_eq!(files.files, vec![newer, older, image, offline]);
    }

    #[test]
    fn cancellation_discards_the_entire_obsolete_scan() {
        let directory = tempfile::tempdir().unwrap();
        for i in 0..20 {
            std::fs::write(directory.path().join(format!("{i}.mp4")), b"source").unwrap();
        }
        let checks = Cell::new(0);
        let files = scan(
            ScanRequest {
                root: directory.path().into(),
                toolkit_files: vec![],
                imported_files: BTreeSet::new(),
            },
            || {
                checks.set(checks.get() + 1);
                checks.get() >= 7
            },
        );
        assert!(files.is_none());
        assert_eq!(checks.get(), 7);
    }

    #[test]
    fn newest_request_replaces_queued_results_and_reference_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("old.png");
        let current = directory.path().join("current.png");
        let mut scanner = LibraryScanner::new();
        scanner.request(ScanRequest {
            root: directory.path().join("missing"),
            toolkit_files: vec![],
            imported_files: BTreeSet::from([old]),
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        // Leave the first completed result queued, then supersede its snapshot.
        while scanner.result.lock().unwrap().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        scanner.request(ScanRequest {
            root: directory.path().join("missing"),
            toolkit_files: vec![],
            imported_files: BTreeSet::from([current.clone()]),
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let files = loop {
            if let Some(files) = scanner.poll() {
                break files;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(files.files, vec![current]);
        assert!(files.root_error.is_some());
        assert!(!scanner.is_pending());
    }

    #[test]
    fn unavailable_root_retains_catalog_and_imports_until_successful_retry() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("missing-capture-root");
        let existing = directory.path().join("last-known-capture.mp4");
        let offline = directory.path().join("disconnected-import.mp4");
        let request = || ScanRequest { root: root.clone(), toolkit_files: vec![],
            imported_files: BTreeSet::from([offline.clone()]) };
        let outcome = scan(request(), || false).unwrap();
        let error = outcome.root_error.as_ref().unwrap();
        assert_eq!(error.root, root);
        assert_eq!(error.kind, std::io::ErrorKind::NotFound);
        let mut files = vec![existing.clone()];
        assert!(outcome.apply(&mut files).is_some());
        assert_eq!(files, vec![existing, offline.clone()]);
        std::fs::create_dir(&root).unwrap();
        let recovered = root.join("recovered.mp4");
        std::fs::write(&recovered, b"owned fixture").unwrap();
        assert!(scan(request(), || false).unwrap().apply(&mut files).is_none());
        assert_eq!(files, vec![recovered.canonicalize().unwrap(), offline.clone()]);
        assert!(scan(request(), || true).is_none(), "Cancellation must not become a root error");
    }

    #[test]
    fn failed_root_appends_new_references_without_reordering_or_duplicates() {
        let mut files = ["z.mp4", "a.mp4"].map(PathBuf::from).to_vec();
        let outcome = ScanOutcome { files: ["a.mp4", "b.mp4", "b.mp4"].map(PathBuf::from).to_vec(), modified: Default::default(),
            root_error: Some(recordings::RootScanError { root: "missing".into(), kind: std::io::ErrorKind::NotFound }) };
        assert!(outcome.apply(&mut files).is_some());
        assert_eq!(files, ["z.mp4", "a.mp4", "b.mp4"].map(PathBuf::from));
    }

    #[test]
    fn terminal_cancel_rejects_late_result_and_cannot_admit_another_request() {
        let mut scanner = LibraryScanner::new();
        scanner.cancel();
        let revision = scanner.revision;
        // Model the worker passing its cancellation check immediately before
        // cancel cleared the slot, then writing its result immediately after.
        *scanner.result.lock().unwrap() = Some(ScanResult { revision,
            outcome: ScanOutcome { files: vec!["late.mp4".into()], modified: Default::default(), root_error: None } });
        assert!(scanner.poll().is_none());
        scanner.request(ScanRequest { root: "unused".into(), toolkit_files: vec![], imported_files: BTreeSet::new() });
        assert_eq!(scanner.revision, revision);
        assert!(!scanner.is_pending());
        assert!(scanner.pending.lock().unwrap().is_none());
    }
}
