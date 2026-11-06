//! Worker-confined raw PCM decoding for the editor's conservative native path.
//! Filtering, limiting, track mixing and physical output belong to the caller.
use crate::{
    ffmpeg_runtime::{load_runtime, FfmpegRuntimeConfig, FfmpegRuntimeRequirement, Runtime},
    playback_gpu::decode::ffi,
};
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::VecDeque,
    ffi::{c_char, c_int, c_void, CStr, CString},
    marker::PhantomData,
    path::{Component, Path, Prefix},
    ptr,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const RATE: i64 = 48_000;
const AGAIN: c_int = -11;
const EOF: c_int = -541478725;
const NO_PTS: i64 = i64::MIN;
const BLOCK: usize = 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
#[derive(Debug)]
pub(super) struct UnsupportedNativeAudio(pub &'static str);
impl std::fmt::Display for UnsupportedNativeAudio {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { formatter.write_str(self.0) }
}
impl std::error::Error for UnsupportedNativeAudio {}

macro_rules! api {
    ($($component:literal => $name:ident: $signature:ty),* $(,)?) => {
        #[allow(non_snake_case)]
        struct Api { $($name: $signature,)* _runtime: Arc<Runtime> }
        impl Api {
            fn load(cancel: &AtomicBool) -> Result<Self> {
                let runtime = load_runtime(&FfmpegRuntimeConfig::Automatic,
                    FfmpegRuntimeRequirement::Gpu702, false, || cancel.load(Ordering::Acquire))?;
                Ok(Self {
                    $($name: unsafe { runtime.symbol::<$signature>($component, stringify!($name).as_bytes())? },)*
                    _runtime: runtime,
                })
            }
            fn check(&self, result: c_int, operation: &str) -> Result<()> {
                if result >= 0 { return Ok(()); }
                let mut message = [0_i8; 256];
                let status = unsafe { (self.av_strerror)(result, message.as_mut_ptr(), message.len()) };
                let message = if status >= 0 {
                    unsafe { CStr::from_ptr(message.as_ptr()) }.to_string_lossy().into_owned()
                } else { format!("FFmpeg error {result}") };
                bail!("Native audio {operation}: {message}")
            }
        }
    }
}
api! {
    0 => av_strerror: unsafe extern "C" fn(c_int, *mut c_char, usize) -> c_int,
    0 => av_frame_alloc: unsafe extern "C" fn() -> *mut ffi::AVFrame,
    0 => av_frame_free: unsafe extern "C" fn(*mut *mut ffi::AVFrame),
    0 => av_frame_unref: unsafe extern "C" fn(*mut ffi::AVFrame),
    0 => av_dict_set: unsafe extern "C" fn(*mut *mut ffi::AVDictionary, *const c_char, *const c_char, c_int) -> c_int,
    0 => av_dict_free: unsafe extern "C" fn(*mut *mut ffi::AVDictionary),
    1 => avcodec_find_decoder: unsafe extern "C" fn(ffi::AVCodecID) -> *const ffi::AVCodec,
    1 => avcodec_alloc_context3: unsafe extern "C" fn(*const ffi::AVCodec) -> *mut ffi::AVCodecContext,
    1 => avcodec_parameters_to_context: unsafe extern "C" fn(*mut ffi::AVCodecContext, *const ffi::AVCodecParameters) -> c_int,
    1 => avcodec_open2: unsafe extern "C" fn(*mut ffi::AVCodecContext, *const ffi::AVCodec, *mut *mut ffi::AVDictionary) -> c_int,
    1 => avcodec_send_packet: unsafe extern "C" fn(*mut ffi::AVCodecContext, *const ffi::AVPacket) -> c_int,
    1 => avcodec_receive_frame: unsafe extern "C" fn(*mut ffi::AVCodecContext, *mut ffi::AVFrame) -> c_int,
    1 => avcodec_free_context: unsafe extern "C" fn(*mut *mut ffi::AVCodecContext),
    1 => av_packet_alloc: unsafe extern "C" fn() -> *mut ffi::AVPacket,
    1 => av_packet_clone: unsafe extern "C" fn(*const ffi::AVPacket) -> *mut ffi::AVPacket,
    1 => av_packet_unref: unsafe extern "C" fn(*mut ffi::AVPacket),
    1 => av_packet_free: unsafe extern "C" fn(*mut *mut ffi::AVPacket),
    2 => avformat_alloc_context: unsafe extern "C" fn() -> *mut ffi::AVFormatContext,
    2 => avformat_open_input: unsafe extern "C" fn(*mut *mut ffi::AVFormatContext, *const c_char, *const ffi::AVInputFormat, *mut *mut ffi::AVDictionary) -> c_int,
    2 => avformat_close_input: unsafe extern "C" fn(*mut *mut ffi::AVFormatContext),
    2 => av_read_frame: unsafe extern "C" fn(*mut ffi::AVFormatContext, *mut ffi::AVPacket) -> c_int,
}

struct Interrupt {
    cancel: Arc<AtomicBool>,
    deadline: Instant,
}
impl Interrupt {
    fn check(&self) -> Result<()> {
        ensure!(
            !self.cancel.load(Ordering::Acquire),
            "Native audio cancelled"
        );
        ensure!(
            Instant::now() < self.deadline,
            "Native audio input timed out"
        );
        Ok(())
    }
}
unsafe extern "C" fn interrupt(opaque: *mut c_void) -> c_int {
    if opaque.is_null() {
        return 1;
    }
    i32::from(unsafe { (&*opaque.cast::<Interrupt>()).check().is_err() })
}

struct Decoder {
    api: Api,
    format: *mut ffi::AVFormatContext,
    codec: *mut ffi::AVCodecContext,
    packet: *mut ffi::AVPacket,
    frame: *mut ffi::AVFrame,
    interrupt: Box<Interrupt>,
    stream: c_int,
    time_base: ffi::AVRational,
    origin_sample: i64,
    pcm: bool,
    pending: VecDeque<*mut ffi::AVPacket>,
    // FFmpeg objects and callbacks never move to a different worker.
    _worker: PhantomData<Rc<()>>,
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            for mut packet in self.pending.drain(..) {
                (self.api.av_packet_free)(&mut packet);
            }
            (self.api.av_frame_free)(&mut self.frame);
            (self.api.av_packet_free)(&mut self.packet);
            (self.api.avcodec_free_context)(&mut self.codec);
            (self.api.avformat_close_input)(&mut self.format);
        }
    }
}

