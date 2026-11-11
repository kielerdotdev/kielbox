//! D3D11 desktop capture and video processing. No staging textures or CPU Map.
use anyhow::{Context, Result, bail, ensure};
use media_core::{VideoCapture, VideoConfig};
use std::{
    mem::ManuallyDrop,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, HMODULE, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
    },
    core::Interface,
};

const RECOVERY_GRACE: Duration = Duration::from_secs(5);
const RECOVERY_RETRY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureRecoveryEvent {
    Recovering,
    Recovered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryStep {
    Retry,
    Wait,
    Expired,
}

struct RecoveryWindow {
    started: Instant,
    next_attempt: Instant,
}
impl RecoveryWindow {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            next_attempt: now,
        }
    }
    fn begin(window: &mut Option<Self>, now: Instant) -> bool {
        if window.is_some() {
            return false;
        }
        *window = Some(Self::new(now));
        true
    }
    fn step(&self, now: Instant) -> RecoveryStep {
        if now.saturating_duration_since(self.started) >= RECOVERY_GRACE {
            RecoveryStep::Expired
        } else if now >= self.next_attempt {
            RecoveryStep::Retry
        } else {
            RecoveryStep::Wait
        }
    }
    fn attempted(&mut self, now: Instant) {
        self.next_attempt = now + RECOVERY_RETRY;
    }
}

fn temporary_recovery_error(code: windows::core::HRESULT) -> bool {
    matches!(
        code,
        E_ACCESSDENIED
            | DXGI_ERROR_ACCESS_LOST
            | DXGI_ERROR_SESSION_DISCONNECTED
            | DXGI_ERROR_UNSUPPORTED
            | DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
            | DXGI_ERROR_NOT_FOUND
    )
}

pub struct Capture {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub adapter_name: String,
    duplication: Option<IDXGIOutputDuplication>,
    adapter_luid: (u32, i32),
    output_name: [u16; 32],
    physical_display: Option<crate::display::Resolved>,
    verify_before_frame: bool,
    recovery: Option<RecoveryWindow>,
    recovery_event: Option<CaptureRecoveryEvent>,
    pub source: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    pub has_frame: bool,
}

pub fn texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    shared: bool,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0
            | if format == DXGI_FORMAT_NV12 {
                0
            } else {
                D3D11_BIND_SHADER_RESOURCE.0
            }) as u32,
        CPUAccessFlags: 0,
        MiscFlags: if shared {
            D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32
        } else {
            0
        },
    };
    let mut result = None;
    // SAFETY: descriptor is valid; COM ownership is transferred to result.
    unsafe {
        device.CreateTexture2D(&desc, None, Some(&mut result))?;
    }
    result.context("D3D11 did not return a texture")
}

