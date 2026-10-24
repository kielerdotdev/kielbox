//! Worker-only FFmpeg discovery. A loaded family is held until process exit;
//! changing a loaded/incompatible runtime requires restarting the application.
//! No process search paths or environment variables are modified here.
use anyhow::{bail, ensure, Context, Result};
use libloading::Library;
use serde::Serialize;
use std::{
    collections::VecDeque,
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf, Prefix},
    sync::{Arc, Mutex, OnceLock, TryLockError},
    time::{Duration, Instant},
};
use windows::{
    core::PCWSTR,
    Win32::{
        Storage::FileSystem::GetDriveTypeW,
        System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW},
    },
};

pub const MAX_RUNTIME_CANDIDATES: usize = 32;
const MAX_INPUT_CANDIDATES: usize = 256;
const NEGATIVE_LIMIT: usize = 8;
const NEGATIVE_TTL: Duration = Duration::from_secs(30);
const DISCOVERY_BUDGET: Duration = Duration::from_secs(3);
const COMPONENTS: [(&str, &str, u32); 4] = [
    ("avutil-59.dll", "avutil_version", 59),
    ("avcodec-61.dll", "avcodec_version", 61),
    ("avformat-61.dll", "avformat_version", 61),
    ("swscale-8.dll", "swscale_version", 8),
];
// These dependencies may be implicitly reused by Windows' loaded-module list.
const FAMILY_NAMES: [&str; 7] = [
    "avutil-59.dll",
    "avcodec-61.dll",
    "avformat-61.dll",
    "swscale-8.dll",
    "swresample-5.dll",
    "avfilter-10.dll",
    "avdevice-61.dll",
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum FfmpegRuntimeConfig {
    #[default]
    Automatic,
    /// First candidate; a compatible process-pinned family always takes precedence.
    Directory(PathBuf),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FfmpegRuntimeRequirement {
    /// Existing NativeScrub public-prefix ABI (59/61/61/8).
    ScrubMajorAbi,
    /// Generated full-structure bindings require exactly FFmpeg 7.0.2.
    Gpu702,
}
#[derive(Clone, Debug, Serialize)]
pub struct FfmpegRuntimeInfo {
    pub directory: PathBuf,
    /// util, codec, format, scale packed FFmpeg version integers.
    pub versions: [u32; 4],
}
#[derive(Debug)]
pub struct FfmpegRuntimeBusy;
impl std::fmt::Display for FfmpegRuntimeBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FFmpeg runtime discovery is busy; retry after the current worker finishes")
    }
}
impl std::error::Error for FfmpegRuntimeBusy {}
#[derive(Debug)]
pub struct FfmpegRuntimeCancelled;
impl std::fmt::Display for FfmpegRuntimeCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FFmpeg runtime discovery cancelled")
    }
}
impl std::error::Error for FfmpegRuntimeCancelled {}

