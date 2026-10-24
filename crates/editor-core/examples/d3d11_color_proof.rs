//! Explicit owned-media color proof. Staging readback is confined to this example.
#[cfg(windows)]
#[path = "../src/playback_gpu/convert.rs"]
#[allow(dead_code)]
mod convert;
#[cfg(windows)]
#[path = "../src/playback_gpu/decode.rs"]
#[allow(dead_code)]
mod decode;
#[cfg(windows)]
#[path = "../src/ffmpeg_runtime.rs"]
#[allow(dead_code)]
mod ffmpeg_runtime;

#[cfg(windows)]
mod proof {
    use super::{
        convert::{ConvertConfig, Nv12Converter, QueryDiagnostics, ReadTicket, TrackedPublish},
        decode::{DecodedFrame, Decoder},
    };
    use crate::decode;
    use anyhow::{ensure, Context, Result};
    use serde_json::{json, Value};
    use std::{
        fs::File,
        io::Read,
        path::{Component, Path, PathBuf, Prefix},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };
    use windows::{
        core::Interface,
        Win32::Graphics::{
            Direct3D11::*,
            Dxgi::{Common::*, IDXGIKeyedMutex, DXGI_ERROR_WAS_STILL_DRAWING},
        },
    };

    const PIXEL_LIMIT: usize = 64 * 1024 * 1024;
    const QUERY: QueryDiagnostics = QueryDiagnostics {
        kick_after_polls: 1,
        read_bool: true,
    };

    struct Options {
        input: PathBuf,
        reference: PathBuf,
        dlls: PathBuf,
        width: u32,
        height: u32,
        luid: Option<(u32, i32)>,
    }

