//! Optional offline processing of an owned replay snapshot. The capture ring,
//! continuous recording, video and system-audio allocations are left untouched.
use anyhow::{ensure, Result};
use rcr_dpdfnet::Denoiser;
use rcr_media_core::{AudioSource, AudioTrack, MediaPacket, AUDIO_FRAME_BYTES};

pub fn suppress_microphone(
    packets: &mut [MediaPacket],
    tracks: &[AudioTrack],
    mut cancelled: impl FnMut() -> bool,
) -> Result<()> {
    let microphone = tracks
        .iter()
        .filter(|t| t.source == AudioSource::Microphone)
        .map(|t| t.id)
        .collect::<Vec<_>>();
    ensure!(
        !microphone.is_empty(),
        "microphone suppression requires a microphone track"
    );
    let mut denoiser = Denoiser::new()?;
    for track in microphone {
        let indices = packets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.track == track)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let mut first = 0;
        while first < indices.len() {
            ensure!(!cancelled(), "microphone suppression cancelled");
            let mut last = first + 1;
            while last < indices.len() {
                let a = &packets[indices[last - 1]];
                let b = &packets[indices[last]];
                if b.pts_ns.abs_diff(a.pts_ns + a.duration_ns) > 2_000_000 {
                    break;
                }
                last += 1;
            }
            let segment = &indices[first..last];
            let source = segment
                .iter()
                .map(|i| packets[*i].data.clone())
                .collect::<Vec<_>>();
            ensure!(
                source
                    .iter()
                    .all(|p| !p.is_empty() && p.len().is_multiple_of(AUDIO_FRAME_BYTES)),
                "unaligned/empty microphone PCM"
            );
            denoiser.reset();
            let mut target = 0;
            let mut encoded = Vec::with_capacity(source[0].len());
            let mut emit = |samples: &[f32]| -> Result<()> {
                ensure!(!cancelled(), "microphone suppression cancelled");
                for value in samples {
                    encoded.extend_from_slice(&value.to_le_bytes());
                    encoded.extend_from_slice(&value.to_le_bytes());
                    if encoded.len() == source[target].len() {
                        packets[segment[target]].data = std::mem::take(&mut encoded).into();
                        target += 1;
                        if target < source.len() {
                            encoded = Vec::with_capacity(source[target].len());
                        }
                    }
                }
                Ok(())
            };
            // Fixed scratch: no full-track decoded buffer and no per-hop allocation.
            let mut mono = [0f32; 480];
            for data in &source {
                for bytes in data.chunks(480 * AUDIO_FRAME_BYTES) {
                    let count = bytes.len() / AUDIO_FRAME_BYTES;
                    for (out, frame) in mono[..count].iter_mut().zip(bytes.as_chunks::<8>().0) {
                        *out = (f32::from_le_bytes(frame[..4].try_into()?)
                            + f32::from_le_bytes(frame[4..].try_into()?))
                            * 0.5;
                    }
                    denoiser.process(&mono[..count], &mut emit)?;
                }
            }
            denoiser.finish(&mut emit)?;
            ensure!(
                target == source.len() && encoded.is_empty(),
                "suppressed microphone sample count mismatch"
            );
            first = last;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn packet(track: u8, pts_ns: u64, data: Vec<u8>) -> MediaPacket {
        MediaPacket {
            track,
            pts_ns,
            duration_ns: (data.len() / 8) as u64 * 1_000_000_000 / 48_000,
            keyframe: true,
            data: data.into(),
        }
    }
    #[test]
    fn only_microphone_changes_and_packet_clock_and_length_are_preserved() {
        let raw = include_bytes!("../../dpdfnet/tests/fixtures/audio-input.f32");
        let expected = include_bytes!("../../dpdfnet/tests/fixtures/audio-output.f32");
        let mut packets = vec![packet(1, 0, vec![1, 2, 3]), packet(2, 0, vec![0; 480 * 8])];
        let original = packets.clone();
        let mut offset = 0;
        for chunk in raw.chunks(317 * 4) {
            let stereo = chunk
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|v| v.iter().chain(v.iter()).copied())
                .collect::<Vec<_>>();
            packets.push(packet(3, offset * 1_000_000_000 / 48_000, stereo));
            offset += (chunk.len() / 4) as u64;
        }
        let before = packets.clone();
        suppress_microphone(
            &mut packets,
            &[AudioTrack::system(), AudioTrack::microphone()],
            || false,
        )
        .unwrap();
        for (a, b) in packets.iter().zip(&before) {
            assert_eq!(
                (a.track, a.pts_ns, a.duration_ns, a.data.len()),
                (b.track, b.pts_ns, b.duration_ns, b.data.len())
            );
        }
        for i in 0..2 {
            assert!(Arc::ptr_eq(&packets[i].data, &original[i].data));
        }
        let mono = packets[2..]
            .iter()
            .flat_map(|p| p.data.as_chunks::<8>().0.iter())
            .map(|frame| {
                assert_eq!(&frame[..4], &frame[4..]);
                f32::from_le_bytes(frame[..4].try_into().unwrap())
            })
            .collect::<Vec<_>>();
        let maximum = mono
            .iter()
            .zip(expected.as_chunks::<4>().0)
            .map(|(a, b)| (a - f32::from_le_bytes(*b)).abs())
            .fold(0f32, f32::max);
        assert!(maximum < 0.001, "packetized PCM parity error {maximum}");
        assert_eq!(
            &*before[2].data,
            &raw[..317 * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|v| v.iter().chain(v.iter()).copied())
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn discontinuous_segments_reset_model_and_cancellation_is_explicit() {
        let signal = (0..137)
            .flat_map(|i| {
                let v = (i as f32 * 0.17).sin() * 0.2;
                v.to_le_bytes().into_iter().chain(v.to_le_bytes())
            })
            .collect::<Vec<_>>();
        let mut packets = vec![
            packet(3, 0, signal.clone()),
            packet(3, 1_000_000_000, signal),
        ];
        let tracks = [AudioTrack::microphone()];
        let before = packets.clone();
        assert!(suppress_microphone(&mut packets, &tracks, || true).is_err());
        assert!(Arc::ptr_eq(&packets[0].data, &before[0].data));
        suppress_microphone(&mut packets, &tracks, || false).unwrap();
        assert_eq!(packets[0].data, packets[1].data);
        assert_eq!(packets[1].pts_ns, 1_000_000_000);
    }
}
