use crate::{
    audio::{self, AudioEvent, AudioWorkers},
    gpu::{Capture, CaptureRecoveryEvent, Converter, PreviewSurface},
    nvenc::Nvenc,
    pipeline::{observe, CapturePhase, PipelineEvent, RecorderCommand, RunOptions},
};
use anyhow::{ensure, Context, Result};
use media_core::{
    AudioTrack, AvReplayBuffer, MediaPacket, PipelineStats, VideoCapture, VideoConfig, VideoEncoder,
};
use media_io::{contains_sequence_header, save_snapshot, MatroskaWriter};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

type Events = Option<mpsc::SyncSender<PipelineEvent>>;
pub struct ReplaySave {
    worker: Option<thread::JoinHandle<Result<PathBuf>>>,
    cancel: Arc<AtomicBool>,
}
impl ReplaySave {
    pub fn is_finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn complete(mut self) -> Result<PathBuf> {
        self.worker
            .take()
            .expect("owned replay worker")
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Replay writer panicked")))
    }
}
impl Drop for ReplaySave {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
type SaveJob = Option<ReplaySave>;

fn handoff_save(job: &mut SaveJob, events: &Events) {
    if let Some(events) = events.as_ref() {
        if let Some(job) = job.take() {
            // Failed delivery drops the owning handle, cancelling and joining
            // the worker instead of detaching an invisible replay save.
            let _ = events.send(PipelineEvent::ReplayPending(job));
        }
    }
}

struct RecordingOutput {
    sender: mpsc::SyncSender<MediaPacket>,
    deferred: Option<MediaPacket>,
    disconnected: bool,
    finalization_error: Option<String>,
}

impl RecordingOutput {
    fn flush_deferred(&mut self) -> Result<()> {
        if let Some(packet) = self.deferred.take() {
            self.send_final(packet)?;
        }
        Ok(())
    }

    fn send_final(&mut self, packet: MediaPacket) -> Result<()> {
        if self.disconnected {
            return Ok(());
        }
        if self.sender.send(packet).is_err() {
            self.disconnected = true;
            self.finalization_error =
                Some("recording writer disconnected during finalization".into());
            anyhow::bail!("recording writer disconnected during finalization");
        }
        Ok(())
    }

    fn push(&mut self, packet: MediaPacket, finalizing: bool) -> Result<()> {
        if self.disconnected {
            return Ok(());
        }
        if finalizing {
            return self.send_final(packet);
        }
        match self.sender.try_send(packet) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(packet)) => {
                // Stop immediately, but retain this one packet until shutdown
                // drains the writer. A full queue must not leave a hole before
                // the remaining encoder/audio tail is finalized.
                self.deferred = Some(packet);
                anyhow::bail!(
                    "recording writer fell behind; capture stopped to preserve output integrity"
                );
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.disconnected = true;
                anyhow::bail!(
                    "recording writer disconnected; capture stopped to preserve output integrity"
                );
            }
        }
    }
}

fn notify(events: &Events, event: PipelineEvent) {
    if let Some(tx) = events {
        let _ = tx.try_send(event);
    }
}

// Replay outcomes must not disappear behind lossy statistics updates. Shutdown
// disconnects the receiver before joining, so this cannot wait on a closed UI.
fn replay_notify(events: &Events, event: PipelineEvent) {
    let undelivered = match events {
        Some(events) => events.send(event).err().map(|error| error.0),
        None => Some(event),
    };
    match undelivered {
        Some(PipelineEvent::Notice(message)) => eprintln!("{message}"),
        Some(PipelineEvent::ReplaySaved(path)) => eprintln!("Replay saved: {}", path.display()),
        _ => {}
    }
}

/// The command channel has eight slots; finalization services the same bounded
/// batch even after Stop, or explicitly rejects it when the UI has closed.
fn service_replay_commands(
    commands: Option<&mpsc::Receiver<RecorderCommand>>,
    rejection: Option<&str>,
    mut execute: impl FnMut(RecorderCommand) -> Result<()>,
) -> Vec<String> {
    let mut failures = Vec::new();
    if let Some(commands) = commands {
        for command in commands.try_iter().take(8) {
            let RecorderCommand::SaveReplay { path, .. } = &command;
            let path = path.clone();
            let result = match rejection {
                Some(reason) => Err(anyhow::anyhow!("{reason}")),
                None => execute(command),
            };
            if let Err(error) = result {
                failures.push(format!("Replay not saved ({}): {error:#}", path.display()));
            }
        }
    }
    failures
}

