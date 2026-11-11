//! Kielbox Cloud worker: one named thread that owns a current-thread Tokio runtime, takes
//! [`Command`]s from the UI, runs a single upload at a time and reports [`Event`]s back.
//!
//! The UI never touches the network, the keyring or `cloud.json`; it sends commands and polls
//! events. Everything here is platform independent so the queue, the state file and the
//! shutdown path are tested on any host (see `cloud_tests.rs`).
//!
//! `cloud.json` lives next to `preferences.json` but is a separate file with a single
//! writer, this worker. It must not go through the preferences barrier protocol.
use bytes::Bytes;
use cloud::{CloudClient, CloudConfig, CloudError, ItemKind, NewUpload};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::mpsc as tmpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use upload::{ProgressSink, SecretStore};

/// Secret-store key of the Access service token's client id.
pub const CLIENT_ID_KEY: &str = "kielbox-cloud-client-id";
/// Secret-store key of the Access service token's client secret.
pub const CLIENT_SECRET_KEY: &str = "kielbox-cloud-client-secret";
/// The state file, next to `projects/preferences.json`.
pub const STATE_FILE: &str = "projects/cloud.json";
/// How long the worker waits for in-flight work, including the abort call, on shutdown.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(9);
const STATE_VERSION: u32 = 1;
/// A stored share link this close to its expiry is replaced rather than copied.
const SHARE_EXPIRY_MARGIN_MS: i64 = 60_000;

// ---- local state file -------------------------------------------------------------------

/// Size and modification time of a file: what makes an upload record stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub modified_ms: i64,
}

pub fn unix_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

impl Stamp {
    /// Worker-only: reads the file system.
    pub fn of(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            size: metadata.len(),
            modified_ms: unix_ms(metadata.modified()?),
        })
    }
}

/// A share link created for an uploaded item. `Debug` hides the link: it carries the token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareRecord {
    pub id: String,
    pub url: String,
    /// Unix milliseconds; `None` = never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}
impl std::fmt::Debug for ShareRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShareRecord")
            .field("id", &self.id)
            .field("url", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}
impl ShareRecord {
    fn usable(&self, now_ms: i64) -> bool {
        self.expires_at_ms
            .is_none_or(|at| at - SHARE_EXPIRY_MARGIN_MS > now_ms)
    }
}

/// What the state file remembers about one uploaded file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub item_id: String,
    pub size: u64,
    pub modified_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<ShareRecord>,
}
impl Entry {
    /// An entry counts as uploaded only while the file is the one that was uploaded.
    pub fn matches(&self, stamp: Stamp) -> bool {
        self.size == stamp.size && self.modified_ms == stamp.modified_ms
    }
}

/// `cloud.json`: `{version: 1, items: {<path>: {item_id, size, modified_ms, share?}}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateFile {
    pub version: u32,
    pub items: BTreeMap<String, Entry>,
}
impl Default for StateFile {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            items: BTreeMap::new(),
        }
    }
}
impl StateFile {
    pub fn key(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// A missing file is an empty state. An unreadable, corrupt or newer-version file is moved
    /// aside (so a later save cannot destroy it) and reported in the warning.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("Cloud upload history could not be read: {e}")),
                );
            }
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(state) if state.version == STATE_VERSION => (state, None),
            _ => {
                let aside = path.with_extension("json.unreadable");
                let moved = std::fs::rename(path, &aside).is_ok();
                (
                    Self::default(),
                    Some(format!(
                        "Cloud upload history was not understood{}; uploaded files may be offered again.",
                        if moved { " and was set aside" } else { "" }
                    )),
                )
            }
        }
    }

    /// Atomic: write a temporary file next to the target, sync it, rename over the target.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let folder = parent.unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(folder)?;
        let mut temp = tempfile::NamedTempFile::new_in(folder)?;
        serde_json::to_writer_pretty(&mut temp, self).map_err(std::io::Error::other)?;
        temp.flush()?;
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }

    /// The entry for `path`, only while the file still has the uploaded size and mtime.
    pub fn current(&self, path: &Path, stamp: Stamp) -> Option<&Entry> {
        self.items
            .get(&Self::key(path))
            .filter(|entry| entry.matches(stamp))
    }

    /// Entries whose files still match, for the UI. Missing or changed files are left in the
    /// file (a drive may only be disconnected) but not reported.
    pub fn snapshot(&self) -> Vec<(PathBuf, Known)> {
        self.items
            .iter()
            .filter_map(|(key, entry)| {
                let path = PathBuf::from(key);
                let stamp = Stamp::of(&path).ok()?;
                entry.matches(stamp).then(|| (path, Known::from(entry)))
            })
            .collect()
    }
}

