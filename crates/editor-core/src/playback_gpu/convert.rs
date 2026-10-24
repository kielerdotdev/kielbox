//! Isolated, worker-confined SDR video conversion. Not connected to app playback yet.
//!
//! The caller owns the input frame until the queued GPU read is complete. When this
//! shares an FFmpeg D3D11VA device, its recursive device lock must also surround
//! construction/publish/poll and any other access to the immediate/video contexts.
//! Tracked publication returns a ticket even when publication fails after a GPU
//! read was attempted. Retain the corresponding AVFrame until poll_completed
//! returns that ticket. A COM texture/view reference alone does not reserve a
//! decoder pool slice. Closing never waits: stop decoder reuse first, then poll
//! remaining tickets. If shutdown abandons a hung/device-lost session, destroy
//! that decoder before releasing its outstanding frames; never reuse its pool.
use anyhow::{ensure, Context, Result};
use std::{
    collections::VecDeque,
    marker::PhantomData,
    mem::ManuallyDrop,
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use windows::{
    core::{IUnknown, Interface, HRESULT},
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct3D::D3D_FEATURE_LEVEL_11_0,
            Direct3D11::*,
            Dxgi::{Common::*, IDXGIKeyedMutex},
        },
    },
};

const INPUT_CACHE_LIMIT: usize = 4;
const COMPLETION_LIMIT: usize = 3;
static NEXT_CONVERTER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
pub struct ConvertConfig {
    /// Texture extent, including any decoder padding. NV12 requires even sizes.
    pub coded_size: (u32, u32),
    /// Visible, chroma-aligned image within that texture; None means the full image.
    pub source_rect: Option<RECT>,
    /// This converter only scales down, with positive dimensions.
    pub output_size: (u32, u32),
}

impl ConvertConfig {
    fn source(self) -> Result<RECT> {
        let (width, height) = self.coded_size;
        ensure!(
            width > 0 && height > 0 && width <= 16384 && height <= 16384,
            "NV12 texture extent must be within 1..=16384"
        );
        ensure!(
            width % 2 == 0 && height % 2 == 0,
            "NV12 texture extent must be even"
        );
        let rect = self.source_rect.unwrap_or(RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        });
        ensure!(
            rect.left >= 0
                && rect.top >= 0
                && rect.right > rect.left
                && rect.bottom > rect.top
                && rect.right <= width as i32
                && rect.bottom <= height as i32,
            "visible source rectangle is outside the NV12 texture"
        );
        ensure!(
            [rect.left, rect.top, rect.right, rect.bottom]
                .iter()
                .all(|v| v % 2 == 0),
            "NV12 source rectangle must be chroma aligned"
        );
        let (out_width, out_height) = self.output_size;
        ensure!(
            out_width > 0
                && out_height > 0
                && out_width <= (rect.right - rect.left) as u32
                && out_height <= (rect.bottom - rect.top) as u32,
            "BGRA output must be positive and no larger than the visible source"
        );
        Ok(rect)
    }
}

#[derive(Clone)]
pub struct SharedOutput {
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    /// Nonzero identity written under producer key 0 before publishing key 1.
    /// Consumers read under key 1 and retain that identity with their private copy.
    pub published_frame: Arc<AtomicU64>,
    /// Notification only: the consumer increments after successfully releasing
    /// key 0. Producers must still acquire the keyed mutex before writing.
    pub consumer_release: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publish {
    Published,
    Busy,
    Cancelled,
}

/// Opaque identity, unique across converter sessions. It does not own an AVFrame.
#[must_use = "retain the input frame until this ticket is returned by poll_completed"]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReadTicket {
    owner: u64,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    Busy,
    Cancelled,
    QueueFull,
    Closed,
}

/// Err from publish_tracked means no input read was submitted. In contrast,
/// Submitted always requires frame retention, including publication: Err.
#[must_use = "submitted tickets require input retention even if publication failed"]
#[derive(Debug)]
pub enum TrackedPublish {
    Skipped(Skip),
    Submitted {
        ticket: ReadTicket,
        publication: Result<()>,
    },
}

struct PendingRead {
    ticket: ReadTicket,
    query: ID3D11Query,
    diagnostic_polls: u32,
    diagnostic_kicked: bool,
}

