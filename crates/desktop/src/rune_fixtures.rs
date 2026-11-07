//! Scripted fixtures over real media, for headless renders and input replays on any
//! platform: `--scene library-fixture`, `--scene editor-fixture`, and `--shot-fixture` for the
//! screenshot overlay. Media come from `KIELBOX_FIXTURES` (a folder of captures). They never
//! read preferences, scan the capture folder, or touch the screen.
use crate::rune_ui::{Page, State};
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
