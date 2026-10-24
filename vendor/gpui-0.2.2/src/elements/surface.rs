use crate::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    ObjectFit, Pixels, Style, StyleRefinement, Styled, Window,
};
#[cfg(target_os = "macos")]
use core_video::pixel_buffer::CVPixelBuffer;
use refineable::Refineable;

/// A source of a surface's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceSource {
    /// A macOS image buffer from CoreVideo
    #[cfg(target_os = "macos")]
    Surface(CVPixelBuffer),
    /// A shared, keyed-mutex D3D11 BGRA texture. Pixels never enter the CPU atlas.
    #[cfg(target_os = "windows")]
    WindowsGpu(WindowsGpuSurface),
}

/// External GPU image supported by RCR's pinned Windows GPUI extension.
#[cfg(target_os = "windows")]
#[derive(Clone, Debug)]
pub struct WindowsGpuSurface {
    pub(crate) texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) draws: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub(crate) published_frame: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    pub(crate) consumer_release: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    pub(crate) submitted_frame: std::sync::Arc<std::sync::atomic::AtomicU64>,
    error: std::sync::Arc<std::sync::Mutex<Option<&'static str>>>,
}

#[cfg(target_os = "windows")]
impl PartialEq for WindowsGpuSurface {
    fn eq(&self, other: &Self) -> bool { self.texture == other.texture }
}
#[cfg(target_os = "windows")]
impl Eq for WindowsGpuSurface {}

#[cfg(target_os = "windows")]
impl WindowsGpuSurface {
    /// The producer must acquire key 0 before writing, then release key 1.
    /// GPUI acquires key 1 for each new publication, copies into its private image,
    /// then releases 0 before drawing. Redraws reuse that private image without
    /// copying unpublished writes or waiting for the producer.
    pub fn new(texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D) -> anyhow::Result<Self> {
        use windows::Win32::Graphics::{Direct3D11::*, Dxgi::Common::*};
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc); }
        anyhow::ensure!(desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM && desc.SampleDesc.Count == 1 &&
            desc.MiscFlags & D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32 != 0 &&
            desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0,
            "GPUI external surface must be a shared keyed-mutex BGRA shader resource");
        Ok(Self { texture, width: desc.Width, height: desc.Height, draws: Default::default(),
            published_frame: None, consumer_release: None, submitted_frame: Default::default(), error: Default::default() })
    }
    /// Attach before first use. The producer stores a nonzero frame ID while
    /// holding key 0, before releasing key 1. Zero denotes no publication.
    /// GPUI reads it under that same mutex, matching metadata to copied pixels.
    pub fn with_frame_counter(mut self, published: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
        self.published_frame = Some(published);
        self
    }
    /// Optional same-process producer wake address. Incremented and signalled
    /// only after a fresh publication was copied and key 0 released. Cached
    /// redraws do not signal. This never transfers keyed-mutex ownership.
    pub fn with_consumer_release(mut self, epoch: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
        self.consumer_release = Some(epoch);
        self
    }
    /// Last tracked frame submitted for GPU drawing, including cached redraws.
    /// Zero means no tracked submission; this is not physical monitor scanout.
    pub fn submitted_frame_id(&self) -> u64 {
        self.submitted_frame.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Number of GPU draws submitted for this surface (including redraws).
    pub fn draw_count(&self) -> u64 { self.draws.load(std::sync::atomic::Ordering::Relaxed) }

    /// Sticky, bounded preview failure shared by all clones. A new producer
    /// surface starts with no error; capture/other scene drawing can continue.
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).map(str::to_owned)
    }

    pub(crate) fn has_error(&self) -> bool {
        self.error.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_some()
    }

    pub(crate) fn record_error(&self, reason: &'static str) -> bool {
        let mut error = self.error.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if error.is_some() { return false; }
        *error = Some(if reason.len() <= 512 { reason } else { "GPU preview unavailable." });
        true
    }
}

#[cfg(target_os = "windows")]
impl From<WindowsGpuSurface> for SurfaceSource {
    fn from(value: WindowsGpuSurface) -> Self { Self::WindowsGpu(value) }
}

#[cfg(target_os = "macos")]
impl From<CVPixelBuffer> for SurfaceSource {
    fn from(value: CVPixelBuffer) -> Self {
        SurfaceSource::Surface(value)
    }
}

/// A surface element.
pub struct Surface {
    source: SurfaceSource,
    object_fit: ObjectFit,
    style: StyleRefinement,
}

/// Create a new surface element.
pub fn surface(source: impl Into<SurfaceSource>) -> Surface {
    Surface {
        source: source.into(),
        object_fit: ObjectFit::Contain,
        style: Default::default(),
    }
}

impl Surface {
    /// Set the object fit for the image.
    pub fn object_fit(mut self, object_fit: ObjectFit) -> Self {
        self.object_fit = object_fit;
        self
    }
}

impl Element for Surface {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout_id = window.request_layout(style, [], cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] window: &mut Window,
        _: &mut App,
    ) {
        match &self.source {
            #[cfg(target_os = "windows")]
            SurfaceSource::WindowsGpu(surface) => {
                let size = crate::size(surface.width.into(), surface.height.into());
                let new_bounds = self.object_fit.get_bounds(bounds, size);
                window.paint_windows_surface(new_bounds, surface.clone());
            }
            #[cfg(target_os = "macos")]
            SurfaceSource::Surface(surface) => {
                let size = crate::size(surface.get_width().into(), surface.get_height().into());
                let new_bounds = self.object_fit.get_bounds(bounds, size);
                // TODO: Add support for corner_radii
                window.paint_surface(new_bounds, surface.clone());
            }
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }
}

impl IntoElement for Surface {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for Surface {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
