//! Runs entirely on the existing video worker; no GPU object/AVFrame crosses it.
use super::Shared;
use crate::{
    playback_gpu::{
        convert::{ConvertConfig, Nv12Converter, ReadTicket, Skip, TrackedPublish},
        decode::{self, DecodedFrame, Decoder, FrameInfo},
        GpuPlaybackStatus, GpuState,
    },
    Content, Project, SourceStamp, TrackKind, Transform,
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const FUTURE_LIMIT: usize = 3;
const READ_LIMIT: usize = 3;
static GPU_WORKER: AtomicBool = AtomicBool::new(false);
#[derive(Debug)]
pub(super) struct SourceChanged(pub String);
impl std::fmt::Display for SourceChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Source changed or unavailable during playback: {}",
            self.0
        )
    }
}
impl std::error::Error for SourceChanged {}
fn unchanged(path: &std::path::Path, expected: &SourceStamp) -> Result<()> {
    match SourceStamp::read(path) {
        Ok(current) if current == *expected => Ok(()),
        Ok(_) => Err(SourceChanged(path.display().to_string()).into()),
        Err(error) => Err(SourceChanged(format!("{}: {error:#}", path.display())).into()),
    }
}
struct Admission;
impl Admission {
    fn acquire() -> Result<Self> {
        ensure!(
            GPU_WORKER
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "Previous GPU playback worker is still retiring"
        );
        Ok(Self)
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        GPU_WORKER.store(false, Ordering::Release);
    }
}

struct Selection {
    path: PathBuf,
    source_in_ns: u64,
    stream: u32,
    width: u32,
    height: u32,
}
fn select(project: &Project) -> Result<Selection> {
    let mut visual = project.clips.iter().filter(|clip| {
        project
            .tracks
            .iter()
            .any(|t| t.id == clip.track && t.kind == TrackKind::Video)
    });
    let clip = visual
        .next()
        .context("GPU playback requires one video clip")?;
    ensure!(
        visual.next().is_none(),
        "GPU playback composition uses CPU fallback"
    );
    ensure!(
        clip.start == 0
            && clip.end() == project.duration()
            && clip.speed == 1.
            && clip.transform == Transform::default()
            && clip.keyframes.is_empty()
            && clip.fade_in == 0
            && clip.fade_out == 0,
        "GPU playback requires an untransformed continuous clip"
    );
    let track = project
        .tracks
        .iter()
        .find(|t| t.id == clip.track)
        .context("Missing video track")?;
    ensure!(
        !track.muted
            && !project
                .tracks
                .iter()
                .any(|t| t.kind == TrackKind::Video && t.solo && t.id != track.id),
        "Muted video uses CPU fallback"
    );
    let Content::Media { asset, stream } = clip.content else {
        anyhow::bail!("Titles use CPU fallback")
    };
    let asset = project
        .assets
        .iter()
        .find(|a| a.id == asset)
        .context("Missing GPU source")?;
    ensure!(
        (asset.width, asset.height) == (project.width, project.height),
        "Canvas framing uses CPU fallback"
    );
    Ok(Selection {
        path: asset.path.clone(),
        source_in_ns: clip.source_in_ns,
        stream,
        width: asset.width,
        height: asset.height,
    })
}
fn timeline_frame(info: &FrameInfo, source_in_ns: u64, fps: u32) -> Result<Option<u64>> {
    let source_ns = info.seconds * 1_000_000_000.;
    ensure!(source_ns.is_finite(), "Invalid source PTS");
    // Source timestamps can have millisecond precision even for exact 60fps.
    // Nearest frame preserves 0,17,33,50ms as 0,1,2,3 and tolerates a trimmed
    // frame's sub-timebase rounding. Preroll is rejected after this rounding.
    let frame = ((source_ns - source_in_ns as f64) * f64::from(fps) / 1_000_000_000.).round();
    if frame < 0. {
        return Ok(None);
    }
    Ok(Some(frame as u64))
}
fn same_geometry(a: &FrameInfo, b: &FrameInfo) -> bool {
    a.visible_size == b.visible_size
        && a.coded_size == b.coded_size
        && a.crop == b.crop
        && a.sample_aspect_ratio == b.sample_aspect_ratio
}
fn position(shared: &Shared, start: u64, fps: u32, end: u64) -> u64 {
    (start + (shared.elapsed().as_secs_f64() * f64::from(fps)).floor() as u64).min(end)
}
fn cancelled(shared: &Shared, gpu: &GpuState) -> bool {
    shared.cancel.load(Ordering::Acquire) || gpu.disabled.load(Ordering::Acquire)
}
fn wait(shared: &Shared, duration: Duration) {
    let guard = shared.frames.lock().unwrap();
    drop(shared.wake.wait_timeout(guard, duration).unwrap());
}

