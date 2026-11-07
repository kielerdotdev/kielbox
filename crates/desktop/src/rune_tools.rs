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
    cancel: CancelToken,
    surface: NativeSurface,
}
impl Default for State {
    fn default() -> Self {
        Self {
            tool: util::Tool::Color,
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
            cancel: CancelToken::new(),
            surface: NativeSurface::new(),
        }
    }
}
impl State {
    fn run(&mut self, wake: rune::Waker) {
        if self.job.is_some() {
            return;
        }
        let parse = |s: &TextEdit| {
            s.text
                .parse::<u32>()
                .map_err(|_| "Dimensions must be positive whole numbers.")
        };
        let (a, b) = match (parse(&self.a), parse(&self.b)) {
            (Ok(a), Ok(b)) => (a, b),
            _ => {
                self.message = "Dimensions must be whole numbers.".into();
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
                let result = util::with_pool(|| util::run(request, &cancel)?.prepare())
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
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.job else {
            return false;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.job = None;
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
                    Err(e) => self.message = e,
                }
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.job = None;
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
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[derive(Default)]
pub struct Disk {
    pub tree: Option<disk::disk_usage::DiskUsage>,
    root: usize,
    job: Option<mpsc::Receiver<Result<disk::disk_usage::DiskUsage, String>>>,
    progress: Arc<Mutex<Option<disk::disk_usage::ScanProgress>>>,
    cancel: CancelToken,
    message: String,
}
impl Disk {
    fn start(&mut self, path: PathBuf, wake: rune::Waker) {
        self.cancel.cancel();
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
    } else {
        format!("{:.1} KiB", v as f64 / 1024.0)
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
    if s.tools.tool != tool {
        s.tools.message.clear();
        s.tools.result = None;
    }
    s.tools.tool = tool;
    s.tools.open = true;
    s.page = ui::Page::Tools;
}
impl State {
    pub fn open_tool(&self) -> Option<util::Tool> {
        self.open.then_some(self.tool)
    }
    pub fn close(&mut self) {
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
        let mut section = col().w(220.0).gap(0.0).child(
            theme::section_label(group).px(10.0).pb(6.0),
        );
        let mut any = false;
        for tool in tools {
            let (key, i, title) = match tool {
                Some(t) => (format!("tools-{t:?}"), tool_icon(*t), tool_title(*t)),
                None => ("tools-disk".to_string(), &icons::DISK as &rune::Icon, "Disk usage"),
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

fn tool_page(s: &AppState) -> El {
    let t = &s.tools;
    let column_label = |name: &str| theme::section_label(name.to_string());
    // Input: files and text the tool reads.
    let mut input = col()
        .key("tool-input")
        .grow(1.3)
        .min_w(0.0)
        .gap(12.0)
        .pr(24.0)
        .child(column_label("Input"))
        .child(
            row()
                .gap(6.0)
                .flex_wrap()
                .child(
                    Btn::new("utility-input", "Add file…")
                        .icon(&icons::FILE)
                        .on(|_, tx| {
                            tx.open_file(
                                Default::default(),
                                |s: &mut AppState, result, _| match result {
                                    Ok(Some(path)) => s.tools.files.push(path),
                                    Err(e) => s.notice = e.to_string(),
                                    _ => {}
                                },
                            )
                        })
                        .build(),
                )
                .child(
                    Btn::new("utility-capture", "Capture region")
                        .ghost()
                        .icon(&icons::REGION)
                        .on(|s, _| {
                            #[cfg(windows)]
                            screenshot(s, toolkit::core::workflow::CaptureTarget::Region);
                            #[cfg(not(windows))]
                            {
                                s.notice = "Screen capture is available on Windows.".into();
                            }
                        })
                        .build(),
                ),
        );
    let mut files = col()
        .w_full()
        .gap(2.0)
        .padding(8.0)
        .min_h(120.0)
        .radius(6.0)
        .fill(c(0x121316));
    if t.files.is_empty() {
        files = files.child(theme::small("No files yet. Add one, or use the text field below."));
    }
    for (i, path) in t.files.iter().enumerate() {
        files = files.child(
            row()
                .key(format!("input-{i}"))
                .h(28.0)
                .gap(8.0)
                .pl(4.0)
                .center_y()
                .child(theme::glyph(&icons::FILE, 14.0, c(theme::T3)))
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
                    Btn::icon_only(format!("remove-input-{i}"), "Remove", &icons::CLOSE)
                        .h(24.0)
                        .on(move |s, _| {
                            if i < s.tools.files.len() {
                                s.tools.files.remove(i);
                            }
                        })
                        .build(),
                ),
        );
    }
    input = input
        .child(files)
        .child(theme::small("Text, URL, timestamp or folder"))
        .child(theme::field(
            "utility-text",
            "Text, URL, timestamp or folder",
            &t.text,
            |s| &mut s.tools.text,
        ));
    // Options: sizes, colour and where the result goes.
    let labelled = |title: &str, field: El| {
        col()
            .w_full()
            .gap(6.0)
            .child(theme::small(title.to_string()))
            .child(field)
    };
    let output = t
        .output
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "Not chosen".into());
    let options = col()
        .key("tool-options")
        .w(260.0)
        .gap(16.0)
        .px(24.0)
        .child(column_label("Options"))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(labelled(
                    "Width",
                    theme::field("utility-width", "Width", &t.a, |s| &mut s.tools.a),
                ).grow(1.0).min_w(0.0))
                .child(labelled(
                    "Height",
                    theme::field("utility-height", "Height", &t.b, |s| &mut s.tools.b),
                ).grow(1.0).min_w(0.0)),
        )
        .child(labelled(
            "Color",
            theme::field("utility-color", "Color", &t.color, |s| &mut s.tools.color),
        ))
        .child(
            col()
                .w_full()
                .gap(6.0)
                .child(theme::small("Output"))
                .child(
                    rune::text(output)
                        .font(theme::MONO)
                        .color(c(theme::T1))
                        .single_line()
                        .truncate_head(),
                )
                .child(
                    Btn::new("utility-output", "Choose output…")
                        .on(|_, tx| {
                            tx.save_file(
                                Default::default(),
                                |s: &mut AppState, result, _| match result {
                                    Ok(Some(path)) => s.tools.output = Some(path),
                                    Err(e) => s.notice = e.to_string(),
                                    _ => {}
                                },
                            )
                        })
                        .build(),
                ),
        );
    // Result: what the tool produced.
    let status = if t.job.is_some() {
        Some((theme::BUSY, "Working…"))
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
                .on(|s, _| s.tools.run(s.wake.clone()))
                .build(),
        )
        .child(
            Btn::new("utility-cancel", "Cancel")
                .disabled(t.job.is_none())
                .on(|s, _| s.tools.cancel.cancel())
                .build(),
        );
    if let Some(frame) = t.result.as_ref().and_then(|o| o.image.clone()) {
        actions = actions.child(
            Btn::new("utility-save-image", "Save image…")
                .on(move |_, tx| {
                    let frame = frame.clone();
                    tx.save_file(
                        rune::FileDialogOptions::default().filter("PNG", &["png"]),
                        move |s: &mut AppState, result, _| {
                            if let Ok(Some(path)) = result {
                                let frame = frame.clone();
                                s.job(move || {
                                    util::save_image(&path, &frame)
                                        .map(|_| {
                                            JobResult::Notice(format!("Saved {}", path.display()))
                                        })
                                        .map_err(|e| format!("{e:#}"))
                                });
                            }
                        },
                    );
                })
                .build(),
        );
    }
    result = result.child(output_box).child(actions);
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
                    s.tools.open = false;
                    s.go(ui::Page::Tools);
                }))
                .child(theme::h1(tool_title(t.tool)))
                .child(theme::small(t.tool.description()).grow(1.0).min_w(0.0).truncate()),
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
    let scan = Btn::new("disk-scan", if d.tree.is_some() { "Scan another…" } else { "Scan folder…" })
        .icon(&icons::FOLDER)
        .on(|_, tx| {
            tx.open_folder(
                Default::default(),
                |s: &mut AppState, result, _| match result {
                    Ok(Some(path)) => s.disk.start(path, s.wake.clone()),
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
                .on(move |s, _| s.disk.root = parent)
                .build(),
        );
    }
    // A one-level treemap: the largest entries side by side, sized by bytes.
    let mut map = row().key("disk-map").w_full().grow(1.0).min_h(260.0).gap(2.0);
    let shown: Vec<usize> = children.iter().copied().take(8).collect();
    let rest: u64 = children.iter().skip(8).map(|&id| tree.nodes[id].bytes).sum();
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
    for (rank, &id) in children.iter().take(500).enumerate() {
        let n = &tree.nodes[id];
        let fraction = n.bytes as f32 / total as f32;
        let directory = n.is_directory;
        largest = largest.child(
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
        );
    }
    page.child(
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
    use toolkit::core::workflow::CaptureTarget;
    let folder = s.preferences.folder.clone();
    s.job(move || {
        let capturer = toolkit::services::ScreenCapturer::detect(
            None,
            toolkit::services::LastRegionStore::none(),
        );
        let mut frame = capturer
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
        if target == CaptureTarget::Region {
            let input = screenshot_overlay::OverlayInput {
                desktop: frame.clone(),
                monitors: vec![],
                windows: vec![],
                options: Default::default(),
            };
            match screenshot_overlay::select(input).map_err(|e| e.to_string())? {
                screenshot_overlay::OverlayOutcome::Selected(selection) => {
                    let r = selection.rect;
                    frame = frame.crop_desktop(r).map_err(|e| format!("{e:#}"))?;
                }
                _ => return Ok(JobResult::Notice("Capture cancelled.".into())),
            }
        } else if target == CaptureTarget::LastRegion {
            return Err("Select a region again; last-region restore is not yet available.".into());
        }
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let path = folder.join(format!(
            "screenshot-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        util::save_image(&path, &frame).map_err(|e| format!("{e:#}"))?;
        Ok(JobResult::Capture(path))
    });
}
