use crate::features::Feature;
use crate::{
    rune_theme::{self as theme, Btn, c, icons},
    rune_ui::{self as ui, JobResult, State as AppState},
};
use rune::{El, NativeSurface, TextEdit, col, el, row};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
};
use toolkit::{core::workflow::CancelToken, utilities as util};
pub struct State {
    tool: util::Tool,
    generation: u64,
    files: Vec<PathBuf>,
    text: TextEdit,
    a: TextEdit,
    b: TextEdit,
    color: TextEdit,
    mode: usize,
    /// A tool page is open (its tab shows in the title bar).
    pub open: bool,
    pub search: TextEdit,
    pub message: String,
    output: Option<PathBuf>,
    result: Option<util::Output>,
    job: Option<mpsc::Receiver<Result<util::Output, String>>>,
    worker: Option<std::thread::JoinHandle<()>>,
    retired_workers: Vec<std::thread::JoinHandle<()>>,
    save_failed: bool,
    cancel: CancelToken,
    surface: NativeSurface,
}
impl Default for State {
    fn default() -> Self {
        Self {
            tool: util::Tool::Color,
            generation: 0,
            files: vec![],
            text: TextEdit::new("#3ecf8e"),
            a: TextEdit::new("1024"),
            b: TextEdit::new("768"),
            color: TextEdit::new("#131417"),
            mode: 0,
            open: false,
            search: TextEdit::new(""),
            message: String::new(),
            output: None,
            result: None,
            job: None,
            worker: None,
            retired_workers: Vec::new(),
            save_failed: false,
            cancel: CancelToken::new(),
            surface: NativeSurface::new(),
        }
    }
}
impl State {
    fn run(&mut self, wake: rune::Waker) {
        self.run_frame(wake, None);
    }
    fn run_frame(&mut self, wake: rune::Waker, frame: Option<toolkit::types::Frame>) {
        if self.job.is_some() {
            return;
        }
        self.result = None;
        self.save_failed = false;
        self.generation = self.generation.wrapping_add(1);
        let parse = |s: &TextEdit| {
            s.text
                .trim()
                .parse::<u32>()
                .map_err(|_| "Dimensions must be positive whole numbers.")
        };
        let (a, b) = match (
            if parameter_labels(self.tool, self.mode).0.is_some() {
                parse(&self.a)
            } else {
                Ok(0)
            },
            if parameter_labels(self.tool, self.mode).1.is_some() {
                parse(&self.b)
            } else {
                Ok(0)
            },
        ) {
            (Ok(a), Ok(b)) => (a, b),
            _ => {
                self.message = "Enter nonnegative whole numbers in the labelled options.".into();
                return;
            }
        };
        let request = util::Request {
            tool: self.tool,
            files: self.files.clone(),
            text: self.text.text.clone(),
            mode: self.mode,
            a,
            b,
            color: self.color.text.clone(),
            output: self.output.clone(),
        };
        let cancel = CancelToken::new();
        self.cancel = cancel.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("kielbox-utility".into())
            .spawn(move || {
                let result = util::with_pool(|| util::run_with_frame(request, frame, &cancel))
                    .and_then(|v| v)
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
                wake();
            }) {
            Ok(worker) => {
                self.worker = Some(worker);
                self.job = Some(rx);
                self.message = "Working…".into();
            }
            Err(e) => self.message = e.to_string(),
        }
    }
    fn save_result(&mut self, path: PathBuf, wake: rune::Waker) {
        if self.job.is_some() {
            return;
        }
        let Some(output) = &self.result else {
            return;
        };
        let mut output = util::Output {
            text: output.text.clone(),
            image: output.image.clone(),
            preview: None,
            files: output.files.clone(),
        };
        // Keep the generated result available for retry if publication fails.
        self.save_failed = false;
        self.generation = self.generation.wrapping_add(1);
        let cancel = CancelToken::new();
        self.cancel = cancel.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("utility-save".into())
            .spawn(move || {
                let result = (|| {
                    if let Some(frame) = &output.image {
                        util::save_image(&path, frame)?;
                    } else {
                        util::save_new(&path, output.text.as_bytes())?;
                    }
                    output.files.push(path);
                    Ok::<_, anyhow::Error>(output)
                })()
                .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
                wake();
            }) {
            Ok(worker) => {
                self.worker = Some(worker);
                self.job = Some(rx);
                self.message = "Saving…".into();
            }
            Err(e) => {
                self.save_failed = true;
                self.message = e.to_string();
            }
        }
    }
    fn reset(&mut self, tool: util::Tool) {
        self.cancel.cancel();
        self.job = None;
        // Retain ownership without blocking navigation on a cancelled OCR call.
        if let Some(worker) = self.worker.take() {
            self.retired_workers.push(worker);
        }
        self.save_failed = false;
        self.generation = self.generation.wrapping_add(1);
        self.tool = tool;
        self.files.clear();
        self.output = None;
        self.result = None;
        self.message.clear();
        self.mode = 0;
        self.text = TextEdit::new(if tool == util::Tool::Color {
            "#3ecf8e"
        } else {
            ""
        });
        let (a, b) = match tool {
            util::Tool::Beautify => ("32", "16"),
            util::Tool::Split => ("2", "2"),
            util::Tool::Thumbnail => ("1024", "768"),
            util::Tool::VideoThumbnail => ("0", "1024"),
            util::Tool::VideoConvert => ("15", "1024"),
            util::Tool::Effects => ("10", "0"),
            _ => ("0", "0"),
        };
        self.a = TextEdit::new(a);
        self.b = TextEdit::new(b);
    }
    /// Capture integrations must retain this ticket and return it with the selected pixels.
    pub fn capture_ticket(&self) -> (u64, util::Tool) {
        (self.generation, self.tool)
    }
    pub fn accept_capture(
        &mut self,
        ticket: (u64, util::Tool),
        frame: toolkit::types::Frame,
        wake: rune::Waker,
    ) -> bool {
        if self.capture_ticket() != ticket || !self.open || self.job.is_some() {
            return false;
        }
        self.run_frame(wake, Some(frame));
        true
    }
    fn capture_region(&mut self, wake: rune::Waker) {
        #[cfg(windows)]
        {
            if self.job.is_some() {
                return;
            }
            self.result = None;
            self.generation = self.generation.wrapping_add(1);
            let tool = self.tool;
            let cancel = CancelToken::new();
            self.cancel = cancel.clone();
            let (tx, rx) = mpsc::sync_channel(1);
            match std::thread::Builder::new()
                .name("utility-region".into())
                .spawn(move || {
                    let result = (|| -> Result<util::Output, String> {
                        let capturer = toolkit::services::ScreenCapturer::detect(
                            None,
                            toolkit::services::LastRegionStore::none(),
                        );
                        let frame = capturer
                            .with(|source| source.capture_desktop(&Default::default()))
                            .map_err(|e| e.to_string())?;
                        let selection =
                            screenshot_overlay::select(screenshot_overlay::OverlayInput {
                                desktop: frame.clone(),
                                monitors: vec![],
                                windows: vec![],
                                options: Default::default(),
                            })
                            .map_err(|e| e.to_string())?;
                        let screenshot_overlay::OverlayOutcome::Selected(selection) = selection
                        else {
                            return Err("Capture cancelled.".into());
                        };
                        let frame = frame
                            .crop_desktop(selection.rect)
                            .map_err(|e| e.to_string())?;
                        let request = util::Request {
                            tool,
                            files: vec![],
                            text: String::new(),
                            mode: 0,
                            a: 0,
                            b: 0,
                            color: String::new(),
                            output: None,
                        };
                        util::with_pool(|| util::run_with_frame(request, Some(frame), &cancel))
                            .and_then(|r| r)
                            .map_err(|e| format!("{e:#}"))
                    })();
                    let _ = tx.send(result);
                    wake();
                }) {
                Ok(worker) => {
                    self.worker = Some(worker);
                    self.job = Some(rx);
                    self.message = "Select a region…".into();
                }
                Err(e) => self.message = e.to_string(),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = wake;
            self.message = "Screen capture is available on Windows.".into();
        }
    }
    pub fn poll(&mut self) -> bool {
        let mut i = 0;
        while i < self.retired_workers.len() {
            if self.retired_workers[i].is_finished() {
                let _ = self.retired_workers.swap_remove(i).join();
            } else {
                i += 1;
            }
        }
        let Some(rx) = &self.job else {
            return false;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.job = None;
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
                match result {
                    Ok(mut output) => {
                        self.message = output.text.clone();
                        if let Some((w, h, bytes)) = output.preview.take() {
                            if let Err(e) = self.surface.push_bytes(
                                rune::native_surface::ByteFrame {
                                    width: w,
                                    height: h,
                                    format: rune::native_surface::Format::Bgra,
                                    color: Default::default(),
                                    planes: [bytes, vec![]],
                                },
                                0,
                            ) {
                                self.message = e;
                            }
                        }
                        self.result = Some(output);
                    }
                    Err(e) => {
                        self.save_failed = self.result.is_some();
                        self.message = e;
                    }
                }
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                self.save_failed = self.result.is_some();
                self.message = "Utility worker stopped unexpectedly.".into();
                true
            }
            _ => false,
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.job = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        for worker in self.retired_workers.drain(..) {
            let _ = worker.join();
        }
    }
}
#[derive(Default)]
pub struct Disk {
    pub tree: Option<disk::disk_usage::DiskUsage>,
    root: usize,
    page: usize,
    job: Option<mpsc::Receiver<Result<disk::disk_usage::DiskUsage, String>>>,
    progress: Arc<Mutex<Option<disk::disk_usage::ScanProgress>>>,
    cancel: CancelToken,
    message: String,
}
impl Disk {
    fn start(&mut self, path: PathBuf, wake: rune::Waker) {
        self.cancel.cancel();
        self.tree = None;
        self.root = 0;
        self.page = 0;
        self.cancel = CancelToken::new();
        let cancel = self.cancel.clone();
        let progress = Arc::new(Mutex::new(None));
        self.progress = progress.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("kielbox-disk-scan".into())
            .spawn(move || {
                let result = disk::disk_usage::scan(path, &cancel, |p| {
                    *progress.lock().unwrap() = Some(p);
                    wake();
                })
                .map_err(|e| e.to_string());
                let _ = tx.send(result);
                wake();
            }) {
            Ok(_) => {
                self.job = Some(rx);
                self.message = "Scanning…".into();
            }
            Err(e) => self.message = e.to_string(),
        }
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(p) = self.progress.lock().unwrap().take() {
            self.message = format!("{} · {} files · {}", p.stage, p.files, bytes(p.bytes));
            changed = true;
        }
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok(result) => {
                    self.job = None;
                    changed = true;
                    match result {
                        Ok(tree) => {
                            self.message = format!(
                                "{} files · {} · {} errors{}",
                                tree.nodes[0].files,
                                bytes(tree.nodes[0].bytes),
                                tree.nodes[0].errors,
                                if tree.cancelled { " · Cancelled" } else { "" }
                            );
                            self.tree = Some(tree);
                            self.root = 0;
                            self.page = 0;
                        }
                        Err(e) => self.message = e,
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.job = None;
                    self.message = "Disk worker stopped unexpectedly.".into();
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }
}
impl Drop for Disk {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
fn bytes(v: u64) -> String {
    if v >= 1 << 30 {
        format!("{:.2} GiB", v as f64 / (1u64 << 30) as f64)
    } else if v >= 1 << 20 {
        format!("{:.1} MiB", v as f64 / (1u64 << 20) as f64)
    } else if v >= 1024 {
        format!("{:.1} KiB", v as f64 / 1024.0)
    } else {
        format!("{v} B")
    }
}
/// The task-first name a tool goes by in lists and tabs.
pub fn tool_title(tool: util::Tool) -> &'static str {
    use util::Tool::*;
    match tool {
        Color => "Color picker",
        Viewer => "Image viewer",
        Beautify => "Beautify",
        Effects => "Effects",
        Combine => "Combine",
        Split => "Split",
        Thumbnail => "Thumbnails",
        Convert => "Convert",
        VideoConvert => "Convert video",
        VideoThumbnail => "Video thumbnails",
        Ocr => "Text from image",
        QrEncode => "Make QR code",
        QrDecode => "Read QR code",
        Hash => "File hash",
        Index => "Folder index",
        Clipboard => "Clipboard viewer",
        Windows => "Window inspector",
    }
}
pub fn tool_icon(tool: util::Tool) -> &'static rune::Icon {
    use util::Tool::*;
    match tool {
        Color => &icons::PICKER,
        Viewer => &icons::IMAGE,
        Beautify => &icons::BEAUTIFY,
        Effects => &icons::EFFECTS,
        Combine => &icons::COMBINE,
        Split => &icons::GRID_SPLIT,
        Thumbnail => &icons::THUMBNAILS,
        Convert => &icons::CONVERT,
        VideoConvert => &icons::CONVERT,
        VideoThumbnail => &icons::FILM,
        Ocr => &icons::TEXT_SCAN,
        QrEncode => &icons::QR,
        QrDecode => &icons::QR,
        Hash => &icons::HASH,
        Index => &icons::INDEX,
        Clipboard => &icons::CLIPBOARD,
        Windows => &icons::WINDOW,
    }
}
/// Tools grouped by the task they serve. `None` is Disk usage, which has its own page.
const GROUPS: [(&str, &[Option<util::Tool>]); 5] = [
    (
        "Screen",
        &[Some(util::Tool::Color), Some(util::Tool::Windows)],
    ),
    (
        "Read",
        &[
            Some(util::Tool::Ocr),
            Some(util::Tool::QrDecode),
            Some(util::Tool::Clipboard),
        ],
    ),
    (
        "Images",
        &[
            Some(util::Tool::Viewer),
            Some(util::Tool::Beautify),
            Some(util::Tool::Effects),
            Some(util::Tool::Combine),
            Some(util::Tool::Split),
            Some(util::Tool::Convert),
            Some(util::Tool::Thumbnail),
        ],
    ),
    (
        "Video",
        &[
            Some(util::Tool::VideoConvert),
            Some(util::Tool::VideoThumbnail),
        ],
    ),
    (
        "Files",
        &[
            Some(util::Tool::QrEncode),
            Some(util::Tool::Hash),
            Some(util::Tool::Index),
            None,
        ],
    ),
];

pub fn open_tool(s: &mut AppState, tool: util::Tool) {
    if !s.require(Feature::Utilities) { return; }
    if s.tools.tool != tool || !s.tools.open {
        s.tools.reset(tool);
    }
    s.tools.tool = tool;
    s.tools.open = true;
    s.page = ui::Page::Tools;
}
impl State {
    pub fn busy(&self) -> bool { self.job.is_some() }
    pub fn open_tool(&self) -> Option<util::Tool> {
        self.open.then_some(self.tool)
    }
    pub fn close(&mut self) {
        self.reset(self.tool);
        self.open = false;
    }
}

pub fn view(s: &AppState) -> El {
    if s.tools.open {
        return tool_page(s);
    }
    let query = s.tools.search.text.trim().to_lowercase();
    let mut columns = row().w_full().flex_wrap().gaps(8.0, 32.0).items_start();
    // Four columns, short groups stacked: Screen and Files share the first.
    let mut stacks: Vec<El> = (0..4).map(|_| col().w(220.0)).collect();
    for (index, (group, tools)) in GROUPS.into_iter().enumerate() {
        let mut section = col()
            .w(220.0)
            .gap(0.0)
            .child(theme::section_label(group).px(10.0).pb(6.0));
        let mut any = false;
        for tool in tools {
            if !s.allows(if tool.is_some() { Feature::Utilities } else { Feature::StorageAnalysis }) { continue; }
            let (key, i, title) = match tool {
                Some(t) => (format!("tools-{t:?}"), tool_icon(*t), tool_title(*t)),
                None => (
                    "tools-disk".to_string(),
                    &icons::DISK as &rune::Icon,
                    "Disk usage",
                ),
            };
            if !query.is_empty() && !title.to_lowercase().contains(&query) {
                continue;
            }
            any = true;
            let tool = *tool;
            section = section.child(
                row()
                    .key(key)
                    .role("link")
                    .label(title)
                    .focusable(true)
                    .h(32.0)
                    .px(10.0)
                    .gap(10.0)
                    .center_y()
                    .radius(6.0)
                    .hover(theme::white(0.05))
                    .on_press(move |s: &mut AppState| match tool {
                        Some(t) => open_tool(s, t),
                        None => s.go_disk(),
                    })
                    .child(theme::glyph(i, 17.0, c(theme::T3)))
                    .child(
                        rune::text(title)
                            .font(rune::TextStyle::new(13.5, 440.0).lh(18.0))
                            .color(c(theme::T1)),
                    ),
            );
        }
        if any {
            let slot = if index == 4 { 0 } else { index };
            let stack = std::mem::replace(&mut stacks[slot], col());
            stacks[slot] = stack.child(section.mb(24.0));
        }
    }
    for stack in stacks {
        columns = columns.child(stack);
    }
    ui::page()
        .gap(22.0)
        .child(
            row()
                .w_full()
                .gap(24.0)
                .center_y()
                .child(theme::h1("Tools"))
                .child(
                    theme::field_lead(
                        Some(theme::glyph(&icons::SEARCH, 15.0, c(theme::T3))),
                        "tools-search",
                        "Search tools",
                        &s.tools.search,
                        |s| &mut s.tools.search,
                    )
                    .h(36.0)
                    .grow(1.0)
                    .max_w(520.0),
                ),
        )
        .child(ui::scroll(col().w_full().child(columns)))
}

fn back(key: &str, title: &str, action: impl Fn(&mut AppState, &mut rune::Tx) + 'static) -> El {
    Btn::new(key, title)
        .ghost()
        .icon(&icons::BACK)
        .on(action)
        .build()
}

/// Routes the reviewed screenshot overlay back to the originating utility, never the editor.
fn capture_utility(s: &mut AppState) {
    if !s.require(Feature::Utilities) { return; }
    #[cfg(windows)]
    if let Some(overlay) = s.shot.clone() {
        let ticket = s.tools.capture_ticket();
        let folder = s.preferences.folder.clone();
        s.feature_job(Feature::Utilities, move || {
            let capturer = toolkit::services::ScreenCapturer::detect(None, toolkit::services::LastRegionStore::none());
            let frame = capturer.with(|source| source.capture_desktop(&Default::default())).map_err(|e| e.to_string())?;
            let frame = crate::rune_shot::monitor_under_pointer().and_then(|r| frame.crop_desktop(r).ok()).unwrap_or(frame);
            crate::rune_shot::begin_for_utility(&overlay, frame, folder, move |s, path| {
                if !s.tools.open || s.tools.capture_ticket() != ticket || s.tools.job.is_some() {
                    return Err("The originating utility changed. Open the saved image in the intended tool.".into());
                }
                s.tools.files = vec![path];
                s.tools.run(s.wake.clone());
                s.page = ui::Page::Tools;
                Ok(())
            });
            Ok(JobResult::Quiet)
        });
        return;
    }
    s.tools.capture_region(s.wake.clone());
}

fn effect_default(mode: usize) -> &'static str {
    match mode {
        6 | 7 | 8 | 10 => "100",
        9 => "0",
        11 => "128",
        _ => "10",
    }
}
fn parameter_labels(tool: util::Tool, mode: usize) -> (Option<&'static str>, Option<&'static str>) {
    use util::Tool::*;
    match tool {
        Beautify => (
            Some("Padding (px, 0–512)"),
            Some("Corner radius (px, 0–256)"),
        ),
        Split => (Some("Columns"), Some("Rows")),
        Thumbnail => (Some("Maximum width (px)"), Some("Maximum height (px)")),
        Combine => (Some("Gap (px, 0–1024)"), None),
        Index => (Some("Depth (0 = all, maximum 64)"), None),
        VideoThumbnail => (
            Some("Timestamp (seconds)"),
            Some("Maximum width (px, 32–4096)"),
        ),
        VideoConvert if mode == 2 => (
            Some("GIF frame rate (fps, 1–30)"),
            Some("GIF maximum width (px, 32–1920)"),
        ),
        Effects => (
            match mode {
                3 => Some("Blur sigma (px, 1–100)"),
                4 => Some("Block size (px, 1–256)"),
                5 => Some("Sharpen (tenths, 0–100)"),
                6 | 7 => Some("Level (0–200, 100 = unchanged)"),
                8 => Some("Saturation (%, 0–300)"),
                9 => Some("Hue rotation (degrees, 0–360)"),
                10 => Some("Gamma (hundredths, 1–400)"),
                11 => Some("Threshold (0–255)"),
                12 | 13 => Some("Radius / width (px, 0–512)"),
                _ => None,
            },
            None,
        ),
        _ => (None, None),
    }
}
fn path_actions(key: String, path: PathBuf) -> El {
    let copy = path.clone();
    row()
        .gap(6.0)
        .flex_wrap()
        .child(
            rune::text(path.to_string_lossy())
                .font(theme::MONO)
                .color(c(theme::T2))
                .single_line()
                .truncate_head()
                .min_w(0.0)
                .grow(1.0),
        )
        .child(
            Btn::new(format!("{key}-copy"), "Copy path")
                .on(move |s, _| {
                    let path = copy.clone();
                    s.job(move || {
                        use toolkit::core::workflow::Clipboard;
                        toolkit::services::SystemClipboard::new(Default::default())
                            .set_text(&path.to_string_lossy())
                            .map(|_| JobResult::Notice("Copied path.".into()))
                            .map_err(|e| e.to_string())
                    });
                })
                .build(),
        )
        .child(
            Btn::new(format!("{key}-reveal"), "Reveal")
                .on(move |s, _| {
                    let path = path.clone();
                    s.job(move || {
                        let mut command;
                        #[cfg(windows)]
                        {
                            command = std::process::Command::new("explorer.exe");
                            if path.is_dir() {
                                command.arg(&path);
                            } else {
                                command.arg(format!("/select,{}", path.display()));
                            }
                        }
                        #[cfg(not(windows))]
                        {
                            command = std::process::Command::new("xdg-open");
                            command.arg(if path.is_dir() {
                                path.as_path()
                            } else {
                                path.parent().unwrap_or(&path)
                            });
                        }
                        command
                            .spawn()
                            .map(|_| JobResult::Quiet)
                            .map_err(|e| e.to_string())
                    });
                })
                .build(),
        )
}

fn tool_page(s: &AppState) -> El {
    let t = &s.tools;
    let column_label = |name: &str| theme::section_label(name.to_string());
    let tool = t.tool;
    let ticket = t.generation;
    let mut input = col()
        .key("tool-input")
        .grow(1.3)
        .min_w(0.0)
        .gap(12.0)
        .pr(24.0)
        .child(column_label("Input"));
    if tool.accepts_files() || tool == util::Tool::Index {
        input = input.child(
            Btn::new(
                "utility-input",
                if tool == util::Tool::Index {
                    "Choose folder…"
                } else {
                    "Add file…"
                },
            )
            .icon(&icons::FILE)
            .disabled(t.job.is_some())
            .on(move |_, tx| {
                let selected =
                    move |s: &mut AppState, result: rune::FileDialogResult, _: &mut rune::Tx| {
                        if s.tools.generation != ticket || s.tools.tool != tool {
                            return;
                        }
                        match result {
                            Ok(Some(path)) => {
                                s.tools.result = None;
                                if tool != util::Tool::Combine && tool != util::Tool::Hash {
                                    s.tools.files.clear();
                                }
                                s.tools.files.push(path);
                            }
                            Err(e) => s.notice = e.to_string(),
                            _ => {}
                        }
                    };
                if tool == util::Tool::Index {
                    tx.open_folder(Default::default(), selected);
                } else {
                    tx.open_file(Default::default(), selected);
                }
            })
            .build(),
        );
        for (i, path) in t.files.iter().enumerate() {
            input = input.child(
                row()
                    .gap(6.0)
                    .center_y()
                    .child(
                        rune::text(path.to_string_lossy())
                            .font(theme::MONO)
                            .color(c(theme::T1))
                            .single_line()
                            .truncate_head()
                            .grow(1.0)
                            .min_w(0.0),
                    )
                    .child(
                        Btn::new(format!("remove-input-{i}"), "Remove")
                            .disabled(t.job.is_some())
                            .on(move |s, _| {
                                if i < s.tools.files.len() {
                                    s.tools.files.remove(i);
                                }
                            })
                            .build(),
                    ),
            );
            if tool == util::Tool::Combine && i > 0 {
                input = input.child(
                    Btn::new(format!("move-input-{i}"), "Move up")
                        .disabled(t.job.is_some())
                        .on(move |s, _| {
                            if i < s.tools.files.len() {
                                s.tools.files.swap(i, i - 1);
                            }
                        })
                        .build(),
                );
            }
        }
    }
    if matches!(
        tool,
        util::Tool::Ocr | util::Tool::QrDecode | util::Tool::Viewer
    ) {
        input = input.child(
            Btn::new("utility-capture", "Capture region")
                .disabled(t.job.is_some())
                .on(|s, _| capture_utility(s))
                .build(),
        );
    }
    let text_label = match tool {
        util::Tool::Color => Some("Color (#RRGGBB or #RRGGBBAA)"),
        util::Tool::QrEncode => Some("Text or URL"),
        util::Tool::Hash => Some("Expected digest (optional)"),
        _ => None,
    };
    if let Some(label) = text_label {
        input = input.child(theme::small(label)).child(theme::field(
            "utility-text",
            label,
            &t.text,
            |s| &mut s.tools.text,
        ));
    }
    if matches!(tool, util::Tool::Windows | util::Tool::Clipboard) {
        input = input.child(theme::small(
            "Run to read the current value. Nothing is collected in the background.",
        ));
    }
    let mut options = col()
        .key("tool-options")
        .w(260.0)
        .gap(12.0)
        .px(24.0)
        .child(column_label("Options"));
    let modes: &[&str] = match tool {
        util::Tool::Effects => &[
            "Grayscale",
            "Sepia",
            "Invert",
            "Blur",
            "Pixelate",
            "Sharpen",
            "Brightness",
            "Contrast",
            "Saturation",
            "Hue",
            "Gamma",
            "Threshold",
            "Rounded corners",
            "Border",
            "Drop shadow",
        ],
        util::Tool::Combine => &["Vertical", "Horizontal"],
        util::Tool::VideoConvert => &["MP4", "WebM", "GIF"],
        util::Tool::Index => &["Text", "HTML", "JSON", "XML"],
        _ => &[],
    };
    for (mode, title) in modes.iter().enumerate() {
        options = options.child(
            Btn::new(
                format!("utility-mode-{mode}"),
                format!("{}{}", if t.mode == mode { "✓ " } else { "" }, title),
            )
            .disabled(t.job.is_some())
            .on(move |s, _| {
                s.tools.generation = s.tools.generation.wrapping_add(1);
                s.tools.mode = mode;
                s.tools.output = None;
                s.tools.result = None;
                if s.tools.tool == util::Tool::Effects {
                    s.tools.a = TextEdit::new(effect_default(mode));
                }
            })
            .build(),
        );
    }
    let (a_label, b_label) = parameter_labels(tool, t.mode);
    if let Some(label) = a_label {
        options =
            options
                .child(theme::small(label))
                .child(theme::field("utility-a", label, &t.a, |s| &mut s.tools.a));
    }
    if let Some(label) = b_label {
        options =
            options
                .child(theme::small(label))
                .child(theme::field("utility-b", label, &t.b, |s| &mut s.tools.b));
    }
    if matches!(tool, util::Tool::Beautify | util::Tool::Combine)
        || (tool == util::Tool::Effects && t.mode == 13)
    {
        options = options
            .child(theme::small("Background / border color"))
            .child(theme::field(
                "utility-color",
                "Color (#RRGGBBAA)",
                &t.color,
                |s| &mut s.tools.color,
            ));
    }
    if matches!(tool, util::Tool::Split | util::Tool::VideoConvert) {
        options = options
            .child(theme::small(
                t.output
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "Choose a destination before running".into()),
            ))
            .child(
                Btn::new("utility-output", "Choose destination…")
                    .disabled(t.job.is_some())
                    .on(move |_, tx| {
                        let selected =
                            move |s: &mut AppState,
                                  result: rune::FileDialogResult,
                                  _: &mut rune::Tx| {
                                if s.tools.generation != ticket || s.tools.tool != tool {
                                    return;
                                }
                                match result {
                                    Ok(Some(path)) => s.tools.output = Some(path),
                                    Err(e) => s.notice = e.to_string(),
                                    _ => {}
                                }
                            };
                        if tool == util::Tool::Split {
                            tx.open_folder(Default::default(), selected);
                        } else {
                            tx.save_file(
                                rune::FileDialogOptions::default()
                                    .filter("Video", &["mp4", "webm", "gif"]),
                                selected,
                            );
                        }
                    })
                    .build(),
            );
    }
    // Result: what the tool produced.
    let status = if t.job.is_some() {
        Some((theme::BUSY, "Working…"))
    } else if t.save_failed {
        Some((theme::T1, "Save failed · result retained"))
    } else if t.result.is_some() {
        Some((theme::ACCENT, "Done"))
    } else {
        None
    };
    let mut head = row()
        .w_full()
        .gap(10.0)
        .center_y()
        .child(column_label("Result").grow(1.0));
    if let Some((tint, word)) = status {
        head = head.child(
            row()
                .role("status")
                .gap(6.0)
                .center_y()
                .child(theme::dot(7.0, c(tint)))
                .child(rune::text(word).font(theme::UI).color(c(theme::T1))),
        );
    }
    let mut result = col()
        .key("tool-result")
        .grow(1.3)
        .min_w(0.0)
        .gap(12.0)
        .pl(24.0)
        .child(head);
    let mut output_box = col()
        .w_full()
        .grow(1.0)
        .min_h(200.0)
        .padding(16.0)
        .gap(12.0)
        .radius(6.0)
        .fill(c(theme::BG2));
    if t.message.is_empty() && t.result.is_none() {
        output_box = output_box.child(theme::small("Run the tool to see its result here."));
    } else if !t.message.is_empty() {
        output_box = output_box.child(
            rune::text(t.message.clone())
                .font(theme::MONO)
                .color(c(theme::T1))
                .wrap()
                .selectable(true),
        );
    }
    if let Some(output) = &t.result
        && output.image.is_some()
    {
        output_box = output_box.child(
            el().w_full()
                .h(220.0)
                .radius(4.0)
                .native_surface(&t.surface, rune::SurfaceFit::Fit),
        );
    }
    let mut actions = row()
        .gap(8.0)
        .child(
            Btn::new("utility-run", "Run")
                .primary()
                .disabled(t.job.is_some())
                .on(|s, _| { if s.require(Feature::Utilities) { s.tools.run(s.wake.clone()); } })
                .build(),
        )
        .child(
            Btn::new("utility-cancel", "Cancel")
                .disabled(t.job.is_none())
                .on(|s, _| s.tools.cancel.cancel())
                .build(),
        );
    if t.result.as_ref().is_some_and(|o| o.image.is_some()) {
        actions = actions.child(
            Btn::new("utility-save-image", "Save image…")
                .disabled(t.job.is_some())
                .on(move |_, tx| {
                    tx.save_file(
                        rune::FileDialogOptions::default()
                            .filter("Images", &["png", "jpg", "jpeg", "webp", "bmp"]),
                        move |s: &mut AppState, result, _| {
                            if s.tools.generation != ticket || s.tools.tool != tool {
                                return;
                            }
                            match result {
                                Ok(Some(path)) => s.tools.save_result(path, s.wake.clone()),
                                Err(e) => s.notice = e.to_string(),
                                _ => {}
                            }
                        },
                    );
                })
                .build(),
        );
    }
    if let Some(output) = &t.result {
        let image = output.image.clone();
        let text = output.text.clone();
        actions = actions.child(
            Btn::new(
                "utility-copy",
                if image.is_some() {
                    "Copy image"
                } else {
                    "Copy text"
                },
            )
            .on(move |s, _| {
                let image = image.clone();
                let text = text.clone();
                s.feature_job(Feature::Utilities, move || {
                    use toolkit::core::workflow::Clipboard;
                    let clipboard = toolkit::services::SystemClipboard::new(Default::default());
                    if let Some(frame) = image {
                        clipboard.set_image(&frame)
                    } else {
                        clipboard.set_text(&text)
                    }
                    .map(|_| JobResult::Notice("Copied result.".into()))
                    .map_err(|e| e.to_string())
                });
            })
            .build(),
        );
        if output.image.is_none() {
            actions = actions.child(
                Btn::new("utility-save-text", "Save text…")
                    .disabled(t.job.is_some())
                    .on(move |_, tx| {
                        tx.save_file(
                            rune::FileDialogOptions::default()
                                .filter("Reports", &["txt", "html", "json", "xml"]),
                            move |s: &mut AppState, result, _| {
                                if s.tools.generation != ticket || s.tools.tool != tool {
                                    return;
                                }
                                match result {
                                    Ok(Some(path)) => s.tools.save_result(path, s.wake.clone()),
                                    Err(e) => s.notice = e.to_string(),
                                    _ => {}
                                }
                            },
                        );
                    })
                    .build(),
            );
        }
        for (i, path) in output.files.iter().enumerate() {
            output_box = output_box.child(path_actions(format!("utility-file-{i}"), path.clone()));
        }
    }
    result = result.child(output_box).child(actions.flex_wrap());
    let divider = || el().w(1.0).self_stretch().fill(theme::line());
    col()
        .grow(1.0)
        .min_h(0.0)
        .pt(14.0)
        .px(32.0)
        .pb(32.0)
        .gap(18.0)
        .child(
            row()
                .w_full()
                .gap(12.0)
                .center_y()
                .child(back("tool-back", "Tools", |s, _| {
                    s.tools.close();
                    s.go(ui::Page::Tools);
                }))
                .child(theme::h1(tool_title(t.tool)))
                .child(
                    theme::small(t.tool.description())
                        .grow(1.0)
                        .min_w(0.0)
                        .truncate(),
                ),
        )
        .child(ui::scroll(
            row()
                .w_full()
                .items_stretch()
                .child(input)
                .child(divider())
                .child(options)
                .child(divider())
                .child(result),
        ))
}

