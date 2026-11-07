//! Independent screencast session: WGC + bounded recording pipeline + external FFmpeg.
//! No recorder commands, replay packets, or game-capture settings enter this module.
use anyhow::{Context, Result, ensure};
use recording_tools::{
    adapter::{AudioSelection, default_audio},
    audio::AudioSource,
    encode::{EncodeSummary, Encoder, EncoderInput, InputKind, OutputSpec},
    error::RecordError,
    session::{EndReason, GapPolicy, RecordConfig, RecordingSession},
    source::{CaptureTarget, FrameSource, SourceConfig, windows::WgcSource},
    stats::StatsSnapshot,
    time::Fps,
};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct Options {
    pub target: CaptureTarget,
    pub cursor: bool,
    pub system_audio: bool,
    pub microphone: bool,
    pub fps: u32,
    pub folder: PathBuf,
}
pub enum Event {
    Started,
    Progress(StatsSnapshot, Vec<String>),
    Finishing,
    Saved(PathBuf, Vec<String>),
    Failed(String),
    Cancelled,
}
pub struct Worker {
    pub events: mpsc::Receiver<Event>,
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
pub(crate) type SourceSet = (Box<dyn FrameSource>, Vec<Box<dyn AudioSource>>);
pub(crate) type SourceFactory = Box<dyn FnOnce(&Options, Fps) -> Result<SourceSet> + Send>;
impl Worker {
    pub fn start(options: Options) -> Result<Self> {
        Self::start_with_sources(
            options,
            Box::new(|options, fps| {
                let source = WgcSource::new(SourceConfig {
                    target: options.target.clone(),
                    fps,
                    cursor: options.cursor,
                });
                let audio = default_audio(AudioSelection {
                    system: options.system_audio,
                    microphone: options.microphone,
                });
                Ok((Box::new(source), audio))
            }),
        )
    }
    pub(crate) fn start_with_sources(options: Options, factory: SourceFactory) -> Result<Self> {
        let (tx, events) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let abort = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let (s, a, p) = (stop.clone(), abort.clone(), paused.clone());
        let thread = thread::Builder::new()
            .name("rcr-quick-recorder".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(options, &s, &a, &p, &tx, factory)
                }))
                .unwrap_or_else(|_| {
                    Err(anyhow::anyhow!(
                        "Recorder worker stopped unexpectedly during startup"
                    ))
                });
                if let Err(error) = result {
                    let _ = tx.send(Event::Failed(format!("{error:#}")));
                }
            })?;
        Ok(Self {
            events,
            stop,
            abort,
            paused,
            thread: Some(thread),
        })
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn pause(&self, pause: bool) {
        self.paused.store(pause, Ordering::Release);
    }
    pub fn cancel(&self) {
        self.abort.store(true, Ordering::Release);
    }
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
    pub fn poll(&self) -> (Vec<Event>, bool) {
        collect_events(&self.events, || self.finished())
    }
}
fn collect_events<E>(
    receiver: &mpsc::Receiver<E>,
    finished: impl FnOnce() -> bool,
) -> (Vec<E>, bool) {
    let mut events = receiver.try_iter().collect::<Vec<_>>();
    let retired = finished();
    // All terminal messages precede the thread's return. They may arrive after
    // the first drain: consume the entire tail before releasing its receiver.
    if retired {
        events.extend(receiver.try_iter());
    }
    (events, retired)
}
impl Drop for Worker {
    fn drop(&mut self) {
        // Normal application exit finalizes the clip. Only an explicit Discard
        // sets abort; closing the controller must not throw a recording away.
        self.stop.store(true, Ordering::Release);
        // Release event backpressure before joining. All OS resources belong to this worker.
        let (_, empty) = mpsc::sync_channel(1);
        self.events = empty;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn run(
    options: Options,
    stop: &AtomicBool,
    abort: &AtomicBool,
    paused: &AtomicBool,
    events: &mpsc::SyncSender<Event>,
    factory: SourceFactory,
) -> Result<()> {
    ensure!([30, 60].contains(&options.fps), "Choose 30 or 60 FPS");
    fs::create_dir_all(&options.folder)?;
    let staging = tempfile::Builder::new()
        .prefix(".rcr-screencast-")
        .tempdir_in(&options.folder)?;
    let mut cfg = RecordConfig::new(staging.path().join("clip.mp4"));
    cfg.fps = Fps::from_f64(f64::from(options.fps)).context("Invalid frame rate")?;
    cfg.pipeline.queue_budget_bytes = 48 * 1024 * 1024;
    cfg.pipeline.gap_policy = GapPolicy::Duplicate;
    cfg.limits.max_duration = Some(Duration::from_secs(600));
    if abort.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        let _ = events.try_send(Event::Cancelled);
        return Ok(());
    }
    let (source, audio) = factory(&options, cfg.fps)?;
    if abort.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        let _ = events.try_send(Event::Cancelled);
        return Ok(());
    }
    let session = RecordingSession::start_with(
        cfg,
        source,
        audio,
        Box::new(|spec| ExternalEncoder::open(spec).map(|e| Box::new(e) as Box<dyn Encoder>)),
    )?;
    let mut announced = false;
    loop {
        if abort.load(Ordering::Acquire) {
            session.abort();
            let _ = events.try_send(Event::Cancelled);
            return Ok(());
        }
        if stop.load(Ordering::Acquire) || session.finished().is_some() {
            break;
        }
        if paused.load(Ordering::Acquire) {
            session.pause();
        } else {
            session.resume();
        }
        let stats = session.stats();
        if !announced && stats.encoded > 0 {
            announced = events.try_send(Event::Started).is_ok();
        }
        let _ = events.try_send(Event::Progress(stats, session.warnings()));
        thread::sleep(Duration::from_millis(100));
    }
    let _ = events.try_send(Event::Finishing);
    if stop.load(Ordering::Acquire) && session.stats().encoded == 0 {
        session.abort();
        let _ = events.try_send(Event::Cancelled);
        return Ok(());
    }
    let recording = session.stop()?;
    ensure!(recording.stats.encoded > 0, "No video frames were recorded");
    if abort.load(Ordering::Acquire) {
        let _ = events.try_send(Event::Cancelled);
        return Ok(());
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let destination = options.folder.join(format!("screencast-{stamp}.mp4"));
    // Same-volume publication without replacing a user's existing file, also on removable drives.
    tempfile::TempPath::try_from_path(&recording.path)?
        .persist_noclobber(&destination)
        .context("Could not publish the screencast")?;
    let mut warnings = recording.warnings;
    if let EndReason::Failed(reason) = recording.end_reason {
        warnings.push(format!(
            "Capture stopped early: {reason}. The playable prefix was saved."
        ));
    }
    if recording.stats.dropped_backpressure > 0 {
        warnings.push(format!(
            "{} frames missed their capture deadline",
            recording.stats.dropped_backpressure
        ));
    }
    events
        .send(Event::Saved(destination, warnings))
        .context("Recorder window disconnected")?;
    Ok(())
}

fn encoder_error(error: impl std::fmt::Display) -> RecordError {
    RecordError::Encoder {
        encoder: "FFmpeg H.264".into(),
        message: format!("{error:#}"),
    }
}
fn ffmpeg() -> Command {
    let mut command =
        Command::new(std::env::var_os("RCR_FFMPEG").unwrap_or_else(|| "ffmpeg".into()));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command.args(["-hide_banner", "-loglevel", "error", "-n"]);
    command
}

/// A bounded diagnostic tail and a watchdog for blocked writes/waits. A stalled child
/// cannot retain the recording worker indefinitely; Drop always kills and reaps it.
struct Process {
    child: Arc<Mutex<Child>>,
    input: Option<ChildStdin>,
    tail: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    deadline: Arc<Mutex<Option<Instant>>>,
    done: Arc<AtomicBool>,
    watchdog: Option<JoinHandle<()>>,
}
impl Process {
    fn spawn(command: &mut Command) -> Result<Self> {
        let mut child = command
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context(
                "FFmpeg could not start. Install FFmpeg with libx264 and AAC, or set RCR_FFMPEG",
            )?;
        let input = child.stdin.take();
        let mut stderr = child
            .stderr
            .take()
            .context("FFmpeg diagnostic pipe unavailable")?;
        let child = Arc::new(Mutex::new(child));
        let tail = Arc::new(Mutex::new(Vec::new()));
        let collected = tail.clone();
        let reader = thread::spawn(move || {
            let mut buffer = [0; 2048];
            while let Ok(n) = stderr.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                let mut tail = collected.lock().unwrap_or_else(|e| e.into_inner());
                tail.extend_from_slice(&buffer[..n]);
                let excess = tail.len().saturating_sub(8192);
                tail.drain(..excess);
            }
        });
        let deadline = Arc::new(Mutex::new(None::<Instant>));
        let done = Arc::new(AtomicBool::new(false));
        let (d, c, stopped) = (deadline.clone(), child.clone(), done.clone());
        let watchdog = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                if d.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some_and(|at| Instant::now() >= at)
                {
                    let _ = c.lock().unwrap_or_else(|e| e.into_inner()).kill();
                }
                thread::park_timeout(Duration::from_millis(50));
            }
        });
        Ok(Self {
            child,
            input,
            tail,
            reader: Some(reader),
            deadline,
            done,
            watchdog: Some(watchdog),
        })
    }
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.write_with_timeout(bytes, Duration::from_secs(10))
    }
    fn write_with_timeout(&mut self, bytes: &[u8], timeout: Duration) -> Result<()> {
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + timeout);
        let result = self
            .input
            .as_mut()
            .context("Encoder input closed")?
            .write_all(bytes);
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = None;
        result.context("FFmpeg stopped accepting media samples")
    }
    fn wait(&mut self, timeout: Duration) -> Result<()> {
        self.input.take();
        let deadline = Instant::now() + timeout;
        loop {
            let status = self
                .child
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .try_wait()?;
            if let Some(status) = status {
                if let Some(reader) = self.reader.take() {
                    let _ = reader.join();
                }
                let tail = self.tail.lock().unwrap_or_else(|e| e.into_inner());
                ensure!(
                    status.success(),
                    "FFmpeg failed: {}",
                    String::from_utf8_lossy(&tail)
                );
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "FFmpeg did not finish within {} seconds",
                timeout.as_secs()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.input.take();
        {
            let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
            let _ = child.kill();
            let _ = child.wait();
        }
        self.done.store(true, Ordering::Release);
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.thread().unpark();
            let _ = watchdog.join();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

struct ExternalEncoder {
    spec: OutputSpec,
    directory: tempfile::TempDir,
    video: Option<Process>,
    audio: Option<Process>,
    audio_bytes: Vec<u8>,
    audio_failed: bool,
    frames: u64,
    samples: u64,
    last_pixels: Vec<u8>,
}
impl ExternalEncoder {
    fn open(spec: &OutputSpec) -> recording_tools::error::Result<Self> {
        let open = || -> Result<Self> {
            ensure!(!spec.path.exists(), "Output already exists");
            let directory = tempfile::Builder::new()
                .prefix("encoder-")
                .tempdir_in(spec.path.parent().context("Output folder missing")?)?;
            Ok(Self {
                spec: spec.clone(),
                directory,
                video: None,
                audio: None,
                audio_bytes: Vec::new(),
                audio_failed: false,
                frames: 0,
                samples: 0,
                last_pixels: vec![],
            })
        };
        open().map_err(encoder_error)
    }
    fn write_frame(&mut self, slot: u64, input: &EncoderInput) -> Result<()> {
        let EncoderInput::Rgba(frame) = input else {
            anyhow::bail!("Expected RGB pixels");
        };
        let frame = frame.clone().into_rgba8()?;
        let image =
            image::RgbaImage::from_raw(frame.width(), frame.height(), frame.data().to_vec())
                .context("Invalid video pixels")?;
        // Keep a fixed canvas when a captured window is resized, with no aspect distortion.
        let size = self.spec.video.size;
        let pixels = if image.dimensions() == (size.width, size.height) {
            image.into_raw()
        } else {
            let scale = (f64::from(size.width) / f64::from(image.width()))
                .min(f64::from(size.height) / f64::from(image.height()));
            let (w, h) = (
                (f64::from(image.width()) * scale).round().max(1.) as u32,
                (f64::from(image.height()) * scale).round().max(1.) as u32,
            );
            let resized =
                image::imageops::resize(&image, w, h, image::imageops::FilterType::Triangle);
            let mut canvas =
                image::RgbaImage::from_pixel(size.width, size.height, image::Rgba([0, 0, 0, 255]));
            image::imageops::overlay(
                &mut canvas,
                &resized,
                i64::from((size.width - w) / 2),
                i64::from((size.height - h) / 2),
            );
            canvas.into_raw()
        };
        if self.video.is_none() {
            let mut command = ffmpeg();
            command.args(["-f", "rawvideo", "-pixel_format", "rgba", "-video_size", &format!("{}x{}", size.width, size.height),
                "-framerate", &self.spec.video.fps.as_f64().to_string(), "-i", "pipe:0", "-an", "-vf",
                "scale=w='min(1920,iw)':h='min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2:out_color_matrix=bt709:out_range=tv",
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-threads", "2", "-pix_fmt", "yuv420p",
                "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]);
            // Audio recordings are muxed below. Relocate MP4 metadata only for
            // the final file, rather than rewriting the video twice on stop.
            if self.spec.audio.is_none() {
                command.args(["-movflags", "+faststart"]);
            }
            command
                .arg(self.directory.path().join("video.mp4")).stdin(Stdio::piped());
            self.video = Some(Process::spawn(&mut command)?);
        }
        // Rawvideo uses implicit CFR timestamps: fill any dropped slots explicitly.
        let previous = if self.last_pixels.is_empty() {
            &pixels
        } else {
            &self.last_pixels
        };
        let video = self.video.as_mut().unwrap();
        while self.frames < slot {
            video.write(previous)?;
            self.frames += 1;
        }
        video.write(&pixels)?;
        self.frames += 1;
        self.last_pixels = pixels;
        Ok(())
    }
}
impl Encoder for ExternalEncoder {
    fn description(&self) -> String {
        "H.264 (libx264) + AAC · MP4".into()
    }
    fn input_kind(&self) -> InputKind {
        InputKind::Rgba8
    }
    fn has_audio(&self) -> bool {
        self.spec.audio.is_some()
    }
    fn write_video(
        &mut self,
        slot: u64,
        input: &Arc<EncoderInput>,
    ) -> recording_tools::error::Result<()> {
        self.write_frame(slot, input).map_err(encoder_error)
    }
    fn write_audio(&mut self, samples: &[f32]) -> recording_tools::error::Result<()> {
        let mut write = || -> Result<()> {
            ensure!(!self.audio_failed, "Audio encoder previously failed");
            let format = self.spec.audio.context("Audio is disabled")?;
            let channels = usize::from(format.channels);
            ensure!(channels > 0 && samples.len().is_multiple_of(channels), "Invalid audio sample alignment");
            if samples.is_empty() { return Ok(()); }
            if self.audio.is_none() {
                let mut command = ffmpeg();
                command.args(["-f", "f32le", "-ar", &format.sample_rate.to_string(), "-ac",
                    &format.channels.to_string(), "-i", "pipe:0", "-vn", "-c:a", "aac",
                    "-b:a", "128k", "-threads", "1"])
                    .arg(self.directory.path().join("audio.m4a")).stdin(Stdio::piped());
                self.audio = Some(Process::spawn(&mut command)?);
            }
            // AAC runs as samples arrive. Keep serialization scratch bounded
            // even if an audio source hands over a large final silence block.
            for chunk in samples.chunks(2048 * channels) {
                self.audio_bytes.clear();
                self.audio_bytes.extend(chunk.iter().flat_map(|sample| sample.to_le_bytes()));
                self.audio.as_mut().unwrap().write(&self.audio_bytes)?;
                self.samples += (chunk.len() / channels) as u64;
            }
            Ok(())
        };
        let result = write().map_err(encoder_error);
        if result.is_err() { self.audio_failed = true; }
        result
    }
    fn bytes_written(&self) -> u64 {
        fs::metadata(self.directory.path().join("video.mp4")).map_or(0, |m| m.len())
            + fs::metadata(self.directory.path().join("audio.m4a")).map_or(0, |m| m.len())
    }
    fn finish(
        mut self: Box<Self>,
        end: Duration,
    ) -> recording_tools::error::Result<EncodeSummary> {
        let mut finish = || -> Result<EncodeSummary> {
            let mut video = self.video.take().context("No video frames were recorded")?;
            if let Some(format) = self.spec.audio.filter(|_| !self.audio_failed) {
                let target = (end.as_secs_f64() * f64::from(format.sample_rate)).round() as u64;
                // AudioPipeline normally pads the mix already. Also support
                // direct callers and devices that never delivered a sample.
                let silence = vec![0.; 2048 * usize::from(format.channels)];
                while self.samples < target {
                    let frames = (target - self.samples).min(2048) as usize;
                    if self.write_audio(&silence[..frames * usize::from(format.channels)]).is_err() {
                        break;
                    }
                }
            }
            // Flush both encoders concurrently; stop only drains their tails,
            // then stream-copies compressed media into the shared MP4.
            video.input.take();
            if let Some(audio) = self.audio.as_mut() { audio.input.take(); }
            video.wait(Duration::from_secs(30))?;
            if let Some(mut audio) = self.audio.take()
                && let Err(error) = audio.wait(Duration::from_secs(30)) {
                self.audio_failed = true;
                eprintln!("Audio finalization failed; preserving video: {error:#}");
            }
            let has_audio = self.spec.audio.is_some() && !self.audio_failed;
            if self.spec.audio.is_none() {
                // This is already the final H.264 MP4. Publishing it directly
                // avoids a second FFmpeg launch and a full-file remux on stop.
                tempfile::TempPath::try_from_path(self.directory.path().join("video.mp4"))?
                    .persist_noclobber(&self.spec.path)
                    .context("Could not publish the encoded video")?;
                fs::OpenOptions::new()
                    .write(true)
                    .open(&self.spec.path)?
                    .sync_all()?;
                return Ok(EncodeSummary {
                    path: self.spec.path.clone(),
                    encoder: self.description(),
                    video_frames: self.frames,
                    audio_samples: self.samples,
                    bytes: fs::metadata(&self.spec.path)?.len(),
                    duration: end,
                    has_audio: false,
                });
            }
            let mut command = ffmpeg();
            command
                .arg("-i")
                .arg(self.directory.path().join("video.mp4"));
            if has_audio {
                command
                    .arg("-i")
                    .arg(self.directory.path().join("audio.m4a"))
                    .args([
                        "-map", "0:v:0", "-map", "1:a:0", "-c:a", "copy",
                    ]);
            } else {
                command.args(["-map", "0:v:0", "-an"]);
            }
            command
                .args([
                    "-c:v",
                    "copy",
                    "-t",
                    &end.as_secs_f64().to_string(),
                    "-movflags",
                    "+faststart",
                ])
                .arg(&self.spec.path)
                .stdin(Stdio::null());
            Process::spawn(&mut command)?.wait(Duration::from_secs(60))?;
            fs::OpenOptions::new()
                .write(true)
                .open(&self.spec.path)?
                .sync_all()?;
            Ok(EncodeSummary {
                path: self.spec.path.clone(),
                encoder: self.description(),
                video_frames: self.frames,
                audio_samples: if has_audio { self.samples } else { 0 },
                bytes: fs::metadata(&self.spec.path)?.len(),
                duration: end,
                has_audio,
            })
        };
        finish().map_err(encoder_error)
    }
    fn abort(self: Box<Self>) { /* owned temporary directory and child clean up on drop */
    }
}

impl Drop for ExternalEncoder {
    fn drop(&mut self) {
        // Children hold the staging files open on Windows. Reap them before
        // TempDir's destructor tries to remove those owned files.
        self.video.take();
        self.audio.take();
    }
}

#[cfg(test)]
#[path = "quick_recording_stress_tests.rs"]
mod stress_tests;
#[cfg(test)]
mod tests {
    use super::*;
    fn tone_magnitude(samples: &[f64], hz: f64) -> f64 {
        if samples.is_empty() { return 0.; }
        let cosine = (std::f64::consts::TAU * hz / 48_000.).cos();
        let (mut previous, mut before_previous) = (0., 0.);
        for sample in samples {
            let current = sample + 2. * cosine * previous - before_previous;
            before_previous = previous;
            previous = current;
        }
        2. * (previous * previous + before_previous * before_previous
            - 2. * cosine * previous * before_previous).max(0.).sqrt() / samples.len() as f64
    }
    fn preserve_audio_failure(
        path: &std::path::Path,
        recording: &recording_tools::session::Recording,
        source_count: usize,
        samples: &[f64],
        probe: &serde_json::Value,
    ) -> Result<PathBuf> {
        let directory = std::env::var_os("RCR_RECORDING_FAILURE_DIR").map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".downloads/autonomous-eval/recording-failures"));
        fs::create_dir_all(&directory)?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let destination = directory.join(format!("matrix-{}x{}-{}fps-{source_count}sources-{stamp}.mp4",
            recording.size.width, recording.size.height, recording.fps.as_f64()));
        fs::copy(path, &destination)?;
        let windows = samples.chunks(4800).enumerate().map(|(index, samples)| serde_json::json!({
            "start_ms": index * 100, "sample_count": samples.len(),
            "amplitude_440": tone_magnitude(samples, 440.), "amplitude_880": tone_magnitude(samples, 880.),
            "rms": (samples.iter().map(|value| value * value).sum::<f64>() / samples.len() as f64).sqrt(),
        })).collect::<Vec<_>>();
        let peaks = [440., 880.].map(|center| {
            let (frequency, amplitude) = (-40..=40).map(|step| {
                let frequency = center + f64::from(step) * 0.25;
                (frequency, tone_magnitude(samples, frequency))
            }).max_by(|left, right| left.1.total_cmp(&right.1)).unwrap();
            serde_json::json!({"around_hz": center, "best_frequency_hz": frequency, "amplitude": amplitude})
        });
        let s = recording.stats;
        fs::write(destination.with_extension("json"), serde_json::to_vec_pretty(&serde_json::json!({
            "recording": {"path": destination, "size": [recording.size.width, recording.size.height],
                "fps": recording.fps.as_f64(), "duration_seconds": recording.duration.as_secs_f64(),
                "file_bytes": recording.file_bytes, "encoder": recording.encoder, "has_audio": recording.has_audio,
                "end_reason": format!("{:?}", recording.end_reason), "warnings": recording.warnings,
                "gpu_frames": recording.gpu_frames, "cpu_hdr_frames": recording.cpu_hdr_frames,
                "peak_queue_frames": recording.peak_queue_frames, "queue_capacity": recording.queue_capacity,
                "stats": {"captured": s.captured, "surplus_dropped": s.surplus_dropped,
                    "paused_discarded": s.paused_discarded, "duplicated": s.duplicated, "slots": s.slots,
                    "dropped_backpressure": s.dropped_backpressure, "dropped_duplicates": s.dropped_duplicates,
                    "dropped_convert": s.dropped_convert, "encoded": s.encoded, "audio_samples": s.audio_samples,
                    "audio_silence_samples": s.audio_silence_samples, "audio_late_samples": s.audio_late_samples,
                    "audio_resyncs": s.audio_resyncs, "file_bytes": s.file_bytes, "recorded_seconds": s.recorded.as_secs_f64()}},
            "source_count": source_count, "decoded_mono_samples": samples.len(), "probe": probe,
            "windows_100ms": windows, "frequency_search": {"method": "Goertzel DFT, +/-10Hz at0.25Hz spacing", "peaks": peaks},
        }))?)?;
        let pcm = samples.iter().flat_map(|value| (*value as f32).to_le_bytes()).collect::<Vec<_>>();
        fs::write(destination.with_extension("mono.f32"), pcm)?;
        Ok(destination)
    }
    pub(super) fn probe(path: &std::path::Path) -> serde_json::Value {
        let output =
            Command::new(std::env::var_os("RCR_FFPROBE").unwrap_or_else(|| "ffprobe".into()))
                .args([
                    "-v",
                    "error",
                    "-show_streams",
                    "-show_format",
                    "-of",
                    "json",
                ])
                .arg(path)
                .output()
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    #[test]
    fn screencast_matrix_preserves_dimensions_fps_duration_and_mixed_audio() {
        for (width, height, fps, sources) in [
            (321_u32, 181_u32, 30_u32, 0_usize),
            (640, 360, 60, 1),
            (1279, 719, 30, 2),
        ] {
            let folder = tempfile::tempdir().unwrap();
            let path = folder.path().join("matrix.mp4");
            let mut cfg = RecordConfig::new(&path);
            cfg.fps = Fps::from_f64(f64::from(fps)).unwrap();
            cfg.pipeline.gap_policy = GapPolicy::Duplicate;
            // The source runs on virtual time, but a source that ends by itself is padded to
            // the wall clock. Under load (Wine, a busy runner) delivering one virtual second
            // takes longer than one real second; the limit keeps the recording at one second.
            cfg.limits.max_duration = Some(Duration::from_secs(1));
            let source = SyntheticSource::new(
                SyntheticConfig::new(width, height, cfg.fps)
                    .time(TimeMode::Virtual)
                    .max_frames(u64::from(fps)),
            );
            let audio = (0..sources)
                .map(|index| {
                    let mut cfg = SyntheticAudioConfig::new(
                        Signal::Tone {
                            hz: if index == 0 { 440. } else { 880. },
                            amplitude: 0.2,
                            start: Duration::ZERO,
                        },
                        TimeMode::Virtual,
                    );
                    cfg.total = Some(Duration::from_secs(1));
                    Box::new(SyntheticAudio::new(cfg)) as Box<dyn AudioSource>
                })
                .collect();
            let session = RecordingSession::start_with(
                cfg,
                Box::new(source),
                audio,
                Box::new(|spec| {
                    ExternalEncoder::open(spec).map(|e| Box::new(e) as Box<dyn Encoder>)
                }),
            )
            .unwrap();
            assert!(session.wait_finished(Duration::from_secs(20)).is_some());
            let result = session.stop().unwrap();
            assert_eq!(result.stats.encoded, u64::from(fps));
            assert!(result.warnings.is_empty(), "{:?}", result.warnings);
            let info = probe(&path);
            let streams = info["streams"].as_array().unwrap();
            let video = streams
                .iter()
                .find(|stream| stream["codec_type"] == "video")
                .unwrap();
            assert_eq!(video["codec_name"], "h264");
            assert_eq!(video["r_frame_rate"], format!("{fps}/1"));
            assert_eq!(video["width"].as_u64().unwrap() % 2, 0);
            assert_eq!(video["height"].as_u64().unwrap() % 2, 0);
            let duration: f64 = info["format"]["duration"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            assert!((duration - 1.).abs() < 0.05, "{duration}");
            // Both direct video publication and the audio mux must preserve
            // fast-start layout so shared clips can begin before full download.
            let bytes = fs::read(&path).unwrap();
            let mut offset = 0;
            let mut boxes = Vec::new();
            while offset + 8 <= bytes.len() {
                let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                assert!(size >= 8 && offset + size <= bytes.len(), "Invalid MP4 box");
                boxes.push(bytes[offset + 4..offset + 8].to_vec());
                offset += size;
            }
            let moov = boxes.iter().position(|kind| kind == b"moov").unwrap();
            let mdat = boxes.iter().position(|kind| kind == b"mdat").unwrap();
            assert!(moov < mdat, "Shared MP4 must have metadata before video data");
            assert_eq!(
                streams
                    .iter()
                    .filter(|stream| stream["codec_type"] == "audio")
                    .count(),
                usize::from(sources > 0)
            );
            if sources > 0 {
                assert!(streams.iter().any(|stream| stream["codec_name"] == "aac"));
                let decoded = ffmpeg()
                    .arg("-i")
                    .arg(&path)
                    .args([
                        "-map", "0:a:0", "-f", "f32le", "-ar", "48000", "-ac", "1", "pipe:1",
                    ])
                    .output()
                    .unwrap();
                assert!(decoded.status.success());
                let samples = decoded
                    .stdout
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|bytes| f32::from_le_bytes(*bytes) as f64)
                    .collect::<Vec<_>>();
                assert!(
                    (47_000..=50_000).contains(&samples.len()),
                    "{} samples",
                    samples.len()
                );
                for hz in [440., 880.].into_iter().take(sources) {
                    let (mut re, mut im) = (0., 0.);
                    for (index, sample) in samples.iter().enumerate() {
                        let phase = 2. * std::f64::consts::PI * hz * index as f64 / 48_000.;
                        re += sample * phase.cos();
                        im += sample * phase.sin();
                    }
                    let amplitude = 2. * re.hypot(im) / samples.len() as f64;
                    if amplitude <= 0.04 {
                        match preserve_audio_failure(&path, &result, sources, &samples, &info) {
                            Ok(path) => eprintln!("Failed recording, complete statistics and spectral windows preserved: {}", path.display()),
                            Err(error) => eprintln!("Could not preserve audio regression artifacts: {error:#}"),
                        }
                    }
                    assert!(amplitude > 0.04,
                        "Mixed source {hz} Hz missing: {width}x{height}@{fps}, sources={sources}, amplitude={amplitude}, decoded_samples={}, audio_samples={}, silence_samples={}, warnings={:?}",
                        samples.len(), result.stats.audio_samples, result.stats.audio_silence_samples, result.warnings);
                }
            }
        }
    }

    #[test]
    fn abort_removes_only_its_owned_clip_and_encoder_staging() {
        let folder = tempfile::tempdir().unwrap();
        let sentinel = folder.path().join("original.txt");
        fs::write(&sentinel, b"original preserved").unwrap();
        let output = folder.path().join("cancelled.mp4");
        let cfg = RecordConfig::new(&output);
        let source = SyntheticSource::new(
            SyntheticConfig::new(320, 180, cfg.fps)
                .time(TimeMode::Virtual)
                .max_frames(60),
        );
        let session = RecordingSession::start_with(
            cfg,
            Box::new(source),
            vec![],
            Box::new(|spec| ExternalEncoder::open(spec).map(|e| Box::new(e) as Box<dyn Encoder>)),
        )
        .unwrap();
        session.abort();
        assert!(!output.exists());
        assert_eq!(fs::read(&sentinel).unwrap(), b"original preserved");
        assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 1);
    }

    #[test]
    fn child_timeout_kills_and_reaps_the_owned_encoder() {
        let mut command = ffmpeg();
        command
            .args([
                "-re",
                "-f",
                "lavfi",
                "-i",
                "color=size=32x32:rate=30",
                "-f",
                "null",
                "-",
            ])
            .stdin(Stdio::null());
        let mut process = Process::spawn(&mut command).unwrap();
        let child = process.child.clone();
        let started = Instant::now();
        assert!(process.wait(Duration::from_millis(150)).is_err());
        drop(process);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(child.lock().unwrap().try_wait().unwrap().is_some());
    }
    use recording_tools::{
        audio::{
            AudioSource,
            synth::{Signal, SyntheticAudio, SyntheticAudioConfig},
        },
        source::synthetic::{SyntheticConfig, SyntheticSource, TimeMode},
    };

    #[test]
    fn external_encoder_records_synthetic_video_and_audio_and_decodes() {
        // Real independent encode/decode; never captures the desktop or an audio device.
        let folder = tempfile::tempdir().unwrap();
        let output = folder.path().join("owned.mp4");
        let mut cfg = RecordConfig::new(&output);
        cfg.pipeline.gap_policy = GapPolicy::Duplicate;
        let source = SyntheticSource::new(
            SyntheticConfig::new(320, 180, cfg.fps)
                .time(TimeMode::Virtual)
                .max_frames(30),
        );
        let mut audio = SyntheticAudioConfig::new(
            Signal::Tone {
                hz: 440.,
                amplitude: 0.2,
                start: Duration::ZERO,
            },
            TimeMode::Virtual,
        );
        audio.time = TimeMode::Virtual;
        audio.total = Some(Duration::from_secs(1));
        let audio: Vec<Box<dyn AudioSource>> = vec![Box::new(SyntheticAudio::new(audio))];
        let session = RecordingSession::start_with(
            cfg,
            Box::new(source),
            audio,
            Box::new(|spec| ExternalEncoder::open(spec).map(|e| Box::new(e) as Box<dyn Encoder>)),
        )
        .unwrap();
        assert!(session.wait_finished(Duration::from_secs(20)).is_some());
        let result = session.stop().unwrap();
        assert!(result.has_audio);
        assert_eq!(result.stats.encoded, 30);
        assert!(result.file_bytes > 1000);
        let status = ffmpeg()
            .args(["-i"])
            .arg(&output)
            .args(["-map", "0", "-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
}
