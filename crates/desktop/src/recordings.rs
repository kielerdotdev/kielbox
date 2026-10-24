use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

static ROOT: std::sync::OnceLock<std::sync::RwLock<PathBuf>> = std::sync::OnceLock::new();
pub fn root() -> PathBuf {
    ROOT.get_or_init(|| std::sync::RwLock::new(PathBuf::from("recordings")))
        .read()
        .unwrap()
        .clone()
}
pub fn set_root(path: PathBuf) {
    *ROOT
        .get_or_init(|| std::sync::RwLock::new(PathBuf::from("recordings")))
        .write()
        .unwrap() = path;
}
#[path = "shell_actions.rs"]
mod shell_actions;
pub(crate) use shell_actions::Kind as ShellAction;
pub(crate) use shell_actions::begin as begin_shell_action;
/// Run on a worker: directory preparation and shell startup may block on a drive.
pub(crate) fn try_open_folder(path: &Path) -> Result<(), String> {
    open_folder_with(path, |path| {
        std::process::Command::new("explorer.exe").arg(path).spawn()
            .map(|_| ()).map_err(|error| error.to_string())
    })
}
fn open_folder_with(path: &Path, launch: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|error| format!("Could not prepare capture folder: {error}"))?;
    let path = path.canonicalize().map_err(|error| format!("Could not locate capture folder: {error}"))?;
    launch(&path).map_err(|error| format!("Could not open capture folder: {error}"))
}
pub fn is_image(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "ssxe"
        )
    })
}
pub fn is_supported(path: &Path) -> bool {
    is_image(path)
        || path.extension().is_some_and(|extension| {
            matches!(
                extension.to_string_lossy().to_ascii_lowercase().as_str(),
                "mp4"
                    | "mkv"
                    | "webm"
                    | "ivf"
                    | "mov"
                    | "m4v"
                    | "avi"
                    | "wmv"
                    | "mpeg"
                    | "mpg"
                    | "m2ts"
                    | "mts"
                    | "ts"
                    | "ogv"
                    | "flv"
            )
        })
}
// Synchronous compatibility API; GPUI uses LibraryScanner instead.
#[allow(dead_code)]
pub fn list() -> Vec<PathBuf> {
    list_in(root())
}
fn list_in(root: PathBuf) -> Vec<PathBuf> {
    scan_in(root, || false)
        .and_then(Result::ok)
        .unwrap_or_default()
        .into_iter()
        .map(|capture| capture.path)
        .collect()
}

pub(crate) struct ScannedCapture {
    pub path: PathBuf,
    pub modified: Option<std::time::SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RootScanError {
    pub root: PathBuf,
    pub kind: std::io::ErrorKind,
}
fn scan_entry<T>(entry: std::io::Result<T>, folder: &Path, depth: usize) -> Result<Option<T>, RootScanError> {
    match entry {
        Ok(entry) => Ok(Some(entry)),
        Err(error) if depth == 0 => Err(RootScanError { root: folder.to_owned(), kind: error.kind() }),
        Err(_) => Ok(None),
    }
}

/// The caller controls cancellation and thread placement; no media is opened.
pub(crate) fn scan_in(root: PathBuf, cancelled: impl Fn() -> bool) -> Option<Result<Vec<ScannedCapture>, RootScanError>> {
    let mut paths = vec![];
    let mut directories = vec![(root, 0)];
    let mut visited = std::collections::BTreeSet::new();
    while let Some((folder, depth)) = directories.pop() {
        if cancelled() {
            return None;
        }
        let identity = folder.canonicalize().unwrap_or_else(|_| folder.clone());
        if !visited.insert(identity) {
            continue;
        }
        if cancelled() {
            return None;
        }
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if depth == 0 => {
                return if cancelled() { None } else { Some(Err(RootScanError { root: folder, kind: error.kind() })) };
            }
            Err(_) => continue,
        };
        for entry in entries {
            if cancelled() {
                return None;
            }
            let entry = match scan_entry(entry, &folder, depth) {
                Ok(Some(entry)) => entry,
                Ok(None) => continue,
                Err(error) => return if cancelled() { None } else { Some(Err(error)) },
            };
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if cancelled() {
                return None;
            }
            if metadata.is_dir() {
                if depth < 5 {
                    directories.push((path, depth + 1));
                }
                continue;
            }
            if metadata.is_file() && is_supported(&path) {
                paths.push(ScannedCapture {
                    path: path.canonicalize().unwrap_or(path),
                    modified: metadata.modified().ok(),
                });
            }
        }
    }
    if cancelled() {
        return None;
    }
    paths.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.path.cmp(&b.path))
    });
    Some(Ok(paths))
}