fn rescale(value: i64, numerator: i64, denominator: i64) -> Result<i64> {
    ensure!(
        numerator > 0 && denominator > 0,
        "Native audio has invalid time base"
    );
    let value = i128::from(value) * i128::from(numerator);
    let denominator = i128::from(denominator);
    let rounded = if value < 0 {
        -((-value + denominator / 2) / denominator)
    } else {
        (value + denominator / 2) / denominator
    };
    i64::try_from(rounded).context("Native audio timestamp is out of range")
}

fn timestamp_tolerance(base: ffi::AVRational) -> Result<u64> {
    ensure!(
        base.num > 0 && base.den > 0,
        "Native audio has invalid time base"
    );
    // Demux timestamps may round or floor a continuous PCM sample grid to whole
    // milliseconds. Legacy RCR files floor, requiring one full timestamp tick;
    // larger discontinuities retain their explicit gap/overlap handling.
    let numerator = i128::from(base.num) * i128::from(RATE);
    let denominator = i128::from(base.den);
    u64::try_from((numerator + denominator - 1) / denominator + 1)
        .context("Native audio timestamp tolerance is out of range")
}

fn aligned_timestamp(timestamp: i64, expected: Option<i64>, tolerance: u64) -> Result<i64> {
    match expected {
        Some(expected) if timestamp.abs_diff(expected) <= tolerance => Ok(expected),
        Some(_) => Err(UnsupportedNativeAudio("Midstream audio timestamp discontinuity is unsupported by native playback").into()),
        None => Ok(timestamp),
    }
}

fn packet_skip_samples(packet: &ffi::AVPacket) -> Result<u32> {
    ensure!(
        (0..=64).contains(&packet.side_data_elems),
        "Native audio packet has invalid side data"
    );
    if packet.side_data_elems == 0 {
        return Ok(0);
    }
    ensure!(
        !packet.side_data.is_null(),
        "Native audio packet side data is missing"
    );
    for index in 0..packet.side_data_elems as usize {
        let side = unsafe { &*packet.side_data.add(index) };
        if side.type_ == ffi::AVPacketSideDataType_AV_PKT_DATA_SKIP_SAMPLES {
            ensure!(
                !side.data.is_null() && side.size >= 10,
                "Native audio packet has truncated skip-samples data"
            );
            let bytes = unsafe { std::slice::from_raw_parts(side.data, 4) };
            return Ok(u32::from_le_bytes(bytes.try_into()?));
        }
    }
    Ok(0)
}

