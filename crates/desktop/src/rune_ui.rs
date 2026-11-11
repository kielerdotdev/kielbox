//! Application state and Rune views. Media and filesystem jobs run on workers.
use crate::features::Feature;
#[path = "feature_controls.rs"]
mod feature_controls;
#[path = "rune_settings.rs"]
mod settings;
use crate::rune_theme::{self as theme, Btn, icons};
use crate::{
    library_scan::{LibraryScanner, ScanRequest},
    preferences::Preferences,
    recordings,
};
use rune::{AppHandle, Color, Cx, El, Key, KeyEvent, TextEdit, Tx, col, el, row, text};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, SystemTime},
};
pub const BG: u32 = theme::BG;
pub const LINE: u32 = 0x2e2e2e;
pub const ACCENT: u32 = theme::ACCENT;
pub const TEXT: u32 = theme::T1;
pub fn color(hex: u32) -> Color {
    let channel = |shift| rune::color::srgb_decode(((hex >> shift) & 255u32) as f32 / 255.0);
    Color {
        r: channel(16),
        g: channel(8),
        b: channel(0),
        a: 1.0,
    }
}
pub fn label(value: impl Into<String>) -> El {
    theme::t1(value)
}
pub fn muted(value: impl Into<String>) -> El {
    theme::small(value)
}
pub fn heading(value: impl Into<String>) -> El {
    theme::h1(value)
}
pub fn button(
    key: impl Into<String>,
    title: impl Into<String>,
    action: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    Btn::new(key, title).on(action).build()
}
pub fn field(
    key: &str,
    edit: &TextEdit,
    access: impl for<'a> Fn(&'a mut State) -> &'a mut TextEdit + 'static,
) -> El {
    theme::field(key, key, edit, access)
}
/// Shortcut text as the design writes it: `Ctrl+Shift+F10` → `Ctrl Shift F10`.
pub fn keys(binding: &str) -> String {
    binding.replace('+', " ").replace("PrintScreen", "PrtSc")
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Library,
    Capture,
    Editor,
    Disk,
    Tools,
    Settings,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Success,
    Error,
}

