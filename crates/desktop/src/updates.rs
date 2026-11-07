//! Installed-app updates. Network and hashing stay off the UI thread; applying
//! an update is a separate post-shutdown operation owned by main().
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime},
};

pub const DOWNLOAD_PAGE: &str = "https://github.com/kielerdotdev/kielbox/releases";
const INITIAL_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const RETRY_DELAY: Duration = Duration::from_secs(15 * 60);
const MAX_FEED_BYTES: u64 = 64 * 1024;
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Preview,
    Stable,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preview => "Preview",
            Self::Stable => "Stable",
        }
    }

    fn feed(self) -> &'static str {
        match self {
            Self::Preview => "https://github.com/kielerdotdev/kielbox/releases/download/preview/",
            Self::Stable => "https://github.com/kielerdotdev/kielbox/releases/download/stable/",
        }
    }

    fn installed(value: &str) -> Result<Self, String> {
        match value {
            "preview" => Ok(Self::Preview),
            "stable" => Ok(Self::Stable),
            _ => Err("This installation uses an unsupported update channel. Install a current Kielbox build.".into()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub automatic_checks: bool,
    pub automatic_downloads: bool,
    pub install_on_quit: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            automatic_checks: true,
            automatic_downloads: true,
            install_on_quit: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub bytes: u64,
    /// Plain text only. The UI must not render the feed's HTML release notes.
    pub notes: String,
}

impl Release {
    fn from_asset(asset: &Asset) -> Self {
        Self {
            version: asset.version.clone(),
            bytes: asset.size,
            notes: asset.notes.chars().take(8192).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    NotInstalled,
    Idle,
    Checking,
    UpToDate,
    NoReleases,
    Available(Release),
    Downloading(Release),
    Ready(Release),
    Error(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    schema_version: u32,
    appid: String,
    channel: String,
    version: String,
    file: String,
    sha256: String,
    size: u64,
    #[serde(default)]
    notes: String,
}

enum CheckResult {
    Current,
    Empty,
    Available(Asset),
}

trait Backend: Send + Sync {
    fn check(&self, cancelled: &AtomicBool) -> Result<CheckResult, String>;
    fn download(
        &self,
        update: &Asset,
        progress: Arc<AtomicU8>,
        cancelled: &AtomicBool,
    ) -> Result<Asset, String>;
    fn apply(&self, asset: &Asset, restart: bool, data_root: &Path) -> Result<(), String>;
}

#[derive(Clone)]
enum Source {
    Public,
    FileProof(PathBuf),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallMarker {
    schema_version: u32,
    appid: String,
    channel: String,
    version: String,
}

fn load_marker(directory: &Path) -> Result<Option<InstallMarker>, String> {
    let path = directory.join("kielbox-install.json");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot inspect the installation marker: {error}")),
        Ok(metadata) => reject_special_file(&metadata)?,
    }
    let bytes = read_bounded(File::open(path).map_err(|error| error.to_string())?, 4096)?;
    let marker: InstallMarker = serde_json::from_slice(&bytes).map_err(|_| {
        "The Kielbox installation marker is invalid. Run Setup to repair it.".to_owned()
    })?;
    if marker.schema_version != 1 || marker.appid != "Kielbox" {
        return Err(
            "The Kielbox installation marker belongs to an unsupported application.".into(),
        );
    }
    Channel::installed(&marker.channel)?;
    parse_version(&marker.version)?;
    Ok(Some(marker))
}

pub fn installed_version() -> Result<Option<String>, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let directory = executable
        .parent()
        .ok_or("Cannot locate the Kielbox installation.")?;
    Ok(load_marker(directory)?.map(|marker| marker.version))
}

struct InstalledBackend {
    channel: Channel,
    version: semver::Version,
    packages: PathBuf,
    current_directory: PathBuf,
    source: Source,
}

fn request_url(channel: Channel, file: &str) -> String {
    format!("{}{file}", channel.feed())
}

fn trusted_redirect(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

fn tls_config() -> Result<rustls::ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls_platform_verifier::BuilderVerifierExt::with_platform_verifier(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?
    .with_no_client_auth();
    Ok(tls)
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls_config()?)
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .https_only(true)
        .user_agent("Kielbox-Updater")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || !trusted_redirect(attempt.url()) {
                attempt.error("Untrusted update redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|error| bounded_error(error.without_url().to_string()))
}

/// The request and every body poll share this worker-owned runtime. Converting
/// reqwest's async builder into a blocking client leaves per-read timeout body
/// timers outside Tokio's context in reqwest 0.13.5.
struct HttpReader {
    response: reqwest::Response,
    runtime: tokio::runtime::Runtime,
    chunk: Vec<u8>,
    offset: usize,
}

impl HttpReader {
    fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    fn content_length(&self) -> Option<u64> {
        self.response.content_length()
    }

    fn error_for_status(self) -> Result<Self, reqwest::Error> {
        self.response.error_for_status_ref()?;
        Ok(self)
    }
}

impl Read for HttpReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            if self.offset < self.chunk.len() {
                let count = buffer.len().min(self.chunk.len() - self.offset);
                buffer[..count].copy_from_slice(&self.chunk[self.offset..self.offset + count]);
                self.offset += count;
                return Ok(count);
            }
            let chunk = self
                .runtime
                .block_on(self.response.chunk())
                .map_err(|error| std::io::Error::other(error.without_url()))?;
            let Some(chunk) = chunk else {
                return Ok(0);
            };
            self.chunk = chunk.to_vec();
            self.offset = 0;
        }
    }
}

impl InstalledBackend {
    fn package_directory(&self, asset: &Asset) -> PathBuf {
        self.packages.join(&asset.version)
    }

    fn remote(&self, file: &str, timeout: Duration) -> Result<HttpReader, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("Cannot start the update network runtime: {error}"))?;
        let response = runtime.block_on(async {
            http_client()?
                .get(request_url(self.channel, file))
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .timeout(timeout)
                .send()
                .await
                .map_err(|error| bounded_error(error.without_url().to_string()))
        })?;
        Ok(HttpReader {
            response,
            runtime,
            chunk: Vec::new(),
            offset: 0,
        })
    }
}

impl Backend for InstalledBackend {
    fn check(&self, cancelled: &AtomicBool) -> Result<CheckResult, String> {
        check_cancelled(cancelled)?;
        let bytes = match &self.source {
            Source::Public => {
                let response = self.remote("update.json", Duration::from_secs(30))?;
                if response.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok(CheckResult::Empty);
                }
                let response = response
                    .error_for_status()
                    .map_err(|error| bounded_error(error.without_url().to_string()))?;
                read_bounded_cancelled(response, MAX_FEED_BYTES, cancelled)?
            }
            Source::FileProof(directory) => {
                let file = match File::open(directory.join("update.json")) {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(CheckResult::Empty);
                    }
                    Err(error) => return Err(error.to_string()),
                };
                read_bounded_cancelled(file, MAX_FEED_BYTES, cancelled)?
            }
        };
        check_cancelled(cancelled)?;
        let asset: Asset = serde_json::from_slice(&bytes)
            .map_err(|_| "The update catalog is invalid or unsupported.".to_owned())?;
        validate_asset(&asset, self.channel)?;
        if parse_version(&asset.version)?
            .cmp_precedence(&self.version)
            .is_le()
        {
            return Ok(CheckResult::Current);
        }
        Ok(CheckResult::Available(asset))
    }

    fn download(
        &self,
        asset: &Asset,
        progress: Arc<AtomicU8>,
        cancelled: &AtomicBool,
    ) -> Result<Asset, String> {
        validate_asset(asset, self.channel)?;
        if parse_version(&asset.version)?
            .cmp_precedence(&self.version)
            .is_le()
        {
            return Err("The update cannot downgrade or reinstall the running version.".into());
        }
        check_cancelled(cancelled)?;
        ensure_cache_directory(&self.packages)?;
        let directory = self.package_directory(asset);
        ensure_cache_directory(&directory)?;
        discard_corrupt_cache_entry_cancelled(&directory, asset, cancelled)?;
        let destination = directory.join(&asset.file);
        if destination.exists() {
            verify_package_cancelled(&directory, asset, cancelled)?;
            return Ok(asset.clone());
        }
        let reader: Box<dyn Read> = match &self.source {
            Source::Public => {
                let response = self
                    .remote(&asset.file, Duration::from_secs(300))?
                    .error_for_status()
                    .map_err(|error| bounded_error(error.without_url().to_string()))?;
                if response
                    .content_length()
                    .is_some_and(|size| size != asset.size)
                {
                    return Err("The installer response has an unexpected size.".into());
                }
                Box::new(response)
            }
            Source::FileProof(source) => {
                Box::new(File::open(source.join(&asset.file)).map_err(|error| error.to_string())?)
            }
        };
        stage_download(reader, &directory, asset, &progress, cancelled)?;
        verify_package_cancelled(&directory, asset, cancelled)?;
        Ok(asset.clone())
    }

    fn apply(&self, asset: &Asset, restart: bool, data_root: &Path) -> Result<(), String> {
        validate_asset(asset, self.channel)?;
        if parse_version(&asset.version)?
            .cmp_precedence(&self.version)
            .is_le()
        {
            return Err("The update version is no longer eligible.".into());
        }
        let marker = load_marker(&self.current_directory)?
            .ok_or("The installation marker is missing. Run Setup to repair Kielbox.")?;
        if parse_version(&marker.version)? != self.version
            || Channel::installed(&marker.channel)? != self.channel
        {
            return Err("This installation changed after the update was downloaded. Restart Kielbox before updating.".into());
        }
        let directory = self.package_directory(asset);
        verify_cache_directory(&directory)?;
        verify_package(&directory, asset)?;
        validate_data_root(data_root)?;
        ensure_installation_idle(&self.current_directory)?;
        let installer = directory
            .join(&asset.file)
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let handoff = crate::update_handoff::begin_with_installer(
            &asset.version,
            &self.current_directory,
            &installer,
        )?;
        let mut command = std::process::Command::new(&installer);
        command
            .args(installer_arguments(
                restart,
                data_root,
                &self.current_directory,
            ))
            .current_dir(data_root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW; no shell or process termination.
        command
            .spawn()
            .map_err(|error| format!("Could not start the Kielbox installer: {error}"))?;
        handoff.commit();
        Ok(())
    }
}

