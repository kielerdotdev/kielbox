use crate::{
    gpu::{Capture, CaptureRecoveryEvent, Converter, PreviewSurface},
    nvenc::Nvenc,
};
use anyhow::{Context, Result, bail, ensure};
use rcr_media_core::{
    AudioTrack, EncodedPacket, PipelineStats, ReplayBuffer, VideoCapture, VideoConfig, VideoEncoder,
};
use std::{
    fs::{self, File},
    io::{BufWriter, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub video: VideoConfig,
    pub monitor: u32,
    /// Exact display selection; None retains the legacy NVIDIA-only CLI ordinal.
    pub display: Option<crate::display::Selection>,
    pub seconds: Option<f64>,
    pub output: PathBuf,
    pub preview: bool,
    pub replay_seconds: u64,
    pub replay_max_bytes: usize,
    pub audio: Vec<AudioTrack>,
    pub record_to_disk: bool,
    pub save_on_stop: bool,
    /// Applies on replay save only. Live/continuous microphone PCM stays raw.
    pub suppress_microphone: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            video: VideoConfig::default(),
            monitor: 0,
            display: None,
            seconds: Some(10.0),
            output: PathBuf::from("recordings/proof.mkv"),
            preview: false,
            replay_seconds: 120,
            replay_max_bytes: 512 * 1024 * 1024,
            audio: Vec::new(),
            record_to_disk: true,
            save_on_stop: false,
            suppress_microphone: false,
        }
    }
}

pub enum PipelineEvent {
    Started(PreviewSurface),
    Stats(PipelineStats),
    Finished(PipelineStats),
    Error(String),
    ReplaySaved(PathBuf),
    Notice(String),
}

pub enum RecorderCommand {
    SaveReplay {
        path: PathBuf,
        seconds: u64,
        suppress_microphone: bool,
    },
}

pub fn run(
    options: RunOptions,
    stop: Arc<AtomicBool>,
    events: Option<mpsc::SyncSender<PipelineEvent>>,
) -> Result<PipelineStats> {
    run_controlled(options, stop, events, None)
}
pub fn run_controlled(
    options: RunOptions,
    stop: Arc<AtomicBool>,
    events: Option<mpsc::SyncSender<PipelineEvent>>,
    commands: Option<mpsc::Receiver<RecorderCommand>>,
) -> Result<PipelineStats> {
    ensure!(
        !options.suppress_microphone
            || options
                .audio
                .iter()
                .any(|t| t.source == rcr_media_core::AudioSource::Microphone),
        "--suppress-mic requires --audio mic (or system,mic); suppression applies only to saved replay clips"
    );
    if options.output.extension().is_some_and(|e| e == "ivf") {
        ensure!(
            options.audio.is_empty() && options.record_to_disk && !options.save_on_stop,
            "IVF is a video-only proof format; use .mkv for audio or replay mode"
        );
        run_ivf_proof(options, stop, events)
    } else {
        crate::recording::run(options, stop, events, commands)
    }
}

pub struct IvfWriter {
    file: BufWriter<File>,
    count: u32,
    base_pts: Option<u64>,
    partial: PathBuf,
    final_path: PathBuf,
}

impl IvfWriter {
    pub fn create(path: &Path, config: &VideoConfig) -> Result<Self> {
        ensure!(!path.exists(), "output already exists: {}", path.display());
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let partial = path.with_extension("ivf.partial");
        let file =
            File::create_new(&partial).with_context(|| format!("create {}", partial.display()))?;
        let mut file = BufWriter::with_capacity(1024 * 1024, file);
        file.write_all(b"DKIF")?;
        file.write_all(&0u16.to_le_bytes())?;
        file.write_all(&32u16.to_le_bytes())?;
        file.write_all(b"AV01")?;
        file.write_all(&(config.width as u16).to_le_bytes())?;
        file.write_all(&(config.height as u16).to_le_bytes())?;
        file.write_all(&config.fps.to_le_bytes())?;
        file.write_all(&1u32.to_le_bytes())?;
        file.write_all(&0u32.to_le_bytes())?;
        file.write_all(&0u32.to_le_bytes())?;
        Ok(Self {
            file,
            count: 0,
            base_pts: None,
            partial,
            final_path: path.to_owned(),
        })
    }
    pub fn packet(&mut self, packet: &EncodedPacket) -> Result<()> {
        let base = *self.base_pts.get_or_insert(packet.pts);
        self.file
            .write_all(&(packet.data.len() as u32).to_le_bytes())?;
        self.file
            .write_all(&packet.pts.saturating_sub(base).to_le_bytes())?;
        self.file.write_all(&packet.data)?;
        self.count = self
            .count
            .checked_add(1)
            .context("IVF frame count overflow")?;
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        self.file.flush()?;
        self.file.seek(SeekFrom::Start(24))?;
        self.file.write_all(&self.count.to_le_bytes())?;
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        drop(self.file);
        // A destination can appear after create() checked it. Never replace
        // that file; retain the completed partial if publication fails.
        let mut partial = tempfile::TempPath::try_from_path(&self.partial)?;
        partial.disable_cleanup(true);
        partial
            .persist_noclobber(&self.final_path)
            .with_context(|| {
                format!(
                    "publish IVF without replacing {}; completed recording remains at {}",
                    self.final_path.display(),
                    self.partial.display()
                )
            })?;
        Ok(())
    }
}