pub enum JobResult {
    Notice(String),
    Trash(Vec<(PathBuf, PathBuf)>),
    Restored(Vec<(PathBuf, PathBuf)>, String),
    Capture(PathBuf),
    /// A screenshot saved straight to the library.
    Saved,
    /// Work that reports elsewhere (the screenshot overlay).
    Quiet,
}
struct FileJob {
    feature: Option<Feature>,
    result: mpsc::Receiver<Result<JobResult, String>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for FileJob {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub struct State {
    pub page: Page,
    /// The rail section the first title-bar tab returns to.
    pub section: Page,
    /// Library filter: All, Replays, Recordings, Screenshots, Favorites.
    pub filter: usize,
    pub library_page: usize,
    pub trash_open: bool,
    pub recovery: Vec<(PathBuf, PathBuf)>,
    saved_preferences: Preferences,
    pub file_times: BTreeMap<PathBuf, SystemTime>,
    pub library_width: f32,
    pub settings_tab: usize,
    pub palette: Option<crate::rune_palette::Palette>,
    pub preferences: Preferences,
    pub notice: String,
    notice_kind: NoticeKind,
    notice_explicit: String,
    notice_queue: std::collections::VecDeque<(NoticeKind, String)>,
    pub search: TextEdit,
    pub files: Vec<PathBuf>,
    scanner: Option<LibraryScanner>,
    pub editor: crate::rune_editor::State,
    pub tools: crate::rune_tools::State,
    pub disk: crate::rune_tools::Disk,
    jobs: Vec<FileJob>,
    removed: Vec<(PathBuf, PathBuf)>,
    /// The notice as last seen by `poll`, and when it appeared, for auto-dismissal.
    notice_seen: String,
    notice_since: Option<std::time::Instant>,
    pub thumbs: crate::rune_thumbs::Thumbs,
    #[cfg(windows)]
    pub shot: Option<rune::OverlayHandle<crate::rune_shot::Shot>>,
    pub handle: Option<AppHandle<State>>,
    pub wake: rune::Waker,
    pub headless: bool,
    pub hotkey_fields: Vec<TextEdit>,
    #[cfg(windows)]
    pub capture: crate::rune_capture::State,
    #[cfg(windows)]
    pub updates: Option<crate::updates::Updates>,
    #[cfg(windows)]
    hotkeys: Option<crate::hotkeys::Hotkeys>,
    #[cfg(windows)]
    tray: Option<crate::tray::Tray>,
}
impl State {
    pub fn new() -> Self {
        Self::with_headless(rune::tooling::is_headless())
    }
    pub(crate) fn with_headless(headless: bool) -> Self {
        let preferences = if headless {
            Preferences::default()
        } else {
            Preferences::load()
        };
        if !headless {
            recordings::set_root(preferences.folder.clone());
        }
        let hotkey_fields = preferences.hotkeys.iter().map(TextEdit::new).collect();
        Self {
            page: Page::Library,
            section: Page::Library,
            filter: 0,
            library_page: 0,
            trash_open: false,
            recovery: vec![],
            saved_preferences: preferences.clone(),
            file_times: BTreeMap::new(),
            library_width: 0.0,
            settings_tab: 0,
            palette: None,
            preferences,
            notice: String::new(),
            notice_kind: NoticeKind::Error,
            notice_explicit: String::new(),
            notice_queue: Default::default(),
            search: TextEdit::new(""),
            files: vec![],
            scanner: None,
            editor: Default::default(),
            tools: Default::default(),
            disk: Default::default(),
            jobs: vec![],
            removed: vec![],
            notice_seen: String::new(),
            notice_since: None,
            thumbs: Default::default(),
            #[cfg(windows)]
            shot: None,
            handle: None,
            wake: std::sync::Arc::new(|| {}),
            headless,
            hotkey_fields,
            #[cfg(windows)]
            capture: Default::default(),
            #[cfg(windows)]
            updates: None,
            #[cfg(windows)]
            hotkeys: None,
            #[cfg(windows)]
            tray: None,
        }
    }
    pub fn has_tray(&self) -> bool {
        #[cfg(windows)]
        {
            self.tray.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
    pub fn refresh(&mut self) {
        if let Some(scanner) = &mut self.scanner {
            scanner.request(ScanRequest {
                root: self.preferences.folder.clone(),
                toolkit_files: vec![],
                imported_files: self.preferences.imported_files.clone(),
            });
        }
    }
    pub fn save_preferences(&mut self) -> bool {
        let headless = self.headless;
        self.persist_preferences(|preferences| if headless { Ok(()) } else { preferences.save() })
    }
    fn persist_preferences(
        &mut self,
        save: impl FnOnce(&Preferences) -> anyhow::Result<()>,
    ) -> bool {
        if let Err(error) = save(&self.preferences) {
            self.preferences = self.saved_preferences.clone();
            self.notify(
                NoticeKind::Error,
                format!("Settings could not be saved; previous settings restored: {error:#}"),
            );
            return false;
        }
        self.saved_preferences = self.preferences.clone();
        true
    }
    pub fn load_trash(&mut self) {
        match recordings::trash_recovery(&self.preferences.folder) {
            Ok(recovery) => {
                self.recovery = recovery.moves;
                if !recovery.warnings.is_empty() {
                    self.notify(
                        NoticeKind::Error,
                        format!(
                            "Some Trash entries need manual recovery: {}",
                            recovery.warnings.join("; ")
                        ),
                    );
                }
            }
            Err(error) => self.notice = format!("Could not read Trash: {error:#}"),
        }
    }
    pub fn request_quit(&mut self, tx: &mut Tx) {
        if !self.prepare_shutdown() {
            let reason = self
                .editor
                .annotation
                .as_ref()
                .map(|a| a.message.as_str())
                .filter(|message| !message.is_empty())
                .unwrap_or(&self.editor.message);
            self.notify(
                NoticeKind::Error,
                if reason.is_empty() {
                    "Finish or Cancel capture, stop recording, and finish active jobs before quitting.".to_owned()
                } else {
                    reason.to_owned()
                },
            );
            if let Some(handle) = &self.handle {
                handle.open();
            }
        } else {
            tx.quit();
        }
    }
    pub fn prepare_shutdown(&mut self) -> bool {
        if !self.editor.prepare_to_leave() {
            return false;
        }
        let mut blocked =
            !self.jobs.is_empty() || self.editor.playing() || crate::rune_shot::shutdown_blocked();
        #[cfg(windows)]
        {
            blocked |= self.capture.status().0 != Status::Off;
        }
        !blocked
    }
    pub fn shutdown_blocked(&self) -> bool {
        let mut blocked = !self.jobs.is_empty()
            || self.editor.playing()
            || self.editor.annotation.as_ref().is_some_and(|a| a.dirty())
            || crate::rune_shot::shutdown_blocked();
        #[cfg(windows)]
        {
            blocked |= self.capture.status().0 != Status::Off;
        }
        blocked
    }
    pub fn job(&mut self, work: impl FnOnce() -> Result<JobResult, String> + Send + 'static) {
        self.spawn_job(None, work);
    }
    pub fn feature_job(&mut self, feature: Feature, work: impl FnOnce() -> Result<JobResult, String> + Send + 'static) {
        if self.require(feature) { self.spawn_job(Some(feature), work); }
    }
    fn spawn_job(&mut self, feature: Option<Feature>, work: impl FnOnce() -> Result<JobResult, String> + Send + 'static) {
        let (tx, rx) = mpsc::sync_channel(1);
        let wake = self.wake.clone();
        match std::thread::Builder::new()
            .name("kielbox-file-job".into())
            .spawn(move || {
                let _ = tx.send(work());
                wake();
            }) {
            Ok(worker) => self.jobs.push(FileJob {
                feature,
                result: rx,
                worker: Some(worker),
            }),
            Err(e) => self.notice = e.to_string(),
        }
    }
    /// Go to a rail section (Library, Capture, Tools, Settings).
    pub fn go(&mut self, page: Page) {
        if !self.page_available(page) {
            self.notify(NoticeKind::Error, "This page is disabled. See Settings > Features.");
            return;
        }
        self.page = page;
        if matches!(
            page,
            Page::Library | Page::Capture | Page::Tools | Page::Settings
        ) {
            self.section = page;
        }
    }
    pub fn go_disk(&mut self) {
        if !self.require(Feature::StorageAnalysis) { return; }
        self.page = Page::Disk;
        self.section = Page::Tools;
    }
    pub fn open(&mut self, path: PathBuf) {
        if !self.require(if recordings::is_image(&path) { Feature::ImageEditing } else { Feature::VideoEditing }) { return; }
        if !self.editor.prepare_to_leave() {
            self.page = Page::Editor;
            return;
        }
        self.page = Page::Editor;
        self.editor.open(path, self.wake.clone());
    }
    /// A pending undo for the library fixture's toast.
    pub(crate) fn fixture_trash(&mut self) {
        self.removed = vec![(PathBuf::from("trashed.mkv"), PathBuf::from("trashed.mkv"))];
    }
    /// Legacy producers write `notice` directly; those outcomes remain persistent
    /// until explicitly dismissed. Only declared success outcomes auto-dismiss.
    pub fn notify(&mut self, kind: NoticeKind, message: impl Into<String>) {
        let message = message.into();
        if !self.notice.is_empty() && self.notice != message {
            self.notice_queue.push_back((kind, message));
        } else {
            self.notice = message.clone();
            self.notice_explicit = message;
            self.notice_kind = kind;
        }
    }
    pub fn dismiss_notice(&mut self) {
        self.notice_seen.clear();
        self.notice.clear();
        if let Some((kind, message)) = self.notice_queue.pop_front() {
            self.notice = message.clone();
            self.notice_explicit = message;
            self.notice_kind = kind;
        }
    }
    fn notice_kind(&self) -> NoticeKind {
        if self.notice == self.notice_explicit {
            self.notice_kind
        } else {
            NoticeKind::Error
        }
    }
    pub fn undo_trash(&mut self) {
        let mut moved = std::mem::take(&mut self.removed);
        if moved.is_empty() {
            return;
        }
        self.notice.clear();
        self.job(move || {
            let message = match recordings::restore(&mut moved) {
                Ok(()) => "Restored captures.".into(),
                Err(e) => format!("Restore incomplete: {e:#}"),
            };
            Ok(JobResult::Restored(moved, message))
        });
    }
    /// Success toasts leave by themselves; failures stay until dismissed.
    fn expire_notice(&mut self) -> bool {
        if self.notice != self.notice_seen {
            if !self.notice_seen.is_empty()
                && self.notice_kind == NoticeKind::Error
                && !self
                    .notice_queue
                    .iter()
                    .any(|(_, message)| message == &self.notice_seen)
            {
                self.notice_queue
                    .push_front((NoticeKind::Error, self.notice_seen.clone()));
            }
            if self.notice != self.notice_explicit {
                self.notice_kind = NoticeKind::Error;
            }
            self.notice_seen = self.notice.clone();
            self.notice_since = (!self.notice.is_empty()).then(std::time::Instant::now);
            return false;
        }
        let Some(since) = self.notice_since else {
            return false;
        };
        let keep = if self.removed.is_empty() { 5 } else { 10 };
        if self.notice_kind() == NoticeKind::Error || since.elapsed() < Duration::from_secs(keep) {
            return false;
        }
        self.dismiss_notice();
        self.notice_since = None;
        true
    }
    pub fn browse(&mut self, tx: &mut Tx) {
        if !self.page_available(Page::Editor) {
            self.notify(NoticeKind::Error, "Enable image or video editing in Settings > Features before opening a file.");
            return;
        }
        tx.open_file(
            rune::FileDialogOptions::default().filter(
                "Media",
                &[
                    "mkv", "mp4", "webm", "mov", "ivf", "png", "jpg", "webp", "bmp",
                ],
            ),
            |s: &mut State, result, _| match result {
                Ok(Some(path)) => {
                    if !s.require(if recordings::is_image(&path) { Feature::ImageEditing } else { Feature::VideoEditing }) { return; }
                    s.preferences.imported_files.insert(path.clone());
                    s.save_preferences();
                    s.refresh();
                    s.open(path);
                }
                Err(e) => s.notice = e.to_string(),
                _ => {}
            },
        );
    }
}
pub fn setup(s: &mut State, wake: rune::Waker) {
    s.wake = wake;
    if s.headless {
        return;
    }
    #[cfg(windows)]
    if crate::installation::current().is_some_and(|i| i.first_run) {
        s.go(Page::Settings);
        s.settings_tab = 3;
        s.notice = "Welcome to Kielbox. Choose Use existing data to open an earlier workspace on your next launch, or keep the current data folder and start capturing.".into();
    }
    if let Some(error) = &crate::features::policy().error {
        s.notify(NoticeKind::Error, error.clone());
    }
    s.scanner = Some(LibraryScanner::new());
    s.refresh();
    if let Some(notice) = crate::preferences::take_startup_notice() {
        s.notice = notice;
    }
    #[cfg(windows)]
    {
        let mut updates = crate::updates::Updates::new(std::time::Instant::now());
        updates.settings = s.preferences.updates.clone();
        s.updates = Some(updates);
        match crate::hotkeys::Hotkeys::start_with(&s.preferences.features.bindings(&s.preferences.hotkeys)) {
            Ok(keys) => s.hotkeys = Some(keys),
            Err(e) => s.notice = e.to_string(),
        }
        match crate::tray::Tray::new(&s.preferences.features) {
            Ok(tray) => s.tray = Some(tray),
            Err(e) => s.notice = e.to_string(),
        }
    }
}
pub fn poll(s: &mut State, _: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    if let Some(scanner) = &mut s.scanner {
        if let Some(mut result) = scanner.poll() {
            s.file_times.append(&mut result.modified);
            if let Some(e) = result.apply(&mut s.files) {
                s.notify(
                    NoticeKind::Error,
                    format!("Could not scan {}: {:?}", e.root.display(), e.kind),
                );
            }

            changed = true;
        }
    }
    let mut completed = Vec::new();
    s.jobs.retain(|job| match job.result.try_recv() {
        Ok(result) => {
            completed.push(result);
            false
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            completed.push(Err("File worker stopped unexpectedly.".into()));
            false
        }
        _ => true,
    });
    for result in completed {
        changed = true;
        match result {
            Ok(JobResult::Notice(v)) => s.notify(NoticeKind::Success, v),
            Ok(JobResult::Trash(moved)) => {
                s.removed = moved;
                s.notify(NoticeKind::Success, "Moved to capture trash.");
                s.refresh();
            }
            Ok(JobResult::Restored(remaining, message)) => {
                s.removed = remaining;
                s.load_trash();
                s.notify(
                    if s.removed.is_empty() {
                        NoticeKind::Success
                    } else {
                        NoticeKind::Error
                    },
                    message,
                );
                s.refresh();
            }
            Ok(JobResult::Capture(path)) => {
                s.refresh();
                if s.allows(Feature::ImageEditing) { s.open(path); }
                else { s.notify(NoticeKind::Success, "Screenshot saved to the library."); }
            }
            Ok(JobResult::Saved) => {
                s.refresh();
                s.notify(NoticeKind::Success, "Screenshot saved to the library.");
            }
            Ok(JobResult::Quiet) => {}
            Err(e) => s.notify(NoticeKind::Error, e),
        }
    }
    changed |= s.editor.poll();
    let visible: Vec<_> = library_matches(s)
        .into_iter()
        .skip(library_offset(s))
        .take(LIBRARY_PAGE_SIZE)
        .map(|(_, p)| p.clone())
        .collect();
    s.thumbs.sync(&visible, &s.wake);
    changed |= s.thumbs.poll();
    changed |= s.expire_notice();
    changed |= s.tools.poll();
    changed |= s.disk.poll();
    crate::rune_annotation::flush_save(s);
    #[cfg(windows)]
    {
        let mut capture_notices = Vec::new();
        changed |= s.capture.poll(&mut capture_notices);
        for (kind, message) in capture_notices {
            s.notify(kind, message);
        }
        let update_safe = !s.shutdown_blocked();
        if let Some(updates) = &mut s.updates {
            changed |= updates.poll(std::time::Instant::now(), update_safe);
        }
        let events: Vec<_> = s
            .hotkeys
            .as_ref()
            .map(|h| h.events.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            changed = true;
            use crate::hotkeys::Event;
            match event {
                Event::ToggleReplay => {
                    s.capture
                        .toggle(false, &s.preferences, s.wake.clone(), &mut s.notice)
                }
                Event::ToggleRecord => {
                    s.capture
                        .toggle(true, &s.preferences, s.wake.clone(), &mut s.notice)
                }
                Event::SaveReplay => s.capture.save_replay(&s.preferences, &mut s.notice),
                Event::ToggleQuickRecord => s.capture.quick(&s.preferences, &mut s.notice),
                Event::Screenshot(target) => crate::rune_tools::screenshot(s, target),
                Event::Notice(v) => s.notify(NoticeKind::Error, v),
            }
        }
        let events: Vec<_> = s
            .tray
            .as_ref()
            .map(|t| t.events())
            .unwrap_or_default();
        for event in events {
            changed = true;
            match event.as_str() {
                "quit" => s.request_quit(tx),
                "library" | "settings" => {
                    s.go(if event == "library" {
                        Page::Library
                    } else {
                        Page::Settings
                    });
                    if let Some(handle) = &s.handle {
                        handle.open();
                    }
                }
                "open" | "show" => {
                    if let Some(h) = &s.handle {
                        h.open();
                    }
                }
                "toggle-replay" => {
                    s.capture
                        .toggle(false, &s.preferences, s.wake.clone(), &mut s.notice)
                }
                "record" => s
                    .capture
                    .toggle(true, &s.preferences, s.wake.clone(), &mut s.notice),
                "replay" => s.capture.save_replay(&s.preferences, &mut s.notice),
                "quick-record" => s.capture.quick(&s.preferences, &mut s.notice),
                "fullscreen" => crate::rune_tools::screenshot(
                    s,
                    toolkit::core::workflow::CaptureTarget::Fullscreen,
                ),
                "region" => {
                    crate::rune_tools::screenshot(s, toolkit::core::workflow::CaptureTarget::Region)
                }
                _ => s.go(Page::Tools),
            }
        }
        if let Some(tray) = &mut s.tray {
            tray.update_quick_state(s.capture.quick_tray_state(), &s.preferences.hotkeys[6]);
            let _ = tray.update(s.capture.tray_state(), &s.preferences.hotkeys);
        }
    }
    let mut animate = s.editor.playing();
    #[cfg(windows)]
    {
        animate |= s.capture.active;
    }
    if !s.headless {
        tx.wake_after(Duration::from_millis(if animate { 16 } else { 100 }));
    }
    changed
}
pub fn on_key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
    if matches!(event.key, Key::Command('k')) {
        crate::rune_palette::toggle(s);
        return true;
    }
    if s.palette.is_some() {
        return crate::rune_palette::key(s, event, tx);
    }
    if tx.focused().is_some() {
        return false;
    }
    if s.page == Page::Editor && s.page_available(Page::Editor) {
        return s.editor.key(event);
    }
    if matches!(event.key, Key::Command('o')) {
        s.browse(tx);
        return true;
    }
    false
}
/// Title-bar height; on Windows the caption buttons sit over its trailing edge.
pub const TITLEBAR: f32 = 40.0;

pub fn clip_title(s: &State, path: &std::path::Path) -> String {
    s.preferences
        .custom_display_name(path)
        .map(str::to_owned)
        .unwrap_or_else(|| recordings::title(path))
}

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    cx.foreground(theme::c(theme::T1));
    cx.caption_color(theme::c(theme::T2));
    let trailing = cx.chrome_insets().trailing;
    let content = if !s.page_available(s.page) {
        page()
            .child(heading("Feature disabled"))
            .child(theme::small("Choose your tools in Settings > Features."))
    } else {
        match s.page {
            Page::Library => library(s),
            Page::Capture => capture(s),
            Page::Editor => crate::rune_editor::view(s),
            Page::Disk => crate::rune_tools::disk_view(s),
            Page::Tools => crate::rune_tools::view(s),
            Page::Settings => settings::view(s),
        }
    };
    let mut card = col()
        .key("content")
        .grow(1.0)
        .min_h(0.0)
        .min_w(0.0)
        .fill(theme::c(theme::BG))
        .corners([10.0, 0.0, 0.0, 0.0])
        .clip_corners([10.0, 0.0, 0.0, 0.0])
        .box_shadow(rune::Shadow {
            spread: 0.5,
            color: theme::white(0.08),
            ..rune::Shadow::default()
        })
        .child(content);
    if !s.notice.is_empty() {
        // Floats over the page's bottom-right corner; it never takes layout space.
        card = card.child(notice(s).pin(None, None, Some(24.0), Some(16.0)));
    }
    let mut root = col().w_full().h_full().fill(theme::c(theme::DEEP)).child(
        row().w_full().grow(1.0).min_h(0.0).child(rail(s)).child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .h_full()
                .child(titlebar(s, trailing))
                .child(card),
        ),
    );
    if let Some(palette) = crate::rune_palette::view(s) {
        root = root.child(palette);
    }
    root
}

/// Capture state as the rail, tray and toasts show it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Off,
    Ready,
    Recording,
    Saving,
}
pub fn capture_status(s: &State) -> (Status, String) {
    #[cfg(windows)]
    {
        s.capture.status()
    }
    #[cfg(not(windows))]
    {
        let _ = s;
        (Status::Off, String::new())
    }
}
pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    if total >= 3_600 {
        format!("{}:{:02}:{:02}", total / 3_600, total / 60 % 60, total % 60)
    } else {
        format!("{}:{:02}", total / 60, total % 60)
    }
}

