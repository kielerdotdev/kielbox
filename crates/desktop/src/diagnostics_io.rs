//! Best-effort diagnostics use bounded, independent workers; never join on UI.
use std::{
    io,
    path::{Path, PathBuf},
    sync::{OnceLock, mpsc},
};

type Completion = Result<(), String>;
pub type Receipt = async_channel::Receiver<Completion>;
const MAX_REPORT_BYTES: usize = 256 * 1024;
const MAX_LOG_BYTES: usize = 16 * 1024;
const MAX_LOG_FILE_BYTES: u64 = 4 * 1024 * 1024;

struct Job {
    path: PathBuf,
    bytes: Vec<u8>,
    reply: async_channel::Sender<Completion>,
}
struct Writer {
    sender: mpsc::SyncSender<Job>,
}
impl Writer {
    fn start(
        name: &str,
        capacity: usize,
        mut write: impl FnMut(&Path, &[u8]) -> io::Result<()> + Send + 'static,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(capacity);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                for job in receiver {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        write(&job.path, &job.bytes).map_err(|error| error.to_string())
                    })).unwrap_or_else(|_| Err("Diagnostic writer panicked; this report was not saved".into()));
                    // A dropped UI receipt does not interrupt or release an active write.
                    let _ = job.reply.try_send(result);
                }
            })
            .map_err(|error| format!("Cannot start diagnostic writer: {error}"))?;
        Ok(Self { sender })
    }
    fn submit(&self, path: PathBuf, bytes: Vec<u8>) -> Result<Receipt, String> {
        let (reply, receipt) = async_channel::bounded(1);
        self.sender
            .try_send(Job { path, bytes, reply })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    "Diagnostic writer is busy; report was not queued".to_owned()
                }
                mpsc::TrySendError::Disconnected(_) => {
                    "Diagnostic writer stopped; report was not queued".to_owned()
                }
            })?;
        Ok(receipt)
    }
}

pub fn write_report(path: PathBuf, bytes: Vec<u8>) -> Result<Receipt, String> {
    if bytes.len() > MAX_REPORT_BYTES {
        return Err("Diagnostic report exceeds 256 KiB".into());
    }
    static REPORTS: OnceLock<Result<Writer, String>> = OnceLock::new();
    REPORTS
        .get_or_init(|| Writer::start("rcr-reports", 2, |path, bytes| std::fs::write(path, bytes)))
        .as_ref()
        .map_err(Clone::clone)?
        .submit(path, bytes)
}

fn log_line(operation: &str, error: &str) -> Vec<u8> {
    let mut line = String::new();
    for part in [operation, ": ", error] {
        let remaining = MAX_LOG_BYTES - line.len();
        let mut end = part.len().min(remaining);
        while !part.is_char_boundary(end) {
            end -= 1;
        }
        line.push_str(&part[..end]);
        if end < part.len() {
            line.push_str(" [truncated]");
            break;
        }
    }
    line.push('\n');
    line.into_bytes()
}

pub fn log_editor_error(operation: &str, error: &str) {
    static LOGS: OnceLock<Result<Writer, String>> = OnceLock::new();
    let writer = LOGS.get_or_init(|| {
        Writer::start("rcr-editor-log", 32, |path, bytes| append_log(path, bytes, MAX_LOG_FILE_BYTES))
    });
    if let Ok(writer) = writer {
        // Same best-effort contract as the old log; UI error details remain intact.
        let _ = writer.submit(
            PathBuf::from("projects/editor-errors.log"),
            log_line(operation, error),
        );
    }
}

