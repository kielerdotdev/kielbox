//! Minimal FFmpeg 7 public ABI prefixes, pinned to avformat/avcodec 61,
//! avutil 59 and swscale 8. Never allocate these C structs in Rust.
//! Fields/offsets come from the official n7.1.1 headers; reject other majors
//! before accessing any struct. Only ordinary, untransformed video uses this
//! path; compositions continue through the existing FFmpeg filter graph.
use crate::ffmpeg_runtime::{load_runtime, FfmpegRuntimeConfig, FfmpegRuntimeRequirement, Runtime};
use crate::playback_gpu::decode::ffi;
use crate::{Content, PreviewFrame, Project, TrackKind, Transform};
use anyhow::{ensure, Context, Result};
use std::ffi::CStr;
use std::{
    collections::VecDeque,
    ffi::{c_char, c_int, c_void, CString},
    path::{Path, PathBuf},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime},
};

type Opaque = *mut c_void;
#[repr(C)]
#[derive(Clone, Copy)]
struct Rational {
    num: c_int,
    den: c_int,
}
// AVFormatContext's public fields keep their offsets within a major version:
// n7.0.2 avformat.h:1182 and n7.1.1:1212 explicitly forbid reordering/change
// without a major bump. Use checked generated declarations for existing fields
// only; FFmpeg allocates this object, never Rust/sizeof(Format).
type Format = ffi::AVFormatContext;
#[repr(C)]
struct Stream {
    class: Opaque,
    index: c_int,
    id: c_int,
    parameters: *mut Parameters,
    private: Opaque,
    time_base: Rational,
    start: i64,
    duration: i64,
    frames: i64,
    disposition: c_int,
    discard: c_int,
    aspect: Rational,
    metadata: Opaque,
    average_rate: Rational,
}
#[repr(C)]
struct Parameters {
    kind: c_int,
    codec_id: c_int,
}
#[repr(C)]
struct Packet {
    buffer: Opaque,
    pts: i64,
    dts: i64,
    data: *mut u8,
    size: c_int,
    stream: c_int,
}
#[repr(C)]
struct Frame {
    data: [*const u8; 8],
    lines: [c_int; 8],
    extended: Opaque,
    width: c_int,
    height: c_int,
    samples: c_int,
    format: c_int,
    key: c_int,
    picture: c_int,
    aspect: Rational,
    pts: i64,
}

// AVCodecContext prefix through get_format (libavcodec 61). FF_API_TICKS_PER_FRAME
// is enabled in this ABI. The callback selects CUDA surfaces; preroll frames stay
// on the GPU and only the requested frame is transferred for GPUI's BGRA atlas.
#[repr(C)]
struct CodecContext {
    class: Opaque,
    log_offset: c_int,
    kind: c_int,
    codec: Opaque,
    codec_id: c_int,
    codec_tag: u32,
    private: Opaque,
    internal: Opaque,
    opaque: Opaque,
    bitrate: i64,
    flags: c_int,
    flags2: c_int,
    extra: Opaque,
    extra_size: c_int,
    time_base: Rational,
    packet_time_base: Rational,
    framerate: Rational,
    ticks: c_int,
    delay: c_int,
    width: c_int,
    height: c_int,
    coded_width: c_int,
    coded_height: c_int,
    aspect: Rational,
    pixel_format: c_int,
    software_format: c_int,
    primaries: c_int,
    transfer: c_int,
    colorspace: c_int,
    range: c_int,
    chroma: c_int,
    field: c_int,
    refs: c_int,
    b_frames: c_int,
    slice_flags: c_int,
    draw_band: Opaque,
    get_format: Option<unsafe extern "C" fn(*mut CodecContext, *const c_int) -> c_int>,
}
unsafe extern "C" fn cuda_format(codec: *mut CodecContext, formats: *const c_int) -> c_int {
    let desired = *((*codec).opaque.cast::<c_int>());
    for index in 0..64 {
        let format = *formats.add(index);
        if format == desired {
            return format;
        }
        if format == -1 {
            break;
        }
    }
    -1
}

macro_rules! api {
    ($($library:ident => $name:ident : $ty:ty),* $(,)?) => {
        #[allow(non_snake_case)]
        struct Api { $($name: $ty,)* _runtime: Arc<Runtime> }
        impl Api {
            unsafe fn load(cancel: &AtomicBool) -> Result<Self> {
                let runtime = load_runtime(&FfmpegRuntimeConfig::Automatic,
                    FfmpegRuntimeRequirement::ScrubMajorAbi, false,
                    || cancel.load(Ordering::Acquire))?;
                Ok(Self { $($name: runtime.symbol::<$ty>(match stringify!($library) {
                    "util" => 0, "codec" => 1, "format" => 2, "scale" => 3, _ => unreachable!()
                }, stringify!($name).as_bytes())?,)* _runtime: runtime })
            }
        }
    };
}
api! {
    format => avformat_alloc_context: unsafe extern "C" fn() -> *mut Format,
    format => avformat_open_input: unsafe extern "C" fn(*mut *mut Format, *const c_char, Opaque, *mut Opaque) -> c_int,
    format => avformat_find_stream_info: unsafe extern "C" fn(*mut Format, *mut Opaque) -> c_int,
    format => avformat_close_input: unsafe extern "C" fn(*mut *mut Format),
    format => av_read_frame: unsafe extern "C" fn(*mut Format, *mut Packet) -> c_int,
    format => av_seek_frame: unsafe extern "C" fn(*mut Format, c_int, i64, c_int) -> c_int,
    codec => avcodec_get_name: unsafe extern "C" fn(c_int) -> *const c_char,
    codec => avcodec_find_decoder_by_name: unsafe extern "C" fn(*const c_char) -> Opaque,
    codec => avcodec_find_decoder: unsafe extern "C" fn(c_int) -> Opaque,
    codec => avcodec_get_hw_config: unsafe extern "C" fn(Opaque, c_int) -> *const ffi::AVCodecHWConfig,
    codec => avcodec_alloc_context3: unsafe extern "C" fn(Opaque) -> Opaque,
    codec => avcodec_parameters_to_context: unsafe extern "C" fn(Opaque, *const Parameters) -> c_int,
    codec => avcodec_open2: unsafe extern "C" fn(Opaque, Opaque, *mut Opaque) -> c_int,
    codec => avcodec_free_context: unsafe extern "C" fn(*mut Opaque),
    codec => avcodec_flush_buffers: unsafe extern "C" fn(Opaque),
    codec => avcodec_send_packet: unsafe extern "C" fn(Opaque, *const Packet) -> c_int,
    codec => avcodec_receive_frame: unsafe extern "C" fn(Opaque, *mut Frame) -> c_int,
    codec => av_packet_alloc: unsafe extern "C" fn() -> *mut Packet,
    codec => av_packet_free: unsafe extern "C" fn(*mut *mut Packet),
    codec => av_packet_unref: unsafe extern "C" fn(*mut Packet),
    util => av_frame_alloc: unsafe extern "C" fn() -> *mut Frame,
    util => av_frame_free: unsafe extern "C" fn(*mut *mut Frame),
    util => av_frame_ref: unsafe extern "C" fn(*mut Frame, *const Frame) -> c_int,
    util => av_frame_unref: unsafe extern "C" fn(*mut Frame),
    util => av_hwframe_transfer_data: unsafe extern "C" fn(*mut Frame, *const Frame, c_int) -> c_int,
    util => av_hwdevice_ctx_create: unsafe extern "C" fn(*mut *mut ffi::AVBufferRef, ffi::AVHWDeviceType, *const c_char, *mut ffi::AVDictionary, c_int) -> c_int,
    util => av_dict_set: unsafe extern "C" fn(*mut Opaque, *const c_char, *const c_char, c_int) -> c_int,
    util => av_dict_free: unsafe extern "C" fn(*mut Opaque),
    util => av_image_get_buffer_size: unsafe extern "C" fn(c_int, c_int, c_int, c_int) -> c_int,
    util => av_get_pix_fmt: unsafe extern "C" fn(*const c_char) -> c_int,
    scale => sws_getCoefficients: unsafe extern "C" fn(c_int) -> *const c_int,
    scale => sws_setColorspaceDetails: unsafe extern "C" fn(Opaque, *const c_int, c_int, *const c_int, c_int, c_int, c_int, c_int) -> c_int,
    scale => sws_getCachedContext: unsafe extern "C" fn(Opaque, c_int, c_int, c_int, c_int, c_int, c_int, c_int, Opaque, Opaque, *const f64) -> Opaque,
    scale => sws_scale: unsafe extern "C" fn(Opaque, *const *const u8, *const c_int, c_int, c_int, *const *mut u8, *const c_int) -> c_int,
    scale => sws_freeContext: unsafe extern "C" fn(Opaque),
}

pub(super) struct Decoder {
    api: Arc<Api>,
    reader: Option<Reader>,
    disable_hardware: bool,
    software_threads: u8,
    software_slice_threads: bool,
    proxy_profile: bool,
    source_revision: SourceRevision,
    cancel: Arc<AtomicBool>,
    // Last field: readers/codec cleanup must finish before another native worker
    // is admitted, including a software proxy sharing this worker's permit.
    _admission: Arc<NativeAdmission>,
}