pub fn title(path: &Path) -> String {
    if is_image(path) {
        return path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
    }
    let stamp = std::fs::metadata(path)
        .and_then(|m| m.created().or_else(|_| m.modified()))
        .ok();
    #[cfg(windows)]
    if let Some(stamp) = stamp.and_then(|s| s.duration_since(UNIX_EPOCH).ok()) {
        use windows::Win32::{
            Foundation::{FILETIME, SYSTEMTIME},
            System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
        };
        let ticks = (stamp.as_nanos() / 100) as u64 + 116_444_736_000_000_000;
        let ft = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        // Use the operating system's timezone, including daylight saving time.
        if unsafe {
            FileTimeToSystemTime(&ft, &mut utc)
                .and_then(|_| SystemTimeToTzSpecificLocalTime(None, &utc, &mut local))
        }
        .is_ok()
        {
            let months = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            return format!(
                "Screen recording · {} {} {:02}:{:02}",
                local.wDay,
                months[usize::from(local.wMonth.saturating_sub(1)).min(11)],
                local.wHour,
                local.wMinute
            );
        }
    }
    "Screen recording".into()
}

pub fn edit_path(path: &Path) -> PathBuf {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    hashed_edit_path(&path, Path::new("projects/clips"))
}
fn hashed_edit_path(path: &Path, folder: &Path) -> PathBuf {
    let key = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
    folder.join(format!("{key}.json"))
}

/// Worker-only lookup: offline Windows sources lose canonical verbatim prefixes,
/// but their existing sidecars must still reach source validation and recovery.
/// Keep the established hash scheme and only inspect exact compatibility keys.
pub fn edit_path_for_open(path: &Path) -> PathBuf {
    edit_path_for_open_in(path, Path::new("projects/clips"))
}
fn edit_path_for_open_in(path: &Path, folder: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return hashed_edit_path(&canonical, folder);
    }
    let preferred = hashed_edit_path(path, folder);
    // Preserve an unreadable or otherwise damaged preferred entry for normal
    // error reporting; never hide it by selecting another version.
    if preferred.try_exists().unwrap_or(true) {
        return preferred;
    }
    for variant in offline_edit_path_variants(path) {
        let candidate = hashed_edit_path(&variant, folder);
        if candidate.try_exists().unwrap_or(false) {
            return candidate;
        }
    }
    preferred
}
#[cfg(windows)]
fn offline_edit_path_variants(path: &Path) -> Vec<PathBuf> {
    use std::path::{Component, Prefix};
    // Preserve every case-sensitive byte used by the historical hash. In
    // particular, session-key normalization must not change these names.
    let text = path.to_string_lossy();
    let ordinary = if let Some(rest) = text.strip_prefix("\\\\?\\UNC\\") {
        PathBuf::from(format!("\\\\{rest}"))
    } else {
        PathBuf::from(text.strip_prefix("\\\\?\\").unwrap_or(&text))
    };
    let ordinary = if ordinary.is_absolute() {
        ordinary
    } else {
        std::env::current_dir().unwrap_or_default().join(ordinary)
    };
    let text = ordinary.to_string_lossy();
    let verbatim = match ordinary.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(_) => Some(PathBuf::from(format!("\\\\?\\{text}"))),
            Prefix::UNC(_, _) => text
                .strip_prefix("\\\\")
                .map(|rest| PathBuf::from(format!("\\\\?\\UNC\\{rest}"))),
            _ => None,
        },
        _ => None,
    };
    std::iter::once(ordinary).chain(verbatim).collect()
}
#[cfg(not(windows))]
fn offline_edit_path_variants(path: &Path) -> Vec<PathBuf> {
    vec![path.to_owned()]
}