    fn local(path: &Path) -> Result<PathBuf> {
        let path = path
            .canonicalize()
            .context("Owned local proof file is unavailable")?;
        ensure!(
            matches!(path.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))),
            "Color proof accepts local drive files only"
        );
        ensure!(path.is_file(), "Proof path is not a regular file");
        Ok(path)
    }

    fn parse() -> Result<Options> {
        let mut args = std::env::args().skip(1);
        let (mut input, mut reference, mut width, mut height, mut low, mut high) =
            (None, None, None, None, None, None);
        let mut dlls = PathBuf::from("C:/bin");
        while let Some(arg) = args.next() {
            let value = args
                .next()
                .with_context(|| format!("Missing value for {arg}"))?;
            match arg.as_str() {
                "--input" => { ensure!(input.is_none(), "Repeated --input"); input = Some(PathBuf::from(value)); },
                "--reference" => { ensure!(reference.is_none(), "Repeated --reference"); reference = Some(PathBuf::from(value)); },
                "--width" => width = Some(value.parse()?),
                "--height" => height = Some(value.parse()?),
                "--dll-dir" => dlls = PathBuf::from(value),
                "--adapter-low" => low = Some(value.parse()?),
                "--adapter-high" => high = Some(value.parse()?),
                _ => anyhow::bail!("Unknown argument {arg}; required: --input AV1 --reference BGRA --width N --height N"),
            }
        }
        ensure!(
            low.is_some() == high.is_some(),
            "Provide both adapter LUID halves"
        );
        ensure!(dlls.is_absolute(), "DLL directory must be absolute");
        let options = Options {
            input: local(&input.context("--input is required")?)?,
            reference: local(&reference.context("--reference is required")?)?,
            dlls,
            width: width.context("--width is required")?,
            height: height.context("--height is required")?,
            luid: low.zip(high),
        };
        pixel_bytes(options.width, options.height)?;
        Ok(options)
    }

    fn pixel_bytes(width: u32, height: u32) -> Result<usize> {
        ensure!(
            (108..=16384).contains(&width) && (27..=16384).contains(&height),
            "Dimensions cannot support the 3x12 nine-pixel patch grid"
        );
        let bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .context("Pixel extent overflow")?;
        ensure!(
            bytes <= PIXEL_LIMIT,
            "Color proof image exceeds 64MiB readback budget"
        );
        Ok(bytes)
    }

    fn reference(options: &Options) -> Result<Vec<u8>> {
        let expected = pixel_bytes(options.width, options.height)?;
        let mut file = File::open(&options.reference)?;
        ensure!(
            file.metadata()?.len() == expected as u64,
            "Raw BGRA reference length does not match dimensions"
        );
        let mut bytes = Vec::with_capacity(expected);
        Read::by_ref(&mut file)
            .take(expected as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() == expected,
            "Raw reference changed size while reading"
        );
        Ok(bytes)
    }

    fn retire(
        decoder: &Decoder,
        converter: &mut Nv12Converter,
        ticket: &mut Option<ReadTicket>,
        retained: &mut Option<DecodedFrame>,
        deadline: Instant,
    ) -> Result<u64> {
        let mut pending_polls = 0;
        while let Some(expected) = *ticket {
            ensure!(
                Instant::now() < deadline,
                "GPU input completion exceeded proof deadline"
            );
            if let Some(completed) =
                decoder.with_context_lock(|| converter.poll_completed_diagnostic(QUERY))?
            {
                ensure!(
                    completed == expected,
                    "GPU completion returned a different input ticket"
                );
                *ticket = None;
                drop(retained.take()); // release the AVFrame only after its exact EVENT completes.
            } else {
                pending_polls += 1;
                ensure!(
                    Instant::now() < deadline,
                    "GPU input completion exceeded proof deadline"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(pending_polls)
    }

    fn readback(
        decoder: &Decoder,
        converter: &Nv12Converter,
        width: u32,
        height: u32,
        deadline: Instant,
    ) -> Result<Vec<u8>> {
        let output = converter.output();
        ensure!(
            (output.width, output.height) == (width, height),
            "Converted output dimensions differ from reference"
        );
        let bytes = pixel_bytes(width, height)?;
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
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let mut staging = None;
        unsafe {
            decoder
                .device
                .CreateTexture2D(&desc, None, Some(&mut staging))?;
        }
        let staging = staging.context("Missing private proof staging texture")?;
        let mutex: IDXGIKeyedMutex = output.texture.cast()?;
        loop {
            ensure!(
                Instant::now() < deadline,
                "Readback output acquire exceeded deadline"
            );
            let status = decoder.with_context_lock(|| unsafe {
                (Interface::vtable(&mutex).AcquireSync)(mutex.as_raw(), 1, 0)
            });
            if status.0 == 0 {
                break;
            }
            ensure!(
                status.0 == 0x102,
                "Readback output acquire failed: {status:?}"
            );
            ensure!(
                Instant::now() < deadline,
                "Readback output acquire exceeded deadline"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        decoder.with_context_lock(|| unsafe {
            decoder.context.CopyResource(&staging, &output.texture);
            decoder.context.Flush();
            mutex.ReleaseSync(0)
        })?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        loop {
            ensure!(
                Instant::now() < deadline,
                "Private staging copy exceeded proof deadline"
            );
            let result = decoder.with_context_lock(|| unsafe {
                decoder.context.Map(
                    &staging,
                    0,
                    D3D11_MAP_READ,
                    D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                    Some(&mut mapped),
                )
            });
            match result {
                Ok(()) => break,
                Err(error) if error.code() == DXGI_ERROR_WAS_STILL_DRAWING => {
                    ensure!(
                        Instant::now() < deadline,
                        "Private staging copy did not complete within deadline"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error.into()),
            }
        }
        let row_bytes = width as usize * 4;
        let layout_ok = !mapped.pData.is_null()
            && mapped.RowPitch as usize >= row_bytes
            && (mapped.RowPitch as usize)
                .checked_mul(height as usize)
                .is_some_and(|n| n <= 128 * 1024 * 1024);
        if !layout_ok {
            decoder.with_context_lock(|| unsafe { decoder.context.Unmap(&staging, 0) });
            anyhow::bail!(
                "Mapped proof staging RowPitch is invalid or exceeds readback layout bound"
            );
        }
        let mut pixels = vec![0u8; bytes];
        for row in 0..height as usize {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    mapped
                        .pData
                        .cast::<u8>()
                        .add(row * mapped.RowPitch as usize),
                    pixels.as_mut_ptr().add(row * row_bytes),
                    row_bytes,
                );
            }
        }
        decoder.with_context_lock(|| unsafe { decoder.context.Unmap(&staging, 0) });
        Ok(pixels)
    }

    fn patch(pixels: &[u8], width: u32, cx: u32, cy: u32) -> ([u8; 4], [u8; 4]) {
        let mut channels = [[0u8; 81]; 4];
        for (index, (y, x)) in ((cy - 4)..=(cy + 4))
            .flat_map(|y| ((cx - 4)..=(cx + 4)).map(move |x| (y, x)))
            .enumerate()
        {
            let offset = (y as usize * width as usize + x as usize) * 4;
            for channel in 0..4 {
                channels[channel][index] = pixels[offset + channel];
            }
        }
        let mut median = [0; 4];
        let mut range = [0; 4];
        for channel in 0..4 {
            channels[channel].sort_unstable();
            median[channel] = channels[channel][40];
            range[channel] = channels[channel][80] - channels[channel][0];
        }
        (median, range)
    }

    fn compare(reference: &[u8], gpu: &[u8], width: u32, height: u32) -> Result<Value> {
        let expected = pixel_bytes(width, height)?;
        ensure!(
            reference.len() == expected && gpu.len() == expected,
            "Comparison pixel lengths do not match dimensions"
        );
        ensure!(
            gpu.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255),
            "GPU output contains non-opaque alpha"
        );
        let mut accepted = 0;
        let mut skipped = 0;
        let mut failures = 0;
        let mut maximum_error = 0;
        let mut colors: Vec<[u8; 3]> = Vec::new();
        let (mut minimum, mut maximum) = (255u8, 0u8);
        let mut worst = Value::Null;
        for row in 0..3 {
            for column in 0..12 {
                let x = (2 * column + 1) * width / 24;
                let y = (2 * row + 1) * height / 6;
                let (expected, range) = patch(reference, width, x, y);
                if range[..3].iter().any(|&range| range > 2) || expected[3] != 255 {
                    skipped += 1;
                    continue;
                }
                accepted += 1;
                let (actual, _) = patch(gpu, width, x, y);
                let error = (0..3)
                    .map(|channel| actual[channel].abs_diff(expected[channel]))
                    .max()
                    .unwrap();
                if error > maximum_error {
                    maximum_error = error;
                    worst =
                        json!({"grid":[row,column],"reference_bgra":expected,"gpu_bgra":actual});
                }
                if error > 4 {
                    failures += 1;
                }
                for &channel in &expected[..3] {
                    minimum = minimum.min(channel);
                    maximum = maximum.max(channel);
                }
                let color = [expected[0], expected[1], expected[2]];
                if colors
                    .iter()
                    .all(|old| (0..3).any(|i| old[i].abs_diff(color[i]) >= 16))
                {
                    colors.push(color);
                }
            }
        }
        ensure!(
            accepted >= 16,
            "Only {accepted} flat reference patches; require at least16 of36"
        );
        ensure!(
            colors.len() >= 6,
            "Only {} distinct reference colors; require at least6",
            colors.len()
        );
        ensure!(
            maximum - minimum >= 200,
            "Reference channel span {} is below200",
            maximum - minimum
        );
        let result = json!({"accepted_patches":accepted,"skipped_nonflat_patches":skipped,"distinct_reference_colors":colors.len(),
            "reference_channel_span":maximum-minimum,"failed_patches":failures,"maximum_channel_error":maximum_error,
            "tolerance":4,"patch_size":[9,9],"grid":[3,12],"worst_patch":worst,"all_gpu_alpha":255});
        ensure!(
            failures == 0,
            "Decoded GPU color comparison failed: {result}"
        );
        Ok(result)
    }

    pub fn run() -> Result<Value> {
        let options = parse()?;
        let reference = reference(&options)?;
        let started = Instant::now();
        let (device, context, adapter) = decode::create_device(options.luid)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let mut decoder = Some(Decoder::open(
            &options.input,
            &options.dlls,
            device,
            context,
            cancel.clone(),
            Duration::from_secs(15),
        )?);
        let mut retained = None;
        let mut converter = None;
        let mut ticket = None;
        let result = (|| -> Result<Value> {
            let decoder = decoder.as_mut().unwrap();
            retained = Some(
                decoder
                    .next()?
                    .context("Owned AV1 source has no decoded frame")?,
            );
            let frame = retained.as_ref().unwrap();
            frame.info.require_bt709()?;
            ensure!(
                frame.info.visible_size == (options.width, options.height),
                "Decoded visible dimensions differ from raw reference"
            );
            let info = frame.info.clone(); // metadata only; the AVFrame is never cloned.
            converter = Some(decoder.with_context_lock(|| {
                Nv12Converter::new(
                    &decoder.device,
                    &decoder.context,
                    ConvertConfig {
                        coded_size: info.coded_size,
                        source_rect: Some(info.source_rect()),
                        output_size: info.visible_size,
                    },
                )
            })?);
            let converter = converter.as_mut().unwrap();
            match decoder.with_context_lock(|| {
                converter.publish_tracked(&frame.texture, info.array_slice, || {
                    cancel.load(Ordering::Acquire)
                })
            })? {
                TrackedPublish::Submitted {
                    ticket: submitted,
                    publication,
                } => {
                    ticket = Some(submitted);
                    publication?;
                }
                TrackedPublish::Skipped(reason) => anyhow::bail!(
                    "Single-frame proof unexpectedly skipped GPU conversion: {reason:?}"
                ),
            }
            let pending_polls = retire(
                decoder,
                converter,
                &mut ticket,
                &mut retained,
                Instant::now() + Duration::from_secs(3),
            )?;
            let pixels = readback(
                decoder,
                converter,
                options.width,
                options.height,
                Instant::now() + Duration::from_secs(3),
            )?;
            let comparison = compare(&reference, &pixels, options.width, options.height)?;
            Ok(
                json!({"proof":"owned_av1_d3d11va_bt709_color","status":"passed","adapter":adapter,"frame":info,
                "reference_dimensions":[options.width,options.height],"comparison":comparison,"query_pending_polls":pending_polls,
                "strict_event_bool":true,"query_kick_after_polls":1,"decoded_frames":1,"cpu_readback":"private staging; proof only",
                "production_readback":false,"source_writes":false,"elapsed_ms":started.elapsed().as_secs_f64()*1000.}),
            )
        })();
        cancel.store(true, Ordering::Release);
        let cleanup = match (decoder.as_ref(), converter.as_mut()) {
            (Some(decoder), Some(converter)) => {
                decoder.with_context_lock(|| converter.begin_shutdown());
                retire(
                    decoder,
                    converter,
                    &mut ticket,
                    &mut retained,
                    Instant::now() + Duration::from_secs(3),
                )
                .map(|_| ())
            }
            _ => Ok(()),
        };
        // On any failed/hung GPU completion, stop the old decoder before frame
        // release. There is no subsequent decoding into its pool and no reaper.
        drop(decoder.take());
        drop(retained.take());
        drop(converter.take());
        match (result, cleanup) {
            (Err(error), Err(cleanup)) => {
                Err(error.context(format!("GPU cleanup also failed: {cleanup:#}")))
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(report), Ok(())) => Ok(report),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn flat_grid() -> Vec<u8> {
            let colors = [
                [0, 0, 255, 255],
                [0, 255, 0, 255],
                [255, 0, 0, 255],
                [255, 255, 255, 255],
                [0, 0, 0, 255],
                [0, 255, 255, 255],
            ];
            let mut pixels = vec![0; 108 * 27 * 4];
            for y in 0..27 {
                for x in 0..108 {
                    let offset = (y * 108 + x) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&colors[(x / 9) % colors.len()]);
                }
            }
            pixels
        }
        #[test]
        fn patch_comparison_detects_color_error_and_requires_real_color_coverage() {
            let reference = flat_grid();
            let report = compare(&reference, &reference, 108, 27).unwrap();
            assert_eq!(report["accepted_patches"], 36);
            assert_eq!(report["distinct_reference_colors"], 6);
            let mut wrong = reference.clone();
            for pixel in wrong.as_chunks_mut::<4>().0 {
                if pixel[0] == 0 {
                    pixel[0] = 5;
                }
            }
            assert!(compare(&reference, &wrong, 108, 27).is_err());
            let mut alpha = reference.clone();
            alpha[3] = 0;
            assert!(compare(&reference, &alpha, 108, 27).is_err());
            let mut gray = vec![128; 108 * 27 * 4];
            for pixel in gray.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
            assert!(compare(&gray, &reference, 108, 27).is_err());
            assert!(pixel_bytes(16384, 16384).is_err());
        }
        #[test]
        fn nonflat_reference_patch_is_skipped_without_weakening_minimum_coverage() {
            let mut reference = flat_grid();
            let gpu = reference.clone();
            reference[..4].copy_from_slice(&[255, 255, 255, 255]);
            let report = compare(&reference, &gpu, 108, 27).unwrap();
            assert_eq!(report["accepted_patches"], 35);
            assert_eq!(report["skipped_nonflat_patches"], 1);
            assert_eq!(report["maximum_channel_error"], 0);
        }
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&proof::run()?)?);
    Ok(())
}
#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("d3d11_color_proof requires Windows")
}