/// One worker owns both files. Keep the previous log, and never grow either
/// file without a bound when a failing source repeatedly produces diagnostics.
fn append_log(path: &Path, bytes: &[u8], limit: u64) -> io::Result<()> {
    use io::Write;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Log entry exceeds file limit"));
    }
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let size = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    if size.saturating_add(bytes.len() as u64) > limit {
        let backup = path.with_extension("log.1");
        if size > limit {
            // Migrate a log written by older versions without retaining an
            // arbitrarily large backup. Stage the tail before removing source.
            use io::{Read, Seek, SeekFrom};
            let mut source = std::fs::File::open(path)?;
            source.seek(SeekFrom::Start(size - limit))?;
            let mut tail = Vec::with_capacity(limit as usize);
            source.take(limit).read_to_end(&mut tail)?;
            // The first line may start halfway through a UTF-8 character.
            let start = tail.iter().position(|byte| *byte == b'\n').map_or(tail.len(), |index| index + 1);
            let mut staged = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
            staged.write_all(&tail[start..])?;
            staged.persist(&backup).map_err(|error| error.error)?;
            std::fs::remove_file(path)?;
        } else {
            match std::fs::remove_file(&backup) {
                Ok(()) => {},
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
            std::fs::rename(path, backup)?;
        }
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn panicked_diagnostic_job_reports_failure_without_disabling_future_reports() {
        let writer = Writer::start("rcr-panic-recovery-test", 2, |_, bytes| {
            assert_ne!(bytes, b"panic", "Injected diagnostic writer failure");
            Ok(())
        }).unwrap();
        let failed = writer.submit("unused".into(), b"panic".to_vec()).unwrap();
        assert!(failed.recv_blocking().unwrap().unwrap_err().contains("panicked"));
        assert_eq!(writer.submit("unused".into(), b"next report".to_vec()).unwrap()
            .recv_blocking().unwrap(), Ok(()));
    }

    #[test]
    fn repeated_errors_rotate_to_one_bounded_backup_and_keep_whole_entries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("editor-errors.log");
        for entry in [b"first\n".as_slice(), b"second\n", b"third\n", b"fourth\n"] {
            append_log(&path, entry, 13).unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"third\nfourth\n");
        assert_eq!(std::fs::read(path.with_extension("log.1")).unwrap(), b"first\nsecond\n");
        assert!(append_log(&path, &[0; 14], 13).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"third\nfourth\n");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn oversized_legacy_log_migrates_to_bounded_complete_line_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("editor-errors.log");
        std::fs::write(&path, "old entry\n🦀 partial\nrecent\n").unwrap();
        append_log(&path, b"new\n", 16).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new\n");
        let backup = std::fs::read(path.with_extension("log.1")).unwrap();
        assert!(backup.len() <= 16);
        assert_eq!(std::str::from_utf8(&backup).unwrap(), "recent\n");
    }

    #[test]
    fn blocked_writer_is_bounded_and_dropped_receipt_does_not_release_job() {
        let (entered, started) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let (written, results) = mpsc::channel();
        let mut first = true;
        let writer = Writer::start("rcr-owned-diagnostic-test", 2, move |_, bytes| {
            if first {
                first = false;
                entered.send(()).unwrap();
                gate.recv().unwrap();
            }
            written.send(bytes.to_vec()).unwrap();
            Ok(())
        })
        .unwrap();
        let receipt = writer.submit("a".into(), vec![1]).unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(receipt.try_recv().is_err());
        drop(receipt);
        let second = writer.submit("b".into(), vec![2]).unwrap();
        let third = writer.submit("c".into(), vec![3]).unwrap();
        assert!(writer.submit("d".into(), vec![4]).is_err());
        release.send(()).unwrap();
        for expected in [vec![1], vec![2], vec![3]] {
            assert_eq!(
                results.recv_timeout(Duration::from_secs(2)).unwrap(),
                expected
            );
        }
        assert_eq!(second.recv_blocking().unwrap(), Ok(()));
        assert_eq!(third.recv_blocking().unwrap(), Ok(()));
    }

    #[test]
    fn report_receipt_reflects_actual_disk_success_and_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.json");
        let writer = Writer::start("rcr-owned-report-test", 2, |path, bytes| {
            std::fs::write(path, bytes)
        })
        .unwrap();
        assert_eq!(
            writer
                .submit(path.clone(), b"{\"complete\":true}".to_vec())
                .unwrap()
                .recv_blocking()
                .unwrap(),
            Ok(())
        );
        assert_eq!(std::fs::read(path).unwrap(), b"{\"complete\":true}");
        assert!(
            writer
                .submit(directory.path().join("missing/preview.json"), vec![1])
                .unwrap()
                .recv_blocking()
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn oversized_reports_are_rejected_and_log_lines_are_bounded_utf8() {
        assert!(write_report("unused".into(), vec![0; MAX_REPORT_BYTES + 1]).is_err());
        let line = log_line("Seek", &"🦀".repeat(MAX_LOG_BYTES));
        let line = std::str::from_utf8(&line).unwrap();
        assert!(line.starts_with("Seek: ") && line.ends_with(" [truncated]\n"));
        assert!(line.len() <= MAX_LOG_BYTES + " [truncated]\n".len());
    }
}
