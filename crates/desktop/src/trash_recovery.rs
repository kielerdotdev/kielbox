//! Durable managed-Trash recovery. All functions perform filesystem I/O and
//! belong on a cancellable worker; manifests and originals are never deleted.
use crate::recordings;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

pub const MAX_BATCHES: usize = 256;
pub const MAX_ENTRIES: usize = 2048;
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_MOVES_PER_BATCH: usize = 1024;
const MAX_DIRECTORY_ENTRIES: usize = 4096;
const MAX_INSPECTED_MOVES: usize = 4096;
const MAX_WARNINGS: usize = 64;

#[derive(Clone, Debug)]
pub struct TrashEntry {
    pub original: PathBuf,
    pub trashed: PathBuf,
    pub batch: PathBuf,
    pub bytes: u64,
    pub removed_at: Option<SystemTime>,
    pub conflict: bool,
}
#[derive(Default, Debug)]
pub struct RecoveryCatalog {
    pub entries: Vec<TrashEntry>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored,
    NoLongerInTrash,
}

#[derive(Deserialize)]
struct Manifest {
    version: u32,
    moves: Vec<(PathBuf, PathBuf)>,
}
struct Locations {
    root: PathBuf,
    trash: PathBuf,
}
impl RecoveryCatalog {
    fn warn(&mut self, warning: impl Into<String>) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(warning.into());
        } else {
            self.truncated = true;
        }
    }
    fn limit(&mut self) {
        self.truncated = true;
    }
    fn finish(&mut self) {
        self.entries.sort_by(|a, b| {
            b.removed_at
                .cmp(&a.removed_at)
                .then_with(|| a.original.cmp(&b.original))
        });
        if self.truncated {
            if self.warnings.len() == MAX_WARNINGS {
                self.warnings.pop();
            }
            self.warnings.push(format!("Trash recovery reached a scan limit ({MAX_BATCHES} batches, {MAX_ENTRIES} files and bounded journal inspection). Newest journals are inspected first among at most {MAX_DIRECTORY_ENTRIES} discovered directory items; newer batches beyond that discovery limit may not be listed. Additional files or warnings may be omitted. No files were changed."));
        }
    }
}
fn checkpoint(cancelled: &impl Fn() -> bool) -> Result<()> {
    ensure!(!cancelled(), "Trash recovery cancelled");
    Ok(())
}
fn no_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}
fn plain_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        no_reparse(&metadata) && metadata.is_dir(),
        "Recovery directory is a link or is not a regular directory: {}",
        path.display()
    );
    Ok(())
}
fn clean_absolute(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && !path
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir)),
        "Recovery path must be absolute without traversal: {}",
        path.display()
    );
    for part in path.components() {
        if let Component::Normal(name) = part {
            ensure!(
                !name.to_string_lossy().contains(':'),
                "Recovery path contains an alternate data stream: {}",
                path.display()
            );
        }
    }
    Ok(())
}
fn inside_trash(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|part| {
            part.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".trash")
        })
}
fn locations(root: &Path, cancelled: &impl Fn() -> bool) -> Result<Option<Locations>> {
    checkpoint(cancelled)?;
    let root = root
        .canonicalize()
        .context("Capture folder is unavailable")?;
    plain_directory(&root)?;
    let trash = root.join(".trash");
    checkpoint(cancelled)?;
    match fs::symlink_metadata(&trash) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => plain_directory(&trash)?,
    }
    let canonical = trash.canonicalize()?;
    ensure!(
        canonical.parent() == Some(root.as_path()),
        "Trash folder escapes the capture folder"
    );
    Ok(Some(Locations {
        root,
        trash: canonical,
    }))
}
fn checked_batch(locations: &Locations, batch: &Path) -> Result<PathBuf> {
    clean_absolute(batch)?;
    ensure!(
        batch.parent() == Some(locations.trash.as_path())
            && batch
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("batch-")),
        "Invalid managed Trash batch: {}",
        batch.display()
    );
    plain_directory(batch)?;
    let canonical = batch.canonicalize()?;
    ensure!(
        canonical.parent() == Some(locations.trash.as_path()),
        "Trash batch escapes its capture folder"
    );
    Ok(canonical)
}
fn manifest(batch: &Path, cancelled: &impl Fn() -> bool) -> Result<(Manifest, Option<SystemTime>)> {
    checkpoint(cancelled)?;
    let path = batch.join("recovery.json");
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        no_reparse(&metadata) && metadata.is_file(),
        "Recovery journal is a link or is not a regular file"
    );
    ensure!(
        metadata.len() <= MAX_MANIFEST_BYTES as u64,
        "Recovery journal exceeds the byte limit"
    );
    ensure!(
        path.canonicalize()?.parent() == Some(batch),
        "Recovery journal escapes its batch"
    );
    let mut file = fs::File::open(&path)?;
    let mut bytes = Vec::new();
    loop {
        checkpoint(cancelled)?;
        let mut chunk = [0; 16 * 1024];
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        ensure!(
            bytes.len() + count <= MAX_MANIFEST_BYTES,
            "Recovery journal exceeds the byte limit"
        );
        bytes.extend_from_slice(&chunk[..count]);
    }
    // A replacement link during reading is unsafe even if the JSON parses.
    let after = fs::symlink_metadata(&path)?;
    ensure!(
        no_reparse(&after) && after.is_file() && path.canonicalize()?.parent() == Some(batch),
        "Recovery journal changed location while reading"
    );
    let manifest: Manifest =
        serde_json::from_slice(&bytes).context("Recovery journal is corrupt")?;
    ensure!(
        manifest.version == 1,
        "Unsupported recovery journal version {}",
        manifest.version
    );
    ensure!(
        manifest.moves.len() <= MAX_MOVES_PER_BATCH,
        "Recovery journal exceeds the move limit"
    );
    Ok((manifest, metadata.modified().ok()))
}
fn original_parent(locations: &Locations, original: &Path) -> Result<()> {
    clean_absolute(original)?;
    let parent = original
        .parent()
        .context("Original folder is missing from recovery path")?;
    ensure!(
        parent.starts_with(&locations.root) && !inside_trash(&locations.root, parent),
        "Original folder is outside this capture folder or inside Trash"
    );
    // Revalidate every existing directory, including junctions substituted since
    // scanning. Missing folders require explicit repair; never create them here.
    let relative = parent.strip_prefix(&locations.root)?;
    let mut directory = locations.root.clone();
    plain_directory(&directory)?;
    for component in relative.components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "Invalid original folder component"
        );
        directory.push(component.as_os_str());
        plain_directory(&directory).with_context(|| {
            format!(
                "Original folder is missing or unsafe; recreate it before restoring: {}",
                directory.display()
            )
        })?;
    }
    let canonical = parent.canonicalize()?;
    ensure!(
        canonical.starts_with(&locations.root) && !inside_trash(&locations.root, &canonical),
        "Original folder escaped the capture folder"
    );
    Ok(())
}
fn inspect(
    locations: &Locations,
    batch: &Path,
    original: &Path,
    trashed: &Path,
) -> Result<Option<(u64, bool)>> {
    clean_absolute(trashed)?;
    ensure!(
        trashed.parent() == Some(batch)
            && trashed
                .file_name()
                .is_some_and(|name| !name.to_string_lossy().eq_ignore_ascii_case("recovery.json")),
        "Trashed file is outside its own batch or is the recovery journal"
    );
    let metadata = match fs::symlink_metadata(trashed) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) => metadata,
    };
    ensure!(
        no_reparse(&metadata) && metadata.is_file(),
        "Trashed source is a link or is not a regular file"
    );
    ensure!(
        trashed.canonicalize()?.parent() == Some(batch),
        "Trashed source escapes its batch"
    );
    original_parent(locations, original)?;
    let conflict = match fs::symlink_metadata(original) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    Ok(Some((metadata.len(), conflict)))
}

