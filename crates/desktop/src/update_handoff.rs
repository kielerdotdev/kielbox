//! Keep normal launches out of an installation while the native updater applies.
//! The desktop's global user mutex must be held before startup inspection.
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Component, Path, PathBuf},
};
use windows::Win32::{
    Foundation::{GENERIC_READ, HANDLE},
    Storage::FileSystem::{
        DELETE, FILE_DISPOSITION_INFO, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        FileDispositionInfo, SetFileInformationByHandle,
    },
};

const MAX_MARKER_BYTES: u64 = 64 * 1024;
const MARKER_FILE: &str = "update-handoff.json";
const REPARSE_POINT: u32 = 0x400;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema_version: u32,
    id: String,
    target_version: String,
    current_directory: PathBuf,
    installer: PathBuf,
}

struct Loaded {
    marker: Marker,
    bytes: Vec<u8>,
}

pub struct Guard {
    path: PathBuf,
    bytes: Vec<u8>,
    committed: bool,
}

impl Guard {
    /// Keep the startup gate only after the native installer is launched.
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = remove_unchanged(&self.path, &self.bytes);
        }
    }
}

/// Publish immediately before launching the verified Setup.exe, while the desktop
/// still owns its global instance guard. An unsuccessful launch drops this guard.
pub fn begin_with_installer(
    target_version: &str,
    current_directory: &Path,
    installer: &Path,
) -> Result<Guard, String> {
    let current_directory = current_directory
        .canonicalize()
        .map_err(|error| format!("Could not resolve the installation for updating: {error}"))?;
    let own_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .and_then(|path| path.canonicalize().ok())
        .ok_or_else(|| {
            "Could not resolve this application's installation before updating.".to_owned()
        })?;
    if !same_path(&current_directory, &own_directory) {
        return Err(
            "The selected update installation does not match this running application.".into(),
        );
    }
    let installer = installer
        .canonicalize()
        .map_err(|error| format!("Could not resolve the downloaded installer: {error}"))?;
    if !installer.is_file()
        || installer
            .extension()
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("exe"))
        || installer.starts_with(&current_directory)
    {
        return Err(
            "The downloaded installer must be an executable outside the application folder.".into(),
        );
    }
    publish(
        &marker_path()?,
        target_version,
        current_directory,
        installer,
    )
}

/// Return a recoverable notice if a previous attempt has ended without starting
/// its target. A live/ambiguous helper defers startup; it is never stopped here.
pub fn check_startup(installed_version: Option<&str>) -> Result<Option<String>, String> {
    let own_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .and_then(|path| path.canonicalize().ok());
    inspect(
        &marker_path()?,
        installed_version,
        own_directory.as_deref(),
        helper_running,
    )
}

fn marker_path() -> Result<PathBuf, String> {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            "Could not resolve Windows user state for the update startup check.".to_owned()
        })?;
    Ok(local.join("KielboxData").join(MARKER_FILE))
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 128
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

fn valid_directory(directory: &Path) -> bool {
    directory.is_absolute()
        && directory.file_name().is_some()
        && !directory
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
}

fn publish(
    path: &Path,
    target_version: &str,
    current_directory: PathBuf,
    installer: PathBuf,
) -> Result<Guard, String> {
    if !valid_version(target_version)
        || !valid_directory(&current_directory)
        || !valid_directory(&installer)
    {
        return Err("The update handoff contains an invalid version or installation path.".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "The update handoff path has no parent.".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create the update handoff folder: {error}"))?;
    let mut staged = tempfile::Builder::new()
        .prefix(".update-handoff-")
        .suffix(".json")
        .tempfile_in(parent)
        .map_err(|error| format!("Could not stage the update startup gate: {error}"))?;
    let id = staged
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Could not identify the staged update handoff.".to_owned())?
        .to_owned();
    let marker = Marker {
        schema_version: 2,
        id,
        target_version: target_version.into(),
        current_directory,
        installer,
    };
    let bytes = serde_json::to_vec(&marker)
        .map_err(|error| format!("Could not encode the update startup gate: {error}"))?;
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        return Err("The update startup gate exceeds its size limit.".into());
    }
    staged
        .write_all(&bytes)
        .and_then(|()| staged.flush())
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|error| format!("Could not durably save the update startup gate: {error}"))?;
    staged.persist_noclobber(path)
        .map_err(|error| format!("An update handoff already exists or cannot be published. Restart Kielbox before retrying: {}", error.error))?;
    Ok(Guard {
        path: path.to_path_buf(),
        bytes,
        committed: false,
    })
}

