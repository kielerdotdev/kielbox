//! The existing default-speed audio chain, using version-checked opaque filters.
//! This deliberately supports only packed stereo48k PCM, not arbitrary graphs.
use crate::ffmpeg_runtime::{load_runtime, FfmpegRuntimeConfig, FfmpegRuntimeRequirement, Runtime};
use crate::playback_gpu::decode::ffi;
use anyhow::{Context, Result, ensure};
use libloading::Library;
use std::{ffi::{CStr, CString, c_char, c_int, c_void}, ptr, sync::{Arc, atomic::{AtomicBool, Ordering}}};

type Handle = *mut c_void;
const AGAIN: c_int = -11;
const EOF: c_int = -541478725;
const KEEP_REF: c_int = 8; // libavfilter/buffersrc.h AV_BUFFERSRC_FLAG_KEEP_REF.

#[allow(non_snake_case)]
struct Api {
    avfilter_graph_alloc: unsafe extern "C" fn() -> Handle,
    avfilter_graph_free: unsafe extern "C" fn(*mut Handle),
    avfilter_get_by_name: unsafe extern "C" fn(*const c_char) -> *const c_void,
    avfilter_graph_create_filter: unsafe extern "C" fn(*mut Handle, *const c_void, *const c_char, *const c_char, Handle, Handle) -> c_int,
    avfilter_link: unsafe extern "C" fn(Handle, u32, Handle, u32) -> c_int,
    avfilter_graph_config: unsafe extern "C" fn(Handle, Handle) -> c_int,
    av_buffersrc_add_frame_flags: unsafe extern "C" fn(Handle, *mut ffi::AVFrame, c_int) -> c_int,
    av_buffersink_get_frame: unsafe extern "C" fn(Handle, *mut ffi::AVFrame) -> c_int,
    av_frame_alloc: unsafe extern "C" fn() -> *mut ffi::AVFrame,
    av_frame_free: unsafe extern "C" fn(*mut *mut ffi::AVFrame),
    av_frame_unref: unsafe extern "C" fn(*mut ffi::AVFrame),
    av_frame_get_buffer: unsafe extern "C" fn(*mut ffi::AVFrame, c_int) -> c_int,
    av_channel_layout_default: unsafe extern "C" fn(*mut ffi::AVChannelLayout, c_int),
    av_opt_set_int: unsafe extern "C" fn(Handle, *const c_char, i64, c_int) -> c_int,
    _filter: Library,
    _runtime: Arc<Runtime>,
}
impl Api {
    fn load(cancel: &AtomicBool) -> Result<Self> {
        let runtime = load_runtime(&FfmpegRuntimeConfig::Automatic, FfmpegRuntimeRequirement::Gpu702,
            false, || cancel.load(Ordering::Acquire))?;
        let filter: Library = unsafe {
            libloading::os::windows::Library::load_with_flags(runtime.info.directory.join("avfilter-10.dll"),
                windows::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR.0
                | windows::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_DEFAULT_DIRS.0)?
        }.into();
        unsafe {
            let version = filter.get::<unsafe extern "C" fn() -> u32>(b"avfilter_version")?();
            ensure!(version == (10 << 16 | 1 << 8 | 100), "Native audio limiter requires FFmpeg7.0.2 libavfilter");
            Ok(Self {
                avfilter_graph_alloc: *filter.get(b"avfilter_graph_alloc")?,
                avfilter_graph_free: *filter.get(b"avfilter_graph_free")?,
                avfilter_get_by_name: *filter.get(b"avfilter_get_by_name")?,
                avfilter_graph_create_filter: *filter.get(b"avfilter_graph_create_filter")?,
                avfilter_link: *filter.get(b"avfilter_link")?,
                avfilter_graph_config: *filter.get(b"avfilter_graph_config")?,
                av_buffersrc_add_frame_flags: *filter.get(b"av_buffersrc_add_frame_flags")?,
                av_buffersink_get_frame: *filter.get(b"av_buffersink_get_frame")?,
                av_frame_alloc: runtime.symbol(0, b"av_frame_alloc")?,
                av_frame_free: runtime.symbol(0, b"av_frame_free")?,
                av_frame_unref: runtime.symbol(0, b"av_frame_unref")?,
                av_frame_get_buffer: runtime.symbol(0, b"av_frame_get_buffer")?,
                av_channel_layout_default: runtime.symbol(0, b"av_channel_layout_default")?,
                av_opt_set_int: runtime.symbol(0, b"av_opt_set_int")?,
                _filter: filter, _runtime: runtime,
            })
        }
    }
}
pub(super) struct StereoLimiter {
    api: Api,
    graph: Handle,
    source: Handle,
    sink: Handle,
    input: *mut ffi::AVFrame,
    output: *mut ffi::AVFrame,
    cancel: Arc<AtomicBool>,
    supplied: u64,
    emitted: u64,
    expected: u64,
}
impl StereoLimiter {
    pub fn new(cancel: Arc<AtomicBool>, expected: u64) -> Result<Self> {
        let api = crate::ffmpeg_runtime::retry_busy(|| cancel.load(Ordering::Acquire),
            std::time::Duration::from_millis(250), || Api::load(&cancel))?;
        let mut result = Self { api, graph: ptr::null_mut(), source: ptr::null_mut(), sink: ptr::null_mut(),
            input: ptr::null_mut(), output: ptr::null_mut(), cancel, supplied: 0, emitted: 0, expected };
        result.check_cancel()?;
        unsafe {
            result.graph = (result.api.avfilter_graph_alloc)();
            ensure!(!result.graph.is_null(), "Cannot allocate native audio filter graph");
            Self::check((result.api.av_opt_set_int)(result.graph, c"threads".as_ptr(), 2, 0), "bound filter threads")?;
            let source = result.create(c"abuffer", c"input", c"sample_rate=48000:sample_fmt=flt:channel_layout=stereo:time_base=1/48000")?;
            // Keep the default-speed pipeline identical to playback_audio_plan.
            // Identity playback bypasses WSOLA so finite-clip tails stay intact.
            let stages = [
                ("aresample", "48000:async=1:first_pts=0".to_owned()),
                ("aformat", "sample_fmts=fltp:channel_layouts=stereo".into()),
                ("atrim", format!("end_sample={expected}")),
                ("asetpts", "N/SR/TB".into()),
                ("anull", "".into()),
                ("apad", format!("whole_len={expected}")),
                ("atrim", format!("end_sample={expected}")),
                ("asetpts", "N/SR/TB".into()),
                ("asetpts", "PTS+0/TB".into()),
                ("volume", "1:eval=frame".into()),
                ("asetpts", "N/SR/TB".into()),
                ("adelay", "0S:all=1".into()),
                ("apad", format!("whole_len={expected}")),
                ("atrim", format!("end_sample={expected}")),
                ("asetpts", "N/SR/TB".into()),
                ("amix", "inputs=1:normalize=0:dropout_transition=0".into()),
                ("alimiter", "limit=0.98:level=false:latency=1".into()),
                ("amix", "inputs=1:normalize=0:dropout_transition=0".into()),
                ("alimiter", "limit=0.98:level=false:latency=1".into()),
                ("aformat", "sample_fmts=flt:sample_rates=48000:channel_layouts=stereo".into()),
                ("abuffersink", String::new()),
            ];
            let mut previous = source;
            for (index, (kind, args)) in stages.iter().enumerate() {
                let node = result.create(&CString::new(*kind)?, &CString::new(format!("stage{index}"))?, &CString::new(args.as_str())?)?;
                Self::check((result.api.avfilter_link)(previous, 0, node, 0), "link native audio filter")?;
                previous = node;
            }
            Self::check((result.api.avfilter_graph_config)(result.graph, ptr::null_mut()), "configure native audio limiter")?;
            result.source = source;
            result.sink = previous;
            result.input = (result.api.av_frame_alloc)();
            result.output = (result.api.av_frame_alloc)();
            ensure!(!result.input.is_null() && !result.output.is_null(), "Cannot allocate native audio filter frames");
        }
        Ok(result)
    }
    unsafe fn create(&self, kind: &CStr, name: &CStr, args: &CStr) -> Result<Handle> {
        let filter = (self.api.avfilter_get_by_name)(kind.as_ptr());
        ensure!(!filter.is_null(), "Required native audio filter is unavailable");
        let mut context = ptr::null_mut();
        Self::check((self.api.avfilter_graph_create_filter)(&mut context, filter, name.as_ptr(),
            if args.is_empty() { ptr::null() } else { args.as_ptr() }, ptr::null_mut(), self.graph), "create native audio filter")?;
        Ok(context)
    }
    fn check(code: c_int, operation: &str) -> Result<()> {
        ensure!(code >= 0, "Could not {operation} ({code})");
        Ok(())
    }
    fn check_cancel(&self) -> Result<()> {
        ensure!(!self.cancel.load(Ordering::Acquire), "Native audio filter cancelled");
        Ok(())
    }
    pub fn push(&mut self, samples: &[[f32; 2]], emit: &mut impl FnMut(&[[f32; 2]]) -> Result<()>) -> Result<()> {
        for chunk in samples.chunks(4096) {
            self.check_cancel()?;
            ensure!(self.supplied + chunk.len() as u64 <= self.expected, "Native PCM exceeds requested source trim");
            unsafe {
                (self.api.av_frame_unref)(self.input);
                (*self.input).format = ffi::AVSampleFormat_AV_SAMPLE_FMT_FLT;
                (*self.input).sample_rate = 48_000;
                (*self.input).time_base = ffi::AVRational { num: 1, den: 48_000 };
                (*self.input).nb_samples = c_int::try_from(chunk.len()).context("Audio chunk exceeds frame limit")?;
                (*self.input).pts = i64::try_from(self.supplied).context("Audio timeline overflow")?;
                (self.api.av_channel_layout_default)(&mut (*self.input).ch_layout, 2);
                Self::check((self.api.av_frame_get_buffer)(self.input, 0), "allocate native PCM input")?;
                ensure!(!(*self.input).data[0].is_null() && (*self.input).linesize[0] >= (chunk.len() * 8) as c_int,
                    "Native PCM input layout is too short");
                ptr::copy_nonoverlapping(chunk.as_ptr().cast::<u8>(), (*self.input).data[0], chunk.len() * 8);
                Self::check((self.api.av_buffersrc_add_frame_flags)(self.source, self.input, KEEP_REF), "supply native PCM limiter")?;
                self.supplied += chunk.len() as u64;
            }
            self.drain(false, emit)?;
        }
        Ok(())
    }
    pub fn finish(&mut self, emit: &mut impl FnMut(&[[f32; 2]]) -> Result<()>) -> Result<()> {
        self.check_cancel()?;
        unsafe { Self::check((self.api.av_buffersrc_add_frame_flags)(self.source, ptr::null_mut(), 0), "finish native PCM limiter")?; }
        self.drain(true, emit)?;
        ensure!(self.expected == self.emitted, "Native filter changed sample count: {} expected, {} emitted", self.expected, self.emitted);
        Ok(())
    }
    fn drain(&mut self, finished: bool, emit: &mut impl FnMut(&[[f32; 2]]) -> Result<()>) -> Result<()> {
        loop {
            self.check_cancel()?;
            let status = unsafe { (self.api.av_buffersink_get_frame)(self.sink, self.output) };
            if status == EOF { return Ok(()); }
            if status == AGAIN {
                ensure!(!finished, "Native audio limiter needs input after EOF");
                return Ok(());
            }
            Self::check(status, "read native PCM limiter")?;
            let result = (|| -> Result<()> {
                unsafe {
                    let frame = &*self.output;
                    ensure!(frame.format == ffi::AVSampleFormat_AV_SAMPLE_FMT_FLT && frame.sample_rate == 48_000
                        && frame.ch_layout.nb_channels == 2 && frame.nb_samples >= 0 && frame.nb_samples <= 48_000,
                        "Native limiter returned an unsupported PCM frame");
                    let count = frame.nb_samples as usize;
                    ensure!(!frame.data[0].is_null() && frame.linesize[0] >= (count * 8) as c_int, "Native limiter returned a short PCM frame");
                    let samples = std::slice::from_raw_parts(frame.data[0].cast::<[f32; 2]>(), count);
                    emit(samples)?;
                    self.emitted += count as u64;
                }
                Ok(())
            })();
            unsafe { (self.api.av_frame_unref)(self.output); }
            result?;
        }
    }
}
impl Drop for StereoLimiter {
    fn drop(&mut self) {
        unsafe {
            if !self.graph.is_null() { (self.api.avfilter_graph_free)(&mut self.graph); }
            if !self.input.is_null() { (self.api.av_frame_free)(&mut self.input); }
            if !self.output.is_null() { (self.api.av_frame_free)(&mut self.output); }
        }
    }
}