static NATIVE_WORKER: AtomicBool = AtomicBool::new(false);
#[derive(Debug)]
struct NativeAdmissionBusy;
impl std::fmt::Display for NativeAdmissionBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Previous native seek worker is still retiring")
    }
}
impl std::error::Error for NativeAdmissionBusy {}
struct NativeAdmission(&'static AtomicBool);
impl NativeAdmission {
    fn acquire(gate: &'static AtomicBool) -> Result<Arc<Self>> {
        if gate
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(NativeAdmissionBusy.into());
        }
        Ok(Arc::new(Self(gate)))
    }
    fn acquire_cancellable(
        gate: &'static AtomicBool,
        cancel: &AtomicBool,
        budget: Duration,
    ) -> Result<Arc<Self>> {
        let started = Instant::now();
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(SeekStopped(SeekStopReason::Cancelled).into());
            }
            match Self::acquire(gate) {
                Err(error)
                    if error.downcast_ref::<NativeAdmissionBusy>().is_some()
                        && started.elapsed() < budget =>
                {
                    std::thread::sleep(
                        Duration::from_millis(5).min(budget.saturating_sub(started.elapsed())),
                    );
                }
                result => return result,
            }
        }
    }
}
impl Drop for NativeAdmission {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

const OPERATION_TIMEOUT: Duration = Duration::from_secs(3);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SeekStopReason {
    Cancelled,
    Deadline,
    Superseded,
}
#[derive(Debug)]
pub(super) struct SeekStopped(pub SeekStopReason);
impl std::fmt::Display for SeekStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Native seek stopped: {:?}", self.0)
    }
}
impl std::error::Error for SeekStopped {}
pub(super) fn is_stopped(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SeekStopped>().is_some()
}
struct SeekInterrupt {
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    generation: Option<RequestGeneration>,
}
#[derive(Clone)]
pub(super) struct RequestGeneration {
    pub desired: Arc<AtomicU64>,
    pub serial: u64,
}
#[derive(Clone)]
struct ReaderOptions {
    generation: Option<RequestGeneration>,
    software_threads: u8,
    software_slice_threads: bool,
    proxy_profile: bool,
}
impl Default for ReaderOptions {
    fn default() -> Self { Self { generation: None, software_threads: 1, software_slice_threads: false, proxy_profile: false } }
}
fn proxy_threads(value: Option<&std::ffi::OsStr>) -> u8 {
    if value.is_some_and(|value| value == "2") { 2 } else { 1 }
}
fn proxy_slice_threads(threads: u8, value: Option<&std::ffi::OsStr>) -> bool {
    threads == 2 && value.is_some_and(|value| value == "slice")
}
impl RequestGeneration {
    fn superseded(&self) -> bool {
        self.desired.load(Ordering::Acquire) != self.serial
    }
}
enum FrameMode { Exact, Interactive, Original(RequestGeneration) }
impl SeekInterrupt {
    fn new(cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel,
            deadline: Instant::now() + OPERATION_TIMEOUT,
            generation: None,
        }
    }
    fn reason(&self) -> Option<SeekStopReason> {
        if self.cancel.load(Ordering::Acquire) {
            Some(SeekStopReason::Cancelled)
        } else if self.generation.as_ref().is_some_and(RequestGeneration::superseded) {
            Some(SeekStopReason::Superseded)
        } else if Instant::now() >= self.deadline {
            Some(SeekStopReason::Deadline)
        } else {
            None
        }
    }
    fn check(&self) -> Result<()> {
        match self.reason() {
            Some(reason) => Err(SeekStopped(reason).into()),
            None => Ok(()),
        }
    }
    fn begin(&mut self) -> Result<()> {
        self.deadline = Instant::now() + OPERATION_TIMEOUT;
        self.check()
    }
    fn status(&self, status: c_int, message: &str) -> Result<()> {
        self.check()?;
        ensure!(status >= 0, "{message} ({status})");
        Ok(())
    }
}
unsafe extern "C" fn interrupt_seek(opaque: *mut c_void) -> c_int {
    if opaque.is_null() {
        return 1;
    }
    c_int::from((*opaque.cast::<SeekInterrupt>()).reason().is_some())
}
fn retry_allowed(cancel: &AtomicBool, error: &anyhow::Error) -> bool {
    !cancel.load(Ordering::Acquire) && !is_stopped(error)
}
fn retry_if_allowed<T>(
    cancel: &AtomicBool,
    error: anyhow::Error,
    retry: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if retry_allowed(cancel, &error) {
        retry()
    } else {
        Err(error)
    }
}
type SourceStamp = (PathBuf, u32, Option<(u64, SystemTime)>);
#[derive(Default)]
struct SourceRevision(Option<Vec<SourceStamp>>);
/// Allocate visible BGRA and FFmpeg's trailing padding together. Growing a
/// pixel-sized Vec afterwards can reallocate and double its reserved capacity.
fn padded_bgra_buffer(bytes: usize) -> Vec<u8> {
    debug_assert!(bytes.is_multiple_of(4));
    let mut pixels = [0x14, 0x0e, 0x0b, 255].repeat((bytes + 64) / 4);
    pixels[bytes..].fill(0);
    pixels
}
fn fitted_dimensions(source: (u32, u32), aspect: f64, target: (u32, u32)) -> (u32, u32) {
    let factor = (f64::from(target.0) / (f64::from(source.0) * aspect))
        .min(f64::from(target.1) / f64::from(source.1));
    (
        ((f64::from(source.0) * aspect * factor).round() as u32).clamp(1, target.0),
        ((f64::from(source.1) * factor).round() as u32).clamp(1, target.1),
    )
}
impl SourceRevision {
    fn refresh(&mut self, project: &Project) -> bool {
        let mut files = Vec::new();
        for asset in &project.assets {
            for stream in asset
                .streams
                .iter()
                .filter(|stream| stream.kind == TrackKind::Video)
            {
                let path = asset
                    .path
                    .canonicalize()
                    .unwrap_or_else(|_| asset.path.clone());
                let metadata = path
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok().map(|modified| (m.len(), modified)));
                files.push((path, stream.index, metadata));
            }
        }
        let changed = self.0.as_ref().is_some_and(|previous| *previous != files);
        self.0 = Some(files);
        changed
    }
}
impl Decoder {
    pub fn release(&mut self) {
        self.reader = None;
    }
    /// Called only on project revision changes, outside the UI request mutex.
    /// A same-path replacement must invalidate the demuxer and decoded surfaces.
    pub fn refresh_sources(&mut self, project: &Project) -> bool {
        let changed = self.source_revision.refresh(project);
        if changed {
            self.release();
        }
        changed
    }
    pub fn software(&self) -> Self {
        let software_threads = proxy_threads(std::env::var_os("RCR_SEEK_PROXY_THREADS").as_deref());
        Self {
            api: self.api.clone(),
            reader: None,
            disable_hardware: true,
            software_threads,
            software_slice_threads: proxy_slice_threads(software_threads,
                std::env::var_os("RCR_SEEK_PROXY_THREAD_TYPE").as_deref()),
            proxy_profile: true,
            source_revision: SourceRevision::default(),
            cancel: self.cancel.clone(),
            _admission: self._admission.clone(),
        }
    }
    /// Decode at most one neighboring frame between request checks. No transfer
    /// or BGRA allocation occurs, so warming never queues work ahead of input.
    pub fn warm_step(&mut self, cancel: &AtomicBool) -> bool {
        let result = self
            .reader
            .as_mut()
            .map(|reader| unsafe { reader.warm_step(cancel) });
        match result {
            Some(Err(error)) if is_stopped(&error) => {
                self.reader = None;
                false
            }
            Some(Ok(progress)) => progress,
            _ => false,
        }
    }
    pub fn cached_surface_bytes(&self) -> usize {
        self.reader.as_ref().map_or(0, |r| r.cache_bytes)
    }
    pub fn surface_hits(&self) -> u64 {
        self.reader.as_ref().map_or(0, |r| r.cache_hits)
    }

    pub fn hardware(&self) -> bool {
        self.reader.as_ref().is_some_and(|r| r.hardware)
    }
    pub fn new(cancel: &Arc<AtomicBool>) -> Result<Self> {
        SeekInterrupt::new(cancel.clone()).check()?;
        let admission = NativeAdmission::acquire_cancellable(
            &NATIVE_WORKER,
            cancel,
            Duration::from_millis(250),
        )?;
        let api = unsafe { Api::load(cancel)? };
        Ok(Self {
            api: Arc::new(api),
            reader: None,
            disable_hardware: std::env::var_os("RCR_SCRUB_SOFTWARE").is_some(),
            software_threads: 1,
            software_slice_threads: false,
            proxy_profile: false,
            source_revision: SourceRevision::default(),
            cancel: cancel.clone(),
            _admission: admission,
        })
    }
    pub fn frame(
        &mut self,
        project: &Project,
        at: u64,
        size: (u32, u32),
        cancel: &AtomicBool,
    ) -> Result<PreviewFrame> {
        self.frame_mode(project, at, size, cancel, FrameMode::Exact)
    }
    pub fn frame_interactive(
        &mut self, project: &Project, at: u64, size: (u32, u32), cancel: &AtomicBool,
    ) -> Result<PreviewFrame> {
        self.frame_mode(project, at, size, cancel, FrameMode::Interactive)
    }
    pub fn frame_original(
        &mut self, project: &Project, at: u64, size: (u32, u32), cancel: &AtomicBool,
        generation: RequestGeneration,
    ) -> Result<PreviewFrame> {
        let result = self.frame_mode(project, at, size, cancel, FrameMode::Original(generation));
        if result.as_ref().is_err_and(is_stopped) { self.reader = None; }
        if let Some(reader) = &mut self.reader { reader.interrupt.generation = None; }
        result
    }
    fn frame_mode(
        &mut self, project: &Project, at: u64, size: (u32, u32), cancel: &AtomicBool, mode: FrameMode,
    ) -> Result<PreviewFrame> {
        let interactive = matches!(&mode, FrameMode::Interactive);
        let generation = match mode { FrameMode::Original(generation) => Some(generation), _ => None };
        let original = generation.is_some();
        let options = ReaderOptions { generation: generation.clone(), software_threads: self.software_threads,
            software_slice_threads: self.software_slice_threads, proxy_profile: self.proxy_profile };
        let mut interrupt = SeekInterrupt::new(self.cancel.clone());
        interrupt.generation = generation.clone();
        interrupt.check()?;
        let tracks: Vec<_> = project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .collect();
        ensure!(
            tracks.len() == 1 && !tracks[0].muted,
            "Composition requires filter graph"
        );
        let clips: Vec<_> = project
            .clips
            .iter()
            .filter(|c| c.track == tracks[0].id && c.start <= at && at < c.end())
            .collect();
        ensure!(clips.len() == 1, "Composition requires filter graph");
        let clip = clips[0];
        ensure!(
            clip.transform == Transform::default()
                && clip.speed == 1.
                && clip.keyframes.is_empty()
                && clip.fade_in == 0
                && clip.fade_out == 0,
            "Effects require filter graph"
        );
        let Content::Media { asset, stream } = clip.content else {
            anyhow::bail!("Title requires filter graph");
        };
        let asset = project
            .assets
            .iter()
            .find(|a| a.id == asset)
            .context("Missing source")?;
        let resize = crate::preview_size(
            &Project { width: asset.width, height: asset.height, ..project.clone() }, size,
        );
        if !self
            .reader
            .as_ref()
            .is_some_and(|r| r.path == asset.path && r.stream == stream
                && original_reader_compatible(original, r.decoder_resize)
                && resize_covers(r.decoder_resize, resize))
        {
            // Retire an undersized/old-source reader before allocating the
            // full-source refinement decoder; both share this worker's lease.
            self.reader = None;
            self.reader = Some(
                (if original {
                    Reader::open_original(self.api.clone(), &asset.path, stream, self.cancel.clone(), options.clone(), !self.disable_hardware)
                } else {
                    Reader::open_selected(self.api.clone(), &asset.path, stream,
                        if self.disable_hardware { None } else { Some(resize) }, self.cancel.clone(), !self.disable_hardware, options.clone())
                })
                .or_else(|error| {
                    retry_if_allowed(cancel, error, || {
                        Reader::open_selected(
                            self.api.clone(),
                            &asset.path,
                            stream,
                            None,
                            self.cancel.clone(),
                            false,
                            options.clone(),
                        )
                    })
                })?,
            );
        }
        let seconds =
            clip.source_in_ns as f64 / 1e9 + project.seconds(at - clip.start) * clip.speed;
        let landing = interactive.then_some(LandingMap {
            clip_start: clip.start, clip_duration: clip.duration,
            source_in_ns: clip.source_in_ns, fps: project.fps,
        });
        let rate = self.reader.as_ref().unwrap().rate;
        ensure!(
            i64::from(rate.num) == i64::from(project.fps) * i64::from(rate.den) && rate.den > 0,
            "Frame-rate conversion requires filter graph"
        );
        // All raw C pointers below are owned by this worker, never by the UI.
        self.reader.as_mut().unwrap().interrupt.generation = generation.clone();
        let result = unsafe {
            self.reader
                .as_mut()
                .unwrap()
                .frame_mode(seconds, at, size, cancel, landing)
        };
        if result.as_ref().is_err_and(is_stopped) {
            // Interrupted AVIO can retain an error/EOF state. Reopen only for a
            // later explicit request; preserve hardware eligibility/session state.
            self.reader = None;
            return result;
        }
        if result
            .as_ref()
            .is_err_and(|error| retry_allowed(cancel, error))
            && self.hardware()
        {
            if self.reader.as_ref().is_some_and(|reader| reader.backend == SeekBackend::NativeAv1) {
                // A generic experimental decoder failure must try the existing
                // hardware path before disabling hardware for this session.
                self.reader = None;
                let resize = crate::preview_size(
                    &Project { width: asset.width, height: asset.height, ..project.clone() }, size,
                );
                let legacy = Reader::open_backend_scoped(self.api.clone(), &asset.path, stream,
                    if original { None } else { Some(resize) }, self.cancel.clone(), SeekBackend::Cuvid, options.clone());
                let legacy_result = match legacy {
                    Ok(reader) => {
                        self.reader = Some(reader);
                        unsafe { self.reader.as_mut().unwrap().frame_mode(seconds, at, size, cancel, landing) }
                    }
                    Err(error) => Err(error),
                };
                match legacy_result {
                    Ok(frame) => return Ok(frame),
                    Err(error) if !retry_allowed(cancel, &error) => {
                        self.reader = None;
                        return Err(error);
                    }
                    Err(_) => {}
                }
            }
            self.disable_hardware = true;
            self.reader = None;
            self.reader = Some(Reader::open_selected(
                self.api.clone(),
                &asset.path,
                stream,
                None,
                self.cancel.clone(),
                false,
                options,
            )?);
            return unsafe {
                self.reader
                    .as_mut()
                    .unwrap()
                    .frame_mode(seconds, at, size, cancel, landing)
            };
        }
        result
    }
}