pub(crate) struct Runtime {
    pub info: FfmpegRuntimeInfo,
    libraries: Vec<Library>,
}
impl Runtime {
    /// The caller selects an ABI-checked signature and retains this Runtime.
    pub(crate) unsafe fn symbol<T: Copy>(&self, component: usize, name: &[u8]) -> Result<T> {
        Ok(*self.libraries[component].get::<T>(name)?)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchKey {
    config: FfmpegRuntimeConfig,
    requirement: FfmpegRuntimeRequirement,
    directory: Option<OsString>,
    executable: Option<OsString>,
    path: Option<OsString>,
}
impl SearchKey {
    fn capture(config: &FfmpegRuntimeConfig, requirement: FfmpegRuntimeRequirement) -> Self {
        Self {
            config: config.clone(),
            requirement,
            directory: std::env::var_os("RCR_FFMPEG_LIB_DIR"),
            executable: std::env::var_os("RCR_FFMPEG"),
            path: std::env::var_os("PATH"),
        }
    }
    fn candidates(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let FfmpegRuntimeConfig::Directory(path) = &self.config {
            paths.push(path.clone());
        }
        if let Some(path) = &self.directory {
            paths.push(PathBuf::from(path));
        }
        if let Some(path) = self.executable.as_ref().map(PathBuf::from) {
            // An unqualified program name must never introduce CWD discovery.
            if path.is_absolute() {
                if let Some(parent) = path.parent() {
                    paths.push(parent.to_owned());
                }
            }
        }
        if let Some(path) = &self.path {
            paths.extend(
                std::env::split_paths(path).take(MAX_INPUT_CANDIDATES.saturating_sub(paths.len())),
            );
        }
        paths
    }
}
struct Negative {
    key: SearchKey,
    at: Instant,
    error: String,
}
#[derive(Default)]
struct State {
    pinned: Option<Arc<Runtime>>,
    // Failed loads can leave implicit dependency references held by third parties.
    // Never attempt a second family after an uncertain/partial actual load.
    poisoned: Option<String>,
    negatives: VecDeque<Negative>,
}
impl State {
    fn negative(&mut self, key: &SearchKey, refresh: bool, now: Instant) -> Option<String> {
        self.negatives
            .retain(|entry| now.saturating_duration_since(entry.at) < NEGATIVE_TTL);
        if refresh {
            return None;
        }
        self.negatives
            .iter()
            .find(|entry| entry.key == *key)
            .map(|entry| entry.error.clone())
    }
    fn record_negative(&mut self, key: SearchKey, error: String, now: Instant) {
        self.negatives.retain(|entry| {
            entry.key != key && now.saturating_duration_since(entry.at) < NEGATIVE_TTL
        });
        if self.negatives.len() == NEGATIVE_LIMIT {
            self.negatives.pop_front();
        }
        self.negatives.push_back(Negative {
            key,
            at: now,
            error,
        });
    }
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn compatible(versions: [u32; 4], requirement: FfmpegRuntimeRequirement) -> bool {
    versions
        .iter()
        .zip(COMPONENTS)
        .all(|(version, (_, _, major))| version >> 16 == major)
        && (requirement == FfmpegRuntimeRequirement::ScrubMajorAbi
            || versions[..3] == [pack(59, 8, 100), pack(61, 3, 100), pack(61, 1, 100)])
}
const fn pack(major: u32, minor: u32, patch: u32) -> u32 {
    (major << 16) | (minor << 8) | patch
}
fn local_drive(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let drive = match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
            _ => return false,
        },
        _ => return false,
    };
    let root = [u16::from(drive), b':' as u16, b'\\' as u16, 0];
    // Unknown/no-root/network drives cannot be safely searched.
    matches!(
        unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) },
        2 | 3 | 5 | 6
    )
}
fn path_key(path: &Path) -> OsString {
    // Windows path comparison is case insensitive for normal DLL directories.
    // No filesystem access is performed by this deduplication helper.
    OsString::from(path.as_os_str().to_string_lossy().to_lowercase())
}
fn checked_directory(path: &Path) -> Result<PathBuf> {
    ensure!(
        local_drive(path),
        "FFmpeg runtime candidate is not an absolute local drive directory"
    );
    let path = path
        .canonicalize()
        .context("FFmpeg runtime directory unavailable")?;
    ensure!(
        local_drive(&path) && path.is_dir(),
        "FFmpeg runtime directory resolves outside a local drive"
    );
    Ok(path)
}
fn loaded_family_directory() -> Result<Option<PathBuf>> {
    let mut directory: Option<PathBuf> = None;
    for name in FAMILY_NAMES {
        let wide: Vec<u16> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let Ok(module) = (unsafe { GetModuleHandleW(PCWSTR(wide.as_ptr())) }) else {
            continue;
        };
        let mut buffer = [0u16; 32768];
        let count = unsafe { GetModuleFileNameW(Some(module), &mut buffer) } as usize;
        ensure!(
            count > 0 && count < buffer.len(),
            "Cannot identify preloaded FFmpeg component {name}"
        );
        let file = PathBuf::from(OsString::from_wide(&buffer[..count]));
        let parent =
            checked_directory(file.parent().context("Preloaded FFmpeg has no directory")?)?;
        if let Some(previous) = &directory {
            ensure!(
                path_key(previous) == path_key(&parent),
                "Conflicting preloaded FFmpeg families; restart with one compatible runtime"
            );
        } else {
            directory = Some(parent);
        }
    }
    Ok(directory)
}
fn check(cancelled: &impl Fn() -> bool, started: Instant) -> Result<()> {
    if cancelled() {
        return Err(FfmpegRuntimeCancelled.into());
    }
    ensure!(
        started.elapsed() < DISCOVERY_BUDGET,
        "FFmpeg runtime discovery time budget exhausted"
    );
    Ok(())
}

