//! Repeatable hidden-window evaluation against exclusively owned media copies.
//! Timings end at successful GPUI painting, not physical display completion.
use super::keyboard_smoke::history_signature;
use super::recovery_smoke::{Fixture, prepare, verify_and_cleanup};
use super::*;
use anyhow::{Context as _, ensure};
use std::path::Path;

pub struct ClipShowEvaluation {
    phase: u8,
    step: usize,
    started: Instant,
    requested: Instant,
    preparing: Option<mpsc::Receiver<Result<Vec<Fixture>, String>>>,
    verifying: Option<mpsc::Receiver<Result<(), String>>>,
    fixtures: Vec<Fixture>,
    signature: Option<serde_json::Value>,
    metrics: Vec<serde_json::Value>,
    target: u64,
    generation: u64,
    expected_generation: u64,
    observation: Option<PlaybackObservation>,
    audio_fixture: bool,
}
struct PlaybackObservation {
    requested: Instant,
    first_paint_observed_ms: f64,
    first_painted_ms: f64,
    ready_ms: Option<f64>,
    audio_clock_ms: Option<f64>,
    audio_tracks: usize,
    silent_sink_starts: u64,
}
impl Default for ClipShowEvaluation {
    fn default() -> Self {
        Self { phase: 0, step: 0, started: Instant::now(), requested: Instant::now(),
            preparing: None, verifying: None, fixtures: Vec::new(), signature: None,
            metrics: Vec::new(), target: 0, generation: 0, expected_generation: 0,
            observation: None, audio_fixture: false }
    }
}
impl ClipShowEvaluation {
    pub fn hidden(&self) -> bool { matches!(self.phase, 14 | 15) }
    fn metric(&mut self, name: &str, view: &EditorView) {
        let observed = self.observation.as_ref().filter(|observation| observation.requested == self.requested);
        let sample = serde_json::json!({"operation":name,
            "latency_ms":observed.map_or_else(|| self.requested.elapsed().as_secs_f64() * 1000., |observation| observation.first_paint_observed_ms),
            "first_frame_painted_ms":observed.map(|observation| observation.first_painted_ms),
            "playback_ready_ms":observed.and_then(|observation| observation.ready_ms),
            "audio_clock_advancing_ms":observed.and_then(|observation| observation.audio_clock_ms),
            "audio_tracks":observed.map(|observation| observation.audio_tracks),
            "silent_sink_starts":observed.map(|observation| observation.silent_sink_starts),
            "pipeline":view.load_diagnostics()});
        println!("Clip show evaluation sample: {sample}");
        self.metrics.push(sample);
    }
    fn wait_for_paint(&mut self, view: &EditorView, source: &Path) -> anyhow::Result<bool> {
        ensure!(self.requested.elapsed() < Duration::from_secs(10),
            "Clip show first frame exceeded 10s in phase {}: {}", self.phase, view.snapshot_status());
        ensure!(view.error.is_none(), "Clip show phase {} failed: {}", self.phase, view.snapshot_status());
        let painted = view.recording.as_deref() == Some(source) && view.ready
            && view.generation.load(Ordering::Relaxed) == self.expected_generation
            && view.painted_frame.get().is_some_and(|(_, at)| at >= self.requested);
        if !painted { return Ok(false); }
        ensure!(matches!(view.preview_output, PlaybackOutput::Silent), "Evaluation attempted physical audio output");
        let audio_tracks = view.model.project.tracks.iter().filter(|track| track.kind == TrackKind::Audio
            && view.model.project.clips.iter().any(|clip| clip.track == track.id)).count();
        self.audio_fixture |= audio_tracks > 0;
        if self.observation.as_ref().is_none_or(|observation| observation.requested != self.requested) {
            self.observation = Some(PlaybackObservation { requested: self.requested,
                first_paint_observed_ms: self.requested.elapsed().as_secs_f64() * 1000.,
                first_painted_ms: view.painted_frame.get().expect("painted frame").1.duration_since(self.requested).as_secs_f64() * 1000.,
                ready_ms: None, audio_clock_ms: None, audio_tracks, silent_sink_starts: 0 });
        }
        let session = view.preview.as_ref().context("Painted viewer lost its playback session")?;
        let observation = self.observation.as_mut().expect("playback observation");
        if session.ready() && observation.ready_ms.is_none() {
            observation.ready_ms = Some(self.requested.elapsed().as_secs_f64() * 1000.);
        }
        if audio_tracks == 0 { return Ok(session.ready()); }
        let diagnostics = session.audio_diagnostics();
        observation.silent_sink_starts = diagnostics.device_starts;
        if session.ready() && diagnostics.device_starts > 0 && diagnostics.clock_position_ns >= 10_000_000 {
            observation.audio_clock_ms = Some(self.requested.elapsed().as_secs_f64() * 1000.);
            return Ok(true);
        }
        Ok(false)
    }
}

