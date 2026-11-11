//! Explicitly invoked Windows GPU proof. Own synthetic NV12 data only: no capture,
//! decoder, user media, windows, audio, or output files. CPU readback exists here only.
#[cfg(windows)]
#[path = "../src/playback_gpu/convert.rs"]
mod convert;

#[cfg(windows)]
mod proof {
    use super::convert::{
        ConvertConfig, Nv12Converter, Publish, ReadTicket, SharedOutput, Skip, TrackedPublish,
    };
    use anyhow::{ensure, Context, Result};
    use std::{ffi::c_void, time::Instant};
    use windows::{
        core::Interface,
        Win32::{
            Foundation::{HMODULE, RECT},
            Graphics::{
                Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
                Direct3D11::*,
                Dxgi::{Common::*, IDXGIKeyedMutex},
            },
        },
    };

    const WIDTH: u32 = 128;
    const HEIGHT: u32 = 64;
    type Yuv = [u8; 3];
    const PATCHES: [[Yuv; 2]; 2] = [
        [[16, 128, 128], [235, 128, 128]],
        [[63, 102, 240], [173, 42, 26]],
    ];

    fn device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        Ok((
            device.context("missing D3D11 device")?,
            context.context("missing immediate context")?,
        ))
    }

    fn nv12(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
        let buffers: Vec<Vec<u8>> = PATCHES
            .iter()
            .map(|patches| {
                let mut bytes = vec![0; (width * height * 3 / 2) as usize];
                for y in 0..height {
                    for x in 0..width {
                        bytes[(y * width + x) as usize] = patches[usize::from(x >= width / 2)][0];
                    }
                }
                for y in 0..height / 2 {
                    for x in (0..width).step_by(2) {
                        let patch = patches[usize::from(x >= width / 2)];
                        let offset = (width * height + y * width + x) as usize;
                        bytes[offset] = patch[1];
                        bytes[offset + 1] = patch[2];
                    }
                }
                bytes
            })
            .collect();
        let data: Vec<_> = buffers
            .iter()
            .map(|bytes| D3D11_SUBRESOURCE_DATA {
                pSysMem: bytes.as_ptr().cast::<c_void>(),
                SysMemPitch: width,
                SysMemSlicePitch: bytes.len() as u32,
            })
            .collect();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 2,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_DECODER.0 as u32,
            ..Default::default()
        };
        let mut texture = None;
        unsafe {
            device.CreateTexture2D(&desc, Some(data.as_ptr()), Some(&mut texture))?;
        }
        texture.context("missing owned NV12 array")
    }

    fn bgra(device: &ID3D11Device) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: WIDTH,
            Height: HEIGHT,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let mut texture = None;
        unsafe {
            device.CreateTexture2D(&desc, None, Some(&mut texture))?;
        }
        texture.context("missing invalid-format fixture")
    }

    /// CPU staging is deliberately confined to this synthetic verification program.
    fn sample(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        output: &SharedOutput,
    ) -> Result<[[u8; 4]; 2]> {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            output.texture.GetDesc(&mut desc);
            ensure!(
                desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM
                    && desc.MipLevels == 1
                    && desc.ArraySize == 1
                    && desc.SampleDesc.Count == 1
                    && desc.CPUAccessFlags == 0
                    && desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0
                    && desc.MiscFlags & D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32 != 0,
                "shared output is incompatible with the UI source contract"
            );
            desc.Usage = D3D11_USAGE_STAGING;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.BindFlags = 0;
            desc.MiscFlags = 0;
            let mut staging = None;
            device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            let staging = staging.context("missing proof staging texture")?;
            let mutex: IDXGIKeyedMutex = output.texture.cast()?;
            let status = (Interface::vtable(&mutex).AcquireSync)(mutex.as_raw(), 1, 0);
            ensure!(
                status.0 == 0,
                "published output key1 unavailable: {status:?}"
            );
            context.CopyResource(&staging, &output.texture);
            context.Flush();
            mutex.ReleaseSync(0)?;
            output.consumer_release.fetch_add(1, std::sync::atomic::Ordering::Release);
            windows::Win32::System::Threading::WakeByAddressSingle(output.consumer_release.as_ptr().cast());
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let mut result = [[0u8; 4]; 2];
            for (index, x) in [output.width / 4, output.width * 3 / 4]
                .into_iter()
                .enumerate()
            {
                let offset = (output.height / 2 * mapped.RowPitch + x * 4) as usize;
                std::ptr::copy_nonoverlapping(
                    mapped.pData.cast::<u8>().add(offset),
                    result[index].as_mut_ptr(),
                    4,
                );
            }
            context.Unmap(&staging, 0);
            Ok(result)
        }
    }

    fn expected([y, u, v]: Yuv) -> [u8; 4] {
        let y = (f64::from(y) - 16.0) * 1.164383;
        let u = f64::from(u) - 128.0;
        let v = f64::from(v) - 128.0;
        let channel = |value: f64| value.round().clamp(0.0, 255.0) as u8;
        [
            channel(y + 2.112402 * u),
            channel(y - 0.213249 * u - 0.532909 * v),
            channel(y + 1.792741 * v),
            255,
        ]
    }

    fn check(samples: [[u8; 4]; 2], patches: [Yuv; 2]) -> Result<()> {
        for (actual, patch) in samples.into_iter().zip(patches) {
            let expected = expected(patch);
            ensure!(
                actual[3] == 255,
                "video processor produced non-opaque alpha: {actual:?}"
            );
            ensure!(
                (0..3).all(|i| actual[i].abs_diff(expected[i]) <= 4),
                "BT.709 conversion mismatch: BGRA {actual:?}, expected {expected:?}"
            );
        }
        Ok(())
    }

    fn submitted(result: TrackedPublish) -> Result<ReadTicket> {
        match result {
            TrackedPublish::Submitted {
                ticket,
                publication,
            } => {
                publication?;
                Ok(ticket)
            }
            TrackedPublish::Skipped(reason) => {
                anyhow::bail!("expected tracked submission, got {reason:?}")
            }
        }
    }

    fn skipped(result: TrackedPublish, expected: Skip) -> Result<()> {
        match result {
            TrackedPublish::Skipped(reason) => {
                ensure!(reason == expected, "expected {expected:?}, got {reason:?}")
            }
            TrackedPublish::Submitted { .. } => {
                anyhow::bail!("unexpected GPU read instead of {expected:?}")
            }
        }
        Ok(())
    }

    fn completion_proof(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        config: ConvertConfig,
    ) -> Result<serde_json::Value> {
        let mut converter = Nv12Converter::new(device, context, config)?;
        let output = converter.output();
        ensure!(converter.consumer_release_epoch().load(std::sync::atomic::Ordering::Acquire) == 0,
            "new output inherited a consumer release notification");
        let mut retained = Vec::new();
        // Retain each owned source keyed by its ticket, exactly as the future decoder
        // worker must retain an AVFrame (texture COM references alone are insufficient).
        for (index, slice) in [0, 1, 0].into_iter().enumerate() {
            let input = nv12(device, WIDTH, HEIGHT)?;
            let frame_id = 100 + index as u64;
            let ticket =
                submitted(converter.publish_tracked_frame(&input, slice, frame_id, || false)?)?;
            ensure!(
                output
                    .published_frame
                    .load(std::sync::atomic::Ordering::Acquire)
                    == frame_id,
                "published GPU pixels did not retain the caller's frame identity"
            );
            skipped(
                converter.publish_tracked(&input, slice, || false)?,
                if retained.len() == 2 {
                    Skip::QueueFull
                } else {
                    Skip::Busy
                },
            )?;
            check(sample(device, context, &output)?, PATCHES[slice as usize])?;
            retained.push((ticket, input));
        }
        ensure!(
            converter.pending_reads() == 3,
            "tracked queue did not retain three unretired reads"
        );
        skipped(
            converter.publish_tracked(&retained[0].1, 0, || false)?,
            Skip::QueueFull,
        )?;
        // Each poll checks only one query. No application wait is hidden in the converter.
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        let mut pending_polls = 0;
        let mut completed = 0;
        while !retained.is_empty() {
            ensure!(
                Instant::now() < deadline,
                "tracked GPU completion timed out; retained {} inputs",
                retained.len()
            );
            if let Some(ticket) = converter.poll_completed()? {
                ensure!(
                    retained[0].0 == ticket,
                    "completion changed input/ticket order"
                );
                let (_completed_ticket, completed_input) = retained.remove(0);
                drop(completed_input); // release ONLY the frame with this completed ticket.
                completed += 1;
            } else {
                pending_polls += 1;
                std::thread::yield_now();
            }
        }
        ensure!(
            converter.pending_reads() == 0,
            "retired query slot remained busy"
        );
        let input = nv12(device, WIDTH, HEIGHT)?;
        skipped(
            converter.publish_tracked(&input, 0, || true)?,
            Skip::Cancelled,
        )?;
        ensure!(
            converter.pending_reads() == 0,
            "pre-submit cancellation armed a GPU read"
        );
        // Reuse a completed query, seal with a real input still in flight, then drain.
        let ticket = submitted(converter.publish_tracked(&input, 1, || false)?)?;
        converter.begin_shutdown();
        skipped(
            converter.publish_tracked(&input, 0, || false)?,
            Skip::Closed,
        )?;
        let mut shutdown_pending_polls = 0;
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        loop {
            ensure!(
                Instant::now() < deadline,
                "sealed GPU completion did not drain; input remains owned"
            );
            if let Some(completed_ticket) = converter.poll_completed()? {
                ensure!(
                    completed_ticket == ticket,
                    "sealed drain returned the wrong input"
                );
                break;
            }
            shutdown_pending_polls += 1;
            std::thread::yield_now();
        }
        ensure!(
            converter.pending_reads() == 0,
            "sealed queue retained a completed read"
        );
        // Consumer may still show the output after input retirement; it is a separate texture.
        check(sample(device, context, &output)?, PATCHES[1])?;
        let diagnostics = converter.diagnostic_counters();
        ensure!(
            diagnostics.query_kicks == 4 && diagnostics.query_bool_reads >= 4,
            "normal completion policy did not kick exactly once per submitted query"
        );
        drop(input);
        drop(converter);
        Ok(
            serde_json::json!({"query_capacity": 3, "completed_inputs": completed + 1,
            "queue_full_skip": true, "cancel_before_submit": true, "sealed_drain": true,
            "caller_frame_ids": true, "query_kicks": diagnostics.query_kicks,
            "query_bool_reads": diagnostics.query_bool_reads,
            "pending_polls_observed": pending_polls + shutdown_pending_polls,
            "real_S_FALSE_observed": pending_polls + shutdown_pending_polls > 0,
            "note": "GPU timing may complete before first poll; pure test always checks raw S_FALSE classification"}),
        )
    }

    pub fn run() -> Result<()> {
        let started = Instant::now();
        let (device, context) = device()?;
        let input = nv12(&device, WIDTH, HEIGHT)?;
        let config = ConvertConfig {
            coded_size: (WIDTH, HEIGHT),
            source_rect: None,
            output_size: (64, 32),
        };
        let mut converter = Nv12Converter::new(&device, &context, config)?;
        let output = converter.output();
        let mut array_samples = Vec::new();
        for slice in 0..2 {
            ensure!(
                converter.publish(&input, slice, || false)? == Publish::Published,
                "initial publish was busy"
            );
            ensure!(
                converter.publish(&input, slice, || false)? == Publish::Busy,
                "unconsumed key1 must skip, without waiting"
            );
            let pixels = sample(&device, &context, &output)?;
            check(pixels, PATCHES[slice as usize])?;
            array_samples.push(pixels);
        }
        ensure!(
            converter.publish(&input, 0, || true)? == Publish::Cancelled,
            "cancelled conversion submitted work"
        );
        ensure!(
            converter.publish(&input, 2, || false).is_err(),
            "out-of-range array slice accepted"
        );
        ensure!(
            converter.publish(&bgra(&device)?, 0, || false).is_err(),
            "BGRA incorrectly accepted as NV12"
        );
        ensure!(
            converter
                .publish(&nv12(&device, WIDTH / 2, HEIGHT)?, 0, || false)
                .is_err(),
            "changed coded extent accepted"
        );
        let invalid_config = ConvertConfig {
            output_size: (WIDTH + 1, HEIGHT),
            ..config
        };
        ensure!(
            Nv12Converter::new(&device, &context, invalid_config).is_err(),
            "upscale config accepted"
        );
        ensure!(
            Nv12Converter::new(
                &device,
                &context,
                ConvertConfig {
                    coded_size: (0, HEIGHT),
                    ..config
                }
            )
            .is_err(),
            "empty coded extent accepted"
        );
        // Distinct retained identities exercise eviction and subsequent view recreation.
        let mut churn = Vec::new();
        let mut maximum_cached = converter.cached_input_views();
        for _ in 0..6 {
            let texture = nv12(&device, WIDTH, HEIGHT)?;
            ensure!(
                converter.publish(&texture, 1, || false)? == Publish::Published,
                "post-cancel publish did not recover"
            );
            check(sample(&device, &context, &output)?, PATCHES[1])?;
            maximum_cached = maximum_cached.max(converter.cached_input_views());
            ensure!(
                maximum_cached <= 4,
                "input view cache exceeded four entries"
            );
            churn.push(texture);
        }
        ensure!(
            maximum_cached == 4,
            "cache churn did not exercise its limit"
        );
        ensure!(
            converter.publish(&input, 0, || false)? == Publish::Published,
            "evicted original view could not be recreated"
        );
        check(sample(&device, &context, &output)?, PATCHES[0])?;

        let mut cropped = Nv12Converter::new(
            &device,
            &context,
            ConvertConfig {
                source_rect: Some(RECT {
                    left: 64,
                    top: 0,
                    right: 128,
                    bottom: 64,
                }),
                output_size: (32, 32),
                ..config
            },
        )?;
        let mut crop_samples = Vec::new();
        for slice in 0..2 {
            ensure!(
                cropped.publish(&input, slice, || false)? == Publish::Published,
                "crop publish was busy"
            );
            let pixels = sample(&device, &context, &cropped.output())?;
            check(pixels, [PATCHES[slice as usize][1]; 2])?;
            crop_samples.push(pixels);
        }
        let completion = completion_proof(&device, &context, config)?;
        // Map above completed all synthetic input reads before resources are released.
        drop(cropped);
        drop(converter);
        drop(output);
        drop(churn);
        drop(input);
        unsafe {
            context.Flush();
            device.GetDeviceRemovedReason()?;
        }
        println!(
            "{}",
            serde_json::json!({
                "proof": "owned_nv12_array_to_shared_bgra", "status": "passed",
                "input": [WIDTH, HEIGHT], "array_slices": 2, "output": [64, 32],
                "array_bgra_samples": array_samples, "cropped_bgra_samples": crop_samples,
                "rgb_tolerance": 4, "alpha": 255, "input_cache_max": maximum_cached,
                "busy_skip": true, "cancel_then_resume": true, "invalid_inputs_rejected": true,
                "completion": completion,
                "cpu_upload_and_readback": "synthetic proof only", "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
            })
        );
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    proof::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("gpu_convert_proof requires Windows with D3D11 video processing");
}
