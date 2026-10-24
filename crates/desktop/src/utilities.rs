//! GPUI front end for local ShareX-style utilities.
use crate::{image_editor::render_image, text_input::TextInput, theme::*};
use gpui::{prelude::*, *};
use rcr_toolkit::{
    core::workflow::{CancelToken, Clipboard},
    services,
    types::Frame,
    utilities::{self, Output, Request, Tool},
};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};
pub enum UtilityEvent {
    Edit(Arc<Frame>),
    Reveal,
}
enum WorkResult {
    Replace(Output),
    Notice(String),
}
#[derive(Clone, Copy)]
struct DialogTicket {
    tool: Tool,
    input_revision: u64,
    result_revision: Option<u64>,
}
impl DialogTicket {
    fn current(self, tool: Tool, input_revision: u64, result_revision: Option<u64>, busy: bool) -> bool {
        !busy && self.tool == tool && self.input_revision == input_revision
            && self.result_revision.is_none_or(|revision| Some(revision) == result_revision)
    }
}
#[derive(Default)]
enum ScreenFeedback {
    #[default]
    Quiet,
    Failure,
    Completion,
}
impl ScreenFeedback {
    fn finish(&mut self, failed: bool, cancelled: bool) -> bool {
        match std::mem::take(self) {
            Self::Quiet => false,
            Self::Failure => failed && !cancelled,
            Self::Completion => !cancelled,
        }
    }
}
pub struct Utilities {
    tool: Tool,
    files: Vec<PathBuf>,
    output: Option<PathBuf>,
    text: Entity<TextInput>,
    a: Entity<TextInput>,
    b: Entity<TextInput>,
    color: Entity<TextInput>,
    mode: usize,
    result: Option<Output>,
    image: Option<Arc<RenderImage>>,
    retired: Vec<Arc<RenderImage>>,
    status: String,
    failed: bool,
    input_revision: u64,
    job_revision: u64,
    result_revision: u64,
    result_dirty: bool,
    screen_feedback: ScreenFeedback,
    job: Option<(CancelToken, mpsc::Receiver<anyhow::Result<WorkResult>>)>,
    screen: Option<mpsc::Receiver<anyhow::Result<crate::screen_tools::Picked>>>,
    borderless: Arc<std::sync::Mutex<Vec<rcr_toolkit::window_tools::Borderless>>>,
}
impl EventEmitter<UtilityEvent> for Utilities {}
impl Utilities {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let text = cx.new(|cx| TextInput::new(cx, "#33CC99"));
        let a = cx.new(|cx| TextInput::new(cx, "32"));
        let b = cx.new(|cx| TextInput::new(cx, "16"));
        let color = cx.new(|cx| TextInput::new(cx, "#20352D"));
        text.update(cx, |v, cx| v.set_placeholder("#RRGGBB or #RRGGBBAA", cx));
        color.update(cx, |v, cx| v.set_placeholder("#RRGGBB or #RRGGBBAA", cx));
        for input in [&a, &b] {
            input.update(cx, |v, cx| v.set_placeholder("Whole number", cx));
        }
        for input in [&text, &a, &b, &color] {
            cx.observe(input, |this, _, cx| this.input_changed(cx))
                .detach();
        }
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                if view.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            tool: Tool::Color,
            files: vec![],
            output: None,
            text,
            a,
            b,
            color,
            mode: 0,
            result: None,
            image: None,
            retired: vec![],
            status: "Ready".into(),
            failed: false,
            input_revision: 0,
            job_revision: 0,
            result_revision: 0,
            result_dirty: false,
            screen_feedback: ScreenFeedback::Quiet,
            job: None,
            screen: None,
            borderless: Arc::new(std::sync::Mutex::new(vec![])),
        }
    }
    pub fn choose_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        if self.job.is_some() || self.screen.is_some() {
            return;
        }
        self.tool = tool;
        // Explicitly invalidate prompts even for A → B → A, independent of
        // deferred TextInput observers or whether the defaults actually change.
        self.input_revision = self.input_revision.wrapping_add(1);
        self.mode = 0;
        self.output = None;
        self.result = None;
        self.result_revision = self.result_revision.wrapping_add(1);
        self.result_dirty = false;
        if let Some(image) = self.image.take() {
            self.retired.push(image);
        }
        let (a, b) = match tool {
            Tool::Split => ("2", "2"),
            Tool::Thumbnail => ("1280", "720"),
            Tool::Beautify => ("48", "16"),
            Tool::Index => ("3", "0"),
            Tool::VideoThumbnail => ("0", "1280"),
            Tool::VideoConvert => ("15", "720"),
            Tool::Effects => ("10", "0"),
            _ => ("0", "0"),
        };
        self.a.update(cx, |v, cx| v.set_value(a, cx));
        self.b.update(cx, |v, cx| v.set_value(b, cx));
        self.text.update(cx, |v, cx| {
            v.set_value(if tool == Tool::Color { "#33CC99" } else { "" }, cx);
            v.set_placeholder(
                match tool {
                    Tool::Hash => "Expected digest (optional)",
                    Tool::Color => "#RRGGBB or #RRGGBBAA",
                    _ => "Text or URL",
                },
                cx,
            );
        });
        self.status = "Ready".into();
        self.failed = false;
        cx.notify();
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let screen = self.screen.as_ref().map(|rx| rx.try_recv());
        if let Some(Ok(result)) = screen {
            self.screen = None;
            match result {
                Ok(picked) => {
                    if let Some(frame) = picked.image {
                        if matches!(
                            picked.mode,
                            crate::screen_tools::Mode::Ocr | crate::screen_tools::Mode::Qr
                        ) {
                            self.screen_feedback = ScreenFeedback::Completion;
                            let qr = picked.mode == crate::screen_tools::Mode::Qr;
                            self.job(
                                move |cancel| {
                                    Ok(message(utilities::extract_text(&frame, qr, &cancel)?))
                                },
                                cx,
                            );
                        } else {
                            match crate::screen_tools::pin(Arc::new(frame), cx) {
                                Ok(_) => {
                                    self.status = "Pinned to screen".into();
                                    self.failed = false;
                                }
                                Err(e) => {
                                    self.status = e.to_string();
                                    self.failed = true;
                                }
                            }
                        }
                    } else {
                        let text = picked.text;
                        let copy_text = if text.starts_with('#') {
                            text.lines().next().unwrap_or(&text).to_owned()
                        } else {
                            text.clone()
                        };
                        if let Some(old) = self.image.take() {
                            self.retired.push(old);
                        }
                        self.result = Some(message(text.clone()));
                        self.result_revision = self.result_revision.wrapping_add(1);
                        self.side_job(
                            move |_| {
                                services::SystemClipboard::new(Default::default())
                                    .set_text(&copy_text)?;
                                Ok(())
                            },
                            "Copied",
                            cx,
                        );
                    }
                }
                Err(e) => {
                    self.status = e.to_string();
                    self.failed = true;
                }
            }
            // Acquisition/pin failures are terminal. OCR and clipboard work
            // retain the screen-origin policy until their follow-up finishes.
            if self.job.is_none() && self.screen_feedback.finish(self.failed, false) {
                cx.emit(UtilityEvent::Reveal);
            }
            cx.notify();
        } else if matches!(screen, Some(Err(mpsc::TryRecvError::Disconnected))) {
            self.screen = None;
            self.status = "Cancelled".into();
            self.failed = false;
            self.screen_feedback.finish(false, true);
            cx.notify();
        }
        let result = self.job.as_ref().and_then(|(_, rx)| match rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("Utility worker stopped")))
            }
            Err(_) => None,
        });
        if let Some(result) = result {
            let cancelled = self
                .job
                .as_ref()
                .is_some_and(|(cancel, _)| cancel.is_cancelled());
            self.job = None;
            match result {
                Ok(WorkResult::Replace(mut out)) => {
                    if let Some(old) = self.image.take() {
                        self.retired.push(old);
                    }
                    if let Some((w, h, bytes)) = out.preview.take() {
                        self.image = render_image(w, h, bytes);
                    }
                    self.failed = false;
                    self.status = format!("{} complete", self.tool.name());
                    self.result_dirty = self.input_revision != self.job_revision;
                    self.result = Some(out);
                    self.result_revision = self.result_revision.wrapping_add(1);
                }
                Ok(WorkResult::Notice(text)) => {
                    self.status = text;
                    self.failed = false;
                }
                Err(e) => {
                    self.status = format!("{e:#}");
                    self.failed = true;
                }
            };
            cx.notify();
            if self.screen_feedback.finish(self.failed, cancelled) {
                cx.emit(UtilityEvent::Reveal);
            }
        }
    }
    fn job(
        &mut self,
        work: impl FnOnce(CancelToken) -> anyhow::Result<Output> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.start(move |cancel| work(cancel).map(WorkResult::Replace), cx);
    }
    fn start(
        &mut self,
        work: impl FnOnce(CancelToken) -> anyhow::Result<WorkResult> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.job.is_some() || self.screen.is_some() {
            return;
        }
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let (tx, rx) = mpsc::channel();
        let spawn = std::thread::Builder::new()
            .name("rcr-utility".into())
            .spawn(move || {
                let result = rcr_toolkit::utilities::with_pool(move || work(token)).and_then(|r| r);
                let _ = tx.send(result);
            });
        match spawn {
            Ok(_) => {
                self.job = Some((cancel, rx));
                self.job_revision = self.input_revision;
                self.status = "Working…".into();
                self.failed = false;
            }
            Err(e) => {
                self.status = e.to_string();
                self.failed = true;
            }
        };
        cx.notify();
    }
    fn input_changed(&mut self, cx: &mut Context<Self>) {
        self.input_revision = self.input_revision.wrapping_add(1);
        self.result_dirty = self.result.is_some();
        if self.job.is_none() && self.screen.is_none() {
            self.status = "Ready".into();
            self.failed = false;
        }
        cx.notify();
    }
    fn input_problem(&self, cx: &App) -> Option<String> {
        if self.tool.accepts_files() || self.tool == Tool::Index {
            if self.files.is_empty() {
                return Some(
                    if self.tool == Tool::Index {
                        "Choose a directory to index."
                    } else {
                        "Choose an input file to continue."
                    }
                    .into(),
                );
            }
            if self.files.len() > 1 && !matches!(self.tool, Tool::Combine | Tool::Hash) {
                return Some("This tool processes one file at a time. Remove the extra inputs or choose another file.".into());
            }
            if self.tool == Tool::Combine && self.files.len() < 2 {
                return Some("Choose at least two images to combine.".into());
            }
        }
        if matches!(self.tool, Tool::Split | Tool::VideoConvert) && self.output.is_none() {
            return Some(
                if self.tool == Tool::Split {
                    "Choose an output directory for the image tiles."
                } else {
                    "Choose a new output filename before converting."
                }
                .into(),
            );
        }
        if self.tool == Tool::QrEncode && self.text.read(cx).value().trim().is_empty() {
            return Some("Enter the text or URL for your QR code.".into());
        }
        None
    }
    fn run(&mut self, cx: &mut Context<Self>) {
        if self.job.is_some() || self.screen.is_some() {
            return;
        }
        if let Some(problem) = self.input_problem(cx) {
            self.status = problem;
            self.failed = true;
            cx.notify();
            return;
        }
        // Hidden fields must not prevent an unrelated tool from running.
        let parameters = if let Some((a_label, b_label)) = self.fields() {
            let parse = |input: &Entity<TextInput>, label: &str| {
                input
                    .read(cx)
                    .value()
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| format!("{label}: enter a nonnegative whole number."))
            };
            parse(&self.a, a_label).and_then(|a| {
                let b = b_label.map_or(Ok(0), |label| parse(&self.b, label))?;
                if matches!(self.tool, Tool::Thumbnail | Tool::Split) && (a == 0 || b == 0) {
                    return Err("Width, height, rows and columns must be greater than zero.".into());
                }
                Ok((a, b))
            })
        } else {
            Ok((0, 0))
        };
        let (a, b) = match parameters {
            Ok(values) => values,
            Err(problem) => {
                self.status = problem;
                self.failed = true;
                cx.notify();
                return;
            }
        };
        let request = Request {
            tool: self.tool,
            files: self.files.clone(),
            text: self.text.read(cx).value(),
            mode: self.mode,
            a,
            b,
            color: self.color.read(cx).value(),
            output: self.output.clone(),
        };
        self.job(move |cancel| utilities::run(request, &cancel), cx);
    }
    pub fn screen_tool(
        &mut self,
        mode: crate::screen_tools::Mode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.job.is_none() && self.screen.is_none() {
            self.screen_feedback = ScreenFeedback::Failure;
            self.screen = Some(crate::screen_tools::launch(mode, window, cx));
            self.status = "Screen tool active".into();
            cx.notify();
        }
    }
    fn choose_input(&mut self, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() || self.screen.is_some() {
            return;
        }
        let ticket = self.dialog_ticket(false);
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple: !folder && matches!(self.tool, Tool::Combine | Tool::Hash),
            prompt: Some(
                if folder {
                    "Choose folder"
                } else {
                    "Choose input files"
                }
                .into(),
            ),
        });
        cx.spawn_in(window, async move |view, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = view.update(cx, |this, cx| {
                    this.complete_input(ticket, paths, cx);
                });
            }
        })
        .detach();
    }
    fn choose_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() || self.screen.is_some() {
            return;
        }
        let ticket = self.dialog_ticket(false);
        if self.tool == Tool::Split {
            let paths = cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some("Choose output directory".into()),
            });
            cx.spawn_in(window, async move |view, cx| {
                if let Ok(Ok(Some(paths))) = paths.await {
                    let _ = view.update(cx, |this, cx| {
                        this.complete_output(ticket, paths.into_iter().next(), cx);
                    });
                }
            })
            .detach();
        } else {
            let name = match self.mode {
                0 => "converted.mp4",
                1 => "converted.webm",
                _ => "converted.gif",
            };
            let path =
                cx.prompt_for_new_path(&std::env::current_dir().unwrap_or_default(), Some(name));
            cx.spawn_in(window, async move |view, cx| {
                if let Ok(Ok(Some(path))) = path.await {
                    let _ = view.update(cx, |this, cx| {
                        this.complete_output(ticket, Some(path), cx);
                    });
                }
            })
            .detach();
        }
    }
    fn save_result(&mut self, image: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() || self.screen.is_some() {
            return;
        }
        let Some(result) = &self.result else {
            return;
        };
        let frame = if image { result.image.clone() } else { None };
        if image && frame.is_none() {
            return;
        }
        let text = result.text.clone();
        let ticket = self.dialog_ticket(true);
        let default = if image {
            "image.png"
        } else {
            match (self.tool, self.mode) {
                (Tool::Index, 1) => "index.html",
                (Tool::Index, 2) => "index.json",
                (Tool::Index, 3) => "index.xml",
                _ => "result.txt",
            }
        };
        let path =
            cx.prompt_for_new_path(&std::env::current_dir().unwrap_or_default(), Some(default));
        cx.spawn_in(window, async move |view, cx| {
            if let Ok(Ok(Some(path))) = path.await {
                let _ = view.update(cx, |this, cx| {
                    this.complete_save(ticket, path, frame, text, cx);
                });
            }
        })
        .detach();
    }
    fn dialog_ticket(&self, result: bool) -> DialogTicket {
        DialogTicket { tool: self.tool, input_revision: self.input_revision,
            result_revision: result.then_some(self.result_revision) }
    }
    fn dialog_current(&self, ticket: DialogTicket) -> bool {
        ticket.current(self.tool, self.input_revision, self.result.as_ref().map(|_| self.result_revision),
            self.job.is_some() || self.screen.is_some())
    }
    fn complete_input(&mut self, ticket: DialogTicket, paths: Vec<PathBuf>, cx: &mut Context<Self>) -> bool {
        if !self.dialog_current(ticket) { return false; }
        self.files = paths;
        self.input_changed(cx);
        true
    }
    fn complete_output(&mut self, ticket: DialogTicket, path: Option<PathBuf>, cx: &mut Context<Self>) -> bool {
        if !self.dialog_current(ticket) { return false; }
        self.output = path;
        self.input_changed(cx);
        true
    }
    fn complete_save(&mut self, ticket: DialogTicket, path: PathBuf, frame: Option<Arc<Frame>>, text: String, cx: &mut Context<Self>) -> bool {
        if !self.dialog_current(ticket) { return false; }
        self.side_job(move |cancel| {
            cancel.check()?;
            if let Some(frame) = frame { utilities::save_image(&path, &frame)?; }
            else { utilities::save_new(&path, text.as_bytes())?; }
            Ok(())
        }, "Saved", cx);
        true
    }
    fn copy(&mut self, image: bool, cx: &mut Context<Self>) {
        if self.job.is_some() {
            return;
        }
        let Some(result) = &self.result else {
            return;
        };
        let frame = if image { result.image.clone() } else { None };
        let text = result.text.clone();
        self.side_job(
            move |cancel| {
                cancel.check()?;
                let clipboard = services::SystemClipboard::new(Default::default());
                if let Some(f) = frame {
                    clipboard.set_image(&f)?;
                } else {
                    clipboard.set_text(&text)?;
                }
                Ok(())
            },
            "Copied",
            cx,
        );
    }
    fn side_job(
        &mut self,
        work: impl FnOnce(CancelToken) -> anyhow::Result<()> + Send + 'static,
        label: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.job.is_some() {
            return;
        }
        self.start(
            move |c| {
                work(c)?;
                Ok(WorkResult::Notice(label.into()))
            },
            cx,
        );
    }
    fn modes(&self) -> &'static [&'static str] {
        match self.tool {
            Tool::Effects => &[
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
                "Round corners",
                "Border",
                "Shadow",
            ],
            Tool::Combine => &["Vertical", "Horizontal"],
            Tool::Index => &["Text", "HTML", "JSON", "XML"],
            Tool::VideoConvert => &["MP4", "WebM", "GIF"],
            _ => &[],
        }
    }
    fn fields(&self) -> Option<(&'static str, Option<&'static str>)> {
        match self.tool {
            Tool::Beautify => Some(("Padding (px)", Some("Corner radius (px)"))),
            Tool::Combine => Some(("Gap (px)", None)),
            Tool::Split => Some(("Columns", Some("Rows"))),
            Tool::Thumbnail => Some(("Maximum width", Some("Maximum height"))),
            Tool::VideoThumbnail => Some(("Timestamp (seconds)", Some("Maximum width"))),
            Tool::VideoConvert if self.mode == 2 => {
                Some(("Frames per second", Some("Maximum width")))
            }
            Tool::Effects if (3..=13).contains(&self.mode) => Some((
                match self.mode {
                    3 => "Blur sigma (1–100)",
                    4 => "Block size (1–256)",
                    5 => "Strength ×10",
                    6 | 7 => "Amount (0–200, 100 neutral)",
                    8 => "Saturation (0–300%, 100 neutral)",
                    9 => "Hue rotation (0–360°)",
                    10 => "Gamma (1–400, 100 neutral)",
                    11 => "Threshold (0–255)",
                    12 => "Radius (px)",
                    13 => "Border width (px)",
                    _ => "Amount",
                },
                None,
            )),
            Tool::Index => Some(("Maximum depth (0 = all)", None)),
            _ => None,
        }
    }
    /// Exercise real completion handling using owned channels only. Never
    /// launch a screen capture, clipboard operation, or utility subprocess.
    pub fn smoke_screen_feedback(
        &mut self,
        stage: usize,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.job.is_none() && self.screen.is_none(),
            "Synthetic feedback fixture found live work"
        );
        match stage {
            0 | 1 => {
                self.screen_feedback = ScreenFeedback::Failure;
                let (tx, rx) = mpsc::channel();
                if stage == 0 {
                    tx.send(Err(anyhow::anyhow!("Synthetic screen acquisition failure")))?;
                }
                drop(tx); // Empty disconnected screen channel is ordinary Esc cancellation.
                self.screen = Some(rx);
            }
            2..=4 => {
                self.screen_feedback = if stage == 4 {
                    ScreenFeedback::Quiet
                } else {
                    ScreenFeedback::Failure
                };
                let (tx, rx) = mpsc::channel();
                let result = if stage == 3 {
                    Ok(WorkResult::Notice("Copied".into()))
                } else {
                    Err(anyhow::anyhow!("Synthetic clipboard follow-up failure"))
                };
                tx.send(result)?;
                self.job = Some((CancelToken::new(), rx));
            }
            _ => {
                self.status = "Ready".into();
                self.failed = false;
                return Ok(());
            }
        }
        self.poll(cx);
        anyhow::ensure!(
            self.job.is_none() && self.screen.is_none(),
            "Synthetic completion did not drain"
        );
        anyhow::ensure!(
            self.failed == matches!(stage, 0 | 2 | 4),
            "Incorrect synthetic failure state"
        );
        if stage == 1 {
            anyhow::ensure!(self.status == "Cancelled", "Esc was reported as failure");
        }
        if stage == 3 {
            anyhow::ensure!(
                self.status == "Copied",
                "Successful clipboard completion lost status"
            );
        }
        Ok(())
    }
    /// Exercise prompt completion and the real QR image follow-up using only
    /// generated pixels. No OS dialog, clipboard, capture, or file write runs.
    pub fn smoke_dialogs(&mut self, stage: &mut u8, cx: &mut Context<Self>) -> anyhow::Result<bool> {
        const PAYLOAD: &str = "RCR synthetic dialog admission proof";
        if *stage == 0 {
            anyhow::ensure!(self.job.is_none() && self.screen.is_none(), "Dialog proof found live work");
            self.choose_tool(Tool::QrDecode, cx);
            self.result = Some(message("Previous result"));
            self.result_revision = self.result_revision.wrapping_add(1);
            let input = self.dialog_ticket(false);
            let save = self.dialog_ticket(true);
            let original_files = self.files.clone();
            self.choose_tool(Tool::Hash, cx);
            self.choose_tool(Tool::QrDecode, cx);
            anyhow::ensure!(!self.complete_input(input, vec![PathBuf::from("stale-input")], cx)
                && !self.complete_output(input, Some(PathBuf::from("stale-output")), cx)
                && !self.complete_save(save, PathBuf::new(), None, "stale".into(), cx),
                "An A → B → A tool switch accepted a stale prompt result");
            anyhow::ensure!(self.files == original_files && self.output.is_none() && self.job.is_none(),
                "Stale dialog mutated the new tool or started work");
            self.result = Some(message("First result"));
            self.result_revision = self.result_revision.wrapping_add(1);
            let previous_result = self.dialog_ticket(true);
            self.result = Some(message("New result, unchanged inputs"));
            self.result_revision = self.result_revision.wrapping_add(1);
            anyhow::ensure!(!self.complete_save(previous_result, PathBuf::new(), None, "stale".into(), cx),
                "A replaced result's pending save was admitted");
            let pending_save = self.dialog_ticket(true);
            let (tx, rx) = mpsc::channel();
            self.screen = Some(rx);
            self.screen_feedback = ScreenFeedback::Failure;
            anyhow::ensure!(!self.complete_save(pending_save, PathBuf::new(), None, "stale".into(), cx),
                "A late save stole the active screen tool's worker slot");
            self.start(|_| panic!("Ordinary work admitted during screen acquisition"), cx);
            anyhow::ensure!(self.job.is_none(), "Screen acquisition allowed competing work");
            use rcr_toolkit::core::workflow::{QrCodeRenderer, QrRenderer};
            tx.send(Ok(crate::screen_tools::Picked { mode: crate::screen_tools::Mode::Qr,
                text: String::new(), image: Some(QrCodeRenderer.render(PAYLOAD)?) }))?;
            self.poll(cx);
            anyhow::ensure!(self.screen.is_none(), "Synthetic QR selection was not consumed");
            *stage = 1;
            return Ok(false);
        }
        self.poll(cx);
        if self.job.is_some() { return Ok(false); }
        anyhow::ensure!(!self.failed && self.result.as_ref().is_some_and(|result| result.text == PAYLOAD),
            "Selected QR pixels lost their follow-up after a stale save: {}", self.status);
        *stage = 2;
        Ok(true)
    }
    pub fn smoke(&mut self, stage: usize, cx: &mut Context<Self>) -> bool {
        if stage < Tool::ALL.len() {
            self.choose_tool(Tool::ALL[stage], cx);
            if self.tool == Tool::Color {
                self.result = Some(Output {
                    text: utilities::color_text([12, 34, 56, 255]),
                    image: Some(Arc::new(rcr_toolkit::effects::solid_frame(
                        240,
                        120,
                        [12, 34, 56, 255],
                    ))),
                    preview: None,
                    files: vec![],
                });
                self.result_revision = self.result_revision.wrapping_add(1);
                self.image = render_image(240, 120, [56u8, 34, 12, 255].repeat(240 * 120));
            }
            if self.tool == Tool::Windows {
                self.result = Some(message(
                    serde_json::to_string(&vec![rcr_toolkit::window_tools::Info {
                        handle: 0,
                        process: 0,
                        title: "Synthetic fixture".into(),
                        class: "fixture".into(),
                        x: 0,
                        y: 0,
                        width: 300,
                        height: 200,
                    }])
                    .unwrap(),
                ));
                self.result_revision = self.result_revision.wrapping_add(1);
            }
            true
        } else {
            false
        }
    }
    fn borderless(&mut self, info: rcr_toolkit::window_tools::Info, cx: &mut Context<Self>) {
        let saved = self.borderless.clone();
        self.side_job(
            move |cancel| {
                cancel.check()?;
                let mut saved = saved.lock().unwrap_or_else(|e| e.into_inner());
                if saved.iter().any(|s| s.handle() == info.handle) {
                    return Ok(());
                }
                saved.push(rcr_toolkit::window_tools::Borderless::apply(&info)?);
                Ok(())
            },
            "Window borders removed · use Restore windows to undo",
            cx,
        );
    }
    fn restore_windows(&mut self, cx: &mut Context<Self>) {
        let saved = self.borderless.clone();
        self.side_job(
            move |_| {
                let mut saved = saved.lock().unwrap_or_else(|e| e.into_inner());
                for s in saved.iter_mut() {
                    s.restore()?;
                }
                saved.clear();
                Ok(())
            },
            "Window styles restored",
            cx,
        );
    }
}
fn message(text: impl Into<String>) -> Output {
    Output {
        text: text.into(),
        image: None,
        preview: None,
        files: vec![],
    }
}
impl Render for Utilities {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for old in self.retired.drain(..) {
            let _ = window.drop_image(old);
        }
        let busy = self.job.is_some() || self.screen.is_some();
        let narrow = window.viewport_size().width < px(850.);
        let mut nav = div()
            .id("utility-sidebar")
            .max_h(px(
                (f32::from(window.viewport_size().height) - 210.).max(260.)
            ))
            .overflow_y_scroll()
            .w(px(204.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_1()
            .when(narrow, |s| s.w_full().flex_row().flex_wrap());
        for (label, tools) in [
            (
                "IMAGES",
                &[
                    Tool::Viewer,
                    Tool::Beautify,
                    Tool::Effects,
                    Tool::Combine,
                    Tool::Split,
                    Tool::Thumbnail,
                    Tool::Convert,
                ][..],
            ),
            ("VIDEO", &[Tool::VideoConvert, Tool::VideoThumbnail][..]),
            (
                "TEXT & DATA",
                &[
                    Tool::Ocr,
                    Tool::QrEncode,
                    Tool::QrDecode,
                    Tool::Color,
                    Tool::Hash,
                    Tool::Index,
                ][..],
            ),
            ("SYSTEM", &[Tool::Clipboard, Tool::Windows][..]),
        ] {
            nav = nav.child(
                muted(label)
                    .text_size(px(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .mt_3()
                    .mb_1()
                    .px_2()
                    .when(narrow, |s| s.w_full()),
            );
            for tool in tools.iter().copied() {
                let selected = self.tool == tool;
                let index = Tool::ALL.iter().position(|t| *t == tool).unwrap_or(0);
                nav = nav.child(
                    button(
                        ("utility-tool", index),
                        if tool == Tool::Windows {
                            "Window inspector"
                        } else {
                            tool.name()
                        },
                    )
                    .justify_start()
                    .when(narrow, |s| s.h(px(28.)).text_size(px(12.)))
                    .bg(rgb(if selected { 0x173025 } else { BG }))
                    .text_color(rgb(if selected { TEXT } else { MUTED }))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_tool(tool, cx))),
                );
            }
        }
        let mut content = div()
            .flex_1()
            .min_w_0()
            .when(narrow, |s| s.w_full())
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_size(px(22.)).child(self.tool.name()))
            .child(muted(self.tool.description()));
        nav = nav
            .child(
                muted("SCREEN TOOLS")
                    .text_size(px(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .mt_3()
                    .mb_1()
                    .px_2()
                    .when(narrow, |s| s.w_full()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .when(narrow, |s| s.flex_row().flex_wrap())
                    .gap_1()
                    .child(
                        button("screen-color", "Screen color picker").on_click(cx.listener(
                            |this, _, w, cx| {
                                this.screen_tool(crate::screen_tools::Mode::Color, w, cx)
                            },
                        )),
                    )
                    .child(button("screen-ruler", "Ruler").on_click(cx.listener(
                        |this, _, w, cx| this.screen_tool(crate::screen_tools::Mode::Ruler, w, cx),
                    )))
                    .child(
                        button("screen-pin-region", "Pin region").on_click(cx.listener(
                            |this, _, w, cx| {
                                this.screen_tool(crate::screen_tools::Mode::Pin, w, cx)
                            },
                        )),
                    )
                    .child(
                        button("screen-ocr-region", "OCR region").on_click(cx.listener(
                            |this, _, w, cx| {
                                this.screen_tool(crate::screen_tools::Mode::Ocr, w, cx)
                            },
                        )),
                    )
                    .child(
                        button("screen-qr-region", "Read QR from screen").on_click(cx.listener(
                            |this, _, w, cx| this.screen_tool(crate::screen_tools::Mode::Qr, w, cx),
                        )),
                    )
                    .child(
                        button("screen-monitor-test", "Monitor test").on_click(cx.listener(
                            |this, _, _, cx| {
                                if let Err(e) = crate::screen_tools::monitor(cx) {
                                    this.status = e.to_string();
                                    this.failed = true;
                                    cx.notify();
                                }
                            },
                        )),
                    ),
            );
        if self.tool.accepts_files() || self.tool == Tool::Index {
            content = content.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "utility-input",
                            if self.tool == Tool::Index {
                                "Choose directory"
                            } else {
                                if matches!(self.tool, Tool::Combine | Tool::Hash) {
                                    "Choose files"
                                } else {
                                    "Choose file"
                                }
                            },
                        )
                        .on_click(cx.listener(|this, _, w, cx| {
                            this.choose_input(this.tool == Tool::Index, w, cx)
                        })),
                    )
                    .child(button("utility-clear-files", "Clear").on_click(cx.listener(
                        |this, _, _, cx| {
                            if this.job.is_none() {
                                this.files.clear();
                                this.input_changed(cx);
                            }
                        },
                    ))),
            );
            for (i, path) in self.files.iter().enumerate() {
                let name = path.to_string_lossy().into_owned();
                content = content.child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    mono(
                                        path.file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                            .into_owned(),
                                    )
                                    .truncate(),
                                )
                                .child(muted(name).truncate()),
                        )
                        .child(button(("utility-file-up", i), "↑").on_click(cx.listener(
                            move |this, _, _, cx| {
                                if this.job.is_none() && i > 0 {
                                    this.files.swap(i, i - 1);
                                    this.input_changed(cx);
                                }
                            },
                        )))
                        .when(i + 1 < self.files.len(), |s| {
                            s.child(button(("utility-file-down", i), "↓").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if this.job.is_none() && i + 1 < this.files.len() {
                                        this.files.swap(i, i + 1);
                                        this.input_changed(cx);
                                    }
                                },
                            )))
                        })
                        .child(
                            button(("utility-file-remove", i), "×").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if this.job.is_none() {
                                        this.files.remove(i);
                                        this.input_changed(cx);
                                    }
                                },
                            )),
                        ),
                );
            }
        }
        if matches!(self.tool, Tool::Color | Tool::QrEncode | Tool::Hash) {
            content = content
                .child(muted(match self.tool {
                    Tool::Color => "Hex color",
                    Tool::Hash => "Expected hash (optional)",
                    _ => "Text or URL",
                }))
                .child(self.text.clone());
        }
        let mut modes = div().flex().flex_wrap().gap_2();
        for (i, name) in self.modes().iter().enumerate() {
            modes = modes.child(
                button(("utility-mode", i), *name)
                    .bg(rgb(if self.mode == i { 0x173025 } else { SOFT }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.job.is_none() {
                            this.mode = i;
                            if this.tool == Tool::Effects {
                                let value = match i {
                                    6 | 7 | 8 | 10 => "100",
                                    9 => "90",
                                    11 => "128",
                                    12 => "16",
                                    13 => "2",
                                    _ => "10",
                                };
                                this.a.update(cx, |v, cx| v.set_value(value, cx));
                            }
                            this.output = None;
                            this.input_changed(cx);
                        }
                    })),
            );
        }
        content = content.child(modes);
        if let Some((a, b)) = self.fields() {
            let mut fields = div()
                .flex()
                .gap_3()
                .child(div().w(px(240.)).child(muted(a)).child(self.a.clone()));
            if let Some(b) = b {
                fields = fields.child(div().w(px(240.)).child(muted(b)).child(self.b.clone()));
            }
            content = content.child(fields.flex_wrap());
        }
        if matches!(self.tool, Tool::Beautify | Tool::Combine)
            || self.tool == Tool::Effects && self.mode == 13
        {
            content = content.child(
                div()
                    .w(px(260.))
                    .child(muted("Background / border #RRGGBB[AA]"))
                    .child(self.color.clone()),
            );
        }
        if matches!(self.tool, Tool::Split | Tool::VideoConvert) {
            content = content
                .child(
                    button("utility-output", "Choose output…")
                        .on_click(cx.listener(|this, _, w, cx| this.choose_output(w, cx))),
                )
                .child(mono(
                    self.output
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "Choose a new filename / folder before running".into()),
                ));
        }
        let input_problem = self.input_problem(cx);
        let run_label = match self.tool {
            Tool::Color => "Convert color",
            Tool::Viewer | Tool::Convert => "Open image",
            Tool::Beautify => "Beautify image",
            Tool::Effects => "Apply effect",
            Tool::Combine => "Combine images",
            Tool::Split => "Split image",
            Tool::Thumbnail => "Create thumbnail",
            Tool::VideoConvert => "Convert video",
            Tool::VideoThumbnail => "Extract frame",
            Tool::Ocr => "Extract text",
            Tool::QrEncode => "Create QR code",
            Tool::QrDecode => "Read QR code",
            Tool::Hash => "Calculate hashes",
            Tool::Index => "Create index",
            Tool::Clipboard => "Read clipboard",
            Tool::Windows => "Refresh windows",
        };
        content = content.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .child(
                    button("utility-run", if busy { "Working…" } else { run_label })
                        .bg(rgb(if busy { SOFT } else { ACCENT }))
                        .text_color(rgb(if busy { MUTED } else { BG }))
                        .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
                )
                .when(self.job.is_some(), |s| {
                    s.child(button("utility-cancel", "Cancel").on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some((c, _)) = &this.job {
                                c.cancel();
                                this.status = "Cancelling…".into();
                                cx.notify();
                            }
                        },
                    )))
                }),
        );
        if self.screen.is_some() {
            content = content.child(muted("Screen tool active · press Esc to cancel"));
        }
        if let Some(problem) = input_problem
            && self.status == "Ready"
        {
            content = content.child(muted(problem));
        } else {
            content = content.child(
                div()
                    .id("utility-status")
                    .max_h(px(110.))
                    .overflow_y_scroll()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .child(
                        muted(self.status.chars().take(4_000).collect::<String>())
                            .text_color(rgb(if self.failed { 0xffa0a0 } else { MUTED })),
                    ),
            );
            if self.failed {
                let details = self.status.clone();
                content = content.child(
                    button("utility-copy-error", "Copy error details").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(details.clone()))
                    }),
                );
            }
        }
        if let Some(out) = &self.result {
            if let Some(path) = out.files.first() {
                let path = path.clone();
                content = content.child(
                    button("utility-reveal-output", "Show in folder")
                        .on_click(move |_, _, cx| cx.reveal_path(&path)),
                );
            }
            if self.tool == Tool::Windows {
                content = content.child(
                    button("restore-window-styles", "Restore windows")
                        .on_click(cx.listener(|this, _, _, cx| this.restore_windows(cx))),
                );
                if let Ok(windows) =
                    serde_json::from_str::<Vec<rcr_toolkit::window_tools::Info>>(&out.text)
                {
                    for (i, info) in windows.into_iter().enumerate() {
                        let action = info.clone();
                        content = content.child(
                            panel()
                                .child(info.title)
                                .child(mono(format!(
                                    "HWND {:X} · PID {} · {} · {}×{} at {},{}",
                                    info.handle,
                                    info.process,
                                    info.class,
                                    info.width,
                                    info.height,
                                    info.x,
                                    info.y
                                )))
                                .child(
                                    button(("borderless-window", i), "Remove borders").on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.borderless(action.clone(), cx)
                                        }),
                                    ),
                                ),
                        );
                    }
                }
            }
            let image = out.image.is_some();
            let mut actions = div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(
                    button("utility-copy-text", "Copy text")
                        .on_click(cx.listener(|this, _, _, cx| this.copy(false, cx))),
                )
                .child(
                    button("utility-save-text", "Save text…")
                        .on_click(cx.listener(|this, _, w, cx| this.save_result(false, w, cx))),
                );
            if image {
                actions = actions
                    .child(
                        button("utility-edit-result", "Edit image").on_click(cx.listener(
                            |this, _, _, cx| {
                                if let Some(frame) =
                                    this.result.as_ref().and_then(|r| r.image.clone())
                                {
                                    cx.emit(UtilityEvent::Edit(frame));
                                }
                            },
                        )),
                    )
                    .child(
                        button("utility-copy-image", "Copy image")
                            .on_click(cx.listener(|this, _, _, cx| this.copy(true, cx))),
                    )
                    .child(
                        button("utility-save-image", "Save image…")
                            .on_click(cx.listener(|this, _, w, cx| this.save_result(true, w, cx))),
                    )
                    .child(button("utility-pin", "Pin to screen").on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(f) = this.result.as_ref().and_then(|r| r.image.clone())
                                && let Err(e) = crate::screen_tools::pin(f, cx)
                            {
                                this.status = e.to_string();
                                this.failed = true;
                                cx.notify();
                            }
                        },
                    )));
            }
            content = content.child(
                div()
                    .mt_3()
                    .pt_3()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(div().mb_2().font_weight(FontWeight::MEDIUM).child(
                        if self.result_dirty {
                            "Previous result · run again to apply your changes"
                        } else {
                            "Result"
                        },
                    ))
                    .child(actions),
            );
            if let Some(image) = self.image.clone() {
                let sz = image.size(0);
                content = content.child(
                    img(image)
                        .max_w_full()
                        .w(px(sz.width.0 as f32))
                        .h(px(sz.height.0 as f32))
                        .object_fit(ObjectFit::Contain),
                );
            }
            let text = out.text.chars().take(32_000).collect::<String>();
            content = content.child(
                div()
                    .id("utility-result-text")
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .p_3()
                    .bg(rgb(PANEL))
                    .rounded_lg()
                    .child(mono(text)),
            );
            if out.text.chars().take(32_001).count() > 32_000 {
                content = content.child(muted("Showing the first 32,000 characters. Copy or save text to get the full result."));
            }
        }
        div()
            .flex()
            .items_start()
            .gap_5()
            .when(narrow, |s| s.flex_col())
            .w_full()
            .child(nav)
            .child(content)
    }
}
impl Drop for Utilities {
    fn drop(&mut self) {
        if let Some((c, _)) = &self.job {
            c.cancel();
        }
    }
}