fn rail(s: &State) -> El {
    let mut nav = col()
        .key("rail")
        .role("tablist")
        .label("Main")
        .w(72.0)
        .h_full()
        .pt(10.0)
        .pb(12.0)
        .gap(4.0)
        .items_center()
        .window_drag()
        .child(el().h(30.0).mb(10.0).center().child(theme::glyph(
            &icons::LOGO,
            22.0,
            theme::c(theme::T1),
        )));
    for (page, key, i, title) in [
        (Page::Library, "library", &icons::LIBRARY, "Library"),
        (Page::Capture, "capture", &icons::CAPTURE, "Capture"),
        (Page::Tools, "tools", &icons::TOOLS, "Tools"),
    ] {
        if !s.page_available(page) { continue; }
        let on = s.page == page || (page == Page::Tools && s.page == Page::Disk);
        nav = nav.child(
            theme::rail_item(&format!("nav-{key}"), i, title, on, move |s, _| s.go(page))
                .no_window_drag(),
        );
    }
    let (status, time) = capture_status(s);
    let (tint, word, name) = match status {
        Status::Off => (theme::T3, "Off", "Capture off"),
        Status::Ready => (theme::ACCENT, "Ready", "Replay ready"),
        Status::Recording => (theme::REC, "REC", "Recording"),
        Status::Saving => (theme::BUSY, "Saving", "Saving capture"),
    };
    let mut light = col()
        .key("nav-status")
        .role("button")
        .label(if time.is_empty() {
            name.to_string()
        } else {
            format!("{name}, {time}")
        })
        .focusable(true)
        .w(56.0)
        .pt(8.0)
        .pb(7.0)
        .gap(4.0)
        .items_center()
        .radius(8.0)
        .hover(theme::white(0.04))
        .no_window_drag()
        .on_press(|s: &mut State| s.go(Page::Capture))
        .child(theme::status_light(theme::c(tint)))
        .child(
            text(word)
                .font(theme::RAIL)
                .color(theme::c(match status {
                    Status::Recording => theme::REC_T,
                    Status::Off => theme::T3,
                    _ => theme::T1,
                }))
                .single_line(),
        );
    if !time.is_empty() {
        light = light.child(
            text(time)
                .font(theme::KBD.features("'tnum' 1"))
                .color(theme::c(theme::T3))
                .single_line(),
        );
    }
    let nav = nav.child(theme::spacer());
    let nav = if s.page_available(Page::Capture) { nav.child(light) } else { nav };
    nav
        .child(el().h(4.0))
        .child(
            theme::rail_item(
                "nav-settings",
                &icons::SETTINGS,
                "Settings",
                s.page == Page::Settings,
                |s, _| s.go(Page::Settings),
            )
            .no_window_drag(),
        )
}

