//! Serial native-tool cancellation and ordinary audio-metadata regression.
use anyhow::{Context, Result, ensure};
use rcr_editor_core::*;
use std::{path::PathBuf, time::{Duration, Instant}};

fn cancelled<T>(name: &str, run: impl FnOnce(&dyn Fn() -> bool) -> Result<T>) -> Result<()> {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(25);
    let result = run(&|| Instant::now() >= deadline);
    let elapsed = started.elapsed();
    match result {
        Err(error) => ensure!(format!("{error:#}").to_lowercase().contains("cancelled"),
            "{name} failed for another reason: {error:#}"),
        Ok(_) => ensure!(elapsed < Duration::from_millis(100),
            "{name} completed without observing cancellation after {elapsed:?}"),
    }
    ensure!(elapsed < Duration::from_millis(750), "{name} cancellation stalled: {elapsed:?}");
    println!("{name}: returned in {:.1} ms", elapsed.as_secs_f64() * 1000.);
    Ok(())
}

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).context("A/V recording required")?);
    let asset = probe(&path)?;
    let stream = asset.streams.iter().find(|stream| stream.kind == TrackKind::Audio)
        .context("Fixture requires a real audio track")?.index;
    let bins = waveform(&path, stream)?;
    ensure!(!bins.is_empty() && bins.iter().all(|bin| bin.rms.is_finite()),
        "Ordinary waveform decoding is incomplete or nonfinite");
    println!("Normal waveform: {} ten-millisecond bins", bins.len());
    for _ in 0..3 {
        cancelled("probe", |cancel| probe_cancellable(&path, cancel))?;
        cancelled("thumbnail", |cancel| thumbnail_cancellable(&asset, 0.5, (320, 180), cancel))?;
        cancelled("waveform", |cancel| waveform_cancellable(&path, stream, cancel))?;
    }
    println!("Media cancellation proof passed: ordinary waveform and nine serial cancellable jobs");
    Ok(())
}
