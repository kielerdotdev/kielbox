//! Production header eligibility with a counted sentinel device factory.
//! Opens one local source; never creates a D3D/audio device or starts playback.
#[cfg(windows)]
#[path = "../src/playback_gpu/decode.rs"]
#[allow(dead_code)]
mod decode;
#[cfg(windows)]
#[path = "../src/ffmpeg_runtime.rs"]
#[allow(dead_code)]
mod ffmpeg_runtime;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{bail, ensure, Context, Result};
    use std::{
        cell::Cell,
        path::PathBuf,
        sync::{atomic::AtomicBool, Arc},
        time::{Duration, Instant},
    };
    #[derive(Debug)]
    struct DeviceFactoryReached;
    impl std::fmt::Display for DeviceFactoryReached {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("counted sentinel factory reached; no D3D device created")
        }
    }
    impl std::error::Error for DeviceFactoryReached {}
    let mut input = None;
    let mut directory = PathBuf::new();
    let mut eligible = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--input") => input = Some(PathBuf::from(args.next().context("--input requires a local path")?)),
            Some("--dll-dir") => directory = PathBuf::from(args.next().context("--dll-dir requires an absolute directory")?),
            Some("--expect") => eligible = Some(match args.next().and_then(|v| v.into_string().ok()).as_deref() {
                Some("eligible") => true, Some("ineligible") => false,
                _ => bail!("--expect must be eligible or ineligible"),
            }),
            _ => bail!("usage: gpu_startup_probe --input FILE --expect eligible|ineligible [--dll-dir ABSOLUTE_DIR]"),
        }
    }
    let input = input.context("--input is required")?;
    let eligible = eligible.context("--expect is required")?;
    let worker = std::thread::Builder::new()
        .name("rcr-startup-probe".into())
        .spawn(move || -> Result<_> {
            let calls = Cell::new(0);
            let started = Instant::now();
            let error = decode::Decoder::open_transport_lazy(
                &input,
                &directory,
                Arc::new(AtomicBool::new(false)),
                decode::TransportOptions {
                    timeout: Duration::from_secs(5),
                    source_in_ns: 0,
                    stream: 0,
                },
                ((2560, 1440), 60),
                || {
                    calls.set(calls.get() + 1);
                    Err(DeviceFactoryReached.into())
                },
            )
            .err()
            .context("Unexpected decoder success without a real device")?;
            if eligible {
                ensure!(
                    calls.get() == 1 && error.downcast_ref::<DeviceFactoryReached>().is_some(),
                    "Eligible source failed before the counted device factory: {error:#}"
                );
            } else {
                ensure!(
                    calls.get() == 0
                        && error.downcast_ref::<decode::GpuInputIneligible>().is_some(),
                    "Expected source eligibility rejection before D3D, received: {error:#}"
                );
            }
            Ok(
                serde_json::json!({"header_eligible": eligible, "device_factory_calls": calls.get(),
            "elapsed_ms": started.elapsed().as_secs_f64() * 1000., "gpu_device_created": false,
            "audio_device_created": false, "reason": error.to_string()}),
            )
        })?;
    let report = worker
        .join()
        .map_err(|_| anyhow::anyhow!("startup probe worker panicked"))??;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    eprintln!("GPU startup eligibility probe is Windows-only");
}