/// A release notification is only a reason to retry normal zero-timeout keyed
/// mutex acquisition. No GPU/context lock is held here. The epoch was sampled
/// before acquisition, preventing a release between Busy and this wait being
/// lost. A finite timeout preserves cancellation and non-UI consumers.
fn wait_for_consumer(epoch: &std::sync::atomic::AtomicU64, observed: u64) -> bool {
    wait_for_consumer_timeout(epoch, observed, 1);
    epoch.load(Ordering::Acquire) != observed
}
fn wait_for_consumer_timeout(epoch: &std::sync::atomic::AtomicU64, observed: u64, timeout_ms: u32) {
    if epoch.load(Ordering::Acquire) != observed { return; }
    unsafe {
        let _ = windows::Win32::System::Threading::WaitOnAddress(
            epoch.as_ptr().cast(), (&observed as *const u64).cast(),
            std::mem::size_of::<u64>(), Some(timeout_ms));
    }
}
fn wait_paused(shared: &Shared, gpu: &GpuState, pending: bool) {
    let guard = shared.frames.lock().unwrap();
    if !shared.requested.load(Ordering::Acquire) && !cancelled(shared, gpu) {
        drop(
            shared
                .wake
                .wait_timeout(
                    guard,
                    if pending {
                        Duration::from_millis(1)
                    } else {
                        Duration::from_millis(250)
                    },
                )
                .unwrap(),
        );
    }
}
fn retire(
    decoder: &Decoder,
    converter: &mut Nv12Converter,
    pending: &mut VecDeque<(ReadTicket, DecodedFrame)>,
) -> Result<()> {
    while let Some(ticket) = decoder.with_context_lock(|| converter.poll_completed())? {
        ensure!(
            pending
                .front()
                .is_some_and(|(expected, _)| *expected == ticket),
            "GPU completion did not match retained frame"
        );
        pending.pop_front();
    }
    Ok(())
}