fn section_tab(page: Page) -> (&'static rune::Icon, &'static str) {
    match page {
        Page::Capture => (&icons::CAPTURE, "Capture"),
        Page::Tools => (&icons::TOOLS, "Tools"),
        Page::Settings => (&icons::SETTINGS, "Settings"),
        _ => (&icons::LIBRARY, "Library"),
    }
}

fn titlebar(s: &State, trailing: f32) -> El {
    let (i, title) = section_tab(s.section);
    let mut tabs = row()
        .key("tabs")
        .role("tablist")
        .label("Open")
        .gap(4.0)
        .min_w(0.0)
        .center_y()
        .child(theme::tab(
            "tab-section",
            theme::glyph(i, 15.0, theme::c(theme::T3)),
            title,
            s.page == s.section,
            |s, _| s.page = s.section,
            None,
        ));
    if let Some(path) = &s.editor.path {
        let kind = if recordings::is_image(path) {
            &icons::IMAGE
        } else {
            &icons::FILM
        };
        tabs = tabs.child(theme::tab(
            "tab-editor",
            theme::glyph(kind, 15.0, theme::c(theme::T3)),
            &clip_title(s, path),
            s.page == Page::Editor,
            |s, _| s.page = Page::Editor,
            Some(Box::new(|s: &mut State, _| {
                if s.editor.close() && s.page == Page::Editor {
                    s.page = s.section;
                }
            })),
        ));
    }
    if let Some(tool) = s.tools.open_tool() {
        tabs = tabs.child(theme::tab(
            "tab-tool",
            theme::glyph(
                crate::rune_tools::tool_icon(tool),
                15.0,
                theme::c(theme::T3),
            ),
            crate::rune_tools::tool_title(tool),
            s.page == Page::Tools && s.section != Page::Tools,
            move |s, _| crate::rune_tools::open_tool(s, tool),
            Some(Box::new(|s: &mut State, _| {
                s.tools.close();
                if s.page == Page::Tools && s.section != Page::Tools {
                    s.page = s.section;
                }
            })),
        ));
    }
    if s.page == Page::Disk || s.disk.tree.is_some() {
        tabs = tabs.child(theme::tab(
            "tab-disk",
            theme::glyph(&icons::DISK, 15.0, theme::c(theme::T3)),
            "Disk usage",
            s.page == Page::Disk,
            |s, _| s.page = Page::Disk,
            Some(Box::new(|s: &mut State, _| {
                s.disk.close();
                if s.page == Page::Disk {
                    s.page = s.section;
                }
            })),
        ));
    }
    let search = row()
        .key("search")
        .role("button")
        .label("Search")
        .focusable(true)
        .no_window_drag()
        .w(220.0)
        .h(28.0)
        .px(10.0)
        .gap(8.0)
        .center_y()
        .radius(6.0)
        .fill(theme::white(0.03))
        .hover(theme::white(0.06))
        .box_shadow(rune::Shadow {
            spread: 0.5,
            color: theme::white(0.16),
            ..rune::Shadow::default()
        })
        .on_press(crate::rune_palette::toggle)
        .child(theme::glyph(&icons::SEARCH, 14.0, theme::c(theme::T3)))
        .child(
            text("Search")
                .font(theme::UI)
                .color(theme::c(theme::T3))
                .grow(1.0),
        )
        .child(theme::kbd("Ctrl K"));
    let mut bar = row()
        .key("titlebar")
        .h(TITLEBAR)
        .w_full()
        .pl(2.0)
        .pr(trailing.max(8.0))
        .gap(4.0)
        .center_y()
        .window_drag()
        .child(tabs)
        .child(el().grow(1.0).min_w(12.0))
        .child(search);
    let (status, _) = capture_status(s);
    if status == Status::Ready && s.page != Page::Capture {
        bar = bar.child(el().w(6.0)).child(
            Btn::new("titlebar-save-replay", "Save replay")
                .primary()
                .on(|s, _| {
                    #[cfg(windows)]
                    s.capture.save_replay(&s.preferences, &mut s.notice);
                    #[cfg(not(windows))]
                    let _ = s;
                })
                .build()
                .no_window_drag(),
        );
    }
    bar
}