impl EditorView {
    pub fn evaluate_clip_show(view: Entity<Self>, probe: &mut ClipShowEvaluation, source: &Path,
        window: &mut Window, cx: &mut App) -> anyhow::Result<bool> {
        ensure!(probe.started.elapsed() < Duration::from_secs(75),
            "Clip show evaluation timed out in phase {}: {}", probe.phase, view.read(cx).snapshot_status());
        if probe.phase == 0 {
            if probe.preparing.is_none() {
                let source = source.to_owned();
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || {
                    let result = (|| -> anyhow::Result<Vec<Fixture>> {
                        let asset = core::probe(&source)?;
                        ensure!(asset.duration_ns >= 1_000_000_000, "Evaluation needs at least one second of video");
                        let mut fixtures = Vec::new();
                        for _ in 0..3 {
                            let fixture = prepare(source.clone())?;
                            std::fs::remove_file(&fixture.sidecar)?;
                            fixtures.push(fixture);
                        }
                        Ok(fixtures)
                    })();
                    let _ = tx.send(result.map_err(|error| format!("{error:#}")));
                });
                probe.preparing = Some(rx);
                return Ok(false);
            }
            probe.fixtures = match probe.preparing.as_ref().expect("preparation receiver").try_recv() {
                Ok(result) => result.map_err(anyhow::Error::msg)?,
                Err(mpsc::TryRecvError::Empty) => return Ok(false),
                Err(error) => return Err(error.into()),
            };
            let choices = probe.fixtures.iter().enumerate().map(|(index, fixture)|
                (fixture.media.clone(), format!("Evaluation clip {}", index + 1))).collect();
            probe.requested = Instant::now();
            view.update(cx, |editor, cx| {
                editor.preview_output = PlaybackOutput::Silent;
                editor.set_library_choices(choices);
                editor.open_for_clip_show(probe.fixtures[0].media.clone(), cx);
            });
            probe.expected_generation = view.read(cx).generation.load(Ordering::Relaxed);
            probe.phase = 1;
            return Ok(false);
        }
        let paths = probe.fixtures.iter().map(|fixture| fixture.media.clone()).collect::<Vec<_>>();
        match probe.phase {
            1 => {
                if !probe.wait_for_paint(view.read(cx), &paths[0])? { return Ok(false); }
                probe.metric("cold_open_to_first_paint", view.read(cx));
                ensure!(view.read(cx).thumbnails.is_empty() && view.read(cx).waveforms.is_empty()
                    && view.read(cx).requested.is_empty(), "Watching generated editor-only media work");
                // Build both Undo and Redo branches through actual editor commands.
                probe.signature = Some(view.update(cx, |editor, cx| {
                    editor.pause();
                    editor.edit(Edit::Reframe { vertical: true, center: 0.25 }, false, cx);
                    editor.edit(Edit::Reframe { vertical: true, center: 0.75 }, false, cx);
                    editor.undo(cx);
                    history_signature(&mut editor.model)
                })?);
                probe.requested = Instant::now();
                view.update(cx, |editor, cx| editor.navigate_recording(true, cx));
                probe.expected_generation = view.read(cx).generation.load(Ordering::Relaxed);
                probe.phase = 2;
            }
            2..=5 => {
                let expected = match probe.phase { 2 | 4 => 1, 3 => 2, _ => 0 };
                if !probe.wait_for_paint(view.read(cx), &paths[expected])? { return Ok(false); }
                probe.metric(match probe.phase { 2 => "switch_A_to_B", 3 => "switch_B_to_C", 4 => "switch_C_to_B", _ => "switch_B_to_edited_A" }, view.read(cx));
                if probe.phase < 5 {
                    let forward = probe.phase == 2;
                    probe.requested = Instant::now();
                    view.update(cx, |editor, cx| editor.navigate_recording(forward, cx));
                    probe.expected_generation = view.read(cx).generation.load(Ordering::Relaxed);
                    probe.phase += 1;
                } else {
                    let signature = view.update(cx, |editor, _| history_signature(&mut editor.model))?;
                    ensure!(Some(signature) == probe.signature, "Switching lost project, link mode, Undo or Redo");
                    view.update(cx, |editor, _| editor.pause());
                    probe.step = 0;
                    probe.phase = 6;
                    probe.requested = Instant::now();
                    probe.target = view.read(cx).model.project.duration() / 5;
                    view.update(cx, |editor, cx| editor.seek(probe.target, cx));
                }
            }
            6 => {
                ensure!(probe.requested.elapsed() < Duration::from_secs(10), "Clip show exact seek exceeded 10s: {}", view.read(cx).snapshot_status());
                ensure!(view.read(cx).error.is_none(), "Clip show seek failed: {}", view.read(cx).snapshot_status());
                if !view.read(cx).painted_seek.get().is_some_and(|painted| painted.at >= probe.requested
                    && painted.identity.frame == probe.target && painted.identity.seek.is_some_and(|seek| seek.exact)) { return Ok(false); }
                ensure!(view.read(cx).scrub.as_ref().is_some_and(|scrub| !scrub.proxy_preparation_enabled()
                    && !scrub.seek_proxy_preparing() && !scrub.seek_proxy_ready()),
                    "Viewer seek started or reused editor proxy preparation");
                probe.metric(&format!("exact_seek_{}", probe.step + 1), view.read(cx));
                probe.step += 1;
                if probe.step < 3 {
                    probe.target = view.read(cx).model.project.duration() * [1, 2, 4][probe.step] / 5;
                    probe.requested = Instant::now();
                    view.update(cx, |editor, cx| editor.seek(probe.target, cx));
                } else {
                    probe.requested = Instant::now();
                    view.update(cx, |editor, cx| {
                        for _ in 0..25 {
                            editor.navigate_recording(true, cx);
                            editor.navigate_recording(true, cx);
                            editor.navigate_recording(false, cx);
                            editor.navigate_recording(false, cx);
                        }
                    });
                    probe.expected_generation = view.read(cx).generation.load(Ordering::Relaxed);
                    probe.phase = 7;
                }
            }
            7 => {
                if !probe.wait_for_paint(view.read(cx), &paths[0])? { return Ok(false); }
                probe.metric("rapid_100_switches_to_correct_first_paint", view.read(cx));
                let signature = view.update(cx, |editor, _| history_signature(&mut editor.model))?;
                ensure!(Some(signature) == probe.signature, "Rapid switches lost edited clip history");
                let missing = paths[0].with_file_name("owned-evaluation-missing-video.mp4");
                probe.requested = Instant::now();
                view.update(cx, |editor, cx| {
                    editor.set_library_choices(vec![(paths[0].clone(), "Edited clip".into()),
                        (missing, "Unavailable clip".into()), (paths[2].clone(), "Available clip".into())]);
                    editor.navigate_recording(true, cx);
                });
                probe.phase = 8;
            }
            8 => {
                ensure!(probe.requested.elapsed() < Duration::from_secs(10), "Unavailable clip never showed a recoverable error");
                if view.read(cx).error.is_none() { return Ok(false); }
                ensure!(!view.read(cx).loading && !view.read(cx).playing && view.read(cx).image.is_none(),
                    "Unavailable clip retained stale playing/image/loading state");
                probe.metric("unavailable_source_to_error", view.read(cx));
                probe.requested = Instant::now();
                view.update(cx, |editor, cx| editor.navigate_recording(true, cx));
                probe.expected_generation = view.read(cx).generation.load(Ordering::Relaxed);
                probe.phase = 9;
            }
            9 => {
                if !probe.wait_for_paint(view.read(cx), &paths[2])? { return Ok(false); }
                probe.metric("skip_unavailable_to_next_first_paint", view.read(cx));
                probe.requested = Instant::now();
                view.update(cx, |editor, cx| {
                    editor.set_library_choices(vec![(paths[2].clone(), "Tail clip".into()),
                        (paths[1].clone(), "Next clip".into()), (paths[0].clone(), "Edited clip".into())]);
                    let tail = editor.model.project.duration().saturating_sub(u64::from(editor.model.project.fps) / 4 + 1);
                    editor.seek(tail, cx);
                    editor.request_preview(true);
                });
                probe.expected_generation = view.read(cx).generation.load(Ordering::Relaxed) + 1;
                probe.phase = 10;
            }
            10 => {
                if !probe.wait_for_paint(view.read(cx), &paths[1])? { return Ok(false); }
                probe.metric("automatic_next_including_quarter_second_tail", view.read(cx));
                probe.generation = view.read(cx).generation.load(Ordering::Relaxed);
                probe.requested = Instant::now();
                view.update(cx, |editor, cx| {
                    editor.set_library_choices(vec![(paths[1].clone(), "Final clip".into())]);
                    let tail = editor.model.project.duration().saturating_sub(u64::from(editor.model.project.fps) / 4 + 1);
                    editor.seek(tail, cx);
                    editor.request_preview(true);
                });
                probe.phase = 11;
            }
            11 => {
                ensure!(probe.requested.elapsed() < Duration::from_secs(10), "Final clip did not stop at queue end");
                if view.read(cx).playing || view.read(cx).preview_due.is_some()
                    || !view.read(cx).preview.as_ref().is_some_and(PlaybackSession::finished) { return Ok(false); }
                ensure!(view.read(cx).generation.load(Ordering::Relaxed) == probe.generation,
                    "Automatic playback wrapped at the end of the queue");
                probe.metric("queue_end_stop_including_quarter_second_tail", view.read(cx));
                probe.requested = Instant::now();
                view.update(cx, |editor, cx| editor.open_for_clip_show(paths[0].clone(), cx));
                probe.expected_generation = view.read(cx).generation.load(Ordering::Relaxed);
                probe.phase = 12;
            }
            12 => {
                if !probe.wait_for_paint(view.read(cx), &paths[0])? { return Ok(false); }
                probe.metric("reopen_edited_clip_after_error_and_autoplay", view.read(cx));
                let signature = view.update(cx, |editor, _| history_signature(&mut editor.model))?;
                ensure!(Some(signature) == probe.signature, "Evaluation changed preserved edit history");
                // Viewer controls affect monitoring only, never the saved project.
                view.update(cx, |editor, cx| {
                    editor.toggle_viewer_mute(cx);
                    editor.adjust_viewer_volume(-0.4, cx);
                    editor.toggle_viewer_mute(cx);
                });
                let signature = view.update(cx, |editor, _| history_signature(&mut editor.model))?;
                ensure!(Some(signature) == probe.signature, "Playback volume changed project or edit history");
                probe.requested = Instant::now();
                view.update(cx, |editor, cx| {
                    editor.set_library_choices(vec![(paths[2].clone(), "Natural short clip".into()),
                        (paths[1].clone(), "Natural next clip".into())]);
                    editor.open_for_clip_show(paths[2].clone(), cx);
                });
                probe.phase = 13;
            }
            13 => {
                ensure!(probe.requested.elapsed() < Duration::from_secs(10), "Natural EOF fixture did not load");
                ensure!(view.read(cx).error.is_none(), "Natural EOF source failed: {}", view.read(cx).snapshot_status());
                if !view.read(cx).ready || view.read(cx).recording.as_deref() != Some(paths[2].as_path()) { return Ok(false); }
                probe.requested = Instant::now();
                view.update(cx, |editor, _| {
                    // A quarter-second owned project, with every source still
                    // starting at0, exercises real native EOF without a seek.
                    let duration = u64::from(editor.model.project.fps / 4).max(1);
                    for clip in &mut editor.model.project.clips { clip.duration = clip.duration.min(duration); }
                    editor.playhead = 0;
                    editor.remember();
                    editor.request_preview(true);
                });
                probe.expected_generation = view.read(cx).generation.load(Ordering::Relaxed);
                probe.phase = 16;
            }
            16 => {
                if !probe.wait_for_paint(view.read(cx), &paths[2])? { return Ok(false); }
                if probe.audio_fixture {
                    let audio = view.read(cx).preview.as_ref().context("Natural clip lost transport")?.audio_diagnostics();
                    ensure!(audio.native_decoders > 0 && audio.external_decoders == 0,
                        "Natural EOF fixture must use native audio, not seek fallback: {audio:?}");
                }
                probe.metric("natural_short_native_clip_ready", view.read(cx));
                probe.expected_generation += 1;
                probe.observation = None;
                probe.phase = 17;
            }
            17 => {
                if !probe.wait_for_paint(view.read(cx), &paths[1])? { return Ok(false); }
                if probe.audio_fixture {
                    let audio = view.read(cx).preview.as_ref().context("Natural next clip lost transport")?.audio_diagnostics();
                    ensure!(audio.native_decoders > 0 && audio.external_decoders == 0,
                        "Natural next clip must use native audio: {audio:?}");
                }
                probe.metric("natural_native_audio_eof_to_next_including_full_short_clip", view.read(cx));
                view.update(cx, |editor, cx| editor.hide(cx));
                probe.phase = 14;
            }
            14 => {
                let editor = view.read(cx);
                if !editor.pending_saves.is_empty() || editor.save_states.values().any(|state|
                    state.in_flight.is_some() || state.status != SaveStatus::Saved) { return Ok(false); }
                let expected_project = probe.signature.as_ref().context("Missing edit signature")?[0].clone();
                let fixtures = std::mem::take(&mut probe.fixtures);
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || {
                    let result = (|| -> anyhow::Result<()> {
                        let saved = Project::load(&fixtures[0].sidecar)?;
                        ensure!(serde_json::to_value(saved)? == expected_project, "Saved edits differ after clip show evaluation");
                        for fixture in fixtures { verify_and_cleanup(fixture)?; }
                        Ok(())
                    })();
                    let _ = tx.send(result.map_err(|error| format!("{error:#}")));
                });
                probe.verifying = Some(rx);
                probe.phase = 15;
            }
            15 => match probe.verifying.as_ref().expect("verification receiver").try_recv() {
                Ok(result) => {
                    result.map_err(anyhow::Error::msg)?;
                    let report = serde_json::json!({
                        "samples":probe.metrics,"total_ms":probe.started.elapsed().as_secs_f64() * 1000.,
                        "rapid_switch_count":100,"owned_source_copies":3,"original_media_and_edits_unchanged":true,
                        "audio_fixture":probe.audio_fixture,"playback_output":"Silent",
                        "scope":"Hidden native GPUI paint plus observed PCM readiness/software audio-clock advancement; source/generation guards; no physical audio output, audio/desktop capture, OS input or network"});
                    if let Some(output) = std::env::var_os("RCR_CLIP_SHOW_EVAL_OUTPUT") {
                        let output = PathBuf::from(output);
                        ensure!(output.is_absolute(), "RCR_CLIP_SHOW_EVAL_OUTPUT must be absolute");
                        std::fs::write(output, serde_json::to_vec_pretty(&report)?)?;
                    }
                    println!("Clip show evaluation JSON: {report}");
                    return Ok(true);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(error) => return Err(error.into()),
            },
            _ => anyhow::bail!("Unknown clip show evaluation phase {}", probe.phase),
        }
        let _ = window;
        Ok(false)
    }
}