/// What the UI knows about an uploaded file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub item_id: String,
    pub size: u64,
    pub modified_ms: i64,
}
impl From<&Entry> for Known {
    fn from(entry: &Entry) -> Self {
        Self {
            item_id: entry.item_id.clone(),
            size: entry.size,
            modified_ms: entry.modified_ms,
        }
    }
}

// ---- queue ------------------------------------------------------------------------------

/// First in, first out; one upload runs at a time.
#[derive(Debug, Default)]
pub struct Queue {
    waiting: VecDeque<PathBuf>,
    active: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removed {
    /// It had not started and is gone.
    Waiting,
    /// It is running; the caller must cancel its token.
    Active,
    Unknown,
}
impl Queue {
    /// False when the path is already running or waiting.
    pub fn enqueue(&mut self, path: PathBuf) -> bool {
        if self.active.as_ref() == Some(&path) || self.waiting.contains(&path) {
            return false;
        }
        self.waiting.push_back(path);
        true
    }
    /// Starts the oldest waiting path unless one is already running.
    pub fn next(&mut self) -> Option<PathBuf> {
        if self.active.is_some() {
            return None;
        }
        self.active = self.waiting.pop_front();
        self.active.clone()
    }
    /// The running upload ended.
    pub fn finish(&mut self) {
        self.active = None;
    }
    pub fn cancel(&mut self, path: &Path) -> Removed {
        if self.active.as_deref() == Some(path) {
            return Removed::Active;
        }
        match self.waiting.iter().position(|p| p == path) {
            Some(index) => {
                self.waiting.remove(index);
                Removed::Waiting
            }
            None => Removed::Unknown,
        }
    }
    #[cfg(test)]
    pub fn active(&self) -> Option<&Path> {
        self.active.as_deref()
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.waiting.len() + usize::from(self.active.is_some())
    }
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---- messages ---------------------------------------------------------------------------

/// Access service token. `Debug` never prints the secret.
#[derive(Clone)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
}
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

pub enum Command {
    /// Where to upload and how long new share links live (0 = never expire).
    Configure {
        url: String,
        share_days: u32,
    },
    /// Call `/api/me`. With `credentials`, those are tried first and saved if they work;
    /// without, the saved ones are used.
    Test {
        url: String,
        credentials: Option<Credentials>,
    },
    Upload(PathBuf),
    Cancel(PathBuf),
    Share(PathBuf),
    /// Revoke the share link of a file. The Library has no button for it yet.
    #[allow(
        dead_code,
        reason = "no UI control sends it yet; covered by the worker tests"
    )]
    Revoke(PathBuf),
    Shutdown,
}

/// A successful connection test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tested {
    pub identity: String,
    /// Where the credentials were saved, when this test saved them.
    pub saved_in: Option<String>,
}

pub enum Event {
    /// The state file was read: files that are still the uploaded version.
    Loaded {
        known: Vec<(PathBuf, Known)>,
        warning: Option<String>,
    },
    /// Whether a service token is saved, and where.
    Credentials {
        saved_in: Option<String>,
    },
    Tested(Result<Tested, String>),
    Started {
        path: PathBuf,
    },
    Progress {
        path: PathBuf,
        sent: u64,
        total: Option<u64>,
    },
    Uploaded {
        path: PathBuf,
        item_id: String,
        size: u64,
        modified_ms: i64,
    },
    Shared {
        path: PathBuf,
        url: String,
    },
    Revoked {
        path: PathBuf,
    },
    /// The file changed (or vanished) since it was uploaded: the record no longer applies.
    Stale {
        path: PathBuf,
    },
    Cancelled {
        path: PathBuf,
    },
    Failed {
        path: PathBuf,
        message: String,
    },
    /// Something non-fatal, for a notice.
    Warning(String),
}

// ---- worker -----------------------------------------------------------------------------