impl Capture {
    pub fn open(output_index: u32) -> Result<Self> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut monitor = 0;
            for ai in 0..32 {
                let Ok(adapter) = factory.EnumAdapters1(ai) else {
                    break;
                };
                let desc = adapter.GetDesc1()?;
                if desc.VendorId != 0x10de {
                    continue;
                }
                for oi in 0..32 {
                    let Ok(output) = adapter.EnumOutputs(oi) else {
                        break;
                    };
                    if monitor != output_index {
                        monitor += 1;
                        continue;
                    }
                    let od = output.GetDesc()?;
                    ensure!(
                        od.AttachedToDesktop.as_bool(),
                        "display is not attached to desktop"
                    );
                    ensure!(
                        od.Rotation == DXGI_MODE_ROTATION_IDENTITY
                            || od.Rotation == DXGI_MODE_ROTATION_UNSPECIFIED,
                        "rotated displays are not supported by the initial proof"
                    );
                    if let Ok(out6) = output.cast::<IDXGIOutput6>() {
                        ensure!(
                            out6.GetDesc1()?.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                            "HDR display detected; select an SDR display for this proof (HDR conversion is pending)"
                        );
                    }
                    let mut device = None;
                    let mut context = None;
                    D3D11CreateDevice(
                        &adapter,
                        D3D_DRIVER_TYPE_UNKNOWN,
                        HMODULE::default(),
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                        Some(&[D3D_FEATURE_LEVEL_11_0]),
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        None,
                        Some(&mut context),
                    )?;
                    let device = device.context("D3D11 device missing")?;
                    let context = context.context("D3D11 context missing")?;
                    let duplication = output.cast::<IDXGIOutput1>()?.DuplicateOutput(&device)
                        .context("Desktop Duplication failed (locked session, unavailable output, or capture limit)")?;
                    let dd = duplication.GetDesc();
                    let width = dd.ModeDesc.Width;
                    let height = dd.ModeDesc.Height;
                    let source =
                        texture(&device, width, height, DXGI_FORMAT_B8G8R8A8_UNORM, false)?;
                    return Ok(Self {
                        device,
                        context,
                        adapter_name: String::from_utf16_lossy(&desc.Description)
                            .trim_end_matches('\0')
                            .to_owned(),
                        duplication: Some(duplication),
                        adapter_luid: (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart),
                        output_name: od.DeviceName,
                        physical_display: None,
                        verify_before_frame: false,
                        recovery: None,
                        recovery_event: None,
                        source,
                        width,
                        height,
                        has_frame: false,
                    });
                }
            }
        }
        bail!(
            "NVIDIA-attached display {output_index} not found; hybrid-GPU capture is not supported yet"
        )
    }

    /// Opens an explicitly selected physical display, never falling back by ordinal or name.
    /// Legacy index resolution is rechecked after duplication and before any frame is copied.
    pub fn open_selected(selection: &crate::display::Selection) -> Result<Self> {
        let expected = crate::display::resolve(selection)?;
        let index = unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut ordinal = 0;
            let mut found = None;
            'adapters: for ai in 0..32 {
                let adapter = match factory.EnumAdapters1(ai) {
                    Ok(adapter) => adapter,
                    Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(error) => return Err(error.into()),
                };
                let desc = adapter.GetDesc1()?;
                if desc.VendorId != 0x10de {
                    continue;
                }
                for oi in 0..32 {
                    let output = match adapter.EnumOutputs(oi) {
                        Ok(output) => output,
                        Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                        Err(error) => return Err(error.into()),
                    };
                    if (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart)
                        == expected.adapter_luid
                        && output.GetDesc()?.DeviceName == expected.output_name
                    {
                        found = Some(ordinal);
                        break 'adapters;
                    }
                    ordinal += 1;
                }
            }
            found
                .context("The selected display connection changed. Refresh and select it again.")?
        };
        let mut capture = Self::open(index)?;
        ensure!(
            capture.adapter_luid == expected.adapter_luid
                && capture.output_name == expected.output_name,
            "The selected display changed while capture was starting. Refresh and select it again."
        );
        ensure!(
            capture.width == expected.info.width && capture.height == expected.info.height,
            "The selected display size changed while capture was starting. Refresh and select it again."
        );
        crate::display::verify(&expected)?;
        capture.physical_display = Some(expected);
        capture.verify_before_frame = true;
        Ok(capture)
    }

    pub fn take_recovery_event(&mut self) -> Option<CaptureRecoveryEvent> {
        self.recovery_event.take()
    }

    fn begin_recovery(&mut self, now: Instant) {
        self.duplication.take();
        // Repeated loss, including after duplication recreation, belongs to the
        // same outage until a fresh frame has actually been copied and released.
        if RecoveryWindow::begin(&mut self.recovery, now) {
            self.recovery_event = Some(CaptureRecoveryEvent::Recovering);
        }
    }

    fn recreate_duplication(&self) -> Result<IDXGIOutputDuplication> {
        if let Some(expected) = &self.physical_display {
            crate::display::verify(expected)?;
        }
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            for ai in 0..32 {
                let adapter = match factory.EnumAdapters1(ai) {
                    Ok(adapter) => adapter,
                    Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(error) => return Err(error.into()),
                };
                let desc = adapter.GetDesc1()?;
                if (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart) != self.adapter_luid {
                    continue;
                }
                for oi in 0..32 {
                    let output = match adapter.EnumOutputs(oi) {
                        Ok(output) => output,
                        Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                        Err(error) => return Err(error.into()),
                    };
                    let desc = output.GetDesc()?;
                    if desc.DeviceName != self.output_name {
                        continue;
                    }
                    if !desc.AttachedToDesktop.as_bool() {
                        return Err(windows::core::Error::from_hresult(DXGI_ERROR_NOT_FOUND).into());
                    }
                    ensure!(
                        desc.Rotation == DXGI_MODE_ROTATION_IDENTITY
                            || desc.Rotation == DXGI_MODE_ROTATION_UNSPECIFIED,
                        "display rotation changed; stop capture and select an unrotated SDR display"
                    );
                    if let Ok(output6) = output.cast::<IDXGIOutput6>() {
                        ensure!(
                            output6.GetDesc1()?.ColorSpace
                                == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                            "display changed to HDR; capture stopped because HDR conversion is unsupported"
                        );
                    }
                    let duplication = output
                        .cast::<IDXGIOutput1>()?
                        .DuplicateOutput(&self.device)?;
                    let mode = duplication.GetDesc().ModeDesc;
                    ensure!(
                        mode.Width == self.width && mode.Height == self.height,
                        "display size changed from {}×{} to {}×{}; capture stopped to preserve recording geometry",
                        self.width,
                        self.height,
                        mode.Width,
                        mode.Height
                    );
                    if let Some(expected) = &self.physical_display {
                        crate::display::verify(expected)?;
                    }
                    return Ok(duplication);
                }
                break;
            }
            Err(windows::core::Error::from_hresult(DXGI_ERROR_NOT_FOUND).into())
        }
    }

    fn recover_duplication(&mut self, timeout_ms: u32) -> Result<bool> {
        let Some(window) = &self.recovery else {
            return Ok(true);
        };
        let now = Instant::now();
        let step = window.step(now);
        ensure!(
            step != RecoveryStep::Expired,
            "display capture remained unavailable for 5 seconds; capture stopped after a desktop or session change"
        );
        if self.duplication.is_some() {
            return Ok(true);
        }
        if step == RecoveryStep::Wait {
            // Startup calls with a timeout must not spin while the desktop is
            // unavailable. The normal paced capture loop passes zero here.
            if timeout_ms > 0 {
                std::thread::sleep(
                    Duration::from_millis(u64::from(timeout_ms)).min(Duration::from_millis(20)),
                );
            }
            return Ok(false);
        }
        self.recovery.as_mut().unwrap().attempted(now);
        // A removed/reset device also invalidates NVENC and its registered
        // textures. Duplication-only recovery must never reuse such a device.
        unsafe { self.device.GetDeviceRemovedReason() }
            .context("capture GPU device was removed or reset; restart capture")?;
        let recreated = self.recreate_duplication();
        #[cfg(test)]
        tests::note_recreate(&recreated);
        match recreated {
            Ok(duplication) => {
                self.duplication = Some(duplication);
                self.verify_before_frame = self.physical_display.is_some();
                Ok(true)
            }
            Err(error)
                if error
                    .downcast_ref::<windows::core::Error>()
                    .is_some_and(|error| temporary_recovery_error(error.code())) =>
            {
                Ok(false)
            }
            Err(error) => {
                Err(error).context("could not recover desktop capture on the original GPU/display")
            }
        }
    }
}

