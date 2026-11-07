//! Native capture/replay ownership, independent of the view tree.
use crate::{
    preferences::Preferences,
    quick_recording,
    rune_theme::{self as theme, Btn, icons},
    rune_ui::{self as ui, State as AppState, color},
};
use rune::row;
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
impl State {
    /// What the rail light and title bar show, and the time beside it.
    pub fn status(&self) -> (ui::Status, String) {
        if self.stopping || !self.pending.is_empty() {
            (ui::Status::Saving, String::new())
        } else if self.quick.is_some() {
            (ui::Status::Recording, String::new())
        } else if self.active && self.recording {
            (ui::Status::Recording, ui::clock(self.stats.elapsed_seconds))
        } else if self.active {
            (ui::Status::Ready, ui::clock(self.stats.replay_seconds))
        } else {
            (ui::Status::Off, String::new())
        }
    }
}

pub fn view(s: &AppState) -> rune::El {
    let c = &s.capture;
    let p = &s.preferences;
    let keys = &p.hotkeys;
    let (status, _) = c.status();
    let recording = c.active && c.recording;
    let display = p
        .display
        .as_ref()
        .map(|d| d.display_label())
        .unwrap_or_else(|| "Primary display".into());
    // Left: the live preview and what goes into the capture.
    let mut badge = None;
    if c.active {
        badge = Some(
            row()
                .gap(7.0)
                .px(9.0)
                .py(5.0)
                .radius(5.0)
                .fill(theme::black(0.65))
                .center_y()
                .child(theme::dot(
                    7.0,
                    theme::c(if recording { theme::REC } else { theme::ACCENT }),
                ))
                .child(
                    rune::text(if recording {
                        format!("REC {}", ui::clock(c.stats.elapsed_seconds))
                    } else {
                        "Live".into()
                    })
                    .font(rune::TextStyle::new(12.0, 600.0).lh(16.0).features("'tnum' 1"))
                    .color(theme::c(theme::T1)),
                )
                .absolute(12.0, 12.0),
        );
    }
    let mut preview = rune::el()
        .key("capture-preview")
        .w_full()
        .aspect_ratio(16.0 / 9.0)
        .radius(8.0)
        .clip_rounded(8.0)
        .fill(color(0x000000))
        .native_surface(&c.surface, rune::SurfaceFit::Fit);
    if recording {
        preview = preview.outline(2.0, 0.0, theme::c(theme::REC));
    }
    if !c.active {
        preview = preview.center().child(
            rune::col()
                .gap(8.0)
                .items_center()
                .child(theme::glyph(&icons::DISPLAY, 28.0, theme::c(theme::T3)))
                .child(theme::small("Preview appears while capture is on")),
        );
    }
    if let Some(badge) = badge {
        preview = preview.child(badge);
    }
    let mut left = rune::col()
        .grow(1.0)
        .min_w(0.0)
        .gap(16.0)
        .child(
            row()
                .w_full()
                .gap(12.0)
                .center_y()
                .child(theme::h1("Capture").grow(1.0))
                .child(
                    row()
                        .gap(8.0)
                        .center_y()
                        .child(theme::glyph(&icons::DISPLAY, 16.0, theme::c(theme::T2)))
                        .child(theme::small(display)),
                ),
        )
        .child(preview)
        .child(
            row()
                .w_full()
                .gap(28.0)
                .center_y()
                .flex_wrap()
                .child(theme::switch_line(
                    "system-audio",
                    "Desktop audio",
                    p.system_audio,
                    |s, _| {
                        s.preferences.system_audio = !s.preferences.system_audio;
                        s.save_preferences();
                    },
                ))
                .child(theme::switch_line(
                    "microphone",
                    "Microphone",
                    p.microphone,
                    |s, _| {
                        s.preferences.microphone = !s.preferences.microphone;
                        s.save_preferences();
                    },
                ))
                .child(theme::spacer())
                .child(theme::small(format!(
                    "{}p · {} fps · AV1",
                    p.height, p.fps
                ))),
        );
    if c.active {
        left = left.child(theme::small(format!(
            "{} encoded frames · {} dropped",
            c.stats.encoded, c.stats.missed_deadlines
        )));
    }
    if let Some(e) = &c.preview_error {
        left = left.child(theme::small(format!("Preview unavailable: {e}")));
    }
    if c.fixture {
        left = left.child(theme::small(format!(
            "Generated GPU proof: {:?}; {}",
            c.surface.counts(),
            c.bridge
                .as_ref()
                .map(|b| b.diagnostic())
                .unwrap_or_default()
        )));
    }
    // Right: replay, recording and screenshots, one section each.
    let replay_on = c.active && !c.recording;
    let mut replay = rune::col()
        .w_full()
        .gap(14.0)
        .child(
            row()
                .w_full()
                .gap(10.0)
                .center_y()
                .child(theme::h2("Instant replay").grow(1.0))
                .child(
                    theme::switch(
                        "toggle-replay",
                        "Instant replay",
                        replay_on,
                        |s, _| {
                            s.capture
                                .toggle(false, &s.preferences, s.wake.clone(), &mut s.notice)
                        },
                    )
                    .disabled(recording || c.stopping),
                ),
        );
    if recording {
        replay = replay
            .child(
                Btn::new("save-replay", "Save replay")
                    .h(40.0)
                    .wide()
                    .hint(ui::keys(&keys[1]))
                    .disabled(c.stopping)
                    .on(|s, _| s.capture.save_replay(&s.preferences, &mut s.notice))
                    .build(),
            )
            .child(theme::small("Saves a separate clip. Recording continues."));
    } else {
        replay = replay
            .child(
                row()
                    .gap(10.0)
                    .items_end()
                    .child(
                        rune::text(ui::clock(if replay_on {
                            c.stats.replay_seconds
                        } else {
                            p.replay_seconds as f64
                        }))
                        .font(theme::BIG)
                        .color(theme::c(if replay_on { theme::T1 } else { theme::T3 })),
                    )
                    .child(
                        theme::t1(if replay_on {
                            "ready to save"
                        } else {
                            "off"
                        })
                        .color(theme::c(theme::T2))
                        .pb(4.0),
                    ),
            )
            .child(
                Btn::new("save-replay", "Save replay")
                    .primary()
                    .h(44.0)
                    .wide()
                    .hint(ui::keys(&keys[1]))
                    .disabled(!replay_on || c.stopping)
                    .on(|s, _| s.capture.save_replay(&s.preferences, &mut s.notice))
                    .build(),
            );
    }
    let mut record = rune::col().w_full().gap(12.0);
    if recording {
        record = record
            .child(theme::h2("Recording").color(theme::c(theme::REC_T)))
            .child(
                row()
                    .gap(12.0)
                    .center_y()
                    .child(theme::dot(12.0, theme::c(theme::REC)))
                    .child(
                        rune::text(ui::clock(c.stats.elapsed_seconds))
                            .font(theme::BIG)
                            .color(theme::c(theme::T1)),
                    ),
            )
            .child(
                Btn::new("start-recording", "Stop recording")
                    .rec()
                    .h(44.0)
                    .wide()
                    .lead(rune::el().size(10.0, 10.0).radius(2.0).fill(theme::c(theme::REC_T)))
                    .hint(ui::keys(&keys[2]))
                    .disabled(c.stopping)
                    .on(|s, _| {
                        s.capture
                            .toggle(true, &s.preferences, s.wake.clone(), &mut s.notice)
                    })
                    .build(),
            )
            .child(theme::small(format!(
                "{:.0} MB · {}",
                c.stats.compressed_bytes as f64 / 1e6,
                p.folder.display()
            )));
    } else {
        record = record
            .child(theme::h2("Recording"))
            .child(
                Btn::new("start-recording", "Start recording")
                    .h(40.0)
                    .wide()
                    .lead(theme::dot(9.0, theme::c(theme::REC)))
                    .hint(ui::keys(&keys[2]))
                    .disabled(c.active)
                    .on(|s, _| {
                        s.capture
                            .toggle(true, &s.preferences, s.wake.clone(), &mut s.notice)
                    })
                    .build(),
            )
            .child(theme::small(if c.active {
                "Turn off instant replay to start a recording."
            } else {
                "Records the display to disk with the replay buffer."
            }));
    }
    let shot = |key: &'static str, title: &'static str, i, target| {
        Btn::new(key, title)
            .icon(i)
            .wide()
            .on(move |s, _| crate::rune_tools::screenshot(s, target))
            .build()
    };
    use toolkit::core::workflow::CaptureTarget;
    let screenshots = rune::col()
        .w_full()
        .gap(12.0)
        .child(theme::h2("Screenshot"))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(shot("screenshot-region", "Region", &icons::REGION, CaptureTarget::Region))
                .child(shot("screenshot-window", "Window", &icons::WINDOW, CaptureTarget::Window))
                .child(shot(
                    "screenshot-screen",
                    "Screen",
                    &icons::DISPLAY,
                    CaptureTarget::Fullscreen,
                )),
        );
    let quick = rune::col()
        .w_full()
        .gap(12.0)
        .child(theme::h2("Quick screencast"))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    Btn::new(
                        "quick-record",
                        if c.quick.is_some() {
                            "Stop screencast"
                        } else {
                            "Record screencast"
                        },
                    )
                    .variant(if c.quick.is_some() {
                        theme::Variant::Rec
                    } else {
                        theme::Variant::Neutral
                    })
                    .wide()
                    .hint(ui::keys(&keys[6]))
                    .on(|s, _| s.capture.quick(&s.preferences, &mut s.notice))
                    .build(),
                )
                .child(theme::switch(
                    "quick-pause",
                    "Pause screencast",
                    c.quick_paused,
                    |s, _| {
                        s.capture.quick_paused = !s.capture.quick_paused;
                        if let Some(w) = &s.capture.quick {
                            w.pause(s.capture.quick_paused);
                        }
                    },
                ))
                .child(theme::small("Pause")),
        )
        .child(theme::small("Uses FFmpeg, without the game recorder."));
    let _ = status;
    let right = rune::col()
        .w(340.0)
        .gap(24.0)
        .pt(48.0)
        .child(replay)
        .child(theme::hr())
        .child(record)
        .child(theme::hr())
        .child(screenshots)
        .child(theme::hr())
        .child(quick);
    ui::scroll(
        row()
            .w_full()
            .pt(22.0)
            .px(32.0)
            .pb(32.0)
            .gap(40.0)
            .items_start()
            .child(left)
            .child(right),
    )
}
