//! CPU-only mux/finalization evaluation, using one reusable software AV1 GOP.
//! cargo run -p rcr-media-io --example eval-finalization -- report.json
use anyhow::{Context, Result, ensure};
use rcr_media_core::{AudioTrack, MediaPacket, VideoConfig};
use rcr_media_io::MatroskaWriter;
use std::{path::Path, process::{Command, Stdio}, sync::Arc, time::Instant};

fn command(program: &str) -> Command {
    let executable = std::env::var_os(if program == "ffmpeg" { "RCR_FFMPEG" } else { "RCR_FFPROBE" })
        .unwrap_or_else(|| program.into());
    let mut command = Command::new(executable);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command.stdin(Stdio::null());
    command
}

fn verify(path: &Path, seconds: u64, audio: bool) -> Result<()> {
    let output = command("ffprobe").args(["-v", "error", "-count_packets", "-show_streams", "-show_format", "-of", "json"])
        .arg(path).output()?;
    ensure!(output.status.success(), "FFprobe failed: {}", String::from_utf8_lossy(&output.stderr));
    let info: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let streams = info["streams"].as_array().context("Missing streams")?;
    ensure!(streams.iter().filter(|stream| stream["codec_name"] == "av1").count() == 1, "Missing AV1 video");
    ensure!(streams.iter().filter(|stream| stream["codec_type"] == "audio").count() == if audio { 2 } else { 0 }, "Audio track count changed");
    for stream in streams {
        let count = stream["nb_read_packets"].as_str().context("Missing packet count")?.parse::<u64>()?;
        ensure!(count == seconds * if stream["codec_type"] == "video" { 5 } else { 100 }, "Packets lost during mux");
    }
    let duration = info["format"]["duration"].as_str().context("Missing duration")?.parse::<f64>()?;
    ensure!((duration - seconds as f64).abs() < 0.01, "Duration changed: {duration}");
    let output = command("ffmpeg").args(["-v", "error", "-threads", "1", "-i"])
        .arg(path).args(["-map", "0", "-f", "null", "-"]).output()?;
    ensure!(output.status.success(), "Full decode failed: {}", String::from_utf8_lossy(&output.stderr));
    Ok(())
}

fn main() -> Result<()> {
    let report_path = std::env::args_os().nth(1).context("Usage: eval-finalization <report.json>")?;
    let durations = std::env::var("RCR_STOP_EVAL_SECONDS").unwrap_or_else(|_| "1,10,60".into())
        .split(',').map(str::parse::<u64>).collect::<std::result::Result<Vec<_>, _>>()?;
    let repeats = std::env::var("RCR_STOP_EVAL_REPEATS").unwrap_or_else(|_| "2".into()).parse::<usize>()?;
    ensure!(!durations.is_empty() && durations.iter().all(|seconds| (1..=600).contains(seconds)), "Durations must be 1..600 seconds");
    ensure!((1..=10).contains(&repeats), "Repeats must be 1..10");
    let folder = tempfile::tempdir()?;
    let ivf = folder.path().join("gop.ivf");
    let output = command("ffmpeg").args(["-hide_banner", "-v", "error", "-f", "lavfi", "-i",
        "testsrc2=size=160x90:rate=5", "-frames:v", "5", "-an", "-c:v", "libaom-av1", "-threads", "1",
        "-cpu-used", "8", "-lag-in-frames", "0", "-g", "5", "-crf", "40", "-b:v", "0", "-f", "ivf"])
        .arg(&ivf).output()?;
    ensure!(output.status.success(), "Software AV1 fixture failed: {}", String::from_utf8_lossy(&output.stderr));
    let bytes = std::fs::read(ivf)?;
    ensure!(bytes.starts_with(b"DKIF"), "Invalid IVF header");
    let mut offset = 32;
    let mut gop: Vec<Arc<[u8]>> = Vec::new();
    while offset + 12 <= bytes.len() {
        let count = u32::from_le_bytes(bytes[offset..offset + 4].try_into()?) as usize;
        offset += 12;
        gop.push(bytes.get(offset..offset + count).context("Truncated IVF packet")?.into());
        offset += count;
    }
    ensure!(gop.len() == 5 && offset == bytes.len(), "Unexpected GOP length");
    let video = VideoConfig { width: 160, height: 90, fps: 5, bitrate: 1_000_000, keyframe_interval: 5 };
    let silence: Arc<[u8]> = vec![0; 480 * 8].into();
    let mut rows = Vec::new();
    for repeat in 0..repeats {
        for &seconds in &durations {
            for audio_enabled in [false, true] {
                let path = folder.path().join(format!("mux-{repeat}-{seconds}-{audio_enabled}.mkv"));
                let audio = if audio_enabled { vec![AudioTrack::system(), AudioTrack::microphone()] } else { vec![] };
                let first = MediaPacket { track: 1, pts_ns: 0, duration_ns: 200_000_000, keyframe: true, data: gop[0].clone() };
                let mut writer = MatroskaWriter::create(&path, &video, &audio, &first)?;
                let started = Instant::now();
                for tick in 0..seconds * 100 {
                    if tick.is_multiple_of(20) {
                        let frame = tick / 20;
                        writer.packet(&MediaPacket { track: 1, pts_ns: tick * 10_000_000, duration_ns: 200_000_000,
                            keyframe: frame.is_multiple_of(5), data: gop[frame as usize % gop.len()].clone() })?;
                    }
                    for track in &audio {
                        writer.packet(&MediaPacket { track: track.id, pts_ns: tick * 10_000_000,
                            duration_ns: 10_000_000, keyframe: true, data: silence.clone() })?;
                    }
                }
                let packet_write_ms = started.elapsed().as_secs_f64() * 1000.;
                let started = Instant::now();
                writer.finish_with_metadata(&serde_json::json!({"fixture": true, "frames": seconds * 5}))?;
                let finalize_ms = started.elapsed().as_secs_f64() * 1000.;
                verify(&path, seconds, audio_enabled)?;
                let row = serde_json::json!({"repeat": repeat, "seconds": seconds, "audio_tracks": audio.len(),
                    "frames": seconds * 5, "packet_write_ms": packet_write_ms, "finalize_ms": finalize_ms,
                    "bytes": std::fs::metadata(&path)?.len(), "fully_decoded": true,
                    "shared_packet_payload_bytes": gop.iter().map(|packet| packet.len()).sum::<usize>() + silence.len()});
                println!("MUX_EVAL {row}");
                rows.push(row);
            }
        }
    }
    std::fs::write(report_path, serde_json::to_vec_pretty(&serde_json::json!({
        "scope": "Matroska packet write and mux/sync_all finalization only; excludes live capture, audio devices and hardware encoding",
        "width": 160, "height": 90, "fps": 5, "software_fixture_threads": 1, "rows": rows
    }))?)?;
    Ok(())
}