impl VideoCapture for Capture {
    type Frame = ID3D11Texture2D;
    fn next_frame(&mut self, timeout_ms: u32) -> Result<Option<Self::Frame>> {
        if !self.recover_duplication(timeout_ms)? {
            return Ok(None);
        }
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;
            let acquired = self
                .duplication
                .as_ref()
                .context("desktop duplication is unavailable")?
                .AcquireNextFrame(timeout_ms, &mut info, &mut resource);
            #[cfg(test)]
            tests::note_acquire(&acquired, &info, resource.is_some());
            match acquired {
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    self.begin_recovery(Instant::now());
                    return Ok(None);
                }
                Err(e) => {
                    return Err(e).context(
                        "display capture lost; restart capture after a mode/session change",
                    );
                }
                Ok(()) => {}
            }
            // Release on *every* path after successful acquisition, including failed casts.
            let copied = (|| -> Result<bool> {
                if info.LastPresentTime == 0 && self.has_frame && self.recovery.is_none() {
                    return Ok(false);
                }
                let frame: ID3D11Texture2D =
                    resource.context("capture returned no resource")?.cast()?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                frame.GetDesc(&mut desc);
                ensure!(
                    desc.Width == self.width && desc.Height == self.height,
                    "display size changed; restart capture"
                );
                if self.verify_before_frame {
                    if let Some(expected) = &self.physical_display {
                        crate::display::verify(expected)?;
                    }
                }
                // Queue the GPU copy before giving the desktop-owned frame back to DXGI.
                self.context.CopyResource(&self.source, &frame);
                Ok(true)
            })();
            let released = self.duplication.as_ref().unwrap().ReleaseFrame();
            #[cfg(test)]
            tests::note_release(&released);
            let changed = copied?;
            if released
                .as_ref()
                .is_err_and(|error| error.code() == DXGI_ERROR_ACCESS_LOST)
            {
                self.begin_recovery(Instant::now());
                return Ok(None);
            }
            released?;
            if changed {
                self.verify_before_frame = false;
                self.has_frame = true;
                if self.recovery.take().is_some() {
                    self.recovery_event = Some(CaptureRecoveryEvent::Recovered);
                }
                Ok(Some(self.source.clone()))
            } else {
                Ok(None)
            }
        }
    }
}