fn ivf_recovery_notice(
    capture: &mut Capture,
    events: &Option<mpsc::SyncSender<PipelineEvent>>,
) -> bool {
    let Some(event) = capture.take_recovery_event() else {
        return false;
    };
    let message = match event {
        CaptureRecoveryEvent::Recovering => {
            "Display capture interrupted; reconnecting for up to 5 seconds. Recording repeats the last frame."
        }
        CaptureRecoveryEvent::Recovered => {
            "Display capture recovered on the original GPU and display."
        }
    };
    if let Some(events) = events {
        let _ = events.try_send(PipelineEvent::Notice(message.into()));
    } else {
        eprintln!("{message}");
    }
    event == CaptureRecoveryEvent::Recovered
}

/// The initial proof keeps all D3D/NVENC calls confined to this worker;
/// a bounded writer queue isolates filesystem latency. UI events are best-effort.
fn run_ivf_proof(
    options: RunOptions,
    stop: Arc<AtomicBool>,
    events: Option<mpsc::SyncSender<PipelineEvent>>,
) -> Result<PipelineStats> {
    options.video.validate()?;
    ensure!(
        options
            .seconds
            .is_none_or(|s| s.is_finite() && s > 0.0 && s <= 86400.0),
        "duration must be between 0 and 86400 seconds"
    );
    let mut capture = match &options.display {
        Some(selection) => Capture::open_selected(selection)?,
        None => Capture::open(options.monitor)?,
    };
    let converter = Converter::new(&capture, &options.video)?;
    let mut encoder = Nvenc::new(&capture.device, &options.video)?;
    let views = encoder
        .textures()
        .iter()
        .map(|t| converter.output_view(t))
        .collect::<Result<Vec<_>>>()?;
    let preview = if options.preview {
        Some(PreviewSurface::new(&capture.device, &converter)?)
    } else {
        None
    };
    if let (Some(preview), Some(events)) = (&preview, &events) {
        events
            .send(PipelineEvent::Started(preview.clone()))
            .context("preview UI disconnected")?;
    }
    let mut replay = ReplayBuffer::new(
        options.replay_max_bytes,
        options.replay_seconds * u64::from(options.video.fps),
    )?;
    let mut stats = PipelineStats {
        adapter: capture.adapter_name.clone(),
        output: options.output.display().to_string(),
        ..Default::default()
    };
    let initial_deadline = Instant::now() + Duration::from_secs(5);
    let mut force_keyframe = false;
    let initial_frame = (|| -> Result<()> {
        while !capture.has_frame && !stop.load(Ordering::Relaxed) {
            let frame = capture.next_frame(100)?;
            force_keyframe |= ivf_recovery_notice(&mut capture, &events);
            if frame.is_some() {
                stats.captured += 1;
            }
            ensure!(
                Instant::now() < initial_deadline,
                "no desktop frames received in five seconds"
            );
        }
        Ok(())
    })();
    if stop.load(Ordering::Relaxed) {
        if let Some(events) = events {
            let _ = events.send(PipelineEvent::Finished(stats.clone()));
        }
        return Ok(stats);
    }
    initial_frame?;
    // No writer thread exists during fallible initial-frame acquisition.
    let writer = IvfWriter::create(&options.output, &options.video)?;
    let (output_tx, output_rx) = mpsc::sync_channel::<EncodedPacket>(120);
    let writer_thread =
        thread::Builder::new()
            .name("rcr-disk".into())
            .spawn(move || -> Result<()> {
                let mut writer = writer;
                for packet in output_rx {
                    writer.packet(&packet)?;
                }
                writer.finish()
            })?;
    let start = Instant::now();
    let mut pts = 0u64;
    let fps = u64::from(options.video.fps);
    let total_frames = options.seconds.map(|s| (s * fps as f64).round() as u64);
    // A timer-resolution guard is balanced even on error; no busy-spin pacing.
    struct TimerResolution;
    unsafe {
        windows::Win32::Media::timeBeginPeriod(1);
    }
    impl Drop for TimerResolution {
        fn drop(&mut self) {
            unsafe {
                windows::Win32::Media::timeEndPeriod(1);
            }
        }
    }
    let _timer_resolution = TimerResolution;
    let mut last_report = Instant::now();
    let result = (|| -> Result<()> {
        while !stop.load(Ordering::Relaxed) && total_frames.is_none_or(|n| pts < n) {
            let target = start + Duration::from_nanos(pts * 1_000_000_000 / fps);
            if let Some(remaining) = target.checked_duration_since(Instant::now()) {
                thread::sleep(remaining);
            }
            let behind = start.elapsed().as_nanos() as u64 * fps / 1_000_000_000;
            if behind > pts + 1 {
                stats.missed_deadlines += behind - pts;
                pts = behind;
                if total_frames.is_some_and(|n| pts >= n) {
                    break;
                }
            }
            let frame = capture.next_frame(0)?;
            force_keyframe |= ivf_recovery_notice(&mut capture, &events);
            if frame.is_some() {
                stats.captured += 1;
            } else {
                stats.repeated += 1;
            }
            while let Some(packet) = encoder.receive(false)? {
                consume(packet, &output_tx, &mut replay, &mut stats, false)?;
            }
            if encoder.free_surface().is_none() {
                let before = Instant::now();
                if let Some(packet) = encoder.receive(true)? {
                    consume(packet, &output_tx, &mut replay, &mut stats, false)?;
                }
                stats.encode_wait_ms_total += before.elapsed().as_secs_f64() * 1000.0;
            }
            let (slot, _) = encoder.free_surface().context("encoder pool exhausted")?;
            converter.convert(&views[slot], false)?;
            encoder.submit(
                &slot,
                pts,
                force_keyframe || pts.is_multiple_of(u64::from(options.video.keyframe_interval)),
            )?;
            force_keyframe = false;
            stats.submitted += 1;
            if pts.is_multiple_of(2) {
                if let Some(preview) = &preview {
                    if preview.publish(&converter, &capture.context)? {
                        stats.preview_published += 1;
                    }
                }
            }
            pts += 1;
            stats.elapsed_seconds = start.elapsed().as_secs_f64();
            if last_report.elapsed() >= Duration::from_millis(500) {
                if let Some(events) = &events {
                    let _ = events.try_send(PipelineEvent::Stats(stats.clone()));
                }
                last_report = Instant::now();
            }
        }
        Ok(())
    })();
    // A terminal capture error must not skip already-submitted NVENC frames.
    // A healthy writer is allowed to drain its bounded queue during shutdown.
    let finalize = Instant::now();
    let drain_result = (|| -> Result<()> {
        encoder.flush()?;
        while encoder.pending() > 0 {
            if let Some(packet) = encoder.receive(true)? {
                consume(packet, &output_tx, &mut replay, &mut stats, true)?;
            }
        }
        Ok(())
    })();
    drop(output_tx);
    let writer_result = writer_thread
        .join()
        .map_err(|_| anyhow::anyhow!("output writer panicked"))?;
    stats.elapsed_seconds = start.elapsed().as_secs_f64();
    stats.finalization_seconds = finalize.elapsed().as_secs_f64();
    result?;
    drain_result?;
    writer_result?;
    fs::write(
        options.output.with_extension("stats.json"),
        serde_json::to_vec_pretty(&stats)?,
    )?;
    if let Some(events) = events {
        let _ = events.send(PipelineEvent::Finished(stats.clone()));
    }
    Ok(stats)
}

