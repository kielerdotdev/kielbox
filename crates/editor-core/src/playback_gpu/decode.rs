//! Version-checked FFmpeg 7.0.2 D3D11VA AV1 decoder; worker confined.
//! All live decoder/frame/context objects stay on their creating worker.
use crate::ffmpeg_runtime::{load_runtime, FfmpegRuntimeConfig, FfmpegRuntimeRequirement, Runtime};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    marker::PhantomData,
    path::{Component, Path, PathBuf, Prefix},
    ptr,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{IUnknown, Interface},
    Win32::{
        Foundation::{HMODULE, LUID, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::*,
            Dxgi::{
                Common::*, CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice,
                IDXGIFactory1,
            },
        },
    },
};

#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    clippy::all
)]
#[path = "ffmpeg7/bindings.rs"]
pub mod ffi;

pub const MAX_FRAMES: usize = 120;
const AGAIN: c_int = -11;
const EOF: c_int = -541478725;
const NO_PTS: i64 = i64::MIN;
const SEEK_BACKWARD: c_int = 1; // Public libavformat/avformat.h AVSEEK_FLAG_BACKWARD.

macro_rules! api {
    ($($lib:ident => $name:ident: $ty:ty),* $(,)?) => {
        #[allow(non_snake_case)]
        struct Api { $($name: $ty,)* _runtime: Arc<Runtime> }
        impl Api {
            fn load(directory: &Path, cancel: &AtomicBool) -> Result<Arc<Self>> {
                let config = if directory.as_os_str().is_empty() {
                    FfmpegRuntimeConfig::Automatic
                } else { FfmpegRuntimeConfig::Directory(directory.to_owned()) };
                let runtime = load_runtime(&config, FfmpegRuntimeRequirement::Gpu702,
                    false, || cancel.load(Ordering::Acquire))?;
                Ok(Arc::new(Self {
                    $($name: unsafe { runtime.symbol::<$ty>(match stringify!($lib) {
                        "util" => 0, "codec" => 1, "format" => 2, _ => unreachable!()
                    }, stringify!($name).as_bytes())? },)*
                    _runtime: runtime,
                }))
            }
            fn error(&self, code: c_int) -> String {
                let mut text = [0i8; 256];
                if unsafe { (self.av_strerror)(code, text.as_mut_ptr(), text.len()) } < 0 {
                    return format!("FFmpeg error {code}");
                }
                unsafe { CStr::from_ptr(text.as_ptr()) }.to_string_lossy().into_owned()
            }
        }
    }
}
api! {
    util => av_strerror: unsafe extern "C" fn(c_int,*mut c_char,usize)->c_int,
    util => av_hwdevice_ctx_alloc: unsafe extern "C" fn(ffi::AVHWDeviceType)->*mut ffi::AVBufferRef,
    util => av_hwdevice_ctx_init: unsafe extern "C" fn(*mut ffi::AVBufferRef)->c_int,
    util => av_buffer_ref: unsafe extern "C" fn(*const ffi::AVBufferRef)->*mut ffi::AVBufferRef,
    util => av_buffer_unref: unsafe extern "C" fn(*mut *mut ffi::AVBufferRef),
    util => av_frame_alloc: unsafe extern "C" fn()->*mut ffi::AVFrame,
    util => av_frame_free: unsafe extern "C" fn(*mut *mut ffi::AVFrame),
    util => av_dict_set: unsafe extern "C" fn(*mut *mut ffi::AVDictionary,*const c_char,*const c_char,c_int)->c_int,
    util => av_dict_free: unsafe extern "C" fn(*mut *mut ffi::AVDictionary),
    codec => avcodec_find_decoder_by_name: unsafe extern "C" fn(*const c_char)->*const ffi::AVCodec,
    codec => avcodec_get_hw_config: unsafe extern "C" fn(*const ffi::AVCodec,c_int)->*const ffi::AVCodecHWConfig,
    codec => avcodec_alloc_context3: unsafe extern "C" fn(*const ffi::AVCodec)->*mut ffi::AVCodecContext,
    codec => avcodec_free_context: unsafe extern "C" fn(*mut *mut ffi::AVCodecContext),
    codec => avcodec_parameters_to_context: unsafe extern "C" fn(*mut ffi::AVCodecContext,*const ffi::AVCodecParameters)->c_int,
    codec => avcodec_open2: unsafe extern "C" fn(*mut ffi::AVCodecContext,*const ffi::AVCodec,*mut *mut ffi::AVDictionary)->c_int,
    codec => avcodec_send_packet: unsafe extern "C" fn(*mut ffi::AVCodecContext,*const ffi::AVPacket)->c_int,
    codec => avcodec_receive_frame: unsafe extern "C" fn(*mut ffi::AVCodecContext,*mut ffi::AVFrame)->c_int,
    codec => avcodec_flush_buffers: unsafe extern "C" fn(*mut ffi::AVCodecContext),
    codec => av_packet_alloc: unsafe extern "C" fn()->*mut ffi::AVPacket,
    codec => av_packet_free: unsafe extern "C" fn(*mut *mut ffi::AVPacket),
    codec => av_packet_unref: unsafe extern "C" fn(*mut ffi::AVPacket),
    format => avformat_alloc_context: unsafe extern "C" fn()->*mut ffi::AVFormatContext,
    format => avformat_open_input: unsafe extern "C" fn(*mut *mut ffi::AVFormatContext,*const c_char,*const ffi::AVInputFormat,*mut *mut ffi::AVDictionary)->c_int,
    format => avformat_find_stream_info: unsafe extern "C" fn(*mut ffi::AVFormatContext,*mut *mut ffi::AVDictionary)->c_int,
    format => avformat_close_input: unsafe extern "C" fn(*mut *mut ffi::AVFormatContext),
    format => av_read_frame: unsafe extern "C" fn(*mut ffi::AVFormatContext,*mut ffi::AVPacket)->c_int,
    format => av_seek_frame: unsafe extern "C" fn(*mut ffi::AVFormatContext,c_int,i64,c_int)->c_int,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Cancelled,
    Deadline,
}
#[derive(Debug)]
pub struct DecodeStopped(pub StopReason);
impl std::fmt::Display for DecodeStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "D3D11VA decode stopped: {:?}", self.0)
    }
}
impl std::error::Error for DecodeStopped {}
#[derive(Debug)]
pub struct GpuInputIneligible(pub String);
impl std::fmt::Display for GpuInputIneligible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for GpuInputIneligible {}
struct Interrupt {
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}
impl Interrupt {
    fn reason(&self) -> Option<StopReason> {
        if self.cancel.load(Ordering::Acquire) {
            Some(StopReason::Cancelled)
        } else if Instant::now() >= self.deadline {
            Some(StopReason::Deadline)
        } else {
            None
        }
    }
    fn check(&self) -> Result<()> {
        match self.reason() {
            Some(reason) => Err(DecodeStopped(reason).into()),
            None => Ok(()),
        }
    }
}
unsafe extern "C" fn interrupt(opaque: *mut c_void) -> c_int {
    if opaque.is_null() {
        return 1;
    }
    i32::from((*opaque.cast::<Interrupt>()).reason().is_some())
}
unsafe extern "C" fn d3d_format(
    _: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    if formats.is_null() {
        return ffi::AVPixelFormat_AV_PIX_FMT_NONE;
    }
    for index in 0..64 {
        let format = *formats.add(index);
        if format == ffi::AVPixelFormat_AV_PIX_FMT_D3D11 {
            return format;
        }
        if format == ffi::AVPixelFormat_AV_PIX_FMT_NONE {
            break;
        }
    }
    ffi::AVPixelFormat_AV_PIX_FMT_NONE // A hardware proof never falls back to CPU.
}

