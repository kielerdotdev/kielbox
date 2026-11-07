//! Native capture/replay ownership, independent of the view tree.
use crate::{
    preferences::Preferences,
    quick_recording,
    rune_ui::{State as AppState, button, check, color, heading, label, muted},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{SystemTime, UNIX_EPOCH},
};
use windows_capture::pipeline::{self, PipelineEvent, RecorderCommand};
#[derive(Default)]
pub struct State {
    worker: Option<JoinHandle<()>>,
    events: Option<mpsc::Receiver<PipelineEvent>>,
    commands: Option<mpsc::SyncSender<RecorderCommand>>,
    stop: Arc<AtomicBool>,
    pub stats: media_core::PipelineStats,
    pub active: bool,
    pub stopping: bool,
    recording: bool,
    surface: rune::NativeSurface,
    bridge: Option<crate::rune_gpu::Bridge>,
    preview_error: Option<String>,
    quick: Option<quick_recording::Worker>,
    quick_paused: bool,
    pending: Vec<pipeline::ReplaySave>,
    fixture: bool,
}
fn unique(folder: &std::path::Path, prefix: &str, extension: &str) -> PathBuf {
    folder.join(format!(
        "{prefix}-{}.{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        extension
    ))
}
impl State {
    pub fn cpu_fixture(&mut self) {
        self.fixture = true;
        let _ = self
            .surface
            .push_bytes(rune::native_surface::synthetic(64, 64, 0), 1);
    }
    pub fn gpu_fixture(&mut self) {
        self.fixture = true;
        match crate::rune_gpu::fixture(self.surface.clone()) {
            Ok(mut bridge) => {
                // Only the generated fixture waits; live preview refresh is nonblocking.
                for _ in 0..100 {
                    match bridge.refresh() {
                        Ok(true) => break,
                        Ok(false) => std::thread::sleep(std::time::Duration::from_millis(1)),
                        Err(e) => {
                            self.preview_error = Some(e.to_string());
                            break;
                        }
                    }
                }
                self.bridge = Some(bridge);
            }
            Err(error) => self.preview_error = Some(format!("{error:#}")),
        }
    }
    pub fn toggle(
        &mut self,
        recording: bool,
        prefs: &Preferences,
        wake: rune::Waker,
        notice: &mut String,
    ) {
        if self.active {
            self.stop.store(true, Ordering::Release);
            self.stopping = true;
            return;
        }
        if self.worker.is_some() {
            *notice = "The previous recording is still finishing.".into();
            return;
        }
        let mut audio = Vec::new();
        if prefs.system_audio {
            let mut t = media_core::AudioTrack::system();
            t.endpoint_id = prefs.system_audio_device.as_ref().map(|e| e.id.clone());
            audio.push(t);
        }
        if prefs.microphone {
            let mut t = media_core::AudioTrack::microphone();
            t.endpoint_id = prefs.microphone_device.as_ref().map(|e| e.id.clone());
            audio.push(t);
        }
        let display = Some(
            prefs
                .display
                .as_ref()
                .map_or(windows_capture::display::Selection::Primary, |d| {
                    windows_capture::display::Selection::DevicePath(d.id.clone())
                }),
        );
        let options = pipeline::RunOptions {
            video: media_core::VideoConfig {
                width: prefs.height * 16 / 9,
                height: prefs.height,
                fps: prefs.fps,
                bitrate: prefs.bitrate,
                keyframe_interval: prefs.capture_priority.keyframe_interval(prefs.fps),
            },
            display,
            seconds: None,
            output: unique(&prefs.folder, "recording", "mkv"),
            preview: true,
            replay_seconds: prefs.replay_seconds,
            audio,
            record_to_disk: recording,
            suppress_microphone: prefs.clean_microphone && prefs.microphone,
            ..Default::default()
        };
        let (tx, events) = mpsc::sync_channel(32);
        let (commands, rx) = mpsc::sync_channel(8);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let fail = tx.clone();
        match std::thread::Builder::new()
            .name("kielbox-native-capture".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pipeline::run_controlled(options, worker_stop, Some(tx), Some(rx))
                }));
                match result {
                    Ok(Err(e)) => {
                        let _ = fail.send(PipelineEvent::Error(format!("{e:#}")));
                    }
                    Err(_) => {
                        let _ = fail.send(PipelineEvent::Error(
                            "Capture worker stopped unexpectedly.".into(),
                        ));
                    }
                    _ => {}
                }
                wake();
            }) {
            Ok(worker) => {
                self.worker = Some(worker);
                self.events = Some(events);
                self.commands = Some(commands);
                self.stop = stop;
                self.active = true;
                self.stopping = false;
                self.recording = recording;
                self.stats = Default::default();
                self.preview_error = None;
                *notice = "Starting capture…".into();
            }
            Err(e) => *notice = e.to_string(),
        }
    }
    pub fn save_replay(&mut self, prefs: &Preferences, notice: &mut String) {
        if !self.active || self.stopping {
            *notice = "Start instant replay before saving a clip.".into();
            return;
        }
        let command = RecorderCommand::SaveReplay {
            path: unique(&prefs.folder, "replay", "mkv"),
            seconds: prefs.replay_seconds,
            suppress_microphone: prefs.microphone && prefs.clean_microphone,
        };
        match self.commands.as_ref().map(|tx| tx.try_send(command)) {
            Some(Ok(())) => *notice = "Saving replay…".into(),
            _ => *notice = "Replay is busy. Try again after the current save finishes.".into(),
        }
    }
    pub fn quick(&mut self, prefs: &Preferences, notice: &mut String) {
        if let Some(worker) = &self.quick {
            worker.stop();
            *notice = "Finishing screencast…".into();
            return;
        }
        let options = quick_recording::Options {
            target: recording_tools::source::CaptureTarget::Desktop,
            cursor: true,
            system_audio: prefs.system_audio,
            microphone: prefs.microphone,
            fps: prefs.fps.min(60),
            folder: prefs.folder.clone(),
        };
        match quick_recording::Worker::start(options) {
            Ok(w) => {
                self.quick = Some(w);
                *notice = "Starting screencast…".into();
            }
            Err(e) => *notice = format!("{e:#}"),
        }
    }
    pub fn tray_state(&self) -> crate::tray::TrayState {
        crate::tray::TrayState {
            active: self.active,
            stopping: self.stopping,
            recording: self.recording,
            replay_ready: self.stats.encoded > 0,
            replay_seconds: self.stats.replay_seconds as u64,
        }
    }
    pub fn poll(&mut self, notice: &mut String) -> bool {
        let mut changed = false;
        let events: Vec<_> = self
            .events
            .as_ref()
            .map(|e| e.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            changed = true;
            match event {
                PipelineEvent::Started(p) => {
                    match crate::rune_gpu::Bridge::new(p.texture, self.surface.clone()) {
                        Ok(b) => self.bridge = Some(b),
                        Err(e) => self.preview_error = Some(format!("{e:#}")),
                    }
                }
                PipelineEvent::Stats(stats) => self.stats = stats,
                PipelineEvent::Finished(stats) => {
                    self.stats = stats;
                    self.active = false;
                    self.stopping = false;
                    self.bridge = None;
                    *notice = "Recording finished.".into();
                }
                PipelineEvent::Error(e) => {
                    self.active = false;
                    self.stopping = false;
                    self.bridge = None;
                    *notice = e;
                }
                PipelineEvent::Notice(v) => *notice = v,
                PipelineEvent::ReplaySaved(path) => *notice = format!("Saved {}", path.display()),
                PipelineEvent::ReplayPending(job) => self.pending.push(job),
            }
        }
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(w) = self.worker.take() {
                let _ = w.join();
            }
            self.active = false;
            self.stopping = false;
            self.events = None;
            self.commands = None;
            self.bridge = None;
            changed = true;
        }
        if let Some(bridge) = &mut self.bridge {
            match bridge.refresh() {
                Ok(fresh) => changed |= fresh,
                Err(e) => {
                    self.preview_error = Some(format!("{e:#}"));
                    self.bridge = None;
                }
            }
        }
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i].is_finished() {
                let job = self.pending.swap_remove(i);
                *notice = match job.complete() {
                    Ok(path) => format!("Saved {}", path.display()),
                    Err(e) => format!("Replay failed: {e:#}"),
                };
                changed = true;
            } else {
                i += 1;
            }
        }
        let events: Vec<_> = self
            .quick
            .as_ref()
            .map(|w| w.events.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            changed = true;
            match event {
                quick_recording::Event::Saved(path, warnings) => {
                    *notice = format!("Saved {} {}", path.display(), warnings.join("; "));
                    self.quick = None;
                }
                quick_recording::Event::Failed(e) => {
                    *notice = e;
                    self.quick = None;
                }
                quick_recording::Event::Cancelled => self.quick = None,
                quick_recording::Event::Finishing => *notice = "Finishing screencast…".into(),
                _ => {}
            }
        }
        changed
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.events.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        for job in &self.pending {
            job.cancel();
        }
    }
}
pub fn view(s: &AppState) -> rune::El {
    let c = &s.capture;
    let mut page = rune::col()
        .grow(1.0)
        .min_h(0.0)
        .gap(16.0)
        .child(heading("Capture"))
        .child(muted("Game recording and instant replay · NVIDIA AV1"))
        .child(
            rune::row()
                .gap(8.0)
                .child(button(
                    "toggle-replay",
                    if c.active {
                        "Stop capture"
                    } else {
                        "Start instant replay"
                    },
                    |s, _| {
                        s.capture
                            .toggle(false, &s.preferences, s.wake.clone(), &mut s.notice)
                    },
                ))
                .child(
                    button("start-recording", "Record to disk", |s, _| {
                        s.capture
                            .toggle(true, &s.preferences, s.wake.clone(), &mut s.notice)
                    })
                    .disabled(c.active),
                )
                .child(
                    button("save-replay", "Save replay", |s, _| {
                        s.capture.save_replay(&s.preferences, &mut s.notice)
                    })
                    .disabled(!c.active || c.stopping),
                ),
        )
        .child(label(format!(
            "{} encoded frames · {} dropped",
            c.stats.encoded, c.stats.missed_deadlines
        )))
        .child(
            rune::el()
                .key("capture-preview")
                .grow(1.0)
                .min_h(120.0)
                .w_full()
                .fill(color(0x000000))
                .native_surface(&c.surface, rune::SurfaceFit::Fit),
        );
    if let Some(e) = &c.preview_error {
        page = page.child(label(format!("Preview unavailable: {e}")));
    }
    if c.fixture {
        page = page.child(label(format!(
            "Generated GPU proof: {:?}; {}",
            c.surface.counts(),
            c.bridge
                .as_ref()
                .map(|b| b.diagnostic())
                .unwrap_or_default()
        )));
    }
    page.child(heading("Quick screencast"))
        .child(muted(
            "Record a display using FFmpeg, without the game recorder.",
        ))
        .child(
            rune::row()
                .gap(8.0)
                .child(button(
                    "quick-record",
                    if c.quick.is_some() {
                        "Stop screencast"
                    } else {
                        "Record video"
                    },
                    |s, _| s.capture.quick(&s.preferences, &mut s.notice),
                ))
                .child(check("quick-pause", "Pause", c.quick_paused, |s, _| {
                    s.capture.quick_paused = !s.capture.quick_paused;
                    if let Some(w) = &s.capture.quick {
                        w.pause(s.capture.quick_paused);
                    }
                })),
        )
        .child(
            rune::row()
                .gap(8.0)
                .child(button("screenshot-region", "Screenshot region", |s, _| {
                    crate::rune_tools::screenshot(s, toolkit::core::workflow::CaptureTarget::Region)
                }))
                .child(button("screenshot-screen", "Screenshot screen", |s, _| {
                    crate::rune_tools::screenshot(
                        s,
                        toolkit::core::workflow::CaptureTarget::Fullscreen,
                    )
                }))
                .child(button("screenshot-window", "Screenshot window", |s, _| {
                    crate::rune_tools::screenshot(s, toolkit::core::workflow::CaptureTarget::Window)
                })),
        )
}