fn priming_adjusted_origin(timestamp_us: i64, skip: u32, sample_rate: i32) -> Result<i64> {
    ensure!(sample_rate > 0, "Native AAC origin has no sample rate");
    ensure!(
        timestamp_us >= 0 || skip > 0,
        "Native AAC negative origin has ambiguous priming"
    );
    timestamp_us
        .checked_add(rescale(i64::from(skip), 1_000_000, i64::from(sample_rate))?)
        .context("Native AAC origin overflow")
}

fn stereo(layout: ffi::AVChannelLayout, pcm: bool) -> bool {
    layout.nb_channels == 2
        && (layout.order == ffi::AVChannelOrder_AV_CHANNEL_ORDER_NATIVE
            && unsafe { layout.u.mask == 3 }
            || pcm && layout.order == ffi::AVChannelOrder_AV_CHANNEL_ORDER_UNSPEC)
}

impl Decoder {
    fn open(path: &Path, stream: u32, start: u64, cancel: Arc<AtomicBool>) -> Result<Self> {
        ensure!(!cancel.load(Ordering::Acquire), "Native audio cancelled");
        let path = path
            .canonicalize()
            .context("Native audio file is unavailable")?;
        ensure!(
            matches!(path.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))),
            "Native audio accepts local drive files only"
        );
        ensure!(path.is_file(), "Native audio source is not a regular file");
        let path = path.to_str().context("Native audio path is not UTF-8")?;
        let path = CString::new(path.strip_prefix("\\\\?\\").unwrap_or(path))?;
        let api = crate::ffmpeg_runtime::retry_busy(
            || cancel.load(Ordering::Acquire),
            Duration::from_millis(250),
            || Api::load(&cancel),
        )?;
        let mut decoder = Self {
            api,
            format: ptr::null_mut(),
            codec: ptr::null_mut(),
            packet: ptr::null_mut(),
            frame: ptr::null_mut(),
            interrupt: Box::new(Interrupt {
                cancel,
                deadline: Instant::now() + IO_TIMEOUT,
            }),
            stream: c_int::try_from(stream).context("Native audio stream index is out of range")?,
            time_base: ffi::AVRational { num: 0, den: 1 },
            origin_sample: 0,
            pcm: false,
            pending: VecDeque::new(),
            _worker: PhantomData,
        };
        unsafe {
            decoder.initialize(&path, start)?;
        }
        Ok(decoder)
    }

    fn begin_io(&mut self) -> Result<()> {
        self.interrupt.deadline = Instant::now() + IO_TIMEOUT;
        self.interrupt.check()
    }

    unsafe fn initialize(&mut self, path: &CString, start: u64) -> Result<()> {
        self.format = unsafe { (self.api.avformat_alloc_context)() };
        ensure!(
            !self.format.is_null(),
            "Cannot allocate native audio demuxer"
        );
        unsafe {
            (*self.format).interrupt_callback = ffi::AVIOInterruptCB {
                callback: Some(interrupt),
                opaque: (&mut *self.interrupt as *mut Interrupt).cast(),
            };
            (*self.format).probesize = 262_144;
            (*self.format).max_analyze_duration = 0;
            (*self.format).max_streams = 128;
            (*self.format).max_probe_packets = 32;
        }
        let mut options = ptr::null_mut();
        let opened = (|| -> Result<()> {
            self.api.check(
                unsafe {
                    (self.api.av_dict_set)(
                        &mut options,
                        c"protocol_whitelist".as_ptr(),
                        c"file".as_ptr(),
                        0,
                    )
                },
                "set protocol whitelist",
            )?;
            self.api.check(
                unsafe {
                    (self.api.av_dict_set)(
                        &mut options,
                        c"format_whitelist".as_ptr(),
                        c"mov,matroska,webm".as_ptr(),
                        0,
                    )
                },
                "set container whitelist",
            )?;
            self.begin_io()?;
            self.api.check(
                unsafe {
                    (self.api.avformat_open_input)(
                        &mut self.format,
                        path.as_ptr(),
                        ptr::null(),
                        &mut options,
                    )
                },
                "open input",
            )
        })();
        unsafe {
            (self.api.av_dict_free)(&mut options);
        }
        opened?;
        // MP4/MKV headers carry complete audio codec parameters. Avoid probing
        // or opening the video codec solely to start an audio stream.
        let format = unsafe { &*self.format };
        ensure!(
            format.nb_streams <= 128
                && !format.streams.is_null()
                && (self.stream as u32) < format.nb_streams,
            "Native audio stream is unavailable"
        );
        let stream = unsafe { *format.streams.add(self.stream as usize) };
        ensure!(!stream.is_null(), "Native audio stream is missing");
        let parameters = unsafe { (*stream).codecpar };
        ensure!(
            !parameters.is_null(),
            "Native audio stream has no codec parameters"
        );
        let parameters = unsafe { &*parameters };
        ensure!(
            parameters.codec_type == ffi::AVMediaType_AVMEDIA_TYPE_AUDIO,
            "Native audio selected a non-audio stream"
        );
        self.pcm = parameters.codec_id == ffi::AVCodecID_AV_CODEC_ID_PCM_F32LE;
        ensure!(
            self.pcm || parameters.codec_id == ffi::AVCodecID_AV_CODEC_ID_AAC,
            "Native audio codec is unsupported"
        );
        if !self.pcm && start > 0 {
            // AAC PNS/priming state depends on the container's CLI seek anchor.
            // Retain the existing decoder rather than publish differing PCM.
            return Err(UnsupportedNativeAudio("AAC source trims and seeks require external playback").into());
        }
        ensure!(
            parameters.sample_rate == 48_000
                && parameters.ch_layout.nb_channels == 2
                && (!self.pcm || stereo(parameters.ch_layout, true)),
            "Native audio requires 48 kHz stereo AAC or PCM float: rate={}, channels={}, order={}",
            parameters.sample_rate,
            parameters.ch_layout.nb_channels,
            parameters.ch_layout.order
        );
        ensure!(
            self.pcm || parameters.extradata_size > 0 && !parameters.extradata.is_null(),
            "Native AAC has no codec configuration"
        );
        self.time_base = unsafe { (*stream).time_base };
        let codec_id = parameters.codec_id;
        ensure!(
            self.time_base.num > 0 && self.time_base.den > 0,
            "Native audio has invalid stream time base"
        );
        self.packet = unsafe { (self.api.av_packet_alloc)() };
        ensure!(
            !self.packet.is_null(),
            "Cannot allocate native audio packet"
        );
        let origin = self.container_origin()?;
        if origin != 0 {
            return Err(UnsupportedNativeAudio("Nonzero container timestamp origins require external audio playback").into());
        }
        self.origin_sample = rescale(origin, RATE, 1_000_000)?;
        // Packet scanning can update/reallocate codec parameters. Reacquire
        // them instead of retaining a shared Rust reference across demux calls.
        let stream = unsafe { *(*self.format).streams.add(self.stream as usize) };
        ensure!(!stream.is_null(), "Native audio selected stream disappeared");
        let parameters = unsafe { (*stream).codecpar };
        ensure!(!parameters.is_null() && unsafe { (*parameters).codec_id == codec_id },
            "Native audio codec changed while reading headers");
        self.time_base = unsafe { (*stream).time_base };
        ensure!(self.time_base.num > 0 && self.time_base.den > 0, "Native audio timebase changed while reading headers");
        let codec = unsafe { (self.api.avcodec_find_decoder)(codec_id) };
        ensure!(!codec.is_null(), "Native audio decoder is unavailable");
        self.codec = unsafe { (self.api.avcodec_alloc_context3)(codec) };
        ensure!(
            !self.codec.is_null(),
            "Cannot allocate native audio decoder"
        );
        self.api.check(
            unsafe { (self.api.avcodec_parameters_to_context)(self.codec, parameters) },
            "copy codec parameters",
        )?;
        unsafe {
            (*self.codec).thread_count = 1;
            (*self.codec).pkt_timebase = self.time_base;
        }
        self.api.check(
            unsafe { (self.api.avcodec_open2)(self.codec, codec, ptr::null_mut()) },
            "open audio codec",
        )?;
        let codec = unsafe { &*self.codec };
        ensure!(
            codec.sample_rate == 48_000
                && stereo(codec.ch_layout, self.pcm)
                && (codec.sample_fmt == ffi::AVSampleFormat_AV_SAMPLE_FMT_FLT
                    || codec.sample_fmt == ffi::AVSampleFormat_AV_SAMPLE_FMT_FLTP),
            "Native audio decoder output is unsupported: rate={}, channels={}, order={}, format={}",
            codec.sample_rate,
            codec.ch_layout.nb_channels,
            codec.ch_layout.order,
            codec.sample_fmt
        );
        self.frame = unsafe { (self.api.av_frame_alloc)() };
        ensure!(
            !self.packet.is_null() && !self.frame.is_null(),
            "Cannot allocate native audio buffers"
        );
        ensure!(
            !self.pcm || start <= 30 * RATE as u64,
            "Native PCM exact seeking is limited to 30 seconds of preroll"
        );
        Ok(())
    }

    fn container_origin(&mut self) -> Result<i64> {
        let format = unsafe { &*self.format };
        if format.start_time != NO_PTS {
            return Ok(format.start_time);
        }
        let mut starts = Vec::with_capacity(format.nb_streams as usize);
        let mut needed = Vec::with_capacity(format.nb_streams as usize);
        for index in 0..format.nb_streams as usize {
            let stream = unsafe { *format.streams.add(index) };
            ensure!(!stream.is_null(), "Native audio stream table is incomplete");
            let stream = unsafe { &*stream };
            ensure!(
                !stream.codecpar.is_null(),
                "Native audio stream metadata is incomplete"
            );
            let parameters = unsafe { &*stream.codecpar };
            let relevant = parameters.codec_type == ffi::AVMediaType_AVMEDIA_TYPE_AUDIO
                || parameters.codec_type == ffi::AVMediaType_AVMEDIA_TYPE_VIDEO;
            needed.push(relevant);
            starts.push(if relevant && stream.start_time != NO_PTS {
                Some(rescale(
                    stream.start_time,
                    i64::from(stream.time_base.num) * 1_000_000,
                    i64::from(stream.time_base.den),
                )?)
            } else {
                None
            });
        }
        let resolved = |starts: &[Option<i64>]| {
            needed
                .iter()
                .zip(starts)
                .all(|(needed, start)| !needed || start.is_some())
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut scanned_bytes = 0_usize;
        for _ in 0..128 {
            if resolved(&starts) {
                break;
            }
            self.begin_io()?;
            self.interrupt.deadline = self.interrupt.deadline.min(deadline);
            self.interrupt.check()?;
            let read = unsafe { (self.api.av_read_frame)(self.format, self.packet) };
            if read == EOF {
                break;
            }
            self.api.check(read, "scan container timestamp origin")?;
            let packet = unsafe { &*self.packet };
            ensure!(packet.size >= 0, "Native audio scanned an invalid packet");
            scanned_bytes = scanned_bytes.saturating_add(packet.size as usize);
            ensure!(
                scanned_bytes <= 2 * 1024 * 1024,
                "Native audio container origin exceeds bounded header scan"
            );
            let index = usize::try_from(packet.stream_index)
                .context("Native audio packet has invalid stream")?;
            ensure!(
                index < starts.len(),
                "Native audio packet references an unknown stream"
            );
            if needed[index] && starts[index].is_none() && packet.pts != NO_PTS {
                let stream = unsafe { &**(*self.format).streams.add(index) };
                let parameters = unsafe { &*stream.codecpar };
                let mut timestamp = rescale(
                    packet.pts,
                    i64::from(stream.time_base.num) * 1_000_000,
                    i64::from(stream.time_base.den),
                )?;
                // AAC's first negative packet is encoder priming, not a
                // negative container origin. Preserve the packet unchanged for
                // avcodec's default skip-samples processing.
                if parameters.codec_id == ffi::AVCodecID_AV_CODEC_ID_AAC {
                    let skip = packet_skip_samples(packet)?;
                    timestamp = priming_adjusted_origin(timestamp, skip, parameters.sample_rate)?;
                }
                starts[index] = Some(timestamp);
            }
            if packet.stream_index == self.stream {
                let copy = unsafe { (self.api.av_packet_clone)(self.packet) };
                ensure!(!copy.is_null(), "Cannot retain native audio header packet");
                self.pending.push_back(copy);
            }
            unsafe {
                (self.api.av_packet_unref)(self.packet);
            }
        }
        if !resolved(&starts) {
            return Err(UnsupportedNativeAudio("Ambiguous container timestamp origins require external audio playback").into());
        }
        starts
            .into_iter()
            .flatten()
            .min()
            .context("Native audio container has no timestamp origin")
    }

    fn next_frame(&mut self) -> Result<bool> {
        loop {
            self.interrupt.check()?;
            let received = unsafe { (self.api.avcodec_receive_frame)(self.codec, self.frame) };
            if received == 0 {
                return Ok(true);
            }
            if received == EOF {
                return Ok(false);
            }
            if received != AGAIN {
                self.api.check(received, "receive audio frame")?;
            }
            if let Some(mut packet) = self.pending.pop_front() {
                let sent = unsafe { (self.api.avcodec_send_packet)(self.codec, packet) };
                unsafe {
                    (self.api.av_packet_free)(&mut packet);
                }
                self.api.check(sent, "send retained audio packet")?;
                continue;
            }
            self.begin_io()?;
            let read = unsafe { (self.api.av_read_frame)(self.format, self.packet) };
            if read == EOF {
                self.api.check(
                    unsafe { (self.api.avcodec_send_packet)(self.codec, ptr::null()) },
                    "drain audio decoder",
                )?;
                let received = unsafe { (self.api.avcodec_receive_frame)(self.codec, self.frame) };
                if received == EOF {
                    return Ok(false);
                }
                self.api.check(received, "receive final audio frame")?;
                return Ok(true);
            }
            self.api.check(read, "read input packet")?;
            let selected = unsafe { (*self.packet).stream_index == self.stream };
            let sent = if selected {
                unsafe { (self.api.avcodec_send_packet)(self.codec, self.packet) }
            } else {
                0
            };
            unsafe {
                (self.api.av_packet_unref)(self.packet);
            }
            self.api.check(sent, "send audio packet")?;
        }
    }
}