/// Retry only transient admission contention, never missing/ABI/poison failures.
/// This runs on the seek worker, with cancellation checked between short waits.
pub(crate) fn retry_busy<T>(
    cancelled: impl Fn() -> bool,
    budget: Duration,
    mut operation: impl FnMut() -> Result<T>,
) -> Result<T> {
    let started = Instant::now();
    loop {
        if cancelled() {
            return Err(FfmpegRuntimeCancelled.into());
        }
        match operation() {
            Err(error)
                if error.downcast_ref::<FfmpegRuntimeBusy>().is_some()
                    && started.elapsed() < budget =>
            {
                std::thread::sleep(
                    Duration::from_millis(5).min(budget.saturating_sub(started.elapsed())),
                );
            }
            result => return result,
        }
    }
}

/// Worker-only read-only discovery probe. Loads/version-checks libraries, but
/// never opens media, activates devices, downloads or mutates process search paths.
/// `refresh` bypasses negative results, never the process runtime pin.
pub fn probe_ffmpeg_runtime(
    config: &FfmpegRuntimeConfig,
    requirement: FfmpegRuntimeRequirement,
    refresh: bool,
    cancelled: impl Fn() -> bool,
) -> Result<FfmpegRuntimeInfo> {
    Ok(load_runtime(config, requirement, refresh, cancelled)?
        .info
        .clone())
}
pub(crate) fn load_runtime(
    config: &FfmpegRuntimeConfig,
    requirement: FfmpegRuntimeRequirement,
    refresh: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Arc<Runtime>> {
    let started = Instant::now();
    check(&cancelled, started)?;
    let mut state = match STATE
        .get_or_init(|| Mutex::new(State::default()))
        .try_lock()
    {
        Ok(state) => state,
        Err(TryLockError::WouldBlock) => return Err(FfmpegRuntimeBusy.into()),
        Err(TryLockError::Poisoned(_)) => {
            bail!("FFmpeg runtime loader state unavailable; restart required")
        }
    };
    if let Some(reason) = &state.poisoned {
        bail!("FFmpeg runtime load failed; restart required: {reason}");
    }
    if let Some(runtime) = &state.pinned {
        ensure!(compatible(runtime.info.versions, requirement),
            "Pinned FFmpeg runtime is incompatible with {requirement:?}; restart with a compatible runtime");
        return Ok(runtime.clone());
    }
    let key = SearchKey::capture(config, requirement);
    if let Some(error) = state.negative(&key, refresh, Instant::now()) {
        bail!("{error} (cached for up to 30 seconds; Refresh retries)");
    }
    check(&cancelled, started)?;
    // Existing components take precedence over all search paths. Never mix a
    // partially resident family with a different directory, even after failure.
    let resident = loaded_family_directory()?;
    let candidates = resident
        .clone()
        .map_or_else(|| key.candidates(), |path| vec![path]);
    let mut unique = Vec::new();
    let mut last_error =
        String::from("FFmpeg 7 shared libraries were not found in bounded local search");
    for candidate in candidates {
        if unique.len() == MAX_RUNTIME_CANDIDATES {
            break;
        }
        check(&cancelled, started)?;
        let directory = match checked_directory(&candidate) {
            Ok(directory) => directory,
            Err(_) => continue,
        };
        let dedupe = path_key(&directory);
        if unique.contains(&dedupe) {
            continue;
        }
        unique.push(dedupe);
        let mut complete = true;
        for (name, _, _) in COMPONENTS {
            check(&cancelled, started)?;
            let file = directory.join(name);
            let valid = file
                .canonicalize()
                .ok()
                .filter(|p| local_drive(p))
                .filter(|p| {
                    p.parent()
                        .is_some_and(|parent| path_key(parent) == path_key(&directory))
                })
                .is_some_and(|p| p.is_file());
            complete &= valid;
        }
        if !complete {
            last_error = format!("Incomplete FFmpeg runtime in {}", directory.display());
            continue;
        }
        check(&cancelled, started)?;
        // From the first loader call onward, preserve every acquired handle even
        // on error/cancellation. There is never a second-family load attempt.
        let mut libraries = Vec::new();
        let mut versions = [0; 4];
        let mut attempted_load = false;
        let loaded = (|| -> Result<()> {
            use libloading::os::windows::{
                Library as WinLibrary, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            };
            for (index, (name, symbol, _)) in COMPONENTS.iter().enumerate() {
                check(&cancelled, started)?;
                attempted_load = true;
                let library: Library = unsafe {
                    WinLibrary::load_with_flags(
                        directory.join(name),
                        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
                    )?
                }
                .into();
                libraries.push(library);
                check(&cancelled, started)?;
                versions[index] = unsafe {
                    libraries[index].get::<unsafe extern "C" fn() -> u32>(symbol.as_bytes())?()
                };
            }
            let actual =
                loaded_family_directory()?.context("Loaded FFmpeg family cannot be identified")?;
            ensure!(
                path_key(&actual) == path_key(&directory),
                "Windows loaded conflicting FFmpeg dependencies"
            );
            ensure!(
                compatible(versions, FfmpegRuntimeRequirement::ScrubMajorAbi),
                "Unsupported FFmpeg major ABI: {versions:?}"
            );
            Ok(())
        })();
        if !attempted_load {
            // A cancellation/budget check before the first OS loader call has
            // acquired no family or dependencies and must not poison the process.
            return match loaded {
                Err(error) => Err(error),
                Ok(()) => anyhow::bail!("FFmpeg loader completed without attempting a DLL load"),
            };
        }
        let runtime = Arc::new(Runtime {
            info: FfmpegRuntimeInfo {
                directory,
                versions,
            },
            libraries,
        });
        state.pinned = Some(runtime.clone());
        if let Err(error) = loaded {
            if error.downcast_ref::<FfmpegRuntimeCancelled>().is_some() {
                state.poisoned = Some("cancelled during DLL loading".into());
                return Err(error);
            }
            state.poisoned = Some(format!("{error:#}"));
            return Err(
                error.context("FFmpeg runtime could not be pinned safely; restart required")
            );
        }
        ensure!(compatible(versions, requirement), "Pinned FFmpeg runtime is incompatible with {requirement:?}; restart with FFmpeg 7.0.2 for GPU preview");
        return Ok(runtime);
    }
    if resident.is_some() {
        last_error
            .push_str("; a preloaded family prevents searching another runtime (restart required)");
    }
    state.record_negative(key, last_error.clone(), Instant::now());
    bail!(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn busy_retry_preserves_typed_context_and_never_retries_real_failures() {
        let mut calls = 0;
        let value = retry_busy(
            || false,
            Duration::from_millis(100),
            || {
                calls += 1;
                if calls <= 2 {
                    Err(anyhow::Error::new(FfmpegRuntimeBusy).context("Native decoder setup"))
                } else {
                    Ok(7)
                }
            },
        )
        .unwrap();
        assert_eq!((value, calls), (7, 3));
        calls = 0;
        let error = retry_busy::<()>(
            || false,
            Duration::from_millis(100),
            || {
                calls += 1;
                anyhow::bail!("incompatible pinned ABI")
            },
        )
        .unwrap_err();
        assert_eq!(calls, 1);
        assert_eq!(error.to_string(), "incompatible pinned ABI");
        calls = 0;
        let error = retry_busy::<()>(
            || false,
            Duration::ZERO,
            || {
                calls += 1;
                Err(FfmpegRuntimeBusy.into())
            },
        )
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(error.downcast_ref::<FfmpegRuntimeBusy>().is_some());
    }
    #[test]
    fn busy_retry_observes_cancel_without_another_load_attempt() {
        let cancelled = std::cell::Cell::new(false);
        let mut calls = 0;
        let error = retry_busy::<()>(
            || cancelled.get(),
            Duration::from_millis(100),
            || {
                calls += 1;
                cancelled.set(true);
                Err(FfmpegRuntimeBusy.into())
            },
        )
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(error.downcast_ref::<FfmpegRuntimeCancelled>().is_some());
    }
    #[test]
    fn negative_cache_is_bounded_expiring_refreshable_and_environment_specific() {
        let key = |index| SearchKey {
            config: FfmpegRuntimeConfig::Directory(PathBuf::from(format!(r"C:\runtime{index}"))),
            requirement: FfmpegRuntimeRequirement::Gpu702,
            directory: None,
            executable: None,
            path: Some(r"C:\tools".into()),
        };
        let now = Instant::now();
        let mut state = State::default();
        for index in 0..=NEGATIVE_LIMIT {
            state.record_negative(key(index), format!("missing{index}"), now);
        }
        assert_eq!(state.negatives.len(), NEGATIVE_LIMIT);
        assert!(state.negative(&key(0), false, now).is_none());
        assert_eq!(
            state.negative(&key(1), false, now).as_deref(),
            Some("missing1")
        );
        assert!(state.negative(&key(1), true, now).is_none());
        let mut different = key(1);
        different.path = Some(r"C:\new-tools".into());
        assert!(state.negative(&different, false, now).is_none());
        state.record_negative(key(1), "updated".into(), now);
        assert_eq!(state.negatives.len(), NEGATIVE_LIMIT);
        assert_eq!(
            state.negative(&key(1), false, now).as_deref(),
            Some("updated")
        );
        assert!(state.negative(&key(1), false, now + NEGATIVE_TTL).is_none());
        assert!(state.negatives.is_empty());
    }
    #[test]
    fn preserves_major_scrub_abi_but_requires_exact_gpu_struct_abi() {
        let exact = [
            pack(59, 8, 100),
            pack(61, 3, 100),
            pack(61, 1, 100),
            pack(8, 1, 100),
        ];
        assert!(compatible(exact, FfmpegRuntimeRequirement::Gpu702));
        let newer = [
            pack(59, 39, 100),
            pack(61, 19, 100),
            pack(61, 7, 100),
            pack(8, 3, 100),
        ];
        assert!(compatible(newer, FfmpegRuntimeRequirement::ScrubMajorAbi));
        assert!(!compatible(newer, FfmpegRuntimeRequirement::Gpu702));
        let mut wrong = exact;
        wrong[3] = pack(9, 0, 0);
        assert!(!compatible(wrong, FfmpegRuntimeRequirement::ScrubMajorAbi));
    }
    #[test]
    fn candidate_precedence_and_finite_input_ignore_implicit_executable_cwd() {
        let mut key = SearchKey {
            config: FfmpegRuntimeConfig::Directory(PathBuf::from(r"C:\explicit")),
            requirement: FfmpegRuntimeRequirement::Gpu702,
            directory: Some(r"C:\legacy".into()),
            executable: Some(r"C:\tools\ffmpeg.exe".into()),
            path: Some(std::env::join_paths([r"C:\path1", r"C:\path2"]).unwrap()),
        };
        assert_eq!(
            key.candidates(),
            [
                r"C:\explicit",
                r"C:\legacy",
                r"C:\tools",
                r"C:\path1",
                r"C:\path2"
            ]
            .map(PathBuf::from)
        );
        key.executable = Some("ffmpeg.exe".into());
        assert_eq!(key.candidates().len(), 4);
        key.path =
            Some(std::env::join_paths((0..1000).map(|i| format!(r"C:\candidate{i}"))).unwrap());
        assert_eq!(key.candidates().len(), MAX_INPUT_CANDIDATES);
    }
    #[test]
    fn rejects_relative_unc_and_device_paths_before_filesystem_access() {
        for path in [
            "",
            "relative",
            r"C:relative",
            r"\\server\share",
            r"\\?\UNC\server\share",
            r"\\.\C:\folder",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1",
        ] {
            assert!(!local_drive(Path::new(path)), "{path}");
        }
    }
    #[test]
    fn cancellation_precedes_loader_admission_and_os_operations() {
        let error = probe_ffmpeg_runtime(
            &FfmpegRuntimeConfig::Automatic,
            FfmpegRuntimeRequirement::Gpu702,
            false,
            || true,
        )
        .unwrap_err();
        assert!(error.downcast_ref::<FfmpegRuntimeCancelled>().is_some());
    }
}
