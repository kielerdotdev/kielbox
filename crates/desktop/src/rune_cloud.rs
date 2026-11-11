//! Kielbox Cloud in the Rune UI: the Settings tab, the per-card cloud button, and the UI-side
//! model of what the `cloud` worker reports. Nothing here touches disk, the keyring or the
//! network; it sends commands to the worker and applies its events on the UI thread.
use crate::{
    cloud::{self, Command, Credentials, Event, Known},
    preferences::Preferences,
    rune_theme::{self as theme, Btn, icons},
    rune_ui::{NoticeKind, State as App, settings_header},
};
use rune::{El, TextEdit, Tx, col};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// A file must be this recent to count as a newly created capture for auto-upload.
const NEW_CAPTURE_WINDOW: Duration = Duration::from_mins(10);
/// Settings choices for new share links, in days (0 = never).
pub const SHARE_DAYS: [u32; 4] = [1, 7, 30, 0];
const SHARE_LABELS: [&str; 4] = ["1 day", "7 days", "30 days", "Never"];

/// What a card's cloud button shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Card {
    NotUploaded,
    Queued,
    /// Percent sent, 0 to 100.
    Uploading(u8),
    Uploaded,
    /// Creating the share link.
    Sharing,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    Queued,
    Uploading { sent: u64, total: u64 },
    Sharing,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Test {
    Idle,
    Running,
    Connected(String),
    Failed(String),
}

/// What `poll` asks the application to do with a worker event.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Notice(NoticeKind, String),
    /// Put this share link on the clipboard.
    Copy(String),
}

pub struct State {
    enabled: bool,
    wake: Option<rune::Waker>,
    handle: Option<cloud::Handle>,
    pub known: HashMap<PathBuf, Known>,
    pub activity: HashMap<PathBuf, Activity>,
    /// Where the service token is saved, once it is.
    pub saved_in: Option<String>,
    pub replacing: bool,
    pub test: Test,
    pub url: TextEdit,
    pub client_id: TextEdit,
    pub client_secret: TextEdit,
    /// Files present at the last scan; `None` until the first scan completes.
    baseline: Option<BTreeSet<PathBuf>>,
}

impl State {
    /// `enabled` is false for headless runs: no worker, no network, no keyring.
    pub fn new(preferences: &Preferences, enabled: bool) -> Self {
        Self {
            enabled,
            wake: None,
            handle: None,
            known: HashMap::new(),
            activity: HashMap::new(),
            saved_in: None,
            replacing: false,
            test: Test::Idle,
            url: TextEdit::new(&preferences.cloud_url),
            client_id: TextEdit::new(""),
            client_secret: TextEdit::new("").secure(),
            baseline: None,
        }
    }

    pub fn configured(preferences: &Preferences) -> bool {
        !preferences.cloud_url.trim().is_empty()
    }

    /// Called from `setup`: remember the waker and start the worker if the cloud is set up.
    pub fn start(&mut self, wake: rune::Waker, preferences: &Preferences) {
        self.wake = Some(wake);
        if Self::configured(preferences) {
            self.ensure(preferences);
        }
    }

    /// Start the worker if it is not running (after the first use, or after a shutdown that
    /// did not end the program).
    fn ensure(&mut self, preferences: &Preferences) -> bool {
        if !self.enabled {
            return false;
        }
        if self.handle.is_some() {
            return true;
        }
        let Some(wake) = self.wake.clone() else {
            return false;
        };
        match cloud::Handle::spawn(cloud::WorkerConfig::production(wake)) {
            Ok(handle) => {
                handle.send(configure(preferences));
                self.handle = Some(handle);
                true
            }
            Err(_) => false,
        }
    }

    fn send(&mut self, preferences: &Preferences, command: Command) -> bool {
        self.ensure(preferences)
            && self
                .handle
                .as_ref()
                .is_some_and(|handle| handle.send(command))
    }

    /// Push the address and share expiry to the worker after a settings change.
    pub fn configure(&mut self, preferences: &Preferences) {
        if self.handle.is_some() || Self::configured(preferences) {
            self.send(preferences, configure(preferences));
        }
    }

    pub fn upload(&mut self, preferences: &Preferences, path: PathBuf) {
        if !Self::configured(preferences) {
            return;
        }
        let previous = self.activity.insert(path.clone(), Activity::Queued);
        if !self.send(preferences, Command::Upload(path.clone())) {
            match previous {
                Some(old) => self.activity.insert(path, old),
                None => self.activity.remove(&path),
            };
        }
    }