fn consume(
    packet: EncodedPacket,
    tx: &mpsc::SyncSender<EncodedPacket>,
    replay: &mut ReplayBuffer,
    stats: &mut PipelineStats,
    wait_for_writer: bool,
) -> Result<()> {
    stats.encoded += 1;
    stats.compressed_bytes += packet.data.len() as u64;
    replay.push(packet.clone());
    stats.replay_bytes = replay.bytes();
    if wait_for_writer {
        tx.send(packet)
            .context("output writer disconnected during final encoder drain")?;
    } else {
        tx.try_send(packet)
            .context("output writer cannot keep up (bounded queue full or disk error)")?;
    }
    Ok(())
}

struct CliOptions {
    options: RunOptions,
    save_at: Option<f64>,
    replay_path: Option<PathBuf>,
}
fn parse_cli(args: impl IntoIterator<Item = String>) -> Result<Option<CliOptions>> {
    let mut options = RunOptions::default();
    let mut save_at = None;
    let mut replay_path = None;
    let mut process = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("missing value for {arg}"))
        };
        match arg.as_str() {
            "--seconds" => options.seconds = Some(value()?.parse()?),
            "--output" => options.output = value()?.into(),
            "--monitor" => options.monitor = value()?.parse()?,
            "--width" => options.video.width = value()?.parse()?,
            "--height" => options.video.height = value()?.parse()?,
            "--fps" => options.video.fps = value()?.parse()?,
            "--bitrate" => options.video.bitrate = value()?.parse()?,
            "--audio" => {
                options.audio.clear();
                for source in value()?.split(',') {
                    match source {
                        "system" => options.audio.push(AudioTrack::system()),
                        "mic" => options.audio.push(AudioTrack::microphone()),
                        "none" => {}
                        _ => bail!("audio source must be system, mic, or none"),
                    }
                }
            }
            "--audio-process" => {
                ensure!(
                    process.is_none(),
                    "--audio-process may be specified only once"
                );
                let pid: u32 = value()?
                    .parse()
                    .context("--audio-process requires a numeric process ID")?;
                ensure!(pid > 0, "--audio-process requires a nonzero process ID");
                process = Some(pid);
            }
            "--replay-only" => options.record_to_disk = false,
            "--save-on-stop" => options.save_on_stop = true,
            "--suppress-mic" => options.suppress_microphone = true,
            "--replay-seconds" => options.replay_seconds = value()?.parse()?,
            "--save-at" => save_at = Some(value()?.parse::<f64>()?),
            "--save-replay" => replay_path = Some(PathBuf::from(value()?)),
            "--help" | "-h" => {
                return Ok(None);
            }
            _ => bail!("unknown argument: {arg}"),
        }
    }
    if let Some(pid) = process {
        options
            .audio
            .retain(|track| track.source == rcr_media_core::AudioSource::Microphone);
        options.audio.insert(0, AudioTrack::process(pid)?);
    }
    let mut ids = Vec::new();
    for track in &options.audio {
        ensure!(!ids.contains(&track.id), "duplicate audio source");
        ids.push(track.id);
    }
    Ok(Some(CliOptions {
        options,
        save_at,
        replay_path,
    }))
}