fn notice(s: &State) -> El {
    let failed = s.notice_kind() == NoticeKind::Error;
    let mut toast = theme::floating(
        row()
            .key("notice")
            .role("status")
            .max_w(560.0)
            .min_w(0.0)
            .pl(14.0)
            .pr(6.0)
            .py(6.0)
            .gap(10.0)
            .center_y(),
    )
    .child(theme::glyph(
        if failed {
            &icons::WARNING
        } else {
            &icons::CHECK
        },
        16.0,
        theme::c(if failed { theme::REC_T } else { theme::ACCENT }),
    ))
    .child(
        text(s.notice.clone())
            .font(theme::SMALL)
            .color(theme::c(theme::T1))
            .wrap()
            .min_w(0.0),
    );
    if !s.removed.is_empty() {
        toast = toast.child(
            Btn::new("undo-trash", "Undo")
                .icon(&icons::UNDO)
                .h(26.0)
                .on(|s, _| s.undo_trash())
                .build(),
        );
    }
    toast.child(
        Btn::icon_only("dismiss-notice", "Dismiss", &icons::CLOSE)
            .h(26.0)
            .on(|s, _| s.dismiss_notice())
            .build(),
    )
}

/// Page padding and the header line every page starts with.
pub fn page() -> El {
    col()
        .grow(1.0)
        .min_h(0.0)
        .min_w(0.0)
        .pt(22.0)
        .px(32.0)
        .pb(32.0)
        .gap(18.0)
}
pub fn scroll(e: El) -> El {
    e.scroll(rune::ScrollSpec {
        stick_bottom: false,
    })
    .grow(1.0)
    .min_h(0.0)
}