struct Reader {
    api: Arc<Api>,
    path: PathBuf,
    stream: u32,
    format: *mut Format,
    codec: Opaque,
    packet: *mut Packet,
    decoded: *mut Frame,
    last: *mut Frame,
    cpu: *mut Frame,
    cuda_format: Box<c_int>,
    scaler: Opaque,
    base: Rational,
    rate: Rational,
    origin: i64,
    position: Option<i64>,
    draining: bool,
    hardware: bool,
    backend: SeekBackend,
    decoder_resize: Option<(u32, u32)>,
    codec_threads: i32,
    codec_thread_type: i32,
    proxy_profile: bool,
    // Per-request diagnostics: returned frames and accepted video packets.
    // These exclude codec-internal prefetch/DPB work and reset before cache lookup.
    decoded_frames: u64,
    video_packets: u64,
    first_decoded_pts: Option<i64>,
    mapped: bool,
    mapped_pool: Option<c_int>,
    mapped_peak_owned: u32,
    mapped_received: u64,
    mapped_advancing_releases: u64,
    mapped_readbacks: u64,
    cache: VecDeque<Surface>,
    cache_bytes: usize,
    surface_accounting: super::MemoryAccount,
    cache_hits: u64,
    warm_until: Option<i64>,
    // Remains alive through avformat_close_input in Drop. The callback only
    // cooperatively interrupts FFmpeg; an OS read already blocked may not return.
    interrupt: Box<SeekInterrupt>,
}
struct Surface {
    frame: *mut Frame,
    pts: i64,
    bytes: usize,
}

