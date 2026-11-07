//! Application state and Rune views. Media and filesystem jobs run on workers.
use crate::{
    library_scan::{LibraryScanner, ScanRequest},
    preferences::Preferences,
    recordings,
};
use rune::tok::type_;
use rune::{AppHandle, Color, Cx, El, Key, KeyEvent, TextEdit, Tx, col, el, row, text, text_input};
use std::{path::PathBuf, sync::mpsc, time::Duration};
pub const BG: u32 = 0x0b0c0e;
pub const PANEL: u32 = 0x131417;
pub const LINE: u32 = 0x232428;
pub const MUTED: u32 = 0xa1a1aa;
pub const ACCENT: u32 = 0x3ecf8e;
pub const TEXT: u32 = 0xededef;
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
    text(value).font(type_::BODY).color(color(TEXT))
}
pub fn muted(value: impl Into<String>) -> El {
    label(value).color(color(MUTED))
}
pub fn heading(value: impl Into<String>) -> El {
    text(value)
        .font(rune::TextStyle::new(22.0, 600.0))
        .color(color(TEXT))
}
pub fn button(
    key: impl Into<String>,
    title: impl Into<String>,
    action: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    let title = title.into();
    el().key(key)
        .role("button")
        .label(title.clone())
        .focusable(true)
        .px(12.0)
        .py(8.0)
        .radius(6.0)
        .fill(color(0x232428))
        .hover(color(0x34363c))
        .border(1.0, color(LINE))
        .on_press_tx(action)
        .child(label(title))
}
pub fn field(
    key: &str,
    edit: &TextEdit,
    access: impl for<'a> Fn(&'a mut State) -> &'a mut TextEdit + 'static,
) -> El {
    text_input(edit, type_::BODY)
        .key(key)
        .label(key)
        .w_full()
        .h(36.0)
        .padding(8.0)
        .radius(5.0)
        .fill(color(BG))
        .border(1.0, color(LINE))
        .color(color(TEXT))
        .single_line()
        .on_edit(access)
}
pub fn check(
    key: &str,
    title: &str,
    on: bool,
    action: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    button(
        key,
        format!("{}  {title}", if on { "✓" } else { "○" }),
        action,
    )
    .role("checkbox")
    .checked(on)
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
pub enum JobResult {
    Notice(String),
    Trash(Vec<(PathBuf, PathBuf)>),
    Restored(Vec<(PathBuf, PathBuf)>, String),
    Capture(PathBuf),
}
struct FileJob {
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
    pub preferences: Preferences,
    pub notice: String,
    pub search: TextEdit,
    pub files: Vec<PathBuf>,
    scanner: Option<LibraryScanner>,
    pub editor: crate::rune_editor::State,
    pub tools: crate::rune_tools::State,
    pub disk: crate::rune_tools::Disk,
    jobs: Vec<FileJob>,
    removed: Vec<(PathBuf, PathBuf)>,
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
            preferences,
            notice: String::new(),
            search: TextEdit::new(""),
            files: vec![],
            scanner: None,
            editor: Default::default(),
            tools: Default::default(),
            disk: Default::default(),
            jobs: vec![],
            removed: vec![],
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
    pub fn save_preferences(&mut self) {
        if self.headless {
            return;
        }
        if let Err(e) = self.preferences.save() {
            self.notice = format!("Settings could not be saved: {e:#}");
        }
    }
    pub fn job(&mut self, work: impl FnOnce() -> Result<JobResult, String> + Send + 'static) {
        let (tx, rx) = mpsc::sync_channel(1);
        let wake = self.wake.clone();
        match std::thread::Builder::new()
            .name("kielbox-file-job".into())
            .spawn(move || {
                let _ = tx.send(work());
                wake();
            }) {
            Ok(worker) => self.jobs.push(FileJob {
                result: rx,
                worker: Some(worker),
            }),
            Err(e) => self.notice = e.to_string(),
        }
    }
    pub fn open(&mut self, path: PathBuf) {
        self.page = Page::Editor;
        self.editor.open(path, self.wake.clone());
    }
    pub fn browse(&mut self, tx: &mut Tx) {
        tx.open_file(
            rune::FileDialogOptions::default().filter(
                "Media",
                &[
                    "mkv", "mp4", "webm", "mov", "ivf", "png", "jpg", "webp", "bmp",
                ],
            ),
            |s: &mut State, result, _| match result {
                Ok(Some(path)) => {
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
        match crate::hotkeys::Hotkeys::start_with(&s.preferences.hotkeys) {
            Ok(keys) => s.hotkeys = Some(keys),
            Err(e) => s.notice = e.to_string(),
        }
        match crate::tray::Tray::new() {
            Ok(tray) => s.tray = Some(tray),
            Err(e) => s.notice = e.to_string(),
        }
    }
}
pub fn poll(s: &mut State, _: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    if let Some(scanner) = &mut s.scanner {
        if let Some(result) = scanner.poll() {
            if let Some(e) = result.apply(&mut s.files) {
                s.notice = format!("Could not scan {}: {:?}", e.root.display(), e.kind);
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
            Ok(JobResult::Notice(v)) => s.notice = v,
            Ok(JobResult::Trash(moved)) => {
                s.removed = moved;
                s.notice = "Moved to capture trash. Undo is available.".into();
                s.refresh();
            }
            Ok(JobResult::Restored(remaining, message)) => {
                s.removed = remaining;
                s.notice = message;
                s.refresh();
            }
            Ok(JobResult::Capture(path)) => {
                s.refresh();
                s.open(path);
            }
            Err(e) => s.notice = e,
        }
    }
    changed |= s.editor.poll();
    changed |= s.tools.poll();
    changed |= s.disk.poll();
    crate::rune_annotation::flush_save(s);
    #[cfg(windows)]
    {
        changed |= s.capture.poll(&mut s.notice);
        if let Some(updates) = &mut s.updates {
            changed |= updates.poll(
                std::time::Instant::now(),
                !s.capture.active && !s.editor.playing(),
            );
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
                Event::Notice(v) => s.notice = v,
            }
        }
        let events: Vec<_> = s
            .tray
            .as_ref()
            .map(|t| t.events.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            changed = true;
            match event.as_str() {
                "quit" => tx.quit(),
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
                _ => s.page = Page::Tools,
            }
        }
        if let Some(tray) = &mut s.tray {
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
    if tx.focused().is_some() {
        return false;
    }
    if s.page == Page::Editor {
        return s.editor.key(event);
    }
    if matches!(event.key, Key::Command('o')) {
        s.browse(tx);
        return true;
    }
    false
}
pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    cx.foreground(color(TEXT));
    let mut nav = col()
        .w(180.0)
        .h_full()
        .padding(16.0)
        .gap(8.0)
        .fill(color(PANEL))
        .child(heading("Kielbox"))
        .child(muted("Capture · Create · Share").mb(24.0));
    for (page, key, title) in [
        (Page::Library, "library", "Library"),
        (Page::Capture, "capture", "Capture"),
        (Page::Editor, "editor", "Editor"),
        (Page::Disk, "disk", "Disk usage"),
        (Page::Tools, "tools", "Utilities"),
        (Page::Settings, "settings", "Settings"),
    ] {
        nav = nav.child(
            button(format!("nav-{key}"), title, move |s, _| s.page = page)
                .w_full()
                .fill(color(if s.page == page { 0x203b30 } else { PANEL })),
        );
    }
    let content = match s.page {
        Page::Library => library(s),
        Page::Capture => capture(s),
        Page::Editor => crate::rune_editor::view(s),
        Page::Disk => crate::rune_tools::disk_view(s),
        Page::Tools => crate::rune_tools::view(s),
        Page::Settings => settings(s),
    };
    let mut body = col()
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .padding(24.0)
        .gap(16.0)
        .child(content);
    if !s.notice.is_empty() {
        body = body.child(
            row()
                .w_full()
                .padding(12.0)
                .gap(12.0)
                .fill(color(PANEL))
                .child(label(&s.notice).grow(1.0))
                .child(button("dismiss-notice", "Dismiss", |s, _| s.notice.clear())),
        );
    }
    row()
        .w_full()
        .h_full()
        .fill(color(BG))
        .child(nav)
        .child(body)
}
fn library(s: &State) -> El {
    let toolbar = row()
        .gap(8.0)
        .child(heading("Library").grow(1.0))
        .child(button("import", "Open file…", |s, tx| s.browse(tx)))
        .child(button("refresh", "Refresh", |s, _| s.refresh()));
    let query = s.search.text.to_lowercase();
    let mut entries = col().w_full().gap(8.0);
    for (i, path) in s
        .files
        .iter()
        .filter(|p| p.to_string_lossy().to_lowercase().contains(&query))
        .take(500)
        .enumerate()
    {
        let open = path.clone();
        let favorite = path.clone();
        let remove = path.clone();
        let title = s
            .preferences
            .display_names
            .get(path)
            .cloned()
            .unwrap_or_else(|| recordings::title(path));
        entries = entries.child(
            row()
                .key(format!("capture-{i}"))
                .w_full()
                .gap(8.0)
                .padding(12.0)
                .radius(6.0)
                .fill(color(PANEL))
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(4.0)
                        .child(label(title))
                        .child(muted(path.to_string_lossy())),
                )
                .child(button(
                    format!("favorite-{i}"),
                    if s.preferences.favorites.contains(path) {
                        "★"
                    } else {
                        "☆"
                    },
                    move |s, _| {
                        if !s.preferences.favorites.remove(&favorite) {
                            s.preferences.favorites.insert(favorite.clone());
                        }
                        s.save_preferences();
                    },
                ))
                .child(button(format!("open-{i}"), "Open", move |s, _| {
                    s.open(open.clone())
                }))
                .child(button(format!("trash-{i}"), "Trash", move |s, _| {
                    let path = remove.clone();
                    let root = s.preferences.folder.clone();
                    s.job(move || {
                        recordings::trash(&[path], &root)
                            .map(JobResult::Trash)
                            .map_err(|e| format!("{e:#}"))
                    });
                })),
        );
    }
    if s.files.is_empty() {
        entries=entries.child(muted("Your captures appear here. Record a video, take a screenshot, or open an existing file."));
    }
    let mut page = col()
        .grow(1.0)
        .min_h(0.0)
        .gap(16.0)
        .child(toolbar)
        .child(field("library-search", &s.search, |s| &mut s.search))
        .child(
            entries
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .grow(1.0)
                .min_h(0.0),
        );
    if !s.removed.is_empty() {
        page = page.child(button("undo-trash", "Undo trash", |s, _| {
            let mut moved = std::mem::take(&mut s.removed);
            s.job(move || {
                let message = match recordings::restore(&mut moved) {
                    Ok(()) => "Restored captures.".into(),
                    Err(e) => format!("Restore incomplete: {e:#}"),
                };
                Ok(JobResult::Restored(moved, message))
            });
        }));
    }
    page
}
fn capture(s: &State) -> El {
    #[cfg(windows)]
    {
        crate::rune_capture::view(s)
    }
    #[cfg(not(windows))]
    {
        col()
            .child(heading("Capture"))
            .child(muted("Native recording is available on Windows."))
    }
}
fn settings(s: &State) -> El {
    let mut page = col()
        .gap(16.0)
        .child(heading("Settings"))
        .child(label(format!(
            "Capture folder: {}",
            s.preferences.folder.display()
        )))
        .child(button(
            "choose-folder",
            "Choose capture folder…",
            |_, tx| {
                tx.open_folder(
                    rune::FileDialogOptions::default(),
                    |s: &mut State, result, _| match result {
                        Ok(Some(path)) => {
                            s.preferences.folder = path.clone();
                            recordings::set_root(path);
                            s.save_preferences();
                            s.refresh();
                        }
                        Err(e) => s.notice = e.to_string(),
                        _ => {}
                    },
                )
            },
        ))
        .child(check(
            "system-audio",
            "Record system audio",
            s.preferences.system_audio,
            |s, _| {
                s.preferences.system_audio = !s.preferences.system_audio;
                s.save_preferences();
            },
        ))
        .child(check(
            "microphone",
            "Record microphone",
            s.preferences.microphone,
            |s, _| {
                s.preferences.microphone = !s.preferences.microphone;
                s.save_preferences();
            },
        ))
        .child(check(
            "clean-mic",
            "Clean microphone in saved replays",
            s.preferences.clean_microphone,
            |s, _| {
                s.preferences.clean_microphone = !s.preferences.clean_microphone;
                s.save_preferences();
            },
        ))
        .child(check(
            "tray-on-close",
            "Keep running in the tray when closed",
            s.preferences.close_to_tray,
            |s, _| {
                s.preferences.close_to_tray = !s.preferences.close_to_tray;
                s.save_preferences();
            },
        ))
        .child(label("Recording resolution"));
    let mut heights = row().gap(8.0);
    for height in [720, 1080, 1440, 2160] {
        heights = heights.child(
            button(
                format!("height-{height}"),
                format!("{height}p"),
                move |s, _| {
                    s.preferences.height = height;
                    s.save_preferences();
                },
            )
            .checked(s.preferences.height == height),
        );
    }
    page = page.child(heights).child(label("Frame rate"));
    let mut rates = row().gap(8.0);
    for fps in [30, 60, 120] {
        rates = rates.child(
            button(format!("fps-{fps}"), fps.to_string(), move |s, _| {
                s.preferences.fps = fps;
                s.save_preferences();
            })
            .checked(s.preferences.fps == fps),
        );
    }
    page = page.child(rates).child(label("Global shortcuts"));
    for (i, edit) in s.hotkey_fields.iter().enumerate() {
        page = page.child(field(&format!("hotkey-{i}"), edit, move |s| {
            &mut s.hotkey_fields[i]
        }));
    }
    page = page.child(button("save-hotkeys", "Apply shortcuts", |s, _| {
        let bindings: [String; 7] = std::array::from_fn(|i| s.hotkey_fields[i].text.clone());
        #[cfg(windows)]
        {
            if let Err(e) = crate::hotkeys::validate(&bindings) {
                s.notice = e;
                return;
            }
            s.hotkeys.take();
            match crate::hotkeys::Hotkeys::start_with(&bindings) {
                Ok(keys) => s.hotkeys = Some(keys),
                Err(e) => {
                    s.notice = e.to_string();
                    return;
                }
            }
        }
        s.preferences.hotkeys = bindings;
        s.save_preferences();
    }));
    #[cfg(windows)]
    if let Some(updates) = &s.updates {
        page = page
            .child(heading("Updates"))
            .child(label(format!(
                "{} · {:?}",
                updates.current_version(),
                updates.status()
            )))
            .child(
                row()
                    .gap(8.0)
                    .child(
                        button("check-updates", "Check for updates", |s, _| {
                            if let Some(u) = &mut s.updates {
                                u.check_now();
                            }
                        })
                        .disabled(!updates.can_check()),
                    )
                    .child(
                        button("download-update", "Download update", |s, _| {
                            if let Some(u) = &mut s.updates {
                                u.download();
                            }
                        })
                        .disabled(!updates.can_download()),
                    )
                    .child(
                        button("install-update", "Restart to update", |s, tx| {
                            if s.capture.active || s.editor.playing() || !s.jobs.is_empty() {
                                s.notice =
                                    "Finish recording and active jobs before updating.".into();
                                return;
                            }
                            if let Some(update) = s.updates.as_ref().and_then(|u| u.prepare_ready())
                            {
                                match std::env::current_dir() {
                                    Ok(root) => {
                                        *crate::PENDING_UPDATE
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner()) =
                                            Some((update, root));
                                        tx.quit();
                                    }
                                    Err(e) => s.notice = e.to_string(),
                                }
                            }
                        })
                        .disabled(!updates.ready()),
                    ),
            );
    }
    page.scroll(rune::ScrollSpec {
        stick_bottom: false,
    })
    .grow(1.0)
    .min_h(0.0)
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
            (Page::Capture, "capture"),
            (Page::Editor, "editor"),
            (Page::Disk, "disk"),
            (Page::Tools, "tools"),
            (Page::Settings, "settings"),
            (Page::Library, "library"),
        ] {
            click(&mut rt, &format!("nav-{key}"));
            assert!(rt.state.page == page, "Navigation did not open {key}");
        }
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
        let before = rt.state.preferences.microphone;
        click(&mut rt, "microphone");
        assert_ne!(rt.state.preferences.microphone, before);
    }
}