const FILTERS: [&str; 5] = ["All", "Replays", "Recordings", "Screenshots", "Favorites"];
fn shown(s: &State, path: &std::path::Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let image = recordings::is_image(path);
    match s.filter {
        1 => !image && name.starts_with("replay"),
        2 => !image && !name.starts_with("replay"),
        3 => image,
        4 => s.preferences.favorites.contains(path),
        _ => true,
    }
}

const LIBRARY_PAGE_SIZE: usize = 100;
fn library_matches(s: &State) -> Vec<(usize, &PathBuf)> {
    let query = s.search.text.to_lowercase();
    s.files
        .iter()
        .enumerate()
        .filter(|(_, path)| {
            shown(s, path)
                && (query.is_empty()
                    || clip_title(s, path).to_lowercase().contains(&query)
                    || path.to_string_lossy().to_lowercase().contains(&query))
        })
        .collect()
}
fn library_offset(s: &State) -> usize {
    s.library_page
        .min(library_matches(s).len().saturating_sub(1) / LIBRARY_PAGE_SIZE)
        * LIBRARY_PAGE_SIZE
}
fn library(s: &State) -> El {
    if s.trash_open {
        return trash_view(s);
    }
    let header = row()
        .w_full()
        .gap(28.0)
        .center_y()
        .flex_wrap()
        .child(theme::h1("Library"))
        .child(theme::chips(
            "library-filter",
            &FILTERS,
            s.filter,
            |s, i| {
                s.filter = i;
                s.library_page = 0;
            },
        ))
        .child(theme::spacer())
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(
                    theme::field_lead(
                        Some(theme::glyph(&icons::SEARCH, 14.0, theme::c(theme::T3))),
                        "library-search",
                        "Search clips",
                        &s.search,
                        |s| &mut s.search,
                    )
                    .w(220.0),
                )
                .child(
                    Btn::icon_only("refresh", "Refresh", &icons::RESCAN)
                        .on(|s, _| s.refresh())
                        .build(),
                )
                .child(
                    Btn::new("import", "Import")
                        .hint("Ctrl O")
                        .disabled(!s.page_available(Page::Editor))
                        .on(|s, tx| s.browse(tx))
                        .build(),
                ),
        );
    let matches = library_matches(s);
    let total = matches.len();
    let offset = library_offset(s);
    let now = recordings::local_time(SystemTime::now());
    let yesterday = recordings::local_time(SystemTime::now() - Duration::from_secs(86_400));
    let columns = (((s.library_width + 16.0) / (280.0 + 16.0)).floor() as usize).max(1);
    let mut groups: Vec<(String, Vec<El>)> = Vec::new();
    let mut count = 0;
    for (i, path) in matches.into_iter().skip(offset).take(LIBRARY_PAGE_SIZE) {
        let title = clip_title(s, path);
        count += 1;
        let when = s
            .file_times
            .get(path)
            .and_then(|t| recordings::local_time(*t));
        let group = match when {
            Some(t) if Some(t.date()) == now.map(|n| n.date()) => "Today".to_string(),
            Some(t) if Some(t.date()) == yesterday.map(|n| n.date()) => "Yesterday".to_string(),
            _ => "Earlier".to_string(),
        };
        let stamp = match when {
            Some(t) if group != "Earlier" => format!("{:02}:{:02}", t.hour, t.minute),
            Some(t) => format!("{} {}", t.day, t.month_name()),
            None => String::new(),
        };
        let card = clip_card(s, i, path, title, stamp);
        match groups.last_mut() {
            Some((name, cards)) if *name == group => cards.push(card),
            _ => groups.push((group, vec![card])),
        }
    }
    let mut body = col()
        .key("library-grid")
        .w_full()
        .gap(18.0)
        .on_layout(|s: &mut State, r| {
            if (s.library_width - r[2]).abs() > 0.5 {
                s.library_width = r[2];
            }
        });
    for (name, cards) in groups {
        body = body.child(
            text(name.clone())
                .font(TextStyleBold::GROUP)
                .color(theme::c(theme::T2))
                .role("heading")
                .level(2)
                .mt(6.0),
        );
        let mut grid = col().w_full().gap(26.0);
        let mut cards = cards.into_iter().peekable();
        while cards.peek().is_some() {
            let mut line = row().w_full().gap(16.0).items_start();
            for _ in 0..columns {
                line = line.child(match cards.next() {
                    Some(card) => card,
                    None => el().grow(1.0).min_w(0.0),
                });
            }
            grid = grid.child(line);
        }
        body = body.child(grid);
    }
    if count == 0 {
        body = body.child(empty_library(s));
    }
    page()
        .child(header)
        .child(
            row()
                .gap(12.0)
                .child(theme::small(format!(
                    "{}–{} of {total} matches",
                    if total == 0 { 0 } else { offset + 1 },
                    offset + count
                )))
                .child(
                    Btn::new("library-previous", "Previous")
                        .disabled(offset == 0)
                        .on(|s, _| {
                            s.library_page = library_offset(s) / LIBRARY_PAGE_SIZE;
                            s.library_page = s.library_page.saturating_sub(1);
                        })
                        .build(),
                )
                .child(
                    Btn::new("library-next", "Next")
                        .disabled(offset + count >= total)
                        .on(|s, _| s.library_page = library_offset(s) / LIBRARY_PAGE_SIZE + 1)
                        .build(),
                )
                .child(
                    Btn::new("library-trash", "Trash")
                        .on(|s, _| {
                            s.load_trash();
                            s.trash_open = true;
                        })
                        .build(),
                ),
        )
        .child(scroll(body))
}

struct TextStyleBold;
impl TextStyleBold {
    const GROUP: rune::TextStyle = rune::TextStyle::new(12.0, 600.0).lh(16.0);
}

fn empty_library(s: &State) -> El {
    let (title, copy) = if s.files.is_empty() {
        (
            "No captures yet",
            "Captured and imported media appear here. Choose the tools you use in Settings > Features.",
        )
    } else {
        ("Nothing matches", "Try another filter or search.")
    };
    col()
        .w_full()
        .py(64.0)
        .gap(8.0)
        .items_center()
        .child(theme::glyph(&icons::LIBRARY, 28.0, theme::c(theme::T3)))
        .child(theme::h2(title))
        .child(theme::small(copy))
}

