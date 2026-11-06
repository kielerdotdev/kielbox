//! Unified screenshot, annotation, workflow, destination and upload-history surface.
use crate::{
    image_editor::{ImageEditor, ImageEvent},
    recordings, screenshot_overlay,
    text_input::TextInput,
    theme::*,
};
use gpui::{prelude::*, *};
use ::toolkit::{
    core::{
        settings::{InputKind, Paths},
        workflow::{CaptureTarget, Event as Progress, Outcome, RunReport, StepKind, StepStatus},
    },
    worker::{Action, Event, Snapshot, UiRequest, Worker},
};
use std::{path::PathBuf, sync::mpsc, time::Duration};

#[path = "workflow_ui.rs"]
mod workflow_ui;
#[path = "history_ui.rs"]
mod history_ui;
pub use history_ui::HistorySmoke;

pub enum ToolkitEvent {
    Activate,
    FilesChanged(Vec<PathBuf>),
    Recorder,
}
type CaptureSmoke = (
    WindowHandle<crate::capture_overlay::CaptureOverlay>,
    usize,
    mpsc::Receiver<Option<::toolkit::worker::OverlayResult>>,
    Option<[u8; 32]>,
);
type StandaloneSmoke = (u8, mpsc::Receiver<Option<::toolkit::worker::StandaloneImage>>, Option<[u8; 32]>);
pub struct Toolkit {
    quick_recorder: Entity<crate::quick_recorder::QuickRecorder>,
    capture_extras: bool,
    capture_requests: Vec<(CaptureTarget, ::toolkit::worker::ScreenshotOptions)>,
    capture_probe: Option<crate::quick_recorder::menu_proof::Probe>,
    smoke_image_refs: Vec<std::sync::Weak<RenderImage>>,
    shell_operation: u64,
    history_ui: history_ui::HistoryUi,
    workflow_probe: Option<workflow_ui::LayoutProbe>,
    workflow_busy_override: Option<bool>,
    workflow_focus: std::collections::BTreeMap<usize, FocusHandle>,
    smoke_capture: Option<CaptureSmoke>,
    smoke_standalone: Option<StandaloneSmoke>,
    smoke_utility_dialogs: u8,
    worker: Option<Worker>,
    snapshot: Option<Box<Snapshot>>,
    pane: usize,
    editor: Entity<ImageEditor>,
    utilities: Entity<crate::utilities::Utilities>,
    disk_usage: Entity<crate::disk_usage::DiskUsageView>,
    editing: bool,
    reply: Option<mpsc::SyncSender<Option<::toolkit::types::Frame>>>,
    standalone_reply: Option<mpsc::SyncSender<Option<::toolkit::worker::StandaloneImage>>>,
    edit_cancel: Option<::toolkit::core::workflow::CancelToken>,
    edit: bool,
    upload: bool,
    query: Entity<TextInput>,
    qr: Entity<TextInput>,
    pattern: Entity<TextInput>,
    workflow_index: usize,
    status: String,
    error: Option<String>,
    cancelling: bool,
    capture_in_flight: bool,
    result_path: Option<PathBuf>,
    result_url: Option<String>,
}
impl EventEmitter<ToolkitEvent> for Toolkit {}
impl Toolkit {
    /// Select an existing surface for the explicit native visual fixture. No task runs.
    pub fn snapshot_surface(&mut self, route: &str, cx: &mut Context<Self>) -> bool {
        if route == "tools-history-fixture" { return self.seed_history_fixture(cx); }
        let effect = match route {
            "tools-image-blur" => Some(::toolkit::annotation::Tool::Blur),
            "tools-image-pixelate" => Some(::toolkit::annotation::Tool::Pixelate),
            "tools-image-magnifier" => Some(::toolkit::annotation::Tool::Magnify),
            _ => None,
        };
        if let Some(tool) = effect {
            if !self.editing {
                if let Err(error) = self.editor.update(cx, |editor, cx| editor.seed_property_preview(tool, cx)) {
                    eprintln!("Owned image-property snapshot setup failed: {error:#}");
                    return false;
                }
                self.editing = true;
                cx.notify();
            }
            return self.editor.read(cx).property_preview_ready(tool);
        }
        if route == "tools-image-retry" {
            if !self.editing {
                self.editor.update(cx, |editor, cx| editor.seed_retry_preview(cx));
                self.editing = true;
                cx.notify();
            }
            return self.editor.read(cx).retry_preview_ready();
        }
        if self.snapshot.is_none() {
            return false;
        }
        let pane = match route {
            "tools-workflows" => 1,
            "tools-destinations" => 2,
            "tools-history" => 3,
            "tools-preferences" => 4,
            "tools-utilities" => 5,
            "tools-disk-usage" => 6,
            "tools-disk-usage-map" => 6,
            "tools-disk-usage-files" => 6,
            "tools-disk-usage-explore" | "tools-disk-usage-explore-boxes" => 6,
            _ => 0,
        };
        if pane != self.pane {
            self.pane = pane;
            cx.notify();
        }
        if route == "tools-disk-usage" || route == "tools-disk-usage-map" {
            self.disk_usage.update(cx, |view,cx|view.show_tree(cx));
        }
        if route == "tools-disk-usage-map" {
            self.disk_usage.update(cx, |view, cx| view.show_map(cx));
        }
        if route == "tools-disk-usage-files" {
            self.disk_usage.update(cx, |view, cx| view.show_files(cx));
        }
        if route.starts_with("tools-disk-usage-explore") {
            self.disk_usage.update(cx,|view,cx|view.show_explore(route.ends_with("-boxes"),cx));
        }
        if route.starts_with("tools-disk-usage") {
            return self.disk_usage.read(cx).scan_finished(cx);
        }
        true
    }
    pub fn smoke_step(
        view: Entity<Self>,
        stage: &mut usize,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<bool> {
        if *stage >= 25 { return Self::smoke_standalone(view, window, cx); }
        if view.read(cx).snapshot.is_none() {
            return Ok(false);
        }
        if *stage < 6 {
            // Observe the previous turn's actual UtilityEvent::Reveal routing.
            // Only hidden acquisition/follow-up failures should select Utilities.
            if *stage > 0 {
                let expected = match *stage {
                    1 | 3 => 5,
                    previous => previous - 1,
                };
                anyhow::ensure!(
                    view.read(cx).pane == expected,
                    "Screen utility completion routed to pane {}, expected {expected}",
                    view.read(cx).pane
                );
            }
            view.update(cx, |this, cx| {
                this.pane = *stage;
                cx.notify();
            });
            window.draw(cx).clear();
            if *stage == 1 {
                workflow_ui::smoke(view.clone(), window, cx)?;
            }
            let utilities = view.read(cx).utilities.clone();
            utilities.update(cx, |this, cx| this.smoke_screen_feedback(*stage, cx))?;
            *stage += 1;
            return Ok(false);
        }
        if *stage < 23 {
            let utilities = view.read(cx).utilities.clone();
            utilities.update(cx, |this, cx| {
                this.smoke(*stage - 6, cx);
            });
            window.draw(cx).clear();
            *stage += 1;
            return Ok(false);
        }
        if *stage == 23 {
            if view.read(cx).smoke_utility_dialogs < 2 {
                view.update(cx, |this, cx| {
                    let utilities = this.utilities.clone();
                    utilities.update(cx, |utilities, cx| utilities.smoke_dialogs(&mut this.smoke_utility_dialogs, cx))
                })?;
                return Ok(false);
            }
            crate::screen_tools::smoke(cx)?;
            let (tx, rx) = mpsc::sync_channel(1);
            let overlay = crate::capture_overlay::open(
                ::toolkit::effects::solid_frame(1280, 720, [35, 50, 70, 255]),
                true,
                false,
                tx,
                ::toolkit::core::workflow::CancelToken::new(),
                false,
                cx,
            )?;
            view.update(cx, |this, _| this.smoke_capture = Some((overlay, 0, rx, None)));
            *stage += 1;
            return Ok(false);
        }
        view.update(cx, |this, cx| -> anyhow::Result<bool> {
            use sha2::Digest;
            use ::toolkit::worker::{OverlayAction, OverlayCompletion};
            let (handle, step, rx, digest) = this.smoke_capture.as_mut()
                .ok_or_else(|| anyhow::anyhow!("Missing capture overlay"))?;
            let completed = crate::capture_overlay::CaptureOverlay::SMOKE_STAGES;
            let raw: AnyWindowHandle = (*handle).into();
            if *step == completed + 3 {
                if raw.update(cx, |_, _, _| ()).is_err() {
                    *stage = 25;
                    println!("Overlay smoke: failed Copy retains Actor/edits, real Ctrl+S rerenders identical pixels, successful ACK closes overlay");
                    return Ok(false);
                }
                return Ok(false);
            }
            if let Ok(Some(result)) = rx.try_recv() {
                anyhow::ensure!((result.frame.width(), result.frame.height()) == (result.region.width, result.region.height),
                    "Saved pixels do not match selection");
                let hash: [u8; 32] = sha2::Sha256::digest(result.frame.data()).into();
                if *step == completed {
                    anyhow::ensure!(result.action == OverlayAction::Copy, "Wrong initial overlay action");
                    anyhow::ensure!(result.frame.data() != ::toolkit::effects::solid_frame(
                        result.frame.width(), result.frame.height(), [35, 50, 70, 255]).data(), "Annotation missing from copied crop");
                    *digest = Some(hash);
                    result.completion.try_send(OverlayCompletion::Failed("Synthetic clipboard busy. Your annotated capture remains open.".into()))?;
                    *step = completed + 1;
                } else {
                    anyhow::ensure!(*step == completed + 2 && result.action == OverlayAction::Save, "Wrong retry action");
                    anyhow::ensure!(*digest == Some(hash), "Retry did not preserve the same frozen annotated pixels");
                    result.completion.try_send(OverlayCompletion::Succeeded)?;
                    *step = completed + 3;
                }
                return Ok(false);
            }
            if *step == completed + 1 {
                if raw.update(cx, |root, window, cx| crate::capture_overlay::CaptureOverlay::smoke_recover_retry(
                    root.downcast::<crate::capture_overlay::CaptureOverlay>().expect("Capture root"), window, cx))?? {
                    *step = completed + 2;
                }
            } else if *step < completed {
                let before = *step;
                raw.update(cx, |root, window, cx| crate::capture_overlay::CaptureOverlay::smoke_step(
                    root.downcast::<crate::capture_overlay::CaptureOverlay>().expect("Capture root"), step, window, cx))??;
                if *step != before { println!("Overlay phase {step}"); }
            }
            Ok(false)
        })
    }
    fn smoke_standalone(view: Entity<Self>, window: &mut Window, cx: &mut App) -> anyhow::Result<bool> {
        use sha2::Digest;
        use ::toolkit::worker::StandaloneCompletion;
        if view.read(cx).smoke_standalone.is_none() {
            let (tx, rx) = mpsc::sync_channel(1);
            view.update(cx, |this, cx| -> anyhow::Result<()> {
                this.smoke_standalone = Some((0, rx, None));
                this.standalone_reply = Some(tx);
                this.edit_cancel = Some(::toolkit::core::workflow::CancelToken::new());
                this.editing = true;
                this.editor.update(cx, |editor, cx| editor.smoke_open_standalone(
                    ::toolkit::image_editor::Source::Image(::toolkit::effects::solid_frame(320, 180, [235, 240, 245, 255])), cx))?;
                cx.notify();
                Ok(())
            })?;
            return Ok(false);
        }
        let phase = view.read(cx).smoke_standalone.as_ref().unwrap().0;
        let editor = view.read(cx).editor.clone();
        match phase {
            0 => {
                if ImageEditor::smoke_annotate(editor, window, cx)? {
                    view.update(cx, |this, _| this.smoke_standalone.as_mut().unwrap().0 = 1);
                }
            }
            1 | 3 => {
                if phase == 1 {
                    let images = editor.read(cx).smoke_surface_images();
                    anyhow::ensure!(!images.is_empty(), "Standalone editor had no image tiles to track");
                    view.update(cx, |this, _| this.smoke_image_refs = images);
                }
                if ImageEditor::smoke_finish(editor, window, cx, phase == 3)? {
                    view.update(cx, |this, _| this.smoke_standalone.as_mut().unwrap().0 += 1);
                }
            }
            2 | 4 => {
                let submission = view.update(cx, |this, _| this.smoke_standalone.as_mut().unwrap().1.try_recv().ok().flatten());
                if let Some(submission) = submission {
                    let hash: [u8; 32] = sha2::Sha256::digest(submission.frame.data()).into();
                    if phase == 2 {
                        view.update(cx, |this, _| this.smoke_standalone.as_mut().unwrap().2 = Some(hash));
                        submission.completion.try_send(StandaloneCompletion::Failed {
                            detail: "Synthetic clipboard failure after successful Save".into(),
                            saved_path: Some(PathBuf::from("synthetic-owned-image.png")),
                        })?;
                    } else {
                        anyhow::ensure!(view.read(cx).smoke_standalone.as_ref().unwrap().2 == Some(hash),
                            "Standalone retry lost frozen annotated pixels");
                        submission.completion.try_send(StandaloneCompletion::Succeeded)?;
                    }
                    view.update(cx, |this, _| this.smoke_standalone.as_mut().unwrap().0 += 1);
                }
            }
            _ => {
                if !view.read(cx).editing && editor.read(cx).smoke_closed() {
                    if view.read(cx).smoke_image_refs.iter().any(|image| image.upgrade().is_some()) { return Ok(false); }
                    println!("Standalone editor resources released: all tracked CPU image weak references expired without reopening the image editor");
                    println!("Standalone editor smoke: stale project/insert dialog results rejected after reopen, native annotation/Enter, partial-success ACK retains Actor/path, identical pixel retry, success closes; no clipboard/file output");
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    pub fn new(cx: &mut Context<Self>) -> Self {
        let quick_recorder = cx.new(crate::quick_recorder::QuickRecorder::new);
        cx.subscribe(&quick_recorder, |this, _, _: &crate::quick_recorder::ShowControls, cx| {
            this.pane = 0;
            cx.emit(ToolkitEvent::Activate);
            cx.notify();
        }).detach();
        cx.subscribe(&quick_recorder, |this, _, event: &crate::quick_recorder::Screenshot, cx| {
            if this.capture_probe.is_some() { this.capture_requests.push((event.target, event.options.clone())); return; }
            if !this.submit(Action::Screenshot { target: event.target, edit: this.edit, upload: this.upload, options: Some(event.options.clone()) }, cx) {
                cx.emit(ToolkitEvent::Activate);
                activate_main(cx);
            }
        }).detach();
        let query = cx.new(|cx| TextInput::new(cx, ""));
        query.update(cx, |input, cx| input.set_placeholder("Search filename, link, app or window title…", cx));
        cx.observe(&query, |this, _, cx| this.schedule_history_search(cx)).detach();
        let qr = cx.new(|cx| TextInput::new(cx, ""));
        let pattern = cx.new(|cx| TextInput::new(cx, "Screenshot_%y-%mo-%d_%h-%mi-%s"));
        let editor = cx.new(ImageEditor::new);
        let utilities = cx.new(crate::utilities::Utilities::new);
        let disk_usage = cx.new(crate::disk_usage::DiskUsageView::new);
        cx.subscribe(
            &utilities,
            |this, _, event: &crate::utilities::UtilityEvent, cx| match event {
                crate::utilities::UtilityEvent::Edit(frame) => {
                    this.submit(Action::EditFrame(frame.clone()), cx);
                }
                crate::utilities::UtilityEvent::Reveal => {
                    this.pane = 5;
                    cx.notify();
                    cx.emit(ToolkitEvent::Activate);
                    activate_main(cx);
                }
            },
        )
        .detach();
        cx.subscribe(&editor, |this, _, event: &ImageEvent, cx| {
            match event {
                ImageEvent::Ready => {
                    let Some(frame) = this.editor.update(cx, |editor, _| editor.take_accepted()) else { return; };
                    if let Some(reply) = &this.standalone_reply {
                        let completed = this.editor.update(cx, |editor, cx| editor.completion_channel(cx));
                        if reply.try_send(Some(::toolkit::worker::StandaloneImage { frame, completion: completed })).is_err() {
                            this.editor.update(cx, |editor, cx| editor.finish_failed("The save workflow is no longer available. Your edits remain open.".into(), cx));
                        }
                        cx.notify();
                        return;
                    }
                    if let Some(reply) = this.reply.take() { let _ = reply.try_send(Some(frame)); }
                }
                ImageEvent::Cancel => {
                    if let Some(cancel) = this.edit_cancel.take() { cancel.cancel(); }
                    if let Some(reply) = this.standalone_reply.take() { let _ = reply.try_send(None); }
                    if let Some(reply) = this.reply.take() { let _ = reply.try_send(None); }
                }
                ImageEvent::Finished => {
                    this.standalone_reply.take();
                    this.edit_cancel.take();
                }
            }
            this.editing = false;
            this.editor.update(cx, |editor, cx| editor.close(cx));
            cx.notify();
        })
        .detach();
        let worker = Worker::new(
            Paths::rooted_at("projects/toolkit"),
            recordings::root().canonicalize().unwrap_or_else(|_| {
                std::env::current_dir()
                    .unwrap_or_default()
                    .join(recordings::root())
            }),
        );
        let (worker, error) = match worker {
            Ok(w) => (Some(w), None),
            Err(e) => (None, Some(e.to_string())),
        };
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
            quick_recorder,
            capture_extras: false,
            capture_requests: vec![],
            capture_probe: None,
            smoke_image_refs: Vec::new(),
            shell_operation: 0,
            history_ui: Default::default(),
            workflow_probe: None,
            workflow_busy_override: None,
            workflow_focus: Default::default(),
            smoke_capture: None,
            smoke_standalone: None,
            smoke_utility_dialogs: 0,
            worker,
            snapshot: None,
            pane: 0,
            editor,
            utilities,
            disk_usage,
            editing: false,
            reply: None,
            standalone_reply: None,
            edit_cancel: None,
            edit: true,
            upload: false,
            query,
            qr,
            pattern,
            workflow_index: 0,
            status: "Loading toolkit…".into(),
            error,
            cancelling: false,
            capture_in_flight: false,
            result_path: None,
            result_url: None,
        }
    }
    pub fn quick_recorder(&self) -> Entity<crate::quick_recorder::QuickRecorder> {
        self.quick_recorder.clone()
    }
    pub(crate) fn capture_menu_proof(&mut self, probe: crate::quick_recorder::menu_proof::Probe, cx: &mut Context<Self>) {
        self.quick_recorder.update(cx, |recorder, _| recorder.set_probe(probe.clone()));
        self.capture_probe = Some(probe); self.pane = 0; cx.notify();
    }
    pub(crate) fn capture_requests(&self) -> &[(CaptureTarget, ::toolkit::worker::ScreenshotOptions)] { &self.capture_requests }
    pub(crate) fn capture_menu_ready(&self) -> bool { self.snapshot.is_some() && self.error.is_none() }
    pub fn screen_utility(
        &mut self,
        mode: crate::screen_tools::Mode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pane = 5;
        self.utilities
            .update(cx, |this, cx| this.screen_tool(mode, window, cx));
        cx.notify();
    }
    pub fn utility(&mut self, tool: Option<::toolkit::utilities::Tool>, cx: &mut Context<Self>) {
        self.pane = 5;
        if let Some(tool) = tool {
            self.utilities
                .update(cx, |this, cx| this.choose_tool(tool, cx));
        }
        cx.emit(ToolkitEvent::Activate);
        cx.notify();
    }
    pub fn analyze_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.pane = 6;
        self.disk_usage.update(cx, |view, cx| view.start(path, cx));
        cx.notify();
    }
    fn submit(&mut self, action: Action, cx: &mut Context<Self>) -> bool {
        let capture_in_flight = match &action {
            Action::Screenshot { .. } => true,
            Action::Workflow(id) => self
                .snapshot
                .as_ref()
                .and_then(|snapshot| {
                    snapshot
                        .settings
                        .workflows
                        .iter()
                        .find(|workflow| &workflow.id == id)
                })
                .is_some_and(|workflow| {
                    matches!(
                        workflow.input,
                        InputKind::CaptureRegion
                            | InputKind::CaptureFullscreen
                            | InputKind::CaptureWindow
                            | InputKind::CaptureMonitor
                            | InputKind::CaptureLastRegion
                    )
                }),
            _ => false,
        };
        let status = match &action {
            Action::Screenshot { .. } => "Preparing capture…",
            Action::Files(_) | Action::WorkflowFiles { .. } => "Preparing files…",
            Action::OpenImage(_) | Action::EditFrame(_) | Action::EditClipboard => {
                "Opening editor…"
            }
            Action::Qr(_) => "Creating QR code…",
            Action::SaveSettings(_) => "Saving settings…",
            Action::ImportSettings(_) | Action::ImportUploader(_) => "Importing configuration…",
            Action::Refresh => "Reloading settings…",
            Action::Workflow(_) => "Starting workflow…",
        };
        let result = self
            .worker
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Toolkit worker did not start"))
            .and_then(|w| w.submit(action));
        match result {
            Ok(()) => {
                self.status = status.into();
                self.error = None;
                self.cancelling = false;
                self.capture_in_flight = capture_in_flight;
                self.result_path = None;
                self.result_url = None;
                cx.notify();
                true
            }
            Err(e) => {
                self.error = Some(e.to_string());
                cx.notify();
                false
            }
        }
    }
    fn show_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let pane = self.pane;
        let result_path = self.result_path.clone();
        let history_generation = self.history_generation();
        let previous_error = self.error.clone();
        match recordings::begin_shell_action(recordings::ShellAction::Reveal, path, cx, move |this, operation, _, result, cx| {
            if this.shell_operation != operation { return; }
            this.shell_operation = 0;
            if this.pane == pane && this.result_path == result_path && this.history_generation() == history_generation && this.error == previous_error
                && let Err(error) = result { this.error = Some(error); }
            cx.notify();
        }) {
            Ok(operation) => self.shell_operation = operation,
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
    pub fn share_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.pane = 2;
        self.submit(Action::Files(paths), cx);
    }
    pub fn open_image(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.submit(Action::OpenImage(path), cx);
    }
    pub fn preferences(&mut self, cx: &mut Context<Self>) {
        self.pane = 4;
        cx.notify();
    }
    pub fn capture(&mut self, target: CaptureTarget, cx: &mut Context<Self>) -> bool {
        self.submit(
            Action::Screenshot {
                target,
                edit: self.edit,
                upload: self.upload,
                options: None,
            },
            cx,
        )
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let events = self
            .worker
            .as_ref()
            .map(|w| w.events.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        self.poll_history(cx);
        if events.is_empty() {
            return;
        }
        for event in events {
            match event {
                Event::Ui(UiRequest::Capture {
                    frame,
                    choose_region,
                    configured_action,
                    reply,
                    cancel,
                }) => {
                    if cancel.is_cancelled() {
                        let _ = reply.try_send(None);
                        continue;
                    }
                    let desktop = ::toolkit::types::Rect::new(frame.origin.x, frame.origin.y, frame.width(), frame.height());
                    // Freeze the native target geometry before creating our own
                    // overlay. Enumeration is bounded and never blocks UI input.
                    let (targets_tx, targets_rx) = async_channel::bounded(1);
                    cx.background_executor().spawn(async move {
                        let targets = if choose_region { crate::region_targets::TargetSnapshot::capture(desktop, None) }
                            else { Default::default() };
                        let _ = targets_tx.try_send(targets);
                    }).detach();
                    cx.spawn(async move |view, cx| {
                        // A single slow OS call cannot delay capture indefinitely.
                        // The detached worker retains its admission until it really
                        // returns; a late result cannot replace the displayed frame.
                        let targets = targets_rx.recv().with_timeout(Duration::from_millis(100), cx.background_executor())
                            .await.ok().and_then(Result::ok).unwrap_or_default();
                        if cancel.is_cancelled() {
                            let _ = reply.try_send(None);
                            return;
                        }
                        let _ = view.update(cx, |this, cx| {
                            if let Err(error) = crate::capture_overlay::open_with_targets(
                                frame, choose_region, configured_action, reply, cancel, true, targets, cx,
                            ) {
                                this.error = Some(error.to_string());
                                cx.notify();
                            }
                        });
                    }).detach();
                }
                Event::Snapshot(snapshot) => {
                    // A history refresh must not overwrite a naming pattern being edited.
                    let previous_pattern = self
                        .snapshot
                        .as_ref()
                        .map(|old| old.settings.general.file_name_pattern.as_str());
                    let draft = self.pattern.read(cx).value();
                    if previous_pattern.is_none_or(|previous| previous == draft) {
                        self.pattern.update(cx, |input, cx| {
                            input.set_value(snapshot.settings.general.file_name_pattern.clone(), cx)
                        });
                    }
                    let paths = snapshot
                        .history
                        .iter()
                        .filter_map(|entry| entry.local_path.clone())
                        .collect::<Vec<_>>();
                    // History IDs also detect a new capture that reused a filename.
                    let history_changed = self.snapshot.as_ref().is_none_or(|old| {
                        !old.history
                            .iter()
                            .map(|entry| (entry.id, &entry.local_path))
                            .eq(snapshot
                                .history
                                .iter()
                                .map(|entry| (entry.id, &entry.local_path)))
                    });
                    self.snapshot = Some(snapshot);
                    if history_changed {
                        self.history_changed(cx);
                        cx.emit(ToolkitEvent::FilesChanged(paths));
                    }
                    if self.status.starts_with("Loading") {
                        self.status = "Ready".into();
                    }
                }
                Event::Ui(UiRequest::Edit {
                    source,
                    reply,
                    cancel,
                }) => {
                    if cancel.is_cancelled() {
                        let _ = reply.try_send(None);
                        continue;
                    }
                    self.status = "Annotate your image, then continue or cancel".into();
                    self.editor.update(cx, |editor, cx| editor.open(source, cx));
                    self.reply = Some(reply);
                    self.editing = true;
                    cx.emit(ToolkitEvent::Activate);
                    activate_main(cx);
                }
                Event::Ui(UiRequest::StandaloneEdit { source, reply, cancel }) => {
                    if cancel.is_cancelled() { let _ = reply.try_send(None); continue; }
                    self.status = "Annotate your image, then save or go back".into();
                    self.editor.update(cx, |editor, cx| editor.open_standalone(source, cx));
                    self.standalone_reply = Some(reply);
                    self.edit_cancel = Some(cancel);
                    self.editing = true;
                    cx.emit(ToolkitEvent::Activate);
                    activate_main(cx);
                }
                Event::Ui(UiRequest::Region {
                    frame,
                    reply,
                    cancel,
                }) => {
                    if cancel.is_cancelled() {
                        let _ = reply.try_send(None);
                        continue;
                    }
                    if let Err(e) = screenshot_overlay::open(frame, reply, cancel, cx) {
                        self.error = Some(e.to_string());
                    }
                }
                Event::Ui(UiRequest::Save { suggested, reply }) => {
                    if self.cancelling {
                        let _ = reply.try_send(None);
                        continue;
                    }
                    self.status = "Choose where to save your image".into();
                    activate_main(cx);
                    let selected = cx.prompt_for_new_path(
                        suggested.parent().unwrap_or(std::path::Path::new(".")),
                        suggested.file_name().and_then(|n| n.to_str()),
                    );
                    cx.spawn(async move |_, _| {
                        let path = selected.await.ok().and_then(Result::ok).flatten();
                        let _ = reply.try_send(path);
                    })
                    .detach();
                }
                Event::Progress(progress) => {
                    if self.cancelling {
                        continue;
                    }
                    self.status = match progress {
                        Progress::RunStarted { workflow_name, .. } => workflow_name,
                        Progress::StepStarted { step, .. } => step_status(step).into(),
                        Progress::StepProgress { done, total, .. } => upload_progress(done, total),
                        _ => self.status.clone(),
                    }
                }
                Event::Finished(result) => {
                    self.cancelling = false;
                    self.status = match result {
                        Ok(Some(report)) => {
                            self.result_path = report
                                .items
                                .iter()
                                .rev()
                                .find_map(|item| item.local_path.clone());
                            self.result_url = report.urls().last().map(|url| (*url).to_owned());
                            let summary = completion_status(&report);
                            let failures = report
                                .all_steps()
                                .filter_map(|s| {
                                    if let ::toolkit::core::workflow::StepStatus::Failed(f) =
                                        &s.status
                                    {
                                        Some(f.message.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>();
                            if !failures.is_empty() {
                                self.error = Some(failures.join("\n"));
                            }
                            summary
                        }
                        Ok(None) => "Ready".into(),
                        Err(e) => {
                            self.error = Some(e);
                            "Operation failed".into()
                        }
                    };
                    // Captures started from the app minimize its window. Bring the error
                    // back into view instead of leaving a failed capture invisible.
                    if self.capture_in_flight && self.error.is_some() {
                        cx.emit(ToolkitEvent::Activate);
                        activate_main(cx);
                    }
                    self.capture_in_flight = false;
                    if self.editing && self.standalone_reply.is_none() {
                        self.reply.take();
                        self.editing = false;
                        self.editor.update(cx, |editor, cx| editor.close(cx));
                    }
                }
            }
        }
        // A batch of byte-progress events requires one paint, not one per event.
        cx.notify();
    }
    fn choose_files(&mut self, kind: usize, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = [
            "Open image or .ssxe project",
            "Import ShareX uploader (.sxcu)",
            "Import toolbox settings.toml",
            "Share files",
        ][kind];
        let workflow = if kind == 3 && self.pane == 1 {
            self.snapshot
                .as_ref()
                .and_then(|s| s.settings.workflows.get(self.workflow_index))
                .map(|w| w.id.clone())
        } else {
            None
        };
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: kind == 3,
            multiple: kind == 3,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await {
                let _ = view.update(cx, |this, cx| {
                    if kind == 3 {
                        if let Some(id) = workflow {
                            this.submit(Action::WorkflowFiles { id, paths }, cx);
                        } else {
                            this.share_files(paths, cx);
                        }
                    } else if let Some(path) = paths.into_iter().next() {
                        this.submit(
                            match kind {
                                0 => Action::OpenImage(path),
                                1 => Action::ImportUploader(path),
                                _ => Action::ImportSettings(path),
                            },
                            cx,
                        );
                    }
                });
            }
        })
        .detach();
    }
    fn save(&mut self, settings: ::toolkit::core::settings::Settings, cx: &mut Context<Self>) {
        self.submit(Action::SaveSettings(Box::new(settings)), cx);
    }
    fn home(&self, cx: &mut Context<Self>) -> Div {
        let extras = div().flex().flex_col().gap_4()
            .child(div().flex().flex_wrap().gap_3()
                .child(toggle("edit-screenshot", self.edit).on_click(cx.listener(|this, _, _, cx| { this.edit = !this.edit; cx.notify(); })))
                .child("Annotate captures")
                .child(toggle("share-screenshot", self.upload).on_click(cx.listener(|this, _, _, cx| { this.upload = !this.upload; cx.notify(); })))
                .child("Upload after capture"))
            .child(div().flex().flex_wrap().gap_2()
                .child(button("last-region-tool", "Last area").on_click(cx.listener(|this, _, window, cx| { if this.capture(CaptureTarget::LastRegion, cx) { window.minimize_window(); } })))
                .child(button("desktop-tool", "All displays").on_click(cx.listener(|this, _, window, cx| { if this.capture(CaptureTarget::Fullscreen, cx) { window.minimize_window(); } })))
                .child(button("edit-clipboard-image", "Edit clipboard image").on_click(cx.listener(|this, _, _, cx| { this.submit(Action::EditClipboard, cx); }))))
            .child(panel().child("QR code").child(self.qr.clone())
                .child(button("make-qr", "Create QR code").on_click(cx.listener(|this, _, _, cx| { let text = this.qr.read(cx).value(); if !text.trim().is_empty() { this.submit(Action::Qr(text), cx); } }))));
        div().w_full().min_w_0().flex().flex_col().gap_4().py_3()
            .child(self.quick_recorder.clone())
            .child(div().flex().flex_wrap().gap_2()
                .child(button("open-image-tool", "Open image").child(icon("folder")).on_click(cx.listener(|this, _, w, cx| this.choose_files(0, w, cx))))
                .child(button("share-files-tool", "Share files").child(icon("download")).on_click(cx.listener(|this, _, w, cx| this.choose_files(3, w, cx))))
                .child(button("capture-extras", "More tools").on_click(cx.listener(|this, _, _, cx| { this.capture_extras = !this.capture_extras; cx.notify(); }))))
            .when(self.capture_extras, |s| s.child(div().w_full().max_w(px(520.)).child(extras)))
    }
    fn destinations(&self, cx: &mut Context<Self>) -> Div {
        let mut root=div().flex().flex_col().gap_4().child(div().text_size(px(22.)).child("Upload destinations"))
            .child(muted("Import ShareX .sxcu uploaders or existing toolbox settings for S3, Imgur and custom HTTP destinations."))
            .child(div().flex().gap_2().child(button("import-sxcu","Import .sxcu").on_click(cx.listener(|this,_,w,cx|this.choose_files(1,w,cx))))
                .child(button("import-ssx","Import toolbox settings").on_click(cx.listener(|this,_,w,cx|this.choose_files(2,w,cx))))
                .child(button("share-upload-files","Choose files to share").on_click(cx.listener(|this,_,w,cx|this.choose_files(3,w,cx)))));
        if let Some(snapshot) = &self.snapshot {
            root = root.child(mono(format!(
                "Images: {} · Files: {} · Videos: {}",
                snapshot
                    .settings
                    .destinations
                    .image
                    .as_deref()
                    .unwrap_or("not set"),
                snapshot
                    .settings
                    .destinations
                    .file
                    .as_deref()
                    .unwrap_or("not set"),
                snapshot
                    .settings
                    .destinations
                    .video
                    .as_deref()
                    .unwrap_or("file default")
            )));
            for (i, uploader) in snapshot.uploaders.iter().enumerate() {
                let usable = uploader.error.is_none() && !uploader.uploads.is_empty();
                let mut destinations = div().flex().flex_wrap().gap_2();
                for (kind_index, kind) in ["image", "file", "video", "text"].into_iter().enumerate()
                {
                    if !usable || !uploader.uploads.contains(&kind) {
                        continue;
                    }
                    let selected = match kind {
                        "image" => snapshot.settings.destinations.image.as_deref(),
                        "file" => snapshot.settings.destinations.file.as_deref(),
                        "video" => snapshot.settings.destinations.video.as_deref().or(snapshot
                            .settings
                            .destinations
                            .file
                            .as_deref()),
                        _ => snapshot.settings.destinations.text.as_deref(),
                    } == Some(uploader.name.as_str());
                    let name = uploader.name.clone();
                    destinations = destinations.child(
                        button(
                            ("destination-type", i * 4 + kind_index),
                            format!(
                                "{}{}",
                                if selected { "✓ " } else { "Use for " },
                                match kind {
                                    "image" => "Images",
                                    "file" => "Files",
                                    "video" => "Videos",
                                    _ => "Text",
                                }
                            ),
                        )
                        .bg(rgb(if selected { 0x173025 } else { SOFT }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(snapshot) = &this.snapshot {
                                let mut settings = snapshot.settings.clone();
                                match kind {
                                    "image" => settings.destinations.image = Some(name.clone()),
                                    "file" => settings.destinations.file = Some(name.clone()),
                                    "video" => settings.destinations.video = Some(name.clone()),
                                    _ => settings.destinations.text = Some(name.clone()),
                                }
                                this.save(settings, cx);
                            }
                        })),
                    );
                }
                root = root.child(
                    panel().child(
                        div()
                            .flex()
                            .items_start()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(uploader.name.clone())
                                    .child(muted(format!(
                                        "{} · {}",
                                        uploader.kind, uploader.origin
                                    )))
                                    .when_some(uploader.error.clone(), |s, error| {
                                        s.child(muted(error).text_color(rgb(0xffa0a0)))
                                    }),
                            )
                            .child(destinations),
                    ),
                );
            }
        }
        root.child(muted("The local destination keeps files on this PC. No remote upload occurs until you choose a configured destination and run a sharing action."))
    }
    fn settings(&self, cx: &mut Context<Self>) -> Div {
        let mut root = div().flex().flex_col().gap_4().child(
            div()
                .text_size(px(22.))
                .child("Screenshot & sharing settings"),
        );
        if let Some(snapshot) = &self.snapshot {
            root=root.child(panel().child("Screenshot format").child(div().flex().gap_2().children([("PNG",::toolkit::core::settings::ImageFormatKind::Png),("JPEG",::toolkit::core::settings::ImageFormatKind::Jpg),("WebP",::toolkit::core::settings::ImageFormatKind::Webp)].into_iter().enumerate().map(|(i,(name,format))|button(("screenshot-format",i),name).bg(rgb(if snapshot.settings.general.image_format==format{0x173025}else{SOFT})).on_click(cx.listener(move|this,_,_,cx|{if let Some(s)=&this.snapshot{let mut settings=s.settings.clone();settings.general.image_format=format;this.save(settings,cx);}})))))
                .child(div().flex().items_center().justify_between().child("Include cursor").child(toggle("screenshot-cursor",snapshot.settings.capture.show_cursor).on_click(cx.listener(|this,_,_,cx|{if let Some(s)=&this.snapshot{let mut settings=s.settings.clone();settings.capture.show_cursor= !settings.capture.show_cursor;this.save(settings,cx);}})))))
                .child(panel().child("Filename pattern").child(self.pattern.clone()).child(button("save-screenshot-pattern","Save naming pattern").on_click(cx.listener(|this,_,_,cx|{if let Some(s)=&this.snapshot{let mut settings=s.settings.clone();settings.general.file_name_pattern=this.pattern.read(cx).value();this.save(settings,cx);}}))))
                .child(panel().child("Save folder").child(mono(snapshot.settings.general.resolve_save_dir().display().to_string())).child(button("screenshot-folder","Change").on_click(cx.listener(|_,_,window,cx|{
                    let selected=cx.prompt_for_paths(PathPromptOptions{files:false,directories:true,multiple:false,prompt:Some("Save screenshots here".into())});
                    cx.spawn_in(window,async move|view,cx|{if let Ok(Ok(Some(paths)))=selected.await&&let Some(path)=paths.into_iter().next(){let _=view.update(cx,|this,cx|{if let Some(s)=&this.snapshot{let mut settings=s.settings.clone();settings.general.save_dir=Some(path);this.save(settings,cx);}});}}).detach();
                }))))
                .child(panel().child("HDR tone mapping").child(muted("RCR converts each HDR display before stitching it with SDR displays."))
                    .child(button("toggle-highlight-preservation",if snapshot.settings.capture.hdr.knee>=0.99{"Faithful SDR whites"}else{"Preserve highlight detail"}).on_click(cx.listener(|this,_,_,cx|{if let Some(s)=&this.snapshot{let mut settings=s.settings.clone();settings.capture.hdr=if settings.capture.hdr.knee>=0.99{::toolkit::core::settings::HdrConfig::preserve_highlights()}else{::toolkit::core::settings::HdrConfig::faithful()};this.save(settings,cx);}}))))
                .child(panel().child("Advanced toolbox configuration").child(muted("All imported uploader, workflow, naming and post-upload settings remain available in this file. Secrets use the system keyring."))
                    .child(button("open-toolkit-config","Edit settings.toml").on_click({let path=snapshot.config_path.clone();move|_,_,_|{let _=std::process::Command::new("notepad.exe").arg(&path).spawn();}}))
                    .child(button("reload-toolkit-config","Reload settings").on_click(cx.listener(|this,_,_,cx|{this.submit(Action::Refresh,cx);}))));
        }
        root
    }
}
fn activate_main(cx: &mut App) {
    if let Some(window) = cx
        .windows()
        .into_iter()
        .find(|w| w.downcast::<crate::Studio>().is_some())
    {
        let _ = window.update(cx, |_, window, _| window.activate_window());
    }
}
impl Render for Toolkit {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.editing {
            return div()
                .size_full()
                .child(self.editor.clone())
                .into_any_element();
        }
        let mut nav = div().flex().flex_wrap().gap_2().px_6().py_3();
        if self.pane == 6 { nav = nav.gap_1().px_2().py_1(); }
        for (i, name) in [
            "Screenshots",
            "Workflows",
            "Destinations",
            "History",
            "Preferences",
            "Utilities",
            "Disk usage",
        ]
        .into_iter()
        .enumerate()
        {
            nav = nav.child(
                button(("toolkit-pane", i), name)
                    .when(self.pane == 6, |s| s.h(px(24.)).px_2().rounded(px(2.)).text_size(px(12.)))
                    .bg(rgb(if self.pane == i { SOFT } else { BG }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pane = i;
                        cx.notify();
                    })),
            );
        }
        let content = match self.pane {
            1 => self
                .workflows(window.viewport_size().width < px(860.), cx)
                .into_any_element(),
            2 => self.destinations(cx).into_any_element(),
            3 => self.history(cx).into_any_element(),
            4 => self.settings(cx).into_any_element(),
            5 => self.utilities.clone().into_any_element(),
            6 => self.disk_usage.clone().into_any_element(),
            _ => self.home(cx).into_any_element(),
        };
        if self.pane == 3 { self.reveal_history_focus(window, cx); }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(nav)
            .child(
                div()
                    .id(if self.pane == 3 { "toolkit-history-content" } else { "toolkit-content" })
                    .flex_1()
                    .min_h_0()
                    .when(self.pane != 3 && self.pane != 6, |s| s.overflow_y_scroll())
                    .when(self.pane == 3, |s| s.flex().flex_col().overflow_hidden())
                    .when(self.pane == 6, |s| s.flex().flex_col().overflow_y_scroll())
                    .p_6()
                    .when(self.pane == 6, |s| s.p_1())
                    .child(content),
            )
            .child(
                div()
                    .px_6()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(muted(self.status.clone()).flex_1().min_w_0().truncate())
                    .when(self.pane == 6 && !self.worker.as_ref().is_some_and(|w| w.busy()), |s| s.hidden())
                    .when_some(self.result_url.clone(), |s, url| {
                        s.child(button("copy-result-url", "Copy link").on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                                this.status = "Link copied".into();
                                cx.notify();
                            },
                        )))
                    })
                    .when_some(self.result_path.clone(), |s, path| {
                        s.child(
                            button("reveal-result-file", if self.shell_operation != 0 { "Opening…" } else { "Show file" })
                                .on_click(cx.listener(move |this, _, _, cx| this.show_file(path.clone(), cx))),
                        )
                    })
                    .when(self.worker.as_ref().is_some_and(|w| w.busy()), |s| {
                        s.child(
                            button(
                                "cancel-toolkit-job",
                                if self.cancelling {
                                    "Cancelling…"
                                } else {
                                    "Cancel"
                                },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(w) = &this.worker {
                                    w.cancel();
                                    this.cancelling = true;
                                    this.status = "Cancelling…".into();
                                    if let Some(reply) = this.reply.take() {
                                        let _ = reply.try_send(None);
                                    }
                                    if this.editing {
                                        this.editing = false;
                                        this.editor.update(cx, |editor, cx| editor.close(cx));
                                    }
                                    cx.notify();
                                }
                            })),
                        )
                    }),
            )
            .when_some(self.error.clone(), |s, e| {
                s.child(
                    div()
                        .px_6()
                        .pb_3()
                        .flex()
                        .items_start()
                        .gap_3()
                        .child(div().flex_1().text_color(rgb(0xffa0a0)).child(e))
                        .child(
                            button("dismiss-toolkit-error", "Dismiss").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.error = None;
                                    cx.notify();
                                },
                            )),
                        ),
                )
            })
            .into_any_element()
    }
}
impl Drop for Toolkit {
    fn drop(&mut self) {
        if let Some(w) = &self.worker {
            w.cancel();
        }
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(None);
        }
    }
}

