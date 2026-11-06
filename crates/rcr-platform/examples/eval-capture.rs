//! Opt-in actual capture: eval-capture [--output NEW-REPORT.json].
use rcr_hdr::TonemapSettings;
use rcr_image_types::{Frame, PixelFormat};
use rcr_platform::ScreenSource;
use rcr_screenshot::CaptureOptions;
use std::{
    error::Error,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

fn pixel_metadata(frame: &Frame) -> Result<String, Box<dyn Error>> {
    if frame.format() != PixelFormat::Rgba8 || frame.width() == 0 || frame.height() == 0 {
        return Err("Expected nonempty RGBA8 ScreenSource frame".into());
    }
    let started = Instant::now();
    let (mut count, mut black, mut sum, mut squares) = (0_u64, 0_u64, 0_u64, 0_u64);
    let (mut minimum, mut maximum, mut hash) = (255_u8, 0_u8, 0xcbf2_9ce4_8422_2325_u64);
    for y in 0..frame.height() {
        for rgba in frame.row(y).as_chunks::<4>().0 {
            let luma =
                ((77 * u32::from(rgba[0]) + 150 * u32::from(rgba[1]) + 29 * u32::from(rgba[2]))
                    >> 8) as u8;
            count += 1;
            black += u64::from(rgba[..3].iter().all(|channel| *channel <= 20));
            minimum = minimum.min(luma);
            maximum = maximum.max(luma);
            sum += u64::from(luma);
            squares += u64::from(luma) * u64::from(luma);
            for byte in rgba {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
            }
        }
    }
    let mean = sum as f64 / count as f64;
    let variance = (squares as f64 / count as f64 - mean * mean).max(0.);
    Ok(format!(
        concat!(
            "\"width\":{},\"height\":{},\"origin_x\":{},\"origin_y\":{},\"buffer_bytes\":{},\"stride_bytes\":{},",
            "\"pixel_count\":{},\"near_black_pixels\":{},\"near_black_fraction\":{:.8},\"luma_mean\":{:.6},\"luma_variance\":{:.6},",
            "\"luma_min\":{},\"luma_max\":{},\"pixel_hash_fnv1a64\":\"{:016x}\",\"analysis_ms\":{:.4}"
        ),
        frame.width(),
        frame.height(),
        frame.origin.x,
        frame.origin.y,
        frame.data().len(),
        frame.stride(),
        count,
        black,
        black as f64 / count as f64,
        mean,
        variance,
        minimum,
        maximum,
        hash,
        started.elapsed().as_secs_f64() * 1000.
    ))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let output: Option<PathBuf> = match args.as_slice() {
        [] => None,
        [flag] if flag == "--help" || flag == "-h" => {
            println!(
                "eval-capture [--output NEW-REPORT.json]: actual desktop capture, five runs, JSON metadata only, no saved screenshots. Requires explicit capture authorization."
            );
            return Ok(());
        }
        [flag, path] if flag == "--output" => Some(path.into()),
        _ => return Err("Usage: eval-capture [--output NEW-REPORT.json]".into()),
    };
    if let Some(path) = &output {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => return Err("Report destination exists; use a fresh --output path".into()),
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if !parent.is_dir() {
            return Err("Report parent must already exist".into());
        }
    }
    let total = Instant::now();
    let detection = Instant::now();
    let source = ScreenSource::detect(TonemapSettings::default())?;
    let detect_ms = detection.elapsed().as_secs_f64() * 1000.;
    let enumeration = Instant::now();
    let monitor_count = source.monitors()?.len();
    let enumeration_ms = enumeration.elapsed().as_secs_f64() * 1000.;
    let mut rows = Vec::with_capacity(5);
    let mut times = [0.; 5];
    for (run, milliseconds) in times.iter_mut().enumerate() {
        let started = Instant::now();
        let frame = source.capture_desktop(&CaptureOptions {
            include_cursor: false,
        })?;
        *milliseconds = started.elapsed().as_secs_f64() * 1000.;
        rows.push(format!(
            "{{\"run\":{run},\"capture_ms\":{milliseconds:.4},{}}}",
            pixel_metadata(&frame)?
        ));
    }
    times.sort_by(f64::total_cmp);
    let report = format!(
        concat!(
            "{{\"schema_version\":1,\"success\":true,\"scope\":\"Actual current ScreenSource capture, not overlay/UI latency or before-after comparison\",",
            "\"screenshots_persisted\":false,\"cursor\":false,\"window_enumeration\":false,\"run_count\":5,\"source_reused\":true,\"tonemap\":\"default\",",
            "\"content_interpretation\":\"Full-image brightness/variance only; still desktop need not change; FNV1a64 is noncryptographic\",",
            "\"backend\":\"{}\",\"detect_ms\":{:.4},\"enumeration_ms\":{:.4},\"monitor_count\":{},",
            "\"minimum_ms\":{:.4},\"median_ms\":{:.4},\"maximum_ms\":{:.4},\"runs\":[{}],\"total_ms\":{:.4}}}"
        ),
        source.backend_name(),
        detect_ms,
        enumeration_ms,
        monitor_count,
        times[0],
        times[2],
        times[4],
        rows.join(","),
        total.elapsed().as_secs_f64() * 1000.
    );
    if let Some(path) = output {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        writeln!(file, "{report}")?;
        file.sync_all()?;
    }
    println!("{report}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn brightness_uses_all_pixels_but_ignores_alpha() {
        let frame = Frame::from_rgba8(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 0]).unwrap();
        let json = pixel_metadata(&frame).unwrap();
        assert!(json.contains("\"pixel_count\":2,"));
        assert!(json.contains("\"near_black_fraction\":0.50000000,"));
        assert!(json.contains("\"luma_mean\":127.500000,"));
        assert!(json.contains("\"luma_variance\":16256.250000,"));
    }
}