    pub fn cancel(&mut self, preferences: &Preferences, path: PathBuf) {
        self.send(preferences, Command::Cancel(path));
    }

    pub fn share(&mut self, preferences: &Preferences, path: PathBuf) {
        if self.send(preferences, Command::Share(path.clone())) {
            self.activity.insert(path, Activity::Sharing);
        }
    }

    /// Test the typed address and token, or the saved token when none was typed.
    pub fn test(&mut self, preferences: &Preferences, url: &str) {
        let id = self.client_id.text.trim().to_owned();
        let secret = self.client_secret.text.clone();
        let credentials = (!id.is_empty() && !secret.is_empty()).then_some(Credentials {
            client_id: id,
            client_secret: secret,
        });
        self.test = Test::Running;
        let command = Command::Test {
            url: url.to_owned(),
            credentials,
        };
        if !self.send(preferences, command) {
            self.test = Test::Failed("Kielbox Cloud is not available in this window.".into());
        }
    }

    /// Stop the worker before quitting; see [`cloud::Handle::shutdown`]. Never blocks longer
    /// than the worker's grace period, and uploads do not block quitting.
    pub fn shutdown(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            handle.shutdown();
        }
        self.activity
            .retain(|_, a| matches!(a, Activity::Failed(_)));
    }

    /// How the card for `path` should look. `modified` is its mtime from the last scan.
    pub fn card(&self, path: &Path, modified: Option<SystemTime>) -> Card {
        match self.activity.get(path) {
            Some(Activity::Queued) => return Card::Queued,
            Some(Activity::Uploading { sent, total }) => {
                let percent = if *total == 0 {
                    0
                } else {
                    sent.saturating_mul(100) / total
                };
                return Card::Uploading(percent.min(100) as u8);
            }
            Some(Activity::Sharing) => return Card::Sharing,
            Some(Activity::Failed(message)) => return Card::Failed(message.clone()),
            None => {}
        }
        match (self.known.get(path), modified) {
            (Some(known), Some(time)) if known.modified_ms != cloud::unix_ms(time) => {
                Card::NotUploaded
            }
            (Some(_), _) => Card::Uploaded,
            _ => Card::NotUploaded,
        }
    }

    /// Apply the worker's events. Returns whether anything changed, and what to show or copy.
    pub fn poll(&mut self) -> (bool, Vec<Outcome>) {
        let events = self
            .handle
            .as_ref()
            .map(cloud::Handle::poll)
            .unwrap_or_default();
        let changed = !events.is_empty();
        let mut out = Vec::new();
        for event in events {
            self.apply(event, &mut out);
        }
        (changed, out)
    }

    pub fn apply(&mut self, event: Event, out: &mut Vec<Outcome>) {
        match event {
            Event::Loaded { known, warning } => {
                self.known = known.into_iter().collect();
                if let Some(warning) = warning {
                    out.push(Outcome::Notice(NoticeKind::Error, warning));
                }
            }
            Event::Credentials { saved_in } => {
                self.saved_in = saved_in;
                self.replacing = false;
            }
            Event::Tested(result) => {
                self.test = match result {
                    Ok(tested) => {
                        if tested.saved_in.is_some() {
                            self.client_id.clear();
                            self.client_secret.clear();
                        }
                        Test::Connected(tested.identity)
                    }
                    Err(message) => Test::Failed(message),
                };
            }
            Event::Started { path } => {
                self.activity
                    .insert(path, Activity::Uploading { sent: 0, total: 0 });
            }
            Event::Progress { path, sent, total } => {
                if let Some(Activity::Uploading { .. }) = self.activity.get(&path) {
                    self.activity.insert(
                        path,
                        Activity::Uploading {
                            sent,
                            total: total.unwrap_or(0),
                        },
                    );
                }
            }
            Event::Uploaded {
                path,
                item_id,
                size,
                modified_ms,
            } => {
                self.activity.remove(&path);
                self.known.insert(
                    path,
                    Known {
                        item_id,
                        size,
                        modified_ms,
                    },
                );
                out.push(Outcome::Notice(
                    NoticeKind::Success,
                    "Uploaded to the cloud.".into(),
                ));
            }
            Event::Shared { path, url } => {
                self.activity.remove(&path);
                out.push(Outcome::Copy(url));
            }
            Event::Revoked { path } => {
                self.activity.remove(&path);
                out.push(Outcome::Notice(
                    NoticeKind::Success,
                    "Share link revoked.".into(),
                ));
            }
            Event::Cancelled { path } => {
                self.activity.remove(&path);
            }
            Event::Stale { path } => {
                self.activity.remove(&path);
                self.known.remove(&path);
                out.push(Outcome::Notice(
                    NoticeKind::Error,
                    "The file changed since it was uploaded. Upload it again to share it.".into(),
                ));
            }
            Event::Failed { path, message } => {
                let message = format!("Cloud: {message}");
                if matches!(self.activity.get(&path), Some(Activity::Sharing)) {
                    // The upload is still good; only the link failed.
                    self.activity.remove(&path);
                } else {
                    self.activity
                        .insert(path, Activity::Failed(message.clone()));
                }
                out.push(Outcome::Notice(NoticeKind::Error, message));
            }
            Event::Warning(message) => out.push(Outcome::Notice(NoticeKind::Error, message)),
        }
    }

    /// Call after every library scan. Returns the captures that appeared since the previous
    /// scan and should be uploaded: new, recent, not imported by hand, not already uploaded.
    /// The first scan only records what is there, so enabling auto-upload (or starting the app)
    /// never uploads the existing library.
    pub fn new_captures(
        &mut self,
        preferences: &Preferences,
        files: &[PathBuf],
        modified: &BTreeMap<PathBuf, SystemTime>,
        now: SystemTime,
    ) -> Vec<PathBuf> {
        let current: BTreeSet<PathBuf> = files.iter().cloned().collect();
        let Some(before) = self.baseline.replace(current.clone()) else {
            return Vec::new();
        };
        if !preferences.cloud_auto_upload || !Self::configured(preferences) {
            return Vec::new();
        }
        files
            .iter()
            .filter(|path| !before.contains(*path))
            .filter(|path| !preferences.imported_files.contains(*path))
            .filter(|path| !self.known.contains_key(*path) && !self.activity.contains_key(*path))
            .filter(|path| {
                modified
                    .get(*path)
                    .and_then(|time| now.duration_since(*time).ok())
                    .is_some_and(|age| age <= NEW_CAPTURE_WINDOW)
            })
            .cloned()
            .collect()
    }
}

