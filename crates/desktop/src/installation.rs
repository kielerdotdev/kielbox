//! Resolve installed-app data before preferences, projects, or logs are opened.
//!
//! The installer owns the application directory. This module owns only a small
//! bootstrap file outside it; existing working folders are never moved or merged.
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAX_BOOTSTRAP_BYTES: u64 = 64 * 1024;
const BOOTSTRAP_FILE: &str = "bootstrap.json";

#[derive(Clone, Debug)]
pub struct State {
    pub managed: bool,
    pub working_directory: PathBuf,
    pub first_run: bool,
    pub configuration_path: Option<PathBuf>,
    installation_directory: Option<PathBuf>,
}

impl State {
    /// Persist an explicit user selection for the next launch. Do not change the
    /// live process directory: media workers and loaded preferences still own it.
    /// The caller must finish its shutdown barrier and restart to activate it.
    pub fn remember_working_directory(&self, directory: &Path) -> Result<PathBuf, String> {
        let path = self.configuration_path.as_ref().ok_or_else(|| {
            "This build does not own an installed-app data-folder selection.".to_owned()
        })?;
        let previous = read_bootstrap(path)?
            .ok_or_else(|| "The installation configuration disappeared. Restart Kielbox before changing its data folder.".to_owned())?;
        let directory = existing_directory(directory, &self.working_directory)?;
        reject_installation_directory(&directory, self.installation_directory.as_deref())?;
        let configuration = Bootstrap::new(directory.clone());
        publish_bootstrap(path, &configuration, Some(&previous.bytes))?;
        Ok(directory)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Bootstrap {
    schema_version: u32,
    working_directory: PathBuf,
}

impl Bootstrap {
    fn new(working_directory: PathBuf) -> Self {
        Self {
            schema_version: 1,
            working_directory,
        }
    }
}

struct LoadedBootstrap {
    configuration: Bootstrap,
    bytes: Vec<u8>,
}

struct Locations {
    launch_directory: PathBuf,
    state_directory: Option<PathBuf>,
    installation_directory: Option<PathBuf>,
}

/// Call before initializing application
/// preferences, projects, crash logs, or capture paths. `managed` must come from
/// the adjacent Inno installation marker, not a release/debug compile flag.
pub fn initialize(managed: bool, args: &[String]) -> Result<State, String> {
    let launch_directory = std::env::current_dir()
        .map_err(|error| format!("Could not resolve the launch folder: {error}"))?;
    let state_directory = if managed {
        let local = std::env::var_os("LOCALAPPDATA")
            .ok_or_else(|| "Windows did not provide LOCALAPPDATA; Kielbox cannot resolve its installed data folder.".to_owned())?;
        let local = PathBuf::from(local);
        if !local.is_absolute() {
            return Err("LOCALAPPDATA must name an absolute folder.".into());
        }
        Some(local.join("KielboxData"))
    } else {
        None
    };
    let installation_directory = if managed {
        let executable = std::env::current_exe()
            .map_err(|error| format!("Could not resolve the installed executable: {error}"))?;
        let application = executable
            .parent()
            .ok_or_else(|| "The installed executable has no parent folder.".to_owned())?;
        Some(
            std::fs::canonicalize(application)
                .map_err(|error| format!("Could not resolve the installation folder: {error}"))?,
        )
    } else {
        None
    };
    let state = prepare(
        managed,
        args,
        &Locations {
            launch_directory,
            state_directory,
            installation_directory,
        },
    )?;
    std::env::set_current_dir(&state.working_directory).map_err(|error| {
        format!(
            "Could not open the selected data folder {}: {error}",
            state.working_directory.display()
        )
    })?;
    Ok(state)
}

fn requested_directory(args: &[String]) -> Result<Option<PathBuf>, String> {
    let mut selected = None;
    for (index, argument) in args.iter().enumerate() {
        if argument != "--working-directory" {
            continue;
        }
        if selected.is_some() {
            return Err("Specify --working-directory only once.".into());
        }
        let value = args
            .get(index + 1)
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .ok_or_else(|| "--working-directory requires an existing folder path.".to_owned())?;
        selected = Some(PathBuf::from(value));
    }
    Ok(selected)
}

fn prepare(managed: bool, args: &[String], locations: &Locations) -> Result<State, String> {
    let requested = requested_directory(args)?;
    if !managed {
        let working_directory = match requested {
            Some(path) => existing_directory(&path, &locations.launch_directory)?,
            None => locations.launch_directory.clone(),
        };
        return Ok(State {
            managed: false,
            working_directory,
            first_run: false,
            configuration_path: None,
            installation_directory: None,
        });
    }
    let state_directory = locations
        .state_directory
        .as_deref()
        .ok_or_else(|| "Installed Kielbox needs a stable user-state folder.".to_owned())?;
    if !state_directory.is_absolute() {
        return Err("The installed user-state folder must be absolute.".into());
    }
    let configuration_path = state_directory.join(BOOTSTRAP_FILE);
    // Even an explicit override does not overwrite unreadable/malformed state.
    // The caller can present its path and an actionable recovery error instead.
    let previous = read_bootstrap(&configuration_path)?;
    let first_run = previous.is_none();
    let directory = if let Some(path) = requested {
        existing_directory(&path, &locations.launch_directory)?
    } else if let Some(loaded) = &previous {
        existing_directory(&loaded.configuration.working_directory, &locations.launch_directory)
            .map_err(|error| format!("The saved data folder is unavailable. Restore access or explicitly select an existing folder with --working-directory. {error}"))?
    } else {
        let default = state_directory.join("workspace");
        reject_installation_directory(&default, locations.installation_directory.as_deref())?;
        std::fs::create_dir_all(&default).map_err(|error| {
            format!(
                "Could not create the Kielbox data folder {}: {error}",
                default.display()
            )
        })?;
        existing_directory(&default, &locations.launch_directory)?
    };
    reject_installation_directory(&directory, locations.installation_directory.as_deref())?;
    if first_run
        || previous
            .as_ref()
            .is_some_and(|loaded| loaded.configuration.working_directory != directory)
    {
        publish_bootstrap(
            &configuration_path,
            &Bootstrap::new(directory.clone()),
            previous.as_ref().map(|loaded| loaded.bytes.as_slice()),
        )?;
    }
    Ok(State {
        managed: true,
        working_directory: directory,
        first_run,
        configuration_path: Some(configuration_path),
        installation_directory: locations.installation_directory.clone(),
    })
}

fn existing_directory(path: &Path, launch_directory: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        launch_directory.join(path)
    };
    let directory = std::fs::canonicalize(&absolute)
        .map_err(|error| format!("Could not open data folder {}: {error}", absolute.display()))?;
    if !directory.is_dir() {
        return Err(format!(
            "The selected data path is not a folder: {}",
            absolute.display()
        ));
    }
    Ok(directory)
}

fn reject_installation_directory(
    directory: &Path,
    installation: Option<&Path>,
) -> Result<(), String> {
    if installation.is_some_and(|installation| directory.starts_with(installation)) {
        return Err("Choose a data folder outside the Kielbox installation; updates replace application files.".into());
    }
    Ok(())
}

fn read_bootstrap(path: &Path) -> Result<Option<LoadedBootstrap>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Could not inspect installation configuration {}: {error}. The original was preserved.",
                path.display()
            ));
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "Installation configuration must be a regular file: {}. The original was preserved.",
            path.display()
        ));
    }
    if metadata.len() > MAX_BOOTSTRAP_BYTES {
        return Err(format!(
            "Installation configuration exceeds 64 KiB: {}. The original was preserved.",
            path.display()
        ));
    }
    let file = std::fs::File::open(path).map_err(|error| {
        format!(
            "Could not read installation configuration {}: {error}. The original was preserved.",
            path.display()
        )
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_BOOTSTRAP_BYTES + 1).read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read installation configuration {}: {error}. The original was preserved.", path.display()))?;
    if bytes.len() as u64 > MAX_BOOTSTRAP_BYTES {
        return Err(format!(
            "Installation configuration exceeds 64 KiB: {}. The original was preserved.",
            path.display()
        ));
    }
    let configuration: Bootstrap = serde_json::from_slice(&bytes)
        .map_err(|_| format!("Installation configuration is invalid at {}. Repair it before starting Kielbox; no data folder was changed.", path.display()))?;
    if configuration.schema_version != 1 || !configuration.working_directory.is_absolute() {
        return Err(format!(
            "Installation configuration has an unsupported version or nonabsolute data path at {}. The original was preserved.",
            path.display()
        ));
    }
    Ok(Some(LoadedBootstrap {
        configuration,
        bytes,
    }))
}