fn installer_arguments(restart: bool, data_root: &Path, install_directory: &Path) -> Vec<OsString> {
    let mut working = OsString::from("/WORKINGDIRECTORY=");
    working.push(data_root.as_os_str());
    let mut destination = OsString::from("/DIR=");
    destination.push(install_directory.as_os_str());
    let mut args = vec![
        "/VERYSILENT".into(),
        "/SUPPRESSMSGBOXES".into(),
        "/NORESTART".into(),
        format!("/WAITFORPID={}", std::process::id()).into(),
        working,
        destination,
    ];
    if restart {
        args.push("/RESTARTAPP".into());
    }
    args
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) {
        Err("Update work was cancelled during shutdown.".into())
    } else {
        Ok(())
    }
}

fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>, String> {
    read_bounded_cancelled(reader, limit, &AtomicBool::new(false))
}

fn read_bounded_cancelled(
    mut reader: impl Read,
    limit: u64,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        check_cancelled(cancelled)?;
        let capacity = buffer
            .len()
            .min((limit.saturating_sub(bytes.len() as u64) + 1) as usize);
        let count = reader
            .read(&mut buffer[..capacity])
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.len() as u64 > limit {
            return Err("The update metadata exceeds its size limit.".into());
        }
    }
}

fn ensure_cache_directory(path: &Path) -> Result<(), String> {
    // Inspect each component before descending, refusing junction/symlink cache
    // roots instead of following an unexpected redirect into user files.
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        ensure_cache_directory(parent)?;
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(format!("Cannot create the update cache: {error}")),
            }
            std::fs::symlink_metadata(path).map_err(|error| error.to_string())?
        }
        Err(error) => return Err(format!("Cannot inspect the update cache: {error}")),
    };
    if !metadata.is_dir() || is_special_file(&metadata) {
        return Err("The update cache directory is not a regular directory.".into());
    }
    Ok(())
}

