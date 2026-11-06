//! Shared desktop picker and independent screencast lifecycle.
use crate::{
    quick_recording::{Event, Options, Worker},
    recordings,
    theme::*,
};
use gpui::{prelude::*, *};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use recording_tools::{source::CaptureTarget, stats::StatsSnapshot};
use ::toolkit::{
    core::workflow::{CancelToken, CaptureRequest, CaptureTarget as StillTarget, Capturer},
    services::{LastRegionStore, ScreenCapturer},
};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};
#[path = "capture_menu_proof.rs"]
pub(crate) mod menu_proof;
#[path = "capture_picker.rs"]
pub(crate) mod picker;
#[path = "quick_recorder_ui.rs"]
mod ui;

pub struct Saved;
impl EventEmitter<Saved> for QuickRecorder {}
pub struct ShowControls;
impl EventEmitter<ShowControls> for QuickRecorder {}
pub struct Screenshot {
    pub target: StillTarget,
    pub options: ::toolkit::worker::ScreenshotOptions,
}
impl EventEmitter<Screenshot> for QuickRecorder {}
struct PendingStart {
    at: Instant,
    options: Options,
}

pub struct QuickRecorder {
    probe: Option<menu_proof::Probe>,
    fixture: bool,
    snapshot_factory: fn(&CancelToken) -> anyhow::Result<picker::Desktop>,
    worker_factory: fn(Options) -> anyhow::Result<Worker>,
    worker: Option<Worker>,
    snapshot: Option<mpsc::Receiver<anyhow::Result<picker::PreparedDesktop>>>,
    selection: Option<mpsc::Receiver<Option<picker::Choice>>>,
    picker_window: Option<AnyWindowHandle>,
    cancel_selection: CancelToken,
    pending: Option<PendingStart>,
    settings: picker::Settings,
    recording: bool,
    finishing: bool,
    paused: bool,
    stats: StatsSnapshot,
    warnings: Vec<String>,
    notice: String,
    saved: Option<PathBuf>,
    copy_pending: bool,
}
fn desktop_snapshot(cancel: &CancelToken) -> anyhow::Result<picker::Desktop> {
    let capturer = ScreenCapturer::detect(None, LastRegionStore::none());
    let frame = capturer
        .capture(
            &CaptureRequest {
                target: StillTarget::Fullscreen,
                include_cursor: false,
                hdr: Default::default(),
            },
            cancel,
        )?
        .frame;
    let with_cursor = capturer
        .capture(
            &CaptureRequest {
                target: StillTarget::Fullscreen,
                include_cursor: true,
                hdr: Default::default(),
            },
            cancel,
        )?
        .frame;
    anyhow::ensure!(
        frame.rect() == with_cursor.rect(),
        "Display arrangement changed. Try capturing again."
    );
    // Reuse the capture backend. Constructing another Windows backend repeats
    // display/HDR discovery just to enumerate the same desktop once more.
    let (monitors, windows) = capturer.with(|source| Ok((source.monitors()?, source.windows()?)))?;
    let windows = windows
        .into_iter()
        .filter(|w| {
            if w.minimized {
                return false;
            }
            let Ok(raw) = w.id.parse::<isize>() else {
                return false;
            };
            let mut pid = 0;
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                    windows::Win32::Foundation::HWND(raw as *mut _),
                    Some(&mut pid),
                );
            }
            pid != std::process::id()
        })
        .take(256)
        .collect();
    Ok(picker::Desktop {
        frame,
        with_cursor,
        monitors,
        windows,
    })
}
impl QuickRecorder {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if view.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            probe: None,
            fixture: false,
            snapshot_factory: desktop_snapshot,
            worker_factory: Worker::start,
            worker: None,
            snapshot: None,
            selection: None,
            picker_window: None,
            cancel_selection: CancelToken::new(),
            pending: None,
            settings: Default::default(),
            recording: false,
            finishing: false,
            paused: false,
            stats: Default::default(),
            warnings: vec![],
            notice: String::new(),
            saved: None,
            copy_pending: false,
        }
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some()
            || self.snapshot.is_some()
            || self.selection.is_some()
            || self.pending.is_some()
    }
    pub fn recording(&self) -> bool {
        self.recording
    }
    pub fn shutdown(&mut self) {
        self.cancel_selection.cancel();
        self.pending = None;
        self.worker.take();
    }
    pub(crate) fn set_probe(&mut self, probe: menu_proof::Probe) {
        self.probe = Some(probe);
    }
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.recording {
            self.stop(cx);
            self.show_controls(cx);
        } else {
            self.show(cx);
        }
    }
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if self.worker.is_some() || self.pending.is_some() {
            self.show_controls(cx);
        } else {
            self.launch(true, cx);
        }
    }
    pub(crate) fn show_controls(&mut self, cx: &mut Context<Self>) {
        cx.emit(ShowControls);
        if !self.fixture {
            cx.defer(|cx| {
                if let Some(handle) = cx
                    .windows()
                    .into_iter()
                    .find(|h| h.downcast::<crate::Studio>().is_some())
                {
                    let _ = handle.update(cx, |_, window, _| window.activate_window());
                }
            });
        }
        cx.notify();
    }
    pub(crate) fn launch(&mut self, video: bool, cx: &mut Context<Self>) {
        if self.snapshot.is_some() || self.selection.is_some() {
            return;
        }
        if video && (self.worker.is_some() || self.pending.is_some()) {
            self.show_controls(cx);
            return;
        }
        self.settings.video = video;
        // Returning from the tray should reveal the active recording controls.
        cx.emit(ShowControls);
        self.notice.clear();
        self.cancel_selection = CancelToken::new();
        let cancel = self.cancel_selection.clone();
        let factory = self.snapshot_factory;
        let (tx, rx) = mpsc::sync_channel(1);
        self.snapshot = Some(rx);
        let fixture = self.fixture;
        if !fixture {
            cx.defer(|cx| {
                for handle in cx
                    .windows()
                    .into_iter()
                    .filter(|h| h.downcast::<crate::Studio>().is_some())
                {
                    let _ = handle.update(cx, |_, window, _| window.minimize_window());
                }
            });
        }
        cx.spawn(async move |_, cx| {
            if !fixture {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            }
            cx.background_executor()
                .spawn(async move {
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let desktop = factory(&cancel)?;
                            picker::PreparedDesktop::new(desktop, &cancel)
                        }))
                            .unwrap_or_else(|_| {
                                Err(anyhow::anyhow!("Could not prepare the capture screen"))
                            });
                    let _ = tx.send(result);
                })
                .await;
        })
        .detach();
        cx.notify();
    }
    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(worker) = &self.worker {
            worker.stop();
            self.finishing = true;
        }
        cx.notify();
    }
    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel_selection.cancel();
        self.pending = None;
        if let Some(worker) = &self.worker {
            worker.cancel();
            self.finishing = true;
        }
        self.notice = "Recording cancelled.".into();
        cx.notify();
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        if let Some(result) = self.snapshot.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.snapshot = None;
            if self.cancel_selection.is_cancelled() {
                self.show_controls(cx);
            } else {
                match result {
                    Ok(desktop) => {
                        let (tx, rx) = mpsc::sync_channel(1);
                        match picker::open(
                            desktop,
                            self.settings,
                            tx,
                            self.cancel_selection.clone(),
                            picker::Host {
                                allow_video: self.worker.is_none() && self.pending.is_none(),
                                hidden: self.fixture,
                                probe: self.probe.clone(),
                            },
                            cx,
                        ) {
                            Ok(window) => {
                                self.picker_window = Some(window);
                                self.selection = Some(rx);
                            }
                            Err(error) => {
                                self.notice = format!("Could not open capture: {error}");
                                self.show_controls(cx);
                            }
                        }
                    }
                    Err(error) => {
                        self.notice = format!("Could not prepare capture: {error}");
                        self.show_controls(cx);
                    }
                }
            }
            cx.notify();
        }
        if let Some(choice) = self.selection.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.selection = None;
            self.picker_window = None;
            if let Some(choice) = choice.filter(|_| !self.cancel_selection.is_cancelled()) {
                self.settings = choice.settings;
                if choice.settings.video {
                    self.saved = None;
                    self.warnings.clear();
                    self.stats = Default::default();
                    self.pending = Some(PendingStart {
                        at: Instant::now()
                            + Duration::from_millis(if self.fixture { 0 } else { 250 }),
                        options: Options {
                            target: choice.target,
                            cursor: choice.settings.cursor,
                            system_audio: choice.settings.sound,
                            microphone: choice.settings.microphone,
                            fps: choice.settings.fps,
                            folder: recordings::root().join("screencasts"),
                        },
                    });
                } else {
                    use ::toolkit::services::ExplicitTarget;
                    let (target, explicit) = match choice.target {
                        CaptureTarget::Region(rect) => {
                            (StillTarget::Region, ExplicitTarget::Rect(rect))
                        }
                        CaptureTarget::Monitor(id) => {
                            (StillTarget::Monitor, ExplicitTarget::Monitor(id))
                        }
                        CaptureTarget::Window(id) => {
                            (StillTarget::Window, ExplicitTarget::Window(id))
                        }
                        _ => unreachable!("picker only returns explicit desktop targets"),
                    };
                    cx.emit(Screenshot {
                        target,
                        options: ::toolkit::worker::ScreenshotOptions {
                            cursor: choice.settings.cursor,
                            explicit: Some(explicit),
                            captured: choice.captured,
                        },
                    });
                }
            } else {
                self.show_controls(cx);
            }
            cx.notify();
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|p| Instant::now() >= p.at)
        {
            let options = self.pending.take().unwrap().options;
            match (self.worker_factory)(options) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    self.notice = format!("Could not record: {error:#}");
                    self.show_controls(cx);
                }
            }
            cx.notify();
        }
        let (events, retired) = self.worker.as_ref().map(Worker::poll).unwrap_or_default();
        for event in events {
            match event {
                Event::Started => self.recording = true,
                Event::Progress(stats, warnings) => {
                    self.stats = stats;
                    self.warnings = warnings;
                }
                Event::Finishing => self.finishing = true,
                Event::Saved(path, warnings) => {
                    self.saved = Some(path);
                    self.warnings = warnings;
                    self.notice.clear();
                    cx.emit(Saved);
                }
                Event::Failed(error) => self.notice = error,
                Event::Cancelled => self.notice = "Video discarded.".into(),
            }
            cx.notify();
        }
        if retired {
            self.worker.take();
            self.recording = false;
            self.finishing = false;
            self.paused = false;
            self.show_controls(cx);
        }
    }
    fn copy(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(path) = self.saved.clone() else {
            return;
        };
        if self.copy_pending {
            return;
        }
        let Ok(native) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::Win32(native) = native.as_raw() else {
            return;
        };
        let owner = native.hwnd.get();
        self.copy_pending = true;
        if let Err(error) = recordings::begin_shell_action(
            recordings::ShellAction::CopyFile { owner },
            path,
            cx,
            |this, _, path, result, cx| {
                this.copy_pending = false;
                if this.saved.as_deref() == Some(path) {
                    this.notice = match result {
                        Ok(()) => "File copied. Paste into Discord or Slack.".into(),
                        Err(error) => error,
                    };
                }
                cx.notify();
            },
        ) {
            self.copy_pending = false;
            self.notice = error;
        }
        cx.notify();
    }
}
impl Drop for QuickRecorder {
    fn drop(&mut self) {
        self.shutdown();
    }
}
