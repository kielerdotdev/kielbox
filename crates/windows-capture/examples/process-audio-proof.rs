//! Audio-only isolation proof; starts no desktop capture, microphone, or output file.
//! Run: process-audio-proof PID [seconds=3] [includedHz=440] [excludedHz=880] [minRatio=10]
//! Append --expect-exit or --expect-fingerprint-mismatch for lifecycle proofs.
//! A parent PID tests child-tree capture with the ordinary isolation mode.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProofMode { Isolation, Exit, FingerprintMismatch }
#[cfg(windows)]
fn proof_args(args: impl IntoIterator<Item = String>) -> anyhow::Result<(ProofMode, Vec<String>)> {
    let mut mode = ProofMode::Isolation;
    let mut positional = Vec::new();
    for arg in args {
        let requested = match arg.as_str() {
            "--expect-exit" => Some(ProofMode::Exit),
            "--expect-fingerprint-mismatch" => Some(ProofMode::FingerprintMismatch),
            _ => None,
        };
        if let Some(requested) = requested {
            anyhow::ensure!(mode == ProofMode::Isolation, "choose exactly one lifecycle proof mode");
            mode = requested;
        } else {
            anyhow::ensure!(!arg.starts_with("--"), "unknown proof option: {arg}");
            positional.push(arg);
        }
    }
    Ok((mode, positional))
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use rcr_media_core::{AudioTrack, AUDIO_RATE};
    use rcr_windows::audio::{AudioEvent, AudioWorkers, process_creation_time, qpc_hns};
    use std::time::{Duration, Instant};
    let (mode, args) = proof_args(std::env::args().skip(1))?;
    let mut args = args.into_iter();
    let pid: u32 = args.next().context("usage: process-audio-proof PID [seconds] [includedHz] [excludedHz] [minRatio]")?.parse()?;
    let seconds: f64 = args.next().unwrap_or_else(|| "3".into()).parse()?;
    let included: f64 = args.next().unwrap_or_else(|| "440".into()).parse()?;
    let excluded: f64 = args.next().unwrap_or_else(|| "880".into()).parse()?;
    let minimum: f64 = args.next().unwrap_or_else(|| "10".into()).parse()?;
    ensure!(args.next().is_none(), "too many arguments");
    ensure!(seconds.is_finite() && (0.1..=30.).contains(&seconds), "proof duration must be 0.1–30 seconds");
    ensure!([included, excluded].iter().all(|f| f.is_finite() && *f > 0. && *f < f64::from(AUDIO_RATE) / 2.)
        && (included - excluded).abs() >= 1., "tone frequencies must differ and lie below Nyquist");
    ensure!(minimum.is_finite() && minimum >= 1., "minimum isolation ratio must be at least 1");
    if mode == ProofMode::FingerprintMismatch {
        let actual = process_creation_time(pid)?;
        let wrong = actual.checked_add(1).context("creation time overflow")?;
        let started = Instant::now();
        let result = AudioWorkers::start(&[AudioTrack::process_instance(pid, wrong)?], 0);
        let error = match result {
            Ok(mut workers) => { workers.stop(); anyhow::bail!("mismatched process fingerprint was accepted"); }
            Err(error) => format!("{error:#}"),
        };
        ensure!(error.contains("Selected application process changed"), "fingerprint rejected for unexpected reason: {error}");
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "mode": "fingerprint-mismatch", "pid": pid, "actual_creation_hns": actual,
            "requested_creation_hns": wrong, "rejected": true, "error": error,
            "elapsed_ms": started.elapsed().as_secs_f64() * 1000.,
        }))?);
        return Ok(());
    }
    let mut audio = AudioWorkers::start(&[AudioTrack::process(pid)?], 0)?;
    audio.set_epoch(qpc_hns()?);
    let start = Instant::now();
    let mut frames = 0u64;
    let mut packets = 0u64;
    let mut discontinuities = 0;
    let mut sum_square = 0.;
    let mut projection = [(0f64, 0f64), (0f64, 0f64)];
    let mut first_pts = None;
    let mut last_pts = 0;
    let mut exit_error = None;
    'capture: while start.elapsed() < Duration::from_secs_f64(seconds) {
        for event in audio.receiver.try_iter() {
            match event {
                AudioEvent::Packet(packet) => {
                    ensure!(packet.track == 2 && packet.data.len() % 8 == 0, "invalid application PCM packet");
                    ensure!(first_pts.is_none() || packet.pts_ns >= last_pts, "application timestamps moved backward");
                    first_pts.get_or_insert(packet.pts_ns);
                    last_pts = packet.pts_ns;
                    packets += 1;
                    for bytes in packet.data.as_chunks::<8>().0 {
                        let left = f32::from_le_bytes(bytes[..4].try_into()?);
                        let right = f32::from_le_bytes(bytes[4..].try_into()?);
                        ensure!(left.is_finite() && right.is_finite(), "nonfinite application PCM");
                        let sample = (f64::from(left) + f64::from(right)) * 0.5;
                        sum_square += sample * sample;
                        for (frequency, (real, imaginary)) in [included, excluded].into_iter().zip(projection.iter_mut()) {
                            let phase = std::f64::consts::TAU * frequency * frames as f64 / f64::from(AUDIO_RATE);
                            *real += sample * phase.cos();
                            *imaginary += sample * phase.sin();
                        }
                        frames += 1;
                    }
                }
                AudioEvent::Discontinuity => discontinuities += 1,
                AudioEvent::Error(detail) => {
                    if mode == ProofMode::Exit && detail.contains("Application process") && detail.contains("exited") {
                        exit_error = Some(detail);
                        break 'capture;
                    }
                    anyhow::bail!("application audio failed: {detail}");
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let stop_started = Instant::now();
    audio.stop();
    let stop_ms = stop_started.elapsed().as_secs_f64() * 1000.;
    for event in audio.receiver.try_iter() {
        if let AudioEvent::Error(detail) = event {
            if mode == ProofMode::Exit && detail.contains("Application process") && detail.contains("exited") {
                exit_error = Some(detail);
            } else { anyhow::bail!("application audio failed: {detail}"); }
        }
    }
    if mode == ProofMode::Exit {
        let error = exit_error.context("target did not report process exit before proof deadline")?;
        ensure!(frames > 0, "target exited before producing any PCM; capture-before-exit was not proved");
        ensure!(stop_ms < 1000., "audio worker did not stop promptly after target exit");
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "mode": "target-exit", "pid": pid, "frames_before_exit": frames,
            "packets_before_exit": packets, "discontinuities": discontinuities,
            "error": error, "stop_ms": stop_ms, "elapsed_ms": start.elapsed().as_secs_f64() * 1000.,
        }))?);
        return Ok(());
    }
    ensure!(frames > 0, "application audio produced no PCM frames");
    let amplitude = projection.map(|(real, imaginary)| 2. * real.hypot(imaginary) / frames as f64);
    let rms = (sum_square / frames as f64).sqrt();
    let ratio = amplitude[0] / amplitude[1].max(1e-12);
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "pid": pid, "sample_rate": AUDIO_RATE, "frames": frames, "packets": packets,
        "first_pts_ns": first_pts, "last_pts_ns": last_pts, "discontinuities": discontinuities,
        "rms": rms, "included_hz": included, "excluded_hz": excluded,
        "included_amplitude": amplitude[0], "excluded_amplitude": amplitude[1],
        "isolation_ratio": ratio, "required_ratio": minimum,
    }))?);
    ensure!(rms > 0.001 && amplitude[0] > 0.001, "selected application tone is absent or too quiet");
    ensure!(ratio >= minimum, "unselected tone leaked into application capture: ratio {ratio:.2} < {minimum:.2}");
    Ok(())
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_flags_preserve_positional_parent_pid_and_reject_ambiguity() {
        let args = |values: &[&str]| values.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let (mode, positional) = proof_args(args(&["1234", "3", "--expect-exit"])).unwrap();
        assert_eq!(mode, ProofMode::Exit);
        assert_eq!(positional, ["1234", "3"]);
        assert_eq!(proof_args(args(&["--expect-fingerprint-mismatch", "1234"])).unwrap().0, ProofMode::FingerprintMismatch);
        assert_eq!(proof_args(args(&["1234"])).unwrap().0, ProofMode::Isolation);
        assert!(proof_args(args(&["1234", "--expect-exit", "--expect-fingerprint-mismatch"])).is_err());
        assert!(proof_args(args(&["1234", "--unknown"])).is_err());
    }
}
#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("Windows process audio backend required")
}