#[cfg(test)]
mod feedback_tests {
    use super::{DialogTicket, ScreenFeedback};
    use rcr_toolkit::utilities::Tool;

    #[test]
    fn result_dialog_requires_same_tool_inputs_result_and_idle_worker() {
        let ticket = DialogTicket { tool: Tool::QrDecode, input_revision: 17, result_revision: Some(9) };
        assert!(ticket.current(Tool::QrDecode, 17, Some(9), false));
        assert!(!ticket.current(Tool::Hash, 17, Some(9), false));
        assert!(!ticket.current(Tool::QrDecode, 18, Some(9), false));
        assert!(!ticket.current(Tool::QrDecode, 17, Some(10), false));
        assert!(!ticket.current(Tool::QrDecode, 17, None, false));
        assert!(!ticket.current(Tool::QrDecode, 17, Some(9), true));
    }
    #[test]
    fn input_dialog_ignores_result_refresh_but_not_navigation_or_input_changes() {
        let ticket = DialogTicket { tool: Tool::Combine, input_revision: 6, result_revision: None };
        assert!(ticket.current(Tool::Combine, 6, Some(15), false));
        assert!(!ticket.current(Tool::Combine, 8, Some(15), false)); // A → B → A.
        assert!(!ticket.current(Tool::Combine, 6, None, true)); // Screen/job admission.
    }

    #[test]
    fn hidden_screen_failures_reveal_once_without_affecting_later_local_work() {
        let mut policy = ScreenFeedback::Failure;
        assert!(policy.finish(true, false));
        assert!(!policy.finish(true, false));
        let mut policy = ScreenFeedback::Failure;
        assert!(!policy.finish(false, false)); // Color/pin success stays on the desktop.
        assert!(!policy.finish(true, false)); // A later unrelated failure stays in its own UI.
    }

    #[test]
    fn explicit_screen_cancellation_is_quiet_but_ocr_results_still_reveal() {
        let mut policy = ScreenFeedback::Failure;
        assert!(!policy.finish(false, true));
        let mut policy = ScreenFeedback::Completion;
        assert!(policy.finish(false, false));
        let mut policy = ScreenFeedback::Completion;
        assert!(policy.finish(true, false));
        let mut policy = ScreenFeedback::Completion;
        assert!(!policy.finish(true, true)); // Explicit cancellation must not steal navigation.
    }
}