/// The secret store and a human name for where it keeps secrets.
pub struct Secrets {
    pub store: Arc<dyn SecretStore>,
    pub location: String,
}
impl Secrets {
    /// The OS credential store, or memory for this session when there is none. Probing it can
    /// touch D-Bus or the keychain, so this runs on the worker thread.
    pub fn open_default() -> Self {
        let store = toolkit_services::secrets::LayeredSecretStore::open();
        let location = store
            .status()
            .backend
            .map_or_else(|| "memory (this session only)".to_owned(), str::to_owned);
        Self {
            store: Arc::new(store),
            location,
        }
    }
}

type ClientFactory = Arc<dyn Fn(CloudConfig) -> Result<CloudClient, CloudError> + Send + Sync>;

pub struct WorkerConfig {
    pub state_path: PathBuf,
    /// Built on the worker thread.
    pub secrets: Box<dyn FnOnce() -> Secrets + Send>,
    pub make_client: ClientFactory,
    pub wake: rune::Waker,
    /// How long shutdown waits for the running upload to abort.
    pub grace: Duration,
}
impl WorkerConfig {
    pub fn production(wake: rune::Waker) -> Self {
        Self {
            state_path: PathBuf::from(STATE_FILE),
            secrets: Box::new(Secrets::open_default),
            make_client: Arc::new(CloudClient::new),
            wake,
            grace: SHUTDOWN_GRACE,
        }
    }
}

/// UI-side end of the worker.
pub struct Handle {
    commands: tmpsc::UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
    finished: mpsc::Receiver<()>,
    grace: Duration,
}

impl Handle {
    pub fn spawn(config: WorkerConfig) -> std::io::Result<Self> {
        let (commands, command_rx) = tmpsc::unbounded_channel();
        let (event_tx, events) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        let grace = config.grace;
        let thread = std::thread::Builder::new()
            .name("kielbox-cloud".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(run(config, command_rx, event_tx)),
                    Err(e) => {
                        let _ = event_tx
                            .send(Event::Warning(format!("Kielbox Cloud is unavailable: {e}")));
                        (config.wake)();
                    }
                }
                let _ = finished_tx.send(());
            })?;
        Ok(Self {
            commands,
            events,
            thread: Some(thread),
            finished,
            grace,
        })
    }

    /// False when the worker has already stopped.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Cancel the running upload, let it ask the server to discard the half-finished item, and
    /// wait for the worker to stop. Returns false if it did not stop in time; the thread is
    /// then left behind rather than blocking quit.
    pub fn shutdown(&mut self) -> bool {
        let Some(thread) = self.thread.take() else {
            return true;
        };
        let _ = self.commands.send(Command::Shutdown);
        let stopped = matches!(
            self.finished
                .recv_timeout(self.grace + Duration::from_secs(1)),
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected)
        );
        if stopped {
            let _ = thread.join();
        }
        stopped
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handle")
            .field("running", &self.thread.is_some())
            .finish_non_exhaustive()
    }
}

/// Results of spawned tasks, folded into the worker state by its single loop.
enum Done {
    Upload {
        path: PathBuf,
        stamp: Option<Stamp>,
        result: Result<cloud::Item, CloudError>,
    },
    Share {
        path: PathBuf,
        item_id: String,
        result: Result<ShareRecord, CloudError>,
    },
    Revoke {
        path: PathBuf,
        result: Result<(), CloudError>,
    },
    Test {
        result: Result<Tested, String>,
        saved: bool,
    },
}

struct Settings {
    url: String,
    share_days: u32,
}

struct Worker {
    state_path: PathBuf,
    secrets: Arc<Secrets>,
    make_client: ClientFactory,
    events: mpsc::Sender<Event>,
    wake: rune::Waker,
    grace: Duration,
    state: StateFile,
    queue: Queue,
    settings: Settings,
    client: Option<Arc<CloudClient>>,
    cancel: Option<CancellationToken>,
    tasks: JoinSet<()>,
    done: tmpsc::UnboundedSender<Done>,
}