fn read_bytes(file: &mut File) -> Result<Vec<u8>, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not inspect the update startup gate: {error}"))?;
    if !metadata.is_file() || metadata.file_attributes() & REPARSE_POINT != 0 {
        return Err(
            "The update startup gate is not a regular owned file. The original was preserved."
                .into(),
        );
    }
    if metadata.len() > MAX_MARKER_BYTES {
        return Err("The update startup gate exceeds 64 KiB. The original was preserved.".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the update startup gate: {error}"))?;
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        return Err("The update startup gate exceeds 64 KiB. The original was preserved.".into());
    }
    Ok(bytes)
}

fn load(path: &Path) -> Result<Option<Loaded>, String> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Could not read the update startup gate at {}: {error}. Retry after the update finishes.",
                path.display()
            ));
        }
    };
    let bytes = read_bytes(&mut file)?;
    let marker: Marker = serde_json::from_slice(&bytes)
        .map_err(|_| "The update startup gate is invalid. The original was preserved; repair the installation before retrying.".to_owned())?;
    if marker.schema_version != 2
        || marker.id.is_empty()
        || marker.id.len() > 128
        || marker.id.chars().any(char::is_control)
        || !valid_version(&marker.target_version)
        || !valid_directory(&marker.current_directory)
        || !valid_directory(&marker.installer)
    {
        return Err("The update startup gate has unsupported or invalid fields. The original was preserved.".into());
    }
    Ok(Some(Loaded { marker, bytes }))
}

/// Delete by the compared file handle while excluding replacement/writes. A path
/// compare followed by remove_file would leave a marker-replacement race.
fn remove_unchanged(path: &Path, expected: &[u8]) -> Result<bool, String> {
    let mut file = match OpenOptions::new()
        .access_mode(GENERIC_READ.0 | DELETE.0)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Could not retire the update startup gate: {error}. Retry after the updater finishes."
            ));
        }
    };
    if read_bytes(&mut file)? != expected {
        return Ok(false);
    }
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(|error| format!("Could not retire the owned update startup gate: {error}"))?;
    drop(file);
    Ok(true)
}

fn inspect(
    path: &Path,
    installed_version: Option<&str>,
    own_directory: Option<&Path>,
    mut is_running: impl FnMut(&Path) -> Result<bool, String>,
) -> Result<Option<String>, String> {
    let Some(loaded) = load(path)? else {
        return Ok(None);
    };
    if installed_version == Some(loaded.marker.target_version.as_str())
        && own_directory.is_some_and(|own| same_path(own, &loaded.marker.current_directory))
    {
        if !remove_unchanged(path, &loaded.bytes)? {
            return Err("The update startup gate changed. Retry launching Kielbox.".into());
        }
        return Ok(None);
    }
    if is_running(&loaded.marker.installer)? {
        return Err("Kielbox is installing an update. Wait for it to finish, then open Kielbox again. No recording or other process was stopped.".into());
    }
    if !remove_unchanged(path, &loaded.bytes)? {
        return Err("The update startup gate changed. Retry launching Kielbox.".into());
    }
    Ok(Some("The previous update did not start its target version. You can keep using this installation and retry from Settings.".into()))
}

fn same_path(left: &Path, right: &Path) -> bool {
    fn normalized(path: &Path) -> String {
        let value = path.to_string_lossy().replace('/', "\\");
        let value = if let Some(unc) = value.strip_prefix("\\\\?\\UNC\\") {
            format!("\\\\{unc}")
        } else {
            value.strip_prefix("\\\\?\\").unwrap_or(&value).to_owned()
        };
        value.trim_end_matches('\\').to_lowercase()
    }
    normalized(left) == normalized(right)
}