fn verify_cache_directory(path: &Path) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        verify_cache_directory(parent)?;
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || is_special_file(&metadata) {
        return Err("The update cache directory has changed or is a reparse point.".into());
    }
    Ok(())
}

fn stage_download(
    mut reader: impl Read,
    directory: &Path,
    asset: &Asset,
    progress: &AtomicU8,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_cancelled(cancelled)?;
        let capacity = buffer
            .len()
            .min((asset.size.saturating_sub(bytes) + 1) as usize);
        let count = reader
            .read(&mut buffer[..capacity])
            .map_err(|error| format!("Cannot download the installer: {error}"))?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > asset.size {
            return Err("The installer exceeded its declared size.".into());
        }
        file.write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
        hash.update(&buffer[..count]);
        progress.store(((bytes * 99) / asset.size) as u8, Ordering::Relaxed);
    }
    if bytes != asset.size || !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&asset.sha256)
    {
        return Err("The downloaded installer failed its size or SHA256 integrity check. It has not been installed.".into());
    }
    check_cancelled(cancelled)?;
    file.as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    file.persist_noclobber(directory.join(&asset.file))
        .map_err(|error| format!("Cannot save the verified installer: {error}"))?;
    Ok(())
}

/// A ready package plus its expected metadata. Merely creating or cloning this
/// token starts no process and performs no installation.
#[derive(Clone)]
pub struct PreparedUpdate {
    backend: Arc<dyn Backend>,
    asset: Asset,
}

