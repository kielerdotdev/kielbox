//! One cancellable workflow worker; interactive steps rendezvous with the GPUI thread.
use crate::{core, services, types};
use anyhow::{Context, Result, bail};
use core::{
    history::{Entry, History, Query},
    pattern::FileCounter,
    settings::{AfterCapture, AfterUpload, InputKind, Paths, Settings, Workflow},
    workflow::{
        CancelToken, CaptureTarget, EditResult, Engine, EventSink, Input, Naming, RunReport,
        ServiceError,
    },
};
use services::{ProductionOptions, ProductionServices, RegionSelector, UploaderInfo};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use types::{Frame, Rect};

#[path = "history_search.rs"]
mod history_search;
pub use history_search::{HISTORY_PAGE_SIZE, HistoryPage, HistoryPageResult, HistorySearch};

pub enum UiRequest {
    Capture {
        frame: Frame,
        choose_region: bool,
        configured_action: bool,
        reply: mpsc::SyncSender<Option<OverlayResult>>,
        cancel: CancelToken,
    },
    Save {
        suggested: PathBuf,
        reply: mpsc::SyncSender<Option<PathBuf>>,
    },
    Edit {
        source: crate::image_editor::Source,
        reply: mpsc::SyncSender<Option<Frame>>,
        cancel: CancelToken,
    },
    StandaloneEdit {
        source: crate::image_editor::Source,
        reply: mpsc::SyncSender<Option<StandaloneImage>>,
        cancel: CancelToken,
    },
    Region {
        frame: Frame,
        reply: mpsc::SyncSender<Option<Rect>>,
        cancel: CancelToken,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayAction {
    Copy,
    Save,
    Configured,
}
pub struct OverlayResult {
    pub frame: Frame,
    pub region: Rect,
    pub action: OverlayAction,
    pub completion: CompletionSender<OverlayCompletion>,
}
#[derive(Debug)]
pub enum OverlayCompletion {
    Succeeded,
    Failed(String),
}
pub struct StandaloneImage {
    pub frame: Frame,
    pub completion: CompletionSender<StandaloneCompletion>,
}
/// A bounded completion rendezvous with an application-owned, nonblocking wake.
/// Dropping the last sender also wakes the receiver so a stopped worker does not
/// leave the UI waiting for its periodic cancellation watchdog.
pub struct CompletionSender<T> {
    sender: Option<mpsc::SyncSender<T>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl<T> CompletionSender<T> {
    pub fn new(sender: mpsc::SyncSender<T>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self { sender: Some(sender), wake: Arc::new(wake) }
    }
    pub fn try_send(&self, value: T) -> Result<(), mpsc::TrySendError<T>> {
        let result = self.sender.as_ref().expect("Live completion sender").try_send(value);
        if result.is_ok() { (self.wake)(); }
        result
    }
}
impl<T> Drop for CompletionSender<T> {
    fn drop(&mut self) {
        // Publish disconnection before waking: the UI may run immediately.
        self.sender.take();
        (self.wake)();
    }
}
#[derive(Debug)]
pub enum StandaloneCompletion {
    Succeeded,
    Failed { detail: String, saved_path: Option<PathBuf> },
}

fn image_identity(frame: &Frame, cancel: &CancelToken) -> Result<[u8; 32]> {
    use sha2::Digest;
    let mut digest = sha2::Sha256::new();
    digest.update(frame.width().to_le_bytes());
    digest.update(frame.height().to_le_bytes());
    for chunk in frame.data().chunks(64 * 1024) {
        cancel.check()?;
        digest.update(chunk);
    }
    cancel.check()?;
    Ok(digest.finalize().into())
}

/// Only identity and successful-step metadata survive a failed attempt. Images
/// remain owned by the annotation Actor or move into the current Engine run.
#[derive(Default)]
struct StandaloneProgress {
    identity: Option<[u8; 32]>,
    saved_path: Option<PathBuf>,
    saved_step: Option<core::workflow::StepReport>,
    copied_step: Option<core::workflow::StepReport>,
}
impl StandaloneProgress {
    fn prepare(&mut self, identity: [u8; 32]) -> Vec<AfterCapture> {
        if self.identity != Some(identity) {
            *self = Self { identity: Some(identity), ..Default::default() };
        }
        let mut steps = Vec::new();
        if self.saved_path.is_none() { steps.push(AfterCapture::SaveToFile); }
        if self.copied_step.is_none() { steps.push(AfterCapture::CopyImageToClipboard); }
        steps
    }
    fn observe(&mut self, report: &RunReport) {
        use core::workflow::StepKind;
        if let Some(step) = report.all_steps().find(|step| step.kind == StepKind::SaveToFile && step.status.is_success())
            && let Some(path) = report.items.iter().find_map(|item| item.local_path.clone()) {
            self.saved_path = Some(path);
            self.saved_step = Some(step.clone());
        }
        if let Some(step) = report.all_steps().find(|step| step.kind == StepKind::CopyImage && step.status.is_success()) {
            self.copied_step = Some(step.clone());
        }
    }
    fn complete(&self) -> bool { self.saved_path.is_some() && self.copied_step.is_some() }
    fn restore_metadata(&self, report: &mut RunReport) {
        if let Some(item) = report.items.first_mut() {
            if item.local_path.is_none() {
                item.local_path = self.saved_path.clone();
                item.created_by_workflow = self.saved_path.is_some();
            }
            for step in [&self.saved_step, &self.copied_step].into_iter().flatten() {
                if !item.steps.iter().any(|current| current.kind == step.kind && current.status.is_success()) {
                    item.steps.push(step.clone());
                }
            }
            if self.complete() { item.outcome = core::workflow::Outcome::Success; }
        }
        if self.complete() { report.outcome = core::workflow::Outcome::Success; }
    }
}

fn overlay_workflow(baseline: &Workflow, action: OverlayAction) -> Workflow {
    let mut workflow = baseline.clone();
    workflow.after_capture.retain(|step| *step != AfterCapture::OpenEditor);
    match action {
        OverlayAction::Copy => {
            workflow.after_capture = vec![AfterCapture::CopyImageToClipboard];
            workflow.after_upload.clear();
        }
        OverlayAction::Save => {
            workflow.after_capture = vec![AfterCapture::SaveToFile];
            workflow.after_upload.clear();
        }
        OverlayAction::Configured => {}
    }
    workflow
}

fn overlay_completion(action: OverlayAction, report: &RunReport) -> OverlayCompletion {
    use core::workflow::{Outcome, StepKind, StepStatus};
    let succeeded = match action {
        OverlayAction::Configured => report.outcome == Outcome::Success,
        OverlayAction::Copy | OverlayAction::Save => {
            let expected = if action == OverlayAction::Copy { StepKind::CopyImage } else { StepKind::SaveToFile };
            report.all_steps().any(|step| step.kind == expected && step.status.is_success())
        }
    };
    if succeeded { return OverlayCompletion::Succeeded; }
    let mut detail = String::new();
    let mut remaining = 4_000;
    for failure in report.all_steps().filter_map(|step| match &step.status {
        StepStatus::Failed(failure) => Some(failure.message.as_str()), _ => None,
    }).take(16) {
        if remaining == 0 { break; }
        if !detail.is_empty() { detail.push('\n'); remaining -= 1; }
        for character in failure.chars().take(remaining) { detail.push(character); remaining -= 1; }
    }
    if detail.is_empty() { detail = "The requested operation did not complete.".into(); }
    if action == OverlayAction::Configured {
        detail.push_str("\nSome workflow actions may have completed. Use Save or Copy to recover; the workflow will not run again.");
    }
    OverlayCompletion::Failed(detail)
}
pub struct Snapshot {
    pub settings: Settings,
    pub uploaders: Vec<UploaderInfo>,
    pub history: Vec<Entry>,
    pub config_path: PathBuf,
}
pub enum Event {
    Snapshot(Box<Snapshot>),
    Ui(UiRequest),
    Progress(core::workflow::Event),
    Finished(Result<Option<RunReport>, String>),
}
#[derive(Clone, Debug)]
pub struct ScreenshotOptions {
    pub cursor: bool,
    pub explicit: Option<services::ExplicitTarget>,
    /// Already frozen by the desktop picker; do not capture the desktop again.
    pub captured: Option<core::workflow::Captured>,
}
fn screenshot_delay_ms(configured: u32, options: Option<&ScreenshotOptions>) -> u32 {
    // A frozen picker frame is already captured: waiting cannot change its
    // pixels and only postpones the annotation surface or clipboard result.
    if options.is_some_and(|options| options.captured.is_some()) {
        return 0;
    }
    configured.max(if options.is_some() { 250 } else { 0 })
}
pub enum Action {
    Refresh,
    Screenshot {
        target: CaptureTarget,
        edit: bool,
        upload: bool,
        options: Option<ScreenshotOptions>,
    },
    Workflow(String),
    Files(Vec<PathBuf>),
    WorkflowFiles {
        id: String,
        paths: Vec<PathBuf>,
    },
    EditClipboard,
    OpenImage(PathBuf),
    EditFrame(Arc<Frame>),
    Qr(String),
    SaveSettings(Box<Settings>),
    ImportSettings(PathBuf),
    ImportUploader(PathBuf),
}
struct Job {
    action: Action,
    cancel: CancelToken,
}
struct Bridge(mpsc::Sender<Event>);
struct SaveBridge<'a> {
    events: &'a mpsc::Sender<Event>,
    cancel: &'a CancelToken,
}
impl core::workflow::SaveDialog for SaveBridge<'_> {
    fn choose_path(&self, suggested: &std::path::Path) -> Result<Option<PathBuf>, ServiceError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.events
            .send(Event::Ui(UiRequest::Save {
                suggested: suggested.to_owned(),
                reply: tx,
            }))
            .map_err(|_| ServiceError::Cancelled)?;
        wait(rx, self.cancel)
    }
}
fn wait<T>(rx: mpsc::Receiver<Option<T>>, cancel: &CancelToken) -> Result<Option<T>, ServiceError> {
    wait_borrowed(&rx, cancel)
}
fn wait_borrowed<T>(rx: &mpsc::Receiver<Option<T>>, cancel: &CancelToken) -> Result<Option<T>, ServiceError> {
    loop {
        if cancel.is_cancelled() {
            return Err(ServiceError::Cancelled);
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(ServiceError::Cancelled),
        }
    }
}
impl core::workflow::Editor for Bridge {
    fn edit(&self, frame: &Frame, cancel: &CancelToken) -> Result<EditResult, ServiceError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.0
            .send(Event::Ui(UiRequest::Edit {
                source: crate::image_editor::Source::Image(frame.clone()),
                reply: tx,
                cancel: cancel.clone(),
            }))
            .map_err(|_| ServiceError::Cancelled)?;
        Ok(wait(rx, cancel)?.map_or(EditResult::Cancelled, EditResult::Edited))
    }
}
impl RegionSelector for Bridge {
    fn select(&self, _: &[types::Monitor], frame: &Frame) -> Result<Option<Rect>, ServiceError> {
        let (tx, rx) = mpsc::sync_channel(1);
        let cancel = CancelToken::new();
        self.0
            .send(Event::Ui(UiRequest::Region {
                frame: frame.clone(),
                reply: tx,
                cancel: cancel.clone(),
            }))
            .map_err(|_| ServiceError::Cancelled)?;
        wait(rx, &cancel)
    }
    fn pick(
        &self,
        req: &services::PickRequest<'_>,
    ) -> Result<Option<services::Picked>, ServiceError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.0
            .send(Event::Ui(UiRequest::Region {
                frame: req.desktop.clone(),
                reply: tx,
                cancel: req.cancel.clone(),
            }))
            .map_err(|_| ServiceError::Cancelled)?;
        Ok(wait(rx, req.cancel)?.map(services::Picked::rect))
    }
}
impl EventSink for Bridge {
    fn event(&self, event: core::workflow::Event) {
        let _ = self.0.send(Event::Progress(event));
    }
}
pub struct Worker {
    pub history_search: HistorySearch,
    tx: Option<mpsc::SyncSender<Job>>,
    pub events: mpsc::Receiver<Event>,
    busy: Arc<AtomicBool>,
    current: Mutex<CancelToken>,
    shutdown: Arc<AtomicBool>,
}
impl Worker {
    pub fn new(paths: Paths, save_dir: PathBuf) -> Result<Self> {
        let history_search = HistorySearch::waiting()?;
        let initialize_search = history_search.initializer();
        let (tx, rx) = mpsc::sync_channel::<Job>(1);
        let (events, out) = mpsc::channel();
        // The bounded channel can accept the first command while state initializes.
        let busy = Arc::new(AtomicBool::new(false));
        let in_flight = busy.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let quitting = shutdown.clone();
        thread::Builder::new()
            .name("toolkit-workflows".into())
            .spawn(move || {
                let mut state = match State::new(paths, save_dir, events.clone()) {
                    Ok(state) => state,
                    Err(error) => {
                        initialize_search(Err(format!("{error:#}")));
                        let _ = events.send(Event::Finished(Err(format!("{error:#}"))));
                        in_flight.store(false, Ordering::Release);
                        return;
                    }
                };
                initialize_search(Ok(state.history.clone()));
                if let Err(error) = state.snapshot() {
                    let _ = events.send(Event::Finished(Err(format!("{error:#}"))));
                }
                while let Ok(job) = rx.recv() {
                    if quitting.load(Ordering::Acquire) {
                        break;
                    }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        state.run(job.action, &job.cancel)
                    }))
                    .unwrap_or_else(|_| {
                        Err(anyhow::anyhow!("Toolkit operation failed unexpectedly"))
                    })
                    .map_err(|e| format!("{e:#}"));
                    let _ = state.snapshot();
                    in_flight.store(false, Ordering::Release);
                    if events.send(Event::Finished(result)).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            history_search,
            tx: Some(tx),
            events: out,
            busy,
            current: Mutex::new(CancelToken::new()),
            shutdown,
        })
    }
    pub fn submit(&self, action: Action) -> Result<()> {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            bail!("A toolkit operation is already running; finish or cancel it first");
        }
        let cancel = CancelToken::new();
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = cancel.clone();
        if self
            .tx
            .as_ref()
            .context("Toolkit worker stopped")?
            .try_send(Job { action, cancel })
            .is_err()
        {
            self.busy.store(false, Ordering::Release);
            bail!("Toolkit worker is unavailable");
        }
        Ok(())
    }
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }
    pub fn cancel(&self) {
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.cancel();
        self.tx.take();
    }
}