pub fn scan(root: &Path, cancelled: impl Fn() -> bool) -> Result<RecoveryCatalog> {
    let mut catalog = RecoveryCatalog::default();
    let locations = match locations(root, &cancelled) {
        Ok(Some(locations)) => locations,
        Ok(None) => return Ok(catalog),
        Err(error) => {
            checkpoint(&cancelled)?;
            catalog.warn(format!("Trash could not be inspected: {error:#}"));
            return Ok(catalog);
        }
    };
    // Directory iteration has no time ordering. Discover a bounded set before
    // applying the journal-inspection limit, so old restored batches cannot
    // crowd every newer recovery journal out of the catalog.
    let mut batches = Vec::new();
    let mut examined = 0;
    let mut seen = BTreeSet::new();
    for (index, item) in fs::read_dir(&locations.trash)?.enumerate() {
        checkpoint(&cancelled)?;
        if index >= MAX_DIRECTORY_ENTRIES {
            catalog.limit();
            break;
        }
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                catalog.warn(format!("Could not inspect a Trash item: {error}"));
                continue;
            }
        };
        if !item.file_name().to_string_lossy().starts_with("batch-") {
            catalog.warn(format!(
                "Unrecognized Trash item was kept for manual recovery: {}",
                item.path().display()
            ));
            continue;
        }
        let candidate = (|| -> Result<_> {
            let batch = checked_batch(&locations, &item.path())?;
            checkpoint(&cancelled)?;
            let path = batch.join("recovery.json");
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                no_reparse(&metadata) && metadata.is_file(),
                "Recovery journal is a link or is not a regular file"
            );
            ensure!(
                path.canonicalize()?.parent() == Some(batch.as_path()),
                "Recovery journal escapes its batch"
            );
            Ok((metadata.modified().ok(), batch))
        })();
        match candidate {
            Ok(candidate) => batches.push(candidate),
            Err(error) => {
                checkpoint(&cancelled)?;
                catalog.warn(format!(
                    "Recovery journal in {} was kept: {error:#}",
                    item.path().display()
                ));
            }
        }
    }
    checkpoint(&cancelled)?;
    batches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if batches.len() > MAX_BATCHES {
        catalog.limit();
    }
    for (_, batch) in batches.into_iter().take(MAX_BATCHES) {
        checkpoint(&cancelled)?;
        let result = (|| -> Result<()> {
            // Repeat validation after discovery; directories may have changed
            // while collecting and ordering the candidate journals.
            let batch = checked_batch(&locations, &batch)?;
            let (manifest, removed_at) = manifest(&batch, &cancelled)?;
            for (original, trashed) in manifest.moves {
                checkpoint(&cancelled)?;
                if examined >= MAX_INSPECTED_MOVES || catalog.entries.len() >= MAX_ENTRIES {
                    catalog.limit();
                    return Ok(());
                }
                examined += 1;
                match inspect(&locations, &batch, &original, &trashed) {
                    Ok(Some((bytes, conflict)))
                        if seen.insert((original.clone(), trashed.clone())) =>
                    {
                        catalog.entries.push(TrashEntry {
                            original,
                            trashed,
                            batch: batch.clone(),
                            bytes,
                            removed_at,
                            conflict,
                        })
                    }
                    Ok(_) => {}
                    Err(error) => catalog.warn(format!(
                        "Recovery entry in {} was kept but cannot be restored: {error:#}",
                        batch.display()
                    )),
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            checkpoint(&cancelled)?;
            catalog.warn(format!(
                "Recovery journal in {} was kept: {error:#}",
                batch.display()
            ));
        }
        if examined >= MAX_INSPECTED_MOVES || catalog.entries.len() >= MAX_ENTRIES {
            catalog.limit();
            break;
        }
    }
    checkpoint(&cancelled)?;
    catalog.finish();
    Ok(catalog)
}

pub fn restore_entry(
    root: &Path,
    entry: &TrashEntry,
    cancelled: impl Fn() -> bool,
) -> Result<RestoreOutcome> {
    let locations = locations(root, &cancelled)?.context("Trash folder is no longer available")?;
    let batch = checked_batch(&locations, &entry.batch)?;
    let (manifest, _) = manifest(&batch, &cancelled)?;
    ensure!(
        manifest
            .moves
            .iter()
            .any(|(original, trashed)| original == &entry.original && trashed == &entry.trashed),
        "Recovery entry is no longer recorded in its journal"
    );
    checkpoint(&cancelled)?;
    let Some((_, conflict)) = inspect(&locations, &batch, &entry.original, &entry.trashed)? else {
        return Ok(RestoreOutcome::NoLongerInTrash);
    };
    ensure!(
        !conflict,
        "A file already exists at the original location; both files were kept"
    );
    // Repeat parent/source validation immediately before the no-clobber move.
    // This is a managed local recovery tool, not a security boundary against an
    // adversary swapping directory junctions during the path-based move itself.
    plain_directory(&locations.trash)?;
    checked_batch(&locations, &batch)?;
    inspect(&locations, &batch, &entry.original, &entry.trashed)?
        .context("Trashed file is no longer available")?;
    checkpoint(&cancelled)?;
    recordings::restore(&mut vec![(entry.original.clone(), entry.trashed.clone())])?;
    Ok(RestoreOutcome::Restored)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture_root() -> (tempfile::TempDir, PathBuf) {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("captures");
        fs::create_dir(&root).unwrap();
        (folder, root.canonicalize().unwrap())
    }
    fn batch(root: &Path, name: &str) -> PathBuf {
        let path = root.join(".trash").join(format!("batch-{name}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn journal(batch: &Path, moves: &[(PathBuf, PathBuf)]) {
        fs::write(
            batch.join("recovery.json"),
            serde_json::to_vec(&serde_json::json!({ "version": 1, "moves": moves })).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn restart_scan_restores_nested_unicode_path_and_keeps_durable_journal() {
        let (_folder, root) = capture_root();
        let original = root.join("游戏 captures/été/精彩片段.MKV");
        fs::create_dir_all(original.parent().unwrap()).unwrap();
        fs::write(&original, b"owned original media").unwrap();
        let moved = recordings::trash(std::slice::from_ref(&original), &root).unwrap();
        let manifest = moved[0].1.parent().unwrap().join("recovery.json");
        let before = fs::read(&manifest).unwrap();
        drop(moved); // No session Undo survives this simulated restart.
        let catalog = scan(&root, || false).unwrap();
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        assert_eq!(catalog.entries.len(), 1);
        let entry = &catalog.entries[0];
        assert_eq!(entry.original, original);
        assert_eq!(entry.bytes, b"owned original media".len() as u64);
        assert!(!entry.conflict);
        assert_eq!(
            restore_entry(&root, entry, || false).unwrap(),
            RestoreOutcome::Restored
        );
        assert_eq!(fs::read(&original).unwrap(), b"owned original media");
        assert_eq!(fs::read(&manifest).unwrap(), before);
        assert!(scan(&root, || false).unwrap().entries.is_empty());
        assert_eq!(
            restore_entry(&root, entry, || false).unwrap(),
            RestoreOutcome::NoLongerInTrash
        );
    }
    #[test]
    fn original_collision_keeps_both_files_and_cancel_never_moves() {
        let (_folder, root) = capture_root();
        let original = root.join("clip.mkv");
        fs::write(&original, b"old clip").unwrap();
        recordings::trash(std::slice::from_ref(&original), &root).unwrap();
        let entry = scan(&root, || false).unwrap().entries.remove(0);
        assert!(restore_entry(&root, &entry, || true).is_err());
        assert!(entry.trashed.is_file());
        assert!(!original.exists());
        fs::write(&original, b"newer recording").unwrap();
        assert!(scan(&root, || false).unwrap().entries[0].conflict);
        assert!(restore_entry(&root, &entry, || false).is_err());
        assert_eq!(fs::read(&original).unwrap(), b"newer recording");
        assert_eq!(fs::read(&entry.trashed).unwrap(), b"old clip");
    }
    #[test]
    fn corrupt_traversal_outside_cross_batch_and_journal_sources_are_preserved_as_warnings() {
        let (folder, root) = capture_root();
        let corrupt = batch(&root, "corrupt");
        fs::write(corrupt.join("recovery.json"), b"{ damaged journal").unwrap();
        let unsafe_batch = batch(&root, "unsafe");
        let other = batch(&root, "other");
        journal(&other, &[]);
        let mut moves = Vec::new();
        for (index, original) in [
            root.join("../escape.mkv"),
            folder.path().join("outside.mkv"),
            root.join(".trash/inside.mkv"),
        ]
        .into_iter()
        .enumerate()
        {
            let source = unsafe_batch.join(format!("{index}.mkv"));
            fs::write(&source, b"kept source").unwrap();
            moves.push((original, source));
        }
        let other_source = other.join("other.mkv");
        fs::write(&other_source, b"other batch's media").unwrap();
        moves.push((root.join("safe.mkv"), other_source.clone()));
        moves.push((
            root.join("not-a-media-file.json"),
            unsafe_batch.join("RECOVERY.JSON"),
        ));
        journal(&unsafe_batch, &moves);
        let before = fs::read(unsafe_batch.join("recovery.json")).unwrap();
        let catalog = scan(&root, || false).unwrap();
        assert!(catalog.entries.is_empty());
        assert!(!catalog.warnings.is_empty());
        assert_eq!(
            fs::read(unsafe_batch.join("recovery.json")).unwrap(),
            before
        );
        assert_eq!(
            fs::read(corrupt.join("recovery.json")).unwrap(),
            b"{ damaged journal"
        );
        assert_eq!(fs::read(other_source).unwrap(), b"other batch's media");
        assert!(!root.join("safe.mkv").exists());
    }
    #[test]
    fn oversized_versioned_and_excessive_journals_are_never_executed() {
        let (_folder, root) = capture_root();
        let oversized = batch(&root, "oversized");
        fs::write(
            oversized.join("recovery.json"),
            vec![b'x'; MAX_MANIFEST_BYTES + 1],
        )
        .unwrap();
        let future = batch(&root, "future");
        fs::write(
            future.join("recovery.json"),
            b"{\"version\":2,\"moves\":[]}",
        )
        .unwrap();
        let excessive = batch(&root, "excessive");
        journal(
            &excessive,
            &vec![(root.join("source.mkv"), excessive.join("source.mkv")); MAX_MOVES_PER_BATCH + 1],
        );
        let catalog = scan(&root, || false).unwrap();
        assert!(catalog.entries.is_empty());
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.contains("byte limit"))
        );
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.contains("version 2"))
        );
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.contains("move limit"))
        );
        assert!(oversized.join("recovery.json").exists());
    }
    #[test]
    fn batch_limit_and_cancel_report_incomplete_scan_without_changes() {
        let (_folder, root) = capture_root();
        for index in 0..MAX_BATCHES + 1 {
            journal(&batch(&root, &index.to_string()), &[]);
        }
        let catalog = scan(&root, || false).unwrap();
        assert!(catalog.truncated);
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.contains("scan limit"))
        );
        assert!(catalog.warnings.len() <= MAX_WARNINGS);
        assert!(
            scan(&root.join("does-not-exist"), || true)
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        let calls = std::sync::atomic::AtomicUsize::new(0);
        assert!(
            scan(&root, || calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                >= 7)
            .is_err()
        );
        assert_eq!(
            fs::read_dir(root.join(".trash")).unwrap().count(),
            MAX_BATCHES + 1
        );
    }
    #[test]
    fn newest_live_journal_is_inspected_before_limit_despite_older_empty_batches() {
        let (_folder, root) = capture_root();
        let old_time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        let new_time = old_time + std::time::Duration::from_secs(600);
        let set_modified = |path: &Path, modified| {
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(modified))
                .unwrap();
        };
        for index in 0..MAX_BATCHES + 8 {
            let old = batch(&root, &format!("old-{index:04}"));
            journal(&old, &[]); // Journals remain after their files were restored.
            set_modified(&old.join("recovery.json"), old_time);
        }
        let newest = batch(&root, "zz-newest-live");
        let original = root.join("recent capture.mkv");
        let trashed = newest.join("recent capture.mkv");
        fs::write(&trashed, b"recoverable new capture").unwrap();
        journal(&newest, &[(original.clone(), trashed.clone())]);
        let manifest_path = newest.join("recovery.json");
        set_modified(&manifest_path, new_time);
        let before = fs::read(&manifest_path).unwrap();

        let catalog = scan(&root, || false).unwrap();
        assert!(catalog.truncated);
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].original, original);
        assert_eq!(catalog.entries[0].trashed, trashed);
        assert_eq!(catalog.entries[0].removed_at, Some(new_time));
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.contains("discovery limit"))
        );
        assert_eq!(fs::read(&manifest_path).unwrap(), before);
        assert_eq!(fs::read(&trashed).unwrap(), b"recoverable new capture");
        assert!(!original.exists());
        assert_eq!(
            fs::read_dir(root.join(".trash")).unwrap().count(),
            MAX_BATCHES + 9
        );
    }
    #[test]
    fn stale_or_forged_entry_cannot_restore_a_file_missing_from_its_journal() {
        let (_folder, root) = capture_root();
        let own_batch = batch(&root, "owned");
        let trashed = own_batch.join("clip.mkv");
        fs::write(&trashed, b"kept").unwrap();
        journal(&own_batch, &[]);
        let entry = TrashEntry {
            original: root.join("clip.mkv"),
            trashed: trashed.clone(),
            batch: own_batch,
            bytes: 4,
            removed_at: None,
            conflict: false,
        };
        assert!(restore_entry(&root, &entry, || false).is_err());
        assert_eq!(fs::read(trashed).unwrap(), b"kept");
    }
    #[test]
    fn missing_original_parent_warns_without_recreating_folders_or_moving_media() {
        let (_folder, root) = capture_root();
        let parent = root.join("removed folder");
        fs::create_dir(&parent).unwrap();
        let original = parent.join("clip.mkv");
        fs::write(&original, b"kept media").unwrap();
        let moved = recordings::trash(std::slice::from_ref(&original), &root).unwrap();
        fs::remove_dir(&parent).unwrap();
        let catalog = scan(&root, || false).unwrap();
        assert!(catalog.entries.is_empty());
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.contains("recreate"))
        );
        assert!(!parent.exists());
        assert_eq!(fs::read(&moved[0].1).unwrap(), b"kept media");
    }
    #[cfg(any(unix, windows))]
    #[test]
    fn original_folder_link_substituted_after_scan_is_rejected() {
        let (folder, root) = capture_root();
        let parent = root.join("nested");
        fs::create_dir(&parent).unwrap();
        let original = parent.join("clip.mkv");
        fs::write(&original, b"kept media").unwrap();
        recordings::trash(std::slice::from_ref(&original), &root).unwrap();
        let entry = scan(&root, || false).unwrap().entries.remove(0);
        fs::remove_dir(&parent).unwrap();
        let outside = folder.path().join("outside");
        fs::create_dir(&outside).unwrap();
        #[cfg(unix)]
        let link = std::os::unix::fs::symlink(&outside, &parent);
        #[cfg(windows)]
        let link = std::os::windows::fs::symlink_dir(&outside, &parent);
        if let Err(error) = link {
            eprintln!(
                "Directory-link substitution not exercised: symlink creation unavailable: {error}"
            );
            return;
        }
        assert!(restore_entry(&root, &entry, || false).is_err());
        assert!(!outside.join("clip.mkv").exists());
        assert_eq!(fs::read(&entry.trashed).unwrap(), b"kept media");
    }
}