/// Decode raw source-relative 48 kHz stereo sample frames, in bounded callbacks.
/// The caller retains all existing limiter/mix/filter behavior. Once any callback
/// publishes samples, an error must terminate playback rather than restart a
/// fallback decoder and duplicate already-published audio.
#[cfg(test)]
pub(super) fn decode(
    path: &Path,
    stream: u32,
    source_start_sample: u64,
    max_samples: u64,
    cancel: Arc<AtomicBool>,
    emit: impl FnMut(&[[f32; 2]]) -> Result<()>,
) -> Result<()> {
    decode_mode(
        path,
        stream,
        source_start_sample,
        max_samples,
        cancel,
        true,
        emit,
    )
}

/// Preserve the source EOF for downstream atempo/apad/trim filtering. Leading
/// source timestamp gaps are still aligned, but final silence is not inserted.
#[cfg(test)]
pub(super) fn decode_unpadded(
    path: &Path,
    stream: u32,
    source_start_sample: u64,
    max_samples: u64,
    cancel: Arc<AtomicBool>,
    emit: impl FnMut(&[[f32; 2]]) -> Result<()>,
) -> Result<()> {
    decode_mode(
        path,
        stream,
        source_start_sample,
        max_samples,
        cancel,
        false,
        emit,
    )
}