impl std::fmt::Debug for PreparedUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedUpdate")
            .field("version", &self.asset.version)
            .finish_non_exhaustive()
    }
}

impl PreparedUpdate {
    pub fn version(&self) -> &str {
        &self.asset.version
    }

    /// Call at main's final exit boundary, after every GPUI entity and worker
    /// owner has retired. This module never calls process::exit from the live
    /// UI. The installer waits for this parent to exit and aborts on a timeout;
    /// it never forcibly closes a running capture or helper.
    pub fn apply_after_shutdown(&self, restart: bool, data_root: &Path) -> Result<(), String> {
        self.backend.apply(&self.asset, restart, data_root)
    }
}

enum WorkResult {
    Checked(Result<CheckResult, String>),
    Downloaded(Result<Asset, String>),
    Failed(String),
}

struct Work {
    receiver: Receiver<WorkResult>,
    handle: JoinHandle<()>,
    result: Option<WorkResult>,
}

pub struct Updates {
    pub settings: Settings,
    status: Status,
    installed: bool,
    version: String,
    channel: Channel,
    backend: Option<Arc<dyn Backend>>,
    worker: Option<Work>,
    available: Option<Asset>,
    ready: Option<Asset>,
    progress: Arc<AtomicU8>,
    cancelled: Arc<AtomicBool>,
    reported_progress: u8,
    next_check: Instant,
    last_checked: Option<SystemTime>,
    failures: u32,
}

impl std::fmt::Debug for Updates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Updates")
            .field("status", &self.status)
            .field("installed", &self.installed)
            .field("version", &self.version)
            .field("channel", &self.channel)
            .field("busy", &self.busy())
            .finish_non_exhaustive()
    }
}

impl Updates {
    pub fn new(now: Instant) -> Self {
        match Self::installed_with_source(now, Source::Public) {
            Ok(updates) => updates,
            Err((installed, message)) => {
                let mut updates = Self::unmanaged(now);
                if installed {
                    updates.status = Status::Error(message);
                    updates.installed = true;
                }
                updates
            }
        }
    }

    /// Opt-in native proof entry point. It accepts an existing local directory,
    /// never a URL, and requires an actual managed installation. Production new()
    /// always uses the two fixed public release endpoints above.
    pub fn with_file_source_for_proof(path: &Path, now: Instant) -> Result<Self, String> {
        #[cfg(windows)]
        if path.components().any(|component| {
            matches!(component, std::path::Component::Prefix(prefix)
                if matches!(prefix.kind(), std::path::Prefix::UNC(..) | std::path::Prefix::VerbatimUNC(..)))
        }) {
            return Err("The update proof feed must be on a local drive.".into());
        }
        if !path.is_dir() {
            return Err("The update proof feed must be an existing local directory.".into());
        }
        let path = path.canonicalize().map_err(|error| error.to_string())?;
        let mut updates = Self::installed_with_source(now, Source::FileProof(path))
            .map_err(|(_, error)| error)?;
        updates.settings.automatic_checks = false;
        updates.settings.automatic_downloads = false;
        updates.settings.install_on_quit = false;
        Ok(updates)
    }