async fn run(
    config: WorkerConfig,
    mut commands: tmpsc::UnboundedReceiver<Command>,
    events: mpsc::Sender<Event>,
) {
    let secrets = Arc::new((config.secrets)());
    let (state, warning) = StateFile::load(&config.state_path);
    let (done_tx, mut done_rx) = tmpsc::unbounded_channel();
    let mut worker = Worker {
        state_path: config.state_path,
        secrets,
        make_client: config.make_client,
        events,
        wake: config.wake,
        grace: config.grace,
        state,
        queue: Queue::default(),
        settings: Settings {
            url: String::new(),
            share_days: 7,
        },
        client: None,
        cancel: None,
        tasks: JoinSet::new(),
        done: done_tx,
    };
    worker.emit(Event::Loaded {
        known: worker.state.snapshot(),
        warning,
    });
    let saved_in = worker.saved_credentials_location();
    worker.emit(Event::Credentials { saved_in });
    loop {
        worker.start_next();
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Shutdown) => break,
                Some(command) => worker.command(command),
            },
            Some(done) = done_rx.recv() => worker.finished(done),
            Some(_) = worker.tasks.join_next(), if !worker.tasks.is_empty() => {}
        }
    }
    worker.shut_down().await;
}

impl Worker {
    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
        (self.wake)();
    }

    fn saved_credentials_location(&self) -> Option<String> {
        let id = self.secrets.store.get(CLIENT_ID_KEY).ok().flatten()?;
        let secret = self.secrets.store.get(CLIENT_SECRET_KEY).ok().flatten()?;
        (!id.is_empty() && !secret.is_empty()).then(|| self.secrets.location.clone())
    }

    fn save_state(&self) {
        if let Err(e) = self.state.save(&self.state_path) {
            self.emit(Event::Warning(format!(
                "Cloud upload history could not be saved: {e}"
            )));
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Configure { url, share_days } => {
                self.settings = Settings {
                    url: url.trim().to_owned(),
                    share_days,
                };
                self.client = None;
            }
            Command::Test { url, credentials } => self.test(&url, credentials),
            Command::Upload(path) => {
                self.queue.enqueue(path);
            }
            Command::Cancel(path) => match self.queue.cancel(&path) {
                Removed::Waiting => self.emit(Event::Cancelled { path }),
                Removed::Active => {
                    if let Some(token) = &self.cancel {
                        token.cancel();
                    }
                }
                Removed::Unknown => {}
            },
            Command::Share(path) => self.share(path),
            Command::Revoke(path) => self.revoke(path),
            Command::Shutdown => {}
        }
    }

    /// Credentials from the secret store plus the configured address.
    fn client(&mut self) -> Result<Arc<CloudClient>, String> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let creds = self.saved_credentials()?;
        let client = Arc::new(build_client(&self.make_client, &self.settings.url, creds)?);
        self.client = Some(client.clone());
        Ok(client)
    }

    fn saved_credentials(&self) -> Result<Credentials, String> {
        let read = |key| {
            self.secrets
                .store
                .get(key)
                .map_err(|e| e.to_string())
                .map(|v| v.filter(|v| !v.is_empty()))
        };
        match (read(CLIENT_ID_KEY)?, read(CLIENT_SECRET_KEY)?) {
            (Some(client_id), Some(client_secret)) => Ok(Credentials {
                client_id,
                client_secret,
            }),
            _ => Err("Add the Access service token in Settings, Cloud.".into()),
        }
    }

    fn start_next(&mut self) {
        while let Some(path) = self.queue.next() {
            match self.client() {
                Ok(client) => {
                    let token = CancellationToken::new();
                    self.cancel = Some(token.clone());
                    self.emit(Event::Started { path: path.clone() });
                    let sink = Arc::new(Sink {
                        path: path.clone(),
                        events: self.events.clone(),
                        wake: self.wake.clone(),
                    });
                    let done = self.done.clone();
                    self.tasks
                        .spawn(upload_task(client, path, sink, token, done));
                    return;
                }
                Err(message) => {
                    self.queue.finish();
                    self.emit(Event::Failed { path, message });
                }
            }
        }
    }

    fn test(&mut self, url: &str, credentials: Option<Credentials>) {
        let typed = credentials.is_some();
        let creds = match credentials.map_or_else(|| self.saved_credentials(), Ok) {
            Ok(creds) => creds,
            Err(message) => {
                self.emit(Event::Tested(Err(message)));
                return;
            }
        };
        let make = self.make_client.clone();
        let secrets = self.secrets.clone();
        let url = url.trim().to_owned();
        let done = self.done.clone();
        self.tasks.spawn(async move {
            let outcome = async {
                let client = build_client(&make, &url, creds.clone())?;
                let me = client.me().await.map_err(|e| e.to_string())?;
                Ok::<_, String>(me.identity)
            }
            .await;
            let (result, saved) = match outcome {
                Ok(identity) if typed => {
                    let stored = secrets
                        .store
                        .set(CLIENT_ID_KEY, &creds.client_id)
                        .and_then(|()| secrets.store.set(CLIENT_SECRET_KEY, &creds.client_secret));
                    match stored {
                        Ok(()) => (
                            Ok(Tested {
                                identity,
                                saved_in: Some(secrets.location.clone()),
                            }),
                            true,
                        ),
                        Err(e) => (
                            Err(format!("Connected, but the token could not be saved: {e}")),
                            false,
                        ),
                    }
                }
                Ok(identity) => (
                    Ok(Tested {
                        identity,
                        saved_in: None,
                    }),
                    false,
                ),
                Err(message) => (Err(message), false),
            };
            let _ = done.send(Done::Test { result, saved });
        });
    }

    fn share(&mut self, path: PathBuf) {
        let Ok(stamp) = Stamp::of(&path) else {
            self.emit(Event::Stale { path });
            return;
        };
        let Some(entry) = self.state.current(&path, stamp) else {
            self.emit(Event::Stale { path });
            return;
        };
        if let Some(share) = entry
            .share
            .as_ref()
            .filter(|s| s.usable(unix_ms(SystemTime::now())))
        {
            let url = share.url.clone();
            self.emit(Event::Shared { path, url });
            return;
        }
        let item_id = entry.item_id.clone();
        let client = match self.client() {
            Ok(client) => client,
            Err(message) => {
                self.emit(Event::Failed { path, message });
                return;
            }
        };
        let expires = (self.settings.share_days > 0)
            .then(|| Duration::from_secs(u64::from(self.settings.share_days) * 86_400));
        let done = self.done.clone();
        self.tasks.spawn(async move {
            let result = client
                .create_share(&item_id, expires)
                .await
                .and_then(|share| {
                    let url = share
                        .url
                        .clone()
                        .ok_or_else(|| CloudError::InvalidResponse {
                            why: "the server returned no share link".into(),
                        })?;
                    Ok(ShareRecord {
                        id: share.id,
                        url,
                        expires_at_ms: share.expires_at,
                    })
                });
            let _ = done.send(Done::Share {
                path,
                item_id,
                result,
            });
        });
    }

    fn revoke(&mut self, path: PathBuf) {
        let share = self
            .state
            .items
            .get(&StateFile::key(&path))
            .and_then(|entry| entry.share.clone());
        let Some(share) = share else {
            self.emit(Event::Revoked { path });
            return;
        };
        let client = match self.client() {
            Ok(client) => client,
            Err(message) => {
                self.emit(Event::Failed { path, message });
                return;
            }
        };
        let done = self.done.clone();
        self.tasks.spawn(async move {
            let result = client.revoke_share(&share.id).await;
            let _ = done.send(Done::Revoke { path, result });
        });
    }

    fn finished(&mut self, done: Done) {
        match done {
            Done::Upload {
                path,
                stamp,
                result,
            } => {
                self.queue.finish();
                self.cancel = None;
                match (result, stamp) {
                    (Ok(item), Some(stamp)) => {
                        self.state.items.insert(
                            StateFile::key(&path),
                            Entry {
                                item_id: item.id.clone(),
                                size: stamp.size,
                                modified_ms: stamp.modified_ms,
                                share: None,
                            },
                        );
                        self.save_state();
                        self.emit(Event::Uploaded {
                            path,
                            item_id: item.id,
                            size: stamp.size,
                            modified_ms: stamp.modified_ms,
                        });
                    }
                    (Err(CloudError::Cancelled), _) => self.emit(Event::Cancelled { path }),
                    (Err(e), _) => self.emit(Event::Failed {
                        path,
                        message: e.to_string(),
                    }),
                    (Ok(_), None) => self.emit(Event::Failed {
                        path,
                        message: "The file could not be read.".into(),
                    }),
                }
            }
            Done::Share {
                path,
                item_id,
                result,
            } => match result {
                Ok(record) => {
                    let url = record.url.clone();
                    if let Some(entry) = self.state.items.get_mut(&StateFile::key(&path))
                        && entry.item_id == item_id
                    {
                        entry.share = Some(record);
                        self.save_state();
                    }
                    self.emit(Event::Shared { path, url });
                }
                Err(e) => self.emit(Event::Failed {
                    path,
                    message: e.to_string(),
                }),
            },
            Done::Revoke { path, result } => match result {
                Ok(()) => {
                    if let Some(entry) = self.state.items.get_mut(&StateFile::key(&path)) {
                        entry.share = None;
                        self.save_state();
                    }
                    self.emit(Event::Revoked { path });
                }
                Err(e) => self.emit(Event::Failed {
                    path,
                    message: e.to_string(),
                }),
            },
            Done::Test { result, saved } => {
                if saved {
                    self.client = None;
                    let saved_in = Some(self.secrets.location.clone());
                    self.emit(Event::Credentials { saved_in });
                }
                self.emit(Event::Tested(result));
            }
        }
    }

    /// Cancel what is running and give it `grace` to tell the server to discard the item.
    async fn shut_down(&mut self) {
        if let Some(token) = &self.cancel {
            token.cancel();
        }
        let _ = tokio::time::timeout(self.grace, async {
            while self.tasks.join_next().await.is_some() {}
        })
        .await;
        self.tasks.abort_all();
    }
}