impl Disk {
    pub fn close(&mut self) {
        self.cancel.cancel();
        self.job = None;
        self.tree = None;
        self.root = 0;
        self.message.clear();
    }
}

/// Treemap colours from the design, cycled over the largest entries.
const TINTS: [u32; 5] = [0x3987e5, 0x9085e9, 0x199e70, 0xd95926, 0xd55181];

pub fn disk_view(s: &AppState) -> El {
    let d = &s.disk;
    let scan = Btn::new(
        "disk-scan",
        if d.tree.is_some() {
            "Scan another…"
        } else {
            "Scan folder…"
        },
    )
    .icon(&icons::FOLDER)
    .on(|_, tx| {
        tx.open_folder(
            Default::default(),
            |s: &mut AppState, result, _| match result {
                Ok(Some(path)) => { if s.require(Feature::StorageAnalysis) { s.disk.start(path, s.wake.clone()); } },
                Err(e) => s.notice = e.to_string(),
                _ => {}
            },
        )
    })
    .build();
    let mut header = row()
        .w_full()
        .gap(12.0)
        .center_y()
        .flex_wrap()
        .child(back("disk-back", "Tools", |s, _| s.go(ui::Page::Tools)))
        .child(theme::h1("Disk usage"))
        .child(theme::spacer())
        .child(theme::small(d.message.clone()));
    if d.job.is_some() {
        header = header.child(
            Btn::new("disk-cancel", "Cancel scan")
                .ghost()
                .on(|s, _| s.disk.cancel.cancel())
                .build(),
        );
    }
    header = header.child(scan);
    let page = col()
        .grow(1.0)
        .min_h(0.0)
        .pt(14.0)
        .px(32.0)
        .pb(28.0)
        .gap(18.0)
        .child(header);
    let Some(tree) = &d.tree else {
        return page.child(
            col()
                .w_full()
                .grow(1.0)
                .gap(8.0)
                .center()
                .child(theme::glyph(&icons::DISK, 28.0, c(theme::T3)))
                .child(theme::h2(if d.job.is_some() { "Scanning…" } else { "Pick a drive or folder" }))
                .child(theme::small(
                    "Logical file sizes. Links and offline files are skipped; allocated space can differ.",
                )),
        );
    };
    let node = &tree.nodes[d.root];
    let mut children: Vec<usize> = node.children.clone();
    children.sort_by_key(|&id| std::cmp::Reverse(tree.nodes[id].bytes));
    let total = node.bytes.max(1);
    let mut summary = row()
        .w_full()
        .gap(12.0)
        .center_y()
        .child(
            rune::text(format!("{} ", bytes(node.bytes)))
                .font(theme::H2)
                .color(c(theme::T1)),
        )
        .child(
            rune::text(tree.path(d.root).to_string_lossy())
                .font(theme::SMALL)
                .color(c(theme::T2))
                .single_line()
                .truncate_head()
                .grow(1.0)
                .min_w(0.0),
        );
    if let Some(parent) = node.parent {
        summary = summary.child(
            Btn::new("disk-up", "Up one level")
                .ghost()
                .icon(&icons::CHEVRON_UP)
                .on(move |s, _| {
                    s.disk.root = parent;
                    s.disk.page = 0;
                })
                .build(),
        );
    }
    // A one-level treemap: the largest entries side by side, sized by bytes.
    let mut map = row()
        .key("disk-map")
        .w_full()
        .grow(1.0)
        .min_h(260.0)
        .gap(2.0);
    let shown: Vec<usize> = children.iter().copied().take(8).collect();
    let rest: u64 = children
        .iter()
        .skip(8)
        .map(|&id| tree.nodes[id].bytes)
        .sum();
    for (rank, &id) in shown.iter().enumerate() {
        let n = &tree.nodes[id];
        if n.bytes == 0 {
            continue;
        }
        let tint = c(TINTS[rank % TINTS.len()]);
        let directory = n.is_directory;
        map = map.child(
            col()
                .key(format!("disk-block-{id}"))
                .role("button")
                .label(format!("{}, {}", n.name.to_string_lossy(), bytes(n.bytes)))
                .focusable(directory)
                .grow(n.bytes as f32 / total as f32)
                .min_w(0.0)
                .radius(4.0)
                .padding(10.0)
                .justify_between()
                .fill(tint.alpha(0.34))
                .hover(tint.alpha(0.46))
                .clip_rounded(4.0)
                .on_press(move |s: &mut AppState| {
                    if directory {
                        s.disk.root = id;
                        s.disk.page = 0;
                    }
                })
                .child(
                    rune::text(n.name.to_string_lossy())
                        .font(rune::TextStyle::new(13.0, 600.0).lh(16.0))
                        .color(c(theme::T1))
                        .single_line()
                        .truncate(),
                )
                .child(
                    rune::text(bytes(n.bytes))
                        .font(theme::NUM)
                        .color(c(theme::T2))
                        .single_line(),
                ),
        );
    }
    if rest > 0 {
        map = map.child(
            col()
                .grow(rest as f32 / total as f32)
                .min_w(0.0)
                .radius(4.0)
                .padding(10.0)
                .justify_end()
                .fill(c(0x2a2b2f))
                .child(
                    rune::text(bytes(rest))
                        .font(theme::NUM)
                        .color(c(theme::T2))
                        .single_line(),
                ),
        );
    }
    let mut largest = col()
        .w_full()
        .gap(0.0)
        .child(theme::section_label("Largest").pb(8.0));
    let pages = children.len().div_ceil(100).max(1);
    let current = d.page.min(pages - 1);
    largest = largest
        .child(theme::small(format!(
            "{}–{} of {} entries · page {} / {}",
            if children.is_empty() {
                0
            } else {
                current * 100 + 1
            },
            ((current + 1) * 100).min(children.len()),
            children.len(),
            current + 1,
            pages
        )))
        .child(
            row()
                .gap(8.0)
                .child(
                    Btn::new("disk-previous", "Previous")
                        .disabled(current == 0)
                        .on(|s, _| s.disk.page = s.disk.page.saturating_sub(1))
                        .build(),
                )
                .child(
                    Btn::new("disk-next", "Next")
                        .disabled(current + 1 >= pages)
                        .on(move |s, _| s.disk.page = (current + 1).min(pages - 1))
                        .build(),
                ),
        );
    for (rank, &id) in children.iter().enumerate().skip(current * 100).take(100) {
        let n = &tree.nodes[id];
        let fraction = n.bytes as f32 / total as f32;
        let directory = n.is_directory;
        largest = largest
            .child(
                row()
                    .key(format!("disk-node-{id}"))
                    .role("button")
                    .label(format!("{}, {}", n.name.to_string_lossy(), bytes(n.bytes)))
                    .focusable(true)
                    .w_full()
                    .h(32.0)
                    .px(6.0)
                    .gap(12.0)
                    .center_y()
                    .radius(5.0)
                    .hover(theme::white(0.05))
                    .on_press(move |s: &mut AppState| {
                        if directory {
                            s.disk.root = id;
                            s.disk.page = 0;
                        }
                    })
                    .child(
                        rune::text(n.name.to_string_lossy())
                            .font(rune::TextStyle::new(13.0, 440.0).lh(16.0))
                            .color(c(theme::T1))
                            .single_line()
                            .truncate()
                            .grow(1.0)
                            .min_w(0.0),
                    )
                    .child(theme::meter(
                        120.0,
                        fraction,
                        if rank < 8 {
                            c(TINTS[rank % TINTS.len()])
                        } else {
                            c(theme::T3)
                        },
                    ))
                    .child(
                        rune::text(bytes(n.bytes))
                            .font(theme::NUM)
                            .color(c(theme::T2))
                            .single_line()
                            .w(80.0)
                            .align(rune::Align::End),
                    ),
            )
            .child(path_actions(format!("disk-entry-{id}"), tree.path(id)));
    }
    let mut details = col().w_full().gap(6.0)
        .child(theme::small(format!("{} · {:.1}s · {} files · {} folders · {} skipped · {} errors", tree.method.label(), tree.elapsed.as_secs_f64(), node.files, node.directories, node.skipped, node.errors)))
        .child(theme::small("Logical file sizes, not allocated disk space. Hard links count once per path. Sparse/compressed files can differ. Links and offline entries are skipped."));
    if tree.cancelled || tree.nodes[0].errors > 0 || tree.nodes[0].skipped > 0 {
        details = details.child(theme::small("Partial totals: cancelled, skipped or unreadable entries are omitted. Resolve access errors and rescan to include readable entries."));
    }
    if let Some(reason) = &tree.fallback_reason {
        details = details.child(theme::small(format!("Backend fallback: {reason}")));
    }
    for (i, error) in tree.errors.iter().enumerate() {
        details = details
            .child(
                rune::text(format!("{}: {}", error.path.display(), error.message))
                    .font(theme::SMALL)
                    .color(c(theme::T2))
                    .wrap()
                    .selectable(true),
            )
            .child(path_actions(format!("disk-error-{i}"), error.path.clone()));
    }
    details = details.child(path_actions("disk-current".into(), tree.path(d.root)));
    let mut crumbs = row().gap(6.0).flex_wrap();
    let mut ancestors = vec![d.root];
    while let Some(parent) = tree.nodes[*ancestors.last().unwrap()].parent {
        ancestors.push(parent);
    }
    for id in ancestors.into_iter().rev() {
        crumbs = crumbs.child(
            Btn::new(
                format!("disk-crumb-{id}"),
                tree.nodes[id].name.to_string_lossy().to_string(),
            )
            .ghost()
            .on(move |s, _| {
                s.disk.root = id;
                s.disk.page = 0;
            })
            .build(),
        );
    }
    page.child(crumbs)
        .child(ui::scroll(details).max_h(180.0))
        .child(
            row()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .gap(32.0)
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(12.0)
                        .child(summary)
                        .child(map),
                )
                .child(ui::scroll(largest).w(360.0).grow(0.0)),
        )
}