fn configure(preferences: &Preferences) -> Command {
    Command::Configure {
        url: preferences.cloud_url.clone(),
        share_days: preferences.cloud_share_days,
    }
}

/// Run from `rune_ui::poll`: apply worker events, copy links, queue new captures.
pub fn poll(s: &mut App, tx: &mut Tx) -> bool {
    let (changed, outcomes) = s.cloud.poll();
    for outcome in outcomes {
        match outcome {
            Outcome::Notice(kind, message) => s.notify(kind, message),
            Outcome::Copy(url) => {
                tx.copy(url.clone());
                s.notify(NoticeKind::Success, format!("Share link copied: {url}"));
            }
        }
    }
    changed
}

/// After a library scan was applied: queue newly created captures when auto-upload is on.
pub fn scanned(s: &mut App) {
    let fresh = s
        .cloud
        .new_captures(&s.preferences, &s.files, &s.file_times, SystemTime::now());
    for path in fresh {
        s.cloud.upload(&s.preferences, path);
    }
}

// ---- card control -----------------------------------------------------------------------

/// The cloud button before Favorite on a Library card, only when a cloud address is set.
pub fn card_button(s: &App, i: usize, path: &Path) -> Option<El> {
    if !State::configured(&s.preferences) {
        return None;
    }
    let key = format!("cloud-{i}");
    let target = path.to_path_buf();
    let button = match s.cloud.card(path, s.file_times.get(path).copied()) {
        Card::NotUploaded => Btn::icon_only(key, "Upload to cloud", &icons::CLOUD_UP)
            .h(24.0)
            .on(move |s, _| s.cloud.upload(&s.preferences, target.clone())),
        Card::Queued => Btn::new(key, "Queued").h(24.0).on(move |s, _| {
            s.cloud.cancel(&s.preferences, target.clone());
        }),
        Card::Uploading(percent) => Btn::new(key, format!("{percent}%"))
            .h(24.0)
            .on(move |s, _| s.cloud.cancel(&s.preferences, target.clone())),
        Card::Uploaded => Btn::icon_only(key, "Copy share link", &icons::LINK)
            .h(24.0)
            .checked(true)
            .on(move |s, _| s.cloud.share(&s.preferences, target.clone())),
        Card::Sharing => Btn::icon_only(key, "Creating share link", &icons::LINK)
            .h(24.0)
            .disabled(true),
        Card::Failed(_) => Btn::icon_only(key, "Retry upload", &icons::WARNING)
            .h(24.0)
            .rec()
            .on(move |s, _| s.cloud.upload(&s.preferences, target.clone())),
    };
    Some(button.build())
}