#[derive(Clone, Debug, Serialize)]
pub struct AdapterInfo {
    pub name: String,
    pub luid_low: u32,
    pub luid_high: i32,
    pub vendor_id: u32,
}
pub fn create_device(
    luid: Option<(u32, i32)>,
) -> Result<(ID3D11Device, ID3D11DeviceContext, AdapterInfo)> {
    unsafe {
        let adapter: Option<IDXGIAdapter1> = if let Some((low, high)) = luid {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut found = None;
            for index in 0..64 {
                let Ok(adapter) = factory.EnumAdapters1(index) else {
                    break;
                };
                let desc = adapter.GetDesc1()?;
                if desc.AdapterLuid
                    == (LUID {
                        LowPart: low,
                        HighPart: high,
                    })
                {
                    found = Some(adapter);
                    break;
                }
            }
            Some(found.context("Requested UI adapter LUID was not found")?)
        } else {
            None
        };
        let adapter = adapter
            .map(|adapter| adapter.cast::<IDXGIAdapter>())
            .transpose()?;
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            adapter.as_ref(),
            if adapter.is_some() {
                D3D_DRIVER_TYPE_UNKNOWN
            } else {
                D3D_DRIVER_TYPE_HARDWARE
            },
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let device = device.context("Missing D3D11 device")?;
        let context = context.context("Missing immediate context")?;
        let desc = device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?;
        let end = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        let info = AdapterInfo {
            name: String::from_utf16_lossy(&desc.Description[..end]),
            luid_low: desc.AdapterLuid.LowPart,
            luid_high: desc.AdapterLuid.HighPart,
            vendor_id: desc.VendorId,
        };
        Ok((device, context, info))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FrameInfo {
    pub pts: i64,
    pub seconds: f64,
    pub time_base: (i32, i32),
    pub rate: (i32, i32),
    pub visible_size: (u32, u32),
    pub coded_size: (u32, u32),
    pub crop: (u32, u32, u32, u32),
    pub array_slice: u32,
    pub array_size: u32,
    pub color_range: i32,
    pub color_matrix: i32,
    pub color_primaries: i32,
    pub color_transfer: i32,
    pub sample_aspect_ratio: (i32, i32),
}
impl FrameInfo {
    pub fn require_bt709(&self) -> Result<()> {
        ensure!(
            self.color_range == ffi::AVColorRange_AVCOL_RANGE_MPEG
                && self.color_matrix == ffi::AVColorSpace_AVCOL_SPC_BT709
                && self.color_primaries == ffi::AVColorPrimaries_AVCOL_PRI_BT709
                && self.color_transfer == ffi::AVColorTransferCharacteristic_AVCOL_TRC_BT709,
            "Conversion requires explicit limited-range BT.709 SDR metadata; actual frame: {}",
            serde_json::to_string(self)?
        );
        ensure!(
            self.sample_aspect_ratio == (1, 1),
            "Conversion proof requires explicit square pixels; actual SAR {:?}",
            self.sample_aspect_ratio
        );
        Ok(())
    }
    pub fn source_rect(&self) -> RECT {
        RECT {
            left: self.crop.0 as i32,
            top: self.crop.1 as i32,
            right: (self.crop.0 + self.visible_size.0) as i32,
            bottom: (self.crop.1 + self.visible_size.1) as i32,
        }
    }
}
pub struct DecodedFrame {
    pub texture: ID3D11Texture2D,
    pub info: FrameInfo,
    _owned: RawFrame,
    _worker: PhantomData<Rc<()>>,
}
struct RawFrame {
    raw: *mut ffi::AVFrame,
    api: Arc<Api>,
}
impl Drop for RawFrame {
    fn drop(&mut self) {
        unsafe { (self.api.av_frame_free)(&mut self.raw) }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct OpenTimings {
    pub source_validation_ms: f64,
    pub dll_load_ms: f64,
    pub file_open_ms: f64,
    pub stream_info_ms: f64,
    pub device_creation_ms: f64,
    pub hardware_context_ms: f64,
    pub codec_open_ms: f64,
    pub total_ms: f64,
}
#[derive(Clone, Copy)]
pub struct TransportOptions {
    pub timeout: Duration,
    pub source_in_ns: u64,
    pub stream: u32,
}
pub struct Decoder {
    api: Arc<Api>,
    format: *mut ffi::AVFormatContext,
    codec: *mut ffi::AVCodecContext,
    packet: *mut ffi::AVPacket,
    hardware: *mut ffi::AVBufferRef,
    interrupt: Box<Interrupt>,
    stream: i32,
    base: ffi::AVRational,
    rate: ffi::AVRational,
    draining: bool,
    frames: usize,
    packets: usize,
    packet_bytes: u64,
    last_pts: Option<i64>,
    transport: Option<TransportOptions>,
    source_start: f64,
    pub source: PathBuf,
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub open_timings: OpenTimings,
    _worker: PhantomData<Rc<()>>,
}
struct Demuxer {
    raw: *mut ffi::AVFormatContext,
    api: Arc<Api>,
}
impl Demuxer {
    fn take(&mut self) -> *mut ffi::AVFormatContext {
        std::mem::replace(&mut self.raw, ptr::null_mut())
    }
}
impl Drop for Demuxer {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { (self.api.avformat_close_input)(&mut self.raw) }
        }
    }
}
// Owns the opened source before any D3D device is created. Header rejection and
// cancellation close this same demuxer; accepted inputs are moved into Decoder.
struct PreparedInput {
    api: Arc<Api>,
    format: Demuxer,
    interrupt: Box<Interrupt>,
    stream: i32,
    base: ffi::AVRational,
    rate: ffi::AVRational,
    stream_sar: ffi::AVRational,
    source_start: f64,
    source: PathBuf,
    transport: Option<TransportOptions>,
    parameters: *const ffi::AVCodecParameters,
    selected_codec: *const ffi::AVCodec,
    open_timings: OpenTimings,
}
impl PreparedInput {
    fn open(
        source: &Path,
        dlls: &Path,
        cancel: Arc<AtomicBool>,
        timeout: Duration,
        transport: Option<TransportOptions>,
    ) -> Result<Self> {
        let open_started = Instant::now();
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(30),
            "Proof deadline must be within 1ns..=30s"
        );
        let interrupt = Box::new(Interrupt {
            cancel,
            deadline: Instant::now() + timeout,
        });
        interrupt.check()?;
        let source = source
            .canonicalize()
            .context("Local proof source is unavailable")?;
        ensure!(
            matches!(source.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))),
            "Proof accepts local drive files only, not UNC or network URLs"
        );
        ensure!(source.is_file(), "Proof source is not a regular file");
        let source_validation_ms = open_started.elapsed().as_secs_f64() * 1000.;
        let stage = Instant::now();
        let api = Api::load(dlls, &interrupt.cancel)?;
        let dll_load_ms = stage.elapsed().as_secs_f64() * 1000.;
        let mut result = Self {
            format: Demuxer {
                raw: ptr::null_mut(),
                api: api.clone(),
            },
            api,
            interrupt,
            stream: -1,
            base: ffi::AVRational { num: 0, den: 0 },
            rate: ffi::AVRational { num: 0, den: 0 },
            stream_sar: ffi::AVRational { num: 0, den: 1 },
            source_start: 0.,
            source,
            transport,
            parameters: ptr::null(),
            selected_codec: ptr::null(),
            open_timings: OpenTimings {
                source_validation_ms,
                dll_load_ms,
                ..Default::default()
            },
        };
        unsafe {
            result.initialize_headers()?;
        }
        result.open_timings.total_ms = open_started.elapsed().as_secs_f64() * 1000.;
        Ok(result)
    }
    fn check(&self, status: c_int, stage: &str) -> Result<()> {
        self.interrupt.check()?;
        ensure!(
            status >= 0,
            "{stage}: {} ({status})",
            self.api.error(status)
        );
        Ok(())
    }
    unsafe fn initialize_headers(&mut self) -> Result<()> {
        let file_open_started = Instant::now();
        let a = self.api.clone();
        self.format.raw = (a.avformat_alloc_context)();
        ensure!(!self.format.raw.is_null(), "Cannot allocate proof demuxer");
        (*self.format.raw).interrupt_callback = ffi::AVIOInterruptCB {
            callback: Some(interrupt),
            opaque: (&mut *self.interrupt as *mut Interrupt).cast(),
        };
        (*self.format.raw).probesize = 8 * 1024 * 1024;
        (*self.format.raw).max_analyze_duration = 2_000_000;
        (*self.format.raw).max_streams = 128;
        (*self.format.raw).max_probe_packets = 256;
        let path = self
            .source
            .to_str()
            .context("Proof source path is not UTF-8")?;
        let path = CString::new(path.strip_prefix("\\\\?\\").unwrap_or(path))?;
        let mut options = ptr::null_mut();
        let configured = (|| -> Result<()> {
            self.check(
                (a.av_dict_set)(
                    &mut options,
                    c"protocol_whitelist".as_ptr(),
                    c"file".as_ptr(),
                    0,
                ),
                "Set local protocol whitelist",
            )?;
            self.check(
                (a.av_dict_set)(
                    &mut options,
                    c"format_whitelist".as_ptr(),
                    c"matroska,webm,mov,ivf".as_ptr(),
                    0,
                ),
                "Set proof format whitelist",
            )?;
            let opened = (a.avformat_open_input)(
                &mut self.format.raw,
                path.as_ptr(),
                ptr::null(),
                &mut options,
            );
            self.check(opened, "Open local source")
        })();
        (a.av_dict_free)(&mut options);
        configured?;
        self.open_timings.file_open_ms = file_open_started.elapsed().as_secs_f64() * 1000.;
        let stage = Instant::now();
        self.check(
            (a.avformat_find_stream_info)(self.format.raw, ptr::null_mut()),
            "Read source stream headers",
        )?;
        self.open_timings.stream_info_ms = stage.elapsed().as_secs_f64() * 1000.;
        ensure!(
            (*self.format.raw).nb_streams <= 128 && !(*self.format.raw).streams.is_null(),
            "Invalid stream table"
        );
        let mut parameters = ptr::null();
        for index in 0..(*self.format.raw).nb_streams as usize {
            let stream = *(*self.format.raw).streams.add(index);
            ensure!(!stream.is_null(), "Missing stream");
            let p = (*stream).codecpar;
            if !p.is_null() && (*p).codec_type == ffi::AVMediaType_AVMEDIA_TYPE_VIDEO {
                if self
                    .transport
                    .is_some_and(|options| options.stream != index as u32)
                {
                    continue;
                }
                if (*p).codec_id != ffi::AVCodecID_AV_CODEC_ID_AV1 {
                    return Err(GpuInputIneligible(
                        "GPU playback requires AV1 video; this codec uses CPU preview".into(),
                    )
                    .into());
                }
                self.stream = index as i32;
                self.base = (*stream).time_base;
                self.rate = (*stream).avg_frame_rate;
                self.stream_sar = (*stream).sample_aspect_ratio;
                if (*stream).start_time != NO_PTS {
                    self.source_start = (*stream).start_time as f64 * f64::from(self.base.num)
                        / f64::from(self.base.den);
                }
                parameters = p;
                break;
            }
        }
        ensure!(!parameters.is_null(), "No AV1 video stream");
        ensure!(
            self.base.num > 0 && self.base.den > 0,
            "Invalid stream time base"
        );
        let codec = (a.avcodec_find_decoder_by_name)(c"av1".as_ptr());
        ensure!(!codec.is_null(), "FFmpeg native AV1 decoder is unavailable");
        let mut supported = false;
        for index in 0..64 {
            let config = (a.avcodec_get_hw_config)(codec, index);
            if config.is_null() {
                break;
            }
            supported |= (*config).device_type == ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_D3D11VA
                && (*config).pix_fmt == ffi::AVPixelFormat_AV_PIX_FMT_D3D11
                && ((*config).methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) != 0;
        }
        ensure!(
            supported,
            "Installed native AV1 decoder does not advertise D3D11VA hardware device support"
        );
        self.parameters = parameters;
        self.selected_codec = codec;
        Ok(())
    }
    fn require_transport_eligibility(&self, size: (u32, u32), fps: u32) -> Result<()> {
        let p = unsafe { &*self.parameters };
        HeaderEligibility {
            size: (p.width, p.height),
            pixel_format: p.format,
            range: p.color_range,
            matrix: p.color_space,
            primaries: p.color_primaries,
            transfer: p.color_trc,
            sar: (p.sample_aspect_ratio.num, p.sample_aspect_ratio.den),
            stream_sar: (self.stream_sar.num, self.stream_sar.den),
            rate: (self.rate.num, self.rate.den),
        }
        .validate(size, fps)
        .map_err(|error| GpuInputIneligible(error.to_string()).into())
    }
}
struct HeaderEligibility {
    size: (i32, i32),
    pixel_format: i32,
    range: i32,
    matrix: i32,
    primaries: i32,
    transfer: i32,
    sar: (i32, i32),
    stream_sar: (i32, i32),
    rate: (i32, i32),
}
impl HeaderEligibility {
    fn validate(&self, size: (u32, u32), fps: u32) -> Result<()> {
        ensure!(
            self.size.0 > 0
                && self.size.1 > 0
                && self.size.0 <= 3840
                && self.size.1 <= 2160
                && (self.size.0 as u32, self.size.1 as u32) == size,
            "GPU source geometry uses CPU fallback"
        );
        ensure!(
            matches!(
                self.pixel_format,
                ffi::AVPixelFormat_AV_PIX_FMT_YUV420P | ffi::AVPixelFormat_AV_PIX_FMT_NV12
            ),
            "GPU source requires known 8-bit 4:2:0 metadata"
        );
        ensure!(
            self.range == ffi::AVColorRange_AVCOL_RANGE_MPEG
                && self.matrix == ffi::AVColorSpace_AVCOL_SPC_BT709
                && self.primaries == ffi::AVColorPrimaries_AVCOL_PRI_BT709
                && self.transfer == ffi::AVColorTransferCharacteristic_AVCOL_TRC_BT709,
            "GPU source requires explicit limited-range BT.709 SDR metadata"
        );
        require_square_sar(self.sar, self.stream_sar)?;
        ensure!(
            fps > 0
                && self.rate.1 > 0
                && i64::from(self.rate.0) == i64::from(fps) * i64::from(self.rate.1),
            "GPU source frame-rate conversion uses CPU fallback"
        );
        Ok(())
    }
}
fn require_square_sar(codec: (i32, i32), stream: (i32, i32)) -> Result<()> {
    let known_square = |ratio: (i32, i32)| ratio.0 > 0 && ratio.1 > 0 && ratio.0 == ratio.1;
    let unknown = |ratio: (i32, i32)| ratio.0 == 0 && ratio.1 >= 0;
    ensure!((unknown(codec) || known_square(codec))
        && (unknown(stream) || known_square(stream))
        && (known_square(codec) || known_square(stream)),
        "GPU source requires explicit consistent square pixels (codec {codec:?}, stream {stream:?})");
    Ok(())
}
impl Decoder {
    pub fn open(
        source: &Path,
        dlls: &Path,
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        cancel: Arc<AtomicBool>,
        timeout: Duration,
    ) -> Result<Self> {
        let input = PreparedInput::open(source, dlls, cancel, timeout, None)?;
        Self::from_input(input, device, context)
    }
    pub fn open_transport(
        source: &Path,
        dlls: &Path,
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        cancel: Arc<AtomicBool>,
        options: TransportOptions,
    ) -> Result<Self> {
        let input = PreparedInput::open(source, dlls, cancel, options.timeout, Some(options))?;
        Self::from_input(input, device, context)
    }
    /// Production setup rejects ineligible source headers before invoking the
    /// device factory, without probing/opening the source a second time.
    pub fn open_transport_lazy(
        source: &Path,
        dlls: &Path,
        cancel: Arc<AtomicBool>,
        options: TransportOptions,
        expected: ((u32, u32), u32),
        device_factory: impl FnOnce() -> Result<(ID3D11Device, ID3D11DeviceContext)>,
    ) -> Result<Self> {
        let started = Instant::now();
        let input = PreparedInput::open(source, dlls, cancel, options.timeout, Some(options))?;
        input.require_transport_eligibility(expected.0, expected.1)?;
        input.interrupt.check()?;
        let device_started = Instant::now();
        let (device, context) = device_factory()?;
        let device_creation_ms = device_started.elapsed().as_secs_f64() * 1000.;
        input.interrupt.check()?;
        let mut decoder = Self::from_input(input, device, context)?;
        decoder.open_timings.device_creation_ms = device_creation_ms;
        decoder.open_timings.total_ms = started.elapsed().as_secs_f64() * 1000.;
        Ok(decoder)
    }
    fn from_input(
        mut input: PreparedInput,
        device: ID3D11Device,
        context: ID3D11DeviceContext,
    ) -> Result<Self> {
        let started = Instant::now();
        input.interrupt.check()?;
        ensure!(
            unsafe { context.GetDevice()? }.cast::<IUnknown>()? == device.cast::<IUnknown>()?,
            "Decoder device and immediate context do not match"
        );
        let parameters = input.parameters;
        let selected_codec = input.selected_codec;
        let mut result = Self {
            format: input.format.take(),
            api: input.api,
            interrupt: input.interrupt,
            stream: input.stream,
            base: input.base,
            rate: input.rate,
            source_start: input.source_start,
            source: input.source,
            transport: input.transport,
            open_timings: input.open_timings,
            codec: ptr::null_mut(),
            packet: ptr::null_mut(),
            hardware: ptr::null_mut(),
            draining: false,
            frames: 0,
            packets: 0,
            packet_bytes: 0,
            last_pts: None,
            device,
            context,
            _worker: PhantomData,
        };
        unsafe {
            result.initialize_hardware(selected_codec, parameters)?;
        }
        result.open_timings.total_ms += started.elapsed().as_secs_f64() * 1000.;
        Ok(result)
    }
    fn check(&self, status: c_int, stage: &str) -> Result<()> {
        self.interrupt.check()?;
        ensure!(
            status >= 0,
            "{stage}: {} ({status})",
            self.api.error(status)
        );
        Ok(())
    }
    unsafe fn initialize_hardware(
        &mut self,
        codec: *const ffi::AVCodec,
        parameters: *const ffi::AVCodecParameters,
    ) -> Result<()> {
        let a = self.api.clone();
        let stage = Instant::now();
        self.hardware = (a.av_hwdevice_ctx_alloc)(ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_D3D11VA);
        ensure!(
            !self.hardware.is_null() && !(*self.hardware).data.is_null(),
            "Cannot allocate D3D11VA hardware context"
        );
        let hardware = (*self.hardware).data.cast::<ffi::AVHWDeviceContext>();
        let d3d = (*hardware).hwctx.cast::<ffi::AVD3D11VADeviceContext>();
        ensure!(!d3d.is_null(), "Missing D3D11VA device context");
        // FFmpeg always releases supplied COM interfaces. Give it independent
        // owned references; Rust retains its original device/context references.
        (*d3d).device = self.device.clone().into_raw().cast();
        (*d3d).device_context = self.context.clone().into_raw().cast();
        self.check(
            (a.av_hwdevice_ctx_init)(self.hardware),
            "Initialize D3D11VA device context",
        )?;
        ensure!(
            (*d3d).lock.is_some() && (*d3d).unlock.is_some(),
            "FFmpeg did not install recursive context locking"
        );
        self.open_timings.hardware_context_ms = stage.elapsed().as_secs_f64() * 1000.;
        self.codec = (a.avcodec_alloc_context3)(codec);
        ensure!(!self.codec.is_null(), "Cannot allocate AV1 decoder");
        self.check(
            (a.avcodec_parameters_to_context)(self.codec, parameters),
            "Configure AV1 source metadata",
        )?;
        (*self.codec).get_format = Some(d3d_format);
        (*self.codec).pkt_timebase = self.base;
        (*self.codec).thread_count = 1;
        if self.transport.is_some() {
            // Caller may hold three future frames plus three in-flight GPU reads.
            // FFmpeg requires this reservation before avcodec_open2 (avcodec.h).
            (*self.codec).extra_hw_frames = 6;
            ensure!(
                (*self.codec).width > 0
                    && (*self.codec).height > 0
                    && (*self.codec).width <= 3840
                    && (*self.codec).height <= 2160,
                "GPU transport source exceeds the bounded 4K SDR pool budget"
            );
        }
        (*self.codec).hw_device_ctx = (a.av_buffer_ref)(self.hardware);
        ensure!(
            !(*self.codec).hw_device_ctx.is_null(),
            "Cannot retain hardware context for decoder"
        );
        let stage = Instant::now();
        self.check(
            (a.avcodec_open2)(self.codec, codec, ptr::null_mut()),
            "Open hardware AV1 decoder",
        )?;
        self.open_timings.codec_open_ms = stage.elapsed().as_secs_f64() * 1000.;
        self.packet = (a.av_packet_alloc)();
        ensure!(!self.packet.is_null(), "Cannot allocate input packet");
        if let Some(options) = self.transport {
            self.interrupt.deadline = Instant::now() + options.timeout;
            if options.source_in_ns > 0 {
                let target = (options.source_in_ns as f64 / 1_000_000_000. + self.source_start)
                    * f64::from(self.base.den)
                    / f64::from(self.base.num);
                ensure!(
                    target.is_finite() && target >= i64::MIN as f64 && target < i64::MAX as f64,
                    "GPU source seek timestamp is out of range"
                );
                self.check(
                    (a.av_seek_frame)(
                        self.format,
                        self.stream,
                        target.round() as i64,
                        SEEK_BACKWARD,
                    ),
                    "Seek GPU source keyframe",
                )?;
                (a.avcodec_flush_buffers)(self.codec);
            }
        }
        Ok(())
    }
    pub fn source_start_seconds(&self) -> f64 {
        self.source_start
    }
    /// External converter/query calls must share FFmpeg's recursive context lock.
    pub fn with_context_lock<T>(&self, operation: impl FnOnce() -> T) -> T {
        struct Unlock {
            f: unsafe extern "C" fn(*mut c_void),
            opaque: *mut c_void,
        }
        impl Drop for Unlock {
            fn drop(&mut self) {
                unsafe { (self.f)(self.opaque) }
            }
        }
        unsafe {
            let hardware = (*self.hardware).data.cast::<ffi::AVHWDeviceContext>();
            let d3d = &*(*hardware).hwctx.cast::<ffi::AVD3D11VADeviceContext>();
            (d3d.lock.expect("initialized context lock"))(d3d.lock_ctx);
            let _unlock = Unlock {
                f: d3d.unlock.expect("initialized context unlock"),
                opaque: d3d.lock_ctx,
            };
            operation()
        }
    }
    pub fn next(&mut self) -> Result<Option<DecodedFrame>> {
        if let Some(options) = self.transport {
            self.interrupt.deadline = Instant::now() + options.timeout;
        }
        self.interrupt.check()?;
        if self.transport.is_none() && self.frames >= MAX_FRAMES {
            return Ok(None);
        }
        let operation_packets = self.packets;
        let operation_bytes = self.packet_bytes;
        let raw = unsafe { (self.api.av_frame_alloc)() };
        ensure!(!raw.is_null(), "Cannot allocate decoded frame");
        let frame = RawFrame {
            raw,
            api: self.api.clone(),
        };
        loop {
            self.interrupt.check()?;
            let received = unsafe { (self.api.avcodec_receive_frame)(self.codec, raw) };
            if received >= 0 {
                let frame = unsafe { self.describe(frame)? };
                self.frames += 1;
                return Ok(Some(frame));
            }
            if received == EOF {
                return Ok(None);
            }
            ensure!(
                received == AGAIN,
                "Receive AV1 frame: {} ({received})",
                self.api.error(received)
            );
            ensure!(
                !self.draining,
                "Decoder requested more input after final drain"
            );
            loop {
                self.interrupt.check()?;
                unsafe {
                    (self.api.av_packet_unref)(self.packet);
                }
                let read = unsafe { (self.api.av_read_frame)(self.format, self.packet) };
                self.interrupt.check()?;
                if read == EOF {
                    self.check(
                        unsafe { (self.api.avcodec_send_packet)(self.codec, ptr::null()) },
                        "Drain AV1 decoder",
                    )?;
                    self.draining = true;
                    break;
                }
                self.check(read, "Read local packet")?;
                self.packets += 1;
                let packet = unsafe { &*self.packet };
                ensure!(
                    (0..=64 * 1024 * 1024).contains(&packet.size),
                    "Invalid or oversized proof packet"
                );
                self.packet_bytes += packet.size as u64;
                ensure!(
                    if self.transport.is_some() {
                        self.packets - operation_packets <= 4096
                            && self.packet_bytes - operation_bytes <= 64 * 1024 * 1024
                    } else {
                        self.packets <= 32768 && self.packet_bytes <= 256 * 1024 * 1024
                    },
                    "Proof packet budget exceeded"
                );
                if packet.stream_index != self.stream {
                    continue;
                }
                let sent = unsafe { (self.api.avcodec_send_packet)(self.codec, self.packet) };
                unsafe {
                    (self.api.av_packet_unref)(self.packet);
                }
                self.check(sent, "Submit AV1 packet")?;
                break;
            }
        }
    }
    unsafe fn describe(&mut self, frame: RawFrame) -> Result<DecodedFrame> {
        let raw = &*frame.raw;
        ensure!(
            raw.format == ffi::AVPixelFormat_AV_PIX_FMT_D3D11,
            "Decoder returned a CPU frame; proof refuses fallback"
        );
        ensure!(
            !raw.data[0].is_null() && !raw.hw_frames_ctx.is_null(),
            "Missing hardware surface ownership"
        );
        let hardware_frames = (*raw.hw_frames_ctx).data.cast::<ffi::AVHWFramesContext>();
        ensure!(
            !hardware_frames.is_null()
                && (*hardware_frames).sw_format == ffi::AVPixelFormat_AV_PIX_FMT_NV12,
            "Proof only accepts 8-bit NV12 hardware output"
        );
        let texture_raw = raw.data[0].cast::<c_void>();
        let texture = ID3D11Texture2D::from_raw_borrowed(&texture_raw)
            .context("Invalid D3D11 texture pointer")?
            .clone();
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        if self.transport.is_some() {
            // AV1 internal references plus six caller-retained surfaces. Refuse
            // undersized/unexpected pools before queuing any retained frames.
            ensure!(
                (18..=32).contains(&desc.ArraySize),
                "GPU transport pool {} does not meet the bounded 18..32 surface budget",
                desc.ArraySize
            );
        }
        let slice = raw.data[1] as usize;
        ensure!(
            slice < desc.ArraySize as usize
                && desc.Format == DXGI_FORMAT_NV12
                && desc.MipLevels == 1
                && desc.SampleDesc.Count == 1
                && desc.Usage == D3D11_USAGE_DEFAULT
                && desc.CPUAccessFlags == 0,
            "Decoded texture format/array/usage is unsupported"
        );
        ensure!(
            texture.GetDevice()?.cast::<IUnknown>()? == self.device.cast::<IUnknown>()?,
            "Decoded surface belongs to a different D3D11 device"
        );
        ensure!(
            raw.width > 0
                && raw.height > 0
                && raw.width <= 16384
                && raw.height <= 16384
                && raw.width as u32 <= desc.Width
                && raw.height as u32 <= desc.Height,
            "Invalid decoded visible dimensions"
        );
        let width = raw.width as usize;
        let height = raw.height as usize;
        ensure!(
            raw.crop_left < width
                && raw.crop_right < width - raw.crop_left
                && raw.crop_top < height
                && raw.crop_bottom < height - raw.crop_top,
            "Invalid frame crop"
        );
        let visible = (
            width - raw.crop_left - raw.crop_right,
            height - raw.crop_top - raw.crop_bottom,
        );
        ensure!(
            [raw.crop_left, raw.crop_top, visible.0, visible.1]
                .iter()
                .all(|v| v % 2 == 0),
            "NV12 crop is not chroma aligned"
        );
        ensure!(
            raw.flags & ffi::AV_FRAME_FLAG_INTERLACED as i32 == 0 && raw.interlaced_frame == 0,
            "Interlaced sources are outside this proof"
        );
        let pts = if raw.pts != NO_PTS {
            raw.pts
        } else {
            raw.best_effort_timestamp
        };
        ensure!(
            pts != NO_PTS && self.last_pts.is_none_or(|last| pts > last),
            "Decoded PTS missing or not strictly increasing"
        );
        let seconds = pts as f64 * f64::from(self.base.num) / f64::from(self.base.den);
        ensure!(seconds.is_finite(), "Invalid decoded timestamp");
        self.last_pts = Some(pts);
        let info = FrameInfo {
            pts,
            seconds,
            time_base: (self.base.num, self.base.den),
            rate: (self.rate.num, self.rate.den),
            visible_size: (visible.0 as u32, visible.1 as u32),
            coded_size: (desc.Width, desc.Height),
            crop: (
                raw.crop_left as u32,
                raw.crop_top as u32,
                raw.crop_right as u32,
                raw.crop_bottom as u32,
            ),
            array_slice: slice as u32,
            array_size: desc.ArraySize,
            color_range: raw.color_range,
            color_matrix: raw.colorspace,
            color_primaries: raw.color_primaries,
            color_transfer: raw.color_trc,
            sample_aspect_ratio: (raw.sample_aspect_ratio.num, raw.sample_aspect_ratio.den),
        };
        Ok(DecodedFrame {
            texture,
            info,
            _owned: frame,
            _worker: PhantomData,
        })
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            (self.api.avcodec_free_context)(&mut self.codec);
            (self.api.av_packet_free)(&mut self.packet);
            (self.api.avformat_close_input)(&mut self.format);
            (self.api.av_buffer_unref)(&mut self.hardware);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_stream_square_sar_fills_unknown_codec_without_guessing() {
        assert!(require_square_sar((0, 1), (1, 1)).is_ok());
        assert!(require_square_sar((1, 1), (0, 1)).is_ok());
        assert!(require_square_sar((1, 1), (1, 1)).is_ok());
        for (codec, stream) in [
            ((0, 1), (0, 1)),
            ((0, 0), (0, 0)),
            ((1, 1), (4, 3)),
            ((4, 3), (1, 1)),
            ((0, 1), (4, 3)),
            ((-1, 1), (1, 1)),
            ((1, 0), (1, 1)),
        ] {
            assert!(
                require_square_sar(codec, stream).is_err(),
                "{codec:?} / {stream:?}"
            );
        }
    }
    #[test]
    fn header_eligibility_rejects_common_fallbacks_before_device_creation() {
        fn valid() -> HeaderEligibility {
            HeaderEligibility {
                size: (2560, 1440),
                pixel_format: ffi::AVPixelFormat_AV_PIX_FMT_YUV420P,
                range: 1,
                matrix: 1,
                primaries: 1,
                transfer: 1,
                sar: (1, 1),
                stream_sar: (1, 1),
                rate: (60, 1),
            }
        }
        assert!(valid().validate((2560, 1440), 60).is_ok());
        for case in 0..10 {
            let mut metadata = valid();
            match case {
                0 => metadata.size = (0, 1440),
                1 => metadata.size = (7680, 4320),
                2 => metadata.pixel_format = ffi::AVPixelFormat_AV_PIX_FMT_YUV420P10LE,
                3 => metadata.range = 0,
                4 => metadata.matrix = 2,
                5 => metadata.primaries = 2,
                6 => metadata.transfer = 2,
                7 => {
                    metadata.sar = (0, 1);
                    metadata.stream_sar = (0, 1);
                }
                8 => metadata.rate = (60000, 1001),
                _ => metadata.rate = (60, 0),
            }
            assert!(metadata.validate((2560, 1440), 60).is_err(), "case {case}");
        }
        assert!(valid().validate((1280, 720), 60).is_err());
        assert!(valid().validate((2560, 1440), 0).is_err());
    }
    #[test]
    fn callback_accepts_only_d3d11_hardware_format() {
        let cpu = [
            ffi::AVPixelFormat_AV_PIX_FMT_NV12,
            ffi::AVPixelFormat_AV_PIX_FMT_NONE,
        ];
        let gpu = [
            ffi::AVPixelFormat_AV_PIX_FMT_NV12,
            ffi::AVPixelFormat_AV_PIX_FMT_D3D11,
            ffi::AVPixelFormat_AV_PIX_FMT_NONE,
        ];
        assert_eq!(
            unsafe { d3d_format(ptr::null_mut(), cpu.as_ptr()) },
            ffi::AVPixelFormat_AV_PIX_FMT_NONE
        );
        assert_eq!(
            unsafe { d3d_format(ptr::null_mut(), gpu.as_ptr()) },
            ffi::AVPixelFormat_AV_PIX_FMT_D3D11
        );
    }
    #[test]
    fn interrupt_reports_cancellation_and_deadline_without_media() {
        let cancel = Arc::new(AtomicBool::new(false));
        let state = Interrupt {
            cancel: cancel.clone(),
            deadline: Instant::now() + Duration::from_secs(1),
        };
        assert_eq!(state.reason(), None);
        cancel.store(true, Ordering::Release);
        assert_eq!(state.reason(), Some(StopReason::Cancelled));
        let expired = Interrupt {
            cancel: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now(),
        };
        assert_eq!(expired.reason(), Some(StopReason::Deadline));
    }
    #[test]
    fn conversion_rejects_unspecified_color_and_sar_instead_of_guessing() {
        let mut info = FrameInfo {
            pts: 0,
            seconds: 0.,
            time_base: (1, 1000),
            rate: (60, 1),
            visible_size: (128, 64),
            coded_size: (128, 64),
            crop: (0, 0, 0, 0),
            array_slice: 0,
            array_size: 1,
            color_range: 1,
            color_matrix: 1,
            color_primaries: 1,
            color_transfer: 1,
            sample_aspect_ratio: (1, 1),
        };
        assert!(info.require_bt709().is_ok());
        for field in 0..4 {
            let mut unknown = info.clone();
            match field {
                0 => unknown.color_range = 0,
                1 => unknown.color_matrix = 2,
                2 => unknown.color_primaries = 2,
                _ => unknown.color_transfer = 2,
            }
            assert!(unknown.require_bt709().is_err());
        }
        info.sample_aspect_ratio = (0, 1);
        assert!(info.require_bt709().is_err());
    }
}
