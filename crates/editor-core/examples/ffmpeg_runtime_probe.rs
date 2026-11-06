//! Opt-in read-only DLL/version discovery; opens no media or audio/GPU devices.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{bail, ensure, Context};
    use editor_core::ffmpeg_runtime::{
        probe_ffmpeg_runtime, FfmpegRuntimeConfig, FfmpegRuntimeRequirement,
    };
    let mut config = FfmpegRuntimeConfig::Automatic;
    let mut requirement = FfmpegRuntimeRequirement::Gpu702;
    let mut refresh = false;
    let mut verify_pin_reuse = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--directory") => {
                config = FfmpegRuntimeConfig::Directory(args.next().context("--directory requires an absolute local path")?.into());
            }
            Some("--scrub-major-abi") => requirement = FfmpegRuntimeRequirement::ScrubMajorAbi,
            Some("--refresh") => refresh = true,
            Some("--verify-pin-reuse") => verify_pin_reuse = true,
            _ => bail!("usage: ffmpeg_runtime_probe [--directory ABSOLUTE_DIR] [--scrub-major-abi] [--refresh] [--verify-pin-reuse]"),
        }
    }
    let worker = std::thread::Builder::new()
        .name("rcr-runtime-probe".into())
        .spawn(move || -> anyhow::Result<_> {
            let info = probe_ffmpeg_runtime(&config, requirement, refresh, || false)?;
            if verify_pin_reuse {
                for config in [
                    FfmpegRuntimeConfig::Automatic,
                    FfmpegRuntimeConfig::Directory(std::path::PathBuf::from(
                        r"C:\rcr-runtime-probe-missing-directory",
                    )),
                ] {
                    let reused = probe_ffmpeg_runtime(&config, requirement, false, || false)?;
                    ensure!(
                        reused.directory == info.directory && reused.versions == info.versions,
                        "Runtime pin changed within the process"
                    );
                }
                let scrub = probe_ffmpeg_runtime(
                    &FfmpegRuntimeConfig::Automatic,
                    FfmpegRuntimeRequirement::ScrubMajorAbi,
                    false,
                    || false,
                )?;
                ensure!(
                    scrub.directory == info.directory && scrub.versions == info.versions,
                    "Scrub did not reuse the same runtime family"
                );
            }
            Ok(
                serde_json::json!({"runtime": info, "pin_reuse_verified": verify_pin_reuse,
                "media_opened": false, "gpu_device_created": false}),
            )
        })?;
    let info = worker
        .join()
        .map_err(|_| anyhow::anyhow!("runtime probe worker panicked"))??;
    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    eprintln!("FFmpeg DLL discovery is Windows-only");
}