// ---- settings tab -----------------------------------------------------------------------

pub fn settings(s: &App) -> El {
    let p = &s.preferences;
    let c = &s.cloud;
    let mut section = col()
        .w_full()
        .child(settings_header("Cloud", None))
        .child(theme::setting(
            "Address",
            Some("Your Kielbox Cloud, for example https://cloud.example.com"),
            theme::field_shell(
                None,
                theme::input("cloud-url", "Cloud address", &c.url, |s| &mut s.cloud.url),
            )
            .w(280.0),
            false,
        ));
    if let (Some(store), false) = (&c.saved_in, c.replacing) {
        section = section.child(theme::setting(
            "Service token",
            Some(&format!("Saved in {store}")),
            Btn::new("cloud-replace", "Replace")
                .on(|s, _| s.cloud.replacing = true)
                .build(),
            false,
        ));
    } else {
        section = section
            .child(theme::setting(
                "Client ID",
                Some("From the Cloudflare Access service token"),
                theme::field_shell(
                    None,
                    theme::input("cloud-client-id", "Client ID", &c.client_id, |s| {
                        &mut s.cloud.client_id
                    }),
                )
                .w(280.0),
                false,
            ))
            .child(theme::setting(
                "Client secret",
                Some("Stored in the system credential store, never in settings"),
                theme::field_shell(
                    None,
                    theme::input(
                        "cloud-client-secret",
                        "Client secret",
                        &c.client_secret,
                        |s| &mut s.cloud.client_secret,
                    )
                    .secure(),
                )
                .w(280.0),
                false,
            ));
    }
    let status = match &c.test {
        Test::Idle => {
            "Checks the address and the token. The address is saved when you test.".to_owned()
        }
        Test::Running => "Testing...".to_owned(),
        Test::Connected(identity) => format!("Connected as {identity}"),
        Test::Failed(message) => message.clone(),
    };
    section
        .child(theme::setting(
            "Test connection",
            Some(&status),
            Btn::new("cloud-test", "Test connection")
                .disabled(c.test == Test::Running)
                .on(|s, _| test_connection(s))
                .build(),
            false,
        ))
        .child(theme::setting(
            "Upload new captures automatically",
            Some("Only captures made while this is on. Your existing library is never uploaded in bulk."),
            theme::switch(
                "cloud-auto-upload",
                "Upload new captures automatically",
                p.cloud_auto_upload,
                |s, _| {
                    s.preferences.cloud_auto_upload = !s.preferences.cloud_auto_upload;
                    s.save_preferences();
                },
            ),
            false,
        ))
        .child(theme::setting(
            "Share links expire after",
            None,
            theme::segmented(
                "cloud-share-days",
                "Share links expire after",
                SHARE_LABELS
                    .iter()
                    .zip(SHARE_DAYS)
                    .map(|(label, days)| ((*label).to_owned(), p.cloud_share_days == days))
                    .collect(),
                |s, i| {
                    s.preferences.cloud_share_days = SHARE_DAYS[i];
                    if s.save_preferences() {
                        s.cloud.configure(&s.preferences);
                    }
                },
            ),
            false,
        ))
        .child(theme::setting(
            "Web library",
            Some("Browse everything you uploaded in your browser"),
            Btn::new("cloud-open-web", "Open web library")
                .disabled(!State::configured(p))
                .on(|s, tx| {
                    let url = s.preferences.cloud_url.trim().to_owned();
                    if let Err(e) = tx.open_url(&url) {
                        s.notify(NoticeKind::Error, format!("Could not open the web library: {e}"));
                    }
                })
                .build(),
            true,
        ))
}

fn test_connection(s: &mut App) {
    let url = s.cloud.url.text.trim().to_owned();
    match url::Url::parse(&url) {
        Ok(parsed) if matches!(parsed.scheme(), "https" | "http") => {}
        _ => {
            s.cloud.test = Test::Failed("Enter the address as https://cloud.example.com".into());
            return;
        }
    }
    if s.preferences.cloud_url != url {
        s.preferences.cloud_url.clone_from(&url);
        if !s.save_preferences() {
            return;
        }
    }
    s.cloud.configure(&s.preferences);
    s.cloud.test(&s.preferences, &url);
}

#[cfg(test)]
#[path = "rune_cloud_tests.rs"]
mod tests;
