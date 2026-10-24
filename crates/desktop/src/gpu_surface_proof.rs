//! Synthetic shared-texture proof. Readback is limited to this owned GPUI window.
use anyhow::{Context as _, Result, ensure};
use gpui::{prelude::*, *};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::*,
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
    },
    core::Interface,
};

struct Producer {
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    mutex: IDXGIKeyedMutex,
    target: ID3D11RenderTargetView,
    image: WindowsGpuSurface,
    published_frame: Arc<AtomicU64>,
    consumer_release: Arc<AtomicU64>,
    held: bool,
}

impl Producer {
    fn new(width: u32, height: u32) -> Result<Self> {
        // Match GPUI's first D3D11-capable adapter, rather than the OS default.
        let factory: IDXGIFactory6 = unsafe { CreateDXGIFactory2(Default::default())? };
        let mut index = 0;
        let (device, context) = loop {
            let adapter = unsafe { factory.EnumAdapters(index)? };
            let mut device = None;
            let mut context = None;
            if unsafe {
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&[
                        D3D_FEATURE_LEVEL_11_1,
                        D3D_FEATURE_LEVEL_11_0,
                        D3D_FEATURE_LEVEL_10_1,
                    ]),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            }
            .is_ok()
            {
                break (
                    device.context("proof device")?,
                    context.context("proof context")?,
                );
            }
            index += 1;
        };
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32,
        };
        let mut texture = None;
        unsafe {
            device.CreateTexture2D(&desc, None, Some(&mut texture))?;
        }
        let texture = texture.context("proof texture")?;
        let mutex = texture.cast()?;
        let mut target = None;
        unsafe {
            device.CreateRenderTargetView(&texture, None, Some(&mut target))?;
        }
        let published_frame = Arc::new(AtomicU64::new(0));
        let consumer_release = Arc::new(AtomicU64::new(0));
        Ok(Self {
            _device: device,
            context,
            mutex,
            target: target.context("proof target")?,
            image: WindowsGpuSurface::new(texture)?.with_frame_counter(published_frame.clone())
                .with_consumer_release(consumer_release.clone()),
            published_frame,
            consumer_release,
            held: false,
        })
    }

    fn hold(&mut self) -> Result<()> {
        ensure!(!self.held, "proof producer already holds mutex");
        let status =
            unsafe { (Interface::vtable(&self.mutex).AcquireSync)(self.mutex.as_raw(), 0, 0) };
        ensure!(status.0 == 0, "proof producer acquire: {status:?}");
        self.held = true;
        Ok(())
    }

    fn publish(&mut self, color: [f32; 4]) -> Result<()> {
        self.write_color(color, 1)
    }

    fn write_color(&mut self, color: [f32; 4], publication_key: u64) -> Result<()> {
        if !self.held {
            self.hold()?;
        }
        unsafe {
            self.context.ClearRenderTargetView(&self.target, &color);
            self.context.Flush();
            if publication_key == 1 {
                self.published_frame.fetch_add(1, Ordering::Release);
            }
            self.mutex.ReleaseSync(publication_key)?;
        }
        self.held = false;
        Ok(())
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        if self.held {
            let _ = unsafe { self.mutex.ReleaseSync(0) };
        }
    }
}

struct ProofView {
    producer: Producer,
    override_image: Option<WindowsGpuSurface>,
}
impl ProofView {
    fn image(&self) -> &WindowsGpuSurface {
        self.override_image.as_ref().unwrap_or(&self.producer.image)
    }
}
impl Render for ProofView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0x101010)).child(
            surface(self.image().clone())
                .object_fit(ObjectFit::Fill)
                .size_full(),
        )
    }
}