fn publish_bootstrap(
    path: &Path,
    configuration: &Bootstrap,
    previous: Option<&[u8]>,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(configuration)
        .map_err(|error| format!("Could not encode the selected data folder: {error}"))?;
    if bytes.len() as u64 > MAX_BOOTSTRAP_BYTES {
        return Err("The selected data-folder configuration exceeds 64 KiB.".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "Installation configuration has no parent folder.".to_owned())?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "Could not create user-state folder {}: {error}",
            parent.display()
        )
    })?;
    let mut staged = tempfile::Builder::new()
        .prefix(".bootstrap-")
        .suffix(".json")
        .tempfile_in(parent)
        .map_err(|error| format!("Could not stage the installation configuration: {error}"))?;
    staged
        .write_all(&bytes)
        .and_then(|()| staged.flush())
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|error| format!("Could not durably save the selected data folder: {error}"))?;
    if let Some(expected) = previous {
        let current = read_bootstrap(path)?.ok_or_else(|| "Installation configuration changed during selection. Retry after restarting Kielbox.".to_owned())?;
        if current.bytes != expected {
            return Err("Installation configuration changed during selection. Retry after restarting Kielbox.".into());
        }
        staged.persist(path).map_err(|error| {
            format!(
                "Could not replace the installation configuration: {}",
                error.error
            )
        })?;
    } else {
        // Concurrent first launches cannot overwrite each other's choice. The
        // application-wide instance lock remains the primary serialization owner.
        staged.persist_noclobber(path)
            .map_err(|error| format!("Could not create the installation configuration without replacing an existing file: {}", error.error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locations(directory: &tempfile::TempDir) -> Locations {
        let launch_directory = directory.path().join("launch");
        let installation_directory = directory.path().join("application");
        std::fs::create_dir_all(&launch_directory).unwrap();
        std::fs::create_dir_all(&installation_directory).unwrap();
        Locations {
            launch_directory: std::fs::canonicalize(launch_directory).unwrap(),
            state_directory: Some(directory.path().join("state")),
            installation_directory: Some(std::fs::canonicalize(installation_directory).unwrap()),
        }
    }

    fn args(path: &Path) -> Vec<String> {
        vec![
            "kielbox.exe".into(),
            "--working-directory".into(),
            path.to_str().unwrap().into(),
        ]
    }

    fn configuration_path(locations: &Locations) -> PathBuf {
        locations
            .state_directory
            .as_ref()
            .unwrap()
            .join(BOOTSTRAP_FILE)
    }

    #[test]
    fn unmanaged_start_preserves_launch_folder_without_registering_a_profile() {
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        let state = prepare(false, &[], &locations).unwrap();
        assert_eq!(state.working_directory, locations.launch_directory);
        assert!(!state.managed && !state.first_run && state.configuration_path.is_none());
        assert!(!locations.state_directory.unwrap().exists());
    }

    #[test]
    fn unmanaged_explicit_relative_folder_is_only_a_launch_selection() {
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        let selected = locations.launch_directory.join("Data with spaces 日本語");
        std::fs::create_dir(&selected).unwrap();
        let state = prepare(
            false,
            &args(Path::new("Data with spaces 日本語")),
            &locations,
        )
        .unwrap();
        assert_eq!(
            state.working_directory,
            std::fs::canonicalize(selected).unwrap()
        );
        assert!(!locations.state_directory.unwrap().exists());
    }

    #[test]
    fn installed_first_launch_uses_stable_default_instead_of_installer_cwd() {
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        std::fs::write(
            locations.launch_directory.join("installer-download.txt"),
            b"keep",
        )
        .unwrap();
        let state = prepare(true, &[], &locations).unwrap();
        assert!(state.managed && state.first_run);
        assert_eq!(
            state.working_directory,
            std::fs::canonicalize(
                locations
                    .state_directory
                    .as_ref()
                    .unwrap()
                    .join("workspace")
            )
            .unwrap()
        );
        let loaded = read_bootstrap(&configuration_path(&locations))
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.configuration.working_directory,
            state.working_directory
        );
        assert_eq!(
            std::fs::read(locations.launch_directory.join("installer-download.txt")).unwrap(),
            b"keep"
        );
        assert!(!locations.launch_directory.join("projects").exists());
    }

    #[test]
    fn installed_existing_root_preserves_preferences_and_media_then_survives_new_cwd() {
        let fixture = tempfile::tempdir().unwrap();
        let mut locations = locations(&fixture);
        let selected = fixture.path().join("Existing library æ 日本語");
        std::fs::create_dir_all(selected.join("projects")).unwrap();
        std::fs::create_dir_all(selected.join("recordings")).unwrap();
        let original_preferences = b"{ \"custom\": \"do not rewrite\" }\n";
        std::fs::write(
            selected.join("projects/preferences.json"),
            original_preferences,
        )
        .unwrap();
        std::fs::write(
            selected.join("recordings/original.mkv"),
            b"owned fixture media",
        )
        .unwrap();
        let first = prepare(true, &args(&selected), &locations).unwrap();
        assert!(first.first_run);
        assert!(
            !locations
                .state_directory
                .as_ref()
                .unwrap()
                .join("workspace")
                .exists()
        );
        let before = std::fs::read(configuration_path(&locations)).unwrap();
        locations.launch_directory = locations.installation_directory.clone().unwrap();
        let second = prepare(true, &[], &locations).unwrap();
        assert!(!second.first_run);
        assert_eq!(second.working_directory, first.working_directory);
        assert_eq!(
            std::fs::read(configuration_path(&locations)).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(selected.join("projects/preferences.json")).unwrap(),
            original_preferences
        );
        assert_eq!(
            std::fs::read(selected.join("recordings/original.mkv")).unwrap(),
            b"owned fixture media"
        );
    }

    #[test]
    fn changing_selection_is_persisted_for_restart_without_mutating_live_state() {
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        let first = prepare(true, &[], &locations).unwrap();
        let selected = fixture.path().join("another existing library");
        std::fs::create_dir(&selected).unwrap();
        let selected = first.remember_working_directory(&selected).unwrap();
        assert_ne!(first.working_directory, selected);
        assert!(first.working_directory.is_dir());
        let next = prepare(true, &[], &locations).unwrap();
        assert_eq!(next.working_directory, selected);
    }

    #[test]
    fn invalid_or_unsupported_bootstrap_never_falls_back_or_overwrites() {
        for original in [
            b"{ broken\n".as_slice(),
            br#"{"schema_version":2,"working_directory":"/data"}"#.as_slice(),
            br#"{"schema_version":1,"working_directory":"relative"}"#.as_slice(),
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let locations = locations(&fixture);
            std::fs::create_dir_all(locations.state_directory.as_ref().unwrap()).unwrap();
            let path = configuration_path(&locations);
            std::fs::write(&path, original).unwrap();
            assert!(prepare(true, &[], &locations).is_err());
            assert!(prepare(true, &args(&locations.launch_directory), &locations).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert!(
                !locations
                    .state_directory
                    .as_ref()
                    .unwrap()
                    .join("workspace")
                    .exists()
            );
        }
    }

    #[test]
    fn oversized_bootstrap_is_preserved_without_creating_a_new_data_folder() {
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        std::fs::create_dir_all(locations.state_directory.as_ref().unwrap()).unwrap();
        let path = configuration_path(&locations);
        let bytes = vec![b'x'; MAX_BOOTSTRAP_BYTES as usize + 1];
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            prepare(true, &[], &locations)
                .unwrap_err()
                .contains("64 KiB")
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(
            !locations
                .state_directory
                .as_ref()
                .unwrap()
                .join("workspace")
                .exists()
        );
    }

    #[test]
    fn unavailable_saved_root_is_not_recreated_or_replaced_with_default() {
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        let missing = fixture.path().join("unavailable volume");
        publish_bootstrap(
            &configuration_path(&locations),
            &Bootstrap::new(missing.clone()),
            None,
        )
        .unwrap();
        let original = std::fs::read(configuration_path(&locations)).unwrap();
        assert!(
            prepare(true, &[], &locations)
                .unwrap_err()
                .contains("saved data folder is unavailable")
        );
        assert!(!missing.exists());
        assert!(
            !locations
                .state_directory
                .as_ref()
                .unwrap()
                .join("workspace")
                .exists()
        );
        assert_eq!(
            std::fs::read(configuration_path(&locations)).unwrap(),
            original
        );
    }

    #[test]
    fn missing_argument_duplicate_selection_and_file_roots_are_rejected() {
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        assert!(prepare(true, &["--working-directory".into()], &locations).is_err());
        assert!(
            prepare(
                true,
                &["--working-directory".into(), "--diagnose".into()],
                &locations
            )
            .is_err()
        );
        let mut duplicate = args(&locations.launch_directory);
        duplicate.extend(args(&locations.launch_directory).into_iter().skip(1));
        assert!(prepare(true, &duplicate, &locations).is_err());
        let file = fixture.path().join("not a folder");
        std::fs::write(&file, b"keep").unwrap();
        assert!(prepare(true, &args(&file), &locations).is_err());
        assert!(!configuration_path(&locations).exists());
    }

    #[test]
    fn managed_data_cannot_be_placed_inside_replaceable_application_files() {
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        let selected = locations
            .installation_directory
            .as_ref()
            .unwrap()
            .join("existing data");
        std::fs::create_dir(&selected).unwrap();
        std::fs::write(selected.join("keep.txt"), b"unchanged").unwrap();
        assert!(
            prepare(true, &args(&selected), &locations)
                .unwrap_err()
                .contains("outside")
        );
        assert_eq!(
            std::fs::read(selected.join("keep.txt")).unwrap(),
            b"unchanged"
        );
        assert!(!configuration_path(&locations).exists());
    }

    #[test]
    fn nonfile_bootstrap_is_not_treated_as_a_missing_profile() {
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        std::fs::create_dir_all(configuration_path(&locations)).unwrap();
        assert!(
            prepare(true, &[], &locations)
                .unwrap_err()
                .contains("regular file")
        );
        assert!(configuration_path(&locations).is_dir());
        assert!(
            !locations
                .state_directory
                .as_ref()
                .unwrap()
                .join("workspace")
                .exists()
        );
    }

    #[test]
    fn publishing_a_first_choice_never_clobbers_an_existing_choice() {
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        let path = configuration_path(&locations);
        let configuration = Bootstrap::new(locations.launch_directory.clone());
        publish_bootstrap(&path, &configuration, None).unwrap();
        let original = std::fs::read(&path).unwrap();
        let other = Bootstrap::new(locations.installation_directory.unwrap());
        assert!(publish_bootstrap(&path, &other, None).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_bootstrap_does_not_replace_the_profile() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let locations = locations(&fixture);
        let path = configuration_path(&locations);
        publish_bootstrap(
            &path,
            &Bootstrap::new(locations.launch_directory.clone()),
            None,
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(
            prepare(true, &[], &locations)
                .unwrap_err()
                .contains("Could not read")
        );
        assert!(
            !locations
                .state_directory
                .as_ref()
                .unwrap()
                .join("workspace")
                .exists()
        );
        drop(locked);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}
