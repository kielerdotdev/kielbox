//! Bounded D3D11 → Rune/D3D12 preview bridge. No pixel mapping or CPU readback.
//! Copy on a separate device while owning the producer's keyed mutex. A query
//! establishes completion before publication; Rune's release fence guards reuse.
use anyhow::{Context, Result, ensure};
use rune::{NativeSurface, native_surface::windows::SharedTexture};
use std::sync::atomic::{AtomicU64, Ordering};
use windows::{
    Win32::{
        Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
    },
    core::{HSTRING, Interface},
};
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
#[derive(Clone, Copy)]
enum Stage {
    Free,
    Copying(u64),
    Published(u64),
}
struct Slot {
    texture: ID3D11Texture2D,
    query: ID3D11Query,
    name: String,
    _handle: Handle,
    stage: Stage,
}
pub struct Bridge {
    _producer: ID3D11Texture2D,
    imported: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
    context: ID3D11DeviceContext,
    release: ID3D11Fence,
    release_name: String,
    _fence_handle: Handle,
    slots: Vec<Slot>,
    surface: NativeSurface,
    width: u32,
    height: u32,
    next: u64,
    published: u64,
}
impl Bridge {
    pub fn diagnostic(&self) -> String {
        format!(
            "next={}, published={}, released={}, error={:?}",
            self.next,
            self.published,
            unsafe { self.release.GetCompletedValue() },
            self.surface.last_error()
        )
    }
    pub fn new(producer: ID3D11Texture2D, surface: NativeSurface) -> Result<Self> {
        unsafe {
            let producer_device = producer.GetDevice()?;
            let adapter = producer_device.cast::<IDXGIDevice>()?.GetAdapter()?;
            let luid = adapter.GetDesc()?.AdapterLuid;
            if let Some((high, low)) = rune::native_surface::windows::adapter_luid() {
                ensure!(
                    luid.HighPart == high && luid.LowPart == low,
                    "Capture and Rune use different graphics adapters."
                );
            }
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.context("Create preview copy device")?;
            let context = context.context("Create preview copy context")?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            producer.GetDesc(&mut desc);
            ensure!(
                desc.Width > 0
                    && desc.Height > 0
                    && desc.MipLevels == 1
                    && desc.ArraySize == 1
                    && desc.SampleDesc.Count == 1
                    && desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM,
                "Preview source must be single-plane BGRA8"
            );
            let shared = producer.cast::<IDXGIResource>()?.GetSharedHandle()?;
            let mut imported = None;
            device.OpenSharedResource(shared, &mut imported)?;
            let imported: ID3D11Texture2D = imported.context("Import capture preview")?;
            let mutex = imported.cast()?;
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let prefix = format!("Local\\Kielbox-Rune-{}-{unique}", std::process::id());
            let release_name = format!("{prefix}-release");
            let mut release = None;
            device.cast::<ID3D11Device5>()?.CreateFence::<ID3D11Fence>(
                0,
                D3D11_FENCE_FLAG_SHARED,
                &mut release,
            )?;
            let release = release.context("Create shared preview release fence")?;
            let fence_handle = Handle(release.CreateSharedHandle(
                None,
                GENERIC_ALL.0,
                &HSTRING::from(&release_name),
            )?);
            desc.MiscFlags =
                (D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE).0 as u32;
            desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
            desc.CPUAccessFlags = 0;
            desc.Usage = D3D11_USAGE_DEFAULT;
            let mut slots = Vec::new();
            for index in 0..3 {
                let mut texture = None;
                device.CreateTexture2D(&desc, None, Some(&mut texture))?;
                let texture = texture.context("Create named preview image")?;
                let name = format!("{prefix}-{index}");
                let handle = Handle(texture.cast::<IDXGIResource1>()?.CreateSharedHandle(
                    None,
                    (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0,
                    &HSTRING::from(&name),
                )?);
                let mut query = None;
                device.CreateQuery(
                    &D3D11_QUERY_DESC {
                        Query: D3D11_QUERY_EVENT,
                        MiscFlags: 0,
                    },
                    Some(&mut query),
                )?;
                slots.push(Slot {
                    texture,
                    query: query.context("Create preview completion query")?,
                    name,
                    _handle: handle,
                    stage: Stage::Free,
                });
            }
            Ok(Self {
                _producer: producer,
                imported,
                mutex,
                context,
                release,
                release_name,
                _fence_handle: fence_handle,
                slots,
                surface,
                width: desc.Width,
                height: desc.Height,
                next: 1,
                published: 0,
            })
        }
    }
    pub fn refresh(&mut self) -> Result<bool> {
        unsafe {
            if let Some(error) = self.surface.last_error() {
                anyhow::bail!(error);
            }
            let completed = self.release.GetCompletedValue();
            ensure!(
                completed != u64::MAX,
                "The preview graphics device was removed."
            );
            let mut changed = false;
            for slot in &mut self.slots {
                match slot.stage {
                    Stage::Published(serial) if serial <= completed => slot.stage = Stage::Free,
                    Stage::Copying(serial) => {
                        let mut ready = 0i32;
                        let status = (Interface::vtable(&self.context).GetData)(
                            self.context.as_raw(),
                            slot.query.as_raw(),
                            (&mut ready as *mut i32).cast(),
                            4,
                            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                        );
                        status.ok()?;
                        if status.0 == 0 && ready != 0 {
                            if serial > self.published {
                                self.surface
                                    .push_shared(
                                        SharedTexture::named(&slot.name, self.width, self.height)
                                            .release_fence(&self.release_name, serial),
                                        serial,
                                    )
                                    .map_err(anyhow::Error::msg)?;
                                self.published = serial;
                                slot.stage = Stage::Published(serial);
                                changed = true;
                            } else {
                                slot.stage = Stage::Free;
                            }
                        }
                    }
                    _ => {}
                }
            }
            let Some(slot) = self
                .slots
                .iter_mut()
                .find(|s| matches!(s.stage, Stage::Free))
            else {
                return Ok(changed);
            };
            // Positive WAIT_TIMEOUT is not successful mutex ownership.
            let status = (Interface::vtable(&self.mutex).AcquireSync)(self.mutex.as_raw(), 1, 0);
            if status.0 == 0x102 {
                return Ok(changed);
            }
            ensure!(status.0 == 0, "Capture preview mutex failed: {status:?}");
            self.context.CopyResource(&slot.texture, &self.imported);
            self.context.End(&slot.query);
            self.context.Flush();
            self.mutex.ReleaseSync(0)?;
            slot.stage = Stage::Copying(self.next);
            self.next = self
                .next
                .checked_add(1)
                .context("Preview frame counter exhausted")?;
            Ok(changed)
        }
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.surface.clear();
    }
}

/// Generated checkerboard for the opt-in headless GPU interoperability proof.
/// This creates no capture session and reads no screen pixels.
pub fn fixture(surface: NativeSurface) -> Result<Bridge> {
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    unsafe {
        let mut device = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let device = device.context("Create generated preview device")?;
        let size = 64u32;
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        for y in 0..size {
            for x in 0..size {
                let i = ((y * size + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(if (x / 8 + y / 8) % 2 == 0 {
                    &[48, 210, 62, 255]
                } else {
                    &[210, 48, 180, 255]
                });
            }
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size,
            Height: size,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32,
        };
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: size * 4,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        device.CreateTexture2D(&desc, Some(&data), Some(&mut texture))?;
        let texture = texture.context("Create generated preview texture")?;
        let mutex: IDXGIKeyedMutex = texture.cast()?;
        let status = (Interface::vtable(&mutex).AcquireSync)(mutex.as_raw(), 0, 0);
        ensure!(status.0 == 0, "Own generated preview texture");
        mutex.ReleaseSync(1)?;
        Bridge::new(texture, surface)
    }
}
