//! Native capture/replay ownership, independent of the view tree.
use crate::tray::QuickState;
use crate::features::Feature;
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
    quick_state: QuickState,
    quick_stats: recording_tools::stats::StatsSnapshot,
    quick_warnings: Vec<String>,
    source_choices: Vec<(Option<crate::preferences::SavedDisplay>, String)>,
    source_events: Option<
        mpsc::Receiver<
            anyhow::Result<(
                Vec<windows_capture::display::Info>,
                Vec<windows_capture::audio_endpoint::AudioEndpoint>,
            )>,
        >,
    >,
    audio_choices: Vec<windows_capture::audio_endpoint::AudioEndpoint>,
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
        if !prefs.features.allows(Feature::Recording) {
            *notice = "Recording and instant replay are disabled. See Settings > Features.".into();
            return;
        }
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
        if !prefs.features.allows(Feature::Recording) { *notice = "Instant replay is disabled.".into(); return; }
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
        if !prefs.features.allows(Feature::Screencasts) { *notice = "Screencasts are disabled.".into(); return; }
        if let Some(worker) = &self.quick {
            worker.stop();
            self.quick_state = QuickState::Finishing;
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
                self.quick_state = QuickState::Starting;
                self.quick_stats = Default::default();
                self.quick_warnings.clear();
                *notice = "Starting screencast…".into();
            }
            Err(e) => {
                self.quick_state = QuickState::Failed;
                *notice = format!("{e:#}");
            }
        }
    }
    fn refresh_sources(&mut self, notice: &mut String) {
        if self.source_events.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new().name("capture-source-list".into()).spawn(move || {
            let result = (|| {
                use windows_capture::audio_endpoint::{enumerate_audio_endpoints, AudioEndpointKind};
                let displays = windows_capture::display::enumerate()?;
                let mut audio = enumerate_audio_endpoints(AudioEndpointKind::Render)?;
                let microphones = enumerate_audio_endpoints(AudioEndpointKind::Capture)?;
                audio.truncated |= microphones.truncated;
                audio.unavailable_entries += microphones.unavailable_entries;
                audio.endpoints.extend(microphones.endpoints);
                if audio.truncated || audio.unavailable_entries > 0 {
                    anyhow::bail!("Some audio devices could not be listed. Refresh devices to retry; saved choices are unchanged.");
                }
                Ok((displays, audio.endpoints))
            })();
            let _ = tx.send(result);
        }) {
            Ok(_) => { self.source_events = Some(rx); *notice = "Loading capture devices…".into(); }
            Err(error) => *notice = format!("Could not load capture devices: {error}"),
        }
    }
    pub fn quick_tray_state(&self) -> QuickState {
        self.quick_state
    }
    pub fn pause_quick(&mut self) {
        if self.quick_state.can_pause() {
            self.quick_state = if self.quick_state == QuickState::Paused {
                QuickState::Recording
            } else {
                QuickState::Paused
            };
            if let Some(worker) = &self.quick {
                worker.pause(self.quick_state == QuickState::Paused);
            }
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
    pub fn poll(&mut self, notices: &mut Vec<(ui::NoticeKind, String)>) -> bool {
        let mut changed = false;
        if let Some(result) = self
            .source_events
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(anyhow::anyhow!(
                    "Device discovery stopped unexpectedly. Refresh devices to retry."
                ))),
            })
        {
            self.source_events = None;
            changed = true;
            match result {
                Ok((displays, audio)) => {
                    self.source_choices = displays
                        .into_iter()
                        .filter_map(|d| {
                            if d.unsupported_reason.is_some() {
                                return None;
                            }
                            let id = d.device_path?;
                            let label = format!("{} · {} × {}", d.label, d.width, d.height);
                            Some((
                                Some(crate::preferences::SavedDisplay {
                                    id,
                                    label: label.clone(),
                                }),
                                label,
                            ))
                        })
                        .collect();
                    self.audio_choices = audio;
                    notices.push((
                        ui::NoticeKind::Success,
                        "Capture devices refreshed. Selections apply to the next native session."
                            .into(),
                    ));
                }
                Err(error) => notices.push((
                    ui::NoticeKind::Error,
                    format!("Could not load capture devices: {error:#}"),
                )),
            }
        }
        let (events, native_retired) = self
            .events
            .as_ref()
            .map(|receiver| {
                quick_recording::collect_events(receiver, || {
                    self.worker.as_ref().is_some_and(JoinHandle::is_finished)
                })
            })
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
                    notices.push((ui::NoticeKind::Success, "Recording finished.".into()));
                }
                PipelineEvent::Error(e) => {
                    self.active = false;
                    self.stopping = false;
                    self.bridge = None;
                    notices.push((ui::NoticeKind::Error, e));
                }
                PipelineEvent::Notice(v) => notices.push((ui::NoticeKind::Error, v)),
                PipelineEvent::ReplaySaved(path) => {
                    notices.push((ui::NoticeKind::Success, format!("Saved {}", path.display())))
                }
                PipelineEvent::ReplayPending(job) => self.pending.push(job),
            }
        }
        if native_retired {
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
                notices.push(match job.complete() {
                    Ok(path) => (ui::NoticeKind::Success, format!("Saved {}", path.display())),
                    Err(e) => (ui::NoticeKind::Error, format!("Replay failed: {e:#}")),
                });
                changed = true;
            } else {
                i += 1;
            }
        }
        let (events, retired) = self.quick.as_ref().map(|w| w.poll()).unwrap_or_default();
        let mut terminal = false;
        for event in events {
            changed = true;
            match event {
                quick_recording::Event::Started => {
                    if self.quick_state == QuickState::Starting {
                        self.quick_state = QuickState::Recording;
                    }
                }
                quick_recording::Event::Progress(stats, warnings) => {
                    self.quick_stats = stats;
                    self.quick_warnings = warnings;
                    if stats.encoded > 0 && self.quick_state == QuickState::Starting {
                        self.quick_state = QuickState::Recording;
                    }
                }
                quick_recording::Event::Saved(path, warnings) => {
                    notices.push((
                        if warnings.is_empty() {
                            ui::NoticeKind::Success
                        } else {
                            ui::NoticeKind::Error
                        },
                        format!("Saved {} {}", path.display(), warnings.join("; ")),
                    ));
                    self.quick_state = QuickState::Idle;
                    terminal = true;
                }
                quick_recording::Event::Failed(e) => {
                    notices.push((ui::NoticeKind::Error, e));
                    self.quick_state = QuickState::Failed;
                    terminal = true;
                }
                quick_recording::Event::Cancelled => {
                    self.quick_state = QuickState::Idle;
                    terminal = true;
                }
                quick_recording::Event::Finishing => {
                    self.quick_state = QuickState::Finishing;
                    notices.push((ui::NoticeKind::Success, "Finishing screencast…".into()));
                }
            }
        }
        if terminal || retired {
            if retired && !terminal {
                self.quick_state = QuickState::Failed;
                notices.push((
                    ui::NoticeKind::Error,
                    "Screencast worker exited without a result. Try again.".into(),
                ));
            }
            self.quick = None;
            changed = true;
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
    pub fn feature_change_blocked(&self, feature: Feature) -> bool {
        match feature {
            Feature::Recording => self.worker.is_some() || self.source_events.is_some() || !self.pending.is_empty(),
            Feature::Screencasts => self.quick.is_some(),
            _ => false,
        }
    }
    /// What the rail light and title bar show, and the time beside it.
    pub fn status(&self) -> (ui::Status, String) {
        if self.stopping || !self.pending.is_empty() || self.quick_state == QuickState::Finishing {
            (ui::Status::Saving, String::new())
        } else if self.quick_state == QuickState::Recording {
            (
                ui::Status::Recording,
                ui::clock(self.quick_stats.recorded.as_secs_f64()),
            )
        } else if self.active && self.recording {
            (ui::Status::Recording, ui::clock(self.stats.elapsed_seconds))
        } else if matches!(self.quick_state, QuickState::Starting | QuickState::Paused) {
            (ui::Status::Ready, self.quick_state.label().into())
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
                    .font(
                        rune::TextStyle::new(12.0, 600.0)
                            .lh(16.0)
                            .features("'tnum' 1"),
                    )
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
                        .child(theme::small(display.clone())),
                ),
        )
        .child(preview)
        .child(
            row()
                .w_full()
                .gap(28.0)
                .center_y()
                .flex_wrap()
                .child(
                    theme::switch_line("system-audio", "Desktop audio", p.system_audio, |s, _| {
                        s.preferences.system_audio = !s.preferences.system_audio;
                        s.save_preferences();
                    })
                    .disabled(c.active || c.quick.is_some()),
                )
                .child(
                    theme::switch_line("microphone", "Microphone", p.microphone, |s, _| {
                        s.preferences.microphone = !s.preferences.microphone;
                        s.save_preferences();
                    })
                    .disabled(c.active || c.quick.is_some()),
                )
                .child(theme::spacer())
                .child(theme::small(format!("{}p · {} fps · AV1", p.height, p.fps))),
        );
    let source_locked = c.active || c.worker.is_some();
    let mut sources = rune::col().w_full().gap(8.0)
        .child(theme::h2("Native capture sources"))
        .child(theme::small("Applies to the next recording or instant replay session. Stop capture to change sources. Unavailable saved devices fail explicitly; they are never replaced by name.").wrap().w_full().min_w(0.0))
        .child(theme::small(format!("Display: {}", display)))
        .child(Btn::new("refresh-capture-sources", if c.source_events.is_some() { "Loading devices…" } else { "Refresh devices" })
            .disabled(c.source_events.is_some()).on(|s, _| if s.require(Feature::Recording) { s.capture.refresh_sources(&mut s.notice) }).build())
        .child(Btn::new("capture-primary", "Use Windows primary display").disabled(source_locked)
            .checked(p.display.is_none()).on(|s, _| { s.preferences.display = None; s.save_preferences(); }).build());
    for (index, (choice, label)) in c.source_choices.iter().enumerate() {
        let choice = choice.clone();
        let selected = p.display.as_ref().map(|d| &d.id) == choice.as_ref().map(|d| &d.id);
        sources = sources.child(
            Btn::new(format!("capture-display-{index}"), label.clone())
                .checked(selected)
                .disabled(source_locked)
                .on(move |s, _| {
                    s.preferences.display = choice.clone();
                    s.save_preferences();
                })
                .build(),
        );
    }
    use windows_capture::audio_endpoint::AudioEndpointKind;
    for (microphone, kind, title, saved) in [
        (
            false,
            AudioEndpointKind::Render,
            "Desktop audio",
            &p.system_audio_device,
        ),
        (
            true,
            AudioEndpointKind::Capture,
            "Microphone",
            &p.microphone_device,
        ),
    ] {
        let enabled = if microphone {
            p.microphone
        } else {
            p.system_audio
        };
        sources = sources
            .child(theme::small(format!(
                "{}: {}{}",
                title,
                saved
                    .as_ref()
                    .map(|e| e.display_label())
                    .unwrap_or_else(|| "Windows default endpoint".into()),
                if enabled { "" } else { " · disabled" }
            )))
            .child(
                Btn::new(
                    format!("capture-audio-default-{microphone}"),
                    format!("{title}: use Windows default"),
                )
                .checked(saved.is_none())
                .disabled(source_locked)
                .on(move |s, _| {
                    if microphone {
                        s.preferences.microphone_device = None;
                    } else {
                        s.preferences.system_audio_device = None;
                    }
                    s.save_preferences();
                })
                .build(),
            );
        for (index, endpoint) in c
            .audio_choices
            .iter()
            .filter(|e| e.kind == kind)
            .enumerate()
        {
            let choice = crate::preferences::SavedAudioEndpoint {
                id: endpoint.id.clone(),
                label: endpoint.name.clone(),
            };
            let selected = saved.as_ref().is_some_and(|saved| saved.id == choice.id);
            sources = sources.child(
                Btn::new(
                    format!("capture-audio-{microphone}-{index}"),
                    choice.display_label(),
                )
                .checked(selected)
                .disabled(source_locked)
                .on(move |s, _| {
                    if microphone {
                        s.preferences.microphone_device = Some(choice.clone());
                    } else {
                        s.preferences.system_audio_device = Some(choice.clone());
                    }
                    s.save_preferences();
                })
                .build(),
            );
        }
    }
    if s.allows(Feature::Recording) { left = left.child(sources); }
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
    let mut replay = rune::col().w_full().gap(14.0).child(
        row()
            .w_full()
            .gap(10.0)
            .center_y()
            .child(theme::h2("Instant replay").grow(1.0))
            .child(
                theme::switch("toggle-replay", "Instant replay", replay_on, |s, _| {
                    s.capture
                        .toggle(false, &s.preferences, s.wake.clone(), &mut s.notice)
                })
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
        replay =
            replay
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
                            theme::t1(if replay_on { "ready to save" } else { "off" })
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
                    .lead(
                        rune::el()
                            .size(10.0, 10.0)
                            .radius(2.0)
                            .fill(theme::c(theme::REC_T)),
                    )
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
                .child(shot(
                    "screenshot-region",
                    "Region",
                    &icons::REGION,
                    CaptureTarget::Region,
                ))
                .child(shot(
                    "screenshot-window",
                    "Window",
                    &icons::WINDOW,
                    CaptureTarget::Window,
                ))
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
                        if c.quick_state == QuickState::Finishing {
                            "Finishing screencast…"
                        } else if c.quick_state == QuickState::Starting {
                            "Cancel starting screencast"
                        } else if c.quick.is_some() {
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
                    .disabled(c.quick_state == QuickState::Finishing)
                    .on(|s, _| s.capture.quick(&s.preferences, &mut s.notice))
                    .build(),
                )
                .child(theme::switch(
                    "quick-pause",
                    "Pause screencast",
                    c.quick_state == QuickState::Paused,
                    |s, _| s.capture.pause_quick(),
                ).disabled(!c.quick_state.can_pause()))
                .child(theme::small("Pause")),
        )
        .child(theme::small(format!("{} · {}", c.quick_state.label(), ui::clock(c.quick_stats.recorded.as_secs_f64()))))
        .child(theme::small("FFmpeg MP4 · primary display with cursor · maximum 10 minutes (600 seconds).").wrap().w_full().min_w(0.0))
        .child(theme::small("Quick audio uses Windows default playback and microphone endpoints when enabled above. Native device selections do not apply.").wrap().w_full().min_w(0.0))
        .child(theme::small(c.quick_warnings.join("; ")));
    let _ = status;
    let mut right = rune::col()
        .w(340.0)
        .gap(24.0)
        .pt(48.0);
    if s.allows(Feature::Recording) { right = right.child(replay).child(theme::hr()).child(record); }
    if s.allows(Feature::Screenshots) { right = right.child(screenshots); }
    if s.allows(Feature::Screencasts) { right = right.child(quick); }
    if !s.allows(Feature::Recording) {
        left = rune::col().grow(1.0).gap(12.0)
            .child(theme::h1("Capture"))
            .child(theme::small("Choose a screenshot or screencast action. Audio options are in Settings > Audio."))
            .child(theme::small("Native recording and instant replay are disabled. You can keep using another recorder."));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_recorders_do_not_start_workers() {
        let mut prefs = Preferences::default();
        prefs.features.set(Feature::Recording, false);
        prefs.features.set(Feature::Screencasts, false);
        let mut state = State::default();
        let mut notice = String::new();
        state.toggle(false, &prefs, Arc::new(|| {}), &mut notice);
        state.toggle(true, &prefs, Arc::new(|| {}), &mut notice);
        state.quick(&prefs, &mut notice);
        state.save_replay(&prefs, &mut notice);
        assert!(state.worker.is_none());
        assert!(state.quick.is_none());
        assert!(!state.active);
        assert!(notice.contains("disabled"));
    }

    #[test]
    fn poll_preserves_error_and_later_success_as_distinct_typed_outcomes() {
        let mut state = State::default();
        let (tx, rx) = mpsc::channel();
        tx.send(PipelineEvent::Error("finalization failed".into()))
            .unwrap();
        tx.send(PipelineEvent::ReplaySaved(PathBuf::from("clip.mkv")))
            .unwrap();
        state.events = Some(rx);
        let mut notices = Vec::new();
        assert!(state.poll(&mut notices));
        assert_eq!(notices.len(), 2);
        assert!(matches!(notices[0].0, ui::NoticeKind::Error));
        assert_eq!(notices[0].1, "finalization failed");
        assert!(matches!(notices[1].0, ui::NoticeKind::Success));
    }

    #[test]
    fn pause_is_unavailable_outside_a_running_session() {
        let mut state = State::default();
        for phase in [
            QuickState::Idle,
            QuickState::Starting,
            QuickState::Finishing,
            QuickState::Failed,
        ] {
            state.quick_state = phase;
            state.pause_quick();
            assert_eq!(state.quick_tray_state(), phase);
        }
    }

    #[test]
    fn quick_lifecycle_does_not_claim_recording_while_starting_paused_or_saving() {
        let mut state = State::default();
        state.quick_state = QuickState::Starting;
        assert!(matches!(state.status().0, ui::Status::Ready));
        state.quick_state = QuickState::Recording;
        assert!(matches!(state.status().0, ui::Status::Recording));
        state.pause_quick();
        assert_eq!(state.quick_tray_state(), QuickState::Paused);
        assert!(matches!(state.status().0, ui::Status::Ready));
        state.pause_quick();
        assert_eq!(state.quick_tray_state(), QuickState::Recording);
        state.quick_state = QuickState::Finishing;
        assert!(matches!(state.status().0, ui::Status::Saving));
        state.quick_state = QuickState::Paused;
        state.active = true;
        state.recording = true;
        assert!(matches!(state.status().0, ui::Status::Recording));
    }
}