    fn installed_with_source(now: Instant, source: Source) -> Result<Self, (bool, String)> {
        let executable = std::env::current_exe().map_err(|error| (false, error.to_string()))?;
        let current_directory = executable
            .parent()
            .ok_or((false, "Cannot locate the installation.".into()))?
            .to_path_buf();
        let marker = load_marker(&current_directory)
            .map_err(|error| (true, error))?
            .ok_or((false, "This is a portable or source build.".into()))?;
        let channel = Channel::installed(&marker.channel).map_err(|error| (true, error))?;
        let version = parse_version(&marker.version).map_err(|error| (true, error))?;
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or((true, "Cannot locate the local update cache.".into()))?;
        let mut updates = Self::unmanaged(now);
        updates.version = marker.version;
        updates.channel = channel;
        updates.installed = true;
        updates.status = Status::Idle;
        updates.backend = Some(Arc::new(InstalledBackend {
            channel,
            version,
            packages: local.join("KielboxData").join("updates"),
            current_directory,
            source,
        }));
        Ok(updates)
    }

    fn unmanaged(now: Instant) -> Self {
        Self {
            settings: Settings::default(),
            status: Status::NotInstalled,
            installed: false,
            version: env!("CARGO_PKG_VERSION").into(),
            channel: Channel::Preview,
            backend: None,
            worker: None,
            available: None,
            ready: None,
            progress: Arc::new(AtomicU8::new(0)),
            cancelled: Arc::new(AtomicBool::new(false)),
            reported_progress: 0,
            next_check: now + INITIAL_DELAY,
            last_checked: None,
            failures: 0,
        }
    }

    pub fn installed(&self) -> bool {
        self.installed
    }
    pub fn current_version(&self) -> &str {
        &self.version
    }
    pub fn channel(&self) -> Channel {
        self.channel
    }
    pub fn status(&self) -> &Status {
        &self.status
    }
    pub fn progress(&self) -> u8 {
        self.progress.load(Ordering::Relaxed)
    }
    pub fn last_checked(&self) -> Option<SystemTime> {
        self.last_checked
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some()
    }
    pub fn ready(&self) -> bool {
        self.ready.is_some() && self.worker.is_none()
    }
    pub fn can_check(&self) -> bool {
        self.backend.is_some() && self.worker.is_none() && self.ready.is_none()
    }
    pub fn can_download(&self) -> bool {
        self.backend.is_some()
            && self.worker.is_none()
            && self.available.is_some()
            && self.ready.is_none()
    }

    pub fn check_now(&mut self) -> bool {
        if !self.can_check() {
            return false;
        }
        let backend = self.backend.as_ref().unwrap().clone();
        let cancelled = self.cancelled.clone();
        if !self.start(move || WorkResult::Checked(backend.check(&cancelled))) {
            return false;
        }
        self.status = Status::Checking;
        true
    }

    pub fn download(&mut self) -> bool {
        if !self.can_download() {
            return false;
        }
        let update = self.available.as_ref().unwrap().clone();
        let release = Release::from_asset(&update);
        let backend = self.backend.as_ref().unwrap().clone();
        let progress = self.progress.clone();
        progress.store(0, Ordering::Relaxed);
        let cancelled = self.cancelled.clone();
        if !self
            .start(move || WorkResult::Downloaded(backend.download(&update, progress, &cancelled)))
        {
            return false;
        }
        self.status = Status::Downloading(release);
        true
    }