fn upload_progress(done: u64, total: Option<u64>) -> String {
    match total.filter(|total| *total > 0) {
        Some(total) => format!(
            "Uploading {:.1} / {:.1} MB · {}%",
            done as f64 / 1e6,
            total as f64 / 1e6,
            (done.saturating_mul(100) / total).min(100)
        ),
        None => format!("Uploading · {:.1} MB sent", done as f64 / 1e6),
    }
}

fn step_status(step: StepKind) -> &'static str {
    match step {
        StepKind::Capture => "Capturing…",
        StepKind::Record => "Recording…",
        StepKind::ReadClipboard => "Reading clipboard…",
        StepKind::LoadFile => "Opening file…",
        StepKind::Zip => "Compressing folder…",
        StepKind::OpenEditor => "Waiting for your annotations…",
        StepKind::CopyImage => "Copying image…",
        StepKind::SaveToFile => "Saving file…",
        StepKind::SaveAsDialog => "Choosing save location…",
        StepKind::PinToScreen => "Pinning image…",
        StepKind::Ocr => "Recognizing text…",
        StepKind::Upload => "Uploading…",
        StepKind::DeleteLocalFile => "Removing local copy…",
        StepKind::CopyUrl | StepKind::CopyShortUrl => "Copying link…",
        StepKind::OpenUrl => "Opening link…",
        StepKind::ShortenUrl => "Shortening link…",
        StepKind::ShowQrCode => "Creating QR code…",
        StepKind::ShowNotification => "Showing notification…",
        StepKind::RunCommand => "Running configured action…",
        StepKind::RecordHistory => "Updating history…",
    }
}

fn completion_status(report: &RunReport) -> String {
    if report.outcome != Outcome::Success {
        return match report.outcome {
            Outcome::Cancelled => "Cancelled".into(),
            Outcome::PartialSuccess => "Completed with errors · see details below".into(),
            _ => "Could not complete this task · see details below".into(),
        };
    }
    let succeeded = |kind| {
        report
            .all_steps()
            .any(|step| step.kind == kind && matches!(step.status, StepStatus::Succeeded))
    };
    let mut completed = Vec::new();
    if succeeded(StepKind::CopyImage) {
        completed.push("Image copied");
    }
    if succeeded(StepKind::SaveToFile) || succeeded(StepKind::SaveAsDialog) {
        completed.push("File saved");
    }
    if report.items.iter().any(|item| item.url.is_some()) {
        completed.push("Upload complete");
    }
    if succeeded(StepKind::CopyUrl) || succeeded(StepKind::CopyShortUrl) {
        completed.push("Link copied");
    }
    if completed.is_empty() {
        report.summary()
    } else {
        completed.join(" · ")
    }
}