#[cfg(windows)]
pub fn screenshot(s: &mut AppState, target: toolkit::core::workflow::CaptureTarget) {
    if !s.require(Feature::Screenshots) { return; }
    use toolkit::core::workflow::CaptureTarget;
    let folder = s.preferences.folder.clone();
    #[cfg(windows)]
    let overlay = s.shot.clone();
    s.feature_job(Feature::Screenshots, move || {
        let capturer = toolkit::services::ScreenCapturer::detect(
            None,
            toolkit::services::LastRegionStore::none(),
        );
        let frame = capturer
            .with(|source| {
                if target == CaptureTarget::Window {
                    source
                        .capture_active_window(&Default::default())
                        .map(|(f, _)| f)
                } else {
                    source.capture_desktop(&Default::default())
                }
            })
            .map_err(|e| format!("{e}"))?;
        if matches!(target, CaptureTarget::Region | CaptureTarget::LastRegion) {
            // Select and annotate in place, on the monitor under the pointer.
            #[cfg(windows)]
            if let Some(overlay) = overlay {
                let monitor = crate::rune_shot::monitor_under_pointer()
                    .and_then(|r| frame.crop_desktop(r).ok());
                let frame = match monitor {
                    Some(frame) => frame,
                    None => frame,
                };
                crate::rune_shot::begin(&overlay, frame, folder);
                return Ok(JobResult::Quiet);
            }
            let input = screenshot_overlay::OverlayInput {
                desktop: frame.clone(),
                monitors: vec![],
                windows: vec![],
                options: Default::default(),
            };
            return match screenshot_overlay::select(input).map_err(|e| e.to_string())? {
                screenshot_overlay::OverlayOutcome::Selected(selection) => {
                    let frame = frame
                        .crop_desktop(selection.rect)
                        .map_err(|e| format!("{e:#}"))?;
                    save_screenshot(&folder, &frame).map(JobResult::Capture)
                }
                _ => Ok(JobResult::Notice("Capture cancelled.".into())),
            };
        }
        save_screenshot(&folder, &frame).map(|_| JobResult::Saved)
    });
}