fn build_client(
    make: &ClientFactory,
    url: &str,
    credentials: Credentials,
) -> Result<CloudClient, String> {
    if url.trim().is_empty() {
        return Err("Add the cloud address in Settings, Cloud.".into());
    }
    let base_url = url::Url::parse(url.trim())
        .map_err(|_| "The cloud address is not a valid URL.".to_owned())?;
    make(CloudConfig {
        base_url,
        client_id: credentials.client_id,
        client_secret: credentials.client_secret,
    })
    .map_err(|e| e.to_string())
}

struct Sink {
    path: PathBuf,
    events: mpsc::Sender<Event>,
    wake: rune::Waker,
}
impl ProgressSink for Sink {
    fn report(&self, sent: u64, total: Option<u64>) {
        let _ = self.events.send(Event::Progress {
            path: self.path.clone(),
            sent,
            total,
        });
        (self.wake)();
    }
}

async fn upload_task(
    client: Arc<CloudClient>,
    path: PathBuf,
    sink: Arc<Sink>,
    cancel: CancellationToken,
    done: tmpsc::UnboundedSender<Done>,
) {
    // Taken before the first byte: if the file changes while it uploads, the record will not
    // match the file on disk and the card goes back to "not uploaded".
    let stamp = Stamp::of(&path).ok();
    let result = match stamp {
        Some(stamp) => {
            let new = new_upload(&path, stamp);
            client.upload(new, sink, cancel).await
        }
        None => Err(CloudError::Io {
            context: format!("reading {}", path.display()),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        }),
    };
    let _ = done.send(Done::Upload {
        path,
        stamp,
        result,
    });
}

