//! Exercise the exact screenshot service used by GPUI without copying to the clipboard.
use rcr_toolkit::{
    core::{
        settings::{Paths, Settings},
        workflow::{CancelToken, CaptureRequest, CaptureTarget, Capturer},
    },
    services::{ProductionOptions, ProductionServices},
};
fn main() -> anyhow::Result<()> {
    let startup = std::env::args().any(|arg| arg == "--startup");
    if startup {
        use rcr_toolkit::types::{ColorSpace, Frame, PixelFormat, Size};
        let mut frame = Frame::new(Size::new(5120, 1440), PixelFormat::Bgra8, ColorSpace::Srgb);
        let started = std::time::Instant::now();
        frame.set_opaque();
        let opaque_ms = started.elapsed().as_secs_f64() * 1000.;
        let started = std::time::Instant::now();
        let frame = frame.into_rgba8()?;
        let rgba_ms = started.elapsed().as_secs_f64() * 1000.;
        anyhow::ensure!(frame.data()[3] == 255, "Alpha normalization failed");
        println!("Screenshot pixel kernels: opaque_ms={opaque_ms:.2}, rgba_ms={rgba_ms:.2}");
    }
    let temp = tempfile::tempdir()?;
    let paths = Paths::rooted_at(temp.path());
    let settings = Settings::default();
    let services = ProductionServices::new(&settings, &paths, ProductionOptions::default());
    for sample in 0..if startup { 3 } else { 1 } {
        let start = std::time::Instant::now();
        let result = services.capturer.capture(
            &CaptureRequest {
                target: CaptureTarget::Fullscreen,
                include_cursor: false,
                hdr: settings.capture.hdr,
            },
            &CancelToken::new(),
        )?;
        anyhow::ensure!(
            result.frame.width() > 0 && result.frame.height() > 0 && result.frame.is_sdr8(),
            "Invalid screenshot frame"
        );
        println!(
            "Screenshot proof: {}x{}, SDR8={}, backend={}, elapsed_ms={:.0}",
            result.frame.width(),
            result.frame.height(),
            result.frame.is_sdr8(),
            services.capturer.backend_report()?.name,
            start.elapsed().as_secs_f64() * 1000.
        );
        if startup {
            let started = std::time::Instant::now();
            let actor = rcr_toolkit::image_editor::Actor::desktop(result.frame)?;
            loop {
                if let Some(preview) = actor.take_preview() {
                    println!(
                        "Screenshot actor startup: sample={sample}, preview_ready_ms={:.2}, raster_ms={:.2}, tiles={}",
                        started.elapsed().as_secs_f64() * 1000.,
                        preview.render_us as f64 / 1000.,
                        preview.tiles.len()
                    );
                    break;
                }
                anyhow::ensure!(
                    started.elapsed() < std::time::Duration::from_secs(10),
                    "Actor preview timeout"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
    Ok(())
}