/// Preserve an unreadable edit sidecar before the explicit Open original action.
pub fn backup_edits(media: &Path) -> anyhow::Result<Option<PathBuf>> {
    backup_sidecar(&edit_path_for_open(media))
}
/// Worker-only recovery copy of the newest unsaved edits. Never publishes over
/// either the source or its existing sidecar, even if autosave is unavailable.
pub fn backup_pending_edits(
    media: &Path,
    project: &rcr_editor_core::Project,
) -> anyhow::Result<PathBuf> {
    backup_pending_sidecar(&edit_path_for_open(media), project)
}
fn backup_pending_sidecar(
    sidecar: &Path,
    project: &rcr_editor_core::Project,
) -> anyhow::Result<PathBuf> {
    use std::io::Write;
    project.validate()?;
    let folder = sidecar.parent().unwrap_or(Path::new(".")).join("recovery");
    std::fs::create_dir_all(&folder)?;
    let mut temporary = tempfile::NamedTempFile::new_in(&folder)?;
    serde_json::to_writer_pretty(&mut temporary, project)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos();
    let stem = sidecar.file_stem().unwrap_or_default().to_string_lossy();
    let destination = folder.join(format!("{stem}-{stamp}-unsaved.json"));
    temporary.persist_noclobber(&destination)?;
    Ok(destination)
}
fn backup_sidecar(sidecar: &Path) -> anyhow::Result<Option<PathBuf>> {
    use std::io::Write;
    let mut source = match std::fs::File::open(sidecar) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let folder = sidecar.parent().unwrap_or(Path::new(".")).join("recovery");
    std::fs::create_dir_all(&folder)?;
    let mut temporary = tempfile::NamedTempFile::new_in(&folder)?;
    std::io::copy(&mut source, &mut temporary)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos();
    let stem = sidecar.file_stem().unwrap_or_default().to_string_lossy();
    let destination = folder.join(format!("{stem}-{stamp}.json"));
    temporary.persist_noclobber(&destination)?;
    Ok(Some(destination))
}

/// Reversible removal. Originals remain in this library's .trash directory.
pub fn trash(paths: &[PathBuf], root: &Path) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    use anyhow::ensure;
    let root = root.canonicalize()?;
    let sources = paths
        .iter()
        .map(|p| p.canonicalize())
        .collect::<std::io::Result<Vec<_>>>()?;
    for source in &sources {
        ensure!(
            source.starts_with(&root)
                && source.is_file()
                && !source.starts_with(root.join(".trash")),
            "File is outside this Library"
        );
    }
    let trash_root = root.join(".trash");
    std::fs::create_dir_all(&trash_root)?;
    let folder = tempfile::Builder::new()
        .prefix("batch-")
        .tempdir_in(&trash_root)?
        .keep();
    let planned = sources
        .into_iter()
        .enumerate()
        .map(|(index, source)| {
            let mut name = std::ffi::OsString::from(format!("{index}-"));
            name.push(source.file_name().unwrap());
            let destination = folder.join(name);
            (source, destination)
        })
        .collect::<Vec<_>>();
    // Journal the original paths before moving any media. A crash can discard
    // session Undo, but must not discard the path needed to recover nested files.
    // A private directory and create_new avoid replacing another batch's journal.
    let mut manifest = std::fs::File::create_new(folder.join("recovery.json"))?;
    serde_json::to_writer_pretty(
        &mut manifest,
        &serde_json::json!({
            "version": 1,
            "moves": planned,
        }),
    )?;
    std::io::Write::flush(&mut manifest)?;
    manifest.sync_all()?;
    drop(manifest);
    let mut moved: Vec<(PathBuf, PathBuf)> = vec![];
    for (source, destination) in planned {
        if let Err(error) = move_file_noclobber(&source, &destination) {
            for (from, to) in moved.iter().rev() {
                let _ = move_file_noclobber(to, from);
            }
            return Err(error.context(format!(
                "Trash operation did not finish; recovery paths are recorded in {}",
                folder.join("recovery.json").display(),
            )));
        }
        moved.push((source, destination));
    }
    Ok(moved)
}
pub fn restore(moved: &mut Vec<(PathBuf, PathBuf)>) -> anyhow::Result<()> {
    while let Some((source, destination)) = moved.last() {
        anyhow::ensure!(
            !source.exists(),
            "A file already exists at the original location"
        );
        move_file_noclobber(destination, source)?;
        moved.pop();
    }
    Ok(())
}