    fn start(&mut self, operation: impl FnOnce() -> WorkResult + Send + 'static) -> bool {
        let (sender, receiver) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("kielbox-updates".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
                    .unwrap_or_else(|_| {
                        WorkResult::Failed(
                            "The update worker stopped unexpectedly. Try again.".into(),
                        )
                    });
                let _ = sender.send(result);
            }) {
            Ok(handle) => {
                self.worker = Some(Work {
                    receiver,
                    handle,
                    result: None,
                });
                true
            }
            Err(error) => {
                self.status = Status::Error(format!("Could not start update worker: {error}"));
                self.next_check = Instant::now() + RETRY_DELAY;
                false
            }
        }
    }

    /// Poll from the existing UI tick. The idle flag gates only automatic work;
    /// explicit Check/Download never restart the app or affect active capture.
    pub fn poll(&mut self, now: Instant, app_idle: bool) -> bool {
        let previous = self.status.clone();
        let previous_progress = self.reported_progress;
        if let Some(worker) = self.worker.as_mut() {
            if worker.result.is_none() {
                worker.result = match worker.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(WorkResult::Failed(
                        "The update worker disconnected. Try again.".into(),
                    )),
                };
            }
            if worker.handle.is_finished() {
                let worker = self.worker.take().unwrap();
                let _ = worker.handle.join();
                // Publication can race the first try_recv above. The worker has
                // now retired, so sample once more before calling it empty.
                let result = worker.result.or_else(|| worker.receiver.try_recv().ok());
                self.complete(
                    result.unwrap_or_else(|| {
                        WorkResult::Failed(
                            "The update worker returned no result. Try again.".into(),
                        )
                    }),
                    now,
                );
            }
        }
        if self.worker.is_none() && app_idle {
            if self.settings.automatic_downloads && matches!(self.status, Status::Available(_)) {
                self.download();
            } else if self.settings.automatic_checks && self.can_check() && now >= self.next_check {
                self.check_now();
            }
        }
        self.reported_progress = self.progress();
        self.status != previous || self.reported_progress != previous_progress
    }

    fn complete(&mut self, result: WorkResult, now: Instant) {
        match result {
            WorkResult::Checked(Ok(result)) => {
                self.last_checked = Some(SystemTime::now());
                self.failures = 0;
                self.next_check = now
                    + CHECK_INTERVAL
                    + Duration::from_secs(u64::from(std::process::id() % 3600));
                self.available = None;
                self.status = match result {
                    CheckResult::Current => Status::UpToDate,
                    CheckResult::Empty => Status::NoReleases,
                    CheckResult::Available(update) => {
                        let release = Release::from_asset(&update);
                        self.available = Some(update);
                        Status::Available(release)
                    }
                };
            }
            WorkResult::Downloaded(Ok(asset)) => {
                self.progress.store(100, Ordering::Relaxed);
                self.status = Status::Ready(Release::from_asset(&asset));
                self.ready = Some(asset);
            }
            WorkResult::Checked(Err(error))
            | WorkResult::Downloaded(Err(error))
            | WorkResult::Failed(error) => {
                self.last_checked = Some(SystemTime::now());
                self.failures = self.failures.saturating_add(1);
                let multiplier = 1u32 << self.failures.saturating_sub(1).min(4);
                self.next_check = now + RETRY_DELAY * multiplier;
                self.status = Status::Error(bounded_error(error));
            }
        }
    }

    pub fn prepare_ready(&self) -> Option<PreparedUpdate> {
        if self.worker.is_some() {
            return None;
        }
        Some(PreparedUpdate {
            backend: self.backend.as_ref()?.clone(),
            asset: self.ready.as_ref()?.clone(),
        })
    }

    /// Render-only fixture: no backend, network, downloader, or apply authority.
    pub fn presentation_fixture(status: Status, now: Instant) -> Self {
        let mut updates = Self::unmanaged(now);
        updates.installed = !matches!(status, Status::NotInstalled);
        updates.settings.automatic_checks = false;
        updates.settings.automatic_downloads = false;
        updates.settings.install_on_quit = false;
        updates.status = status;
        updates.version = "0.1.0-ci.100".into();
        updates
    }
}

impl Drop for Updates {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            // Native apply runs only after this bounded network/file owner retires.
            // HTTP connect and per-read deadlines are ten seconds; cancellation
            // is checked between chunks and removes unfinished temporary files.
            let _ = worker.handle.join();
        }
    }
}