/// Controlled overrides. Default is the normal bounded submission policy.
#[derive(Clone, Copy, Debug)]
pub struct QueryDiagnostics {
    /// One flags=0 GetData per submitted query at this poll count; zero disables.
    pub kick_after_polls: u32,
    pub read_bool: bool,
}
impl Default for QueryDiagnostics {
    fn default() -> Self {
        Self {
            kick_after_polls: 1,
            read_bool: true,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct PublicationDiagnostics {
    pub flush_after_release: bool,
    /// False is a single-producer no-consumer control: release back to key 0.
    pub consumer_handoff: bool,
}
impl Default for PublicationDiagnostics {
    fn default() -> Self {
        Self {
            flush_after_release: false,
            consumer_handoff: true,
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct DiagnosticCounters {
    pub query_kicks: u64,
    pub query_bool_reads: u64,
    pub producer_post_release_flushes: u64,
    pub no_consumer_publications: u64,
}
fn query_kick_due(polls: u32, kicked: bool, threshold: u32) -> bool {
    threshold > 0 && !kicked && polls >= threshold
}
fn publication_id(requested: Option<u64>, sequence: u64) -> Result<u64> {
    let id = requested.unwrap_or(sequence);
    ensure!(id != 0, "Published GPU frame identity must be nonzero");
    Ok(id)
}

fn query_finished(status: HRESULT) -> Result<bool> {
    match status.0 {
        0 => Ok(true),
        1 => Ok(false), // S_FALSE is success-shaped, but NOT complete.
        _ => anyhow::bail!("GPU input completion query failed: {status:?}"),
    }
}

struct CachedInput {
    // Retaining canonical IUnknown prevents a reused raw address matching a new texture.
    identity: IUnknown,
    slice: u32,
    view: ID3D11VideoProcessorInputView,
}

/// One device/context, one shared output and at most four retained input views.
/// Rc marker deliberately prevents sending a live immediate context across workers.
pub struct Nv12Converter {
    config: ConvertConfig,
    device_identity: IUnknown,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    output: SharedOutput,
    output_view: ID3D11VideoProcessorOutputView,
    mutex: IDXGIKeyedMutex,
    inputs: VecDeque<CachedInput>,
    free_queries: VecDeque<ID3D11Query>,
    pending_reads: VecDeque<PendingRead>,
    owner: u64,
    next_sequence: u64,
    closed: bool,
    diagnostics: DiagnosticCounters,
    _worker: PhantomData<Rc<()>>,
}

impl Nv12Converter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        config: ConvertConfig,
    ) -> Result<Self> {
        let source = config.source()?;
        unsafe {
            // Below FL11, VideoProcessorBlt need not be covered by EVENT queries.
            ensure!(
                device.GetFeatureLevel().0 >= D3D_FEATURE_LEVEL_11_0.0,
                "GPU video input retirement requires feature level 11.0 or newer"
            );
            ensure!(
                context.GetType() == D3D11_DEVICE_CONTEXT_IMMEDIATE,
                "conversion needs the immediate context"
            );
            let device_identity: IUnknown = device.cast()?;
            ensure!(
                context.GetDevice()?.cast::<IUnknown>()? == device_identity,
                "conversion device and context differ"
            );
            let video_device: ID3D11VideoDevice =
                device.cast().context("D3D11 video device unavailable")?;
            let video_context: ID3D11VideoContext = context.cast()?;
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL {
                    Numerator: 30,
                    Denominator: 1,
                },
                InputWidth: config.coded_size.0,
                InputHeight: config.coded_size.1,
                OutputFrameRate: DXGI_RATIONAL {
                    Numerator: 30,
                    Denominator: 1,
                },
                OutputWidth: config.output_size.0,
                OutputHeight: config.output_size.1,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let enumerator = video_device.CreateVideoProcessorEnumerator(&content)?;
            ensure!(
                enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)?
                    & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32
                    != 0,
                "video processor cannot read NV12"
            );
            ensure!(
                enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM)?
                    & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32
                    != 0,
                "video processor cannot produce BGRA"
            );
            // D3D11.1 color-space setters are required; no driver-default color fallback.
            let vc1: ID3D11VideoContext1 = video_context
                .cast()
                .context("explicit SDR color spaces need D3D11.1")?;
            if let Ok(enumerator1) = enumerator.cast::<ID3D11VideoProcessorEnumerator1>() {
                ensure!(
                    enumerator1
                        .CheckVideoProcessorFormatConversion(
                            DXGI_FORMAT_NV12,
                            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                            DXGI_FORMAT_B8G8R8A8_UNORM,
                            DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                        )?
                        .as_bool(),
                    "GPU rejects BT.709 studio NV12 to full RGB conversion"
                );
            }
            let processor = video_device.CreateVideoProcessor(&enumerator, 0)?;
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamAlpha(&processor, 0, true, 1.0);
            video_context.VideoProcessorSetOutputAlphaFillMode(
                &processor,
                D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE_OPAQUE,
                0,
            );
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source));
            let target = RECT {
                left: 0,
                top: 0,
                right: config.output_size.0 as i32,
                bottom: config.output_size.1 as i32,
            };
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&target));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&target));
            vc1.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            vc1.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            let desc = D3D11_TEXTURE2D_DESC {
                Width: config.output_size.0,
                Height: config.output_size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
                MiscFlags: D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32,
                ..Default::default()
            };
            let mut texture = None;
            device.CreateTexture2D(&desc, None, Some(&mut texture))?;
            let texture = texture.context("missing shared BGRA output")?;
            let mutex = texture.cast()?;
            let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut output_view = None;
            video_device.CreateVideoProcessorOutputView(
                &texture,
                &enumerator,
                &view_desc,
                Some(&mut output_view),
            )?;
            let mut free_queries = VecDeque::with_capacity(COMPLETION_LIMIT);
            for _ in 0..COMPLETION_LIMIT {
                let mut query = None;
                device.CreateQuery(
                    &D3D11_QUERY_DESC {
                        Query: D3D11_QUERY_EVENT,
                        MiscFlags: 0,
                    },
                    Some(&mut query),
                )?;
                free_queries.push_back(query.context("missing GPU completion query")?);
            }
            let owner = NEXT_CONVERTER_ID
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .map_err(|_| anyhow::anyhow!("GPU converter identity exhausted"))?;
            Ok(Self {
                config,
                device_identity,
                context: context.clone(),
                video_device,
                video_context,
                enumerator,
                processor,
                output: SharedOutput {
                    texture,
                    width: config.output_size.0,
                    height: config.output_size.1,
                    published_frame: Arc::new(AtomicU64::new(0)),
                    consumer_release: Arc::new(AtomicU64::new(0)),
                },
                output_view: output_view.context("missing video output view")?,
                mutex,
                inputs: VecDeque::new(),
                free_queries,
                pending_reads: VecDeque::with_capacity(COMPLETION_LIMIT),
                owner,
                next_sequence: 1,
                closed: false,
                diagnostics: DiagnosticCounters::default(),
                _worker: PhantomData,
            })
        }
    }

    pub fn output(&self) -> SharedOutput {
        self.output.clone()
    }

    pub fn consumer_release_epoch(&self) -> &AtomicU64 {
        &self.output.consumer_release
    }

    /// Useful for verifying decoder-pool churn remains bounded, not a per-frame allocation count.
    pub fn cached_input_views(&self) -> usize {
        self.inputs.len()
    }

    pub fn pending_reads(&self) -> usize {
        self.pending_reads.len()
    }
    pub fn diagnostic_counters(&self) -> DiagnosticCounters {
        self.diagnostics
    }

    /// Seal submission immediately; callers may continue nonblocking completion polls.
    /// This does not release any caller-owned input frames or pretend GPU work ended.
    pub fn begin_shutdown(&mut self) {
        self.closed = true;
    }

    /// Poll one oldest read. Its first GetData permits command submission; later
    /// polls use DONOTFLUSH. No CPU completion wait/spin occurs. Only S_OK with a
    /// true EVENT BOOL retires its exact ticket; errors retain tickets and seal work.
    pub fn poll_completed(&mut self) -> Result<Option<ReadTicket>> {
        self.poll_completed_diagnostic(QueryDiagnostics::default())
    }

    /// Diagnostic alternatives remain asynchronous, with at most one explicit
    /// query kick per submission; no wait, spin, or implicit retirement is added.
    pub fn poll_completed_diagnostic(
        &mut self,
        options: QueryDiagnostics,
    ) -> Result<Option<ReadTicket>> {
        let Some(pending) = self.pending_reads.front_mut() else {
            return Ok(None);
        };
        pending.diagnostic_polls = pending.diagnostic_polls.saturating_add(1);
        let kick = query_kick_due(
            pending.diagnostic_polls,
            pending.diagnostic_kicked,
            options.kick_after_polls,
        );
        if kick {
            pending.diagnostic_kicked = true;
            self.diagnostics.query_kicks += 1;
        }
        let mut done = 0i32;
        if options.read_bool {
            self.diagnostics.query_bool_reads += 1;
        }
        let status = unsafe {
            // The generated GetData wrapper erases S_FALSE into Ok(()).
            (Interface::vtable(&self.context).GetData)(
                self.context.as_raw(),
                pending.query.as_raw(),
                if options.read_bool {
                    (&mut done as *mut i32).cast()
                } else {
                    std::ptr::null_mut()
                },
                if options.read_bool {
                    std::mem::size_of::<i32>() as u32
                } else {
                    0
                },
                if kick {
                    0
                } else {
                    D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32
                },
            )
        };
        if options.read_bool && status.0 == 0 && done == 0 {
            self.closed = true;
            anyhow::bail!("EVENT query returned S_OK without a true BOOL result");
        }
        match query_finished(status) {
            Ok(false) => Ok(None),
            Ok(true) => {
                let pending = self
                    .pending_reads
                    .pop_front()
                    .context("completion queue vanished")?;
                self.free_queries.push_back(pending.query);
                Ok(Some(pending.ticket))
            }
            Err(error) => {
                self.closed = true;
                Err(error)
            }
        }
    }

    /// Caller retains an owned AVFrame associated with Submitted.ticket before
    /// decoding again. Capacity exhaustion is explicit; no unbounded retirement queue.
    /// All errors returned directly occur before Blt. Errors after Blt are inside
    /// Submitted.publication so the lifetime obligation cannot be mistaken for rejection.
    pub fn publish_tracked(
        &mut self,
        texture: &ID3D11Texture2D,
        slice: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<TrackedPublish> {
        self.publish_tracked_diagnostic(
            texture,
            slice,
            cancelled,
            PublicationDiagnostics::default(),
        )
    }

    /// Publish a caller-owned nonzero frame identity under the shared mutex.
    /// Submitted still requires retaining its AVFrame even on publication error.
    pub fn publish_tracked_frame(
        &mut self,
        texture: &ID3D11Texture2D,
        slice: u32,
        frame_id: u64,
        cancelled: impl Fn() -> bool,
    ) -> Result<TrackedPublish> {
        self.publish_tracked_inner(
            texture,
            slice,
            cancelled,
            PublicationDiagnostics::default(),
            Some(frame_id),
        )
    }

    /// Controlled proof hook; normal publication always uses default ordering.
    pub fn publish_tracked_diagnostic(
        &mut self,
        texture: &ID3D11Texture2D,
        slice: u32,
        cancelled: impl Fn() -> bool,
        options: PublicationDiagnostics,
    ) -> Result<TrackedPublish> {
        self.publish_tracked_inner(texture, slice, cancelled, options, None)
    }

    fn publish_tracked_inner(
        &mut self,
        texture: &ID3D11Texture2D,
        slice: u32,
        cancelled: impl Fn() -> bool,
        options: PublicationDiagnostics,
        frame_id: Option<u64>,
    ) -> Result<TrackedPublish> {
        let frame_id = publication_id(frame_id, self.next_sequence)?;
        if self.closed {
            return Ok(TrackedPublish::Skipped(Skip::Closed));
        }
        if cancelled() {
            return Ok(TrackedPublish::Skipped(Skip::Cancelled));
        }
        if self.free_queries.is_empty() {
            return Ok(TrackedPublish::Skipped(Skip::QueueFull));
        }
        let input = self.input_view(texture, slice)?;
        if cancelled() {
            return Ok(TrackedPublish::Skipped(Skip::Cancelled));
        }
        let sequence = self.next_sequence;
        let next_sequence = sequence
            .checked_add(1)
            .context("GPU read ticket sequence exhausted")?;
        unsafe {
            let status = (Interface::vtable(&self.mutex).AcquireSync)(self.mutex.as_raw(), 0, 0);
            if status.0 == 0x102 {
                return Ok(TrackedPublish::Skipped(Skip::Busy));
            }
            ensure!(status.0 == 0, "shared BGRA acquire failed: {status:?}");
            // No fallible Rust operation after acquisition may lose the mutex or ticket.
            let query = self
                .free_queries
                .pop_front()
                .expect("query capacity checked before acquisition");
            let ticket = ReadTicket {
                owner: self.owner,
                sequence,
            };
            self.next_sequence = next_sequence;
            self.pending_reads.push_back(PendingRead {
                ticket,
                query: query.clone(),
                diagnostic_polls: 0,
                diagnostic_kicked: false,
            });
            let converted = self.blit(input);
            // Arm after every attempted read, including a failed Blt, before Flush.
            // End needs no Begin for EVENT, and a query is reused only after S_OK.
            self.context.End(&query);
            self.context.Flush();
            if converted.is_ok() && options.consumer_handoff {
                self.output
                    .published_frame
                    .store(frame_id, Ordering::Release);
            }
            let released =
                self.mutex
                    .ReleaseSync(if converted.is_ok() && options.consumer_handoff {
                        1
                    } else {
                        0
                    });
            if options.flush_after_release {
                self.context.Flush();
                self.diagnostics.producer_post_release_flushes += 1;
            }
            if !options.consumer_handoff {
                self.diagnostics.no_consumer_publications += 1;
            }
            let publication = converted
                .context("NV12 to BGRA video conversion")
                .and_then(|()| released.context("publish shared BGRA"));
            if publication.is_err() {
                self.closed = true;
            }
            Ok(TrackedPublish::Submitted {
                ticket,
                publication,
            })
        }
    }

    unsafe fn blit(&self, input: ID3D11VideoProcessorInputView) -> windows::core::Result<()> {
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(input)),
            ..Default::default()
        };
        let result = self.video_context.VideoProcessorBlt(
            &self.processor,
            &self.output_view,
            0,
            std::slice::from_ref(&stream),
        );
        ManuallyDrop::drop(&mut stream.pInputSurface);
        result
    }

    fn input_view(
        &mut self,
        texture: &ID3D11Texture2D,
        slice: u32,
    ) -> Result<ID3D11VideoProcessorInputView> {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            ensure!(
                desc.Format == DXGI_FORMAT_NV12,
                "conversion only accepts 8-bit NV12"
            );
            ensure!(
                (desc.Width, desc.Height) == self.config.coded_size,
                "NV12 texture extent changed"
            );
            ensure!(
                desc.ArraySize > slice,
                "NV12 array slice {slice} is out of range ({})",
                desc.ArraySize
            );
            ensure!(
                desc.MipLevels == 1
                    && desc.SampleDesc.Count == 1
                    && desc.SampleDesc.Quality == 0
                    && desc.Usage == D3D11_USAGE_DEFAULT
                    && desc.CPUAccessFlags == 0,
                "conversion needs a single-mip, non-MSAA GPU NV12 texture"
            );
            ensure!(
                texture.GetDevice()?.cast::<IUnknown>()? == self.device_identity,
                "NV12 texture belongs to another device"
            );
            let identity: IUnknown = texture.cast()?;
            if let Some(index) = self
                .inputs
                .iter()
                .position(|entry| entry.identity == identity && entry.slice == slice)
            {
                let entry = self
                    .inputs
                    .remove(index)
                    .context("input view cache index vanished")?;
                let view = entry.view.clone();
                self.inputs.push_back(entry);
                return Ok(view);
            }
            let view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: slice,
                    },
                },
            };
            let mut view = None;
            self.video_device.CreateVideoProcessorInputView(
                texture,
                &self.enumerator,
                &view_desc,
                Some(&mut view),
            )?;
            let view = view.context("missing NV12 input view")?;
            if self.inputs.len() == INPUT_CACHE_LIMIT {
                self.inputs.pop_front();
            }
            self.inputs.push_back(CachedInput {
                identity,
                slice,
                view: view.clone(),
            });
            Ok(view)
        }
    }

    /// Untracked conversion for independently synchronized callers such as the
    /// synthetic staging-readback proof. Decoder integration must use publish_tracked.
    /// Never waits for the GUI consumer. Published does NOT permit input recycling.
    pub fn publish(
        &mut self,
        texture: &ID3D11Texture2D,
        slice: u32,
        cancelled: impl Fn() -> bool,
    ) -> Result<Publish> {
        if cancelled() {
            return Ok(Publish::Cancelled);
        }
        ensure!(!self.closed, "GPU converter is closed");
        let input = self.input_view(texture, slice)?;
        if cancelled() {
            return Ok(Publish::Cancelled);
        }
        let frame_id = publication_id(None, self.next_sequence)?;
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .context("GPU frame sequence exhausted")?;
        unsafe {
            // WAIT_TIMEOUT is a positive HRESULT and windows-rs Result would accept it.
            let status = (Interface::vtable(&self.mutex).AcquireSync)(self.mutex.as_raw(), 0, 0);
            if status.0 == 0x102 {
                return Ok(Publish::Busy);
            }
            ensure!(status.0 == 0, "shared BGRA acquire failed: {status:?}");
            self.next_sequence = next_sequence;
            let converted = self.blit(input);
            self.context.Flush();
            // An unsuccessful conversion must not publish an uninitialized frame.
            if converted.is_ok() {
                self.output
                    .published_frame
                    .store(frame_id, Ordering::Release);
            }
            let released = self
                .mutex
                .ReleaseSync(if converted.is_ok() { 1 } else { 0 });
            converted.context("NV12 to BGRA video conversion")?;
            released.context("publish shared BGRA")?;
        }
        Ok(Publish::Published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normal_query_policy_kicks_first_poll_once_then_never_rekicks() {
        let defaults = QueryDiagnostics::default();
        assert_eq!(defaults.kick_after_polls, 1);
        assert!(defaults.read_bool);
        assert!(query_kick_due(1, false, defaults.kick_after_polls));
        assert!(!query_kick_due(2, true, defaults.kick_after_polls));
        assert!(!query_kick_due(u32::MAX, false, 0));
        assert!(!query_kick_due(99, false, 100));
        assert!(query_kick_due(100, false, 100));
        assert!(query_kick_due(101, false, 100));
        assert!(!query_kick_due(u32::MAX, true, 100));
        let publication = PublicationDiagnostics::default();
        assert!(publication.consumer_handoff);
        assert!(!publication.flush_after_release);
    }
    #[test]
    fn publication_identity_reserves_zero_and_preserves_callers_tag() {
        assert_eq!(publication_id(None, 1).unwrap(), 1);
        assert_eq!(publication_id(Some(77), 1).unwrap(), 77);
        assert!(publication_id(Some(0), 1).is_err());
        assert!(publication_id(None, 0).is_err());
    }
    #[test]
    fn success_shaped_pending_and_failure_never_retire_a_gpu_read() {
        assert!(!query_finished(HRESULT(1)).unwrap());
        assert!(query_finished(HRESULT(0)).unwrap());
        assert!(query_finished(HRESULT(0x887a0005u32 as i32)).is_err());
        assert!(query_finished(HRESULT(0x102)).is_err());
    }
    #[test]
    fn padded_visible_crop_and_downscale_are_validated_without_gpu() {
        let mut config = ConvertConfig {
            coded_size: (1920, 1088),
            source_rect: Some(RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }),
            output_size: (960, 540),
        };
        assert_eq!(config.source().unwrap().bottom, 1080);
        config.output_size = (1921, 540);
        assert!(config.source().is_err());
        config.output_size = (960, 540);
        config.source_rect.as_mut().unwrap().left = 1;
        assert!(config.source().is_err());
        config.source_rect.as_mut().unwrap().left = -2;
        assert!(config.source().is_err());
        config.source_rect = None;
        config.coded_size.0 = 0;
        assert!(config.source().is_err());
    }
}