fn move_file_noclobber(source: &Path, destination: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    let mut source_path = tempfile::TempPath::try_from_path(source)?;
    // This wrapper owns no deletion rights: on failure both files stay intact.
    source_path.disable_cleanup(true);
    source_path
        .persist_noclobber(destination)
        .with_context(|| {
            format!(
                "could not move {} to {} without replacing a file",
                source.display(),
                destination.display(),
            )
        })?;
    Ok(())
}

/// Build CF_HDROP without converting Windows paths through lossy UTF-8.
#[cfg(any(windows, test))]
fn dropfiles_payload(path: &[u16]) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(!path.is_empty() && path.len() <= 32768 && !path.contains(&0), "Invalid or oversized clipboard file path");
    let mut names = if let Some(rest) = path.strip_prefix(&[92, 92, 63, 92, 85, 78, 67, 92]) {
        [vec![92, 92], rest.to_vec()].concat()
    } else { path.strip_prefix(&[92, 92, 63, 92]).unwrap_or(path).to_vec() };
    names.extend([0, 0]);
    // DROPFILES: byte offset, POINT, fNC, fWide, then double-NUL UTF-16.
    let mut bytes = vec![0u8; 20 + names.len() * 2];
    bytes[..4].copy_from_slice(&20u32.to_le_bytes());
    bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
    for (out, value) in bytes[20..].as_chunks_mut::<2>().0.iter_mut().zip(names) {
        out.copy_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

#[cfg(windows)]
fn copy_file_with(path: &Path, owner: isize, write: impl FnOnce(isize, &[u8]) -> anyhow::Result<()>) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    anyhow::ensure!(owner != 0, "The editor window is no longer available");
    let resolved = path.canonicalize().map_err(|error| anyhow::anyhow!("Could not locate {}: {error}", path.display()))?;
    let payload = dropfiles_payload(&resolved.as_os_str().encode_wide().collect::<Vec<_>>())?;
    write(owner, &payload)
}

