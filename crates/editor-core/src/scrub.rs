//! Paused seeking: one decoder worker, one replaceable request and one result.
//! Audio/device startup is reserved for playback. Cache lifetime is one immutable
//! project revision; source quality and output geometry distinguish cache entries.
use crate::{preview_size, PreviewFrame, PreviewJob, Project};
use anyhow::{ensure, Context, Result};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
pub(crate) mod native;
#[cfg(windows)]
mod proxy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeekQuality { Exact, Interactive, Original }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Request { at: u64, serial: u64, quality: SeekQuality, size: Option<(u32, u32)>, queued_at: Option<Instant> }
fn trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RCR_TRACE_SEEK").is_some_and(|value| !value.is_empty())
        || std::env::var_os("RCR_CLIP_SHOW_EVAL_OUTPUT").is_some())
}
fn memory_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RCR_TRACE_MEMORY").is_some_and(|value| value == "1"))
}
static MEMORY_WORKERS: AtomicU64 = AtomicU64::new(0);
static MEMORY_BGRA: AtomicU64 = AtomicU64::new(0);
static MEMORY_SOFTWARE: AtomicU64 = AtomicU64::new(0);
static MEMORY_GPU: AtomicU64 = AtomicU64::new(0);
/// Opt-in retained seek-cache accounting, not total process/driver memory.
/// Surface bytes use the decoder's conservative aligned image-size accounting;
/// codec DPB, uncached current frames, returned images and GPUI are excluded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrubMemorySnapshot {
    pub active_workers: u64,
    pub cached_bgra_bytes: u64,
    pub cached_software_surface_bytes: u64,
    pub cached_gpu_surface_bytes: u64,
}
pub fn scrub_memory_snapshot() -> ScrubMemorySnapshot {
    ScrubMemorySnapshot {
        active_workers: MEMORY_WORKERS.load(Ordering::Acquire),
        cached_bgra_bytes: MEMORY_BGRA.load(Ordering::Acquire),
        cached_software_surface_bytes: MEMORY_SOFTWARE.load(Ordering::Acquire),
        cached_gpu_surface_bytes: MEMORY_GPU.load(Ordering::Acquire),
    }
}
enum MemoryKind { Bgra, Software, Gpu }
struct MemoryAccount { counter: &'static AtomicU64, bytes: u64, enabled: bool }
impl MemoryAccount {
    fn new(kind: MemoryKind) -> Self {
        Self { counter: match kind { MemoryKind::Bgra => &MEMORY_BGRA,
            MemoryKind::Software => &MEMORY_SOFTWARE, MemoryKind::Gpu => &MEMORY_GPU },
            bytes: 0, enabled: memory_trace_enabled() }
    }
    fn set(&mut self, bytes: usize) {
        if !self.enabled { return; }
        let bytes = bytes as u64;
        if bytes >= self.bytes { self.counter.fetch_add(bytes - self.bytes, Ordering::AcqRel); }
        else { self.counter.fetch_sub(self.bytes - bytes, Ordering::AcqRel); }
        self.bytes = bytes;
    }
}
impl Default for MemoryAccount { fn default() -> Self { Self::new(MemoryKind::Bgra) } }
impl Drop for MemoryAccount { fn drop(&mut self) { self.set(0); } }
struct MemoryWorker(bool);
impl MemoryWorker {
    fn new() -> Self {
        let enabled = memory_trace_enabled();
        if enabled { MEMORY_WORKERS.fetch_add(1, Ordering::AcqRel); }
        Self(enabled)
    }
}
impl Drop for MemoryWorker {
    fn drop(&mut self) { if self.0 { MEMORY_WORKERS.fetch_sub(1, Ordering::AcqRel); } }
}
/// Diagnostic callers supply their own enable/cap policy. Format JSON in memory
/// before touching unbuffered stderr: direct Value Display writes every token
/// separately and can stall Windows capture/paint threads for tens of ms.
pub fn write_seek_trace(label: &str, value: serde_json::Value) {
    let line = format!("{label}: {value}\n");
    let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), line.as_bytes());
}
/// Opt-in stage timestamps carried with actual decoded pixels to paint tracing.
/// Timing stays outside identity metadata and never affects request acceptance.
#[derive(Clone, Copy, Debug)]
pub struct ScrubTrace {
    pub queued_at: Instant,
    pub started_at: Instant,
    pub decoded_at: Instant,
    pub cached_ready_at: Instant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrubMetadata {
    pub request_serial: u64,
    pub requested_frame: u64,
    pub exact: bool,
    /// Decoded from original sources at the requested refinement geometry,
    /// bypassing compressed seek proxies and their cached pixels.
    pub original: bool,
}
pub struct ScrubFrame {
    pub frame: PreviewFrame,
    pub metadata: ScrubMetadata,
    pub trace: Option<ScrubTrace>,
}
#[derive(Default)]
struct State {
    pending: Option<Request>,
    active: Option<(u64, Request)>,
    latest: Option<Request>,
    serial: u64,
    exact_barrier: u64,
    result: Option<Result<ScrubFrame, String>>,
    project: Arc<Project>,
    size: (u32, u32),
    revision: u64,
}
impl State {
    fn enqueue(&mut self, at: u64, quality: SeekQuality) -> u64 {
        self.enqueue_sized(at, quality, None)
    }
    fn enqueue_sized(&mut self, at: u64, quality: SeekQuality, size: Option<(u32, u32)>) -> u64 {
        let refinement = self.latest.is_some_and(|r|
            (quality == SeekQuality::Exact && r.quality == SeekQuality::Interactive)
            || (quality == SeekQuality::Original && (r.quality != quality || r.size != size || r.at != at))
            || (r.quality == SeekQuality::Original && quality != SeekQuality::Original));
        if let Some(request) = self.pending.filter(|r| r.at == at && r.quality == quality && r.size == size) {
            return request.serial;
        }
        if let Some((revision, request)) = self.active {
            if revision == self.revision && request.at == at && request.quality == quality && request.size == size
                && request.serial >= self.exact_barrier
                && !refinement {
                self.pending = None;
                self.latest = Some(request);
                return request.serial;
            }
        }
        self.serial = self.serial.saturating_add(1);
        let request = Request { at, serial: self.serial, quality, size,
            queued_at: trace_enabled().then(Instant::now) };
        if refinement {
            self.exact_barrier = request.serial;
            self.result = None;
        }
        self.pending = Some(request);
        self.latest = Some(request);
        request.serial
    }
    fn may_publish(&self, revision: u64, request: Request) -> bool {
        self.revision == revision && request.serial >= self.exact_barrier
    }
    fn may_publish_result(&self, revision: u64, request: Request, success: bool) -> bool {
        self.may_publish(revision, request) && (success || self.latest == Some(request))
    }
    fn cancel_original_refinement(&mut self) -> Option<u64> {
        if self.pending.is_some_and(|request| request.quality == SeekQuality::Original) {
            self.pending = None;
        }
        if self.result.as_ref().is_some_and(|result| result.as_ref().is_ok_and(|frame| frame.metadata.original)) {
            self.result = None;
        }
        if self.latest.is_some_and(|request| request.quality == SeekQuality::Original) {
            self.serial = self.serial.saturating_add(1);
            self.exact_barrier = self.serial;
            self.latest = None;
            self.result = None;
            Some(self.serial)
        } else {
            // A newer fast request already revoked an active refinement. Keep
            // its serial, pending request and progressive publication intact.
            None
        }
    }
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    cancel: Arc<AtomicBool>,
    native: AtomicBool,
    hardware: AtomicBool,
    surface_bytes: AtomicU64,
    surface_hits: AtomicU64,
    desired_serial: Arc<AtomicU64>,
    proxy_ready: AtomicBool,
    proxy_preparing: AtomicBool,
    notify: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

pub struct ScrubSession {
    shared: Arc<Shared>,
    worker: Option<thread::JoinHandle<()>>,
    interactive_landing: bool,
    prepare_proxy: bool,
}

impl ScrubSession {
    pub fn new(project: Project, size: (u32, u32)) -> Result<Self> {
        Self::new_inner(project, size, true)
    }
    /// Viewing must not start a full-source transcode before its first seek.
    /// Unsupported native requests retain the ordinary bounded frame fallback.
    pub fn new_for_viewer(project: Project, size: (u32, u32)) -> Result<Self> {
        Self::new_inner(project, size, false)
    }
    fn new_inner(project: Project, size: (u32, u32), prepare_proxy: bool) -> Result<Self> {
        project.validate()?;
        ensure!(project.duration() > 0, "Cannot seek an empty timeline");
        let size = preview_size(&project, size);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                project: Arc::new(project),
                size,
                ..Default::default()
            }),
            ..Default::default()
        });
        let state = shared.clone();
        let worker = thread::Builder::new()
            .name("rcr-scrub".into())
            .spawn(move || {
                // Declared first, dropped last: zero means the worker's local
                // frame cache, proxy and native readers have actually retired.
                let _memory_worker = MemoryWorker::new();
                if state.cancel.load(Ordering::Acquire) {
                    return;
                }
                let mut cache = FrameCache::default();
                let mut cache_revision = 0;
                let mut warm = false;
                let mut requested_at = Instant::now();
                #[cfg(windows)]
                let mut native = crate::ffmpeg_runtime::retry_busy(
                    || state.cancel.load(Ordering::Acquire),
                    Duration::from_millis(250),
                    || native::Decoder::new(&state.cancel),
                )
                .ok();
                #[cfg(windows)]
                let initial_project = state.state.lock().unwrap().project.clone();
                #[cfg(windows)]
                if let Some(decoder) = &mut native {
                    decoder.refresh_sources(&initial_project);
                }
                #[cfg(windows)]
                let mut proxy = native.as_ref().filter(|_| prepare_proxy).and_then(|decoder| {
                    proxy::Proxy::start(&initial_project, decoder, &state.cancel).ok()
                });
                #[cfg(windows)]
                let mut proxy_revision = 0;
                #[cfg(windows)]
                state.proxy_preparing.store(
                    proxy.as_ref().is_some_and(|p| p.preparing()),
                    Ordering::Release,
                );
                loop {
                    #[cfg(windows)]
                    poll_proxy(&mut proxy, &mut native, &mut warm, &state);
                    let mut slot = state.state.lock().unwrap();
                    while slot.pending.is_none() && !state.cancel.load(Ordering::Acquire) {
                        #[cfg(windows)]
                        {
                            // Process reaping / decoder teardown must not hold
                            // the UI request mutex, even while idle.
                            drop(slot);
                            poll_proxy(&mut proxy, &mut native, &mut warm, &state);
                            slot = state.state.lock().unwrap();
                            if slot.pending.is_some() || state.cancel.load(Ordering::Acquire) {
                                break;
                            }
                        }
                        if slot.latest.is_none() {
                            warm = false;
                        }
                        if warm {
                            let quiet =
                                Duration::from_millis(35).saturating_sub(requested_at.elapsed());
                            if quiet.is_zero() {
                                drop(slot);
                                #[cfg(windows)]
                                {
                                    warm =
                                        native.as_mut().is_some_and(|d| d.warm_step(&state.cancel));
                                }
                                #[cfg(not(windows))]
                                {
                                    warm = false;
                                }
                                #[cfg(windows)]
                                if let Some(decoder) = &native {
                                    state.surface_bytes.store(
                                        decoder.cached_surface_bytes() as u64,
                                        Ordering::Relaxed,
                                    );
                                }
                                slot = state.state.lock().unwrap();
                            } else {
                                slot = state.wake.wait_timeout(slot, quiet).unwrap().0;
                            }
                        } else {
                            #[cfg(windows)]
                            let preparing = proxy.as_ref().is_some_and(|p| p.preparing());
                            #[cfg(not(windows))]
                            let preparing = false;
                            slot = if preparing {
                                state
                                    .wake
                                    .wait_timeout(slot, Duration::from_millis(100))
                                    .unwrap()
                                    .0
                            } else {
                                state.wake.wait(slot).unwrap()
                            };
                        }
                    }
                    if state.cancel.load(Ordering::Acquire) {
                        break;
                    }
                    let request = slot.pending.take().unwrap();
                    let started_at = request.queued_at.map(|_| Instant::now());
                    let at = request.at;
                    requested_at = Instant::now();
                    let project = slot.project.clone();
                    let size = request.size.unwrap_or(slot.size);
                    let original = request.quality == SeekQuality::Original;
                    let revision = slot.revision;
                    slot.active = Some((revision, request));
                    drop(slot);
                    #[cfg(windows)]
                    if revision != proxy_revision {
                        let source_changed = native
                            .as_mut()
                            .is_some_and(|decoder| decoder.refresh_sources(&project));
                        if source_changed
                            || proxy.as_ref().is_none_or(|p| !p.matches_revision(&project))
                        {
                            // Reap any old preparation outside the input mutex before
                            // trying a replacement. A cross-process encoder lease also
                            // serializes rapid close/reopen cycles across worker threads.
                            drop(proxy.take());
                            state.proxy_ready.store(false, Ordering::Release);
                            proxy = native.as_ref().filter(|_| prepare_proxy).and_then(|decoder| {
                                proxy::Proxy::start(&project, decoder, &state.cancel).ok()
                            });
                            state.proxy_preparing.store(
                                proxy.as_ref().is_some_and(|p| p.preparing()),
                                Ordering::Release,
                            );
                        }
                        proxy_revision = revision;
                    }
                    if cache_revision != revision {
                        cache = FrameCache::default();
                        cache_revision = revision;
                    }
                    let cached = cache.get(at, size, original);
                    let cache_miss = cached.is_none();
                    let frame = if let Some(frame) = cached {
                        Ok(frame)
                    } else {
                        #[cfg(windows)]
                        let proxy_frame = proxy.as_mut().filter(|_| !original).map_or(Ok(None), |proxy| {
                            proxy.frame(&project, at, size, &state.cancel)
                        });
                        #[cfg(windows)]
                        let used_proxy = proxy_frame.as_ref().is_ok_and(|frame| frame.is_some());
                        #[cfg(windows)]
                        let decoded = seek_or_next(proxy_frame, || {
                            native.as_mut().map_or(Ok(None), |decoder| {
                                if original {
                                    decoder.frame_original(&project, at, size, &state.cancel,
                                        native::RequestGeneration { desired: state.desired_serial.clone(), serial: request.serial }).map(Some)
                                } else if request.quality == SeekQuality::Interactive {
                                    decoder.frame_interactive(&project, at, size, &state.cancel).map(Some)
                                } else {
                                    decoder.frame(&project, at, size, &state.cancel).map(Some)
                                }
                            })
                        });
                        #[cfg(not(windows))]
                        let decoded: Result<Option<PreviewFrame>> = Ok(None);
                        let native_frame = decoded.as_ref().is_ok_and(|frame| frame.is_some());
                        state.native.store(native_frame, Ordering::Release);
                        #[cfg(windows)]
                        state.hardware.store(
                            native_frame
                                && !used_proxy
                                && native.as_ref().is_some_and(|d| d.hardware()),
                            Ordering::Release,
                        );
                        #[cfg(windows)]
                        if let Some(decoder) = &native {
                            state.surface_bytes.store(
                                (decoder.cached_surface_bytes()
                                    + proxy.as_ref().map_or(0, |p| p.cached_surface_bytes()))
                                    as u64,
                                Ordering::Relaxed,
                            );
                            state.surface_hits.store(
                                decoder.surface_hits()
                                    + proxy.as_ref().map_or(0, |p| p.surface_hits()),
                                Ordering::Relaxed,
                            );
                        }
                        seek_or_next(decoded, || {
                            fallback_scoped(&project, at, size, &state.cancel,
                                original.then_some((&*state.desired_serial, request.serial))).map(Some)
                        })
                        .and_then(|frame| frame.context("No seek frame decoded"))
                    };
                    let decoded_at = started_at.map(|_| Instant::now());
                    if cache_miss {
                        if let Ok(frame) = &frame {
                            cache.insert(frame.clone(), original);
                        }
                    }
                    let trace = request.queued_at.zip(started_at).zip(decoded_at)
                        .map(|((queued_at, started_at), decoded_at)| ScrubTrace {
                            queued_at, started_at, decoded_at, cached_ready_at: Instant::now(),
                        });
                    // Original refinement already has a BGRA cache entry. At
                    // full source resolution, forward warming can evict its
                    // target/backward neighbors and compete with resumed play.
                    warm = !original && state.hardware.load(Ordering::Acquire);
                    if !state.cancel.load(Ordering::Acquire) {
                        let mut slot = state.state.lock().unwrap();
                        slot.active = None;
                        if slot.may_publish_result(revision, request, frame.is_ok()) {
                            slot.result = Some(frame.map(|frame| {
                                let exact = frame.frame == at;
                                ScrubFrame { frame, trace, metadata: ScrubMetadata {
                                    request_serial: request.serial, requested_frame: at, exact, original,
                                } }
                            }).map_err(|e| format!("{e:#}")));
                            drop(slot);
                            let notify = state.notify.lock().unwrap().clone();
                            if let Some(notify) = notify {
                                notify();
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
            interactive_landing: std::env::var_os("RCR_SCRUB_INTERACTIVE_LANDING").is_some_and(|value| value == "1"),
            prepare_proxy,
        })
    }
    pub fn proxy_preparation_enabled(&self) -> bool { self.prepare_proxy }
    pub fn request(&self, frame: u64) {
        self.request_exact(frame);
    }
    pub fn request_exact(&self, frame: u64) -> u64 {
        self.request_quality(frame, SeekQuality::Exact)
    }
    /// Refine a settled frame from original sources, independently of the fast
    /// preview size. Proxy/cache quality is never sufficient for this request.
    pub fn request_original(&self, frame: u64, size: (u32, u32)) -> u64 {
        let mut slot = self.shared.state.lock().unwrap();
        let frame = frame.min(slot.project.duration().saturating_sub(1));
        let size = preview_size(&slot.project, size);
        let serial = slot.enqueue_sized(frame, SeekQuality::Original, Some(size));
        self.shared.desired_serial.store(serial, Ordering::Release);
        self.shared.wake.notify_one();
        serial
    }
    /// Release decoder work for playback without stopping the seek session or
    /// proxy preparation. Any newer fast seek remains pending and publishable.
    pub fn cancel_original_refinement(&self) {
        let mut slot = self.shared.state.lock().unwrap();
        if let Some(serial) = slot.cancel_original_refinement() {
            self.shared.desired_serial.store(serial, Ordering::Release);
            self.shared.wake.notify_one();
        }
    }
    /// Opt-in landing previews retain their actual decoded frame identity. A
    /// release must use request_exact, even when its integer target is unchanged.
    pub fn request_interactive(&self, frame: u64) -> u64 {
        self.request_quality(frame, if self.interactive_landing { SeekQuality::Interactive } else { SeekQuality::Exact })
    }
    fn request_quality(&self, frame: u64, quality: SeekQuality) -> u64 {
        let mut slot = self.shared.state.lock().unwrap();
        let frame = frame.min(slot.project.duration().saturating_sub(1));
        let serial = slot.enqueue(frame, quality);
        self.shared.desired_serial.store(serial, Ordering::Release);
        self.shared.wake.notify_one();
        serial
    }
    pub fn latest_request_serial(&self) -> Option<u64> {
        self.shared.state.lock().unwrap().latest.map(|request| request.serial)
    }
    /// Wake an event-driven UI as soon as a frame is ready, without polling.
    pub fn set_waker(&self, notify: impl Fn() + Send + Sync + 'static) {
        *self.shared.notify.lock().unwrap() = Some(Arc::new(notify));
    }
    /// Replace an edited timeline without starting another decoder worker. The
    /// native reader can retain the same source file while frame mappings change.
    pub fn replace_project(&self, project: Project, size: (u32, u32)) -> Result<()> {
        project.validate()?;
        ensure!(project.duration() > 0, "Cannot seek an empty timeline");
        let size = preview_size(&project, size);
        let mut slot = self.shared.state.lock().unwrap();
        slot.project = Arc::new(project);
        slot.size = size;
        slot.revision = slot.revision.wrapping_add(1);
        slot.pending = None;
        slot.latest = None;
        slot.result = None;
        self.shared.desired_serial.store(0, Ordering::Release);
        Ok(())
    }
    /// Intermediate completed frames remain visible during sustained input;
    /// replacing the pending request never starves the decoder or builds a queue.
    pub fn frame(&self) -> Option<Result<PreviewFrame, String>> {
        self.frame_with_metadata().map(|result| result.map(|frame| frame.frame))
    }
    pub fn frame_with_metadata(&self) -> Option<Result<ScrubFrame, String>> {
        self.shared.state.lock().unwrap().result.take()
    }
    pub fn hardware_decoder(&self) -> bool {
        self.shared.hardware.load(Ordering::Acquire)
    }
    pub fn native_decoder(&self) -> bool {
        self.shared.native.load(Ordering::Acquire)
    }
    pub fn surface_cache_bytes(&self) -> u64 {
        self.shared.surface_bytes.load(Ordering::Relaxed)
    }
    pub fn surface_cache_hits(&self) -> u64 {
        self.shared.surface_hits.load(Ordering::Relaxed)
    }
    pub fn seek_proxy_ready(&self) -> bool {
        self.shared.proxy_ready.load(Ordering::Acquire)
    }
    pub fn seek_proxy_preparing(&self) -> bool {
        self.shared.proxy_preparing.load(Ordering::Acquire)
    }
    pub fn shutdown(mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    fn stop(&self) {
        self.shared.cancel.store(true, Ordering::Release);
        self.shared.wake.notify_all();
    }
}

/// Generic eligibility/decode failures may try another backend. A typed stop is
/// terminal only for this request and must retain its type through proxy/native
/// routing so it cannot start fresh work after the operation deadline.
fn seek_or_next<T>(
    attempt: Result<Option<T>>,
    next: impl FnOnce() -> Result<Option<T>>,
) -> Result<Option<T>> {
    match attempt {
        Ok(Some(frame)) => Ok(Some(frame)),
        Err(error) if seek_stopped(&error) => Err(error),
        Ok(None) | Err(_) => next(),
    }
}
fn seek_stopped(error: &anyhow::Error) -> bool {
    #[cfg(windows)]
    {
        native::is_stopped(error)
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}
impl Drop for ScrubSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
fn poll_proxy(
    proxy: &mut Option<proxy::Proxy>,
    native: &mut Option<native::Decoder>,
    warm: &mut bool,
    state: &Shared,
) {
    if let Some(proxy) = proxy {
        proxy.poll(&state.cancel);
        if proxy.ready() && !state.proxy_ready.load(Ordering::Acquire) {
            if let Some(decoder) = native {
                decoder.release();
            }
            *warm = false;
            state.hardware.store(false, Ordering::Release);
            state.surface_bytes.store(0, Ordering::Relaxed);
        }
        state.proxy_ready.store(proxy.ready(), Ordering::Release);
        state
            .proxy_preparing
            .store(proxy.preparing(), Ordering::Release);
    }
}

#[cfg(test)]
fn fallback(
    project: &Project,
    at: u64,
    size: (u32, u32),
    cancel: &AtomicBool,
) -> Result<PreviewFrame> {
    fallback_scoped(project, at, size, cancel, None)
}
fn fallback_scoped(
    project: &Project, at: u64, size: (u32, u32), cancel: &AtomicBool,
    generation: Option<(&AtomicU64, u64)>,
) -> Result<PreviewFrame> {
    ensure!(!cancel.load(Ordering::Acquire), "Seek cancelled");
    check_generation(generation)?;
    let job = PreviewJob::start_fitted(project.clone(), at, false, size)?;
    let result = loop {
        if let Err(error) = check_generation(generation) { break Err(error); }
        if cancel.load(Ordering::Acquire) {
            break Err(anyhow::anyhow!("Seek cancelled"));
        }
        if let Ok(error) = job.errors.try_recv() {
            break Err(anyhow::anyhow!(error));
        }
        match job.frames.recv_timeout(Duration::from_millis(5)) {
            Ok(frame) => break Ok(frame),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err(anyhow::anyhow!("No seek frame decoded"))
            }
            _ => {}
        }
    };
    job.stop();
    // Reap this decoder before another request, including rapid cancellation.
    while !job.done.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(1));
    }
    result
}
fn check_generation(generation: Option<(&AtomicU64, u64)>) -> Result<()> {
    if generation.is_some_and(|(desired, serial)| desired.load(Ordering::Acquire) != serial) {
        #[cfg(windows)]
        return Err(native::SeekStopped(native::SeekStopReason::Superseded).into());
        #[cfg(not(windows))]
        anyhow::bail!("Seek superseded");
    }
    Ok(())
}

#[derive(Default)]
struct FrameCache {
    frames: VecDeque<CachedFrame>,
    bytes: usize,
    // Fields drop in order: cached BGRA vectors disappear before accounting.
    accounting: MemoryAccount,
}
struct CachedFrame { frame: PreviewFrame, original: bool }
impl FrameCache {
    const BUDGET: usize = 48 * 1024 * 1024;
    fn get(&mut self, at: u64, size: (u32, u32), original: bool) -> Option<PreviewFrame> {
        let index = self.frames.iter().position(|f| f.frame.frame == at && f.original == original
            && (f.frame.width, f.frame.height) == size)?;
        let frame = self.frames.remove(index)?;
        let result = frame.frame.clone();
        self.frames.push_back(frame);
        Some(result)
    }
    fn insert(&mut self, frame: PreviewFrame, original: bool) {
        if self.frames.iter().any(|f| f.frame.frame == frame.frame && f.original == original
            && (f.frame.width, f.frame.height) == (frame.width, frame.height)) || frame.bgra.len() > Self::BUDGET {
            return;
        }
        while self.bytes + frame.bgra.len() > Self::BUDGET || self.frames.len() >= 24 {
            if let Some(old) = self.frames.pop_front() {
                self.bytes -= old.frame.bgra.len();
            } else {
                break;
            }
        }
        self.bytes += frame.bgra.len();
        self.frames.push_back(CachedFrame { frame, original });
        self.accounting.set(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_memory_accounting_tracks_resize_eviction_and_final_owner_drop() {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut account = MemoryAccount { counter: &COUNTER, bytes: 0, enabled: true };
        account.set(4096);
        assert_eq!(COUNTER.load(Ordering::Acquire), 4096);
        let mut second = MemoryAccount { counter: &COUNTER, bytes: 0, enabled: true };
        second.set(1024);
        account.set(2048);
        assert_eq!(COUNTER.load(Ordering::Acquire), 3072);
        drop(account);
        assert_eq!(COUNTER.load(Ordering::Acquire), 1024);
        drop(second);
        assert_eq!(COUNTER.load(Ordering::Acquire), 0);
        let mut disabled = MemoryAccount { counter: &COUNTER, bytes: 0, enabled: false };
        disabled.set(8192);
        drop(disabled);
        assert_eq!(COUNTER.load(Ordering::Acquire), 0);
    }
    #[test]
    fn cancel_original_revokes_generation_and_publication_without_cancelling_new_fast_input() {
        let mut state = State::default();
        let original = state.enqueue_sized(20, SeekQuality::Original, Some((1920, 1080)));
        let desired = AtomicU64::new(original);
        let request = state.pending.take().unwrap();
        state.active = Some((0, request));
        let revoked = state.cancel_original_refinement().unwrap();
        desired.store(revoked, Ordering::Release);
        assert!(check_generation(Some((&desired, original))).is_err());
        assert!(!state.may_publish_result(0, request, true));
        assert!(state.pending.is_none() && state.latest.is_none());
        assert!(state.cancel_original_refinement().is_none());

        state.enqueue_sized(30, SeekQuality::Original, Some((1920, 1080)));
        let old_original = state.pending.take().unwrap();
        state.active = Some((0, old_original));
        let fast = state.enqueue(40, SeekQuality::Exact);
        let pending_fast = state.pending.unwrap();
        assert!(state.cancel_original_refinement().is_none());
        assert_eq!(state.pending, Some(pending_fast));
        assert_eq!(state.latest.unwrap().serial, fast);
        assert!(state.may_publish_result(0, pending_fast, true));
        assert!(!state.may_publish_result(0, old_original, true));

        state.active = None;
        state.enqueue_sized(50, SeekQuality::Original, Some((1920, 1080)));
        assert!(state.cancel_original_refinement().is_some());
        assert!(state.pending.is_none(), "Unstarted original work survived playback start");
    }
    #[test]
    fn superseded_generation_never_starts_composition_fallback() {
        let desired = AtomicU64::new(8);
        let error = fallback_scoped(&Project::default(), 0, (320, 180),
            &AtomicBool::new(false), Some((&desired, 7))).err()
            .expect("Superseded refinement started a subprocess");
        #[cfg(windows)]
        assert!(seek_stopped(&error));
        #[cfg(not(windows))]
        assert_eq!(error.to_string(), "Seek superseded");
        assert!(check_generation(Some((&desired, 8))).is_ok());
        assert!(check_generation(None).is_ok());
    }
    #[test]
    fn stale_fast_errors_cannot_clear_a_new_request_but_successes_remain_progressive() {
        let mut state = State::default();
        state.enqueue(1, SeekQuality::Exact);
        let older = state.pending.take().unwrap();
        state.enqueue(2, SeekQuality::Exact);
        assert!(state.may_publish_result(0, older, true));
        assert!(!state.may_publish_result(0, older, false));
        assert!(state.may_publish_result(0, state.pending.unwrap(), false));
    }
    #[test]
    fn original_refinement_upgrades_same_target_and_new_input_revokes_it() {
        let mut state = State::default();
        state.enqueue(60, SeekQuality::Exact);
        let fast = state.pending.take().unwrap();
        state.active = Some((0, fast));
        state.result = Some(Ok(ScrubFrame {
            frame: PreviewFrame { frame: 60, width: 2, height: 2, bgra: vec![1; 16] },
            metadata: ScrubMetadata { request_serial: fast.serial, requested_frame: 60, exact: true, original: false },
            trace: None,
        }));
        let original = state.enqueue_sized(60, SeekQuality::Original, Some((1920, 1080)));
        assert!(original > fast.serial);
        assert!(state.result.is_none(), "Refinement retained a buffered proxy result");
        assert!(!state.may_publish(0, fast), "Late proxy pixels replaced a requested original");
        let refining = state.pending.take().unwrap();
        assert!(state.may_publish(0, refining));
        state.active = Some((0, refining));
        assert_eq!(state.enqueue_sized(60, SeekQuality::Original, Some((1920, 1080))), original);
        let resized = state.enqueue_sized(60, SeekQuality::Original, Some((2560, 1440)));
        assert!(resized > original);
        assert!(!state.may_publish(0, refining), "Old refinement geometry survived a larger request");
        let resized_request = state.pending.take().unwrap();
        state.active = Some((0, resized_request));
        let resumed = state.enqueue(70, SeekQuality::Interactive);
        assert!(resumed > resized);
        assert!(!state.may_publish(0, resized_request), "New pointer input accepted an obsolete expensive refinement");
    }
    #[test]
    fn refinement_cache_separates_proxy_pixels_original_pixels_and_geometry() {
        let mut cache = FrameCache::default();
        let frame = |width, height, byte| PreviewFrame { frame: 10, width, height,
            bgra: vec![byte; width as usize * height as usize * 4] };
        cache.insert(frame(2, 2, 1), false);
        assert!(cache.get(10, (2, 2), true).is_none());
        cache.insert(frame(2, 2, 2), true);
        cache.insert(frame(4, 4, 3), true);
        assert_eq!(cache.get(10, (2, 2), false).unwrap().bgra, vec![1; 16]);
        assert_eq!(cache.get(10, (2, 2), true).unwrap().bgra, vec![2; 16]);
        assert_eq!(cache.get(10, (4, 4), true).unwrap().bgra, vec![3; 64]);
        assert!(cache.get(10, (4, 4), false).is_none());
        assert_eq!(cache.bytes, 96);
    }
    #[test]
    fn interactive_quality_upgrade_preserves_latest_pending_and_rejects_late_results() {
        let mut state = State::default();
        let first = state.enqueue(30, SeekQuality::Interactive);
        let active = state.pending.take().unwrap();
        state.active = Some((state.revision, active));
        assert_eq!(state.enqueue(30, SeekQuality::Interactive), first);
        let next = state.enqueue(60, SeekQuality::Interactive);
        let latest = state.enqueue(90, SeekQuality::Interactive);
        assert!(latest > next);
        assert_eq!(state.pending.unwrap().at, 90);
        assert!(state.may_publish(state.revision, active), "Continuous input must preserve useful intermediate frames");
        let exact = state.enqueue(30, SeekQuality::Exact);
        assert!(exact > latest, "Same-target quality upgrade was incorrectly deduplicated");
        assert_eq!(state.pending.unwrap().quality, SeekQuality::Exact);
        assert!(!state.may_publish(state.revision, active), "Released drag accepted a late landing");
        let exact_request = state.pending.take().unwrap();
        assert!(state.may_publish(state.revision, exact_request));
        assert!(!state.may_publish(state.revision + 1, exact_request));
        let interactive = state.enqueue(120, SeekQuality::Interactive);
        assert!(interactive > exact);
        assert!(state.may_publish(state.revision, state.pending.unwrap()));
        let mut upgrade = State::default();
        upgrade.enqueue(20, SeekQuality::Exact);
        let previous_exact = upgrade.pending.take().unwrap();
        upgrade.active = Some((0, previous_exact));
        upgrade.enqueue(30, SeekQuality::Interactive);
        let released = upgrade.enqueue(20, SeekQuality::Exact);
        assert!(released > previous_exact.serial, "Release revived an older active exact request instead of establishing a barrier");
        assert!(!upgrade.may_publish(0, previous_exact));
    }
    #[test]
    fn exact_only_input_retains_intermediates_and_landing_cache_uses_actual_identity() {
        let mut state = State::default();
        state.enqueue(10, SeekQuality::Exact);
        let first = state.pending.take().unwrap();
        state.enqueue(20, SeekQuality::Exact);
        assert!(state.may_publish(state.revision, first), "Legacy exact dragging regressed into starvation");
        let mut cache = FrameCache::default();
        cache.insert(PreviewFrame { frame: 12, width: 1, height: 1, bgra: vec![1, 2, 3, 255] }, false);
        assert!(cache.get(20, (1, 1), false).is_none(), "Landing pixels were cached under their requested target");
        assert_eq!(cache.get(12, (1, 1), false).unwrap().bgra, [1, 2, 3, 255]);
    }
    #[cfg(windows)]
    #[test]
    fn typed_deadline_stops_proxy_native_and_cpu_route_only_for_current_request() {
        use std::cell::Cell;
        let native_calls = Cell::new(0);
        let subprocess_calls = Cell::new(0);
        for deadline_from_proxy in [true, false] {
            let deadline = || {
                anyhow::Error::new(native::SeekStopped(native::SeekStopReason::Deadline))
                    .context("injected read interruption")
            };
            let proxy = if deadline_from_proxy {
                Err(deadline())
            } else {
                Ok(None)
            };
            let result = seek_or_next(proxy, || {
                native_calls.set(native_calls.get() + 1);
                Err(deadline())
            });
            let result = seek_or_next::<u32>(result, || {
                subprocess_calls.set(subprocess_calls.get() + 1);
                Ok(Some(99))
            });
            let error = result.unwrap_err();
            assert!(matches!(
                error.downcast_ref::<native::SeekStopped>(),
                Some(native::SeekStopped(native::SeekStopReason::Deadline))
            ));
            assert_eq!(subprocess_calls.get(), 0);
        }
        assert_eq!(
            native_calls.get(),
            1,
            "Proxy deadline invoked another native reader"
        );
        // No persistent disabled/error state is kept in the routing layer. A new
        // explicit request can decode normally, with its own operation deadline.
        let next = seek_or_next(Ok(None), || {
            native_calls.set(native_calls.get() + 1);
            Ok(Some(7))
        });
        let next = seek_or_next(next, || {
            subprocess_calls.set(subprocess_calls.get() + 1);
            Ok(Some(99))
        })
        .unwrap();
        assert_eq!(next, Some(7));
        assert_eq!(native_calls.get(), 2);
        assert_eq!(subprocess_calls.get(), 0);
    }
    #[test]
    fn generic_decode_errors_still_use_the_existing_backend_fallback_chain() {
        let result = seek_or_next::<u32>(Err(anyhow::anyhow!("proxy unsupported")), || {
            Err(anyhow::anyhow!("native unsupported"))
        });
        let result = seek_or_next(result, || Ok(Some(42))).unwrap();
        assert_eq!(result, Some(42));
    }
    #[test]
    fn cancelled_seek_rejects_subprocess_fallback_before_project_or_child_setup() {
        let error = fallback(&Project::default(), 0, (320, 180), &AtomicBool::new(true))
            .err()
            .expect("Cancelled fallback unexpectedly started");
        assert_eq!(error.to_string(), "Seek cancelled");
    }
    #[test]
    fn cache_evicts_oldest_and_retains_recently_visited_frames() {
        let mut cache = FrameCache::default();
        for frame in 0..24 {
            cache.insert(PreviewFrame {
                frame,
                width: 2,
                height: 2,
                bgra: vec![0; 16],
            }, false);
        }
        assert!(cache.get(0, (2, 2), false).is_some());
        cache.insert(PreviewFrame {
            frame: 24,
            width: 2,
            height: 2,
            bgra: vec![0; 16],
        }, false);
        assert!(cache.get(1, (2, 2), false).is_none());
        assert!(cache.get(0, (2, 2), false).is_some());
        assert_eq!(cache.bytes, 24 * 16);
    }
}
