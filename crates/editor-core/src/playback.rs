//! Persistent, bounded editor transport. Pause retains decoders and audio device.
use crate::{
    export::{playback_audio_plan, playback_video_plan},
    preview_size, Id, PreviewFrame, Project, TrackKind,
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Seek, SeekFrom},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
#[cfg(windows)]
#[path = "playback_gpu/transport.rs"]
mod gpu_transport;
mod output;

#[derive(Clone, Copy, Debug, Default)]
pub enum PlaybackOutput {
    #[default]
    Device,
    Silent,
}

#[derive(Default)]
struct WallClock {
    elapsed: Duration,
    anchor: Option<Instant>,
}
impl WallClock {
    fn elapsed(&self) -> Duration {
        self.elapsed + self.anchor.map_or(Duration::ZERO, |t| t.elapsed())
    }
    fn pause(&mut self) {
        self.elapsed = self.elapsed();
        self.anchor = None;
    }
    fn play(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(Instant::now());
        }
    }
}
#[derive(Clone, Copy)]
struct Level {
    muted: bool,
    gain: f32,
}
fn audio_levels(project: &Project) -> HashMap<Id, Level> {
    let solo = project
        .tracks
        .iter()
        .any(|track| track.kind == TrackKind::Audio && track.solo);
    project
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .map(|track| {
            (
                track.id,
                Level {
                    muted: track.muted || (solo && !track.solo),
                    gain: track.gain as f32,
                },
            )
        })
        .collect()
}
struct Pcm {
    samples: Mutex<VecDeque<[f32; 2]>>,
    wake: Condvar,
    done: AtomicBool,
}
impl Pcm {
    fn new() -> Self {
        Self {
            samples: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            done: AtomicBool::new(false),
        }
    }
    fn ready(&self, n: usize) -> bool {
        self.done.load(Ordering::Acquire) || self.samples.lock().unwrap().len() >= n
    }
    fn mix_into(&self, mix: &mut [[f32; 2]], gain: f32) {
        let mut pcm = self.samples.lock().unwrap();
        for frame in mix {
            if let Some(sample) = pcm.pop_front() {
                for channel in 0..2 {
                    if sample[channel].is_finite() {
                        frame[channel] += sample[channel] * gain;
                    }
                }
            }
        }
        drop(pcm);
        self.wake.notify_all();
    }
}
#[derive(Default)]
struct Process(Mutex<Option<Child>>, AtomicBool);
impl Process {
    fn kill(&self) {
        self.1.store(true, Ordering::Release);
        if let Some(child) = self.0.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }
    fn finish(&self, cancel: bool) -> Result<()> {
        if cancel {
            self.kill();
        }
        loop {
            // Keep Child reachable by kill, and never hold the mutex while
            // waiting for process exit. EOF on stdout need not mean exit yet.
            let status = {
                let mut slot = self.0.lock().unwrap();
                let Some(child) = slot.as_mut() else {
                    return Ok(());
                };
                match child.try_wait()? {
                    Some(status) => {
                        slot.take();
                        Some(status)
                    }
                    None => None,
                }
            };
            if let Some(status) = status {
                ensure!(
                    cancel || self.1.load(Ordering::Acquire) || status.success(),
                    "Playback decoder failed"
                );
                return Ok(());
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Ok(slot) = self.0.get_mut() {
            if let Some(mut child) = slot.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}
struct Shared {
    cancel: AtomicBool,
    requested: AtomicBool,
    video_ready: AtomicBool,
    audio_ready: AtomicBool,
    audio_clock: AtomicBool,
    audio_position: AtomicU64,
    audio_diagnostics: AudioDiagnostics,
    clock: Mutex<WallClock>,
    frames: Mutex<VecDeque<PreviewFrame>>,
    wake: Condvar,
    video_done: AtomicBool,
    processes: Mutex<Vec<Arc<Process>>>,
    levels: Mutex<HashMap<Id, Level>>,
    pcm: Vec<(Id, Arc<Pcm>)>,
    errors: mpsc::Sender<String>,
    decoded: AtomicU64,
    dropped: AtomicU64,
    #[cfg(windows)]
    gpu: Option<Arc<crate::playback_gpu::GpuState>>,
}
#[derive(Default)]
struct AudioDiagnostics {
    starts: AtomicU64,
    underruns: AtomicU64,
    waiting_for_pcm: AtomicU64,
}
/// Per-session counters for diagnosing the existing audio clock. They do not
/// sample audio, affect scheduling, or establish presentation/scanout timing.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct PlaybackAudioDiagnostics {
    pub device_starts: u64,
    pub underrun_pauses: u64,
    pub waiting_for_pcm_ticks: u64,
    pub clock_position_ns: u64,
}
impl Shared {
    fn signal_transport(&self) {
        // Pair notifications with GPU paused-state checks under the same mutex.
        // No driver/decoder operation holds this short-lived queue lock.
        let _guard = self.frames.lock().unwrap();
        self.wake.notify_all();
    }
    fn elapsed(&self) -> Duration {
        if self.audio_clock.load(Ordering::Acquire) {
            Duration::from_nanos(self.audio_position.load(Ordering::Acquire))
        } else {
            self.clock.lock().unwrap().elapsed()
        }
    }
    fn process(&self, command: &mut Command) -> Result<(Arc<Process>, std::process::ChildStdout)> {
        ensure!(!self.cancel.load(Ordering::Acquire), "Playback cancelled");
        let mut child = command.stdout(Stdio::piped()).spawn()?;
        let pipe = child.stdout.take().context("Missing decoder pipe")?;
        let process = Arc::new(Process(Mutex::new(Some(child)), AtomicBool::new(false)));
        {
            let mut processes = self.processes.lock().unwrap();
            // Finished cut batches must not accumulate for the life of the editor.
            processes.retain(|process| process.0.lock().unwrap().is_some());
            processes.push(process.clone());
        }
        if self.cancel.load(Ordering::Acquire) {
            process.kill();
        }
        Ok((process, pipe))
    }
    fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
        #[cfg(windows)]
        if let Some(gpu) = &self.gpu {
            gpu.cancel.store(true, Ordering::Release);
        }
        self.requested.store(false, Ordering::Release);
        self.clock.lock().unwrap().pause();
        self.signal_transport();
        for (_, p) in &self.pcm {
            p.wake.notify_all();
        }
        for child in self.processes.lock().unwrap().iter() {
            child.kill();
        }
    }
    fn audio_failed(&self) {
        let mut clock = self.clock.lock().unwrap();
        clock.elapsed = Duration::from_nanos(self.audio_position.load(Ordering::Acquire));
        if self.requested.load(Ordering::Acquire) && self.video_ready.load(Ordering::Acquire) {
            clock.play();
        }
        self.audio_clock.store(false, Ordering::Release);
        self.audio_ready.store(true, Ordering::Release);
    }
}

pub struct PlaybackSession {
    shared: Arc<Shared>,
    workers: Vec<thread::JoinHandle<()>>,
    pub errors: mpsc::Receiver<String>,
    start: u64,
    end: u64,
    fps: u32,
}
impl PlaybackSession {
    pub fn new(
        project: Project,
        start: u64,
        size: (u32, u32),
        output: PlaybackOutput,
    ) -> Result<Self> {
        #[cfg(windows)]
        {
            Self::new_inner(project, start, size, output, None)
        }
        #[cfg(not(windows))]
        {
            Self::new_inner(project, start, size, output)
        }
    }
    /// Explicit opt-in; unsupported video uses the existing CPU composition path.
    #[cfg(windows)]
    pub fn new_with_gpu(
        project: Project,
        start: u64,
        size: (u32, u32),
        output: PlaybackOutput,
        config: crate::GpuPlaybackConfig,
    ) -> Result<Self> {
        Self::new_inner(project, start, size, output, Some(config))
    }
    fn new_inner(
        project: Project,
        start: u64,
        size: (u32, u32),
        output: PlaybackOutput,
        #[cfg(windows)] gpu: Option<crate::GpuPlaybackConfig>,
    ) -> Result<Self> {
        project.validate()?;
        ensure!(project.duration() > 0, "The clip is empty");
        let start = start.min(project.duration() - 1);
        let end = project.duration();
        let fps = project.fps;
        let pcm: Vec<_> = project
            .tracks
            .iter()
            .filter(|t| {
                t.kind == TrackKind::Audio
                    && project
                        .clips
                        .iter()
                        .any(|c| c.track == t.id && c.end() > start)
            })
            .map(|t| (t.id, Arc::new(Pcm::new())))
            .collect();
        let levels = audio_levels(&project);
        let (tx, errors) = mpsc::channel();
        let audio = !pcm.is_empty();
        let shared = Arc::new(Shared {
            cancel: AtomicBool::new(false),
            requested: AtomicBool::new(false),
            video_ready: AtomicBool::new(false),
            audio_ready: AtomicBool::new(!audio),
            audio_clock: AtomicBool::new(audio),
            audio_position: AtomicU64::new(0),
            audio_diagnostics: AudioDiagnostics::default(),
            clock: Mutex::new(WallClock::default()),
            frames: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            video_done: AtomicBool::new(false),
            processes: Mutex::new(Vec::new()),
            levels: Mutex::new(levels),
            pcm,
            errors: tx,
            decoded: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            #[cfg(windows)]
            gpu: gpu.map(|config| Arc::new(crate::playback_gpu::GpuState::new(config))),
        });
        let mut result = Self {
            shared: shared.clone(),
            workers: Vec::new(),
            errors,
            start,
            end,
            fps,
        };
        let size = preview_size(&project, size);
        let video_project = project.clone();
        let state = shared.clone();
        result
            .workers
            .push(
                thread::Builder::new()
                    .name("rcr-play-video".into())
                    .spawn(move || {
                        match decode_selected_video(&video_project, start, size, &state) {
                            Err(e) if !state.cancel.load(Ordering::Acquire) => {
                                let _ = state.errors.send(format!("{e:#}"));
                            }
                            _ => {}
                        }
                        state.video_done.store(true, Ordering::Release);
                    })?,
            );
        for (id, buffer) in &shared.pcm {
            let id = *id;
            let buffer = buffer.clone();
            let project = project.clone();
            let state = shared.clone();
            result.workers.push(
                thread::Builder::new()
                    .name("rcr-play-audio-decode".into())
                    .spawn(move || {
                        match decode_audio(&project, start, id, &buffer, &state) {
                            Err(e) if !state.cancel.load(Ordering::Acquire) => {
                                let _ = state.errors.send(format!("{e:#}"));
                            }
                            _ => {}
                        }
                        buffer.done.store(true, Ordering::Release);
                        buffer.wake.notify_all();
                    })?,
            );
        }
        if audio {
            let state = shared.clone();
            let samples = (project.seconds(end - start) * 48_000.).round() as u64;
            result.workers.push(
                thread::Builder::new()
                    .name("rcr-play-audio-output".into())
                    .spawn(move || match render_audio(&state, samples, output) {
                        Err(e) if !state.cancel.load(Ordering::Acquire) => {
                            let _ = state.errors.send(format!("{e:#}"));
                            state.audio_failed();
                        }
                        _ => {}
                    })?,
            );
        }
        Ok(result)
    }
    pub fn play(&self) {
        self.shared.requested.store(true, Ordering::Release);
        if !self.shared.audio_clock.load(Ordering::Acquire)
            && self.shared.video_ready.load(Ordering::Acquire)
        {
            self.shared.clock.lock().unwrap().play();
        }
        self.shared.signal_transport();
    }
    pub fn pause(&self) {
        self.shared.requested.store(false, Ordering::Release);
        self.shared.clock.lock().unwrap().pause();
        self.shared.signal_transport();
    }
    pub fn ready(&self) -> bool {
        self.shared.video_ready.load(Ordering::Acquire)
            && self.shared.audio_ready.load(Ordering::Acquire)
    }
    pub fn position(&self) -> u64 {
        (self.start + (self.shared.elapsed().as_secs_f64() * f64::from(self.fps)).floor() as u64)
            .min(self.end)
    }
    pub fn audio_diagnostics(&self) -> PlaybackAudioDiagnostics {
        let diagnostics = &self.shared.audio_diagnostics;
        PlaybackAudioDiagnostics {
            device_starts: diagnostics.starts.load(Ordering::Relaxed),
            underrun_pauses: diagnostics.underruns.load(Ordering::Relaxed),
            waiting_for_pcm_ticks: diagnostics.waiting_for_pcm.load(Ordering::Relaxed),
            clock_position_ns: self.shared.audio_position.load(Ordering::Acquire),
        }
    }
    pub fn finished(&self) -> bool {
        self.position() >= self.end && self.shared.video_done.load(Ordering::Acquire)
    }
    /// Present the newest due frame. Never block the UI or accumulate latency.
    pub fn frame(&self) -> Option<PreviewFrame> {
        let position = self.position();
        let mut frames = self.shared.frames.lock().unwrap();
        let (latest, dropped) = due_frame(&mut frames, position);
        self.shared.dropped.fetch_add(dropped, Ordering::Relaxed);
        if latest.is_some() {
            #[cfg(windows)]
            if let Some(gpu) = &self.shared.gpu {
                gpu.output.lock().unwrap().take();
            }
            self.shared.wake.notify_all();
        }
        latest
    }
    /// Track changes affect the next device buffer; no FFmpeg/FFplay restart.
    pub fn set_audio(&self, project: &Project) {
        *self.shared.levels.lock().unwrap() = audio_levels(project);
    }
    pub fn decoded_frames(&self) -> u64 {
        self.shared.decoded.load(Ordering::Relaxed)
    }
    pub fn dropped_frames(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }
    pub fn decoder_processes(&self) -> usize {
        self.shared.processes.lock().unwrap().len()
    }
    /// Latest stable shared output. On CPU fallback it remains available until
    /// frame() returns the first CPU replacement, avoiding a blank transition.
    #[cfg(windows)]
    pub fn gpu_output(&self) -> Option<crate::GpuPlaybackOutput> {
        self.shared
            .gpu
            .as_ref()
            .and_then(|gpu| gpu.output.lock().unwrap().clone())
    }
    #[cfg(windows)]
    pub fn gpu_status(&self) -> Option<crate::GpuPlaybackStatus> {
        self.shared
            .gpu
            .as_ref()
            .map(|gpu| gpu.status.lock().unwrap().clone())
    }
    #[cfg(windows)]
    pub fn gpu_stats(&self) -> Option<crate::GpuPlaybackStats> {
        self.shared
            .gpu
            .as_ref()
            .map(|gpu| *gpu.stats.lock().unwrap())
    }
    /// Rendering failure requests video-only fallback; the audio device/clock stay.
    #[cfg(windows)]
    pub fn disable_gpu(&self) {
        if let Some(gpu) = &self.shared.gpu {
            gpu.disable();
            self.shared.signal_transport();
        }
    }
    pub fn stop(&self) {
        self.shared.stop();
    }
    /// Used on application shutdown and by headless lifecycle tests.
    pub fn shutdown(mut self) {
        self.stop();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
fn due_frame(frames: &mut VecDeque<PreviewFrame>, position: u64) -> (Option<PreviewFrame>, u64) {
    let mut latest = None;
    let mut dropped = 0;
    while frames.front().is_some_and(|f| f.frame <= position) {
        if latest.is_some() {
            dropped += 1;
        }
        latest = frames.pop_front();
    }
    (latest, dropped)
}
impl Drop for PlaybackSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn decode_selected_video(
    project: &Project,
    start: u64,
    size: (u32, u32),
    shared: &Shared,
) -> Result<()> {
    #[cfg(windows)]
    if let Some(gpu) = &shared.gpu {
        let validate = || {
            crate::validate_sources_cancellable(project, || shared.cancel.load(Ordering::Acquire))
        };
        if let Err(error) = validate() {
            gpu.status(crate::GpuPlaybackStatus::Failed(
                format!("{error:#}").chars().take(4096).collect(),
            ));
            return Err(error);
        }
        match gpu_transport::run(project, start, size, shared, gpu) {
            Ok(()) => {
                if !shared.cancel.load(Ordering::Acquire) && !gpu.disabled.load(Ordering::Acquire) {
                    gpu.status(crate::GpuPlaybackStatus::Finished);
                    return Ok(());
                }
            }
            Err(_) if shared.cancel.load(Ordering::Acquire) => return Ok(()),
            Err(error) if error.is::<gpu_transport::SourceChanged>() => {
                gpu.status(crate::GpuPlaybackStatus::Failed(
                    format!("{error:#}").chars().take(4096).collect(),
                ));
                return Err(error);
            }
            Err(error) => gpu.status(crate::GpuPlaybackStatus::Fallback(
                format!("{error:#}").chars().take(4096).collect(),
            )),
        }
        if shared.cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        // Renderer-requested fallback may complete without a decoder error.
        if gpu.disabled.load(Ordering::Acquire) {
            gpu.status(crate::GpuPlaybackStatus::Fallback(
                "GPU presentation disabled; using CPU video".into(),
            ));
        }
        if let Err(error) = validate() {
            gpu.status(crate::GpuPlaybackStatus::Failed(
                format!("{error:#}").chars().take(4096).collect(),
            ));
            return Err(error);
        }
        let resume = (start
            + (shared.elapsed().as_secs_f64() * f64::from(project.fps)).floor() as u64)
            .min(project.duration().saturating_sub(1));
        return decode_video(project, resume, size, shared);
    }
    decode_video(project, start, size, shared)
}
fn decode_video(project: &Project, start: u64, size: (u32, u32), shared: &Shared) -> Result<()> {
    let (width, height) = size;
    let fps = project.fps.min(60);
    let bytes = width as usize * height as usize * 4;
    ensure!(
        bytes <= 64 * 1024 * 1024,
        "Preview exceeds the image memory budget"
    );
    let capacity = (64 * 1024 * 1024 / bytes).clamp(1, 8);
    let mut current = open_video(project, start, size, fps, shared)?;
    loop {
        if shared.cancel.load(Ordering::Acquire) {
            break;
        }
        // One future batch is already decoding when the current batch ends.
        // This avoids a new-process/seek stall at a cut, with at most four
        // video input contexts alive across the current and future batches.
        let next = if current.end < project.duration() {
            Some(open_video(project, current.end, size, fps, shared)?)
        } else {
            None
        };
        let run = (|| -> Result<()> {
            let mut index = 0;
            while !shared.cancel.load(Ordering::Acquire) {
                let mut frames = shared.frames.lock().unwrap();
                while frames.len() >= capacity && !shared.cancel.load(Ordering::Acquire) {
                    frames = shared.wake.wait(frames).unwrap();
                }
                drop(frames);
                if shared.cancel.load(Ordering::Acquire) {
                    break;
                }
                let mut bgra = vec![0; bytes];
                let count = read_block(&mut current.pipe, &mut bgra)?;
                if count == 0 {
                    break;
                }
                ensure!(count == bytes, "Truncated preview frame");
                shared.frames.lock().unwrap().push_back(PreviewFrame {
                    width,
                    height,
                    frame: current.start + index * u64::from(project.fps) / u64::from(fps),
                    bgra,
                });
                shared.decoded.fetch_add(1, Ordering::Relaxed);
                #[cfg(windows)]
                if let Some(gpu) = &shared.gpu {
                    gpu.notify();
                }
                index += 1;
                if !shared.video_ready.swap(true, Ordering::AcqRel)
                    && shared.requested.load(Ordering::Acquire)
                    && !shared.audio_clock.load(Ordering::Acquire)
                {
                    shared.clock.lock().unwrap().play();
                }
            }
            Ok(())
        })();
        let reaped = current
            .child
            .finish(shared.cancel.load(Ordering::Acquire) || run.is_err());
        run?;
        reaped.with_context(|| {
            format!("Video preview decode: {}", decoder_diagnostic(&current.log))
        })?;
        if let Some(prepared) = next {
            current = prepared;
        } else {
            break;
        }
    }
    Ok(())
}
struct VideoDecoder {
    _plan: crate::RenderPlan,
    log: std::fs::File,
    child: Arc<Process>,
    pipe: std::process::ChildStdout,
    start: u64,
    end: u64,
}
impl Drop for VideoDecoder {
    fn drop(&mut self) {
        self.child.kill();
        let _ = self.child.finish(true);
    }
}
fn open_video(
    project: &Project,
    start: u64,
    size: (u32, u32),
    fps: u32,
    shared: &Shared,
) -> Result<VideoDecoder> {
    let plan = playback_video_plan(project, start, size.0, size.1, fps)?;
    let end =
        (start + (plan.seconds * f64::from(project.fps)).round() as u64).min(project.duration());
    ensure!(end > start, "Preview batch is empty");
    let log = tempfile::tempfile()?;
    let mut command = plan.command();
    command
        .args([
            "-map", "[vout]", "-an", "-pix_fmt", "bgra", "-f", "rawvideo", "pipe:1",
        ])
        .stderr(Stdio::from(log.try_clone()?));
    let (child, pipe) = shared.process(&mut command)?;
    Ok(VideoDecoder {
        _plan: plan,
        log,
        child,
        pipe,
        start,
        end,
    })
}

fn read_block(pipe: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut count = 0;
    while count < buffer.len() {
        match pipe.read(&mut buffer[count..]) {
            Ok(0) => break,
            Ok(n) => count += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(count)
}
fn decoder_diagnostic(log: &std::fs::File) -> String {
    let read = || -> std::io::Result<String> {
        let mut file = log.try_clone()?;
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(length.saturating_sub(4096)))?;
        let mut bytes = Vec::new();
        file.take(4096).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
    };
    read().unwrap_or_else(|_| "FFmpeg diagnostic unavailable".into())
}
fn decode_audio(
    project: &Project,
    start: u64,
    id: Id,
    buffer: &Pcm,
    shared: &Shared,
) -> Result<()> {
    let plan = playback_audio_plan(project, start, id)?;
    let log = tempfile::tempfile()?;
    let mut command = plan.command();
    command
        .args([
            "-map", "[mix]", "-vn", "-ar", "48000", "-ac", "2", "-f", "f32le", "pipe:1",
        ])
        .stderr(Stdio::from(log.try_clone()?));
    let (child, mut pipe) = shared.process(&mut command)?;
    let run = (|| -> Result<()> {
        let mut bytes = vec![0; 480 * 8];
        while !shared.cancel.load(Ordering::Acquire) {
            let count = read_block(&mut pipe, &mut bytes)?;
            if count == 0 {
                break;
            }
            ensure!(count.is_multiple_of(8), "Truncated preview PCM");
            let mut queue = buffer.samples.lock().unwrap();
            while queue.len() + count / 8 > 48_000 && !shared.cancel.load(Ordering::Acquire) {
                queue = buffer.wake.wait(queue).unwrap();
            }
            if shared.cancel.load(Ordering::Acquire) {
                break;
            }
            for raw in bytes[..count].as_chunks::<8>().0 {
                queue.push_back([
                    f32::from_le_bytes(raw[..4].try_into()?),
                    f32::from_le_bytes(raw[4..].try_into()?),
                ]);
            }
        }
        Ok(())
    })();
    // A failed reader can leave FFmpeg blocked writing the remaining PCM.
    // Terminate it before waiting, just as the video worker does.
    let reaped = child.finish(shared.cancel.load(Ordering::Acquire) || run.is_err());
    run?;
    reaped.with_context(|| {
        format!(
            "Audio preview decode (track {id}): {}",
            decoder_diagnostic(&log)
        )
    })
}
/// Endpoint state changes are acknowledged before updating the transport. An
/// underrun uses pause, then request cannot restart an empty endpoint.
#[derive(Default)]
struct AudioTransport {
    running: bool,
}
impl AudioTransport {
    fn pause(&mut self, mut transition: impl FnMut(bool) -> Result<()>) -> Result<()> {
        if self.running {
            transition(false)?;
            self.running = false;
        }
        Ok(())
    }
    fn request(
        &mut self,
        requested: bool,
        queued: usize,
        mut transition: impl FnMut(bool) -> Result<()>,
    ) -> Result<()> {
        let next = requested && (self.running || queued > 0);
        if next != self.running {
            transition(next)?;
            self.running = next;
        }
        Ok(())
    }
}

fn render_audio(shared: &Shared, total: u64, mode: PlaybackOutput) -> Result<()> {
    let mut device = output::Output::open(mode)?;
    let capacity = device.capacity() as usize;
    let mut written = 0u64;
    let mut transport = AudioTransport::default();
    let mut clock_offset = 0;
    let mut mix = vec![[0f32; 2]; capacity];
    while !shared.cancel.load(Ordering::Acquire) {
        let padding = device.padding()?;
        let device_position = device.position()?;
        let played = device_position.saturating_sub(clock_offset).min(written);
        let position_ns = played * 1_000_000_000 / 48_000;
        #[cfg(windows)]
        if shared.gpu.is_some() {
            let previous = shared.audio_position.swap(position_ns, Ordering::AcqRel);
            if previous != position_ns {
                shared.signal_transport();
            }
        } else {
            shared.audio_position.store(position_ns, Ordering::Release);
        }
        #[cfg(not(windows))]
        shared.audio_position.store(position_ns, Ordering::Release);
        if played >= total {
            break;
        }
        if transport.running && device_position.saturating_sub(clock_offset) > written {
            // Pause the transport through a decoder underrun instead of allowing
            // device-generated silence to move the video past missing audio.
            transport.pause(|_| device.pause())?;
            shared.audio_diagnostics.underruns.fetch_add(1, Ordering::Relaxed);
            clock_offset = device_position - written;
        }
        let wanted = (capacity - padding as usize).min((total - written) as usize);
        let ready = shared.video_ready.load(Ordering::Acquire)
            && shared.pcm.iter().all(|(_, p)| p.ready(wanted));
        if shared.video_ready.load(Ordering::Acquire)
            && wanted > 0 && !ready {
            shared.audio_diagnostics.waiting_for_pcm.fetch_add(1, Ordering::Relaxed);
        }
        let mut queued = padding as usize;
        if wanted > 0 && ready {
            mix[..wanted].fill([0.; 2]);
            let levels = shared.levels.lock().unwrap();
            for (id, buffer) in &shared.pcm {
                let gain = levels.get(id).filter(|l| !l.muted).map_or(0., |l| l.gain);
                buffer.mix_into(&mut mix[..wanted], gain);
            }
            drop(levels);
            for frame in &mut mix[..wanted] {
                frame[0] = frame[0].clamp(-0.98, 0.98);
                frame[1] = frame[1].clamp(-0.98, 0.98);
            }
            device.write(&mix[..wanted])?;
            written += wanted as u64;
            queued += wanted;
            shared.audio_ready.store(true, Ordering::Release);
        }
        let requested =
            shared.requested.load(Ordering::Acquire) && shared.audio_ready.load(Ordering::Acquire);
        transport.request(requested, queued, |running| {
            if running {
                device.play()?;
                shared.audio_diagnostics.starts.fetch_add(1, Ordering::Relaxed);
                Ok(())
            } else {
                device.pause()
            }
        })?;
        device.wait();
    }
    transport.pause(|_| device.pause())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Invoked only as an owned subprocess by the cancellation regression below.
    #[test]
    #[ignore = "owned child fixture; run through cancellation_reaps_child_after_stdout_eof"]
    fn stdout_eof_child() {
        if std::env::var_os("RCR_PLAYBACK_EXIT_CHILD").as_deref() != Some(std::ffi::OsStr::new("1"))
        {
            return;
        }
        #[cfg(windows)]
        unsafe {
            #[link(name = "kernel32")]
            extern "system" {
                fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
            }
            windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(
                GetStdHandle(-11i32 as u32),
            ))
            .unwrap();
        }
        #[cfg(unix)]
        unsafe {
            extern "C" {
                fn close(fd: i32) -> i32;
            }
            close(1);
        }
        thread::sleep(Duration::from_secs(30));
    }

    #[test]
    #[cfg(any(windows, unix))]
    fn cancellation_reaps_child_after_stdout_eof() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "playback::tests::stdout_eof_child",
                "--ignored",
                "--nocapture",
            ])
            .env("RCR_PLAYBACK_EXIT_CHILD", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let process = Arc::new(Process(Mutex::new(Some(child)), AtomicBool::new(false)));
        let (eof_sender, eof_receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut discarded = Vec::new();
            let result = stdout.read_to_end(&mut discarded);
            let _ = eof_sender.send(result);
        });
        match eof_receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => {
                result.unwrap();
            }
            Err(error) => {
                process.kill();
                process.finish(true).unwrap();
                reader.join().unwrap();
                panic!("owned fixture did not close stdout promptly: {error}");
            }
        }
        reader.join().unwrap();
        assert!(
            process
                .0
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none(),
            "fixture exited instead of waiting after EOF"
        );
        let finishing = process.clone();
        let (done, completion) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let result = finishing.finish(false);
            let _ = done.send(result);
        });
        thread::sleep(Duration::from_millis(25));
        let cancelled = Instant::now();
        process.kill();
        assert!(
            cancelled.elapsed() < Duration::from_secs(1),
            "cancellation waited behind process exit"
        );
        completion
            .recv_timeout(Duration::from_secs(2))
            .expect("cancelled child did not exit and reap")
            .unwrap();
        waiter.join().unwrap();
        assert!(
            process.0.lock().unwrap().is_none(),
            "completed child remained owned/unreaped"
        );
    }

    #[test]
    fn underrun_stays_stopped_until_refilled_and_resume_uses_existing_buffer() {
        let mut transport = AudioTransport::default();
        let mut transitions = Vec::new();
        let mut endpoint = |running| {
            transitions.push(running);
            Ok(())
        };
        transport.request(true, 960, &mut endpoint).unwrap();
        transport.pause(&mut endpoint).unwrap();
        for _ in 0..50 {
            transport.request(true, 0, &mut endpoint).unwrap();
        }
        assert!(!transport.running);
        transport.request(true, 480, &mut endpoint).unwrap();
        transport.request(false, 400, &mut endpoint).unwrap();
        transport.request(true, 400, &mut endpoint).unwrap();
        assert_eq!(transitions, [true, false, true, false, true]);
        assert!(transport.running);
    }

    #[test]
    fn failed_endpoint_start_never_acknowledges_running_or_restarts_empty() {
        let mut transport = AudioTransport::default();
        assert!(transport
            .request(true, 960, |_| Err(anyhow::anyhow!("device removed")))
            .is_err());
        assert!(!transport.running);
        transport
            .request(true, 0, |_| {
                panic!("empty endpoint restarted after failed Start")
            })
            .unwrap();
        transport.request(true, 480, |_| Ok(())).unwrap();
        assert!(transport.running);
    }
    #[test]
    fn live_mixer_honors_audio_solo_and_mute_independently_of_video_solo() {
        let mut project = Project {
            tracks: (1..=3)
                .map(|id| crate::Track {
                    id,
                    name: format!("Track {id}"),
                    kind: if id == 3 {
                        TrackKind::Video
                    } else {
                        TrackKind::Audio
                    },
                    muted: false,
                    solo: id == 3,
                    gain: 0.5,
                    suppress_noise: false,
                })
                .collect(),
            ..Project::default()
        };
        let levels = audio_levels(&project);
        assert!(!levels[&1].muted && !levels[&2].muted);
        assert!(!levels.contains_key(&3));
        project.tracks[0].solo = true;
        let levels = audio_levels(&project);
        assert!(!levels[&1].muted && levels[&2].muted);
        assert_eq!(levels[&1].gain, 0.5);
        project.tracks[0].muted = true;
        assert!(audio_levels(&project).values().all(|level| level.muted));
        project.tracks[0].solo = false;
        assert!(!audio_levels(&project)[&2].muted);
    }
    #[test]
    fn paused_clock_does_not_advance_and_resume_keeps_time() {
        let mut clock = WallClock::default();
        clock.play();
        thread::sleep(Duration::from_millis(12));
        clock.pause();
        let at = clock.elapsed();
        thread::sleep(Duration::from_millis(12));
        assert_eq!(clock.elapsed(), at);
        clock.play();
        thread::sleep(Duration::from_millis(4));
        assert!(clock.elapsed() > at);
    }
    #[test]
    fn arbitrary_pcm_tail_is_not_dropped() {
        let data = vec![42u8; 480 * 8 + 8];
        let mut input = &data[..];
        let mut block = vec![0; 480 * 8];
        assert_eq!(read_block(&mut input, &mut block).unwrap(), 480 * 8);
        assert_eq!(read_block(&mut input, &mut block).unwrap(), 8);
        assert_eq!(read_block(&mut input, &mut block).unwrap(), 0);
    }
    #[test]
    fn muted_audio_consumes_time_and_unmute_does_not_replay_old_samples() {
        let pcm = Pcm::new();
        pcm.samples.lock().unwrap().extend([[0.8, 0.4], [0.2, 0.6]]);
        let mut mix = [[0., 0.]];
        pcm.mix_into(&mut mix, 0.);
        assert_eq!(mix, [[0., 0.]]);
        pcm.mix_into(&mut mix, 0.5);
        assert_eq!(mix, [[0.1, 0.3]]);
        assert!(pcm.samples.lock().unwrap().is_empty());
    }
    #[test]
    fn presentation_drops_late_frames_but_keeps_future_frames() {
        let mut queue: VecDeque<_> = (10..=12)
            .map(|frame| PreviewFrame {
                width: 2,
                height: 2,
                frame,
                bgra: vec![0; 16],
            })
            .collect();
        let (frame, dropped) = due_frame(&mut queue, 11);
        assert_eq!(frame.unwrap().frame, 11);
        assert_eq!(dropped, 1);
        assert!(due_frame(&mut queue, 11).0.is_none());
        assert_eq!(queue.len(), 1);
        assert_eq!(due_frame(&mut queue, 12).0.unwrap().frame, 12);
    }
}