#[derive(Clone, Copy)]
struct LandingMap {
    clip_start: u64,
    clip_duration: u64,
    source_in_ns: u64,
    fps: u32,
}
impl LandingMap {
    fn contains_pts(self, pts: i64, origin: i64, base: Rational) -> bool {
        if base.num <= 0 || base.den <= 0 || self.fps == 0 || pts == i64::MIN { return false; }
        let Some(source) = (i128::from(pts) - i128::from(origin))
            .checked_mul(i128::from(base.num)).and_then(|value| value.checked_mul(1_000_000_000)) else { return false; };
        let Some(start) = i128::from(self.source_in_ns).checked_mul(i128::from(base.den)) else { return false; };
        let Some(offset) = source.checked_sub(start).filter(|offset| *offset >= 0)
            .and_then(|offset| offset.checked_mul(i128::from(self.fps))) else { return false; };
        let Some(end) = i128::from(self.clip_duration).checked_mul(i128::from(base.den))
            .and_then(|value| value.checked_mul(1_000_000_000)) else { return false; };
        offset < end
    }
    fn first_pts(self, origin: i64, base: Rational) -> Option<i64> {
        if base.num <= 0 || base.den <= 0 { return None; }
        let numerator = i128::from(self.source_in_ns).checked_mul(i128::from(base.den))?;
        let denominator = i128::from(base.num).checked_mul(1_000_000_000)?;
        let offset = numerator.checked_add(denominator - 1)?.checked_div(denominator)?;
        i64::try_from(i128::from(origin).checked_add(offset)?).ok()
    }
    fn actual_frame(self, pts: i64, origin: i64, base: Rational) -> Option<u64> {
        if base.num <= 0 || base.den <= 0 || self.fps == 0 || pts == i64::MIN { return None; }
        let source = (i128::from(pts) - i128::from(origin))
            .checked_mul(i128::from(base.num))?.checked_mul(1_000_000_000)?;
        let offset = source.checked_sub(i128::from(self.source_in_ns).checked_mul(i128::from(base.den))?)?;
        if offset < 0 { return None; }
        let denominator = i128::from(base.den).checked_mul(1_000_000_000)?;
        let frame = offset.checked_mul(i128::from(self.fps))?
            .checked_add(denominator / 2)?.checked_div(denominator)?;
        let frame = u64::try_from(frame).ok()?;
        if frame >= self.clip_duration { return None; }
        // Only label/cache a landing as a timeline frame when that frame's exact
        // seek timestamp rounds to this actual source PTS. Arbitrary subframe
        // source trims can otherwise round a neighboring image into the cache.
        let time_numerator = i128::from(self.source_in_ns).checked_mul(i128::from(self.fps))?
            .checked_add(i128::from(frame).checked_mul(1_000_000_000)?)?
            .checked_mul(i128::from(base.den))?;
        let time_denominator = i128::from(self.fps).checked_mul(1_000_000_000)?
            .checked_mul(i128::from(base.num))?;
        let mapped_pts = i128::from(origin).checked_add(time_numerator
            .checked_add(time_denominator / 2)?.checked_div(time_denominator)?)?;
        if mapped_pts != i128::from(pts) { return None; }
        self.clip_start.checked_add(frame)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeekBackend {
    Software,
    Cuvid,
    NativeAv1,
}
impl SeekBackend {
    fn name(self) -> &'static str {
        match self {
            Self::Software => "software",
            Self::Cuvid => "cuvid",
            Self::NativeAv1 => "native_av1_cuda",
        }
    }
}

// The same version contract as FfmpegRuntimeRequirement::Gpu702, expressed
// against the headers whose full AVCodecContext we access below. Legacy readers
// continue to use only their major-compatible public prefixes.
fn native_av1_runtime_compatible(versions: [u32; 4]) -> bool {
    let pack = |major: u32, minor: u32, micro: u32| (major << 16) | (minor << 8) | micro;
    versions[..3] == [
        pack(ffi::LIBAVUTIL_VERSION_MAJOR, ffi::LIBAVUTIL_VERSION_MINOR, ffi::LIBAVUTIL_VERSION_MICRO),
        pack(ffi::LIBAVCODEC_VERSION_MAJOR, ffi::LIBAVCODEC_VERSION_MINOR, ffi::LIBAVCODEC_VERSION_MICRO),
        pack(ffi::LIBAVFORMAT_VERSION_MAJOR, ffi::LIBAVFORMAT_VERSION_MINOR, ffi::LIBAVFORMAT_VERSION_MICRO),
    ] && versions[3] >> 16 == 8
}
fn cuda_device_config(config: &ffi::AVCodecHWConfig) -> bool {
    config.pix_fmt == ffi::AVPixelFormat_AV_PIX_FMT_CUDA
        && config.device_type == ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_CUDA
        && config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX != 0
}
const MAPPED_CALLER_FRAMES: c_int = 2;
fn mapped_eligible(requested: bool, backend: SeekBackend, versions: [u32; 4]) -> bool {
    requested && backend == SeekBackend::NativeAv1 && native_av1_runtime_compatible(versions)
}
fn mapped_pool_valid(pool: c_int, format: ffi::AVPixelFormat, software: ffi::AVPixelFormat) -> bool {
    (MAPPED_CALLER_FRAMES + 1..=32).contains(&pool)
        && format == ffi::AVPixelFormat_AV_PIX_FMT_CUDA
        && software == ffi::AVPixelFormat_AV_PIX_FMT_NV12
}
fn mapped_retention_valid(cached_surfaces: usize, owned_outputs: u32) -> bool {
    cached_surfaces == 0 && owned_outputs <= 1
}
fn original_reader_compatible(original: bool, decoder_resize: Option<(u32, u32)>) -> bool {
    !original || decoder_resize.is_none()
}
fn resize_covers(existing: Option<(u32, u32)>, desired: (u32, u32)) -> bool {
    existing.is_none_or(|existing| existing.0 >= desired.0 && existing.1 >= desired.1)
}
fn forward_near(position: Option<i64>, target: i64, base: Rational, rate: Rational, proxy: bool) -> bool {
    let Some(position) = position else { return false; };
    if base.num <= 0 || base.den <= 0 || rate.num <= 0 || rate.den <= 0 { return false; }
    let Some(delta) = i128::from(target).checked_sub(i128::from(position)).filter(|delta| *delta >= 0)
        else { return false; };
    if proxy {
        // An indexed short-GOP proxy can seek more cheaply than decoding a
        // quarter second forward. Allow one frame, rounded UP to demux ticks
        // so 60fps Matroska timestamps alternating16/17ms both stay nearby.
        let Some(numerator) = i128::from(base.den).checked_mul(i128::from(rate.den)) else { return false; };
        let Some(denominator) = i128::from(base.num).checked_mul(i128::from(rate.num)) else { return false; };
        let Some(ticks) = numerator.checked_add(denominator - 1).and_then(|value| value.checked_div(denominator))
            else { return false; };
        delta <= ticks
    } else {
        // Original-source policy retains its existing250ms forward budget.
        delta.checked_mul(i128::from(base.num)).and_then(|value| value.checked_mul(4))
            .is_some_and(|elapsed| elapsed <= i128::from(base.den))
    }
}

fn native_then_legacy<T>(
    cancel: &AtomicBool,
    native: impl FnOnce() -> Result<T>,
    legacy: impl FnOnce() -> Result<T>,
) -> Result<T> {
    native().or_else(|error| retry_if_allowed(cancel, error, legacy))
}

impl Reader {
    #[cfg(test)]
    fn open(
        api: Arc<Api>,
        path: &Path,
        stream: u32,
        resize: Option<(u32, u32)>,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self> {
        Self::open_selected(api, path, stream, resize, cancel, resize.is_some(), ReaderOptions::default())
    }
    fn open_original(api: Arc<Api>, path: &Path, stream: u32, cancel: Arc<AtomicBool>, options: ReaderOptions, hardware: bool) -> Result<Self> {
        Self::open_selected(api, path, stream, None, cancel, hardware, options)
    }
    fn open_selected(
        api: Arc<Api>, path: &Path, stream: u32, resize: Option<(u32, u32)>,
        cancel: Arc<AtomicBool>, hardware: bool, options: ReaderOptions,
    ) -> Result<Self> {
        if hardware && std::env::var_os("RCR_SCRUB_NATIVE_AV1").is_some_and(|value| value == "1") {
            return native_then_legacy(
                &cancel,
                || Self::open_backend_scoped(api.clone(), path, stream, resize, cancel.clone(), SeekBackend::NativeAv1, options.clone()),
                || Self::open_backend_scoped(api.clone(), path, stream, resize, cancel.clone(), SeekBackend::Cuvid, options.clone()),
            );
        }
        let backend = if hardware { SeekBackend::Cuvid } else { SeekBackend::Software };
        Self::open_backend_scoped(api, path, stream, resize, cancel, backend, options)
    }
    fn open_backend_scoped(
        api: Arc<Api>, path: &Path, stream: u32, resize: Option<(u32, u32)>,
        cancel: Arc<AtomicBool>, backend: SeekBackend, reader_options: ReaderOptions,
    ) -> Result<Self> {
        let mut interrupt = Box::new(SeekInterrupt::new(cancel));
        interrupt.generation = reader_options.generation;
        interrupt.check()?;
        let mapped = mapped_eligible(
            std::env::var_os("RCR_SCRUB_NATIVE_MAPPED").is_some_and(|value| value == "1"),
            backend, api._runtime.info.versions,
        );
        let mut reader = Self {
            api,
            path: path.into(),
            stream,
            format: ptr::null_mut(),
            codec: ptr::null_mut(),
            packet: ptr::null_mut(),
            decoded: ptr::null_mut(),
            last: ptr::null_mut(),
            cpu: ptr::null_mut(),
            cuda_format: Box::new(-1),
            scaler: ptr::null_mut(),
            base: Rational { num: 0, den: 0 },
            rate: Rational { num: 0, den: 0 },
            origin: 0,
            position: None,
            draining: false,
            hardware: false,
            backend,
            decoder_resize: resize.filter(|_| backend == SeekBackend::Cuvid),
            codec_threads: 0,
            codec_thread_type: 0,
            proxy_profile: reader_options.proxy_profile,
            decoded_frames: 0,
            video_packets: 0,
            first_decoded_pts: None,
            mapped,
            mapped_pool: None,
            mapped_peak_owned: 0,
            mapped_received: 0,
            mapped_advancing_releases: 0,
            mapped_readbacks: 0,
            cache: VecDeque::new(),
            cache_bytes: 0,
            surface_accounting: super::MemoryAccount::new(if backend == SeekBackend::Software {
                super::MemoryKind::Software } else { super::MemoryKind::Gpu }),
            cache_hits: 0,
            warm_until: None,
            interrupt,
        };
        let path = path.to_str().context("Source path is not UTF-8")?;
        let path = CString::new(path.strip_prefix("\\\\?\\").unwrap_or(path))?;
        // The shared loader checks the supported public major ABI first.
        unsafe {
            let a = &reader.api;
            reader.interrupt.check()?;
            reader.format = (a.avformat_alloc_context)();
            ensure!(!reader.format.is_null(), "Cannot allocate seek demuxer");
            (*reader.format).interrupt_callback = ffi::AVIOInterruptCB {
                callback: Some(interrupt_seek),
                opaque: (&mut *reader.interrupt as *mut SeekInterrupt).cast(),
            };
            (*reader.format).probesize = 8 * 1024 * 1024;
            (*reader.format).max_analyze_duration = 2_000_000;
            (*reader.format).max_streams = 128;
            (*reader.format).max_probe_packets = 256;
            let opened = (a.avformat_open_input)(
                &mut reader.format,
                path.as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            reader.interrupt.status(opened, "Cannot open source")?;
            let info = (a.avformat_find_stream_info)(reader.format, ptr::null_mut());
            reader
                .interrupt
                .status(info, "Cannot read source streams")?;
            ensure!(
                stream < (*reader.format).nb_streams && !(*reader.format).streams.is_null(),
                "Invalid video stream"
            );
            let s = (*(*reader.format).streams.add(stream as usize)).cast::<Stream>();
            ensure!(
                !s.is_null() && !(*s).parameters.is_null(),
                "Missing stream parameters"
            );
            ensure!(
                (*(*s).parameters).kind == 0,
                "Requested stream is not video"
            );
            reader.base = (*s).time_base;
            reader.rate = (*s).average_rate;
            reader.origin = if (*s).start == i64::MIN {
                0
            } else {
                (*s).start
            };
            ensure!(
                reader.base.num > 0 && reader.base.den > 0,
                "Invalid video timebase"
            );
            let codec_id = (*(*s).parameters).codec_id;
            let decoder = if backend == SeekBackend::NativeAv1 {
                ensure!(native_av1_runtime_compatible(a._runtime.info.versions),
                    "Native AV1 seek requires the verified FFmpeg 7.0.2 runtime");
                ensure!(codec_id == ffi::AVCodecID_AV_CODEC_ID_AV1 as c_int,
                    "Native AV1 seek requires an AV1 source");
                let decoder = (a.avcodec_find_decoder_by_name)(c"av1".as_ptr());
                ensure!(!decoder.is_null(), "Native AV1 decoder unavailable");
                let mut supported = false;
                for index in 0..64 {
                    reader.interrupt.check()?;
                    let config = (a.avcodec_get_hw_config)(decoder, index);
                    if config.is_null() { break; }
                    if cuda_device_config(&*config) { supported = true; break; }
                }
                ensure!(supported, "Native AV1 decoder does not support a CUDA hardware device");
                reader.hardware = true;
                decoder
            } else if backend == SeekBackend::Cuvid {
                let name = CStr::from_ptr((a.avcodec_get_name)(codec_id)).to_str()?;
                let name = CString::new(format!("{name}_cuvid"))?;
                let decoder = (a.avcodec_find_decoder_by_name)(name.as_ptr());
                ensure!(!decoder.is_null(), "NVDEC decoder unavailable");
                reader.hardware = true;
                decoder
            } else {
                (a.avcodec_find_decoder)(codec_id)
            };
            ensure!(!decoder.is_null(), "Video codec unavailable");
            reader.codec = (a.avcodec_alloc_context3)(decoder);
            ensure!(!reader.codec.is_null(), "Cannot allocate video decoder");
            let configured = (a.avcodec_parameters_to_context)(reader.codec, (*s).parameters);
            reader.interrupt.status(configured, "Cannot configure video decoder")?;
            let context = reader.codec.cast::<CodecContext>();
            (*context).packet_time_base = reader.base;
            if reader.hardware {
                *reader.cuda_format = (a.av_get_pix_fmt)(c"cuda".as_ptr());
                ensure!(*reader.cuda_format >= 0, "CUDA pixel format unavailable");
                (*context).opaque = (&mut *reader.cuda_format as *mut c_int).cast();
                (*context).get_format = Some(cuda_format);
            }
            if backend == SeekBackend::NativeAv1 {
                // Compatibility is checked before any access to this generated
                // full struct. Transfer the created reference directly to the
                // codec: avcodec_free_context owns it on all subsequent exits.
                ensure!(native_av1_runtime_compatible(a._runtime.info.versions),
                    "Native AV1 codec layout is not compatible");
                let full_context = reader.codec.cast::<ffi::AVCodecContext>();
                if reader.mapped {
                    // avcodec_send_packet may prefetch one output. RCR releases
                    // all returned mappings before the next receive/input cycle;
                    // reserve two caller frames without retaining a warm pool.
                    (*full_context).hwaccel_flags |= ffi::AV_HWACCEL_FLAG_UNSAFE_OUTPUT as c_int;
                    (*full_context).extra_hw_frames = MAPPED_CALLER_FRAMES;
                }
                let created = (a.av_hwdevice_ctx_create)(
                    &mut (*full_context).hw_device_ctx,
                    ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_CUDA,
                    ptr::null(), ptr::null_mut(), 0,
                );
                reader.interrupt.status(created, "Cannot create native AV1 CUDA device")?;
                ensure!(!(*full_context).hw_device_ctx.is_null(), "Missing native AV1 CUDA device");
            }
            let mut options = ptr::null_mut();
            // Only the proxy decoder opts into two threads. Frame threading
            // adds lookahead, so keep the default/source fallback at one.
            let threads = if backend == SeekBackend::Cuvid { c"4" }
                else if backend == SeekBackend::Software && reader_options.software_threads == 2 { c"2" }
                else { c"1" };
            (a.av_dict_set)(&mut options, c"threads".as_ptr(), threads.as_ptr(), 0);
            if backend == SeekBackend::Software && reader_options.software_threads == 2
                && reader_options.software_slice_threads {
                (a.av_dict_set)(&mut options, c"thread_type".as_ptr(), c"slice".as_ptr(), 0);
            }
            if let Some((width, height)) = resize.filter(|_| backend == SeekBackend::Cuvid) {
                let value = CString::new(format!("{width}x{height}"))?;
                (a.av_dict_set)(&mut options, c"resize".as_ptr(), value.as_ptr(), 0);
                (a.av_dict_set)(&mut options, c"flags".as_ptr(), c"+low_delay".as_ptr(), 0);
            }
            let opened = (a.avcodec_open2)(reader.codec, decoder, &mut options);
            (a.av_dict_free)(&mut options);
            reader.interrupt.check()?;
            ensure!(opened >= 0, "Cannot initialize video decoder");
            if native_av1_runtime_compatible(a._runtime.info.versions) {
                // Actual configured count, not an assertion that slice/frame
                // workers ran in parallel. Full layout access stays ABI-gated.
                reader.codec_threads = (*reader.codec.cast::<ffi::AVCodecContext>()).thread_count;
                reader.codec_thread_type = (*reader.codec.cast::<ffi::AVCodecContext>()).active_thread_type;
            }
            reader.packet = (a.av_packet_alloc)();
            reader.decoded = (a.av_frame_alloc)();
            reader.last = (a.av_frame_alloc)();
            reader.cpu = (a.av_frame_alloc)();
            ensure!(
                !reader.packet.is_null()
                    && !reader.decoded.is_null()
                    && !reader.last.is_null()
                    && !reader.cpu.is_null(),
                "Cannot allocate decode buffers"
            );
        }
        Ok(reader)
    }
    #[cfg(test)]
    unsafe fn frame(
        &mut self,
        seconds: f64,
        at: u64,
        size: (u32, u32),
        cancel: &AtomicBool,
    ) -> Result<PreviewFrame> {
        self.frame_mode(seconds, at, size, cancel, None)
    }
    unsafe fn frame_mode(
        &mut self, seconds: f64, at: u64, size: (u32, u32), cancel: &AtomicBool,
        landing: Option<LandingMap>,
    ) -> Result<PreviewFrame> {
        // Explicit bounded diagnostic only; normal seeking emits no trace.
        self.decoded_frames = 0;
        self.video_packets = 0;
        self.first_decoded_pts = None;
        static TRACE_REQUESTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let trace = super::trace_enabled()
            && TRACE_REQUESTS.fetch_add(1, Ordering::Relaxed) < 256;
        let trace_started = trace.then(Instant::now);
        let milliseconds = |started: Option<Instant>| started.map_or(0., |started| started.elapsed().as_secs_f64()*1000.);
        self.interrupt.begin()?;
        let target = self.origin
            + (seconds * f64::from(self.base.den) / f64::from(self.base.num)).round() as i64;
        self.warm_until =
            Some(target + (0.75 * f64::from(self.base.den) / f64::from(self.base.num)) as i64);
        if let Some(index) = self.cache.iter().position(|entry| entry.pts == target) {
            let surface = self.cache.remove(index).unwrap();
            let frame = surface.frame;
            self.cache.push_back(surface);
            self.cache_hits += 1;
            let result = self.output(frame, at, size);
            if trace { super::write_seek_trace("Native seek trace", serde_json::json!({"requested_frame":at,"hardware":self.hardware,"backend":self.backend.name(),"codec_threads":self.codec_threads,"codec_thread_type":self.codec_thread_type,"decoded_frames":self.decoded_frames,"video_packets":self.video_packets,"first_decoded_pts":self.first_decoded_pts,"direct_mapped":self.mapped,"decoder_pool_size":self.mapped_pool.unwrap_or(0),"mapped_retained_peak":self.mapped_peak_owned,"surface_hit":true,"total_ms":milliseconds(trace_started),"ok":result.is_ok()})); }
            return result;
        }
        let near = forward_near(self.position, target, self.base, self.rate, self.proxy_profile);
        let mut seek_ms = 0.;
        let mut flush_ms = 0.;
        if !near {
            if self.mapped { self.release_mapped_outputs()?; }
            let a = &self.api;
            let phase = trace.then(Instant::now);
            let sought = (a.av_seek_frame)(self.format, self.stream as c_int, target, 1);
            self.interrupt.status(sought, "Cannot seek source")?;
            seek_ms = milliseconds(phase);
            let phase = trace.then(Instant::now);
            (a.avcodec_flush_buffers)(self.codec);
            flush_ms = milliseconds(phase);
            self.position = None;
            self.draining = false;
        }
        let phase = trace.then(Instant::now);
        let decode_target = if !near {
            landing.and_then(|mapping| mapping.first_pts(self.origin, self.base)).unwrap_or(target)
        } else { target };
        self.decode_to(decode_target, cancel)?;
        let actual_frame = landing.and_then(|mapping| self.position.and_then(|pts| mapping.actual_frame(pts, self.origin, self.base)));
        // Out-of-clip landings never borrow the requested identity. Complete the
        // exact route instead; trimmed-away pixels must not be displayed.
        let output_at = if let Some(actual) = actual_frame { actual } else {
            if landing.is_some() { self.decode_to(target, cancel)?; }
            at
        };
        ensure!(landing.is_none_or(|mapping| self.position.is_some_and(|pts| mapping.contains_pts(pts, self.origin, self.base))),
            "Seek landing is outside the active clip source interval");
        let decode_ms = milliseconds(phase);
        let phase = trace.then(Instant::now);
        let result = self.output(self.last, output_at, size);
        if trace { super::write_seek_trace("Native seek trace", serde_json::json!({
            "requested_frame":at,"actual_frame":output_at,"landing_request":landing.is_some(),"target_pts":target,"actual_pts":self.position,"hardware":self.hardware,"backend":self.backend.name(),"codec_threads":self.codec_threads,"codec_thread_type":self.codec_thread_type,
            "decoded_frames":self.decoded_frames,"video_packets":self.video_packets,"first_decoded_pts":self.first_decoded_pts,"proxy_profile":self.proxy_profile,
            "decode_counter_scope":"Returned AVFrames and accepted video packets for this request; excludes codec-internal prefetch/DPB and previous warming",
            "direct_mapped":self.mapped,"decoder_pool_size":self.mapped_pool.unwrap_or(0),"mapped_retained_peak":self.mapped_peak_owned,
            "mapped_received":self.mapped_received,"mapped_releases_before_advance":self.mapped_advancing_releases,"mapped_target_readbacks":self.mapped_readbacks,
            "mapped_counter_scope":"RCR-owned returned mappings only; excludes FFmpeg internal prefetch and codec DPB",
            "surface_hit":false,"near_forward":near,"seek_ms":seek_ms,"flush_ms":flush_ms,
            "decode_ms":decode_ms,"output_ms":milliseconds(phase),"total_ms":milliseconds(trace_started),"ok":result.is_ok()})); }
        result
    }
    unsafe fn mapped_owned_outputs(&self) -> u32 {
        let decoded = (*self.decoded).data[0];
        let last = (*self.last).data[0];
        u32::from(!decoded.is_null()) + u32::from(!last.is_null() && last != decoded)
    }
    unsafe fn release_mapped_outputs(&mut self) -> Result<()> {
        ensure!(mapped_retention_valid(self.cache.len(), self.mapped_owned_outputs()) && self.cache_bytes == 0,
            "Mapped seek cannot retain cached decoder surfaces");
        (self.api.av_frame_unref)(self.decoded);
        (self.api.av_frame_unref)(self.last);
        Ok(())
    }
    unsafe fn inspect_mapped_pool(&mut self, required: bool) -> Result<()> {
        if !self.mapped { return Ok(()); }
        ensure!(mapped_eligible(true, self.backend, self.api._runtime.info.versions),
            "Mapped seek requires the verified native AV1 layout");
        let codec = &*self.codec.cast::<ffi::AVCodecContext>();
        ensure!(codec.hwaccel_flags & ffi::AV_HWACCEL_FLAG_UNSAFE_OUTPUT as c_int != 0
            && codec.extra_hw_frames == MAPPED_CALLER_FRAMES, "Mapped seek hardware reservation changed");
        if codec.hw_frames_ctx.is_null() {
            ensure!(!required, "Mapped decoder has no frames context");
            return Ok(());
        }
        let reference = &*codec.hw_frames_ctx;
        ensure!(!reference.data.is_null() && reference.size >= std::mem::size_of::<ffi::AVHWFramesContext>(),
            "Mapped decoder frames context is unavailable");
        // NVDEC's codec context describes the fixed decoder pool. Returned
        // CUDA images instead reference the separate output context (size zero).
        let frames = &*reference.data.cast::<ffi::AVHWFramesContext>();
        ensure!(mapped_pool_valid(frames.initial_pool_size, frames.format, frames.sw_format),
            "Mapped decoder pool {} or CUDA format is unsupported", frames.initial_pool_size);
        self.mapped_pool = Some(frames.initial_pool_size);
        Ok(())
    }
    unsafe fn retain_surface(&mut self, pts: i64) -> Result<()> {
        if self.mapped { return Ok(()); }
        if self.cache.iter().any(|entry| entry.pts == pts) {
            return Ok(());
        }
        let frame = &*self.decoded;
        let format = if self.hardware {
            (*self.codec.cast::<CodecContext>()).software_format
        } else {
            frame.format
        };
        // Account for conservative device-pitch alignment, not just visible pixels.
        let bytes = (self.api.av_image_get_buffer_size)(format, frame.width, frame.height, 512);
        if bytes <= 0 {
            return Ok(());
        }
        let bytes = bytes as usize;
        let budget = if self.hardware {
            192 * 1024 * 1024
        } else {
            32 * 1024 * 1024
        };
        if bytes > budget {
            return Ok(());
        }
        while self.cache_bytes + bytes > budget || self.cache.len() >= 180 {
            if let Some(mut old) = self.cache.pop_front() {
                self.cache_bytes -= old.bytes;
                (self.api.av_frame_free)(&mut old.frame);
                self.surface_accounting.set(self.cache_bytes);
            } else {
                break;
            }
        }
        let mut retained = (self.api.av_frame_alloc)();
        ensure!(
            !retained.is_null(),
            "Cannot allocate cached surface reference"
        );
        if (self.api.av_frame_ref)(retained, self.decoded) < 0 {
            (self.api.av_frame_free)(&mut retained);
            anyhow::bail!("Cannot retain cached surface");
        }
        self.cache_bytes += bytes;
        self.cache.push_back(Surface {
            frame: retained,
            pts,
            bytes,
        });
        self.surface_accounting.set(self.cache_bytes);
        Ok(())
    }
    unsafe fn warm_step(&mut self, cancel: &AtomicBool) -> Result<bool> {
        if self.mapped || !self.hardware || self.draining || cancel.load(Ordering::Acquire) {
            return Ok(false);
        }
        let Some(position) = self.position else {
            return Ok(false);
        };
        if !self.warm_until.is_some_and(|end| position < end) {
            return Ok(false);
        }
        self.interrupt.begin()?;
        self.decode_to(position + 1, cancel)?;
        Ok(self.position.is_some_and(|p| p > position))
    }
    unsafe fn decode_to(&mut self, target: i64, cancel: &AtomicBool) -> Result<()> {
        let a = self.api.clone();
        while self.position.is_none_or(|p| p < target) {
            self.interrupt.check()?;
            ensure!(!cancel.load(Ordering::Acquire), "Seek cancelled");
            if self.mapped {
                // Never carry the old decoder index into the next decode. If
                // EOF needs the discarded last sample, use ordinary fallback.
                if !(*self.last).data[0].is_null() { self.mapped_advancing_releases += 1; }
                self.release_mapped_outputs()?;
            }
            let received = (a.avcodec_receive_frame)(self.codec, self.decoded);
            if received >= 0 {
                self.decoded_frames = self.decoded_frames.saturating_add(1);
                self.first_decoded_pts.get_or_insert((*self.decoded).pts);
            }
            self.interrupt.check()?;
            if received >= 0 {
                if self.mapped {
                    self.inspect_mapped_pool(true)?;
                    self.mapped_received += 1;
                    self.mapped_peak_owned = self.mapped_peak_owned.max(self.mapped_owned_outputs());
                    ensure!(mapped_retention_valid(self.cache.len(), self.mapped_owned_outputs())
                        && self.mapped_owned_outputs() == 1, "Mapped seek retained unexpected decoder surfaces");
                }
                let pts = (*self.decoded).pts;
                ensure!(pts != i64::MIN, "Video frame has no timestamp");
                self.position = Some(pts);
                self.retain_surface(pts)?;
                (a.av_frame_unref)(self.last);
                ensure!(
                    (a.av_frame_ref)(self.last, self.decoded) >= 0,
                    "Cannot retain video frame"
                );
                continue;
            }
            if received == -541478725
                && self.position.is_some_and(|p| {
                    (target - p) as f64 * f64::from(self.base.num) / f64::from(self.base.den) <= 1.
                })
            {
                ensure!(!self.mapped, "Mapped seek exhausted before target; no retained EOF image");
                break; // Audio/container duration can extend beyond the final video sample.
            }
            ensure!(
                received == -11,
                "Video decode exhausted or failed ({received})"
            );
            ensure!(!self.draining, "No video frame at requested time");
            loop {
                self.interrupt.check()?;
                ensure!(!cancel.load(Ordering::Acquire), "Seek cancelled");
                (a.av_packet_unref)(self.packet);
                let read = (a.av_read_frame)(self.format, self.packet);
                self.interrupt.check()?;
                if read < 0 {
                    ensure!(read == -541478725, "Cannot read source packets ({read})");
                    let drained = (a.avcodec_send_packet)(self.codec, ptr::null());
                    self.interrupt.status(drained, "Cannot drain decoder")?;
                    self.inspect_mapped_pool(false)?;
                    self.draining = true;
                    break;
                }
                if (*self.packet).stream == self.stream as c_int {
                    let sent = (a.avcodec_send_packet)(self.codec, self.packet);
                    if sent >= 0 { self.video_packets = self.video_packets.saturating_add(1); }
                    (a.av_packet_unref)(self.packet);
                    self.interrupt.status(sent, "Cannot decode video packet")?;
                    self.inspect_mapped_pool(false)?;
                    break;
                }
                ensure!(!cancel.load(Ordering::Acquire), "Seek cancelled");
            }
        }
        Ok(())
    }
    unsafe fn output(
        &mut self,
        source: *const Frame,
        at: u64,
        size: (u32, u32),
    ) -> Result<PreviewFrame> {
        self.interrupt.check()?;
        let a = &self.api;
        let frame = if self.hardware {
            (a.av_frame_unref)(self.cpu);
            let transferred = (a.av_hwframe_transfer_data)(self.cpu, source, 0);
            self.interrupt.status(transferred, "Cannot download requested GPU frame")?;
            if self.mapped { self.mapped_readbacks += 1; }
            &*self.cpu
        } else {
            &*source
        };
        ensure!(
            frame.width > 0 && frame.height > 0 && frame.width <= 16384 && frame.height <= 16384,
            "Invalid video dimensions"
        );
        let aspect = if (*source).aspect.num > 0 && (*source).aspect.den > 0 {
            f64::from((*source).aspect.num) / f64::from((*source).aspect.den)
        } else {
            1.
        };
        let (width, height) =
            fitted_dimensions((frame.width as u32, frame.height as u32), aspect, size);
        let bytes = size.0 as usize * size.1 as usize * 4;
        ensure!(
            bytes <= 64 * 1024 * 1024,
            "Seek image exceeds memory budget"
        );
        let mut bgra = padded_bgra_buffer(bytes);
        self.scaler = (a.sws_getCachedContext)(
            self.scaler,
            frame.width,
            frame.height,
            frame.format,
            width as c_int,
            height as c_int,
            (a.av_get_pix_fmt)(c"bgra".as_ptr()),
            1,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        );
        self.interrupt.check()?;
        ensure!(!self.scaler.is_null(), "Cannot configure seek scaler");
        let context = &*self.codec.cast::<CodecContext>();
        let colorspace = context.colorspace;
        let range = context.range;
        let matrix = match colorspace {
            1 | 4 | 5 | 6 | 7 => colorspace as c_int,
            9 | 10 => 9,
            _ => 5,
        };
        let coefficients = (a.sws_getCoefficients)(matrix);
        let configured = (a.sws_setColorspaceDetails)(
                self.scaler,
                coefficients,
                c_int::from(range == 2),
                coefficients,
                1,
                0,
                1 << 16,
                1 << 16
            );
        self.interrupt.status(configured, "Cannot configure seek colors")?;
        let offset = (((size.1 - height) / 2) as usize * size.0 as usize
            + ((size.0 - width) / 2) as usize)
            * 4;
        let mut planes = [ptr::null_mut(); 4];
        planes[0] = bgra.as_mut_ptr().add(offset);
        let strides = [(size.0 * 4) as c_int, 0, 0, 0];
        let scaled = (a.sws_scale)(
                self.scaler,
                frame.data.as_ptr(),
                frame.lines.as_ptr(),
                0,
                frame.height,
                planes.as_ptr(),
                strides.as_ptr()
            );
        self.interrupt.check()?;
        ensure!(scaled == height as c_int, "Cannot convert seek frame");
        bgra.truncate(bytes);
        self.interrupt.check()?;
        Ok(PreviewFrame {
            width: size.0,
            height: size.1,
            frame: at,
            bgra,
        })
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        // All functions accept null resources; the same cleanup covers partial initialization.
        unsafe {
            for mut surface in self.cache.drain(..) {
                (self.api.av_frame_free)(&mut surface.frame);
            }
            (self.api.sws_freeContext)(self.scaler);
            (self.api.av_frame_free)(&mut self.decoded);
            (self.api.av_frame_free)(&mut self.last);
            (self.api.av_frame_free)(&mut self.cpu);
            (self.api.av_packet_free)(&mut self.packet);
            (self.api.avcodec_free_context)(&mut self.codec);
            (self.api.avformat_close_input)(&mut self.format);
        }
        self.cache_bytes = 0;
        self.surface_accounting.set(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proxy_forward_budget_is_one_rounded_frame_while_source_retains_quarter_second() {
        let ms = Rational { num: 1, den: 1000 };
        let fps60 = Rational { num: 60, den: 1 };
        for delta in [0, 16, 17] { assert!(forward_near(Some(1000), 1000 + delta, ms, fps60, true)); }
        for delta in [18, 33, 34, 250] { assert!(!forward_near(Some(1000), 1000 + delta, ms, fps60, true)); }
        let fps30 = Rational { num: 30, den: 1 };
        for delta in [33, 34] { assert!(forward_near(Some(0), delta, ms, fps30, true)); }
        assert!(!forward_near(Some(0), 66, ms, fps30, true));
        assert!(forward_near(Some(0), 250, ms, fps60, false));
        assert!(!forward_near(Some(0), 251, ms, fps60, false));
        assert!(!forward_near(Some(1), 0, ms, fps60, true));
        assert!(!forward_near(None, 0, ms, fps60, true));
        for invalid in [Rational { num: 0, den: 1 }, Rational { num: 1, den: 0 }, Rational { num: -1, den: 1 }] {
            assert!(!forward_near(Some(0), 1, invalid, fps60, true));
            assert!(!forward_near(Some(0), 1, ms, invalid, true));
        }
        let extreme = Rational { num: i32::MAX, den: 1 };
        assert!(!forward_near(Some(i64::MIN), i64::MAX, extreme, extreme, true));
        assert!(!forward_near(Some(i64::MIN), i64::MAX, extreme, extreme, false));
        assert!(!ReaderOptions::default().proxy_profile);
    }
    #[test]
    fn proxy_thread_experiment_is_explicit_and_bounded() {
        use std::ffi::OsStr;
        assert_eq!(proxy_threads(None), 1);
        for value in ["", "0", "1", "3", "64", "auto", "-1"] {
            assert_eq!(proxy_threads(Some(OsStr::new(value))), 1);
        }
        assert_eq!(proxy_threads(Some(OsStr::new("2"))), 2);
        assert_eq!(ReaderOptions::default().software_threads, 1);
        assert!(!ReaderOptions::default().software_slice_threads);
        assert!(proxy_slice_threads(2, Some(OsStr::new("slice"))));
        for threads in [0, 1, 3, 64] {
            assert!(!proxy_slice_threads(threads, Some(OsStr::new("slice"))));
        }
        for value in [None, Some(OsStr::new("frame")), Some(OsStr::new("auto")), Some(OsStr::new("Slice"))] {
            assert!(!proxy_slice_threads(2, value));
        }
    }
    #[test]
    fn original_refinement_refuses_downsized_reader_but_fast_can_reuse_full_source() {
        assert!(!original_reader_compatible(true, Some((1280, 720))));
        assert!(!original_reader_compatible(true, Some((1920, 1080))));
        assert!(original_reader_compatible(true, None));
        assert!(original_reader_compatible(false, None));
        assert!(original_reader_compatible(false, Some((1280, 720))));
        assert!(resize_covers(None, (1920, 1080)));
        assert!(resize_covers(Some((1920, 1080)), (1280, 720)));
        assert!(!resize_covers(Some((1280, 720)), (1920, 1080)));
        assert!(!resize_covers(Some((1920, 720)), (1920, 1080)));
    }
    #[test]
    fn request_generation_interrupt_is_owned_scoped_and_typed_without_global_cancel() {
        let cancel = Arc::new(AtomicBool::new(false));
        let desired = Arc::new(AtomicU64::new(7));
        let mut interrupt = Box::new(SeekInterrupt::new(cancel.clone()));
        interrupt.generation = Some(RequestGeneration { desired: desired.clone(), serial: 7 });
        let callback = (&mut *interrupt as *mut SeekInterrupt).cast();
        assert_eq!(unsafe { interrupt_seek(callback) }, 0);
        desired.store(8, Ordering::Release);
        assert_eq!(unsafe { interrupt_seek(callback) }, 1);
        let error = interrupt.check().unwrap_err().context("superseded original decode");
        assert!(matches!(error.downcast_ref::<SeekStopped>(), Some(SeekStopped(SeekStopReason::Superseded))));
        assert!(!retry_allowed(&cancel, &error), "Superseded decode started a fallback");
        assert!(!cancel.load(Ordering::Acquire));
        interrupt.generation = None;
        interrupt.begin().unwrap();
        assert_eq!(unsafe { interrupt_seek(callback) }, 0, "Later ordinary requests inherited an obsolete scope");
        interrupt.generation = Some(RequestGeneration { desired: desired.clone(), serial: 8 });
        interrupt.begin().unwrap();
        assert_eq!(Arc::strong_count(&desired), 2);
        drop(interrupt);
        assert_eq!(Arc::strong_count(&desired), 1);
    }
    #[test]
    fn direct_mapped_output_requires_exact_native_backend_and_bounded_known_pool() {
        let pack = |major: u32, minor: u32, micro: u32| (major << 16) | (minor << 8) | micro;
        let exact = [pack(59, 8, 100), pack(61, 3, 100), pack(61, 1, 100), pack(8, 1, 100)];
        assert!(mapped_eligible(true, SeekBackend::NativeAv1, exact));
        assert!(!mapped_eligible(false, SeekBackend::NativeAv1, exact));
        assert!(!mapped_eligible(true, SeekBackend::Cuvid, exact));
        assert!(!mapped_eligible(true, SeekBackend::Software, exact));
        let mut changed = exact;
        changed[1] += 1;
        assert!(!mapped_eligible(true, SeekBackend::NativeAv1, changed));
        let cuda = ffi::AVPixelFormat_AV_PIX_FMT_CUDA;
        let nv12 = ffi::AVPixelFormat_AV_PIX_FMT_NV12;
        for pool in [-1, 0, 1, 2, 33, c_int::MAX] { assert!(!mapped_pool_valid(pool, cuda, nv12)); }
        for pool in [3, 23, 32] { assert!(mapped_pool_valid(pool, cuda, nv12)); }
        assert!(!mapped_pool_valid(23, nv12, nv12));
        assert!(!mapped_pool_valid(23, cuda, ffi::AVPixelFormat_AV_PIX_FMT_P010LE));
        assert_eq!(MAPPED_CALLER_FRAMES, 2);
        assert!(mapped_retention_valid(0, 0));
        assert!(mapped_retention_valid(0, 1));
        assert!(!mapped_retention_valid(0, 2));
        assert!(!mapped_retention_valid(1, 1), "Mapped GPU output must never enter the warm surface cache");
    }
    #[test]
    fn unknown_mapped_pool_falls_back_to_legacy_and_deadline_never_does() {
        let calls = std::cell::Cell::new(0);
        let cancel = AtomicBool::new(false);
        let result = native_then_legacy(&cancel,
            || { anyhow::ensure!(mapped_pool_valid(0, ffi::AVPixelFormat_AV_PIX_FMT_CUDA,
                ffi::AVPixelFormat_AV_PIX_FMT_NV12), "Unknown decoder pool"); Ok(1) },
            || { calls.set(calls.get() + 1); Ok(2) });
        assert_eq!(result.unwrap(), 2);
        assert_eq!(calls.get(), 1);
        let error = native_then_legacy::<u32>(&cancel,
            || Err(SeekStopped(SeekStopReason::Deadline).into()),
            || { calls.set(99); Ok(2) }).unwrap_err();
        assert!(is_stopped(&error));
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn landing_identity_preserves_source_origin_trim_bounds_and_timestamp_grid() {
        let base = Rational { num: 1, den: 1000 };
        let mapping = LandingMap { clip_start: 100, clip_duration: 120, source_in_ns: 2_000_000_000, fps: 60 };
        assert_eq!(mapping.first_pts(500, base), Some(2500));
        assert_eq!(mapping.actual_frame(2499, 500, base), None, "Pre-trim source pixels must not receive clip.start identity");
        assert_eq!(mapping.actual_frame(2500, 500, base), Some(100));
        assert_eq!(mapping.actual_frame(2517, 500, base), Some(101));
        assert_eq!(mapping.actual_frame(2533, 500, base), Some(102));
        assert_eq!(mapping.actual_frame(4500, 500, base), None, "Source-out landing escaped the active clip");
        assert!(!mapping.contains_pts(2499, 500, base));
        assert!(mapping.contains_pts(2500, 500, base));
        assert!(mapping.contains_pts(4499, 500, base));
        assert!(!mapping.contains_pts(4500, 500, base));
        assert_eq!(mapping.actual_frame(2501, 500, base), None, "Off-grid pixels could contaminate an exact frame cache");
        let fractional = LandingMap { source_in_ns: 2_000_000_001, ..mapping };
        assert_eq!(fractional.first_pts(500, base), Some(2501), "Trim's first PTS must round up, never reveal trimmed pixels");
        assert_eq!(fractional.actual_frame(2500, 500, base), None);
        assert!(!fractional.contains_pts(2500, 500, base));
        assert!(fractional.contains_pts(2501, 500, base));
        assert_eq!(mapping.actual_frame(i64::MIN, 500, base), None);
        assert_eq!(mapping.actual_frame(2500, 500, Rational { num: 0, den: 1 }), None);
    }
    #[test]
    fn native_av1_full_layout_requires_exact_header_family_and_cuda_device_config() {
        let pack = |major: u32, minor: u32, micro: u32| (major << 16) | (minor << 8) | micro;
        let exact = [pack(59, 8, 100), pack(61, 3, 100), pack(61, 1, 100), pack(8, 1, 100)];
        assert!(native_av1_runtime_compatible(exact));
        for component in 0..3 {
            let mut changed = exact;
            changed[component] += 1;
            assert!(!native_av1_runtime_compatible(changed), "Full layout accepted a different header family");
        }
        let mut changed = exact;
        changed[3] = pack(9, 0, 100);
        assert!(!native_av1_runtime_compatible(changed));
        let valid = ffi::AVCodecHWConfig {
            pix_fmt: ffi::AVPixelFormat_AV_PIX_FMT_CUDA,
            methods: ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX,
            device_type: ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_CUDA,
        };
        assert!(cuda_device_config(&valid));
        assert!(!cuda_device_config(&ffi::AVCodecHWConfig { methods: ffi::AV_CODEC_HW_CONFIG_METHOD_INTERNAL, ..valid }));
        assert!(!cuda_device_config(&ffi::AVCodecHWConfig { pix_fmt: ffi::AVPixelFormat_AV_PIX_FMT_NV12, ..valid }));
        assert!(!cuda_device_config(&ffi::AVCodecHWConfig { device_type: ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_D3D11VA, ..valid }));
    }
    #[test]
    fn native_av1_guard_failure_retries_legacy_but_typed_stops_never_do() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let cancel = AtomicBool::new(false);
        let result = native_then_legacy(&cancel,
            || { anyhow::ensure!(native_av1_runtime_compatible([0; 4]), "Wrong runtime"); Ok(1) },
            || { calls.set(calls.get() + 1); Ok(2) });
        assert_eq!(result.unwrap(), 2);
        assert_eq!(calls.get(), 1);
        assert_eq!(native_then_legacy(&cancel, || Ok(3), || { calls.set(99); Ok(4) }).unwrap(), 3);
        assert_eq!(calls.get(), 1);
        for reason in [SeekStopReason::Cancelled, SeekStopReason::Deadline, SeekStopReason::Superseded] {
            let error = native_then_legacy::<u32>(&cancel,
                || Err(SeekStopped(reason).into()),
                || { calls.set(99); Ok(4) }).unwrap_err();
            assert!(is_stopped(&error));
            assert_eq!(calls.get(), 1);
        }
        // The caller's existing software fallback follows the legacy attempt,
        // and preserves typed stops from either hardware backend.
        let order = std::cell::RefCell::new(Vec::new());
        let result = native_then_legacy::<u32>(&cancel,
            || { order.borrow_mut().push("native"); Err(anyhow::anyhow!("CUDA initialization failed")) },
            || { order.borrow_mut().push("cuvid"); Err(anyhow::anyhow!("Legacy initialization failed")) })
            .or_else(|error| retry_if_allowed(&cancel, error, || {
                order.borrow_mut().push("software"); Ok(7)
            }));
        assert_eq!(result.unwrap(), 7);
        assert_eq!(*order.borrow(), ["native", "cuvid", "software"]);
        let result = native_then_legacy::<u32>(&cancel,
            || Err(anyhow::anyhow!("CUDA initialization failed")),
            || Err(SeekStopped(SeekStopReason::Deadline).into()))
            .or_else(|error| retry_if_allowed(&cancel, error, || { calls.set(99); Ok(4) }));
        assert!(is_stopped(&result.unwrap_err()));
        assert_eq!(calls.get(), 1);
        cancel.store(true, Ordering::Release);
        assert!(native_then_legacy::<u32>(&cancel, || Err(anyhow::anyhow!("Initialization failed")),
            || { calls.set(99); Ok(4) }).is_err());
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn seek_buffer_preserves_background_zero_padding_and_exact_allocation_bound() {
        for bytes in [0, 4, 3 * 5 * 4, 1280 * 720 * 4] {
            let mut pixels = padded_bgra_buffer(bytes);
            assert_eq!(pixels.len(), bytes + 64);
            assert_eq!(pixels.capacity(), bytes + 64, "Seek pixels unexpectedly reserved a second full frame");
            assert!(pixels[..bytes].as_chunks::<4>().0.iter().all(|pixel| *pixel == [0x14, 0x0e, 0x0b, 255]),
                "Letterbox/background pixels changed");
            assert!(pixels[bytes..].iter().all(|byte| *byte == 0), "FFmpeg padding must be fully initialized and zero");
            pixels.truncate(bytes);
            assert_eq!(pixels.len(), bytes);
            assert_eq!(pixels.capacity(), bytes + 64, "Publishing a frame must retain only the accounted padding allocation");
        }
    }
    #[test]
    fn closing_previous_native_worker_allows_next_session_within_retry_budget() {
        static TEST_GATE: AtomicBool = AtomicBool::new(false);
        let first = NativeAdmission::acquire(&TEST_GATE).unwrap();
        let retiring = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            drop(first);
        });
        let second = NativeAdmission::acquire_cancellable(
            &TEST_GATE,
            &AtomicBool::new(false),
            Duration::from_secs(1),
        )
        .unwrap();
        retiring.join().unwrap();
        assert!(TEST_GATE.load(Ordering::Acquire));
        drop(second);
        assert!(!TEST_GATE.load(Ordering::Acquire));
    }
    #[test]
    fn cancelled_native_admission_wait_never_acquires_or_leaks_a_permit() {
        static TEST_GATE: AtomicBool = AtomicBool::new(false);
        let first = NativeAdmission::acquire(&TEST_GATE).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelling = cancel.clone();
        let signal = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            cancelling.store(true, Ordering::Release);
        });
        let error =
            NativeAdmission::acquire_cancellable(&TEST_GATE, &cancel, Duration::from_secs(1))
                .err()
                .expect("Cancelled native admission unexpectedly succeeded");
        signal.join().unwrap();
        assert!(matches!(
            error.downcast_ref::<SeekStopped>(),
            Some(SeekStopped(SeekStopReason::Cancelled))
        ));
        assert!(TEST_GATE.load(Ordering::Acquire));
        drop(first);
        let next = NativeAdmission::acquire(&TEST_GATE).unwrap();
        drop(next);
        assert!(!TEST_GATE.load(Ordering::Acquire));
    }
    #[test]
    fn native_admission_survives_shared_proxy_owner_until_last_teardown() {
        static TEST_GATE: AtomicBool = AtomicBool::new(false);
        let worker = NativeAdmission::acquire(&TEST_GATE).unwrap();
        let proxy = worker.clone();
        assert!(NativeAdmission::acquire(&TEST_GATE).is_err());
        drop(worker);
        assert!(NativeAdmission::acquire(&TEST_GATE).is_err());
        drop(proxy);
        let next = NativeAdmission::acquire(&TEST_GATE).unwrap();
        drop(next);
        assert!(!TEST_GATE.load(Ordering::Acquire));
    }
    #[test]
    fn boxed_callback_remains_valid_after_move_and_observes_cancel_deadline() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut state = Box::new(SeekInterrupt::new(cancel.clone()));
        let opaque = (&mut *state as *mut SeekInterrupt).cast();
        let mut moved_owner = state;
        assert_eq!(unsafe { interrupt_seek(opaque) }, 0);
        cancel.store(true, Ordering::Release);
        assert_eq!(unsafe { interrupt_seek(opaque) }, 1);
        assert_eq!(moved_owner.reason(), Some(SeekStopReason::Cancelled));
        cancel.store(false, Ordering::Release);
        moved_owner.deadline = Instant::now();
        assert_eq!(unsafe { interrupt_seek(opaque) }, 1);
        assert_eq!(moved_owner.reason(), Some(SeekStopReason::Deadline));
        let error = moved_owner.status(-1, "interrupted read").unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SeekStopped>(),
            Some(SeekStopped(SeekStopReason::Deadline))
        ));
        moved_owner.begin().unwrap();
        assert_eq!(unsafe { interrupt_seek(opaque) }, 0);
        drop(moved_owner);
        assert_eq!(Arc::strong_count(&cancel), 1);
        assert_eq!(unsafe { interrupt_seek(ptr::null_mut()) }, 1);
    }
    #[test]
    fn cancellation_or_deadline_never_reopens_software_reader() {
        let cancel = AtomicBool::new(true);
        let calls = std::cell::Cell::new(0);
        assert!(
            retry_if_allowed::<()>(&cancel, anyhow::anyhow!("hardware open failed"), || {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .is_err()
        );
        cancel.store(false, Ordering::Release);
        assert!(retry_if_allowed::<()>(
            &cancel,
            anyhow::Error::new(SeekStopped(SeekStopReason::Deadline)).context("demux open"),
            || {
                calls.set(calls.get() + 1);
                Ok(())
            }
        )
        .is_err());
        assert_eq!(calls.get(), 0);
        retry_if_allowed(
            &cancel,
            anyhow::anyhow!("unsupported hardware decoder"),
            || {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(calls.get(), 1);
    }
    #[test]
    #[ignore = "opt-in read-only native source seek; set RCR_NATIVE_SCRUB_PROOF_SOURCE to an owned short video"]
    fn native_reader_cancellation_owns_callback_through_close() -> Result<()> {
        let path = std::env::var_os("RCR_NATIVE_SCRUB_PROOF_SOURCE")
            .map(PathBuf::from)
            .context("Set RCR_NATIVE_SCRUB_PROOF_SOURCE to an owned short fixture")?;
        ensure!(
            path.is_absolute(),
            "Native seek proof requires an absolute fixture path"
        );
        let before = crate::SourceStamp::read(&path)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let decoder = Decoder::new(&cancel)?;
        let mut reader = Reader::open(decoder.api.clone(), &path, 0, None, cancel.clone())?;
        let frame = unsafe { reader.frame(0., 0, (320, 180), &cancel)? };
        ensure!(frame.bgra.len() == 320 * 180 * 4, "No native seek image");
        cancel.store(true, Ordering::Release);
        let callback = unsafe { (*reader.format).interrupt_callback };
        ensure!(
            unsafe {
                callback
                    .callback
                    .context("Missing installed AVIO callback")?(callback.opaque)
            } == 1,
            "Installed callback missed cancellation"
        );
        ensure!(
            unsafe { reader.frame(0.1, 1, (320, 180), &cancel) }.is_err(),
            "Cancelled reader decoded another image"
        );
        drop(reader);
        ensure!(
            Arc::strong_count(&cancel) == 2,
            "Closed reader retained its cancellation owner"
        );
        drop(decoder);
        ensure!(
            Arc::strong_count(&cancel) == 1,
            "Closed native worker retained cancellation state"
        );
        let admission = NativeAdmission::acquire(&NATIVE_WORKER)?;
        drop(admission);
        ensure!(
            crate::SourceStamp::read(&path)? == before,
            "Source fixture identity changed"
        );
        println!("{{\"native_frame\":true,\"installed_callback_cancelled\":true,\"reader_closed\":true,\"admission_released\":true,\"source_stamp_unchanged\":true}}");
        Ok(())
    }
    #[test]
    #[ignore = "explicit owned-fixture native mapped GPU cancellation proof; requires RCR_TEST_NATIVE_MAPPED=1 and RCR_NATIVE_MAPPED_FIXTURE"]
    fn native_mapped_reader_cancellation_releases_held_gpu_output_and_admission() -> Result<()> {
        ensure!(std::env::var_os("RCR_TEST_NATIVE_MAPPED").is_some_and(|value| value == "1"),
            "Set RCR_TEST_NATIVE_MAPPED=1 for this explicit hardware proof");
        ensure!(std::env::var_os("RCR_SCRUB_NATIVE_AV1").is_some_and(|value| value == "1")
            && std::env::var_os("RCR_SCRUB_NATIVE_MAPPED").is_some_and(|value| value == "1"),
            "Native AV1 and mapped production flags must both be enabled");
        let path = std::env::var_os("RCR_NATIVE_MAPPED_FIXTURE").map(PathBuf::from)
            .context("Set RCR_NATIVE_MAPPED_FIXTURE to an absolute owned .downloads fixture")?;
        ensure!(path.is_absolute(), "Mapped fixture must be an absolute path");
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(Path::parent)
            .context("Cannot locate the mapped proof workspace")?.canonicalize()?;
        let configured_root = workspace.join(".downloads");
        let owned_root = configured_root.canonicalize()?;
        ensure!(owned_root == configured_root, "Owned .downloads root must not resolve through a directory alias");
        let before = crate::SourceStamp::read(&path)?;
        ensure!(before.canonical_path.starts_with(&owned_root) && before.canonical_path != owned_root,
            "Mapped fixture must resolve inside this workspace's .downloads directory");
        ensure!(before.bytes > 0 && before.bytes <= 512 * 1024 * 1024,
            "Mapped lifetime proof requires a bounded owned fixture");
        let cancel = Arc::new(AtomicBool::new(false));
        let decoder = Decoder::new(&cancel)?;
        let mut reader = Reader::open(decoder.api.clone(), &before.canonical_path, 0,
            Some((320, 180)), cancel.clone())?;
        ensure!(reader.backend == SeekBackend::NativeAv1 && reader.mapped && reader.hardware,
            "Proof refused a software or legacy fallback");
        let frame = unsafe { reader.frame(0., 0, (320, 180), &cancel)? };
        ensure!((frame.width, frame.height, frame.bgra.len()) == (320, 180, 320 * 180 * 4),
            "Mapped decoder returned no complete owned BGRA image");
        let pool = reader.mapped_pool.context("Mapped decoder did not report its actual pool")?;
        ensure!((3..=32).contains(&pool) && reader.mapped_peak_owned == 1
            && reader.mapped_readbacks == 1 && reader.cache.is_empty() && reader.cache_bytes == 0,
            "Mapped GPU mode, pool, retention or target readback was not proven");
        ensure!(unsafe { reader.mapped_owned_outputs() } == 1,
            "Cancellation fixture must deliberately hold a real mapped GPU output");
        ensure!(NativeAdmission::acquire(&NATIVE_WORKER).is_err(),
            "Native admission escaped while the GPU reader was still alive");
        cancel.store(true, Ordering::Release);
        let callback = unsafe { (*reader.format).interrupt_callback };
        ensure!(unsafe { callback.callback.context("Missing installed mapped AVIO callback")?(callback.opaque) } == 1,
            "Installed mapped callback missed cancellation");
        let error = unsafe { reader.frame(0.1, 1, (320, 180), &cancel) }.err()
            .context("Cancelled mapped reader decoded another frame")?;
        ensure!(matches!(error.downcast_ref::<SeekStopped>(), Some(SeekStopped(SeekStopReason::Cancelled))),
            "Mapped cancellation lost its typed stop");
        ensure!(unsafe { reader.mapped_owned_outputs() } == 1,
            "Cancellation must leave the held mapping for the actual reader teardown");
        drop(reader);
        ensure!(Arc::strong_count(&cancel) == 2, "Mapped reader retained its callback cancellation owner after close");
        ensure!(NativeAdmission::acquire(&NATIVE_WORKER).is_err(),
            "The owning decoder released admission before its own teardown");
        drop(decoder);
        ensure!(Arc::strong_count(&cancel) == 1, "Mapped worker retained cancellation state after teardown");
        let next_cancel = Arc::new(AtomicBool::new(false));
        let next_decoder = Decoder::new(&next_cancel)?;
        drop(next_decoder);
        ensure!(crate::SourceStamp::read(&path)? == before, "Owned mapped source stamp changed");
        println!("{}", serde_json::json!({"native_mapped_lifetime":"passed",
            "backend":"native_av1_cuda","direct_mapped":true,"decoder_pool_size":pool,
            "mapped_retained_peak":1,"held_mapping_when_cancelled":true,"gpu_surface_cache_bytes":0,
            "installed_callback_cancelled":true,"typed_cancellation":true,
            "reader_closed_before_admission_release":true,"next_decoder_admitted":true,
            "source_stamp_unchanged":true,"source_writes":false,
            "scope":"Synchronous target readback, held CUDA output cancellation and real teardown; not blocked-AVIO interruption or pixel-reference parity"}));
        Ok(())
    }
    #[test]
    fn even_proxy_geometry_preserves_ultrawide_and_anamorphic_display_aspect() {
        let target = (1280, 720);
        // CUDA scaling rounds the ultrawide coded height down to an even 532.
        // Its adjusted SAR carries the rounding difference, preserving source DAR.
        let ultrawide = fitted_dimensions((3840, 1600), 1., target);
        let adjusted_sar = (532. * 3840.) / (1280. * 1600.);
        assert_eq!(
            fitted_dimensions((1280, 532), adjusted_sar, target),
            ultrawide
        );
        assert_eq!(ultrawide, (1280, 533));
        let anamorphic = fitted_dimensions((720, 576), 16. / 15., target);
        assert_eq!(fitted_dimensions((900, 720), 16. / 15., target), anamorphic);
        assert_eq!(anamorphic, (960, 720));
        assert_ne!(fitted_dimensions((900, 720), 1., target), anamorphic);
    }
    #[test]
    fn project_revision_detects_same_path_same_size_source_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.mkv");
        std::fs::write(&path, b"old-source").unwrap();
        let mut project = Project::default();
        project.assets.push(crate::MediaAsset {
            id: 1,
            source_stamp: None,
            path: path.clone(),
            duration_ns: 1_000_000_000,
            width: 1280,
            height: 720,
            streams: vec![crate::MediaStream {
                index: 0,
                kind: TrackKind::Video,
                name: "video".into(),
            }],
        });
        let mut revisions = SourceRevision::default();
        assert!(!revisions.refresh(&project));
        assert!(
            !revisions.refresh(&project),
            "Unchanged edit keeps resident decoder"
        );
        let modified = path.metadata().unwrap().modified().unwrap();
        std::fs::write(&path, b"new-source").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified + std::time::Duration::from_secs(2))
            .unwrap();
        assert!(
            revisions.refresh(&project),
            "Same ID/path/length must invalidate source reader"
        );
        assert!(!revisions.refresh(&project));
        std::fs::remove_file(&path).unwrap();
        assert!(
            revisions.refresh(&project),
            "Disappearing source also releases old reader"
        );
    }
}