fn helper_running(expected: &Path) -> Result<bool, String> {
    use std::{
        ffi::OsString,
        os::windows::ffi::OsStringExt,
        time::{Duration, Instant},
    };
    use windows::{
        Win32::{
            Foundation::{
                CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, WAIT_OBJECT_0,
                WAIT_TIMEOUT,
            },
            System::{
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                    TH32CS_SNAPPROCESS,
                },
                Threading::{
                    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                    PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
                },
            },
        },
        core::PWSTR,
    };
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let snapshot = Handle(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(|error| {
            format!("Could not inspect the active updater: {error}. Retry shortly.")
        })?,
    );
    let started = Instant::now();
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut next = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    let mut count = 0;
    let mut buffer = vec![0u16; 32_768];
    loop {
        match next {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(false),
            Err(error) => {
                return Err(format!(
                    "Could not finish checking the updater: {error}. Retry shortly."
                ));
            }
        }
        count += 1;
        if count > 32_768 || started.elapsed() > Duration::from_secs(3) {
            return Err("The active updater could not be verified within the startup-check budget. Retry shortly.".into());
        }
        let length = entry
            .szExeFile
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(entry.szExeFile.len());
        let candidate = expected.file_name().is_some_and(|expected_name| {
            OsString::from_wide(&entry.szExeFile[..length])
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected_name.to_string_lossy())
        });
        if candidate && entry.th32ProcessID != 0 {
            let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, entry.th32ProcessID) } {
                Ok(handle) => Some(Handle(handle)),
                Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => None, // Exited since the snapshot.
                Err(_) => return Err("A setup process is running but its installation cannot be verified. Wait for updates to finish, then retry.".into()),
            };
            if let Some(process) = process {
                let mut length = buffer.len() as u32;
                let image = unsafe {
                    QueryFullProcessImageNameW(
                        process.0,
                        PROCESS_NAME_WIN32,
                        PWSTR(buffer.as_mut_ptr()),
                        &mut length,
                    )
                };
                let lifetime = unsafe { WaitForSingleObject(process.0, 0) };
                if lifetime != WAIT_OBJECT_0 {
                    if image.is_err() || lifetime != WAIT_TIMEOUT {
                        return Err("A setup process cannot be verified. Wait for updates to finish, then retry.".into());
                    }
                    let image = PathBuf::from(OsString::from_wide(&buffer[..length as usize]));
                    let image = image.canonicalize().unwrap_or(image);
                    let expected = expected
                        .canonicalize()
                        .unwrap_or_else(|_| expected.to_path_buf());
                    if same_path(&image, &expected) {
                        return Ok(true);
                    }
                }
            }
        }
        next = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publish(path: &Path, version: &str, current: PathBuf) -> Result<Guard, String> {
        let installer = current.parent().unwrap().join("Kielbox-Setup.exe");
        super::publish(path, version, current, installer)
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("Application 日本語").join("current");
        std::fs::create_dir_all(&current).unwrap();
        let marker = directory.path().join("state").join(MARKER_FILE);
        let current = current.canonicalize().unwrap();
        (directory, marker, current)
    }

    #[test]
    fn no_handoff_has_no_side_effects_and_does_not_query_processes() {
        let (_directory, marker, current) = fixture();
        assert!(
            inspect(&marker, Some("0.1.0"), Some(&current), |_| panic!(
                "No process probe needed"
            ))
            .unwrap()
            .is_none()
        );
        assert!(!marker.parent().unwrap().exists());
    }

    #[test]
    fn failed_helper_launch_drops_only_its_own_marker() {
        let (_directory, marker, current) = fixture();
        let guard = publish(&marker, "0.2.0", current).unwrap();
        assert!(marker.is_file());
        drop(guard);
        assert!(!marker.exists());
    }

    #[test]
    fn committed_handoff_defers_old_build_until_its_exact_helper_finishes() {
        let (_directory, marker, current) = fixture();
        publish(&marker, "0.2.0", current.clone()).unwrap().commit();
        let before = std::fs::read(&marker).unwrap();
        let error = inspect(&marker, Some("0.1.0"), Some(&current), |helper| {
            assert!(same_path(
                helper,
                &current.parent().unwrap().join("Kielbox-Setup.exe")
            ));
            Ok(true)
        })
        .unwrap_err();
        assert!(error.contains("installing an update"));
        assert_eq!(std::fs::read(&marker).unwrap(), before);
        assert!(
            inspect(&marker, Some("0.1.0"), Some(&current), |_| Ok(false))
                .unwrap()
                .is_some()
        );
        assert!(!marker.exists());
    }

    #[test]
    fn target_build_can_start_while_the_helper_cleans_up() {
        let (_directory, marker, current) = fixture();
        publish(&marker, "0.2.0-preview.7+owned", current.clone())
            .unwrap()
            .commit();
        assert!(
            inspect(
                &marker,
                Some("0.2.0-preview.7+owned"),
                Some(&current),
                |_| panic!("Target version need not wait for cleanup")
            )
            .unwrap()
            .is_none()
        );
        assert!(!marker.exists());
    }

    #[test]
    fn source_build_or_another_install_does_not_clear_a_live_handoff() {
        let (_directory, marker, current) = fixture();
        publish(&marker, "0.2.0", current.clone()).unwrap().commit();
        assert!(inspect(&marker, None, None, |_| Ok(true)).is_err());
        assert!(
            inspect(
                &marker,
                Some("0.2.0"),
                Some(current.parent().unwrap()),
                |_| Ok(true)
            )
            .is_err()
        );
        assert!(marker.exists());
    }

    #[test]
    fn ambiguous_process_probe_preserves_the_gate() {
        let (_directory, marker, current) = fixture();
        publish(&marker, "0.2.0", current.clone()).unwrap().commit();
        let before = std::fs::read(&marker).unwrap();
        assert!(
            inspect(&marker, Some("0.1.0"), Some(&current), |_| Err(
                "Cannot inspect helper".into()
            ))
            .is_err()
        );
        assert_eq!(std::fs::read(&marker).unwrap(), before);
    }

    #[test]
    fn changed_marker_survives_error_guard_cleanup() {
        let (_directory, marker, current) = fixture();
        let guard = publish(&marker, "0.2.0", current).unwrap();
        let mut changed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        changed["id"] = "another owned handoff".into();
        let changed = serde_json::to_vec(&changed).unwrap();
        std::fs::write(&marker, &changed).unwrap();
        drop(guard);
        assert_eq!(std::fs::read(marker).unwrap(), changed);
    }

    #[test]
    fn marker_changed_during_probe_is_not_cleared_or_treated_as_idle() {
        let (_directory, marker, current) = fixture();
        publish(&marker, "0.2.0", current.clone()).unwrap().commit();
        let before = std::fs::read(&marker).unwrap();
        assert!(
            inspect(&marker, Some("0.1.0"), Some(&current), |_| {
                let mut changed: serde_json::Value = serde_json::from_slice(&before).unwrap();
                changed["target_version"] = "0.3.0".into();
                std::fs::write(&marker, serde_json::to_vec(&changed).unwrap()).unwrap();
                Ok(false)
            })
            .is_err()
        );
        assert_eq!(
            load(&marker).unwrap().unwrap().marker.target_version,
            "0.3.0"
        );
    }

    #[test]
    fn malformed_or_oversized_handoffs_are_preserved() {
        for bytes in [
            b"{ invalid\n".to_vec(),
            vec![b'x'; MAX_MARKER_BYTES as usize + 1],
        ] {
            let (_directory, marker, current) = fixture();
            std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
            std::fs::write(&marker, &bytes).unwrap();
            assert!(
                inspect(&marker, Some("0.1.0"), Some(&current), |_| panic!(
                    "Invalid marker must not reach process lookup"
                ))
                .is_err()
            );
            assert_eq!(std::fs::read(&marker).unwrap(), bytes);
        }
    }

    #[test]
    fn publishing_never_replaces_an_existing_handoff() {
        let (_directory, marker, current) = fixture();
        publish(&marker, "0.2.0", current.clone()).unwrap().commit();
        let before = std::fs::read(&marker).unwrap();
        assert!(publish(&marker, "0.3.0", current).is_err());
        assert_eq!(std::fs::read(&marker).unwrap(), before);
    }

    #[test]
    fn windows_extended_and_standard_path_forms_identify_the_same_install() {
        assert!(same_path(
            Path::new(r"\\?\C:\Users\Synthetic\Kielbox\current"),
            Path::new(r"c:\users\synthetic\kielbox\current")
        ));
        assert!(!same_path(
            Path::new(r"C:\Kielbox\current"),
            Path::new(r"C:\OtherKielbox\current")
        ));
        assert!(same_path(
            Path::new(r"\\?\UNC\server\share\Kielbox"),
            Path::new(r"\\server\share\kielbox")
        ));
    }
}