// Only an explicit Stop before the first encoded frame can turn startup into
// cancellation. Device/permission failures otherwise remain real errors.
fn startup_step<T>(result: Result<T>, stopped: bool, encoded: u64) -> Result<Option<T>> {
    if stopped && encoded == 0 {
        drop(result);
        Ok(None)
    } else {
        result.map(Some)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinalizeStream {
    Video,
    Audio,
}

fn combine_failure(result: Result<()>, next: Result<()>, stage: &str) -> Result<()> {
    match (result, next) {
        (Ok(()), result) => result.with_context(|| stage.to_owned()),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(next)) => Err(anyhow::anyhow!("{error:#}; {stage} also failed: {next:#}")),
    }
}

// Capture/access-loss failures must not skip NVENC's pending frames or the
// audio workers' final packets. Each stream gets its own cleanup attempt even
// if the other stream (or the output writer) has already failed.
fn finalize_streams(
    mut result: Result<()>,
    mut finalize: impl FnMut(FinalizeStream) -> Result<()>,
) -> Result<()> {
    for (stream, stage) in [
        (FinalizeStream::Video, "video finalization"),
        (FinalizeStream::Audio, "audio finalization"),
    ] {
        result = combine_failure(result, finalize(stream), stage);
    }
    result
}

fn report_capture_recovery(capture: &mut Capture, events: &Events) -> bool {
    match capture.take_recovery_event() {
        Some(CaptureRecoveryEvent::Recovering) => {
            replay_notify(
                events,
                PipelineEvent::Notice(
                    "Display changed. Reconnecting capture for up to 5 seconds…".into(),
                ),
            );
            false
        }
        Some(CaptureRecoveryEvent::Recovered) => {
            replay_notify(
                events,
                PipelineEvent::Notice("Display capture reconnected.".into()),
            );
            true
        }
        None => false,
    }
}

fn finish_empty_capture(
    stats: PipelineStats,
    commands: Option<&mpsc::Receiver<RecorderCommand>>,
    events: &Events,
) -> PipelineStats {
    let failures = service_replay_commands(
        commands,
        Some("capture stopped before the first encoded frame"),
        |_| unreachable!("empty capture cannot launch a replay save"),
    );
    for failure in failures {
        replay_notify(events, PipelineEvent::Notice(failure));
    }
    if let Some(events) = events {
        let _ = events.send(PipelineEvent::Finished(stats.clone()));
    }
    stats
}

fn writer(
    path: PathBuf,
    video: VideoConfig,
    audio: Vec<AudioTrack>,
    rx: mpsc::Receiver<MediaPacket>,
    final_stats: mpsc::Receiver<PipelineStats>,
) -> Result<PathBuf> {
    let mut muxer = None;
    let mut queue: Vec<MediaPacket> = Vec::new();
    let mut newest = 0u64;
    for packet in rx {
        newest = newest.max(packet.pts_ns);
        if muxer.is_none() && packet.track == 1 {
            muxer = Some(MatroskaWriter::create(&path, &video, &audio, &packet)?);
        }
        queue.push(packet);
        // A short reorder window interleaves source callbacks with encoder completion.
        if let Some(muxer) = &mut muxer {
            queue.sort_by_key(|p| (p.pts_ns, p.track));
            let cutoff = newest.saturating_sub(100_000_000);
            let count = queue.partition_point(|p| p.pts_ns < cutoff);
            for packet in queue.drain(..count) {
                muxer.packet(&packet)?;
            }
        }
        ensure!(
            queue.len() <= 512,
            "mux interleave buffer exceeded its bound"
        );
    }
    let mut muxer = muxer.context("no video frames reached the output writer")?;
    queue.sort_by_key(|p| (p.pts_ns, p.track));
    for packet in queue {
        muxer.packet(&packet)?;
    }
    let mut statistics = serde_json::to_value(
        final_stats
            .recv()
            .context("recording statistics sender stopped")?,
    )?;
    // A moved clip must not carry a stale machine-local output pathname.
    statistics
        .as_object_mut()
        .expect("PipelineStats is an object")
        .remove("output");
    muxer.finish_with_metadata(&serde_json::json!({
        "statistics": statistics,
        "statistics_scope": "capture_stream_drain"
    }))
}

fn consume(
    packet: MediaPacket,
    ring: &mut AvReplayBuffer,
    output: &mut Option<RecordingOutput>,
    stats: &mut PipelineStats,
    finalizing: bool,
) -> Result<()> {
    if packet.track == 1 {
        stats.encoded += 1;
        stats.compressed_bytes += packet.data.len() as u64;
    } else {
        stats.audio_packets += 1;
    }
    ring.push(packet.clone());
    stats.replay_bytes = ring.bytes();
    stats.replay_seconds = ring.duration_ns() as f64 / 1e9;
    if let Some(output) = output {
        if finalizing {
            // Keep draining the encoder/audio into the replay ring even if the
            // disk writer died. Its sticky failure is reported after both drains.
            let _ = output.push(packet, true);
        } else {
            output.push(packet, false)?;
        }
    }
    Ok(())
}

fn receive_audio(
    audio: &AudioWorkers,
    ring: &mut AvReplayBuffer,
    output: &mut Option<RecordingOutput>,
    stats: &mut PipelineStats,
    events: &Events,
    finalizing: bool,
) -> Result<()> {
    for event in audio.receiver.try_iter() {
        match event {
            AudioEvent::Packet(packet) => consume(packet, ring, output, stats, finalizing)?,
            AudioEvent::Discontinuity => stats.audio_discontinuities += 1,
            AudioEvent::Notice(notice) => notify(events, PipelineEvent::Notice(notice)),
            AudioEvent::Error(error) => {
                if !stats.audio_errors.contains(&error) {
                    stats.audio_errors.push(error.clone());
                    notify(events, PipelineEvent::Notice(error));
                }
            }
        }
    }
    Ok(())
}

fn start_save(
    job: &mut SaveJob,
    path: PathBuf,
    seconds: u64,
    ring: &AvReplayBuffer,
    options: &RunOptions,
    events: &Events,
    suppress_microphone: bool,
) -> Result<()> {
    if job.is_some() {
        anyhow::bail!("another replay save is already in progress; try again after it finishes");
    }
    ensure!(
        (1..=3600).contains(&seconds),
        "replay duration must be between 1 and 3600 seconds"
    );
    if suppress_microphone
        && !options
            .audio
            .iter()
            .any(|t| t.source == media_core::AudioSource::Microphone)
    {
        anyhow::bail!("enable microphone capture before requesting microphone suppression");
    }
    let mut packets = ring.snapshot(seconds);
    if packets.is_empty() {
        anyhow::bail!("the replay buffer is not ready yet");
    }
    let video = options.video.clone();
    let mut audio = options.audio.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    if suppress_microphone {
        notify(
            events,
            PipelineEvent::Notice(
                "Suppressing microphone noise in the saved replay… Capture stays raw.".into(),
            ),
        );
    }
    // Only one snapshot can be pinned: retained payload <= one additional ring budget.
    *job = Some(ReplaySave {
        cancel,
        worker: Some(
            thread::Builder::new()
                .name("rcr-replay-save".into())
                .spawn(move || {
                    // Offline CPU work must not compete with capture scheduling.
                    unsafe {
                        let _ = windows::Win32::System::Threading::SetThreadPriority(
                            windows::Win32::System::Threading::GetCurrentThread(),
                            windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL,
                        );
                    }
                    if suppress_microphone {
                        media_io::suppress_microphone(&mut packets, &audio, || {
                            worker_cancel.load(Ordering::Relaxed)
                        })?;
                        for track in &mut audio {
                            if track.source == media_core::AudioSource::Microphone {
                                track.name.push_str(" (DPDFNet)");
                            }
                        }
                    }
                    save_snapshot(&path, &video, &audio, &packets)
                })?,
        ),
    });
    Ok(())
}
fn finish_save(job: &mut SaveJob, stats: &mut PipelineStats, events: &Events, wait: bool) {
    if wait {
        while job.as_ref().is_some_and(|j| !j.is_finished()) {
            if let (Some(job), Some(events)) = (job.as_ref(), events) {
                if matches!(
                    events.try_send(PipelineEvent::Stats(stats.clone())),
                    Err(mpsc::TrySendError::Disconnected(_))
                ) {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
    if job.as_ref().is_some_and(ReplaySave::is_finished) {
        match job.take().unwrap().complete() {
            Ok(path) => {
                stats.replay_saves += 1;
                replay_notify(events, PipelineEvent::ReplaySaved(path));
            }
            Err(error) => replay_notify(
                events,
                PipelineEvent::Notice(format!("Replay save failed: {error:#}")),
            ),
        }
    }
}

pub fn run(
    options: RunOptions,
    stop: Arc<AtomicBool>,
    events: Events,
    commands: Option<mpsc::Receiver<RecorderCommand>>,
) -> Result<PipelineStats> {
    options.video.validate()?;
    ensure!(
        options.output.extension().is_some_and(|e| e == "mkv"),
        "recording output must use .mkv"
    );
    ensure!(
        options
            .seconds
            .is_none_or(|s| s.is_finite() && s > 0. && s <= 86400.),
        "invalid recording duration"
    );
    ensure!(
        !(options.record_to_disk && options.save_on_stop),
        "save-on-stop is for replay-only mode"
    );
    if options.record_to_disk || options.save_on_stop {
        ensure!(
            !options.output.exists(),
            "output already exists: {}",
            options.output.display()
        );
    }
    ensure!(
        options.audio.len() <= 2,
        "at most two audio tracks are supported"
    );
    let mut ids = Vec::new();
    for track in &options.audio {
        ensure!((2..=126).contains(&track.id), "invalid audio track number");
        ensure!(!ids.contains(&track.id), "duplicate audio source");
        ids.push(track.id);
    }
    if stop.load(Ordering::Relaxed) {
        return Ok(finish_empty_capture(
            PipelineStats {
                output: options.output.display().to_string(),
                ..Default::default()
            },
            commands.as_ref(),
            &events,
        ));
    }
    let mut ring = AvReplayBuffer::new(options.replay_max_bytes, options.replay_seconds)?;
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
    let mut stats = PipelineStats {
        adapter: capture.adapter_name.clone(),
        output: options.output.display().to_string(),
        ..Default::default()
    };
    let initial_deadline = Instant::now() + Duration::from_secs(5);
    let first_frame = (|| -> Result<()> {
        while !capture.has_frame && !stop.load(Ordering::Relaxed) {
            if capture.next_frame(100)?.is_some() {
                stats.captured += 1;
            }
            report_capture_recovery(&mut capture, &events);
            ensure!(
                Instant::now() < initial_deadline,
                "no desktop frames received"
            );
        }
        Ok(())
    })();
    if startup_step(first_frame, stop.load(Ordering::Relaxed), stats.encoded)?.is_none() {
        return Ok(finish_empty_capture(stats, commands.as_ref(), &events));
    }
    // No writer exists until audio setup and clock initialization both succeed.
    // A failed or cancelled activation therefore cannot detach a writer thread.
    let audio_start = (|| -> Result<AudioWorkers> {
        let audio = AudioWorkers::start_cancellable(&options.audio, 0, stop.clone())?;
        audio.set_epoch(audio::qpc_hns()?);
        Ok(audio)
    })();
    let Some(mut audio) = startup_step(audio_start, stop.load(Ordering::Relaxed), stats.encoded)?
    else {
        return Ok(finish_empty_capture(stats, commands.as_ref(), &events));
    };
    let (mut output, writer_thread, writer_stats) = if options.record_to_disk {
        let (tx, rx) = mpsc::sync_channel(256);
        let (stats_tx, stats_rx) = mpsc::sync_channel(1);
        let path = options.output.clone();
        let video = options.video.clone();
        let audio = options.audio.clone();
        let worker = thread::Builder::new()
            .name("rcr-matroska".into())
            .spawn(move || writer(path, video, audio, rx, stats_rx))?;
        (
            Some(RecordingOutput {
                sender: tx,
                deferred: None,
                disconnected: false,
                finalization_error: None,
            }),
            Some(worker),
            Some(stats_tx),
        )
    } else {
        (None, None, None)
    };
    let start = Instant::now();
    observe(&options, CapturePhase::CaptureStarted);
    let fps = u64::from(options.video.fps);
    let total = options.seconds.map(|s| (s * fps as f64).round() as u64);
    let mut pts = 0u64;
    let mut last_report = Instant::now();
    let mut save_job = None;
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
    let _timer = TimerResolution;
    let result = (|| -> Result<()> {
        while !stop.load(Ordering::Relaxed) && total.is_none_or(|n| pts < n) {
            let target = start + Duration::from_nanos(pts * 1_000_000_000 / fps);
            if let Some(remaining) = target.checked_duration_since(Instant::now()) {
                thread::sleep(remaining);
            }
            let behind = start.elapsed().as_nanos() as u64 * fps / 1_000_000_000;
            if behind > pts + 1 {
                stats.missed_deadlines += behind - pts;
                pts = behind;
                if total.is_some_and(|n| pts >= n) {
                    break;
                }
            }
            if capture.next_frame(0)?.is_some() {
                stats.captured += 1;
            } else {
                stats.repeated += 1;
            }
            let recovered = report_capture_recovery(&mut capture, &events);
            while let Some(packet) = encoder.receive(false)? {
                let mut packet = MediaPacket::video(packet, options.video.fps);
                packet.keyframe = packet.keyframe && contains_sequence_header(&packet.data);
                consume(packet, &mut ring, &mut output, &mut stats, false)?;
            }
            receive_audio(&audio, &mut ring, &mut output, &mut stats, &events, false)?;
            if encoder.free_surface().is_none() {
                let before = Instant::now();
                if let Some(packet) = encoder.receive(true)? {
                    consume(
                        MediaPacket::video(packet, options.video.fps),
                        &mut ring,
                        &mut output,
                        &mut stats,
                        false,
                    )?;
                }
                stats.encode_wait_ms_total += before.elapsed().as_secs_f64() * 1000.;
            }
            let (slot, _) = encoder.free_surface().context("encoder pool exhausted")?;
            converter.convert(&views[slot], false)?;
            encoder.submit(
                &slot,
                pts,
                recovered || pts.is_multiple_of(u64::from(options.video.keyframe_interval)),
            )?;
            stats.submitted += 1;
            if pts.is_multiple_of(2) {
                if let Some(preview) = &preview {
                    if preview.publish(&converter, &capture.context)? {
                        stats.preview_published += 1;
                    }
                }
            }
            finish_save(&mut save_job, &mut stats, &events, false);
            let failures = service_replay_commands(commands.as_ref(), None, |command| {
                let RecorderCommand::SaveReplay {
                    path,
                    seconds,
                    suppress_microphone,
                } = command;
                start_save(
                    &mut save_job,
                    path,
                    seconds,
                    &ring,
                    &options,
                    &events,
                    suppress_microphone,
                )
            });
            for failure in failures {
                replay_notify(&events, PipelineEvent::Notice(failure));
            }
            stats.elapsed_seconds = start.elapsed().as_secs_f64();
            if last_report.elapsed() >= Duration::from_millis(500) {
                notify(&events, PipelineEvent::Stats(stats.clone()));
                last_report = Instant::now();
            }
            pts += 1;
        }
        Ok(())
    })();
    stats.elapsed_seconds = start.elapsed().as_secs_f64();
    observe(&options, CapturePhase::CaptureLoopStopped);
    let finalize = Instant::now();
    if let Some(output) = &mut output {
        // Any failure is retained below; neither stream may be skipped because
        // the disk writer disconnected while draining the one deferred packet.
        let _ = output.flush_deferred();
    }
    let result = finalize_streams(result, |stream| match stream {
        FinalizeStream::Video => {
            let started = Instant::now();
            let result = (|| {
                encoder.flush()?;
                while encoder.pending() > 0 {
                    if let Some(packet) = encoder.receive(true)? {
                        let mut packet = MediaPacket::video(packet, options.video.fps);
                        packet.keyframe = packet.keyframe && contains_sequence_header(&packet.data);
                        consume(packet, &mut ring, &mut output, &mut stats, true)?;
                    }
                }
                Ok(())
            })();
            stats.encoder_finalization_seconds = started.elapsed().as_secs_f64();
            result
        }
        FinalizeStream::Audio => {
            let started = Instant::now();
            audio.stop();
            let result = receive_audio(&audio, &mut ring, &mut output, &mut stats, &events, true);
            stats.audio_finalization_seconds = started.elapsed().as_secs_f64();
            result
        }
    });
    // Release GPU capture before disk/replay processing. Previously the implicit
    // encoder destructor ran after Finished, hiding its completion waits from
    // both finalization statistics and the UI's "finishing" state.
    let releasing = Instant::now();
    drop(views);
    drop(encoder);
    drop(preview);
    drop(converter);
    drop(capture);
    observe(&options, CapturePhase::GpuReleased);
    stats.encoder_finalization_seconds += releasing.elapsed().as_secs_f64();
    let result = combine_failure(
        result,
        match output
            .as_mut()
            .and_then(|output| output.finalization_error.take())
        {
            Some(error) => Err(anyhow::anyhow!(error)),
            None => Ok(()),
        },
        "recording queue finalization",
    );
    drop(output.take());
    // Stop can race the final startup check and arrive just after spawning the
    // writer. Join it here, then treat its expected "no video" result as a
    // cancelled empty session rather than a recording failure.
    if stop.load(Ordering::Relaxed) && stats.encoded == 0 && result.is_ok() {
        drop(writer_stats);
        if let Some(worker) = writer_thread {
            let _ = worker.join();
        }
        stats.finalization_seconds = finalize.elapsed().as_secs_f64();
        return Ok(finish_empty_capture(stats, commands.as_ref(), &events));
    }
    stats.finalization_seconds = finalize.elapsed().as_secs_f64();
    if let Err(error) = &result {
        stats.error = Some(format!("{error:#}"));
    }
    // Publish the recording before waiting for offline replay cleaning. The
    // single snapshot describes capture drain, not later replay completions.
    if let Some(sender) = writer_stats {
        let _ = sender.send(stats.clone());
    }
    let writer_started = Instant::now();
    let writer_result = writer_thread.map(|worker| {
        worker
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("recording writer panicked")))
    });
    stats.writer_finalization_seconds = writer_started.elapsed().as_secs_f64();
    if writer_result.as_ref().is_some_and(Result::is_ok) {
        observe(&options, CapturePhase::RecordingPublished);
    }
    // A SaveReplay accepted immediately before Stop is still owned by this
    // session. Drain it before destroying the ring, including replay→record.
    // Quit disconnects the UI first and cancels instead of launching new work.
    let closing = events.as_ref().is_some_and(|events| {
        matches!(
            events.try_send(PipelineEvent::Stats(stats.clone())),
            Err(mpsc::TrySendError::Disconnected(_))
        )
    });
    let rejection = if closing {
        Some("cancelled because the application is closing")
    } else {
        None
    };
    let replay_started = Instant::now();
    finish_save(&mut save_job, &mut stats, &events, false);
    let failures = service_replay_commands(commands.as_ref(), rejection, |command| {
        let RecorderCommand::SaveReplay {
            path,
            seconds,
            suppress_microphone,
        } = command;
        start_save(
            &mut save_job,
            path,
            seconds,
            &ring,
            &options,
            &events,
            suppress_microphone,
        )
    });
    for failure in failures {
        replay_notify(&events, PipelineEvent::Notice(failure));
    }
    // Desktop owns unfinished offline work across capture sessions. Handoff
    // occurs only after GPU retirement and disk publication, so starting the
    // next capture cannot overlap the old hardware session. CLI callers keep
    // synchronous completion and save-on-stop still waits for its output.
    if !options.save_on_stop && !closing {
        let pending = save_job.is_some();
        handoff_save(&mut save_job, &events);
        if pending && save_job.is_none() {
            observe(&options, CapturePhase::ReplayHandedOff);
        }
    }
    finish_save(&mut save_job, &mut stats, &events, true);
    let result = if result.is_ok() && options.save_on_stop {
        let saved = (|| -> Result<()> {
            let previous_saves = stats.replay_saves;
            start_save(
                &mut save_job,
                options.output.clone(),
                options.replay_seconds,
                &ring,
                &options,
                &events,
                options.suppress_microphone,
            )?;
            finish_save(&mut save_job, &mut stats, &events, true);
            ensure!(
                stats.replay_saves > previous_saves,
                "save-on-stop replay did not complete"
            );
            Ok(())
        })();
        combine_failure(result, saved, "save-on-stop replay")
    } else {
        result
    };
    stats.replay_finalization_seconds = replay_started.elapsed().as_secs_f64();
    stats.finalization_seconds = finalize.elapsed().as_secs_f64();
    let result = combine_failure(
        result,
        writer_result.map_or(Ok(()), |result| result.map(|_| ())),
        "recording writer finalization",
    );
    stats.finalization_seconds = finalize.elapsed().as_secs_f64();
    if let Err(error) = &result {
        stats.error = Some(format!("{error:#}"));
    }
    result?;
    observe(&options, CapturePhase::Finished);
    if let Some(events) = events {
        let _ = events.send(PipelineEvent::Finished(stats.clone()));
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gated_replay() -> (ReplaySave, mpsc::Sender<()>, mpsc::Receiver<bool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (release, gate) = mpsc::channel();
        let (finished, observed) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            while !worker_cancel.load(Ordering::Relaxed) && gate.try_recv().is_err() {
                thread::yield_now();
            }
            let cancelled = worker_cancel.load(Ordering::Relaxed);
            finished.send(cancelled).unwrap();
            ensure!(!cancelled, "replay cancelled");
            Ok(PathBuf::from("accepted-replay.mkv"))
        });
        (
            ReplaySave {
                worker: Some(worker),
                cancel,
            },
            release,
            observed,
        )
    }

    #[test]
    fn stop_hands_off_an_accepted_replay_without_waiting_or_cancelling() {
        let (replay, release, observed) = gated_replay();
        let (tx, rx) = mpsc::sync_channel(1);
        let mut job = Some(replay);
        handoff_save(&mut job, &Some(tx));
        assert!(job.is_none());
        let PipelineEvent::ReplayPending(replay) = rx.recv().unwrap() else {
            panic!("expected ownership of the pending replay");
        };
        drop(rx); // The capture receiver retires before the offline save.
        assert!(!replay.is_finished());
        assert!(!replay.cancel.load(Ordering::Relaxed));
        release.send(()).unwrap();
        assert_eq!(
            replay.complete().unwrap(),
            PathBuf::from("accepted-replay.mkv")
        );
        assert!(!observed.recv().unwrap());
    }

    #[test]
    fn cli_keeps_ownership_until_the_replay_finishes() {
        let (replay, release, observed) = gated_replay();
        let mut job = Some(replay);
        handoff_save(&mut job, &None);
        assert!(job.is_some());
        release.send(()).unwrap();
        let mut stats = PipelineStats::default();
        finish_save(&mut job, &mut stats, &None, true);
        assert!(job.is_none());
        assert_eq!(stats.replay_saves, 1);
        assert!(!observed.recv().unwrap());
    }

    #[test]
    fn late_replay_failure_survives_capture_receiver_retirement() {
        let replay = ReplaySave {
            worker: Some(thread::spawn(|| anyhow::bail!("fixture disk full"))),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let mut job = Some(replay);
        handoff_save(&mut job, &Some(tx));
        let PipelineEvent::ReplayPending(replay) = rx.recv().unwrap() else {
            panic!("expected pending replay ownership");
        };
        drop(rx);
        assert!(replay
            .complete()
            .unwrap_err()
            .to_string()
            .contains("fixture disk full"));
    }

    #[test]
    fn closed_ui_and_dropped_owner_cancel_and_join_pending_replays() {
        for failed_delivery in [false, true] {
            let (replay, _release, observed) = gated_replay();
            if failed_delivery {
                let (tx, rx) = mpsc::sync_channel(1);
                drop(rx);
                let mut job = Some(replay);
                handoff_save(&mut job, &Some(tx));
                assert!(job.is_none());
            } else {
                drop(replay);
            }
            // Drop does not return until the worker observes cancellation.
            assert!(observed.try_recv().unwrap());
        }
    }

    fn video_packet(pts_ns: u64) -> MediaPacket {
        MediaPacket {
            track: 1,
            pts_ns,
            duration_ns: 16_666_667,
            keyframe: true,
            data: vec![0x12, 0, 0x0a, 2, 0x18, 0].into(),
        }
    }

    #[test]
    fn writer_embeds_final_statistics_before_publish_and_does_not_create_a_sidecar() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metadata.mkv");
        let (tx, rx) = mpsc::sync_channel(2);
        let (stats_tx, stats_rx) = mpsc::sync_channel(1);
        tx.send(video_packet(0)).unwrap();
        drop(tx);
        stats_tx
            .send(PipelineStats {
                encoded: 1,
                replay_saves: 2,
                output: "machine-local-output-sentinel".into(),
                ..Default::default()
            })
            .unwrap();
        writer(path.clone(), VideoConfig::default(), vec![], rx, stats_rx).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes
            .windows(b"\"replay_saves\":2".len())
            .any(|slice| slice == b"\"replay_saves\":2"));
        assert!(!bytes
            .windows(b"machine-local-output-sentinel".len())
            .any(|slice| slice == b"machine-local-output-sentinel"));
        assert!(!path.with_extension("stats.json").exists());
        assert!(!path.with_extension("mkv.partial").exists());
    }

    #[test]
    fn cancelled_statistics_handoff_preserves_the_unpublished_recording() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cancelled.mkv");
        let (tx, rx) = mpsc::sync_channel(2);
        let (stats_tx, stats_rx) = mpsc::sync_channel(1);
        tx.send(video_packet(0)).unwrap();
        drop(tx);
        drop(stats_tx);
        assert!(writer(path.clone(), VideoConfig::default(), vec![], rx, stats_rx).is_err());
        assert!(!path.exists());
        assert!(path.with_extension("mkv.partial").exists());
    }

    #[test]
    fn full_writer_queue_retains_one_packet_and_finalization_delivers_ordered_tail() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut output = Some(RecordingOutput {
            sender,
            deferred: None,
            disconnected: false,
            finalization_error: None,
        });
        let mut ring = AvReplayBuffer::new(1_000_000, 10).unwrap();
        let mut stats = PipelineStats::default();
        consume(video_packet(0), &mut ring, &mut output, &mut stats, false).unwrap();
        assert!(consume(
            video_packet(16_666_667),
            &mut ring,
            &mut output,
            &mut stats,
            false
        )
        .is_err());
        assert_eq!(stats.encoded, 2);
        assert_eq!(
            output.as_ref().unwrap().deferred.as_ref().unwrap().pts_ns,
            16_666_667
        );
        let writer = thread::spawn(move || {
            receiver
                .into_iter()
                .take(3)
                .map(|packet| packet.pts_ns)
                .collect::<Vec<_>>()
        });
        output.as_mut().unwrap().flush_deferred().unwrap();
        consume(
            video_packet(33_333_334),
            &mut ring,
            &mut output,
            &mut stats,
            true,
        )
        .unwrap();
        assert_eq!(writer.join().unwrap(), [0, 16_666_667, 33_333_334]);
        assert_eq!(stats.encoded, 3); // Retrying delivery does not count a packet twice.
        assert!(output.as_ref().unwrap().deferred.is_none());
    }

    #[test]
    fn failed_disk_writer_does_not_interrupt_final_replay_ring_drain() {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let mut output = Some(RecordingOutput {
            sender,
            deferred: None,
            disconnected: false,
            finalization_error: None,
        });
        let mut ring = AvReplayBuffer::new(1_000_000, 10).unwrap();
        let mut stats = PipelineStats::default();
        consume(video_packet(0), &mut ring, &mut output, &mut stats, true).unwrap();
        consume(
            video_packet(16_666_667),
            &mut ring,
            &mut output,
            &mut stats,
            true,
        )
        .unwrap();
        assert_eq!(stats.encoded, 2);
        assert_eq!(
            ring.snapshot(10)
                .iter()
                .map(|packet| packet.pts_ns)
                .collect::<Vec<_>>(),
            [0, 16_666_667]
        );
        let output = output.as_ref().unwrap();
        assert!(output.disconnected);
        assert!(output
            .finalization_error
            .as_ref()
            .unwrap()
            .contains("disconnected"));
    }

    #[test]
    fn capture_failure_still_finalizes_both_streams_and_keeps_all_errors() {
        let mut attempted = Vec::new();
        let result = finalize_streams(Err(anyhow::anyhow!("display access lost")), |stream| {
            attempted.push(stream);
            match stream {
                FinalizeStream::Video => Err(anyhow::anyhow!("encoder removed")),
                FinalizeStream::Audio => Err(anyhow::anyhow!("writer disconnected")),
            }
        });
        assert_eq!(attempted, [FinalizeStream::Video, FinalizeStream::Audio]);
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.starts_with("display access lost"));
        assert!(message.contains("video finalization also failed: encoder removed"));
        assert!(message.contains("audio finalization also failed: writer disconnected"));
    }

    #[test]
    fn clean_stop_reports_finalization_failure_without_skipping_audio() {
        let mut attempted = Vec::new();
        let result = finalize_streams(Ok(()), |stream| {
            attempted.push(stream);
            match stream {
                FinalizeStream::Video => Err(anyhow::anyhow!("encoder timeout")),
                FinalizeStream::Audio => Ok(()),
            }
        });
        assert_eq!(attempted, [FinalizeStream::Video, FinalizeStream::Audio]);
        assert_eq!(
            format!("{:#}", result.unwrap_err()),
            "video finalization: encoder timeout"
        );
    }

    #[test]
    fn startup_cancellation_requires_explicit_stop_and_no_encoded_video() {
        let cancelled: Option<()> =
            startup_step(Err(anyhow::anyhow!("activation cancelled")), true, 0).unwrap();
        assert!(cancelled.is_none());
        let failure =
            startup_step::<()>(Err(anyhow::anyhow!("device access denied")), false, 0).unwrap_err();
        assert_eq!(failure.to_string(), "device access denied");
        let failure =
            startup_step::<()>(Err(anyhow::anyhow!("encoder failed")), true, 1).unwrap_err();
        assert_eq!(failure.to_string(), "encoder failed");
    }
    #[test]
    fn cancelled_successful_startup_releases_initialized_resources() {
        struct Resource(Arc<AtomicBool>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let released = Arc::new(AtomicBool::new(false));
        let result = startup_step(Ok(Resource(released.clone())), true, 0).unwrap();
        assert!(result.is_none());
        assert!(released.load(Ordering::Relaxed));
    }
    fn queued_replays(count: usize) -> mpsc::Receiver<RecorderCommand> {
        let (tx, rx) = mpsc::sync_channel(8);
        for index in 0..count {
            tx.try_send(RecorderCommand::SaveReplay {
                path: PathBuf::from(format!("replay-{index}.mkv")),
                seconds: 10,
                suppress_microphone: false,
            })
            .unwrap_or_else(|_| panic!("fixture command queue full"));
        }
        rx
    }
    #[test]
    fn stop_drains_accepted_replays_and_reports_busy_requests_without_more_jobs() {
        let rx = queued_replays(8);
        let mut job = None;
        let mut calls = 0;
        // A fake single-job executor exercises the same finalization drain as
        // real Stop/transition, without desktop capture or writing media.
        let failures = service_replay_commands(Some(&rx), None, |command| {
            calls += 1;
            anyhow::ensure!(job.is_none(), "another replay save is already in progress");
            let RecorderCommand::SaveReplay { path, .. } = command;
            job = Some(path);
            Ok(())
        });
        assert_eq!(job, Some(PathBuf::from("replay-0.mkv")));
        assert_eq!(calls, 8);
        assert_eq!(failures.len(), 7);
        for (index, failure) in failures.iter().enumerate() {
            assert!(failure.contains(&format!("replay-{}.mkv", index + 1)));
            assert!(failure.contains("already in progress"));
        }
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn quit_rejects_every_accepted_replay_without_running_the_executor() {
        let rx = queued_replays(8);
        let failures = service_replay_commands(
            Some(&rx),
            Some("cancelled because the application is closing"),
            |_| panic!("quit must not launch queued save jobs"),
        );
        assert_eq!(failures.len(), 8);
        for (index, failure) in failures.iter().enumerate() {
            assert!(failure.contains(&format!("replay-{index}.mkv")));
            assert!(failure.contains("cancelled because the application is closing"));
        }
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn replay_request_error_does_not_discard_later_accepted_requests() {
        let rx = queued_replays(2);
        let mut attempts = 0;
        let failures = service_replay_commands(Some(&rx), None, |_| {
            attempts += 1;
            anyhow::ensure!(attempts != 1, "fixture permission failure");
            Ok(())
        });
        assert_eq!(attempts, 2);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("replay-0.mkv"));
        assert!(failures[0].contains("permission failure"));
    }
    #[test]
    fn disconnected_ui_cancels_suppression_without_publishing_a_clip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cancelled.mkv");
        let options = RunOptions {
            audio: vec![AudioTrack::microphone()],
            ..Default::default()
        };
        let mut ring = AvReplayBuffer::new(1_000_000, 10).unwrap();
        ring.push(MediaPacket {
            track: 1,
            pts_ns: 0,
            duration_ns: 1_000_000_000,
            keyframe: true,
            data: vec![0x12, 0, 0x0a, 2, 0x18, 0].into(),
        });
        ring.push(MediaPacket {
            track: 3,
            pts_ns: 0,
            duration_ns: 1_000_000_000,
            keyframe: true,
            data: vec![0; 48_000 * 8].into(),
        });
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        let events = Some(tx);
        let mut job = None;
        let mut stats = PipelineStats::default();
        start_save(&mut job, path.clone(), 10, &ring, &options, &events, true).unwrap();
        finish_save(&mut job, &mut stats, &events, true);
        assert!(job.is_none());
        assert_eq!(stats.replay_saves, 0);
        assert!(!path.exists());
        assert!(!path.with_extension("mkv.partial").exists());
    }
}