pub struct Converter {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
    input: ID3D11VideoProcessorInputView,
    output_size: (u32, u32),
}

impl Converter {
    pub fn new(capture: &Capture, config: &VideoConfig) -> Result<Self> {
        unsafe {
            let video_device: ID3D11VideoDevice = capture.device.cast()?;
            let video_context: ID3D11VideoContext = capture.context.cast()?;
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL {
                    Numerator: config.fps,
                    Denominator: 1,
                },
                InputWidth: capture.width,
                InputHeight: capture.height,
                OutputFrameRate: DXGI_RATIONAL {
                    Numerator: config.fps,
                    Denominator: 1,
                },
                OutputWidth: config.width,
                OutputHeight: config.height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let enumerator = video_device.CreateVideoProcessorEnumerator(&desc)?;
            ensure!(
                enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)?
                    & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32
                    != 0,
                "GPU video processor cannot produce NV12"
            );
            let processor = video_device.CreateVideoProcessor(&enumerator, 0)?;
            let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let mut input = None;
            video_device.CreateVideoProcessorInputView(
                &capture.source,
                &enumerator,
                &input_desc,
                Some(&mut input),
            )?;
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetOutputAlphaFillMode(
                &processor,
                D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE_OPAQUE,
                0,
            );
            let source_rect = RECT {
                left: 0,
                top: 0,
                right: capture.width as i32,
                bottom: capture.height as i32,
            };
            let target_rect = RECT {
                left: 0,
                top: 0,
                right: config.width as i32,
                bottom: config.height as i32,
            };
            video_context.VideoProcessorSetStreamSourceRect(
                &processor,
                0,
                true,
                Some(&source_rect),
            );
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&target_rect));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&target_rect));
            // D3D11.1 provides explicit full-range RGB -> studio-range BT.709 semantics.
            let vc1: ID3D11VideoContext1 = video_context.cast()?;
            vc1.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            vc1.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            Ok(Self {
                video_device,
                video_context,
                processor,
                enumerator,
                input: input.context("video input missing")?,
                output_size: (config.width, config.height),
            })
        }
    }
    pub fn output_view(&self, target: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorOutputView> {
        let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output = None;
        unsafe {
            self.video_device.CreateVideoProcessorOutputView(
                target,
                &self.enumerator,
                &desc,
                Some(&mut output),
            )?;
        }
        output.context("video output view missing")
    }
    pub fn convert(&self, output: &ID3D11VideoProcessorOutputView, rgb: bool) -> Result<()> {
        unsafe {
            let vc1: ID3D11VideoContext1 = self.video_context.cast()?;
            vc1.VideoProcessorSetOutputColorSpace1(
                &self.processor,
                if rgb {
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709
                } else {
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
                },
            );
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(self.input.clone())),
                ..Default::default()
            };
            let result = self.video_context.VideoProcessorBlt(
                &self.processor,
                output,
                0,
                std::slice::from_ref(&stream),
            );
            // windows-rs maps embedded COM fields to ManuallyDrop; release our clone.
            ManuallyDrop::drop(&mut stream.pInputSurface);
            result?;
        }
        Ok(())
    }
    pub fn output_size(&self) -> (u32, u32) {
        self.output_size
    }
}

/// One bounded shared GPU surface for UI. Preview never blocks recording.
#[derive(Clone)]
pub struct PreviewSurface {
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    mutex: IDXGIKeyedMutex,
    view: ID3D11VideoProcessorOutputView,
}