struct State {
    paths: Paths,
    settings: Settings,
    history: Arc<History>,
    events: mpsc::Sender<Event>,
    production: OnceLock<ProductionServices>,
}
impl State {
    fn new(paths: Paths, save_dir: PathBuf, events: mpsc::Sender<Event>) -> Result<Self> {
        paths.ensure()?;
        let loaded = Settings::load(&paths.settings_file())?;
        let mut settings = loaded.settings;
        if !loaded.existed {
            settings.general.save_dir = Some(save_dir);
            settings.capture.delay_ms = 200;
            settings.destinations.image = Some("local".into());
            settings.destinations.file = Some("local".into());
            settings.destinations.video = Some("local".into());
            settings.save(&paths.settings_file())?;
        }
        let history = Arc::new(History::open(&paths.history_db())?);
        Ok(Self {
            paths,
            settings,
            history,
            events,
            production: OnceLock::new(),
        })
    }
    fn production(&self) -> &ProductionServices {
        // Preserve the lazy capture backend, HTTP pool and network runtime between jobs.
        self.production.get_or_init(|| {
            ProductionServices::new(
                &self.settings,
                &self.paths,
                ProductionOptions {
                    selector: Some(Arc::new(Bridge(self.events.clone()))),
                    ..Default::default()
                },
            )
        })
    }
    fn snapshot(&self) -> Result<()> {
        let snapshot = Snapshot {
            settings: self.settings.clone(),
            uploaders: self.production().uploads.list(),
            history: self.history.list(&Query {
                limit: 100,
                thumbnails: false,
                ..Default::default()
            })?,
            config_path: self.paths.settings_file(),
        };
        self.events.send(Event::Snapshot(Box::new(snapshot)))?;
        Ok(())
    }
    fn run(&mut self, mut action: Action, cancel: &CancelToken) -> Result<Option<RunReport>> {
        let screenshot_options = match &mut action {
            Action::Screenshot { options, .. } => options.take(),
            _ => None,
        };
        use core::workflow::QrRenderer;
        match action {
            Action::Refresh => {
                self.settings = Settings::load(&self.paths.settings_file())?.settings;
                self.production.take();
                return Ok(None);
            }
            Action::SaveSettings(settings) => {
                settings.save(&self.paths.settings_file())?;
                self.settings = *settings;
                self.production.take();
                return Ok(None);
            }
            Action::ImportSettings(path) => {
                anyhow::ensure!(path.is_file(), "The settings file does not exist");
                let mut s = Settings::load(&path)?.settings;
                // Keep relative uploader references valid without copying or editing their secrets.
                let original = path
                    .canonicalize()?
                    .parent()
                    .context("Settings file has no parent")?
                    .to_owned();
                for table in s.uploaders.values_mut() {
                    if table.get("type").and_then(toml::Value::as_str) == Some("sxcu")
                        && let Some(toml::Value::String(file)) = table.get_mut("file")
                        && std::path::Path::new(file).is_relative()
                    {
                        *file = original.join(&*file).to_string_lossy().into_owned();
                    }
                }
                // Imported commands remain visible for review; no workflow runs on import.
                if s.general.save_dir.is_none() {
                    s.general.save_dir = self.settings.general.save_dir.clone();
                }
                s.save(&self.paths.settings_file())?;
                self.settings = s;
                self.production.take();
                return Ok(None);
            }
            Action::ImportUploader(path) => {
                services::upload::import_sxcu(&self.paths.config_dir, &path, None, false)?;
                self.production.take();
                return Ok(None);
            }
            Action::OpenImage(path) => {
                let source = if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("ssxe"))
                {
                    crate::image_editor::Source::Project(crate::annotation::project::load(&path)?)
                } else {
                    crate::image_editor::Source::Image(Frame::decode(&std::fs::read(path)?)?)
                };
                return self.edit_and_save(source, cancel);
            }
            Action::EditFrame(frame) => {
                return self
                    .edit_and_save(crate::image_editor::Source::Image((*frame).clone()), cancel);
            }
            Action::EditClipboard => {
                use core::workflow::{Clipboard, ClipboardContent};
                let clipboard = &self.production().clipboard;
                let ClipboardContent::Image(frame) = clipboard.read()? else {
                    bail!("The clipboard does not contain an image");
                };
                return self.edit_and_save(crate::image_editor::Source::Image(frame), cancel);
            }
            Action::Qr(text) => {
                return self.edit_and_save(
                    crate::image_editor::Source::Image(
                        core::workflow::QrCodeRenderer.render(&text)?,
                    ),
                    cancel,
                );
            }
            _ => {}
        }
        let (workflow, input) = match action {
            Action::Screenshot {
                target,
                edit,
                upload,
                ..
            } => {
                let input = match target {
                    CaptureTarget::Region => InputKind::CaptureRegion,
                    CaptureTarget::Fullscreen => InputKind::CaptureFullscreen,
                    CaptureTarget::Monitor => InputKind::CaptureMonitor,
                    CaptureTarget::Window => InputKind::CaptureWindow,
                    CaptureTarget::LastRegion => InputKind::CaptureLastRegion,
                };
                let mut after_capture = vec![];
                if edit {
                    after_capture.push(AfterCapture::OpenEditor);
                }
                after_capture
                    .extend([AfterCapture::SaveToFile, AfterCapture::CopyImageToClipboard]);
                let after_upload = if upload {
                    after_capture.push(AfterCapture::Upload);
                    vec![AfterUpload::CopyUrl, AfterUpload::ShowNotification]
                } else {
                    vec![]
                };
                (
                    Workflow {
                        id: "screenshot".into(),
                        name: "Screenshot".into(),
                        input,
                        after_capture,
                        after_upload,
                        ..Default::default()
                    },
                    Input::Auto,
                )
            }
            Action::Workflow(id) => {
                let workflow = self
                    .settings
                    .find_workflow(&id)
                    .context("Workflow no longer exists")?
                    .clone();
                if workflow.input.is_recording() {
                    bail!("Use rcr Capture for NVENC recording; share the result from the Library");
                }
                if workflow.input == InputKind::Files {
                    bail!("Choose files with Share files first");
                }
                (workflow, Input::Auto)
            }
            Action::Files(paths) => {
                if paths.is_empty() {
                    bail!("Choose at least one file");
                }
                let workflow = self
                    .settings
                    .find_workflow("upload-files")
                    .cloned()
                    .unwrap_or(Workflow {
                        id: "rcr-share-files".into(),
                        name: "Share files".into(),
                        input: InputKind::Files,
                        after_capture: vec![AfterCapture::Upload],
                        after_upload: vec![AfterUpload::CopyUrl, AfterUpload::ShowNotification],
                        ..Default::default()
                    });
                (workflow, Input::Files(paths))
            }
            Action::WorkflowFiles { id, paths } => {
                if paths.is_empty() {
                    bail!("Choose at least one file");
                }
                (
                    self.settings
                        .find_workflow(&id)
                        .context("Workflow no longer exists")?
                        .clone(),
                    Input::Files(paths),
                )
            }
            _ => unreachable!("configuration and image actions return before workflow dispatch"),
        };
        if workflow.input.is_still_capture() {
            return self.capture_workflow(workflow, screenshot_options, cancel);
        }
        Ok(Some(self.execute(&workflow, input, cancel)))
    }
    fn capture_workflow(
        &self,
        workflow: Workflow,
        mut options: Option<ScreenshotOptions>,
        cancel: &CancelToken,
    ) -> Result<Option<RunReport>> {
        use core::workflow::{CaptureRequest, Captured, Capturer};
        let region = workflow.input == InputKind::CaptureRegion
            && options.as_ref().is_none_or(|o| o.explicit.is_none() && o.captured.is_none());
        let target = match workflow.input {
            InputKind::CaptureRegion | InputKind::CaptureFullscreen => CaptureTarget::Fullscreen,
            InputKind::CaptureMonitor => CaptureTarget::Monitor,
            InputKind::CaptureWindow => CaptureTarget::Window,
            _ => CaptureTarget::LastRegion,
        };
        // The compact picker has just minimized. Let the compositor remove it
        // even when the user's configured workflow delay is zero.
        let delay_ms = screenshot_delay_ms(self.settings.capture.delay_ms, options.as_ref());
        let deadline = std::time::Instant::now()
            + Duration::from_millis(delay_ms as u64);
        while std::time::Instant::now() < deadline {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(10));
        }
        let cursor = options.as_ref().map_or(self.settings.capture.show_cursor, |o| o.cursor);
        let captured = if let Some(captured) = options.as_mut().and_then(|o| o.captured.take()) {
            captured
        } else if let Some(explicit) = options.as_ref().and_then(|o| o.explicit.as_ref()) {
            self.production().capturer.capture_explicit_with_cancel(explicit, cursor, &self.settings.capture.hdr, cancel)?
        } else { self.production().capturer.capture(
            &CaptureRequest {
                target,
                include_cursor: cursor,
                hdr: self.settings.capture.hdr,
            },
            cancel,
        )? };
        if region || workflow.after_capture.contains(&AfterCapture::OpenEditor) {
            let origin = captured.frame.origin;
            let (tx, rx) = mpsc::sync_channel(1);
            self.events.send(Event::Ui(UiRequest::Capture {
                frame: captured.frame,
                choose_region: region,
                configured_action: workflow.after_capture.contains(&AfterCapture::Upload),
                reply: tx,
                cancel: cancel.clone(),
            }))?;
            let mut last_report = None;
            let mut configured_attempted = false;
            loop {
                let result = match wait_borrowed(&rx, cancel) {
                    Ok(Some(result)) => result,
                    Ok(None) | Err(ServiceError::Cancelled) => return Ok(last_report),
                    Err(error) => return Err(error.into()),
                };
                if result.action == OverlayAction::Configured && configured_attempted {
                    if result.completion.try_send(OverlayCompletion::Failed(
                        "The workflow already ran. Use Save or Copy to recover without repeating uploads.".into())).is_err() {
                        return Ok(last_report);
                    }
                    continue;
                }
                configured_attempted |= result.action == OverlayAction::Configured;
                if region {
                    services::LastRegionStore::in_dir(&self.paths.data_dir).save(Rect::new(
                        origin.x + result.region.x, origin.y + result.region.y,
                        result.region.width, result.region.height,
                    ));
                }
                let attempt = overlay_workflow(&workflow, result.action);
                // Move every rendered image into the engine. Only small source
                // metadata is cloned; the overlay retains its annotation Actor.
                let report = self.execute(&attempt, Input::Image(Captured {
                    frame: result.frame,
                    window_title: captured.window_title.clone(),
                    process_name: captured.process_name.clone(),
                }), cancel);
                let completion = overlay_completion(result.action, &report);
                let succeeded = matches!(completion, OverlayCompletion::Succeeded);
                let closed = result.completion.try_send(completion).is_err();
                if succeeded || closed || cancel.is_cancelled() { return Ok(Some(report)); }
                last_report = Some(report);
            }
        }
        Ok(Some(self.execute(
            &workflow,
            Input::Image(captured),
            cancel,
        )))
    }
    fn execute(&self, workflow: &Workflow, input: Input, cancel: &CancelToken) -> RunReport {
        let production = self.production();
        let editor = Bridge(self.events.clone());
        let mut services = production.services(Some(&self.history));
        services.editor = &editor;
        let save = SaveBridge {
            events: &self.events,
            cancel,
        };
        services.save_dialog = &save;
        let engine = Engine::new(
            self.settings.clone(),
            Naming::system(Arc::new(FileCounter::new(self.paths.counter_file()))),
        );
        engine.run(workflow, input, &services, &editor, cancel)
    }
    fn edit_and_save(
        &self,
        source: crate::image_editor::Source,
        cancel: &CancelToken,
    ) -> Result<Option<RunReport>> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.events.send(Event::Ui(UiRequest::StandaloneEdit {
            source,
            reply: tx,
            cancel: cancel.clone(),
        }))?;
        let mut progress = StandaloneProgress::default();
        let mut last_report = None;
        loop {
            let submission = match wait_borrowed(&rx, cancel) {
                Ok(Some(submission)) => submission,
                Ok(None) | Err(ServiceError::Cancelled) => return Ok(last_report),
                Err(error) => return Err(error.into()),
            };
            let identity = match image_identity(&submission.frame, cancel) {
                Ok(identity) => identity,
                Err(_) if cancel.is_cancelled() => return Ok(last_report),
                Err(error) => return Err(error),
            };
            let workflow = Workflow { id: "rcr-edit-image".into(), name: "Edit image".into(),
                after_capture: progress.prepare(identity), ..Default::default() };
            let mut report = self.execute(&workflow, Input::Image(core::workflow::Captured::new(submission.frame)), cancel);
            progress.observe(&report);
            progress.restore_metadata(&mut report);
            let completed = progress.complete();
            let completion = if completed { StandaloneCompletion::Succeeded } else {
                let action = if progress.saved_path.is_some() { OverlayAction::Copy } else { OverlayAction::Save };
                let detail = match overlay_completion(action, &report) {
                    OverlayCompletion::Failed(detail) => detail,
                    OverlayCompletion::Succeeded => "The requested save/copy did not complete.".into(),
                };
                StandaloneCompletion::Failed { detail, saved_path: progress.saved_path.clone() }
            };
            let closed = submission.completion.try_send(completion).is_err();
            if completed || closed || cancel.is_cancelled() { return Ok(Some(report)); }
            last_report = Some(report);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screenshot_delay_applies_only_before_new_capture() {
        assert_eq!(screenshot_delay_ms(700, None), 700);
        let mut options = ScreenshotOptions { cursor: false, explicit: None, captured: None };
        assert_eq!(screenshot_delay_ms(0, Some(&options)), 250);
        assert_eq!(screenshot_delay_ms(700, Some(&options)), 700);
        options.captured = Some(core::workflow::Captured::new(
            crate::effects::solid_frame(2, 2, [0, 0, 0, 255])));
        assert_eq!(screenshot_delay_ms(700, Some(&options)), 0);
    }
    #[test]
    fn frozen_picker_frame_opens_selected_without_recapture_or_settings_changes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path().join("config"));
        let (tx, rx) = mpsc::channel();
        let mut state = State::new(paths.clone(), dir.path().join("captures"), tx).unwrap();
        let before = std::fs::read(paths.settings_file()).unwrap();
        let mut frame = crate::effects::solid_frame(37, 29, [91, 47, 182, 255]);
        frame.origin = types::Point::new(-1234, 567);
        let expected = frame.clone();
        let job = thread::spawn(move || state.run(Action::Screenshot {
            target: CaptureTarget::Region, edit: true, upload: false,
            options: Some(ScreenshotOptions { cursor: true, explicit: Some(services::ExplicitTarget::Rect(frame.rect())),
                captured: Some(core::workflow::Captured::new(frame)) }),
        }, &CancelToken::new()));
        let Event::Ui(UiRequest::Capture { frame, choose_region, reply, .. }) = rx.recv_timeout(Duration::from_secs(5)).unwrap()
            else { panic!("Frozen capture must open the existing selection directly"); };
        assert!(!choose_region, "Do not make the user select the area twice");
        assert_eq!(frame, expected, "Use the frozen pixels and physical origin");
        reply.send(None).unwrap();
        assert!(job.join().unwrap().unwrap().is_none());
        assert_eq!(std::fs::read(paths.settings_file()).unwrap(), before);
        assert!(!dir.path().join("captures").exists(), "No output/clipboard workflow was requested");
    }
    #[test]
    fn completion_wakes_after_value_or_disconnection_is_visible() {
        let (tx, rx) = mpsc::sync_channel(1);
        let observations = Arc::new(Mutex::new(Vec::new()));
        let observed = observations.clone();
        let rx = Mutex::new(rx);
        let sender = CompletionSender::new(tx, move || {
            observed.lock().unwrap().push(match rx.lock().unwrap().try_recv() {
                Ok(42) => "value",
                Err(mpsc::TryRecvError::Disconnected) => "disconnected",
                _ => panic!("Completion wake ran before publication"),
            });
        });
        sender.try_send(42).unwrap();
        assert_eq!(*observations.lock().unwrap(), vec!["value"]);
        drop(sender);
        assert_eq!(*observations.lock().unwrap(), vec!["value", "disconnected"]);
    }
    #[test]
    fn completion_keeps_queue_bounded_and_wakes_if_worker_unwinds() {
        let (tx, rx) = mpsc::sync_channel(1);
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = wakes.clone();
        let sender = CompletionSender::new(tx, move || { observed.fetch_add(1, Ordering::Relaxed); });
        sender.try_send(1).unwrap();
        assert!(matches!(sender.try_send(2), Err(mpsc::TrySendError::Full(2))));
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(rx.try_recv().unwrap(), 1);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _sender = sender;
            panic!("Synthetic worker failure");
        })).is_err());
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
    }
    fn standalone_report(path: Option<PathBuf>, saved: bool, copied: bool) -> RunReport {
        use crate::core::workflow::{FailureKind, ItemReport, Outcome, StepFailure, StepKind, StepReport, StepStatus};
        let steps = [(StepKind::SaveToFile, saved), (StepKind::CopyImage, copied)].into_iter().map(|(kind, done)|
            StepReport { kind, item: Some(0), status: if done { StepStatus::Succeeded } else {
                StepStatus::Failed(StepFailure { kind: FailureKind::Io, message: "Synthetic failure".into(), retryable: true })
            }, detail: None, duration: Duration::ZERO }).collect();
        RunReport { workflow_id: "rcr-edit-image".into(), outcome: Outcome::PartialSuccess, steps: vec![],
            items: vec![ItemReport { index: 0, kind: crate::core::history::EntryKind::Image,
                input_path: None, local_path: path, created_by_workflow: saved,
                url: None, short_url: None, thumbnail_url: None, deletion_url: None, uploader: None,
                history_id: None, steps, outcome: Outcome::PartialSuccess }] }
    }
    #[test]
    fn standalone_partial_save_retries_copy_without_saving_twice_and_keeps_output_path() {
        let mut progress = StandaloneProgress::default();
        let identity = [5; 32];
        assert_eq!(progress.prepare(identity), vec![AfterCapture::SaveToFile, AfterCapture::CopyImageToClipboard]);
        let path = PathBuf::from("synthetic-output.png");
        progress.observe(&standalone_report(Some(path.clone()), true, false));
        assert_eq!(progress.prepare(identity), vec![AfterCapture::CopyImageToClipboard]);
        let mut retry = standalone_report(None, false, true);
        retry.items[0].steps.retain(|step| step.kind == crate::core::workflow::StepKind::CopyImage);
        progress.observe(&retry);
        progress.restore_metadata(&mut retry);
        assert!(progress.complete());
        assert_eq!(retry.items[0].local_path, Some(path));
        assert_eq!(retry.outcome, crate::core::workflow::Outcome::Success);
        assert_eq!(progress.prepare([6; 32]), vec![AfterCapture::SaveToFile, AfterCapture::CopyImageToClipboard]);
        assert!(progress.saved_path.is_none(), "Edited pixels reused another image's saved output");
    }
    #[test]
    fn standalone_partial_copy_retries_save_without_replacing_the_clipboard() {
        let mut progress = StandaloneProgress::default();
        let identity = [7; 32];
        progress.prepare(identity);
        progress.observe(&standalone_report(None, false, true));
        assert_eq!(progress.prepare(identity), vec![AfterCapture::SaveToFile]);
        let path = PathBuf::from("recovered-image.png");
        let mut retry = standalone_report(Some(path.clone()), true, false);
        retry.items[0].steps.retain(|step| step.kind == crate::core::workflow::StepKind::SaveToFile);
        progress.observe(&retry);
        progress.restore_metadata(&mut retry);
        assert!(progress.complete());
        assert_eq!(retry.items[0].local_path, Some(path));
        assert_eq!(retry.outcome, crate::core::workflow::Outcome::Success);
        assert_eq!(retry.all_steps().filter(|step| step.kind == crate::core::workflow::StepKind::CopyImage).count(), 1);
    }
    #[test]
    fn standalone_identity_covers_dimensions_and_pixels_and_is_cancellable() {
        let cancel = CancelToken::new();
        let first = crate::effects::solid_frame(8, 6, [12, 34, 56, 255]);
        let reshaped = Frame::from_rgba8(12, 4, first.data().to_vec()).unwrap();
        assert_ne!(image_identity(&first, &cancel).unwrap(), image_identity(&reshaped, &cancel).unwrap());
        let changed = crate::effects::solid_frame(8, 6, [13, 34, 56, 255]);
        assert_ne!(image_identity(&first, &cancel).unwrap(), image_identity(&changed, &cancel).unwrap());
        cancel.cancel();
        assert!(image_identity(&first, &cancel).is_err());
    }
    #[test]
    fn first_command_can_queue_during_initialization() {
        let dir = tempfile::tempdir().unwrap();
        let worker = Worker::new(
            Paths::rooted_at(dir.path().join("config")),
            dir.path().join("captures"),
        )
        .unwrap();
        worker.submit(Action::Refresh).unwrap();
        loop {
            if let Event::Finished(result) =
                worker.events.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                assert!(result.is_ok());
                break;
            }
        }
    }
    #[test]
    fn uploader_import_is_available_without_overwriting_existing_definitions() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path().join("config"));
        let (tx, _) = mpsc::channel();
        let mut state = State::new(paths, dir.path().join("captures"), tx).unwrap();
        let source = dir.path().join("host.sxcu");
        assert!(
            state
                .production()
                .uploads
                .list()
                .iter()
                .all(|i| i.name != "Test-Host")
        );
        std::fs::write(&source,r#"{"Version":"14.0.0","Name":"Test Host","DestinationType":"ImageUploader","RequestMethod":"POST","RequestURL":"http://127.0.0.1:9/upload","Body":"MultipartFormData","FileFormName":"file","URL":"{json:url}"}"#).unwrap();
        state
            .run(Action::ImportUploader(source.clone()), &CancelToken::new())
            .unwrap();
        let host = state
            .production()
            .uploads
            .list()
            .into_iter()
            .find(|i| i.name == "Test-Host")
            .unwrap();
        assert!(host.error.is_none());
        assert_eq!(host.uploads, vec!["image"]);
        assert!(
            state
                .run(Action::ImportUploader(source), &CancelToken::new())
                .is_err()
        );
        let before = std::fs::read(state.paths.settings_file()).unwrap();
        assert!(
            state
                .run(
                    Action::ImportSettings(dir.path().join("missing.toml")),
                    &CancelToken::new()
                )
                .is_err()
        );
        assert_eq!(std::fs::read(state.paths.settings_file()).unwrap(), before);
    }
    #[test]
    fn cancelled_interactive_step_does_not_wait_for_ui() {
        let (_tx, rx) = mpsc::sync_channel::<Option<Frame>>(1);
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(wait(rx, &cancel), Err(ServiceError::Cancelled)));
    }
    #[test]
    fn local_overlay_retries_do_not_repeat_uploads_or_after_upload_actions() {
        let original = Workflow {
            after_capture: vec![AfterCapture::OpenEditor, AfterCapture::SaveToFile, AfterCapture::Upload],
            after_upload: vec![AfterUpload::CopyUrl, AfterUpload::ShowNotification],
            ..Default::default()
        };
        let copy = overlay_workflow(&original, OverlayAction::Copy);
        assert_eq!(copy.after_capture, vec![AfterCapture::CopyImageToClipboard]);
        assert!(copy.after_upload.is_empty());
        let save = overlay_workflow(&original, OverlayAction::Save);
        assert_eq!(save.after_capture, vec![AfterCapture::SaveToFile]);
        assert!(save.after_upload.is_empty());
        let configured = overlay_workflow(&original, OverlayAction::Configured);
        assert_eq!(configured.after_capture, vec![AfterCapture::SaveToFile, AfterCapture::Upload]);
        assert_eq!(configured.after_upload, original.after_upload);
        assert!(original.after_capture.contains(&AfterCapture::OpenEditor));
    }
    #[test]
    fn overlay_ack_requires_the_requested_operation_to_have_succeeded() {
        use crate::core::workflow::{Outcome, StepKind, StepReport, StepStatus};
        let mut report = RunReport { workflow_id: "test".into(), outcome: Outcome::Success,
            items: vec![], steps: vec![] };
        assert!(matches!(overlay_completion(OverlayAction::Copy, &report), OverlayCompletion::Failed(_)));
        report.steps.push(StepReport { kind: StepKind::SaveToFile, item: Some(0),
            status: StepStatus::Succeeded, detail: None, duration: Duration::ZERO });
        assert!(matches!(overlay_completion(OverlayAction::Copy, &report), OverlayCompletion::Failed(_)));
        assert!(matches!(overlay_completion(OverlayAction::Save, &report), OverlayCompletion::Succeeded));
        report.outcome = Outcome::PartialSuccess;
        let OverlayCompletion::Failed(message) = overlay_completion(OverlayAction::Configured, &report) else {
            panic!("Partial workflow must retain the editable overlay")
        };
        assert!(message.contains("workflow will not run again"));
        report.steps[0].status = StepStatus::Failed(crate::core::workflow::StepFailure {
            kind: crate::core::workflow::FailureKind::Io, message: "界🦀".repeat(5_000), retryable: true,
        });
        let OverlayCompletion::Failed(message) = overlay_completion(OverlayAction::Save, &report) else {
            panic!("Failed save acknowledged as successful")
        };
        assert_eq!(message.chars().count(), 4_000);
        assert!(message.ends_with('🦀'));
    }
    #[test]
    fn local_workflow_preserves_image_and_records_history() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path().join("config"));
        let (tx, _) = mpsc::channel();
        let state = State::new(paths, dir.path().join("captures"), tx).unwrap();
        let frame = crate::effects::solid_frame(32, 20, [35, 70, 140, 255]);
        let workflow = Workflow {
            id: "proof".into(),
            name: "Proof".into(),
            after_capture: vec![AfterCapture::SaveToFile],
            ..Default::default()
        };
        let report = state.execute(
            &workflow,
            Input::Image(core::workflow::Captured::new(frame.clone())),
            &CancelToken::new(),
        );
        let path = report.items[0].local_path.as_ref().unwrap();
        assert_eq!(Frame::decode(&std::fs::read(path).unwrap()).unwrap(), frame);
        assert_eq!(state.history.count(&Query::default()).unwrap(), 1);
        assert!(report.urls().is_empty());
    }
}