/// Admission is held until decoder cleanup really finishes, even after UI Drop.
pub(super) fn run(
    project: &Project,
    start: u64,
    size: (u32, u32),
    shared: &Shared,
    gpu: &GpuState,
) -> Result<()> {
    let _admission = Admission::acquire()?;
    let selected = select(project)?;
    let original = SourceStamp::read(&selected.path)?;
    if let Some(saved) = project
        .assets
        .iter()
        .find(|a| a.path == selected.path)
        .and_then(|a| a.source_stamp.as_ref())
    {
        ensure!(*saved == original, "Source changed before GPU playback");
    }
    ensure!(!cancelled(shared, gpu), "GPU playback cancelled");
    let target_ns = selected
        .source_in_ns
        .saturating_add((u128::from(start) * 1_000_000_000 / u128::from(project.fps)) as u64);
    gpu.stats.lock().unwrap().seek_target_ns = target_ns;
    let mut decoder = Decoder::open_transport_lazy(
        &selected.path,
        &gpu.config.dll_directory,
        gpu.cancel.clone(),
        decode::TransportOptions {
            timeout: Duration::from_secs(5),
            source_in_ns: target_ns,
            stream: selected.stream,
        },
        ((selected.width, selected.height), project.fps),
        || {
            unchanged(&selected.path, &original)?;
            let (device, context, _) = decode::create_device(Some(gpu.config.adapter_luid))?;
            Ok((device, context))
        },
    )?;
    unchanged(&selected.path, &original)?;
    let mut future: VecDeque<(u64, DecodedFrame)> = VecDeque::with_capacity(FUTURE_LIMIT);
    let mut pending = VecDeque::with_capacity(READ_LIMIT);
    let mut converter: Option<Nv12Converter> = None;
    let mut geometry: Option<FrameInfo> = None;
    let mut eof = false;
    let mut last_publish = None;
    let mut source_check = Instant::now();
    let mut preroll = 0usize;
    let mut last_decoded = None;
    let result = (|| -> Result<()> {
        loop {
            if cancelled(shared, gpu) {
                break;
            }
            if let Some(converter) = &mut converter {
                retire(&decoder, converter, &mut pending)?;
            }
            if source_check.elapsed() >= Duration::from_secs(1) {
                unchanged(&selected.path, &original)?;
                source_check = Instant::now();
            }
            // Pause keeps all resources; only initial video readiness needs decoding.
            if shared.video_ready.load(Ordering::Acquire)
                && !shared.requested.load(Ordering::Acquire)
            {
                wait_paused(shared, gpu, !pending.is_empty());
                continue;
            }
            while !eof && future.len() < FUTURE_LIMIT && !cancelled(shared, gpu) {
                let Some(frame) = decoder.next()? else {
                    eof = true;
                    break;
                };
                frame.info.require_bt709()?;
                ensure!(
                    frame.info.rate.1 > 0
                        && i64::from(frame.info.rate.0)
                            == i64::from(project.fps) * i64::from(frame.info.rate.1),
                    "GPU playback frame-rate conversion uses CPU fallback"
                );
                ensure!(
                    frame.info.visible_size == (selected.width, selected.height),
                    "Source geometry changed"
                );
                if let Some(first) = &geometry {
                    ensure!(
                        same_geometry(first, &frame.info),
                        "Source GPU geometry changed"
                    );
                } else {
                    geometry = Some(frame.info.clone());
                }
                let mut timing = frame.info.clone();
                timing.seconds -= decoder.source_start_seconds();
                let index = timeline_frame(&timing, selected.source_in_ns, project.fps)?;
                if index.is_none_or(|index| index < start) {
                    preroll += 1;
                    gpu.stats.lock().unwrap().preroll_frames = preroll as u64;
                    ensure!(preroll <= 4096, "GPU keyframe preroll exceeded 4096 frames");
                    continue;
                }
                let index = index.expect("non-preroll frame checked");
                ensure!(
                    last_decoded.is_none_or(|previous| index > previous),
                    "GPU source requires frame-rate conversion for duplicate timeline timestamps"
                );
                last_decoded = Some(index);
                if index >= project.duration() {
                    eof = true;
                    break;
                }
                if converter.is_none() {
                    let output_size = (
                        size.0.min(frame.info.visible_size.0),
                        size.1.min(frame.info.visible_size.1),
                    );
                    let made = decoder.with_context_lock(|| {
                        Nv12Converter::new(
                            &decoder.device,
                            &decoder.context,
                            ConvertConfig {
                                coded_size: frame.info.coded_size,
                                source_rect: Some(frame.info.source_rect()),
                                output_size,
                            },
                        )
                    })?;
                    converter = Some(made);
                    gpu.stats.lock().unwrap().pool_size = u64::from(frame.info.array_size);
                }
                future.push_back((index, frame));
                shared.decoded.fetch_add(1, Ordering::Relaxed);
                let mut stats = gpu.stats.lock().unwrap();
                stats.decoded += 1;
                stats.max_future_frames = stats.max_future_frames.max(future.len() as u64);
            }
            if cancelled(shared, gpu) {
                break;
            }
            let now = position(shared, start, project.fps, project.duration());
            // Keep only the newest due frame. Never publish a future timestamp.
            while future.len() > 1 && future.get(1).is_some_and(|(index, _)| *index <= now) {
                future.pop_front();
                shared.dropped.fetch_add(1, Ordering::Relaxed);
                gpu.stats.lock().unwrap().dropped += 1;
            }
            if future.front().is_some_and(|(index, _)| *index <= now) {
                let converter = converter.as_mut().context("Missing GPU converter")?;
                let (index, frame) = future.front().expect("due frame checked");
                let id = index
                    .checked_add(1)
                    .context("GPU frame identity overflow")?;
                let release_epoch = converter.consumer_release_epoch().load(Ordering::Acquire);
                let publication = decoder.with_context_lock(|| {
                    converter.publish_tracked_frame(
                        &frame.texture,
                        frame.info.array_slice,
                        id,
                        || cancelled(shared, gpu),
                    )
                })?;
                match publication {
                    TrackedPublish::Submitted {
                        ticket,
                        publication,
                    } => {
                        let (index, frame) = future.pop_front().expect("submitted due frame");
                        pending.push_back((ticket, frame));
                        publication?;
                        ensure!(index <= now, "Future GPU frame was published");
                        last_publish = Some(index);
                        {
                            let mut output = gpu.output.lock().unwrap();
                            if output.is_none() {
                                *output = Some(converter.output());
                            }
                        }
                        {
                            let mut stats = gpu.stats.lock().unwrap();
                            if stats.first_published_id == 0 {
                                stats.first_published_id = id;
                                stats.first_source_ns = Some(
                                    ((pending
                                        .back()
                                        .expect("submitted input retained")
                                        .1
                                        .info
                                        .seconds
                                        - decoder.source_start_seconds())
                                        * 1_000_000_000.)
                                        .max(0.)
                                        .round() as u64,
                                );
                            }
                            stats.published += 1;
                            stats.last_published_id = id;
                            stats.last_publish_position = now;
                            stats.max_inflight_reads =
                                stats.max_inflight_reads.max(pending.len() as u64);
                        }
                        gpu.publication_epoch.fetch_add(1, Ordering::Release);
                        gpu.status(GpuPlaybackStatus::Active);
                        if !shared.video_ready.swap(true, Ordering::AcqRel)
                            && shared.requested.load(Ordering::Acquire)
                            && !shared.audio_clock.load(Ordering::Acquire)
                        {
                            shared.clock.lock().unwrap().play();
                        }
                    }
                    TrackedPublish::Skipped(reason @ (Skip::Busy | Skip::QueueFull)) => {
                        let waiting = Instant::now();
                        let notified = if matches!(reason, Skip::Busy) {
                            if !cancelled(shared, gpu) {
                                wait_for_consumer(converter.consumer_release_epoch(), release_epoch)
                            } else { false }
                        } else {
                            wait(shared, Duration::from_millis(1));
                            false
                        };
                        let elapsed = waiting.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
                        let mut stats = gpu.stats.lock().unwrap();
                        if matches!(reason, Skip::Busy) {
                            stats.busy_skips += 1;
                            stats.busy_release_notifications += u64::from(notified);
                            stats.busy_wait_ns = stats.busy_wait_ns.saturating_add(elapsed);
                        } else {
                            stats.queue_full_skips += 1;
                            stats.queue_full_wait_ns = stats.queue_full_wait_ns.saturating_add(elapsed);
                        }
                    }
                    TrackedPublish::Skipped(Skip::Cancelled) => break,
                    TrackedPublish::Skipped(Skip::Closed) => anyhow::bail!("GPU converter closed"),
                }
            } else if eof && future.is_empty() {
                ensure!(last_publish.is_some(), "No clock-due GPU frame was decoded");
                break;
            } else {
                // Audio clock is authoritative. Its output loop also wakes this CV.
                let until = future
                    .front()
                    .map_or(Duration::from_millis(1), |(frame, _)| {
                        Duration::from_secs_f64(
                            (frame.saturating_sub(now) as f64 / f64::from(project.fps)).min(0.01),
                        )
                    });
                let waiting = Instant::now();
                wait(shared, until.max(Duration::from_millis(1)));
                let elapsed = waiting.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
                let mut stats = gpu.stats.lock().unwrap();
                stats.clock_wait_ns = stats.clock_wait_ns.saturating_add(elapsed);
            }
        }
        Ok(())
    })();
    // Cleanup is worker-only. Decoder must close before incomplete frames on fault.
    let cleanup = (|| -> Result<()> {
        if let Some(converter) = &mut converter {
            converter.begin_shutdown();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !pending.is_empty() {
                retire(&decoder, converter, &mut pending)?;
                ensure!(
                    Instant::now() < deadline,
                    "GPU playback retirement deadline"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    })();
    drop(decoder);
    drop(future);
    drop(pending);
    drop(converter);
    // Revalidate even on a GPU error before the caller considers CPU fallback.
    unchanged(&selected.path, &original)?;
    match (result, cleanup) {
        (Err(error), _) | (_, Err(error)) => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod regression_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consumer_release_wait_handles_prewake_real_signal_and_separate_outputs() {
        use std::sync::{Arc, atomic::AtomicU64};
        let old = Arc::new(AtomicU64::new(7));
        let current = Arc::new(AtomicU64::new(0));
        // A release before the wait must return without relying on a future wake.
        wait_for_consumer(&old, 6);
        let receiver = current.clone();
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let waiter = std::thread::spawn(move || {
            ready.send(()).unwrap();
            let waiting = Instant::now();
            // WaitOnAddress permits spurious wakeups. Retry against one fixed
            // total deadline, without turning this into a timeout-polling test.
            while receiver.load(Ordering::Acquire) == 0 && waiting.elapsed() < Duration::from_secs(1) {
                let remaining = Duration::from_secs(1).saturating_sub(waiting.elapsed());
                wait_for_consumer_timeout(&receiver, 0, remaining.as_millis().max(1) as u32);
            }
            (receiver.load(Ordering::Acquire), waiting.elapsed())
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        // Give the waiter an opportunity to enter the OS wait. Unlike the
        // production 1ms timeout, this test cannot pass by repeated polling.
        std::thread::sleep(Duration::from_millis(20));
        current.fetch_add(1, Ordering::Release);
        unsafe { windows::Win32::System::Threading::WakeByAddressSingle(current.as_ptr().cast()); }
        let (released, elapsed) = waiter.join().unwrap();
        assert_eq!(released, 1);
        assert!(elapsed < Duration::from_millis(750), "consumer notification fell back to its 1-second timeout: {elapsed:?}");
        assert_eq!(old.load(Ordering::Acquire), 7, "a replacement output must not wake/change the old identity");
        // A consumer that does not attach notifications still gets a bounded
        // retry rather than leaving cancellation dependent on a future frame.
        wait_for_consumer(&old, 7);
    }
    #[test]
    fn admission_is_exclusive_until_guard_is_dropped() {
        let a = Admission::acquire().unwrap();
        assert!(Admission::acquire().is_err());
        drop(a);
        drop(Admission::acquire().unwrap());
    }
    #[test]
    fn timestamp_mapping_rejects_preroll_and_never_moves_source_in() {
        let mut info = FrameInfo {
            pts: 0,
            seconds: 1.,
            time_base: (1, 1000),
            rate: (60, 1),
            visible_size: (2, 2),
            coded_size: (2, 2),
            crop: (0, 0, 0, 0),
            array_slice: 0,
            array_size: 18,
            color_range: 1,
            color_matrix: 1,
            color_primaries: 1,
            color_transfer: 1,
            sample_aspect_ratio: (1, 1),
        };
        assert_eq!(timeline_frame(&info, 1_000_000_000, 60).unwrap(), Some(0));
        info.seconds = 0.5;
        assert_eq!(timeline_frame(&info, 1_000_000_000, 60).unwrap(), None);
        info.seconds = 1.5;
        assert_eq!(timeline_frame(&info, 1_000_000_000, 60).unwrap(), Some(30));
        for (index, ms) in [1000, 1017, 1033, 1050, 1067, 1083].into_iter().enumerate() {
            info.seconds = f64::from(ms) / 1000.;
            assert_eq!(
                timeline_frame(&info, 1_000_000_000, 60).unwrap(),
                Some(index as u64)
            );
        }
        info.seconds = 0.033;
        assert_eq!(timeline_frame(&info, 33_333_333, 60).unwrap(), Some(0));
    }
}
