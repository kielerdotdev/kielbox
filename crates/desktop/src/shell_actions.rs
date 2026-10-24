//! One admitted filesystem/shell action through its actual return, independent of UI lifetime.
use gpui::Context;
use std::{path::{Path, PathBuf}, sync::{Arc, OnceLock, mpsc, atomic::{AtomicBool, AtomicU64, Ordering}}};

#[derive(Clone, Debug)]
pub(crate) enum Kind {
    Reveal, OpenFolder,
    #[cfg(windows)] CopyFile { owner: isize },
    #[cfg(windows)] CopyText { owner: isize, text: String },
}
#[cfg(windows)]
const MAX_TEXT_BYTES: usize = 256 * 1024;
#[cfg(windows)]
fn text_payload(text: &str) -> Result<Vec<u8>, String> {
    if text.len() > MAX_TEXT_BYTES || text.contains('\0') {
        return Err("Diagnostic text exceeds 256 KiB or contains a null character.".into());
    }
    Ok(text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect())
}
struct Lease(Arc<AtomicBool>);
impl Drop for Lease { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }
struct Job { kind: Kind, path: PathBuf, reply: async_channel::Sender<Result<(), String>>, _lease: Lease }
struct Service { requests: mpsc::SyncSender<Job>, busy: Arc<AtomicBool>, next: AtomicU64 }
#[derive(Debug)]
pub(crate) struct Ticket { pub id: u64, reply: async_channel::Receiver<Result<(), String>> }
impl Ticket {
    async fn finish(self) -> Result<(), String> {
        self.reply.recv().await.map_err(|_| "The file-action worker stopped before reporting its result.".to_owned())?
    }
}
impl Service {
    fn new(run: impl Fn(Kind, &Path) -> Result<(), String> + Send + 'static) -> Result<Self, String> {
        let (requests, jobs) = mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new().name("rcr-file-actions".into()).spawn(move || {
            while let Ok(job) = jobs.recv() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(job.kind, &job.path)))
                    .unwrap_or_else(|_| Err("The file action failed unexpectedly.".into()));
                let Job { reply, _lease, .. } = job;
                // Keep admission even if the requester disappeared while an OS
                // call was blocked. The call has now actually returned.
                drop(_lease);
                let _ = reply.try_send(result);
            }
        }).map_err(|error| format!("Could not start file-action worker: {error}"))?;
        Ok(Self { requests, busy: Arc::new(AtomicBool::new(false)), next: AtomicU64::new(1) })
    }
    fn submit(&self, kind: Kind, path: PathBuf) -> Result<Ticket, String> {
        #[cfg(windows)]
        if let Kind::CopyText { owner, text } = &kind {
            if *owner == 0 { return Err("The editor window is no longer available.".into()); }
            // Validate before admission/allocation; Windows access stays worker-only.
            if text.len() > MAX_TEXT_BYTES || text.contains('\0') {
                return Err("Diagnostic text exceeds 256 KiB or contains a null character.".into());
            }
        }
        self.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Another file action is still running. Wait for it to finish, then try again.".to_owned())?;
        let lease = Lease(self.busy.clone());
        let id = self.next.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| "File-action identifiers exhausted. Restart rcr.".to_owned())?;
        let (reply, result) = async_channel::bounded(1);
        self.requests.try_send(Job { kind, path, reply, _lease: lease })
            .map_err(|_| "The file-action worker is unavailable. Restart rcr.".to_owned())?;
        Ok(Ticket { id, reply: result })
    }
}
fn run_with(kind: Kind, path: &Path, launch: impl FnOnce(Kind, &Path) -> Result<(), String>) -> Result<(), String> {
    if matches!(kind, Kind::OpenFolder) {
        std::fs::create_dir_all(path).map_err(|error| format!("Could not prepare capture folder {}: {error}", path.display()))?;
    }
    let resolved = path.canonicalize().map_err(|error| format!("Could not locate {}: {error}", path.display()))?;
    launch(kind, &resolved).map_err(|error| format!("Could not open {} in File Explorer: {error}", path.display()))
}
fn submit(kind: Kind, path: PathBuf) -> Result<Ticket, String> {
    static SERVICE: OnceLock<Result<Service, String>> = OnceLock::new();
    let service = SERVICE.get_or_init(|| Service::new(|kind, path| {
        #[cfg(windows)]
        if let Kind::CopyFile { owner } = kind {
            return super::copy_file_owned(path, owner).map_err(|error| format!("Could not copy {}: {error:#}", path.display()));
        }
        #[cfg(windows)]
        if let Kind::CopyText { owner, text } = kind {
            let bytes = text_payload(&text)?;
            return super::write_clipboard_owned(owner, 13, &bytes)
                .map_err(|error| format!("Could not copy diagnostic text: {error:#}"));
        }
        if matches!(kind, Kind::OpenFolder) { return super::try_open_folder(path); }
        run_with(kind, path, |kind, path| {
        let mut command = std::process::Command::new("explorer.exe");
        match kind { Kind::Reveal => { command.arg(format!("/select,{}", path.display())); }, Kind::OpenFolder => { command.arg(path); }, #[cfg(windows)] Kind::CopyFile { .. } | Kind::CopyText { .. } => unreachable!("Copy was dispatched before shell launch") }
        command.spawn().map(|_| ()).map_err(|error| error.to_string())
        })
    }));
    service.as_ref().map_err(Clone::clone)?.submit(kind, path)
}
pub(crate) fn begin<T: 'static>(kind: Kind, path: PathBuf, cx: &mut Context<T>,
    complete: impl FnOnce(&mut T, u64, &Path, Result<(), String>, &mut Context<T>) + 'static) -> Result<u64, String> {
    let ticket = submit(kind, path.clone())?;
    let id = ticket.id;
    cx.spawn(async move |view, cx| {
        let result = ticket.finish().await;
        let _ = view.update(cx, |this, cx| complete(this, id, &path, result, cx));
    }).detach();
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn diagnostic_text_is_bounded_utf16_and_never_resolves_a_file() {
        let expected = "Encoder failed\r\n日本語 🎮";
        let bytes = text_payload(expected).unwrap();
        let units = bytes.as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes).collect::<Vec<_>>();
        assert_eq!(units.last(), Some(&0));
        assert_eq!(String::from_utf16(&units[..units.len() - 1]).unwrap(), expected);
        assert!(text_payload("embedded\0text").is_err());
        assert!(text_payload(&"x".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert!(text_payload(&"x".repeat(MAX_TEXT_BYTES)).is_ok());
        let service = Service::new(|kind, path| {
            assert!(path.as_os_str().is_empty());
            assert!(matches!(kind, Kind::CopyText { owner: 42, ref text } if text == "actual diagnostic"));
            Err("Injected clipboard close failure".into())
        }).unwrap();
        assert!(service.submit(Kind::CopyText { owner: 0, text: "text".into() }, PathBuf::new()).is_err());
        assert!(!service.busy.load(Ordering::Acquire));
        let ticket = service.submit(Kind::CopyText { owner: 42, text: "actual diagnostic".into() }, PathBuf::new()).unwrap();
        assert!(ticket.reply.recv_blocking().unwrap().unwrap_err().contains("close failure"));
        assert!(!service.busy.load(Ordering::Acquire));
    }
    #[cfg(windows)]
    #[test]
    fn text_copy_retains_admission_through_dropped_observer() {
        let (entered, seen) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        let service = Service::new(move |_, _| { entered.send(()).unwrap(); wait.recv().unwrap(); Ok(()) }).unwrap();
        let ticket = service.submit(Kind::CopyText { owner: 42, text: "diagnostic".into() }, PathBuf::new()).unwrap();
        seen.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        drop(ticket);
        assert!(service.submit(Kind::Reveal, "source".into()).unwrap_err().contains("still running"));
        assert!(service.busy.load(Ordering::Acquire));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while service.busy.load(Ordering::Acquire) { assert!(std::time::Instant::now() < deadline); std::thread::yield_now(); }
    }
    #[cfg(windows)]
    #[test]
    fn file_copy_and_shell_share_actual_admission_through_dropped_request() {
        let (entered, seen) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        let service = Service::new(move |kind, path| {
            assert!(matches!(kind, Kind::CopyFile { owner: 42 }));
            assert_eq!(path, Path::new("owned source"));
            entered.send(()).unwrap(); wait.recv().unwrap();
            Err("Injected actual clipboard failure".into())
        }).unwrap();
        let copy = service.submit(Kind::CopyFile { owner: 42 }, "owned source".into()).unwrap();
        seen.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(service.submit(Kind::Reveal, "other source".into()).unwrap_err().contains("still running"));
        assert!(service.submit(Kind::CopyFile { owner: 99 }, "new source".into()).unwrap_err().contains("still running"));
        drop(copy);
        assert!(service.busy.load(Ordering::Acquire), "UI lifetime released an active clipboard call");
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while service.busy.load(Ordering::Acquire) { assert!(std::time::Instant::now() < deadline); std::thread::yield_now(); }
    }
    #[test]
    fn dropped_reply_does_not_release_admission_before_actual_call_returns() {
        let (entered, seen) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        let service = Service::new(move |_, _| { entered.send(()).unwrap(); wait.recv().unwrap(); Ok(()) }).unwrap();
        let request = service.submit(Kind::Reveal, "owned".into()).unwrap();
        seen.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        drop(request);
        assert!(service.submit(Kind::OpenFolder, "other".into()).unwrap_err().contains("still running"));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while service.busy.load(Ordering::Acquire) { assert!(std::time::Instant::now() < deadline); std::thread::yield_now(); }
    }
    #[test]
    fn injected_failure_preserves_file_and_actual_launcher_cause() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("clip with spaces, commas.png");
        std::fs::write(&source, b"owned original").unwrap();
        let error = run_with(Kind::Reveal, &source, |kind, path| {
            assert!(matches!(kind, Kind::Reveal)); assert_eq!(path, source.canonicalize().unwrap()); Err("injected launcher denied".into())
        }).unwrap_err();
        assert!(error.contains("injected launcher denied"));
        assert_eq!(std::fs::read(source).unwrap(), b"owned original");
    }
    #[test]
    fn missing_source_does_not_invoke_launcher_or_create_file() {
        let directory = tempfile::tempdir().unwrap(); let missing = directory.path().join("missing.png");
        assert!(run_with(Kind::Reveal, &missing, |_, _| panic!("Missing source reached launcher")).is_err());
        assert!(!missing.exists());
    }
    #[test]
    fn folder_preparation_and_resolved_launch_are_worker_owned() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("new capture folder").join("nested");
        assert!(!folder.exists());
        run_with(Kind::OpenFolder, &folder, |kind, resolved| {
            assert!(matches!(kind, Kind::OpenFolder));
            assert_eq!(resolved, folder.canonicalize().unwrap());
            assert!(resolved.is_dir());
            Ok(())
        }).unwrap();
    }
    #[test]
    fn completions_keep_original_operation_and_source_identity() {
        let (entered, seen) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        let service = Service::new(move |_, path| {
            entered.send(path.to_path_buf()).unwrap();
            wait.recv().unwrap();
            Err(format!("injected failure for {}", path.display()))
        }).unwrap();
        let first = service.submit(Kind::Reveal, "first source".into()).unwrap();
        assert_eq!(seen.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), PathBuf::from("first source"));
        release.send(()).unwrap();
        assert!(first.reply.recv_blocking().unwrap().unwrap_err().contains("first source"));
        let second = service.submit(Kind::Reveal, "new source".into()).unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(seen.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), PathBuf::from("new source"));
        release.send(()).unwrap();
        assert!(second.reply.recv_blocking().unwrap().unwrap_err().contains("new source"));
    }
    #[test]
    fn panic_releases_actual_admission_and_reports_failure_without_explorer() {
        let service = Service::new(|_, _| panic!("owned injected launcher panic")).unwrap();
        let ticket = service.submit(Kind::Reveal, "owned".into()).unwrap();
        assert!(ticket.reply.recv_blocking().unwrap().unwrap_err().contains("unexpectedly"));
        assert!(!service.busy.load(Ordering::Acquire));
    }
}
