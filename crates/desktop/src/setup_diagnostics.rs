//! Read-only setup inspection. Never initialize capture, an encoder, or an audio
//! endpoint. Call on a worker (or before GPUI starts for the CLI).
use ::toolkit::{
    core::workflow::CancelToken,
    services::command::{ProcessOutput, ProcessSpec, STDOUT_HEAD_BYTES, run_process},
};
use serde::Serialize;
use std::{ffi::{OsStr, OsString}, path::{Path, PathBuf}, time::{Duration, Instant}};

#[derive(Clone, Debug)]
pub struct Config {
    pub working_directory: PathBuf,
    pub executable: Option<PathBuf>,
    pub recording_directory: PathBuf,
    pub ffmpeg: OsString,
    pub ffprobe: OsString,
    pub ffmpeg_override: bool,
    pub ffprobe_override: bool,
    pub search_path: OsString,
    pub shared_library_directory: Option<PathBuf>,
    pub timeout: Duration,
    pub debug_build: bool,
    pub checkout: PathBuf,
}
impl Config {
    pub fn capture(recording_directory: PathBuf) -> Self {
        let ffmpeg = std::env::var_os("RCR_FFMPEG");
        let ffprobe = std::env::var_os("RCR_FFPROBE");
        Self {
            working_directory: std::env::current_dir().unwrap_or_default(),
            executable: std::env::current_exe().ok(), recording_directory,
            ffmpeg_override: ffmpeg.is_some(), ffprobe_override: ffprobe.is_some(),
            ffmpeg: ffmpeg.unwrap_or_else(|| "ffmpeg".into()),
            ffprobe: ffprobe.unwrap_or_else(|| "ffprobe".into()),
            search_path: std::env::var_os("PATH").unwrap_or_default(),
            shared_library_directory: std::env::var_os("RCR_FFMPEG_LIB_DIR").map(PathBuf::from),
            timeout: Duration::from_secs(3), debug_build: cfg!(debug_assertions),
            checkout: Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status { Ready, Warning, Error, NotVerified, Cancelled }

#[derive(Debug, Serialize)]
pub struct Check {
    pub id: String,
    pub status: Status,
    pub blocking: bool,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub elapsed_ms: u128,
}
impl Check {
    fn new(id: &str, status: Status, blocking: bool, summary: impl Into<String>, action: Option<&str>) -> Self {
        Self { id: id.into(), status, blocking, summary: summary.into(), action: action.map(str::to_owned),
            path: None, detail: None, elapsed_ms: 0 }
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub app_version: String,
    pub build_profile: String,
    pub platform: String,
    pub executable: Option<PathBuf>,
    pub working_directory: PathBuf,
    pub checks: Vec<Check>,
    pub elapsed_ms: u128,
}
impl Report {
    pub fn has_blocking_issues(&self) -> bool { self.checks.iter().any(|c| c.blocking) }
    pub fn human_summary(&self) -> String {
        self.checks.iter().map(|c| {
            let action = c.action.as_ref().map_or(String::new(), |a| format!(" {a}"));
            format!("{}: {}{}", match c.status { Status::Ready => "OK", Status::Warning => "Check",
                Status::Error => "Needs attention", Status::NotVerified => "Not tested", Status::Cancelled => "Cancelled" }, c.summary, action)
        }).collect::<Vec<_>>().join("\n")
    }
}

fn label(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).take(320).collect()
}
fn absolute(path: &Path, directory: &Path) -> PathBuf {
    if path.is_absolute() { path.to_owned() } else { directory.join(path) }
}

// No shell, environment mutation, implicit download, or fallback from a broken
// explicit override. Launch the resolved file, not an ambiguous bare name.
fn resolve(program: &OsStr, config: &Config) -> Option<PathBuf> {
    let path = Path::new(program);
    let candidates = |path: PathBuf| {
        let mut paths = vec![path.clone()];
        if cfg!(windows) && path.extension().is_none() { paths.push(path.with_extension("exe")); }
        paths
    };
    if path.is_absolute() || path.components().count() > 1 {
        return candidates(absolute(path, &config.working_directory)).into_iter().find(|p| executable_file(p));
    }
    std::env::split_paths(&config.search_path).filter(|p| !p.as_os_str().is_empty()).take(256)
        .flat_map(|directory| candidates(absolute(&directory, &config.working_directory).join(path)))
        .find(|p| executable_file(p))
}
fn executable_file(path: &Path) -> bool {
    path.is_file() && (!cfg!(windows) || path.extension().is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("exe")))
}

trait Runner {
    fn run(&self, program: &Path, args: &[&str], timeout: Duration, cancel: &CancelToken) -> Result<ProcessOutput, String>;
}
struct SystemRunner;
impl Runner for SystemRunner {
    fn run(&self, program: &Path, args: &[&str], timeout: Duration, cancel: &CancelToken) -> Result<ProcessOutput, String> {
        let program = program.to_str().ok_or("The tool path is not valid Unicode")?;
        let args = args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        run_process(&ProcessSpec { program, args: &args, stdin: None, timeout,
            capture_stdout: true, capture_stderr: true }, cancel).map_err(|e| label(&e.to_string()))
    }
}

fn tool_check(config: &Config, name: &str, requested: &OsStr, overridden: bool, runner: &impl Runner, cancel: &CancelToken) -> (Check, Option<PathBuf>) {
    let start = Instant::now();
    if cancel.is_cancelled() {
        return (Check::new(name, Status::Cancelled, false, format!("{name} check cancelled"), None), None);
    }
    let Some(path) = resolve(requested, config) else {
        let mut check = Check::new(name, Status::Error, true, format!("{name} was not found"),
            Some(if name == "ffmpeg" { "Install FFmpeg or set RCR_FFMPEG to its executable." } else { "Install FFprobe or set RCR_FFPROBE to its executable." }));
        check.detail = Some(if overridden { "The explicit override does not resolve; PATH was not substituted.".into() } else { "No executable was found in the configured PATH (first 256 entries).".into() });
        check.path = Some(PathBuf::from(requested)); check.elapsed_ms = start.elapsed().as_millis();
        return (check, None);
    };
    let result = runner.run(&path, &["-version"], config.timeout, cancel);
    let mut check = match result {
        Ok(output) if output.success => {
            let version = String::from_utf8_lossy(&output.stdout).lines().next().unwrap_or_default().to_owned();
            if version.starts_with(&format!("{name} version ")) {
                Check::new(name, Status::Ready, false, label(&version), None)
            } else {
                Check::new(name, Status::Error, true, format!("The selected {name} did not identify itself"), Some("Choose the matching FFmpeg/FFprobe executable."))
            }
        }
        Ok(output) => {
            let mut c = Check::new(name, Status::Error, true, format!("{name} could not run successfully"), Some("Check the executable and its dependencies, then retry."));
            c.detail = Some(label(&format!("Exit {:?}: {}", output.exit_code, output.stderr))); c
        }
        Err(error) => {
            let mut c = Check::new(name, if cancel.is_cancelled() { Status::Cancelled } else { Status::Error }, !cancel.is_cancelled(),
                format!("{name} did not finish its check"), Some("Check the executable and retry; a stalled tool is stopped automatically."));
            c.detail = Some(error); c
        }
    };
    check.path = Some(path.clone());
    check.elapsed_ms = start.elapsed().as_millis();
    let usable = check.status == Status::Ready;
    (check, usable.then_some(path))
}

fn encoders(config: &Config, path: Option<&Path>, runner: &impl Runner, cancel: &CancelToken) -> Check {
    let start = Instant::now();
    let Some(path) = path else {
        return Check::new("av1_nvenc", Status::NotVerified, false, "AV1 export support could not be checked", Some("Fix FFmpeg above, then run this check again."));
    };
    if cancel.is_cancelled() { return Check::new("av1_nvenc", Status::Cancelled, false, "Encoder listing cancelled", None); }
    let mut check = match runner.run(path, &["-hide_banner", "-encoders"], config.timeout, cancel) {
        Ok(output) if output.success => {
            let listed = String::from_utf8_lossy(&output.stdout).lines().any(|line| {
                let mut words = line.split_whitespace();
                words.next().is_some_and(|flags| flags.len() == 6 && flags.starts_with('V'))
                    && words.next() == Some("av1_nvenc")
            });
            if listed {
                Check::new("av1_nvenc", Status::Ready, false, "FFmpeg lists AV1 NVENC export support", None)
            } else if output.stdout.len() >= STDOUT_HEAD_BYTES {
                Check::new("av1_nvenc", Status::Warning, false, "Encoder listing exceeded the diagnostic output limit", Some("Use an FFmpeg build with av1_nvenc; its presence could not be established from this listing."))
            } else {
                Check::new("av1_nvenc", Status::Error, true, "This FFmpeg build does not list av1_nvenc", Some("Install an FFmpeg build with AV1 NVENC support for editor export."))
            }
        }
        Ok(output) => {
            let mut c = Check::new("av1_nvenc", Status::Warning, false, "FFmpeg encoder support could not be listed", Some("Check the FFmpeg installation, then retry."));
            c.detail = Some(label(&output.stderr)); c
        }
        Err(error) => {
            let mut c = Check::new("av1_nvenc", if cancel.is_cancelled() { Status::Cancelled } else { Status::Warning }, false,
                "FFmpeg encoder listing did not finish", Some("Retry the check after fixing FFmpeg."));
            c.detail = Some(error); c
        }
    };
    check.elapsed_ms = start.elapsed().as_millis(); check
}

fn directory_check(id: &str, name: &str, path: PathBuf) -> Check {
    let mut check = match path.metadata() {
        Ok(m) if !m.is_dir() => Check::new(id, Status::Error, true, format!("{name} path is a file, not a folder"), Some("Choose a folder for this data location.")),
        Ok(_) => match std::fs::read_dir(&path) {
            Ok(_) => Check::new(id, Status::Ready, false, format!("{name} is accessible; write access was not tested"), None),
            Err(_) => Check::new(id, Status::Warning, false, format!("{name} could not be read"), Some("Check the drive and folder permissions.")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let existing = path.ancestors().skip(1).find(|p| p.exists());
            if existing.is_some_and(|p| p.is_dir()) {
                Check::new(id, Status::Warning, false, format!("{name} has not been created; write access was not tested"), Some("Rcr creates this folder when saving; check its parent permissions if saving fails."))
            } else {
                Check::new(id, Status::Warning, false, format!("{name} is unavailable"), Some("Reconnect the drive or choose an available data folder."))
            }
        }
        Err(_) => Check::new(id, Status::Warning, false, format!("{name} is unavailable"), Some("Reconnect the drive and check folder permissions.")),
    };
    check.path = Some(path); check
}

pub fn collect(config: Config, cancel: &CancelToken) -> Report { collect_with(config, cancel, &SystemRunner) }

fn collect_with(config: Config, cancel: &CancelToken, runner: &impl Runner) -> Report {
    let started = Instant::now();
    let (ffmpeg, executable) = tool_check(&config, "ffmpeg", &config.ffmpeg, config.ffmpeg_override, runner, cancel);
    let (ffprobe, _) = tool_check(&config, "ffprobe", &config.ffprobe, config.ffprobe_override, runner, cancel);
    let mut checks = vec![ffmpeg, ffprobe, encoders(&config, executable.as_deref(), runner, cancel)];
    if cancel.is_cancelled() {
        checks.push(Check::new("data_locations", Status::Cancelled, false, "Data-location checks cancelled", None));
        return finish_report(&config, checks, started);
    }
    checks.push(Check::new("gpu_capture", Status::NotVerified, false,
        "GPU recording and encoder operation were not tested", Some("A listed encoder does not establish GPU/driver support. This check never starts recording or encoding.")));
    checks.push(directory_check("recording_folder", "Capture folder", absolute(&config.recording_directory, &config.working_directory)));
    checks.push(directory_check("projects_folder", "Project/settings folder", config.working_directory.join("projects")));
    let mut working = Check::new("working_directory", Status::Warning, false,
        "Projects and default captures depend on the launch folder", Some("Keep the same working folder when updating rcr to retain these data locations."));
    working.path = Some(config.working_directory.clone()); checks.push(working);
    checks.push(Check::new("debug_shaders", Status::Ready, false, "Rune embeds its WGSL shaders in development and release builds", None));
    let mut runtime = Check::new("shared_ffmpeg", Status::NotVerified, false,
        "Optional native playback DLL compatibility was not tested", Some("External FFmpeg can provide fallback when installed. No DLL was loaded or GPU initialized by this check."));
    runtime.path = config.shared_library_directory.clone();
    checks.push(runtime);
    finish_report(&config, checks, started)
}
fn finish_report(config: &Config, checks: Vec<Check>, started: Instant) -> Report {
    Report { schema_version: 1, app_version: env!("CARGO_PKG_VERSION").into(),
        build_profile: if config.debug_build { "development" } else { "release" }.into(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH), executable: config.executable.clone(),
        working_directory: config.working_directory.clone(), checks, elapsed_ms: started.elapsed().as_millis() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeRunner { missing_encoder: bool, fail: bool, calls: Mutex<Vec<(PathBuf, Vec<String>)>> }
    impl Runner for FakeRunner {
        fn run(&self, path: &Path, args: &[&str], timeout: Duration, _: &CancelToken) -> Result<ProcessOutput, String> {
            assert_eq!(timeout, Duration::from_millis(80));
            self.calls.lock().unwrap().push((path.to_owned(), args.iter().map(|s| (*s).into()).collect()));
            if self.fail { return Err("synthetic startup/deadline failure".into()); }
            let stdout = if args.contains(&"-encoders") {
                if self.missing_encoder { b" V..... libsvtav1 AV1 encoder\n # description mentions av1_nvenc\n".to_vec() }
                else { b"Encoders:\n V....D av1_nvenc NVIDIA NVENC AV1 encoder\n".to_vec() }
            } else { format!("{} version 7.0.2 test-only\nconfiguration: fake\n", path.file_stem().unwrap().to_string_lossy()).into_bytes() };
            Ok(ProcessOutput { success: true, exit_code: Some(0), stdout, stderr: String::new() })
        }
    }
    fn fixture() -> (tempfile::TempDir, Config) {
        let directory = tempfile::tempdir().unwrap();
        for file in ["ffmpeg.exe", "ffprobe.exe"] { std::fs::write(directory.path().join(file), b"fixture only; never executed").unwrap(); }
        let config = Config { working_directory: directory.path().to_owned(), executable: None,
            recording_directory: PathBuf::from("captures"), ffmpeg: "ffmpeg.exe".into(), ffprobe: "ffprobe.exe".into(),
            ffmpeg_override: false, ffprobe_override: false,
            search_path: std::env::join_paths([directory.path()]).unwrap(), shared_library_directory: None,
            timeout: Duration::from_millis(80), debug_build: false, checkout: directory.path().to_owned() };
        (directory, config)
    }
    fn fake() -> FakeRunner { FakeRunner { missing_encoder: false, fail: false, calls: Mutex::new(Vec::new()) } }
    fn check<'a>(report: &'a Report, id: &str) -> &'a Check { report.checks.iter().find(|c| c.id == id).unwrap() }

    #[test]
    fn doctor_resolves_path_and_reports_listed_encoder_without_claiming_gpu_or_writes() {
        let (directory, config) = fixture(); let runner = fake();
        let report = collect_with(config, &CancelToken::new(), &runner);
        assert!(!report.has_blocking_issues());
        assert_eq!(check(&report, "ffmpeg").path.as_ref().unwrap(), &directory.path().join("ffmpeg.exe"));
        assert_eq!(check(&report, "av1_nvenc").status, Status::Ready);
        assert_eq!(check(&report, "gpu_capture").status, Status::NotVerified);
        assert_eq!(check(&report, "shared_ffmpeg").status, Status::NotVerified);
        assert!(!directory.path().join("captures").exists() && !directory.path().join("projects").exists(), "Read-only doctor created data folders");
        assert!(report.human_summary().contains("does not establish GPU/driver support"));
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|(_, args)| args == &["-version"] || args == &["-hide_banner", "-encoders"]));
    }
    #[test]
    fn doctor_missing_override_never_falls_back_and_does_not_run_encoder() {
        let (_directory, mut config) = fixture(); let runner = fake();
        config.ffmpeg = "missing/explicit-ffmpeg.exe".into(); config.ffmpeg_override = true;
        let report = collect_with(config, &CancelToken::new(), &runner);
        assert!(report.has_blocking_issues());
        assert_eq!(check(&report, "ffmpeg").status, Status::Error);
        assert!(check(&report, "ffmpeg").detail.as_ref().unwrap().contains("PATH was not substituted"));
        assert_eq!(check(&report, "av1_nvenc").status, Status::NotVerified);
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
    }
    #[test]
    fn doctor_absent_encoder_is_blocking_but_description_substring_is_not_support() {
        let (_directory, config) = fixture(); let mut runner = fake(); runner.missing_encoder = true;
        let report = collect_with(config, &CancelToken::new(), &runner);
        assert!(check(&report, "av1_nvenc").blocking);
        assert!(report.has_blocking_issues());
    }
    #[test]
    fn doctor_failures_cancellation_and_missing_debug_checkout_are_actionable() {
        let (_directory, mut config) = fixture(); let mut runner = fake(); runner.fail = true; config.debug_build = true;
        let report = collect_with(config.clone(), &CancelToken::new(), &runner);
        assert!(report.has_blocking_issues());
        assert!(check(&report, "ffmpeg").detail.as_ref().unwrap().contains("deadline"));
        assert!(!check(&report, "debug_shaders").blocking);
        assert_eq!(check(&report, "debug_shaders").status, Status::Ready);
        assert_eq!(runner.calls.lock().unwrap().len(), 2);
        let cancel = CancelToken::new(); cancel.cancel(); let unused = fake();
        let report = collect_with(config, &cancel, &unused);
        assert_eq!(check(&report, "ffmpeg").status, Status::Cancelled);
        assert!(unused.calls.lock().unwrap().is_empty());
    }
    #[test]
    fn doctor_folder_conflicts_are_reported_without_modifying_the_file() {
        let (directory, mut config) = fixture();
        std::fs::write(directory.path().join("captures"), b"keep original").unwrap();
        config.recording_directory = directory.path().join("captures");
        let report = collect_with(config, &CancelToken::new(), &fake());
        assert!(check(&report, "recording_folder").blocking);
        assert_eq!(std::fs::read(directory.path().join("captures")).unwrap(), b"keep original");
    }

    // Reuse the native Rust test binary as a fake executable. No shell, PowerShell,
    // media file, device, encoder, or environment mutation is involved.
    fn child_invocation(name: &str) -> bool {
        let args = std::env::args().collect::<Vec<_>>();
        args.iter().any(|a| a == "--exact") && args.iter().any(|a| a == name)
    }
    #[test]
    fn doctor_child_hang() {
        if child_invocation("setup_diagnostics::tests::doctor_child_hang") { std::thread::sleep(Duration::from_secs(60)); }
    }
    #[test]
    fn doctor_child_failure() {
        if child_invocation("setup_diagnostics::tests::doctor_child_failure") { panic!("synthetic nonzero tool exit"); }
    }
    #[test]
    fn doctor_child_large_output() {
        if child_invocation("setup_diagnostics::tests::doctor_child_large_output") {
            use std::io::Write;
            std::io::stdout().write_all(&vec![b'x'; 2 * 1024 * 1024]).unwrap();
            std::io::stderr().write_all(&vec![b'y'; 32 * 1024]).unwrap();
        }
    }
    fn run_child(name: &str, timeout: Duration) -> Result<ProcessOutput, String> {
        SystemRunner.run(&std::env::current_exe().unwrap(), &["--exact", name, "--nocapture"], timeout, &CancelToken::new())
    }
    #[test]
    fn doctor_native_supervisor_bounds_hang_failure_and_output() {
        let started = Instant::now();
        let hung = run_child("setup_diagnostics::tests::doctor_child_hang", Duration::from_millis(100));
        assert!(hung.is_err());
        assert!(started.elapsed() < Duration::from_secs(3), "Hung diagnostic escaped its process deadline");
        let failed = run_child("setup_diagnostics::tests::doctor_child_failure", Duration::from_secs(3)).unwrap();
        assert!(!failed.success && failed.stderr.len() <= 4096);
        let large = run_child("setup_diagnostics::tests::doctor_child_large_output", Duration::from_secs(3)).unwrap();
        assert!(large.success);
        assert_eq!(large.stdout.len(), STDOUT_HEAD_BYTES);
        assert!(large.stderr.len() <= 4096);
    }
}