/// Worker-only: copy an owned source using the HWND captured from its GPUI window.
#[cfg(windows)]
pub(crate) fn copy_file_owned(path: &Path, owner: isize) -> anyhow::Result<()> {
    copy_file_with(path, owner, write_file_clipboard)
}
#[cfg(windows)]
fn write_file_clipboard(owner: isize, bytes: &[u8]) -> anyhow::Result<()> {
    write_clipboard_owned(owner, 15, bytes)
}
/// Worker-only, immediate transfer of a prepared CF_HDROP/CF_UNICODETEXT buffer.
#[cfg(windows)]
pub(crate) fn write_clipboard_owned(owner: isize, format: u32, bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(owner != 0, "The editor window is no longer available");
    anyhow::ensure!(matches!(format, 13 | 15) && !bytes.is_empty(), "Unsupported clipboard payload");
    use windows::Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND},
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock},
        },
    };
    struct Memory(Option<HGLOBAL>);
    impl Drop for Memory { fn drop(&mut self) { if let Some(memory) = self.0.take() { let _ = unsafe { GlobalFree(Some(memory)) }; } } }
    struct Clipboard;
    impl Drop for Clipboard { fn drop(&mut self) { let _ = unsafe { CloseClipboard() }; } }
    // Allocate before EmptyClipboard; transfer allocation ownership only after
    // Windows actually accepts the prepared format. No retries or UI waits.
    unsafe {
        let mut memory = Memory(Some(GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len())?));
        let data = GlobalLock(memory.0.unwrap());
        if data.is_null() {
            anyhow::bail!("Could not lock clipboard memory");
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.cast(), bytes.len());
        let _ = GlobalUnlock(memory.0.unwrap());
        OpenClipboard(Some(HWND(owner as *mut std::ffi::c_void)))
            .map_err(|error| anyhow::anyhow!("Clipboard is busy or the editor window closed: {error}"))?;
        let clipboard = Clipboard;
        EmptyClipboard().map_err(|error| anyhow::anyhow!("Could not clear clipboard: {error}"))?;
        SetClipboardData(format, Some(HANDLE(memory.0.unwrap().0)))
            .map_err(|error| anyhow::anyhow!("Could not transfer clipboard data: {error}"))?;
        memory.0 = None;
        let result = CloseClipboard().map_err(|error| anyhow::anyhow!("Could not finish clipboard copy: {error}"));
        std::mem::forget(clipboard);
        result?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropfiles_payload_preserves_utf16_unc_and_exact_double_terminator() {
        for (input, expected) in [
            (r"\\?\C:\clips\game.mp4", r"C:\clips\game.mp4"),
            (r"\\?\UNC\Server\Share\clip.mp4", r"\\Server\Share\clip.mp4"),
            (r"C:\clips\spaces, commas.mp4", r"C:\clips\spaces, commas.mp4"),
        ] {
            let payload = dropfiles_payload(&input.encode_utf16().collect::<Vec<_>>()).unwrap();
            assert_eq!(&payload[..4], &20u32.to_le_bytes());
            assert_eq!(&payload[16..20], &1u32.to_le_bytes());
            assert!(payload[4..16].iter().all(|byte| *byte == 0));
            let units = payload[20..].as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes).collect::<Vec<_>>();
            assert_eq!(units, expected.encode_utf16().chain([0, 0]).collect::<Vec<_>>());
        }
        let unpaired = [67, 58, 92, 0xd800, 46, 109, 112, 52];
        let payload = dropfiles_payload(&unpaired).unwrap();
        assert_eq!(&payload[26..28], &0xd800u16.to_le_bytes(), "Unpaired Windows UTF-16 was replaced");
        assert!(dropfiles_payload(&[67, 0, 92]).is_err());
        assert!(dropfiles_payload(&vec![65; 32769]).is_err());
        assert!(dropfiles_payload(&[]).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn file_clipboard_preparation_retains_owned_window_source_and_real_sink_error() {
        use std::os::windows::ffi::OsStrExt;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("owned clip, with spaces.mp4");
        std::fs::write(&source, b"original media").unwrap();
        let expected = dropfiles_payload(&source.canonicalize().unwrap().as_os_str().encode_wide().collect::<Vec<_>>()).unwrap();
        let error = copy_file_with(&source, 42, |owner, payload| {
            assert_eq!(owner, 42, "Captured HWND changed during worker preparation");
            assert_eq!(payload, expected.as_slice());
            anyhow::bail!("Injected clipboard ownership failure")
        }).unwrap_err();
        assert!(error.to_string().contains("Injected clipboard ownership failure"));
        assert_eq!(std::fs::read(&source).unwrap(), b"original media");
        assert!(copy_file_with(&source, 0, |_, _| panic!("Invalid owner reached clipboard")).is_err());
        assert!(copy_file_with(&directory.path().join("missing.mp4"), 42, |_, _| panic!("Missing source reached clipboard")).is_err());
    }

    #[test]
    fn root_iteration_errors_are_typed_while_nested_errors_remain_scoped() {
        let root = Path::new("owned-root");
        let error = scan_entry::<()>(Err(std::io::ErrorKind::PermissionDenied.into()), root, 0).unwrap_err();
        assert_eq!(error.root, root);
        assert_eq!(error.kind, std::io::ErrorKind::PermissionDenied);
        assert!(scan_entry::<()>(Err(std::io::ErrorKind::NotFound.into()), root, 1).unwrap().is_none());
        assert_eq!(scan_entry(Ok(42), root, 0).unwrap(), Some(42));
    }

    #[test]
    fn folder_open_preparation_and_launcher_errors_preserve_owned_original() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.mp4");
        std::fs::write(&original, b"preserved original").unwrap();
        let called = std::cell::Cell::new(false);
        let result = open_folder_with(&original, |_| { called.set(true); Ok(()) });
        assert!(result.unwrap_err().contains("prepare capture folder"));
        assert!(!called.get(), "A failed directory preparation must not invoke the shell");
        assert_eq!(std::fs::read(&original).unwrap(), b"preserved original");
        let folder = directory.path().join("new-capture-folder");
        let result = open_folder_with(&folder, |resolved| {
            assert!(resolved.is_absolute());
            assert_eq!(resolved, folder.canonicalize().unwrap());
            Err("owned injected launch failure".into())
        });
        assert!(result.unwrap_err().contains("owned injected launch failure"));
        assert!(folder.is_dir());
        assert_eq!(std::fs::read(original).unwrap(), b"preserved original");
    }
    #[cfg(windows)]
    #[test]
    fn offline_sidecar_variants_preserve_disk_and_unc_path_case() {
        for (ordinary, verbatim) in [
            (
                r"C:\Users\MixedCase\Clip.MKV",
                r"\\?\C:\Users\MixedCase\Clip.MKV",
            ),
            (
                r"\\Server\Share\MixedCase\Clip.MKV",
                r"\\?\UNC\Server\Share\MixedCase\Clip.MKV",
            ),
        ] {
            let expected = vec![PathBuf::from(ordinary), PathBuf::from(verbatim)];
            assert_eq!(offline_edit_path_variants(Path::new(ordinary)), expected);
            assert_eq!(offline_edit_path_variants(Path::new(verbatim)), expected);
        }
    }
    #[cfg(windows)]
    #[test]
    fn offline_source_finds_existing_canonical_sidecar_without_changing_bytes() {
        let folder = tempfile::tempdir().unwrap();
        let media = folder.path().join("MixedCaseRecording.MKV");
        std::fs::write(&media, b"source fixture").unwrap();
        let canonical = media.canonicalize().unwrap();
        let ordinary = offline_edit_path_variants(&canonical).remove(0);
        let sidecars = folder.path().join("edits");
        std::fs::create_dir(&sidecars).unwrap();
        let canonical_sidecar = hashed_edit_path(&canonical, &sidecars);
        let exact_bytes = b"{damaged edits still need recovery\0\xff";
        std::fs::write(&canonical_sidecar, exact_bytes).unwrap();
        std::fs::rename(&media, folder.path().join("temporarily-offline.MKV")).unwrap();

        assert_eq!(
            edit_path_for_open_in(&ordinary, &sidecars),
            canonical_sidecar
        );
        assert_eq!(std::fs::read(&canonical_sidecar).unwrap(), exact_bytes);
        let ordinary_sidecar = hashed_edit_path(&ordinary, &sidecars);
        assert_ne!(ordinary_sidecar, canonical_sidecar);
        assert!(!ordinary_sidecar.exists());
        // A distinct preferred version must never be hidden by compatibility lookup.
        std::fs::write(&ordinary_sidecar, b"preferred edits").unwrap();
        assert_eq!(
            edit_path_for_open_in(&ordinary, &sidecars),
            ordinary_sidecar
        );
        assert_eq!(std::fs::read(canonical_sidecar).unwrap(), exact_bytes);
    }
    #[test]
    fn pending_recovery_keeps_disk_bytes_and_separate_unsaved_versions() {
        let folder = tempfile::tempdir().unwrap();
        let sidecar = folder.path().join("saved.json");
        let bytes = b"old disk version";
        std::fs::write(&sidecar, bytes).unwrap();
        let mut project = rcr_editor_core::Project::default();
        let first = backup_pending_sidecar(&sidecar, &project).unwrap();
        project.width = 1280;
        let second = backup_pending_sidecar(&sidecar, &project).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&sidecar).unwrap(), bytes);
        assert_eq!(rcr_editor_core::Project::load(&first).unwrap().width, 2560);
        assert_eq!(rcr_editor_core::Project::load(&second).unwrap().width, 1280);
    }
    #[test]
    fn recovering_unreadable_edits_keeps_exact_bytes_and_never_replaces_backups() {
        let folder = tempfile::tempdir().unwrap();
        let sidecar = folder.path().join("damaged.json");
        assert!(backup_sidecar(&sidecar).unwrap().is_none());
        let bytes = b"{unreadable original edits\0\xff";
        std::fs::write(&sidecar, bytes).unwrap();
        let first = backup_sidecar(&sidecar).unwrap().unwrap();
        let second = backup_sidecar(&sidecar).unwrap().unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&sidecar).unwrap(), bytes);
        assert_eq!(std::fs::read(first).unwrap(), bytes);
        assert_eq!(std::fs::read(second).unwrap(), bytes);
    }
    #[test]
    fn library_discovers_nested_images_and_video_but_omits_trash() {
        let dir = tempfile::tempdir().unwrap();
        for relative in [
            "Screenshots/2026/screen.PNG",
            "exports/clip.mp4",
            ".trash/deleted.png",
            "preferences.json",
        ] {
            let path = dir.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }
        let found = list_in(dir.path().to_owned());
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|p| p.file_name().unwrap() == "screen.PNG"));
        assert!(found.iter().any(|p| p.file_name().unwrap() == "clip.mp4"));
    }
    #[test]
    fn library_removal_is_reversible_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("clip.mkv");
        std::fs::write(&original, b"original").unwrap();
        let mut moved = trash(std::slice::from_ref(&original), dir.path()).unwrap();
        assert!(!original.exists());
        assert_eq!(std::fs::read(&moved[0].1).unwrap(), b"original");
        std::fs::write(&original, b"replacement").unwrap();
        assert!(restore(&mut moved).is_err());
        assert_eq!(moved.len(), 1);
        assert_eq!(std::fs::read(&original).unwrap(), b"replacement");
        std::fs::remove_file(&original).unwrap();
        restore(&mut moved).unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), b"original");
        assert!(moved.is_empty());
        let outside = tempfile::NamedTempFile::new().unwrap();
        assert!(trash(&[outside.path().to_owned()], dir.path()).is_err());
    }

    #[test]
    fn trash_journals_nested_original_paths_before_session_undo_can_be_lost() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("Screenshots/2026/日本語.png");
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::fs::write(&original, b"original screenshot").unwrap();
        let canonical_original = original.canonicalize().unwrap();
        let moved = trash(std::slice::from_ref(&original), directory.path()).unwrap();
        let manifest_path = moved[0].1.parent().unwrap().join("recovery.json");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["version"], 1);
        let journaled: Vec<(PathBuf, PathBuf)> =
            serde_json::from_value(manifest["moves"].clone()).unwrap();
        assert_eq!(journaled, moved);
        assert_eq!(journaled[0].0, canonical_original);
        assert_eq!(
            std::fs::read(&journaled[0].1).unwrap(),
            b"original screenshot"
        );
    }

    #[test]
    fn no_clobber_move_preserves_both_files_when_destination_exists() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("recoverable.mkv");
        let destination = directory.path().join("new-recording.mkv");
        std::fs::write(&source, b"recoverable original").unwrap();
        std::fs::write(&destination, b"new recording").unwrap();
        assert!(move_file_noclobber(&source, &destination).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"recoverable original");
        assert_eq!(std::fs::read(&destination).unwrap(), b"new recording");
    }
}
