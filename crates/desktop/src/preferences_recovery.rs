//! Load preferences without allowing fallback defaults to destroy unreadable state.
use super::{MAX_PREFERENCES_BYTES, Preferences};
use crate::preferences_io::PreferenceWriter;
use std::{io::{self, Read, Write}, path::{Path, PathBuf}};

#[derive(Clone)]
enum WritePolicy {
    Allowed,
    Blocked(String),
}

pub(super) struct LoadedPreferences {
    pub preferences: Preferences,
    pub notice: Option<String>,
    path: PathBuf,
    policy: WritePolicy,
}
impl LoadedPreferences {
    pub(super) fn start_writer(&self, initial: Preferences) -> Result<PreferenceWriter, String> {
        let path = self.path.clone();
        let policy = self.policy.clone();
        PreferenceWriter::start(initial, move |candidate| {
            if let WritePolicy::Blocked(error) = &policy { return Err(error.clone()); }
            candidate.save_to(&path).map_err(|error| error.to_string())
        })
    }
}

pub(super) fn load(path: &Path) -> LoadedPreferences {
    // Keep the startup source and subsequent writes on the same absolute path.
    let path = if path.is_absolute() { path.to_path_buf() } else {
        match std::env::current_dir() {
            Ok(directory) => directory.join(path),
            Err(error) => {
                let detail = format!("Could not resolve the settings path {}: {error}. Settings saves are disabled; restore access to the application folder and restart RCR.", path.display());
                return LoadedPreferences { preferences: Preferences::default(), notice: Some(detail.clone()), path: path.to_path_buf(), policy: WritePolicy::Blocked(detail) };
            }
        }
    };
    let read = read_bounded(&path);
    load_with(&path, read, backup_invalid)
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_PREFERENCES_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Settings exceed the 8 MiB read limit"));
    }
    // The limit also covers a file that grows after its metadata was checked.
    let mut bytes = Vec::new();
    file.take(MAX_PREFERENCES_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PREFERENCES_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Settings exceed the 8 MiB read limit"));
    }
    Ok(bytes)
}

fn backup_invalid(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    // NamedTempFile creates a unique sibling with CREATE_NEW. Keep it only once
    // all original bytes have been flushed and synced; never rename the source.
    let mut backup = tempfile::Builder::new().prefix("preferences.recovery-").suffix(".json").tempfile_in(parent)?;
    backup.write_all(bytes)?;
    backup.flush()?;
    backup.as_file().sync_all()?;
    let (_, path) = backup.keep().map_err(|error| error.error)?;
    Ok(path)
}