/// Only full-source playback is native. CLI seek anchors can change PCM/AAC
/// sample history even when the requested timestamp lies on the sample grid.
pub(super) fn decode_for_playback(path: &Path, stream: u32, source_start_sample: u64,
    max_samples: u64, cancel: Arc<AtomicBool>, emit: impl FnMut(&[[f32; 2]]) -> Result<()>) -> Result<()> {
    if source_start_sample > 0 {
        return Err(UnsupportedNativeAudio("Source audio trims and seeks require external playback").into());
    }
    decode_mode(path, stream, source_start_sample, max_samples, cancel, false, emit)
}
fn decode_mode(
    path: &Path,
    stream: u32,
    source_start_sample: u64,
    max_samples: u64,
    cancel: Arc<AtomicBool>,
    pad_eof: bool,
    mut emit: impl FnMut(&[[f32; 2]]) -> Result<()>,
) -> Result<()> {
    if max_samples == 0 {
        return Ok(());
    }
    let mut decoder = Decoder::open(path, stream, source_start_sample, cancel)?;
    let end = source_start_sample
        .checked_add(max_samples)
        .context("Native audio range overflow")?;
    ensure!(
        end <= i64::MAX as u64,
        "Native audio range is out of bounds"
    );
    let mut position = source_start_sample;
    let mut output = Vec::with_capacity(BLOCK);
    let mut expected: Option<i64> = None;
    let timestamp_tolerance = timestamp_tolerance(decoder.time_base)?;
    while position < end {
        // Waiting for the downstream PCM queue is not an input I/O timeout.
        decoder.begin_io()?;
        if !decoder.next_frame()? {
            break;
        }
        let frame = unsafe { &*decoder.frame };
        ensure!(
            frame.sample_rate == 48_000 && stereo(frame.ch_layout, decoder.pcm),
            "Native audio layout/rate changed midstream"
        );
        ensure!(
            frame.nb_samples >= 0 && frame.nb_samples <= 1_048_576,
            "Native audio frame has invalid sample count"
        );
        let planar = frame.format == ffi::AVSampleFormat_AV_SAMPLE_FMT_FLTP;
        ensure!(
            planar || frame.format == ffi::AVSampleFormat_AV_SAMPLE_FMT_FLT,
            "Native audio sample format changed midstream"
        );
        let count = frame.nb_samples as usize;
        let bytes = count * if planar { 4 } else { 8 };
        ensure!(
            frame.linesize[0] >= 0
                && frame.linesize[0] as usize >= bytes
                && !frame.data[0].is_null()
                && (!planar || !frame.data[1].is_null()),
            "Native audio frame has invalid planes"
        );
        let pts = if frame.pts != NO_PTS {
            Some(frame.pts)
        } else if frame.best_effort_timestamp != NO_PTS {
            Some(frame.best_effort_timestamp)
        } else {
            None
        };
        let first = if let Some(pts) = pts {
            rescale(
                pts,
                i64::from(decoder.time_base.num) * RATE,
                i64::from(decoder.time_base.den),
            )?
            .checked_sub(decoder.origin_sample)
            .context("Native audio sample timestamp overflow")?
        } else {
            expected.context("Native audio initial frame has no timestamp")?
        };
        let first = aligned_timestamp(first, expected, timestamp_tolerance)?;
        let frame_end = first
            .checked_add(count as i64)
            .context("Native audio frame timestamp overflow")?;
        expected = Some(frame_end);
        while position < end && (position as i64) < first {
            ensure!(
                !decoder.interrupt.cancel.load(Ordering::Acquire),
                "Native audio cancelled"
            );
            let n = (end - position)
                .min((first as u64).saturating_sub(position))
                .min(BLOCK as u64) as usize;
            output.clear();
            output.resize(n, [0.; 2]);
            emit(&output)?;
            position += n as u64;
        }
        let mut index = (position as i128 - i128::from(first)).clamp(0, count as i128) as usize;
        while position < end && index < count {
            ensure!(
                !decoder.interrupt.cancel.load(Ordering::Acquire),
                "Native audio cancelled"
            );
            let n = (count - index).min(BLOCK).min((end - position) as usize);
            output.clear();
            for sample in index..index + n {
                let pair = unsafe {
                    if planar {
                        [
                            ptr::read_unaligned(frame.data[0].cast::<f32>().add(sample)),
                            ptr::read_unaligned(frame.data[1].cast::<f32>().add(sample)),
                        ]
                    } else {
                        [
                            ptr::read_unaligned(frame.data[0].cast::<f32>().add(sample * 2)),
                            ptr::read_unaligned(frame.data[0].cast::<f32>().add(sample * 2 + 1)),
                        ]
                    }
                };
                ensure!(
                    pair[0].is_finite() && pair[1].is_finite(),
                    "Native audio contains non-finite samples"
                );
                output.push(pair);
            }
            emit(&output)?;
            position += n as u64;
            index += n;
        }
        unsafe {
            (decoder.api.av_frame_unref)(decoder.frame);
        }
    }
    while pad_eof && position < end {
        ensure!(
            !decoder.interrupt.cancel.load(Ordering::Acquire),
            "Native audio cancelled"
        );
        let n = (end - position).min(BLOCK as u64) as usize;
        output.clear();
        output.resize(n, [0.; 2]);
        emit(&output)?;
        position += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamp_rounding_preserves_aac_and_pcm_sample_grid() {
        assert_eq!(rescale(-1024, RATE, RATE).unwrap(), -1024);
        assert_eq!(rescale(10, RATE, 1000).unwrap(), 480);
        assert_eq!(rescale(-10, RATE, 1000).unwrap(), -480);
        assert!(rescale(1, 1, 0).is_err());
    }

    #[test]
    fn coarse_pcm_timestamps_keep_continuity_and_decline_real_discontinuities() {
        let tolerance = timestamp_tolerance(ffi::AVRational { num: 1, den: 1000 }).unwrap();
        assert_eq!(tolerance, 49);
        assert_eq!(aligned_timestamp(1008, Some(1024), tolerance).unwrap(), 1024);
        assert_eq!(aligned_timestamp(2064, Some(2048), tolerance).unwrap(), 2048);
        for timestamp in [2528, 1568] {
            assert!(aligned_timestamp(timestamp, Some(2048), tolerance).unwrap_err()
                .downcast_ref::<UnsupportedNativeAudio>().is_some());
        }
        assert_eq!(aligned_timestamp(480, None, tolerance).unwrap(), 480);
        assert_eq!(aligned_timestamp(96, Some(128), tolerance).unwrap(), 128);
    }

    #[test]
    fn negative_aac_priming_is_not_a_negative_container_start() {
        let priming_pts = rescale(-1024, 1_000_000, 48_000).unwrap();
        assert_eq!(
            priming_adjusted_origin(priming_pts, 1024, 48_000).unwrap(),
            0
        );
        assert!(priming_adjusted_origin(priming_pts, 0, 48_000).is_err());
        // Delayed audio remains delayed relative to an earlier video stream.
        assert_eq!(
            priming_adjusted_origin(500_000, 0, 48_000).unwrap(),
            500_000
        );
    }

    #[test]
    #[ignore = "Generated-file native/raw FFmpeg parity; needs exact runtime and a supplied AV1 fixture"]
    fn legacy_floor_pcm_packets_preserve_tone_seek_and_eof() {
        use media_core::{AudioTrack, MediaPacket, VideoConfig};
        use media_io::MatroskaWriter;
        use std::process::{Command, Stdio};
        let source = std::env::var_os("RCR_NATIVE_AUDIO_AV1_FIXTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| ".downloads/autonomous-eval/viewer-av1-pcm.mkv".into());
        let command = || {
            let mut command =
                Command::new(std::env::var_os("RCR_FFMPEG").unwrap_or_else(|| "ffmpeg".into()));
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000).stdin(Stdio::null());
            command.args(["-v", "error", "-threads", "1"]);
            command
        };
        let packet = command()
            .arg("-i")
            .arg(source)
            .args([
                "-map",
                "0:v:0",
                "-frames:v",
                "1",
                "-c:v",
                "copy",
                "-f",
                "obu",
                "pipe:1",
            ])
            .output()
            .unwrap();
        assert!(
            packet.status.success(),
            "{}",
            String::from_utf8_lossy(&packet.stderr)
        );
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("legacy-128-frame-pcm.mkv");
        let video = VideoConfig {
            width: 640,
            height: 360,
            fps: 30,
            bitrate: 1_000_000,
            keyframe_interval: 30,
        };
        let first = MediaPacket {
            track: 1,
            pts_ns: 0,
            duration_ns: 1_000_000_000,
            keyframe: true,
            data: packet.stdout.into(),
        };
        let mut writer =
            MatroskaWriter::create(&path, &video, &[AudioTrack::system()], &first).unwrap();
        writer.packet(&first).unwrap();
        for packet in 0..375_u64 {
            let first_sample = packet * 128;
            let samples = (first_sample..first_sample + 128)
                .flat_map(|sample| {
                    [440., 880.].into_iter().flat_map(move |hz| {
                        let value = (std::f64::consts::TAU * hz * sample as f64 / 48_000.).sin()
                            as f32
                            * 0.2;
                        value.to_le_bytes()
                    })
                })
                .collect::<Vec<_>>();
            writer
                .packet(&MediaPacket {
                    track: 2,
                    // Deliberately force historical floor quantization, even when
                    // the current writer rounds timestamps to nearest ms.
                    pts_ns: (first_sample * 1_000_000_000 / 48_000 / 1_000_000) * 1_000_000,
                    duration_ns: 128 * 1_000_000_000 / 48_000,
                    keyframe: true,
                    data: samples.into(),
                })
                .unwrap();
        }
        writer.finish().unwrap();
        let reference = command().arg("-i").arg(&path)
            .args(["-map", "0:1", "-vn", "-af", "aresample=48000:async=1:first_pts=0,aformat=sample_fmts=fltp:channel_layouts=stereo",
                "-ar", "48000", "-ac", "2", "-f", "f32le", "pipe:1"]).output().unwrap();
        assert!(
            reference.status.success(),
            "{}",
            String::from_utf8_lossy(&reference.stderr)
        );
        let reference = reference
            .stdout
            .chunks_exact(8)
            .map(|bytes| {
                [
                    f32::from_le_bytes(bytes[..4].try_into().unwrap()),
                    f32::from_le_bytes(bytes[4..].try_into().unwrap()),
                ]
            })
            .collect::<Vec<_>>();
        assert_eq!(reference.len(), 48_000);
        for (start, count) in [(0_u64, 48_000_u64), (12_345, 16_000), (47_000, 4_000)] {
            let mut native = Vec::new();
            decode(
                &path,
                1,
                start,
                count,
                Arc::new(AtomicBool::new(false)),
                |samples| {
                    native.extend_from_slice(samples);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(native.len(), count as usize);
            let mut maximum = 0_f32;
            for (index, sample) in native.iter().enumerate() {
                let expected = reference
                    .get(start as usize + index)
                    .copied()
                    .unwrap_or([0.; 2]);
                for channel in 0..2 {
                    maximum = maximum.max((sample[channel] - expected[channel]).abs());
                }
            }
            assert!(
                maximum <= 1e-7,
                "Legacy floor PCM changed at start={start}: maximum error={maximum}"
            );
        }
        let mut unpadded = 0;
        decode_unpadded(
            &path,
            1,
            47_000,
            4_000,
            Arc::new(AtomicBool::new(false)),
            |samples| {
                unpadded += samples.len();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(unpadded, 1000);
    }
}
