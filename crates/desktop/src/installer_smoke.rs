//! Explicit native install/update proof. Refuses the user's normal installation
//! and profile: executable, data, feed, and LOCALAPPDATA must share an owned root.
use crate::{installation, updates};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn argument(args: &[String], name: &str) -> Result<PathBuf, String> {
    let values: Vec<_> = args.windows(2).filter(|pair| pair[0] == name).collect();
    if values.len() != 1 {
        return Err(format!("Specify {name} exactly once."));
    }
    let value = PathBuf::from(&values[0][1]);
    if !value.is_absolute() {
        return Err(format!("{name} must be an absolute path."));
    }
    Ok(value)
}

fn owned_path(path: &Path, root: &Path) -> Result<PathBuf, String> {
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    if !path.starts_with(root) {
        return Err(
            "Installed-app proof paths must stay inside the owned evaluation folder.".into(),
        );
    }
    Ok(path)
}

fn wait_for_worker(updates: &mut updates::Updates) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    while updates.busy() {
        if Instant::now() >= deadline {
            return Err("Local update worker exceeded the proof budget.".into());
        }
        std::thread::sleep(Duration::from_millis(10));
        updates.poll(Instant::now(), true);
    }
    if let updates::Status::Error(error) = updates.status() {
        return Err(error.clone());
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<(), String> {
    let output = argument(args, "--proof-output")?;
    let root = output
        .parent()
        .ok_or("Missing proof output folder")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if fs::read(root.join(".kielbox-installed-test")).map_err(|error| error.to_string())?
        != b"kielbox-installed-test-v1\n"
    {
        return Err("Owned installer proof marker missing.".into());
    }
    owned_path(
        &std::env::current_exe().map_err(|error| error.to_string())?,
        &root,
    )?;
    let local = std::env::var_os("LOCALAPPDATA").ok_or("Missing isolated LOCALAPPDATA")?;
    owned_path(Path::new(&local), &root)?;
    let data = owned_path(&argument(args, "--working-directory")?, &root)?;
    if updates::installed_version()?.is_none() {
        return Err("The proof requires a real Inno installation.".into());
    }
    let state = installation::initialize(true, args)?;
    if state.working_directory != data {
        return Err("The installed data root changed unexpectedly.".into());
    }
    let apply = args.iter().any(|arg| arg == "--smoke-installed-update");
    let public_check = args.iter().any(|arg| arg == "--smoke-installed-check");
    let mut updater = if apply {
        let feed = owned_path(&argument(args, "--smoke-installed-update")?, &root)?;
        updates::Updates::with_file_source_for_proof(&feed, Instant::now())?
    } else {
        updates::Updates::new(Instant::now())
    };
    updater.settings.automatic_checks = false;
    updater.settings.automatic_downloads = false;
    updater.settings.install_on_quit = false;
    let before = updater.current_version().to_owned();
    crate::update_handoff::check_startup(Some(&before))?;
    if public_check {
        if !updater.check_now() { return Err("The installed updater did not admit a public check.".into()); }
        wait_for_worker(&mut updater)?;
    }
    let prepared = if apply {
        if !updater.check_now() {
            return Err("The installed updater did not admit a local check.".into());
        }
        wait_for_worker(&mut updater)?;
        if !updater.download() {
            return Err(format!("No newer local package: {:?}", updater.status()));
        }
        wait_for_worker(&mut updater)?;
        Some(
            updater
                .prepare_ready()
                .ok_or("The verified package was not ready to apply")?,
        )
    } else {
        None
    };
    // The helper waits for this process. Start the handoff only after the local
    // check/download has finished; this proof has no application/media owners.
    if let Some(prepared) = &prepared {
        prepared.apply_after_shutdown(false, &data)?;
    }
    let report = serde_json::json!({
        "success": true, "managed": updater.installed(), "installed_version": before,
        "target_version": prepared.as_ref().map(|update| update.version()),
        "data_root": state.working_directory, "bootstrap": state.configuration_path,
        "source": if apply { "owned local directory" } else if public_check { "public release catalog" } else { "no network check performed" },
        "update_status": format!("{:?}", updater.status()),
        "gpu_or_audio_started": false, "os_input": false,
        "apply_handoff_started": apply,
    });
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&output)
        .map_err(|error| error.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    Ok(())
}
