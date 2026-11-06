//! Filesystem ownership for disposable seek copies. No recursive deletion.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs::{self, File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}};

pub(super) const SCHEMA: u32 = 1;
const NAMESPACE: &str = "seek-v3-managed";
const ROOT_OWNER: &str = "rcr-seek-cache-owner.json";
const MAX_CLAIM_BYTES: u64 = 256 * 1024;

#[derive(Serialize, Deserialize)]
struct RootOwner { format: String, schema: u32 }
#[derive(Serialize, Deserialize)]
struct PartialOwner { format: String, schema: u32, output: String }

fn reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
}

pub(super) fn regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file() && !reparse(&metadata))
}
pub(super) fn present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn ordinary_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() { continue; }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => ensure!(!reparse(&metadata), "Seek cache path contains a reparse point: {}", ancestor.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn managed_root(base: &Path) -> Result<PathBuf> {
    let base = if base.is_absolute() { base.to_path_buf() } else { std::env::current_dir()?.join(base) };
    ordinary_ancestors(&base)?;
    fs::create_dir_all(&base)?;
    ordinary_ancestors(&base)?;
    let root = base.join(NAMESPACE);
    match fs::create_dir(&root) {
        Ok(()) => {
            let path = root.join(ROOT_OWNER);
            let mut owner = match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(owner) => owner,
                Err(error) => { let _ = fs::remove_dir(&root); return Err(error.into()); }
            };
            let result = (|| -> Result<()> {
                serde_json::to_writer(&mut owner, &RootOwner { format: "rcr-seek-cache".into(), schema: SCHEMA })?;
                owner.flush()?;
                owner.sync_all()?;
                Ok(())
            })();
            drop(owner);
            if let Err(error) = result {
                // Only our exclusively created, empty namespace can be removed.
                // Never recursively remove a directory, even during initialization.
                let _ = fs::remove_file(path);
                let _ = fs::remove_dir(&root);
                return Err(error);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(error) => return Err(error.into()),
    }
    ordinary_ancestors(&root)?;
    ensure!(fs::symlink_metadata(&root)?.is_dir(), "Seek cache namespace is not a directory");
    let owner = root.join(ROOT_OWNER);
    ensure!(regular(&owner), "Seek cache namespace has no regular ownership sentinel; existing files were left untouched");
    let owner: RootOwner = serde_json::from_reader(File::open(owner)?.take(MAX_CLAIM_BYTES))
        .context("Seek cache namespace has an invalid ownership sentinel; existing files were left untouched")?;
    ensure!(owner.format == "rcr-seek-cache" && owner.schema == SCHEMA, "Seek cache namespace ownership schema is unsupported");
    Ok(root)
}

fn claim_path(output: &Path) -> Option<PathBuf> {
    Some(output.with_file_name(format!("{}.owner.json", output.file_name()?.to_str()?)))
}
pub(super) fn claim_bytes(output: &Path) -> u64 {
    claim_path(output).filter(|path| regular(path)).and_then(|path| path.metadata().ok()).map_or(0, |metadata| metadata.len())
}
fn valid_partial_owner(output: &Path) -> bool {
    let Some(name) = output.file_name().and_then(|name| name.to_str()) else { return false; };
    if !name.starts_with("rcr-partial-") { return false; }
    let Some(claim) = claim_path(output).filter(|path| regular(path)) else { return false; };
    let Ok(file) = File::open(claim) else { return false; };
    let Ok(owner) = serde_json::from_reader::<_, PartialOwner>(file.take(MAX_CLAIM_BYTES)) else { return false; };
    owner.format == "rcr-seek-partial" && owner.schema == SCHEMA && owner.output == name
}

pub(super) struct Partial {
    output: tempfile::TempPath,
    claim: PathBuf,
}
impl Partial {
    pub(super) fn create(root: &Path, suffix: &str) -> Result<Self> {
        let output = tempfile::Builder::new().prefix("rcr-partial-").suffix(suffix).tempfile_in(root)?.into_temp_path();
        let claim = claim_path(&output).context("Invalid seek partial filename")?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(&claim)?;
        let result = (|| -> Result<()> {
            serde_json::to_writer(&mut file, &PartialOwner { format: "rcr-seek-partial".into(), schema: SCHEMA,
                output: output.file_name().unwrap().to_string_lossy().into_owned() })?;
            file.flush()?;
            file.sync_all()?;
            Ok(())
        })();
        drop(file);
        if let Err(error) = result {
            let _ = fs::remove_file(&claim);
            return Err(error);
        }
        Ok(Self { output, claim })
    }
    pub(super) fn path(&self) -> &Path { &self.output }
    pub(super) fn metadata(&self) -> std::io::Result<fs::Metadata> { self.output.metadata() }
}
impl Drop for Partial {
    fn drop(&mut self) {
        // The output itself is also owned by TempPath. The sidecar is never a
        // recursive directory and is deleted only with our matching claim.
        if valid_partial_owner(&self.output) { let _ = fs::remove_file(&self.claim); }
    }
}

/// Caller holds both global index and encoder locks; active builds cannot race.
pub(super) fn remove_partial(output: &Path) -> bool {
    if !regular(output) || !valid_partial_owner(output) { return false; }
    let Ok(file) = OpenOptions::new().read(true).write(true).open(output) else { return false; };
    if file.try_lock().is_err() || fs::remove_file(output).is_err() { return false; }
    if let Some(claim) = claim_path(output) { let _ = fs::remove_file(claim); }
    true
}

pub(super) fn remove_orphan_claim(claim: &Path) -> bool {
    let Some(name) = claim.file_name().and_then(|name| name.to_str()).and_then(|name| name.strip_suffix(".owner.json")) else { return false; };
    let output = claim.with_file_name(name);
    if !matches!(fs::symlink_metadata(&output), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        || !valid_partial_owner(&output) { return false; }
    fs::remove_file(claim).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{NAMESPACE, ROOT_OWNER, managed_root, regular};
    use std::fs;

    #[test]
    fn managed_namespace_is_new_reusable_and_does_not_adopt_legacy_or_foreign_files() {
        let base = tempfile::tempdir().unwrap();
        let legacy = base.path().join("seek-v2");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("old-proxy.mkv"), b"legacy cache retained").unwrap();
        fs::write(base.path().join("original.mkv"), b"original media").unwrap();
        let root = managed_root(base.path()).unwrap();
        assert_eq!(root, base.path().join(NAMESPACE));
        assert_eq!(managed_root(base.path()).unwrap(), root);
        assert!(regular(&root.join(ROOT_OWNER)));
        assert_eq!(fs::read(legacy.join("old-proxy.mkv")).unwrap(), b"legacy cache retained");
        assert_eq!(fs::read(base.path().join("original.mkv")).unwrap(), b"original media");
        assert_eq!(fs::read_dir(root).unwrap().count(), 1);
    }

    #[test]
    fn preexisting_unmarked_or_invalid_namespace_is_never_claimed() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join(NAMESPACE);
        fs::create_dir(&root).unwrap();
        let original = root.join("original.mkv");
        fs::write(&original, b"unrelated original").unwrap();
        assert!(managed_root(base.path()).is_err());
        assert!(!root.join(ROOT_OWNER).exists());
        fs::write(root.join(ROOT_OWNER), b"not a valid ownership claim").unwrap();
        assert!(managed_root(base.path()).is_err());
        assert_eq!(fs::read(original).unwrap(), b"unrelated original");
        assert_eq!(fs::read(root.join(ROOT_OWNER)).unwrap(), b"not a valid ownership claim");
        let future = br#"{"format":"rcr-seek-cache","schema":999}"#;
        fs::write(root.join(ROOT_OWNER), future).unwrap();
        assert!(managed_root(base.path()).is_err());
        assert_eq!(fs::read(root.join(ROOT_OWNER)).unwrap(), future);
    }

    #[test]
    fn reparse_namespaces_and_entries_are_rejected_when_symlink_creation_is_available() {
        let base = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let namespace = base.path().join(NAMESPACE);
        if let Err(error) = std::os::windows::fs::symlink_dir(target.path(), &namespace) {
            if error.kind() == std::io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314) { return; }
            panic!("Cannot create isolated symlink fixture: {error}");
        }
        assert!(managed_root(base.path()).is_err());
        let original = target.path().join("original.mkv");
        fs::write(&original, b"preserve link target").unwrap();
        let linked = base.path().join("linked.mkv");
        std::os::windows::fs::symlink_file(&original, &linked).unwrap();
        assert!(!regular(&linked));
        assert_eq!(fs::read(original).unwrap(), b"preserve link target");
        assert!(!target.path().join(ROOT_OWNER).exists());
    }
}