fn bounded_error(error: String) -> String {
    error.chars().take(2048).collect()
}

fn parse_version(value: &str) -> Result<semver::Version, String> {
    if value.len() > 128
        || value.is_empty()
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-+.".contains(&c))
    {
        return Err("The update version is invalid.".into());
    }
    semver::Version::parse(value).map_err(|_| "The update version is not valid SemVer.".into())
}

fn validate_asset(asset: &Asset, channel: Channel) -> Result<(), String> {
    let expected_channel = match channel {
        Channel::Preview => "preview",
        Channel::Stable => "stable",
    };
    if asset.schema_version != 1 || asset.appid != "Kielbox" || asset.channel != expected_channel {
        return Err(
            "The update catalog targets another application, channel, or unsupported format."
                .into(),
        );
    }
    let version = parse_version(&asset.version)?;
    if channel == Channel::Stable && !version.pre.is_empty() {
        return Err("The stable update catalog contains a prerelease version.".into());
    }
    if asset.file != format!("Kielbox-{}-Setup.exe", asset.version) {
        return Err("The update catalog contains an unexpected installer filename.".into());
    }
    if asset.sha256.len() != 64 || !asset.sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("The update has no valid SHA256 installer checksum.".into());
    }
    if asset.size == 0 || asset.size > MAX_PACKAGE_BYTES {
        return Err("The installer size is invalid or exceeds the 1 GiB limit.".into());
    }
    if asset.notes.len() > 8192 {
        return Err("The update notes exceed their size limit.".into());
    }
    Ok(())
}

fn is_special_file(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
}

fn reject_special_file(metadata: &std::fs::Metadata) -> Result<(), String> {
    if !metadata.is_file() || is_special_file(metadata) {
        Err("The update file is not a regular file or is a reparse point.".into())
    } else {
        Ok(())
    }
}

fn verify_package(packages: &Path, asset: &Asset) -> Result<(), String> {
    verify_package_cancelled(packages, asset, &AtomicBool::new(false))
}

fn verify_package_cancelled(
    packages: &Path,
    asset: &Asset,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let path = packages.join(&asset.file);
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("Cannot inspect the downloaded update: {error}"))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("The downloaded update is a reparse point.".into());
        }
    }
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != asset.size {
        return Err("The downloaded update has changed or is incomplete. Check for updates and download again.".into());
    }
    let file =
        File::open(&path).map_err(|error| format!("Cannot open the downloaded update: {error}"))?;
    let mut reader = file.take(asset.size + 1);
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_cancelled(cancelled)?;
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("Cannot verify the downloaded update: {error}"))?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        hash.update(&buffer[..count]);
    }
    let actual = format!("{:x}", hash.finalize());
    if bytes != asset.size || !actual.eq_ignore_ascii_case(&asset.sha256) {
        return Err(
            "The downloaded update failed its SHA256 integrity check. It has not been installed."
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
fn discard_corrupt_cache_entry(packages: &Path, asset: &Asset) -> Result<(), String> {
    discard_corrupt_cache_entry_cancelled(packages, asset, &AtomicBool::new(false))
}

fn discard_corrupt_cache_entry_cancelled(
    packages: &Path,
    asset: &Asset,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let path = packages.join(&asset.file);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Cannot inspect the update cache: {error}")),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("The update cache entry is a reparse point.".into());
        }
    }
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("The update cache entry is not a regular file.".into());
    }
    if verify_package_cancelled(packages, asset, cancelled).is_err() {
        check_cancelled(cancelled)?;
        // Only this validated, expected installer filename in the updater's
        // own cache is removed. User data and arbitrary package names are not.
        std::fs::remove_file(&path)
            .map_err(|error| format!("Cannot replace a damaged update cache entry: {error}"))?;
    }
    Ok(())
}

fn validate_data_root(root: &Path) -> Result<(), String> {
    if !root.is_absolute() || !root.is_dir() {
        return Err(
            "The existing data folder is unavailable. The update has not been installed.".into(),
        );
    }
    Ok(())
}