/// Library category: screenshots are images, `replay*` videos are replays, other videos
/// are recordings (the filters in the Library), anything else is `other`.
pub fn kind_for(path: &Path) -> ItemKind {
    if crate::recordings::is_image(path) {
        return ItemKind::Screenshot;
    }
    if !crate::recordings::is_supported(path) {
        return ItemKind::Other;
    }
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if name.starts_with("replay") {
        ItemKind::Replay
    } else {
        ItemKind::Recording
    }
}

fn new_upload(path: &Path, stamp: Stamp) -> NewUpload {
    let thumb = crate::rune_thumbs::cached(path);
    let duration_ms = thumb
        .as_ref()
        .and_then(|(_, seconds)| *seconds)
        .map(|seconds| (seconds * 1000.0).round() as u64);
    NewUpload {
        path: path.to_path_buf(),
        kind: kind_for(path),
        title: None,
        captured_at_ms: stamp.modified_ms,
        width: None,
        height: None,
        duration_ms,
        thumbnail_jpeg: thumb.and_then(|((w, h, rgba), _)| encode_jpeg(w, h, &rgba)),
    }
}

/// JPEG quality 80, as the plan asks. `None` if the pixels are not a valid image.
pub fn encode_jpeg(width: u32, height: u32, rgba: &[u8]) -> Option<Bytes> {
    if rgba.len() != width as usize * height as usize * 4 || width == 0 || height == 0 {
        return None;
    }
    let rgb: Vec<u8> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|[r, g, b, _]| [*r, *g, *b])
        .collect();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(Bytes::from(out))
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod tests;
