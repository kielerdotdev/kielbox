use anyhow::{Context, Result, ensure};
use windows::{
    Win32::Graphics::{Direct3D11::*, Dxgi::{Common::*, *}},
    core::Interface,
};

/// One imported producer and one consumer-owned last-good image. Retaining the
/// producer also prevents legacy shared-handle reuse while this import is live.
pub(super) struct ExternalSurface {
    pub(super) producer: ID3D11Texture2D,
    imported: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
    image: ID3D11Texture2D,
    pub(super) view: ID3D11ShaderResourceView,
    has_frame: bool,
    pub(super) frame_id: u64,
}

impl ExternalSurface {
    pub(super) fn new(device: &ID3D11Device, producer: &ID3D11Texture2D) -> std::result::Result<Self, &'static str> {
        const IMPORT_FAILED: &str = "GPU preview unavailable: the shared image could not be imported.";
        let adapters = (|| -> Result<_> {
            unsafe {
                let producer_adapter = producer.GetDevice()?.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?;
                let consumer_adapter = device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?;
                Ok((producer_adapter.AdapterLuid, consumer_adapter.AdapterLuid))
            }
        })().map_err(|_| IMPORT_FAILED)?;
        if adapters.0.LowPart != adapters.1.LowPart || adapters.0.HighPart != adapters.1.HighPart {
            return Err("GPU preview unavailable: capture and the interface use different graphics adapters.");
        }
        Self::new_checked(device, producer).map_err(|_| IMPORT_FAILED)
    }

    fn new_checked(device: &ID3D11Device, producer: &ID3D11Texture2D) -> Result<Self> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { producer.GetDesc(&mut desc); }
        ensure!(desc.Width > 0 && desc.Height > 0 && desc.MipLevels == 1 && desc.ArraySize == 1 &&
            desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM && desc.SampleDesc.Count == 1 &&
            desc.SampleDesc.Quality == 0 && desc.Usage == D3D11_USAGE_DEFAULT &&
            desc.CPUAccessFlags == 0 &&
            desc.MiscFlags & D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32 != 0 &&
            desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0,
            "external GPUI texture must be a single shared keyed-mutex BGRA image");
        let shared = unsafe { producer.cast::<IDXGIResource>()?.GetSharedHandle()? };
        let mut imported: Option<ID3D11Texture2D> = None;
        unsafe { device.OpenSharedResource(shared, &mut imported)?; }
        let imported = imported.context("open external GPUI texture")?;
        let mut actual = D3D11_TEXTURE2D_DESC::default();
        unsafe { imported.GetDesc(&mut actual); }
        ensure!(actual.Width == desc.Width && actual.Height == desc.Height &&
            actual.Format == desc.Format && actual.MipLevels == 1 && actual.ArraySize == 1 &&
            actual.SampleDesc.Count == 1 && actual.SampleDesc.Quality == 0,
            "external GPUI import description changed");
        let mutex = imported.cast()?;
        // No shared flag on the private copy: GPUI can sample it after releasing
        // the producer mutex, including while a future producer write is busy.
        desc.MiscFlags = 0;
        desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
        let mut image = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut image))?; }
        let image = image.context("private external GPUI texture")?;
        let mut view = None;
        unsafe { device.CreateShaderResourceView(&image, None, Some(&mut view))?; }
        Ok(Self { producer: producer.clone(), imported, mutex, image,
            view: view.context("private external GPUI texture SRV")?, has_frame: false, frame_id: 0 })
    }

    /// Never wait for a producer. A timeout leaves the private image untouched.
    /// Only a fresh key1 publication establishes the first valid frame. Initial
    /// key0 ownership does not mean the producer has initialized its texture.
    /// WAIT_TIMEOUT/WAIT_ABANDONED are positive HRESULTs, so `is_ok` is unsafe here.
    pub(super) fn refresh(&mut self, context: &ID3D11DeviceContext,
        published_frame: Option<&std::sync::atomic::AtomicU64>,
        consumer_release: Option<&std::sync::atomic::AtomicU64>) -> Result<bool> {
        let acquire = |key| unsafe {
            (Interface::vtable(&self.mutex).AcquireSync)(self.mutex.as_raw(), key, 0)
        };
        // Key 0 is not a new publication. Drawing the private image needs no
        // recopy, and a producer may deliberately release 0 after a failed write.
        let status = acquire(1);
        if status.0 == 0x102 { return Ok(self.has_frame); }
        ensure!(status.0 == 0, "external texture mutex: {status:?}");
        let frame_id = published_frame.map_or(0, |frame| frame.load(std::sync::atomic::Ordering::Acquire));
        unsafe {
            context.CopyResource(&self.image, &self.imported);
            // Submit the cross-device read before returning ownership. This
            // enqueues GPU work; it neither maps pixels nor waits for the GPU.
            context.Flush();
            self.mutex.ReleaseSync(0)?;
        }
        if let Some(epoch) = consumer_release {
            epoch.fetch_add(1, std::sync::atomic::Ordering::Release);
            // Atomic storage remains alive through the surface's Arc. The
            // producer compares its pre-acquisition epoch before waiting, so a
            // release immediately before WaitOnAddress cannot be lost.
            unsafe { windows::Win32::System::Threading::WakeByAddressSingle(epoch.as_ptr().cast()); }
        }
        self.has_frame = true;
        self.frame_id = frame_id;
        Ok(true)
    }
}