pub fn cli() -> Result<()> {
    let Some(CliOptions {
        options,
        save_at,
        replay_path,
    }) = parse_cli(std::env::args().skip(1))?
    else {
        println!(
            "Optional microphone suppression: --suppress-mic with --audio mic or system,mic. Runs on replay save only; capture/continuous recordings stay raw."
        );
        println!(
            "rcr-capture [--seconds 10] [--output recordings/proof.mkv] [--audio system,mic] [--audio-process PID] [--monitor 0] [--width 2560] [--height 1440] [--fps 60] [--bitrate 25000000]\nApplication audio: --audio-process PID replaces system audio with that process and its children, preserving --audio mic. Requires supported Windows process loopback; activation failure never captures system audio instead. Select the actual application, not its launcher. Target exit stops its audio source; relaunch requires a new PID.\nReplay: --replay-only --save-on-stop; or --save-at 5 --save-replay recordings/replay.mkv\nDisplay: --monitor is a zero-based ordinal among NVIDIA-attached DXGI outputs, not the Windows display number. Desktop display selections use an exact monitor identity instead.\nSDR / AV1 NVENC. MKV carries independent PCM audio tracks. IVF remains available for video-only proofs. Existing files are never overwritten."
        );
        return Ok(());
    };
    let commands = if let Some(seconds) = save_at {
        ensure!(
            seconds.is_finite() && seconds > 0. && seconds < 86400.,
            "invalid save time"
        );
        let path = replay_path.context("--save-at requires --save-replay PATH")?;
        let (tx, rx) = mpsc::sync_channel(1);
        let history = options.replay_seconds;
        let suppress_microphone = options.suppress_microphone;
        thread::spawn(move || {
            thread::sleep(Duration::from_secs_f64(seconds));
            let _ = tx.send(RecorderCommand::SaveReplay {
                path,
                seconds: history,
                suppress_microphone,
            });
        });
        Some(rx)
    } else {
        None
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&run_controlled(
            options,
            Arc::new(AtomicBool::new(false)),
            None,
            commands
        )?)?
    );
    Ok(())
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    fn ivf_packet() -> EncodedPacket {
        EncodedPacket {
            data: vec![1, 2, 3, 4].into(),
            pts: 42,
            keyframe: true,
        }
    }
    #[test]
    fn ivf_publication_preserves_a_destination_created_during_capture() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recording.ivf");
        let mut writer = IvfWriter::create(&path, &VideoConfig::default()).unwrap();
        writer.packet(&ivf_packet()).unwrap();
        fs::write(&path, b"another recording").unwrap();
        let error = writer.finish().unwrap_err();
        assert!(error.to_string().contains("completed recording remains"));
        assert_eq!(fs::read(&path).unwrap(), b"another recording");
        let partial = fs::read(path.with_extension("ivf.partial")).unwrap();
        assert_eq!(&partial[..4], b"DKIF");
        assert_eq!(u32::from_le_bytes(partial[24..28].try_into().unwrap()), 1);
        assert_eq!(&partial[44..], &[1, 2, 3, 4]);
    }
    #[test]
    fn successful_ivf_publication_removes_only_its_owned_partial() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recording.ivf");
        let mut writer = IvfWriter::create(&path, &VideoConfig::default()).unwrap();
        writer.packet(&ivf_packet()).unwrap();
        writer.finish().unwrap();
        assert!(!path.with_extension("ivf.partial").exists());
        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"DKIF");
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 1);
        assert_eq!(&bytes[44..], &[1, 2, 3, 4]);
        assert!(IvfWriter::create(&path, &VideoConfig::default()).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    fn parsed(args: &[&str]) -> Result<RunOptions> {
        Ok(parse_cli(args.iter().map(|arg| arg.to_string()))?
            .unwrap()
            .options)
    }
    #[test]
    fn application_audio_replaces_system_and_keeps_microphone_in_either_argument_order() {
        for args in [
            vec!["--audio", "system,mic", "--audio-process", "42"],
            vec!["--audio-process", "42", "--audio", "system,mic"],
        ] {
            let options = parsed(&args).unwrap();
            assert_eq!(options.audio.len(), 2);
            assert_eq!(
                options.audio[0].source,
                rcr_media_core::AudioSource::Process {
                    pid: 42,
                    created_at_hns: None
                }
            );
            assert_eq!(
                options.audio[1].source,
                rcr_media_core::AudioSource::Microphone
            );
            assert_eq!((options.audio[0].id, options.audio[1].id), (2, 3));
        }
        assert!(parsed(&[]).unwrap().audio.is_empty());
        assert_eq!(
            parsed(&["--audio", "system"]).unwrap().audio[0].source,
            rcr_media_core::AudioSource::System
        );
        assert_eq!(parsed(&["--audio-process", "42"]).unwrap().audio.len(), 1);
    }
    #[test]
    fn invalid_or_repeated_application_targets_are_rejected_before_capture() {
        for args in [
            vec!["--audio-process", "0"],
            vec!["--audio-process", "-1"],
            vec!["--audio-process", "4294967296"],
            vec!["--audio-process"],
            vec!["--audio-process", "42", "--audio-process", "43"],
            vec!["--audio", "mic,mic"],
        ] {
            assert!(parsed(&args).is_err(), "Accepted invalid args: {args:?}");
        }
    }
}