fn clip_card(s: &State, i: usize, path: &std::path::Path, title: String, stamp: String) -> El {
    let open = path.to_path_buf();
    let favorite = path.to_path_buf();
    let remove = path.to_path_buf();
    let image = recordings::is_image(path);
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_uppercase();
    let starred = s.preferences.favorites.contains(path);
    let selected = s.editor.path.as_deref() == Some(path);
    col()
        .key(format!("capture-{i}"))
        .grow(1.0)
        .min_w(0.0)
        .gap(9.0)
        .child(
            col()
                .key("open")
                .role("button")
                .label(format!("Open {title}"))
                .focusable(true)
                .w_full()
                .on_press(move |s: &mut State| s.open(open.clone()))
                .child(match s.thumbs.get(path) {
                    Some(thumb) => theme::thumb_tile(
                        &thumb.pixels,
                        thumb.duration.map(clock).as_deref(),
                        selected,
                    ),
                    None => theme::tile(
                        if image { &icons::IMAGE } else { &icons::FILM },
                        &extension,
                        selected,
                    ),
                }),
        )
        .child(
            row()
                .w_full()
                .gap(6.0)
                .center_y()
                .child(
                    text(title)
                        .font(TextStyle500::TITLE)
                        .color(theme::c(theme::T1))
                        .single_line()
                        .truncate()
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(
                    text(stamp)
                        .font(TextStyle500::STAMP)
                        .color(theme::c(theme::T2))
                        .single_line(),
                )
                .child(
                    Btn::icon_only(
                        format!("favorite-{i}"),
                        if starred { "Unfavorite" } else { "Favorite" },
                        if starred {
                            &icons::STAR_FILLED
                        } else {
                            &icons::STAR
                        },
                    )
                    .h(24.0)
                    .checked(starred)
                    .on(move |s, _| {
                        if !s.preferences.favorites.remove(&favorite) {
                            s.preferences.favorites.insert(favorite.clone());
                        }
                        s.save_preferences();
                    })
                    .build(),
                )
                .child(
                    Btn::icon_only(format!("trash-{i}"), "Move to trash", &icons::TRASH)
                        .h(24.0)
                        .on(move |s, _| {
                            let path = remove.clone();
                            // Close/checkpoint the selected document before moving its recovery.
                            if s.editor.path.as_ref() == Some(&path) && !s.editor.close() {
                                s.page = Page::Editor;
                                return;
                            }
                            let root = s.preferences.folder.clone();
                            s.job(move || {
                                recordings::trash(&[path], &root)
                                    .map(JobResult::Trash)
                                    .map_err(|e| format!("{e:#}"))
                            });
                        })
                        .build(),
                ),
        )
}

struct TextStyle500;
impl TextStyle500 {
    const TITLE: rune::TextStyle = rune::TextStyle::new(14.0, 500.0).lh(20.0);
    const STAMP: rune::TextStyle = rune::TextStyle::new(12.0, 440.0)
        .lh(16.0)
        .features("'tnum' 1");
}

fn capture(s: &State) -> El {
    #[cfg(windows)]
    {
        crate::rune_capture::view(s)
    }
    #[cfg(not(windows))]
    {
        let _ = s;
        page()
            .child(theme::h1("Capture"))
            .child(theme::small("Native recording is available on Windows."))
    }
}

fn trash_view(s: &State) -> El {
    let mut body = col().gap(12.0).child(theme::small(format!(
        "{} recoverable files. Existing files are never replaced.",
        s.recovery.len()
    )));
    for (i, (source, _)) in s.recovery.iter().enumerate() {
        body = body.child(
            row()
                .gap(12.0)
                .child(theme::small(source.display().to_string()).grow(1.0))
                .child(
                    Btn::new(format!("restore-{i}"), "Restore")
                        .on(move |s, _| {
                            let Some(pair) = s.recovery.get(i).cloned() else {
                                return;
                            };
                            s.job(move || {
                                let mut moves = vec![pair];
                                recordings::restore(&mut moves).map_err(|e| {
                                    format!("Restore failed; file remains in Trash: {e:#}")
                                })?;
                                Ok(JobResult::Restored(moves, "Restored capture.".into()))
                            });
                        })
                        .build(),
                ),
        );
    }
    page()
        .child(heading("Trash"))
        .child(
            Btn::new("trash-back", "Back to Library")
                .on(|s, _| s.trash_open = false)
                .build(),
        )
        .child(scroll(body))
}

#[cfg(test)]
mod rune_tests {
    use super::*;
    use rune::{Env, Runtime};
    fn runtime() -> Runtime<State> {
        let mut rt = Runtime::new(
            State::with_headless(true),
            view,
            (1200.0, 800.0),
            Env::new(true, 1.0),
        );
        rt.frame(0.0);
        rt
    }
    #[test]
    fn feature_combinations_keep_core_pages_and_imported_media() {
        let mut state = State::with_headless(true);
        let imported = PathBuf::from("shadowplay.mp4");
        state.preferences.imported_files.insert(imported.clone());
        state.save_preferences();
        for f in Feature::ALL { state.set_feature(f, false); }
        assert!(state.page_available(Page::Library));
        assert!(state.page_available(Page::Settings));
        for p in [Page::Capture, Page::Tools, Page::Disk, Page::Editor] { assert!(!state.page_available(p)); }
        state.go_disk();
        assert!(state.page == Page::Library);
        state.open(imported.clone());
        assert!(state.editor.path.is_none());
        assert!(!state.editor.feature_change_blocked());
        assert!(state.preferences.imported_files.contains(&imported));
        state.set_feature(Feature::StorageAnalysis, true);
        state.go_disk();
        assert!(state.page == Page::Disk);
        assert!(state.page_available(Page::Tools));
        assert!(!state.page_available(Page::Capture));
        state.set_feature(Feature::VideoEditing, true);
        assert!(state.page_available(Page::Editor));
        assert!(!state.allows(Feature::Recording));
        assert_eq!(state.preferences.hotkeys, crate::preferences::default_hotkeys());
    }
    #[test]
    fn disabled_features_disappear_from_navigation_and_remain_reachable_in_settings() {
        let mut rt = runtime();
        for f in Feature::ALL { rt.state.set_feature(f, false); }
        rt.frame(0.0);
        assert!(rt.find("nav-capture").is_none());
        assert!(rt.find("nav-tools").is_none());
        click(&mut rt, "nav-settings");
        click(&mut rt, "settings-tab-5");
        assert!(rt.find("feature-screenshots").is_some());
        click(&mut rt, "feature-screenshots");
        assert!(rt.state.allows(Feature::Screenshots));
        assert!(rt.find("nav-capture").is_some());
    }
    #[test]
    fn storage_analysis_works_without_the_utility_bundle() {
        let mut rt = runtime();
        for f in Feature::ALL { rt.state.preferences.features.set(f, false); }
        rt.state.preferences.features.set(Feature::StorageAnalysis, true);
        rt.frame(0.0);
        click(&mut rt, "nav-tools");
        assert!(rt.find("tools-disk").is_some());
        assert!(rt.find("tools-Color").is_none());
        click(&mut rt, "tools-disk");
        assert!(rt.state.page == Page::Disk);
        assert!(rt.find("disk-scan").is_some());
    }
    #[cfg(windows)]
    #[test]
    fn capture_view_retains_its_native_surface_draw() {
        let mut state = State::with_headless(true);
        state.page = Page::Capture;
        state.capture.cpu_fixture();
        let mut rt = Runtime::new(state, view, (1200.0, 800.0), Env::new(true, 1.0));
        rt.frame(0.0);
        assert_eq!(
            rt.surfaces.len(),
            1,
            "Capture must submit a Rune surface draw"
        );
        assert!(rt.surfaces[0].rect[2] > 0.0 && rt.surfaces[0].rect[3] > 0.0);
    }
    fn click(rt: &mut Runtime<State>, key: &str) {
        let (x, y) = rt
            .find(key)
            .unwrap_or_else(|| panic!("Missing clickable element: {key}"));
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..40 {
            rt.frame(1.0 / 60.0);
        }
    }
    #[test]
    fn all_primary_pages_are_reachable_with_pointer_input() {
        let mut rt = runtime();
        for (page, key) in [
            (Page::Capture, "nav-capture"),
            (Page::Tools, "nav-tools"),
            (Page::Disk, "tools-disk"),
            (Page::Settings, "nav-settings"),
            (Page::Library, "nav-library"),
        ] {
            click(&mut rt, key);
            assert!(rt.state.page == page, "{key} did not open its page");
        }
        click(&mut rt, "nav-status");
        assert!(
            rt.state.page == Page::Capture,
            "The capture light opens Capture"
        );
        assert!(
            rt.state.scanner.is_none(),
            "Headless UI must not scan personal captures"
        );
        assert!(
            !rt.state.has_tray(),
            "Headless UI must not register global OS hooks"
        );
    }
    #[test]
    fn tools_open_as_title_bar_tabs_and_close() {
        let mut rt = runtime();
        click(&mut rt, "nav-tools");
        click(&mut rt, "tools-Ocr");
        assert!(rt.state.tools.open_tool() == Some(toolkit::utilities::Tool::Ocr));
        click(&mut rt, "nav-library");
        assert!(rt.find("tab-tool").is_some(), "An open tool keeps its tab");
        click(&mut rt, "tab-tool");
        assert!(rt.state.page == Page::Tools);
        click(&mut rt, "tab-tool/close");
        assert!(rt.state.tools.open_tool().is_none());
    }
    #[test]
    fn search_palette_runs_page_commands() {
        let mut rt = runtime();
        click(&mut rt, "search");
        assert!(rt.state.palette.is_some());
        rt.key(KeyEvent {
            key: Key::Text("disk".into()),
            mods: Default::default(),
        });
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Default::default(),
        });
        for _ in 0..10 {
            rt.frame(1.0 / 60.0);
        }
        assert!(rt.state.palette.is_none());
        assert!(rt.state.page == Page::Disk);
    }
    #[test]
    fn search_edits_bind_to_the_owned_text_model() {
        let mut rt = runtime();
        click(&mut rt, "library-search");
        rt.key(KeyEvent {
            key: Key::Text("日本語 clip".into()),
            mods: Default::default(),
        });
        rt.frame(0.0);
        assert_eq!(rt.state.search.text, "日本語 clip");
    }
    #[test]
    fn settings_changes_do_not_write_user_preferences_in_headless_mode() {
        let mut rt = runtime();
        click(&mut rt, "nav-settings");
        click(&mut rt, "settings-tab-1");
        let before = rt.state.preferences.microphone;
        click(&mut rt, "microphone");
        assert_ne!(rt.state.preferences.microphone, before);
    }
    #[test]
    fn library_paging_reaches_filtered_old_files_and_clamps_page() {
        let mut state = State::with_headless(true);
        state.files = (0..701)
            .map(|i| PathBuf::from(format!("recording-{i}.mp4")))
            .collect();
        state.library_page = 6;
        assert_eq!(library_offset(&state), 600);
        state.search = TextEdit::new("recording-700");
        assert_eq!(library_matches(&state).len(), 1);
        assert_eq!(library_offset(&state), 0);
        assert_eq!(
            library_matches(&state)[0].1,
            &PathBuf::from("recording-700.mp4")
        );
    }
    #[test]
    fn notices_use_explicit_outcomes_and_retain_queued_errors() {
        let mut state = State::with_headless(true);
        state.notify(
            NoticeKind::Error,
            "Permission denied. Choose another folder.",
        );
        state.notify(NoticeKind::Success, "Saved");
        assert!(state.notice_kind() == NoticeKind::Error);
        assert_eq!(state.notice_queue.len(), 1);
        state.dismiss_notice();
        assert_eq!(state.notice, "Saved");
        assert!(state.notice_kind() == NoticeKind::Success);
        state.notice = "Disk is full".into();
        assert!(state.notice_kind() == NoticeKind::Error);
    }
    #[test]
    fn failed_preference_write_rolls_back_live_snapshot_and_retains_error() {
        let mut state = State::with_headless(true);
        let folder = state.preferences.folder.clone();
        let microphone = state.preferences.microphone;
        state.preferences.folder = PathBuf::from("another-folder");
        state.preferences.microphone = !microphone;
        state.preferences.features.set(Feature::Recording, false);
        assert!(!state.persist_preferences(|_| anyhow::bail!("Disk full")));
        assert_eq!(state.preferences.folder, folder);
        assert_eq!(state.preferences.microphone, microphone);
        assert!(state.preferences.features.selected(Feature::Recording));
        assert!(state.notice_kind() == NoticeKind::Error);
        state.notify(NoticeKind::Success, "Unrelated task finished");
        assert!(state.notice.contains("Disk full"));
        assert_eq!(state.notice_queue.len(), 1);
    }
}