/// A final, read-only preflight before starting the native updater. The desktop
/// singleton prevents another cooperating GUI owner; this also catches tools
/// such as clean which are independently running from the replaceable folder.
/// It does not claim to eliminate a subsequent uncooperative process-start race
/// or discover every outside process holding an application file open.
#[cfg(windows)]
fn ensure_installation_idle(current_directory: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStringExt;
    use windows::{
        Win32::{
            Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE},
            System::{
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                    TH32CS_SNAPPROCESS,
                },
                Threading::{
                    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                    QueryFullProcessImageNameW,
                },
            },
        },
        core::{HRESULT, PWSTR},
    };

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    let current_directory = current_directory.canonicalize().map_err(|error| {
        format!("Cannot verify the current installation before updating: {error}")
    })?;
    let mut executable_names = Vec::new();
    for entry in std::fs::read_dir(&current_directory).map_err(|error| {
        format!("Cannot inspect the current installation before updating: {error}")
    })? {
        let entry = entry.map_err(|error| format!("Cannot inspect an installed file: {error}"))?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            executable_names.push(entry.file_name().to_string_lossy().to_lowercase());
        }
    }

    let snapshot = Handle(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(|error| {
            format!("Cannot inspect running processes before updating: {error}")
        })?,
    );
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut next = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    let mut inspected = 0usize;
    let mut buffer = vec![0u16; 32_768];
    loop {
        match next {
            Ok(()) => {}
            Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => {
                return Ok(());
            }
            Err(error) => {
                return Err(format!(
                    "Could not finish checking running processes: {error}"
                ));
            }
        }
        inspected += 1;
        if inspected > 32_768 {
            return Err("Too many running processes to verify a safe update. Try again after closing other applications.".into());
        }
        let pid = entry.th32ProcessID;
        if pid != 0 && pid != std::process::id() {
            let name_length = entry
                .szExeFile
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = OsString::from_wide(&entry.szExeFile[..name_length]);
            let possible_installed_tool =
                executable_names.contains(&name.to_string_lossy().to_lowercase());
            let image = if let Ok(handle) =
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            {
                let process = Handle(handle);
                let mut length = buffer.len() as u32;
                if unsafe {
                    QueryFullProcessImageNameW(
                        process.0,
                        PROCESS_NAME_WIN32,
                        PWSTR(buffer.as_mut_ptr()),
                        &mut length,
                    )
                }
                .is_ok()
                {
                    Some(PathBuf::from(OsString::from_wide(
                        &buffer[..length as usize],
                    )))
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(image) = image {
                let resolved = image.canonicalize().unwrap_or(image);
                if image_in_installation(&current_directory, &resolved) {
                    return Err(format!(
                        "Close {} (process {pid}) before updating Kielbox. Nothing was stopped.",
                        name.to_string_lossy()
                    ));
                }
            } else if possible_installed_tool {
                return Err(format!(
                    "Could not verify running {} (process {pid}). Close it before updating Kielbox. Nothing was stopped.",
                    name.to_string_lossy()
                ));
            }
        }
        next = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
}

fn image_in_installation(current_directory: &Path, image: &Path) -> bool {
    let root = normalized_windows_path(current_directory);
    let image = normalized_windows_path(image);
    !root.is_empty() && image.starts_with(&(root + "\\"))
}

fn normalized_windows_path(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('/', "\\");
    if value
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("\\\\?\\UNC\\"))
    {
        value = format!("\\\\{}", &value[8..]);
    } else if let Some(rest) = value.strip_prefix("\\\\?\\") {
        value = rest.to_owned();
    }
    let unc = value.starts_with("\\\\");
    let anchor = if unc {
        2
    } else if value.as_bytes().get(1) == Some(&b':') {
        1
    } else {
        0
    };
    let mut parts = Vec::new();
    for part in value.split('\\') {
        match part {
            "" | "." => {}
            ".." if parts.len() > anchor => {
                parts.pop();
            }
            ".." => {}
            other => parts.push(other.to_lowercase()),
        }
    }
    format!("{}{}", if unc { "\\\\" } else { "" }, parts.join("\\"))
}

#[cfg(test)]
#[path = "updates_tests.rs"]
mod tests;