fn load_with(
    path: &Path,
    read: io::Result<Vec<u8>>,
    backup: impl FnOnce(&Path, &[u8]) -> io::Result<PathBuf>,
) -> LoadedPreferences {
    let defaults = |notice, policy| LoadedPreferences {
        preferences: Preferences::default(), notice, path: path.to_path_buf(), policy,
    };
    let bytes = match read {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound
            && matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound) => {
            return defaults(None, WritePolicy::Allowed);
        }
        Err(error) => {
            let detail = format!("Could not read settings at {}: {error}. Using defaults for this session. Settings saves are disabled to preserve the original; fix file access or restore the file, then restart RCR.", path.display());
            return defaults(Some(detail.clone()), WritePolicy::Blocked(detail));
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(preferences) => LoadedPreferences { preferences, notice: None, path: path.to_path_buf(), policy: WritePolicy::Allowed },
        Err(error) => {
            let reason: String = error.to_string().chars().take(400).collect();
            match backup(path, &bytes) {
                Ok(backup) => defaults(Some(format!("Could not load settings at {}: {reason}. An exact recovery copy is saved at {}. Using defaults; repair that copy and restore it to the original path before restarting, or save changed settings to start fresh.", path.display(), backup.display())), WritePolicy::Allowed),
                Err(error) => {
                    let detail = format!("Could not load settings at {}: {reason}. Recovery copy failed: {error}. Using defaults for this session. Settings saves are disabled to preserve the original; copy or repair the original file, then restart RCR.", path.display());
                    defaults(Some(detail.clone()), WritePolicy::Blocked(detail))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LoadedPreferences, MAX_PREFERENCES_BYTES, Preferences, backup_invalid, load, load_with};
    use std::{io, path::Path, time::{Duration, Instant}};

    fn assert_all_writes_blocked(loaded: &LoadedPreferences, path: &Path, before: &[u8]) {
        let writer = loaded.start_writer(loaded.preferences.clone()).unwrap();
        let candidate = Preferences { fps: 30, list_view: true, ..loaded.preferences.clone() };
        assert!(writer.save_barrier(&candidate).unwrap_err().contains("saves are disabled"));
        let revision = writer.set_list_view(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(ack) = writer.take_layout_ack() && ack.revision == revision {
                assert!(ack.result.unwrap_err().contains("saves are disabled"));
                break;
            }
            assert!(Instant::now() < deadline, "Blocked layout must receive a failure acknowledgement");
            std::thread::yield_now();
        }
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn missing_preferences_use_defaults_without_backup_or_notice() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("projects/preferences.json");
        let loaded = load(&path);
        assert!(loaded.notice.is_none());
        assert_eq!(loaded.preferences.fps, Preferences::default().fps);
        assert!(!path.exists());
        let writer = loaded.start_writer(loaded.preferences.clone()).unwrap();
        writer.save_barrier(&Preferences { fps: 30, ..loaded.preferences }).unwrap();
        assert_eq!(serde_json::from_slice::<Preferences>(&std::fs::read(path).unwrap()).unwrap().fps, 30);
    }

    #[test]
    fn valid_preferences_are_loaded_without_backup_or_rewrite() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let original = br#"{ "fps": 120, "folder": "original recordings", "list_view": true, "microphone": false }"#;
        std::fs::write(&path, original).unwrap();
        let loaded = load(&path);
        assert!(loaded.notice.is_none());
        assert_eq!(loaded.preferences.fps, 120);
        assert_eq!(loaded.preferences.folder, Path::new("original recordings"));
        assert!(loaded.preferences.list_view);
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn invalid_preferences_have_exact_nonclobber_backup_before_defaults_can_save() {
        for original in [b"{ partial json\n\xff".as_slice(), br#"{"fps":"invalid-type"}"#.as_slice()] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("preferences.json");
            std::fs::write(&path, original).unwrap();
            let first = load(&path);
            let second = load(&path);
            let notice = first.notice.as_ref().unwrap();
            assert!(notice.contains("exact recovery copy") && notice.contains(&path.display().to_string()));
            assert_eq!(std::fs::read(&path).unwrap(), original, "Loading cannot replace the source");
            let backups: Vec<_> = std::fs::read_dir(directory.path()).unwrap().flatten()
                .map(|entry| entry.path()).filter(|entry| entry != &path).collect();
            assert_eq!(backups.len(), 2, "Repeated loads must not clobber a recovery file");
            for backup in &backups { assert_eq!(std::fs::read(backup).unwrap(), original); }
            let writer = second.start_writer(second.preferences.clone()).unwrap();
            writer.save_barrier(&Preferences { fps: 30, ..second.preferences }).unwrap();
            assert_eq!(serde_json::from_slice::<Preferences>(&std::fs::read(&path).unwrap()).unwrap().fps, 30);
            for backup in backups { assert_eq!(std::fs::read(backup).unwrap(), original); }
        }
    }

    #[test]
    fn failed_backup_blocks_barrier_and_layout_writes_even_after_file_is_repaired() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let original = b"{ incomplete";
        std::fs::write(&path, original).unwrap();
        let loaded = load_with(&path, Ok(original.to_vec()), |_, _| {
            Err(io::Error::new(io::ErrorKind::StorageFull, "injected recovery-copy failure"))
        });
        assert!(loaded.notice.as_ref().unwrap().contains("Recovery copy failed"));
        assert_all_writes_blocked(&loaded, &path, original);
        let repaired = br#"{"fps":120}"#;
        std::fs::write(&path, repaired).unwrap();
        assert_all_writes_blocked(&loaded, &path, repaired);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn read_errors_do_not_create_backups_or_allow_default_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let original = br#"{"fps":120}"#;
        std::fs::write(&path, original).unwrap();
        let loaded = load_with(&path, Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected read denial")),
            |_, _| panic!("An unread file cannot be backed up from partial bytes"));
        assert_all_writes_blocked(&loaded, &path, original);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn oversized_preferences_are_preserved_without_reading_or_copying_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        std::fs::File::create(&path).unwrap().set_len(MAX_PREFERENCES_BYTES + 1).unwrap();
        let loaded = load(&path);
        assert!(loaded.notice.as_ref().unwrap().contains("8 MiB read limit"));
        let writer = loaded.start_writer(loaded.preferences.clone()).unwrap();
        assert!(writer.save_barrier(&loaded.preferences).is_err());
        assert_eq!(std::fs::metadata(path).unwrap().len(), MAX_PREFERENCES_BYTES + 1);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn recovery_backup_keeps_every_original_byte() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let original: Vec<u8> = (0..65_537).map(|index| (index % 256) as u8).collect();
        std::fs::write(&path, &original).unwrap();
        let backup = backup_invalid(&path, &original).unwrap();
        assert_eq!(std::fs::read(backup).unwrap(), original);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}