fn save_screenshot(
    folder: &std::path::Path,
    frame: &toolkit::types::Frame,
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let path = folder.join(format!(
        "screenshot-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    util::save_image(&path, frame).map_err(|e| format!("{e:#}"))?;
    Ok(path)
}

#[cfg(test)]
mod utility_form_tests {
    use super::*;
    #[test]
    fn failed_save_keeps_output_and_cancelled_workers_remain_owned() {
        let mut state = State::default();
        state.result = Some(util::Output {
            text: "generated report".into(),
            image: None,
            preview: None,
            files: vec![],
        });
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(Err("destination exists".into())).unwrap();
        state.job = Some(rx);
        assert!(state.poll());
        assert!(state.save_failed);
        assert_eq!(state.result.as_ref().unwrap().text, "generated report");
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        let (release, wait) = mpsc::channel();
        state.worker = Some(std::thread::spawn(move || {
            wait.recv().unwrap();
            flag.store(true, std::sync::atomic::Ordering::Release);
        }));
        state.reset(util::Tool::Hash);
        assert_eq!(state.retired_workers.len(), 1);
        release.send(()).unwrap();
        drop(state);
        assert!(done.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn task_defaults_and_stale_capture_ownership() {
        let mut state = State::default();
        state.reset(util::Tool::Hash);
        assert!(state.text.text.is_empty());
        assert_eq!(parameter_labels(util::Tool::Hash, 0), (None, None));
        let old = state.capture_ticket();
        state.reset(util::Tool::VideoThumbnail);
        assert_ne!(state.capture_ticket(), old);
        assert_eq!(state.a.text, "0");
        assert_eq!(state.b.text, "1024");
        state.reset(util::Tool::Split);
        assert_eq!((&*state.a.text, &*state.b.text), ("2", "2"));
        state.close();
        assert!(!state.open);
        assert_eq!(bytes(0), "0 B");
    }
}
