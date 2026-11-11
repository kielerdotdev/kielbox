//! Scripted fixtures over real media, for headless renders and input replays on any
//! platform: `--scene library-fixture`, `--scene editor-fixture`, and `--shot-fixture` for the
//! screenshot overlay. Media come from `KIELBOX_FIXTURES` (a folder of captures). They never
//! read preferences, scan the capture folder, or touch the screen. The two Cloud scenes
//! (`cloud-settings-fixture`, `cloud-fixture`) are synthetic and need no media.
use crate::{
    cloud::{self, Known},
    rune_cloud::{Activity, Test},
    rune_ui::{Page, State},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

fn folder() -> PathBuf {
    std::env::var_os("KIELBOX_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kielbox-fixtures"))
}

/// The fixture folder's captures, newest first, with their modification times.
fn captures() -> Vec<(PathBuf, SystemTime)> {
    let mut files: Vec<_> = std::fs::read_dir(folder())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| crate::recordings::is_supported(p))
        .filter_map(|p| Some((p.clone(), std::fs::metadata(&p).ok()?.modified().ok()?)))
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1));
    files
}

/// The library with real thumbnails, just after a capture went to the trash.
pub fn library(s: &mut State) {
    s.go(Page::Library);
    for (path, time) in captures() {
        s.file_times.insert(path.clone(), time);
        s.files.push(path);
    }
    let wake = s.wake.clone();
    s.thumbs.sync(&s.files, &wake);
    s.thumbs.wait(Duration::from_secs(30));
    s.notice = "Moved to capture trash.".into();
    s.fixture_trash();
}

/// The editor on the newest recording, split in two, with its first frame decoded.
pub fn editor(s: &mut State) {
    let Some(video) = captures()
        .into_iter()
        .map(|(p, _)| p)
        .find(|p| !crate::recordings::is_image(p))
    else {
        s.notice = "No fixture recording.".into();
        return;
    };
    s.open(video);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        s.editor.poll();
        if s.editor.editor.is_some() && s.editor.has_frame() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if let Some(total) = s.editor.editor.as_ref().map(|e| e.project.duration()) {
        s.editor.seek(total * 6 / 10);
        s.editor.split_at_position();
        s.editor.seek(0);
        s.editor.settle(Duration::from_secs(10));
    }
}

const CLOUD_URL: &str = "https://cloud.example.com";

/// Settings, Cloud tab: address set, service token saved, connection tested.
pub fn cloud_settings(s: &mut State) {
    s.preferences.cloud_url = CLOUD_URL.into();
    s.preferences.cloud_auto_upload = true;
    s.cloud.url = rune::TextEdit::new(CLOUD_URL);
    s.cloud.saved_in = Some("Windows Credential Manager".into());
    s.cloud.test = Test::Connected("kielbox-desktop".into());
    s.go(Page::Settings);
    s.settings_tab = 5;
}

/// The Library with a card in each cloud state: not uploaded, queued, uploading, uploaded,
/// failed, and creating a share link. Synthetic captures, so it renders without media.
pub fn cloud_cards(s: &mut State) {
    s.go(Page::Library);
    s.preferences.cloud_url = CLOUD_URL.into();
    let now = SystemTime::now();
    let names = [
        "Screenshot not uploaded.png",
        "Recording queued.mkv",
        "Recording uploading.mkv",
        "Screenshot uploaded.png",
        "Replay upload failed.mkv",
        "Recording sharing.mkv",
    ];
    for (i, name) in names.iter().enumerate() {
        let path = PathBuf::from(name);
        s.file_times
            .insert(path.clone(), now - Duration::from_secs(60 * (i as u64 + 1)));
        s.files.push(path);
    }
    let path = |i: usize| PathBuf::from(names[i]);
    s.cloud.activity.insert(path(1), Activity::Queued);
    s.cloud.activity.insert(
        path(2),
        Activity::Uploading {
            sent: 420,
            total: 1000,
        },
    );
    s.cloud.known.insert(
        path(3),
        Known {
            item_id: "01FIXTURE".into(),
            size: 1000,
            modified_ms: cloud::unix_ms(s.file_times[&path(3)]),
        },
    );
    s.cloud
        .activity
        .insert(path(4), Activity::Failed("Cloud: network error".into()));
    s.cloud.activity.insert(path(5), Activity::Sharing);
}