/// Run with an isolated hidden window. Produces nine PNGs and verifies actual
/// rendered pixels, including redraws while the producer owns the keyed mutex.
pub fn run(output: PathBuf, failed: Arc<AtomicBool>, cx: &mut App) -> Result<()> {
    use std::io::Write;
    let output = if output.is_absolute() {
        output
    } else {
        std::env::current_dir()?.join(output)
    };
    std::fs::create_dir_all(&output)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let output = output.join(format!("run-{stamp}-{}", std::process::id()));
    // A new owned directory and create_new manifest protect existing user files.
    std::fs::create_dir(&output)?;
    let mut manifest = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("owned-proof.txt"))?;
    manifest.write_all(
        b"rcr-owned-gpu-surface-proof-v1\nSynthetic GPU colors; own GPUI window only.\n",
    )?;
    println!("GPU surface proof output: {}", output.display());
    let mut producer = Producer::new(64, 48)?;
    producer.publish([0., 0.8, 0., 1.])?;
    let requested = size(px(320.), px(200.));
    let window: AnyWindowHandle = cx
        .open_window(
            WindowOptions {
                show: false,
                focus: false,
                titlebar: None,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    requested,
                ))),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| ProofView {
                    producer,
                    override_image: None,
                })
            },
        )?
        .into();
    window.update(cx, |_, window, _| window.resize(requested))?;
    window.update(cx, |root, window, cx| -> Result<()> {
        let renderer = window.windows_gpu_adapter_luid().context("renderer adapter identity")?;
        let view = root.downcast::<ProofView>().map_err(|_| anyhow::anyhow!("proof root type changed"))?;
        let device = &view.read(cx).producer._device;
        let producer = unsafe { device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()? }.AdapterLuid;
        ensure!(renderer == (producer.LowPart, producer.HighPart), "proof producer does not match the actual renderer adapter");
        Ok(())
    })??;
    cx.spawn(async move |cx| {
        let result: Result<()> = async {
            let mut initial = None;
            let mut retired_releases = Vec::new();
            for stage in 0..9 {
                // An owned native redraw, with no desktop capture or OS input.
                cx.background_executor().timer(Duration::from_millis(40)).await;
                let before = window.update(cx, |root, window, cx| -> Result<u64> {
                    let view = root.downcast::<ProofView>().map_err(|_| anyhow::anyhow!("proof root type changed"))?;
                    view.update(cx, |view, cx| -> Result<()> {
                        match stage {
                            1 => view.producer.hold()?,
                            2 => view.producer.publish([0., 0., 0.8, 1.])?,
                            3 => {
                                retired_releases.push((view.producer.consumer_release.clone(),
                                    view.producer.consumer_release.load(Ordering::Acquire)));
                                let replacement = Producer::new(96, 32)?;
                                // A new producer has no last-good image. It must
                                // never inherit pixels from the previous clip.
                                view.producer = replacement;
                            }
                            4 => view.producer.publish([0.8, 0., 0., 1.])?,
                            5 => window.resize(size(px(360.), px(240.))),
                            6 => {
                                // An owned WARP producer deliberately differs
                                // from this hardware GPUI adapter. Import must
                                // fail locally without aborting the UI frame.
                                let mut software_device = None;
                                unsafe { D3D11CreateDevice(None::<&IDXGIAdapter>, D3D_DRIVER_TYPE_WARP,
                                    HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                    Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION,
                                    Some(&mut software_device), None, None)?; }
                                let software_device = software_device.context("owned WARP device")?;
                                let desc = D3D11_TEXTURE2D_DESC {
                                    Width: 64, Height: 48, MipLevels: 1, ArraySize: 1,
                                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                                    Usage: D3D11_USAGE_DEFAULT,
                                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                                    MiscFlags: D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32,
                                    ..Default::default()
                                };
                                let mut texture = None;
                                unsafe { software_device.CreateTexture2D(&desc, None, Some(&mut texture))?; }
                                view.override_image = Some(WindowsGpuSurface::new(texture.context("invalid import fixture texture")?)?);
                            }
                            7 => {
                                retired_releases.push((view.producer.consumer_release.clone(),
                                    view.producer.consumer_release.load(Ordering::Acquire)));
                                view.override_image = None;
                                let mut replacement = Producer::new(80, 40)?;
                                replacement.publish([0.8, 0., 0., 1.])?;
                                view.producer = replacement;
                            }
                            8 => view.producer.write_color([0., 0., 0.8, 1.], 0)?,
                            _ => {}
                        }
                        cx.notify();
                        Ok(())
                    })?;
                    Ok(view.read(cx).image().draw_count())
                })??;
                let expected = if stage >= 5 { size(px(360.), px(240.)) } else { requested };
                // WindowsWindow::resize posts SetWindowPos onto its executor.
                // Await its native size callback before snapshotting: otherwise
                // request_render_snapshot correctly uses the previous client size.
                let resize_started = std::time::Instant::now();
                loop {
                    let viewport = window.update(cx, |_, window, _| window.viewport_size())?;
                    if viewport == expected { break; }
                    ensure!(resize_started.elapsed() < Duration::from_secs(5),
                        "stage {stage}: native resize did not settle: {viewport:?}, expected {expected:?}");
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                }
                let path = output.join(format!("gpu-surface-{stage}.png"));
                let completion = window.update(cx, |_, window, cx| {
                    window.draw(cx).clear();
                    window.request_render_snapshot(path.clone())
                })??;
                completion.await??;
                let pixels = image::open(&path)?.into_rgba8();
                ensure!(pixels.width() >= 320 && pixels.height() >= 200,
                    "stage {stage}: native render target was not initialized at the requested size");
                let center = pixels.get_pixel(pixels.width() / 2, pixels.height() / 2).0;
                if stage == 3 || stage == 6 {
                    ensure!(pixels.pixels().all(|pixel| pixel.0 == [16, 16, 16, 255]),
                        "unpublished producer displayed uninitialized or previous-clip pixels");
                } else {
                    let channel = if stage < 2 { 1 } else if stage == 2 { 2 } else { 0 };
                    ensure!(center[channel] > 100 && center[(channel + 1) % 3] < 20 &&
                        center[(channel + 2) % 3] < 20 && center[3] == 255,
                        "stage {stage}: expected visible synthetic color, got {center:?}");
                }
                if stage == 0 { initial = Some(pixels.clone()); }
                if stage == 1 {
                    ensure!(initial.as_ref() == Some(&pixels), "busy producer changed own-window pixels");
                }
                if stage == 5 {
                    let initial = initial.as_ref().context("initial snapshot")?;
                    ensure!(pixels.width() > initial.width() && pixels.height() > initial.height(),
                        "native resize did not change the owned render target dimensions");
                }
                let (after, viewport, error, submitted_frame, published_frame) = window.update(cx, |root, window, cx| {
                    let view = root.downcast::<ProofView>().expect("proof root");
                    let image = view.read(cx).image();
                    (image.draw_count(), window.viewport_size(), image.error(), image.submitted_frame_id(),
                        view.read(cx).producer.published_frame.load(Ordering::Acquire))
                })?;
                ensure!(viewport == expected, "stage {stage}: incorrect native viewport {viewport:?}");
                let releases = window.update(cx, |root, _, cx| {
                    root.downcast::<ProofView>().expect("proof root").read(cx)
                        .producer.consumer_release.load(Ordering::Acquire)
                })?;
                let expected_releases = match stage { 0 | 1 | 4 | 5 | 6 | 7 | 8 => 1, 2 => 2, 3 => 0, _ => unreachable!() };
                ensure!(releases == expected_releases,
                    "stage {stage}: consumer release epoch {releases}, expected {expected_releases}; cached/unpublished/error draws must never signal");
                for (retired, expected) in &retired_releases {
                    ensure!(retired.load(Ordering::Acquire) == *expected,
                        "stage {stage}: replacement draw signalled a retired output");
                }
                if stage == 6 {
                    ensure!(error.is_some(), "invalid import did not expose a surface error");
                } else {
                    ensure!(error.is_none(), "healthy surface exposed an error: {error:?}");
                }
                if stage == 3 || stage == 6 {
                    ensure!(after == before, "unpublished producer submitted uninitialized pixels");
                    ensure!(submitted_frame == 0, "unpublished image acquired a frame ID");
                } else {
                    ensure!(after > before, "stage {stage}: no real surface draw submitted");
                    ensure!(submitted_frame == published_frame && submitted_frame > 0,
                        "stage {stage}: submitted frame ID does not match copied pixels");
                }
            }
            println!("GPU surface last-good proof passed: {}", output.display());
            Ok(())
        }.await;
        if let Err(error) = result {
            failed.store(true, Ordering::Relaxed);
            eprintln!("GPU surface proof failed: {error:#}");
        }
        let _ = window.update(cx, |_, window, _| window.remove_window());
        let _ = cx.update(|cx| cx.quit());
    }).detach();
    Ok(())
}
