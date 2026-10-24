//! Remux one supplied AV1 keyframe plus synthetic audio; never captures devices.
use anyhow::{Context, Result, ensure};
use rcr_media_core::{AudioTrack, MediaPacket, PipelineStats, VideoConfig};
use rcr_media_io::{MatroskaWriter, save_snapshot};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    ensure!(
        args.len() == 6,
        "metadata-proof <packet.obu> <output-directory> <width> <height> <fps>"
    );
    let data = std::fs::read(&args[1])?;
    let output = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&output)?;
    let number = |index: usize| -> Result<u32> {
        args[index]
            .to_str()
            .context("Expected an integer")?
            .parse()
            .context("Invalid video configuration")
    };
    let video = VideoConfig {
        width: number(3)?,
        height: number(4)?,
        fps: number(5)?,
        ..Default::default()
    };
    video.validate()?;
    let duration_ns = 1_000_000_000 / u64::from(video.fps);
    let audio = vec![AudioTrack::system(), AudioTrack::microphone()];
    let mut packets = vec![MediaPacket {
        track: 1,
        pts_ns: 0,
        duration_ns,
        keyframe: true,
        data: data.into(),
    }];
    for track in &audio {
        packets.push(MediaPacket {
            track: track.id,
            pts_ns: 0,
            duration_ns,
            keyframe: true,
            data: vec![0; (48_000 / video.fps) as usize * 8].into(),
        });
    }
    let path = output.join("recording.mkv");
    let mut muxer = MatroskaWriter::create(&path, &video, &audio, &packets[0])?;
    for packet in &packets {
        muxer.packet(packet)?;
    }
    muxer.finish_with_metadata(&serde_json::json!({"statistics": PipelineStats {
        encoded: 1, audio_packets: 2, ..Default::default()
    }, "fixture": true}))?;
    save_snapshot(&output.join("replay.mkv"), &video, &audio, &packets)?;
    ensure!(
        !path.with_extension("stats.json").exists(),
        "Unexpected stats sidecar"
    );
    println!(
        "Native metadata proof: recording + replay with independent system/mic tracks: {}",
        output.display()
    );
    Ok(())
}