impl PreviewSurface {
    pub fn new(device: &ID3D11Device, converter: &Converter) -> Result<Self> {
        let (width, height) = converter.output_size();
        let texture = texture(device, width, height, DXGI_FORMAT_B8G8R8A8_UNORM, true)?;
        let mutex = texture.cast()?;
        let view = converter.output_view(&texture)?;
        Ok(Self {
            texture,
            width,
            height,
            mutex,
            view,
        })
    }
    pub fn publish(&self, converter: &Converter, context: &ID3D11DeviceContext) -> Result<bool> {
        unsafe {
            // WAIT_TIMEOUT is a positive HRESULT: windows-rs Result alone cannot distinguish it.
            let status = (Interface::vtable(&self.mutex).AcquireSync)(self.mutex.as_raw(), 0, 0);
            if status.0 == 0x102 {
                return Ok(false);
            }
            ensure!(status.0 == 0, "preview mutex acquire failed: {status:?}");
            let converted = converter.convert(&self.view, true);
            context.Flush();
            // Failed conversion must not advertise uninitialized/stale pixels.
            let released = self
                .mutex
                .ReleaseSync(if converted.is_ok() { 1 } else { 0 });
            converted?;
            released?;
        }
        Ok(true)
    }
}

#[cfg(test)]
#[path = "gpu_session_diagnostics.rs"]
mod session_diagnostics;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FrameDiagnostics {
        owned_stimulus: bool,
        stimulus_updates: u64,
        acquire_calls: u64,
        acquire_success: u64,
        acquire_wait_timeout: u64,
        acquire_access_lost: u64,
        acquire_other_error: u64,
        resources_returned: u64,
        desktop_present_updates: u64,
        pointer_updates: u64,
        release_calls: u64,
        release_success: u64,
        release_error: u64,
        recreation_attempts: u64,
        recreation_success: u64,
        last_acquire_hresult: Option<String>,
        last_release_hresult: Option<String>,
        last_recreation_error: Option<String>,
    }
    impl FrameDiagnostics {
        fn json(&self) -> serde_json::Value {
            serde_json::json!({"owned_stimulus": self.owned_stimulus, "stimulus_updates": self.stimulus_updates,
                "acquire_calls": self.acquire_calls, "acquire_success": self.acquire_success,
                "acquire_wait_timeout": self.acquire_wait_timeout, "acquire_access_lost": self.acquire_access_lost,
                "acquire_other_error": self.acquire_other_error, "resources_returned": self.resources_returned,
                "desktop_present_updates": self.desktop_present_updates, "pointer_updates": self.pointer_updates,
                "release_calls": self.release_calls, "release_success": self.release_success, "release_error": self.release_error,
                "recreation_attempts": self.recreation_attempts, "recreation_success": self.recreation_success,
                "last_acquire_hresult": self.last_acquire_hresult, "last_release_hresult": self.last_release_hresult,
                "last_recreation_error": self.last_recreation_error})
        }
    }
    thread_local! {
        static FRAME_DIAGNOSTICS: std::cell::RefCell<FrameDiagnostics> = std::cell::RefCell::new(FrameDiagnostics::default());
    }
    fn stimulus_requested() -> bool {
        std::env::var_os("RCR_GPU_PROOF_STIMULUS").is_some_and(|value| value == "1")
    }
    struct OwnedStimulus {
        window: windows::Win32::Foundation::HWND,
        x: i32,
        y: i32,
        alternate: bool,
        next_update: Instant,
    }
    impl OwnedStimulus {
        fn new(capture: &Capture) -> Result<Self> {
            use windows::Win32::UI::WindowsAndMessaging::*;
            let bounds = (|| -> Result<RECT> {
                let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
                for index in 0..32 {
                    let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                        break;
                    };
                    let desc = unsafe { adapter.GetDesc1()? };
                    if (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart) != capture.adapter_luid
                    {
                        continue;
                    }
                    for index in 0..16 {
                        let Ok(output) = (unsafe { adapter.EnumOutputs(index) }) else {
                            break;
                        };
                        let desc = unsafe { output.GetDesc()? };
                        if desc.DeviceName == capture.output_name
                            && desc.AttachedToDesktop.as_bool()
                        {
                            return Ok(desc.DesktopCoordinates);
                        }
                    }
                }
                bail!("Cannot place owned GPU proof surface on the selected output");
            })()?;
            // SS_BLACKRECT is the SDK's STATIC style 0x0004. Keep this test-only
            // style local instead of enabling another Windows feature module.
            let window = unsafe {
                CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                    windows::core::w!("STATIC"),
                    windows::core::w!("RCR owned GPU proof surface"),
                    WS_POPUP | WINDOW_STYLE(0x0004),
                    bounds.left + 8,
                    bounds.top + 8,
                    32,
                    32,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .context("create owned nonactivating GPU proof surface")?;
            let mut stimulus = Self {
                window,
                x: bounds.left + 8,
                y: bounds.top + 8,
                alternate: false,
                next_update: Instant::now(),
            };
            stimulus.tick(true)?;
            FRAME_DIAGNOSTICS.with_borrow_mut(|diagnostics| diagnostics.owned_stimulus = true);
            Ok(stimulus)
        }
        fn tick(&mut self, force: bool) -> Result<()> {
            use windows::Win32::{
                Graphics::Gdi::{InvalidateRect, UpdateWindow},
                UI::WindowsAndMessaging::*,
            };
            let now = Instant::now();
            if !force && now < self.next_update {
                return Ok(());
            }
            self.alternate = !self.alternate;
            unsafe {
                SetWindowPos(
                    self.window,
                    Some(HWND_TOPMOST),
                    self.x + i32::from(self.alternate),
                    self.y,
                    32,
                    32,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )?;
                let _ = InvalidateRect(Some(self.window), None, true);
                let _ = UpdateWindow(self.window);
                let mut message = MSG::default();
                for _ in 0..32 {
                    if !PeekMessageW(&mut message, Some(self.window), 0, 0, PM_REMOVE).as_bool() {
                        break;
                    }
                    let _ = DispatchMessageW(&message);
                }
            }
            self.next_update = now + Duration::from_millis(100);
            FRAME_DIAGNOSTICS.with_borrow_mut(|diagnostics| diagnostics.stimulus_updates += 1);
            Ok(())
        }
    }
    impl Drop for OwnedStimulus {
        fn drop(&mut self) {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.window);
            }
        }
    }
    pub(super) fn note_acquire(
        result: &windows::core::Result<()>,
        info: &DXGI_OUTDUPL_FRAME_INFO,
        resource: bool,
    ) {
        FRAME_DIAGNOSTICS.with_borrow_mut(|diagnostics| {
            diagnostics.acquire_calls += 1;
            let code = result
                .as_ref()
                .err()
                .map_or(windows::core::HRESULT(0), |error| error.code());
            diagnostics.last_acquire_hresult = Some(format!("0x{:08x}", code.0 as u32));
            match code {
                windows::core::HRESULT(0) => diagnostics.acquire_success += 1,
                DXGI_ERROR_WAIT_TIMEOUT => diagnostics.acquire_wait_timeout += 1,
                DXGI_ERROR_ACCESS_LOST => diagnostics.acquire_access_lost += 1,
                _ => diagnostics.acquire_other_error += 1,
            }
            diagnostics.resources_returned += u64::from(resource);
            diagnostics.desktop_present_updates += u64::from(info.LastPresentTime != 0);
            diagnostics.pointer_updates += u64::from(info.LastMouseUpdateTime != 0);
        });
    }
    pub(super) fn note_release(result: &windows::core::Result<()>) {
        FRAME_DIAGNOSTICS.with_borrow_mut(|diagnostics| {
            diagnostics.release_calls += 1;
            diagnostics.release_success += u64::from(result.is_ok());
            diagnostics.release_error += u64::from(result.is_err());
            diagnostics.last_release_hresult = Some(format!(
                "0x{:08x}",
                result
                    .as_ref()
                    .err()
                    .map_or(0, |error| error.code().0 as u32)
            ));
        });
    }
    pub(super) fn note_recreate(result: &Result<IDXGIOutputDuplication>) {
        FRAME_DIAGNOSTICS.with_borrow_mut(|diagnostics| {
            diagnostics.recreation_attempts += 1;
            diagnostics.recreation_success += u64::from(result.is_ok());
            if let Err(error) = result {
                diagnostics.last_recreation_error = Some(format!("{error:#}"));
            }
        });
    }
    fn native_diagnostics(capture: &Capture, stage: &str) -> serde_json::Value {
        let outputs = (|| -> Result<Vec<serde_json::Value>> {
            let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
            for index in 0..32 {
                let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                    break;
                };
                let desc = unsafe { adapter.GetDesc1()? };
                if (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart) != capture.adapter_luid {
                    continue;
                }
                let mut outputs = Vec::new();
                for index in 0..16 {
                    let Ok(output) = (unsafe { adapter.EnumOutputs(index) }) else {
                        break;
                    };
                    let desc = unsafe { output.GetDesc()? };
                    let name = String::from_utf16_lossy(&desc.DeviceName)
                        .trim_end_matches('\0')
                        .to_owned();
                    let color = output
                        .cast::<IDXGIOutput6>()
                        .ok()
                        .and_then(|output| unsafe { output.GetDesc1().ok() })
                        .map(|desc| desc.ColorSpace.0);
                    outputs.push(serde_json::json!({"name": name, "selected": desc.DeviceName == capture.output_name,
                        "attached": desc.AttachedToDesktop.as_bool(), "rotation": desc.Rotation.0,
                        "desktop_rect": [desc.DesktopCoordinates.left, desc.DesktopCoordinates.top, desc.DesktopCoordinates.right, desc.DesktopCoordinates.bottom],
                        "color_space": color}));
                }
                return Ok(outputs);
            }
            Ok(Vec::new())
        })();
        let device_status = unsafe { capture.device.GetDeviceRemovedReason() };
        FRAME_DIAGNOSTICS.with_borrow(|diagnostics| serde_json::json!({
            "proof": "GPU-only duplication diagnostics", "stage": stage,
            "session_context": session_diagnostics::snapshot(),
            "owned_stimulus_requested": stimulus_requested(), "owned_stimulus": diagnostics.owned_stimulus,
            "adapter": capture.adapter_name, "adapter_luid": capture.adapter_luid,
            "selected_output": String::from_utf16_lossy(&capture.output_name).trim_end_matches('\0'),
            "width": capture.width, "height": capture.height, "has_frame": capture.has_frame,
            "duplication_present": capture.duplication.is_some(), "recovering": capture.recovery.is_some(),
            "device_removed_hresult": format!("0x{:08x}", device_status.err().map_or(0, |error| error.code().0 as u32)),
            "outputs": outputs.as_ref().ok(), "output_query_error": outputs.as_ref().err().map(|error| format!("{error:#}")),
            "frames": diagnostics.json(),
        }))
    }

    /// Exercises real DuplicateOutput recreation, but injects the transition;
    /// it does not prove driver access-loss, mode-switch, or lock-screen recovery.
    #[test]
    #[ignore = "requires an NVIDIA-attached SDR desktop; GPU-only injected duplication recovery proof"]
    fn native_gpu_same_output_duplication_recovery_preserves_resources() -> Result<()> {
        FRAME_DIAGNOSTICS.with_borrow_mut(|diagnostics| *diagnostics = FrameDiagnostics::default());
        let mut capture = Capture::open(0)?;
        let mut stimulus = if stimulus_requested() {
            match OwnedStimulus::new(&capture) {
                Ok(stimulus) => Some(stimulus),
                Err(error) => {
                    println!("{}", native_diagnostics(&capture, "owned-stimulus-error"));
                    return Err(error);
                }
            }
        } else {
            None
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !capture.has_frame {
            if let Some(stimulus) = &mut stimulus {
                stimulus.tick(false)?;
            }
            if Instant::now() >= deadline {
                println!("{}", native_diagnostics(&capture, "initial-frame-timeout"));
                bail!(
                    "GPU recovery proof received no initial frame in five seconds; inspect metadata diagnostics"
                );
            }
            if let Err(error) = capture.next_frame(50) {
                println!("{}", native_diagnostics(&capture, "initial-frame-error"));
                return Err(error);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let original = (
            capture.source.as_raw() as usize,
            capture.device.as_raw() as usize,
            capture.context.as_raw() as usize,
            capture.width,
            capture.height,
        );
        let mut recoveries = Vec::new();
        for round in 1..=2 {
            capture.take_recovery_event();
            ensure!(
                capture.duplication.is_some(),
                "no active duplication before injection"
            );
            let began = Instant::now();
            capture.begin_recovery(began);
            assert!(
                capture.duplication.is_none(),
                "injection must release the original duplication"
            );
            assert_eq!(
                capture.take_recovery_event(),
                Some(CaptureRecoveryEvent::Recovering)
            );
            assert!(
                capture.has_frame,
                "recovery must retain the last valid image"
            );
            let mut recovered = false;
            while !recovered {
                if let Some(stimulus) = &mut stimulus {
                    stimulus.tick(false)?;
                }
                if began.elapsed() >= RECOVERY_GRACE {
                    println!("{}", native_diagnostics(&capture, "recovery-timeout"));
                    bail!("injected recovery did not confirm a fresh frame in five seconds");
                }
                let duplication_was_missing = capture.duplication.is_none();
                let frame = match capture.next_frame(50) {
                    Ok(frame) => frame,
                    Err(error) => {
                        println!("{}", native_diagnostics(&capture, "recovery-error"));
                        return Err(error);
                    }
                };
                if duplication_was_missing && capture.duplication.is_some() {
                    if let Some(stimulus) = &mut stimulus {
                        stimulus.tick(true)?;
                    }
                }
                let resources = (
                    capture.source.as_raw() as usize,
                    capture.device.as_raw() as usize,
                    capture.context.as_raw() as usize,
                    capture.width,
                    capture.height,
                );
                assert_eq!(
                    resources, original,
                    "recovery replaced capture resources or geometry"
                );
                if capture.take_recovery_event() == Some(CaptureRecoveryEvent::Recovered) {
                    ensure!(
                        frame.is_some(),
                        "recovery event preceded the fresh copied/released frame"
                    );
                    recovered = true;
                }
                if !recovered {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            assert!(
                capture.has_frame && capture.duplication.is_some() && capture.recovery.is_none()
            );
            recoveries.push(serde_json::json!({"round": round, "elapsed_ms": began.elapsed().as_secs_f64() * 1000.}));
        }
        println!("{}", native_diagnostics(&capture, "completed"));
        println!(
            "{}",
            serde_json::json!({
                "proof": "GPU-only same-output duplication recreation", "injected_transition": true,
                "owned_stimulus": stimulus.is_some(),
                "real_mode_or_session_change_tested": false,
                "adapter": capture.adapter_name, "width": original.3, "height": original.4,
                "source_pointer": format!("0x{:x}", original.0),
                "device_pointer": format!("0x{:x}", original.1),
                "context_pointer": format!("0x{:x}", original.2),
                "same_resources": true, "recoveries": recoveries,
            })
        );
        Ok(())
    }

    #[test]
    fn repeated_access_loss_cannot_extend_the_unconfirmed_recovery_deadline() {
        let start = Instant::now();
        let mut recovery = None;
        assert!(RecoveryWindow::begin(&mut recovery, start));
        let window = recovery.as_mut().unwrap();
        assert_eq!(window.step(start), RecoveryStep::Retry);
        window.attempted(start);
        assert_eq!(
            window.step(start + Duration::from_millis(249)),
            RecoveryStep::Wait
        );
        assert_eq!(window.step(start + RECOVERY_RETRY), RecoveryStep::Retry);

        // Recreating duplication without a copied/released frame does not count
        // as recovery: a second Acquire/Release ACCESS_LOST keeps this window.
        assert!(!RecoveryWindow::begin(
            &mut recovery,
            start + Duration::from_secs(4)
        ));
        let window = recovery.as_mut().unwrap();
        window.attempted(start + RECOVERY_GRACE - Duration::from_millis(1));
        assert_eq!(window.step(start + RECOVERY_GRACE), RecoveryStep::Expired);
        assert_eq!(window.started, start);

        // A genuinely confirmed fresh frame clears the outage in next_frame.
        recovery.take();
        assert!(RecoveryWindow::begin(&mut recovery, start + RECOVERY_GRACE));
        assert_eq!(
            recovery.unwrap().step(start + RECOVERY_GRACE),
            RecoveryStep::Retry
        );
    }

    #[test]
    fn recovery_retries_are_paced_and_bounded_during_a_persistent_outage() {
        let start = Instant::now();
        let mut recovery = RecoveryWindow::new(start);
        let mut attempts = 0;
        for milliseconds in 0..=5000 {
            let now = start + Duration::from_millis(milliseconds);
            match recovery.step(now) {
                RecoveryStep::Retry => {
                    attempts += 1;
                    recovery.attempted(now);
                }
                RecoveryStep::Wait => {}
                RecoveryStep::Expired => assert_eq!(milliseconds, 5000),
            }
        }
        assert_eq!(attempts, 20);
    }

    #[test]
    fn transition_errors_retry_but_invalid_or_removed_devices_do_not() {
        for error in [
            E_ACCESSDENIED,
            DXGI_ERROR_ACCESS_LOST,
            DXGI_ERROR_UNSUPPORTED,
            DXGI_ERROR_SESSION_DISCONNECTED,
            DXGI_ERROR_NOT_CURRENTLY_AVAILABLE,
            DXGI_ERROR_NOT_FOUND,
        ] {
            assert!(temporary_recovery_error(error));
        }
        for error in [
            DXGI_ERROR_DEVICE_REMOVED,
            DXGI_ERROR_DEVICE_RESET,
            DXGI_ERROR_DEVICE_HUNG,
            DXGI_ERROR_DRIVER_INTERNAL_ERROR,
            DXGI_ERROR_INVALID_CALL,
        ] {
            assert!(!temporary_recovery_error(error));
        }
    }
}
