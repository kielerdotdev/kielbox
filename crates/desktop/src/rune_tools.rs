use crate::rune_ui::{
    self as ui, JobResult, State as AppState, button, color, field, heading, label, muted,
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
pub fn view(s: &AppState) -> El {
    let t = &s.tools;
    let mut choices = col().w(200.0).gap(4.0);
    for (i, tool) in util::Tool::ALL.into_iter().enumerate() {
        choices = choices.child(
            button(format!("tool-{i}"), tool.name(), move |s, _| {
                s.tools.tool = tool;
                s.tools.message.clear();
                s.tools.result = None;
            })
            .checked(t.tool == tool),
        );
    }
    let mut form = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(12.0)
        .child(heading(t.tool.name()))
        .child(muted(t.tool.description()))
        .child(label("Text / timestamp / color / folder"))
        .child(field("utility-text", &t.text, |s| &mut s.tools.text))
        .child(
            row()
                .gap(8.0)
                .child(field("utility-width", &t.a, |s| &mut s.tools.a))
                .child(field("utility-height", &t.b, |s| &mut s.tools.b)),
        )
        .child(field("utility-color", &t.color, |s| &mut s.tools.color))
        .child(button("utility-input", "Add input file…", |_, tx| {
            tx.open_file(
                Default::default(),
                |s: &mut AppState, result, _| match result {
                    Ok(Some(path)) => s.tools.files.push(path),
                    Err(e) => s.notice = e.to_string(),
                    _ => {}
                },
            )
        }));
    for (i, path) in t.files.iter().enumerate() {
        form = form.child(
            row()
                .gap(6.0)
                .child(muted(path.to_string_lossy()).grow(1.0))
                .child(button(
                    format!("remove-input-{i}"),
                    "Remove",
                    move |s, _| {
                        if i < s.tools.files.len() {
                            s.tools.files.remove(i);
                        }
                    },
                )),
        );
    }
    form = form
        .child(button("utility-output", "Choose output…", |_, tx| {
            tx.save_file(
                Default::default(),
                |s: &mut AppState, result, _| match result {
                    Ok(Some(path)) => s.tools.output = Some(path),
                    Err(e) => s.notice = e.to_string(),
                    _ => {}
                },
            )
        }))
        .child(
            row()
                .gap(6.0)
                .child(
                    button("utility-run", "Run", |s, _| s.tools.run(s.wake.clone()))
                        .disabled(t.job.is_some()),
                )
                .child(
                    button("utility-cancel", "Cancel", |s, _| s.tools.cancel.cancel())
                        .disabled(t.job.is_none()),
                ),
        );
    if let Some(path) = &t.output {
        form = form.child(muted(format!("Output: {}", path.display())));
    }
    if !t.message.is_empty() {
        form = form.child(label(&t.message));
    }
    if let Some(output) = &t.result {
        if output.image.is_some() {
            form = form.child(
                el().w_full()
                    .h(250.0)
                    .native_surface(&t.surface, rune::SurfaceFit::Fit),
            );
        }
        if let Some(frame) = &output.image {
            let frame = frame.clone();
            form = form.child(button(
                "utility-save-image",
                "Save result image…",
                move |_, tx| {
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
                },
            ));
        }
    }
    col()
        .grow(1.0)
        .min_h(0.0)
        .gap(16.0)
        .child(heading("Utilities"))
        .child(
            row()
                .grow(1.0)
                .min_h(0.0)
                .gap(20.0)
                .child(choices.scroll(rune::ScrollSpec {
                    stick_bottom: false,
                }))
                .child(
                    form.scroll(rune::ScrollSpec {
                        stick_bottom: false,
                    })
                    .grow(1.0)
                    .min_h(0.0),
                ),
        )
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
pub fn disk_view(s: &AppState) -> El {
    let d = &s.disk;
    let mut page = col()
        .grow(1.0)
        .min_h(0.0)
        .gap(12.0)
        .child(heading("Disk usage"))
        .child(
            row()
                .gap(8.0)
                .child(button("disk-scan", "Scan folder…", |_, tx| {
                    tx.open_folder(
                        Default::default(),
                        |s: &mut AppState, result, _| match result {
                            Ok(Some(path)) => s.disk.start(path, s.wake.clone()),
                            Err(e) => s.notice = e.to_string(),
                            _ => {}
                        },
                    )
                }))
                .child(
                    button("disk-cancel", "Cancel scan", |s, _| s.disk.cancel.cancel())
                        .disabled(d.job.is_none()),
                ),
        )
        .child(muted(
            "Logical file sizes. Links and offline files are skipped; allocated space can differ.",
        ));
    if let Some(tree) = &d.tree {
        let node = &tree.nodes[d.root];
        page = page.child(label(tree.path(d.root).to_string_lossy()));
        if let Some(parent) = node.parent {
            page = page.child(button("disk-up", "Up one level", move |s, _| {
                s.disk.root = parent
            }));
        }
        let mut entries = col().w_full().gap(6.0);
        for &id in node.children.iter().take(500) {
            let n = &tree.nodes[id];
            let percent = if node.bytes == 0 {
                0.0
            } else {
                n.bytes as f32 / node.bytes as f32
            };
            entries = entries.child(
                button(
                    format!("disk-node-{id}"),
                    format!("{}    {}", n.name.to_string_lossy(), bytes(n.bytes)),
                    move |s, _| {
                        if s.disk
                            .tree
                            .as_ref()
                            .is_some_and(|t| t.nodes[id].is_directory)
                        {
                            s.disk.root = id;
                        }
                    },
                )
                .w_full()
                .child(
                    el().h(3.0)
                        .w((percent * 600.0).max(1.0))
                        .fill(color(ui::ACCENT)),
                ),
            );
        }
        page = page.child(
            entries
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .grow(1.0)
                .min_h(0.0),
        );
    }
    page.child(label(&d.message))
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
