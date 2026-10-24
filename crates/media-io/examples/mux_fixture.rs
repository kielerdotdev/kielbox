//! Integration fixture: existing AV1 proof + isolated 440/880 Hz audio tracks.
use anyhow::{Context, Result};
use rcr_media_core::*;
use rcr_media_io::*;
use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::Path,
};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let mut file = File::open(args.get(1).context("input IVF required")?)?;
    let output = Path::new(args.get(2).context("output MKV required")?);
    let mut header = [0; 32];
    file.read_exact(&mut header)?;
    anyhow::ensure!(
        &header[..4] == b"DKIF" && &header[8..12] == b"AV01",
        "AV1 IVF required"
    );
    let video = VideoConfig {
        width: u16::from_le_bytes(header[12..14].try_into()?) as u32,
        height: u16::from_le_bytes(header[14..16].try_into()?) as u32,
        fps: u32::from_le_bytes(header[16..20].try_into()?),
        ..Default::default()
    };
    let tracks = [AudioTrack::system(), AudioTrack::microphone()];
    let mut writer = None;
    loop {
        let mut frame = [0; 12];
        if let Err(error) = file.read_exact(&mut frame) {
            if error.kind() == ErrorKind::UnexpectedEof {
                break;
            }
            return Err(error.into());
        }
        let len = u32::from_le_bytes(frame[..4].try_into()?) as usize;
        let pts = u64::from_le_bytes(frame[4..].try_into()?);
        let mut data = vec![0; len];
        file.read_exact(&mut data)?;
        let packet = MediaPacket::video(
            EncodedPacket {
                keyframe: contains_sequence_header(&data),
                data: data.into(),
                pts,
            },
            video.fps,
        );
        if writer.is_none() {
            writer = Some(MatroskaWriter::create(output, &video, &tracks, &packet)?);
        }
        let writer = writer.as_mut().unwrap();
        writer.packet(&packet)?;
        let first_sample = pts * u64::from(AUDIO_RATE) / u64::from(video.fps);
        let next_sample = (pts + 1) * u64::from(AUDIO_RATE) / u64::from(video.fps);
        for (track, hz) in [(2, 440.0), (3, 880.0)] {
            let mut pcm = Vec::new();
            for sample in first_sample..next_sample {
                let value = (sample as f64 * hz * std::f64::consts::TAU / f64::from(AUDIO_RATE))
                    .sin() as f32
                    * 0.2;
                pcm.extend(value.to_le_bytes());
                pcm.extend(value.to_le_bytes());
            }
            writer.packet(&MediaPacket {
                track,
                pts_ns: packet.pts_ns,
                duration_ns: packet.duration_ns,
                keyframe: true,
                data: pcm.into(),
            })?;
        }
    }
    writer.context("no frames")?.finish()?;
    Ok(())
}
