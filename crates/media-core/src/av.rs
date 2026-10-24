use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

pub const NS_PER_SECOND: u64 = 1_000_000_000;
pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u16 = 2;
pub const AUDIO_FRAME_BYTES: usize = 8;
pub const MAX_AUDIO_ENDPOINT_ID_UNITS: usize = 4096;

/// Endpoint identifiers are opaque: validate bounds, never trim or normalize them.
pub fn validate_audio_endpoint_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && !id.contains('\0')
            && id.encode_utf16().count() <= MAX_AUDIO_ENDPOINT_ID_UNITS,
        "Audio endpoint identifier is empty, contains NUL, or exceeds its4096-unit limit"
    );
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioSource {
    System,
    Microphone,
    /// Runtime process identity; application preferences must not persist a PID.
    Process {
        pid: u32,
        #[serde(default)]
        created_at_hns: Option<u64>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioTrack {
    pub id: u8,
    pub name: String,
    pub source: AudioSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_id: Option<String>,
}
impl AudioTrack {
    pub fn process(pid: u32) -> Result<Self> {
        ensure!(pid > 0, "application audio requires a nonzero process ID");
        Ok(Self {
            id: 2,
            name: format!("Application audio (PID {pid})"),
            source: AudioSource::Process {
                pid,
                created_at_hns: None,
            },
            endpoint_id: None,
        })
    }
    pub fn process_instance(pid: u32, created_at_hns: u64) -> Result<Self> {
        ensure!(
            created_at_hns > 0,
            "application process creation time is invalid"
        );
        let mut track = Self::process(pid)?;
        track.source = AudioSource::Process {
            pid,
            created_at_hns: Some(created_at_hns),
        };
        Ok(track)
    }
    pub fn system() -> Self {
        Self {
            id: 2,
            name: "System audio".into(),
            source: AudioSource::System,
            endpoint_id: None,
        }
    }
    pub fn microphone() -> Self {
        Self {
            id: 3,
            name: "Microphone".into(),
            source: AudioSource::Microphone,
            endpoint_id: None,
        }
    }
    pub fn with_endpoint(mut self, id: impl Into<String>) -> Result<Self> {
        self.endpoint_id = Some(id.into());
        self.validate_endpoint_selection()?;
        Ok(self)
    }
    pub fn validate_endpoint_selection(&self) -> Result<()> {
        if let Some(id) = &self.endpoint_id {
            ensure!(
                !matches!(self.source, AudioSource::Process { .. }),
                "Application audio cannot select a hardware endpoint"
            );
            validate_audio_endpoint_id(id)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct MediaPacket {
    /// Track 1 is AV1; tracks 2+ are independent stereo float PCM sources.
    pub track: u8,
    pub pts_ns: u64,
    pub duration_ns: u64,
    pub keyframe: bool,
    pub data: Arc<[u8]>,
}
impl MediaPacket {
    pub fn video(packet: super::EncodedPacket, fps: u32) -> Self {
        Self {
            track: 1,
            pts_ns: packet.pts * NS_PER_SECOND / u64::from(fps),
            duration_ns: NS_PER_SECOND / u64::from(fps),
            keyframe: packet.keyframe,
            data: packet.data,
        }
    }
    pub fn memory_bytes(&self) -> usize {
        self.data.len() + std::mem::size_of::<Self>()
    }
}

/// Shared clock is supplied by the platform (QPC on Windows).
pub trait AudioCapture {
    fn next_packet(&mut self, epoch_hns: u64) -> Result<Option<MediaPacket>>;
}

/// Byte/time bounded A/V history. Evicts whole video GOPs and corresponding audio.
pub struct AvReplayBuffer {
    video: VecDeque<MediaPacket>,
    audio: BTreeMap<u8, VecDeque<MediaPacket>>,
    bytes: usize,
    max_bytes: usize,
    max_ns: u64,
}
impl AvReplayBuffer {
    pub fn new(max_bytes: usize, seconds: u64) -> Result<Self> {
        ensure!(
            max_bytes > 0 && (1..=3600).contains(&seconds),
            "invalid replay budget"
        );
        Ok(Self {
            video: VecDeque::new(),
            audio: BTreeMap::new(),
            bytes: 0,
            max_bytes,
            max_ns: seconds * NS_PER_SECOND,
        })
    }
    pub fn push(&mut self, packet: MediaPacket) {
        if packet.track == 1 {
            if self.video.is_empty() && !packet.keyframe {
                return;
            }
            self.bytes += packet.memory_bytes();
            self.video.push_back(packet);
        } else {
            if self
                .video
                .front()
                .is_some_and(|v| packet.pts_ns + packet.duration_ns <= v.pts_ns)
            {
                return;
            }
            self.bytes += packet.memory_bytes();
            self.audio
                .entry(packet.track)
                .or_default()
                .push_back(packet);
        }
        self.prune_audio();
        while self.bytes > self.max_bytes || self.duration_ns() > self.max_ns {
            if let Some(packet) = self.video.pop_front() {
                self.bytes -= packet.memory_bytes();
            }
            while self.video.front().is_some_and(|v| !v.keyframe) {
                self.bytes -= self.video.pop_front().unwrap().memory_bytes();
            }
            self.prune_audio();
            if self.video.is_empty() {
                break;
            }
        }
    }
    fn prune_audio(&mut self) {
        // Audio callbacks can precede the first asynchronous encoder completion.
        // Retain a short, byte-bounded prefix until that keyframe arrives.
        let cutoff = self.video.front().map(|v| v.pts_ns).unwrap_or_else(|| {
            self.audio
                .values()
                .filter_map(|p| p.back())
                .map(|p| p.pts_ns + p.duration_ns)
                .max()
                .unwrap_or(0)
                .saturating_sub(200_000_000)
        });
        // Each source is chronological; separate deques avoid rescanning 120 s of audio
        // on every incoming packet when two devices deliver in different orders.
        for packets in self.audio.values_mut() {
            while packets
                .front()
                .is_some_and(|a| a.pts_ns + a.duration_ns <= cutoff)
            {
                self.bytes -= packets.pop_front().unwrap().memory_bytes();
            }
        }
        if self.video.is_empty() {
            while self.bytes > self.max_bytes {
                let oldest = self
                    .audio
                    .iter()
                    .filter_map(|(id, p)| p.front().map(|p| (*id, p.pts_ns)))
                    .min_by_key(|(_, pts)| *pts)
                    .map(|(id, _)| id);
                let Some(id) = oldest else { break };
                self.bytes -= self
                    .audio
                    .get_mut(&id)
                    .unwrap()
                    .pop_front()
                    .unwrap()
                    .memory_bytes();
            }
        }
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn duration_ns(&self) -> u64 {
        self.video
            .front()
            .zip(self.video.back())
            .map_or(0, |(a, b)| b.pts_ns + b.duration_ns - a.pts_ns)
    }
    pub fn snapshot(&self, seconds: u64) -> Vec<MediaPacket> {
        let Some(last) = self.video.back() else {
            return Vec::new();
        };
        let end = last.pts_ns + last.duration_ns;
        let cutoff = end.saturating_sub(seconds * NS_PER_SECOND);
        let Some(first) = self
            .video
            .iter()
            .rfind(|v| v.keyframe && v.pts_ns <= cutoff)
            .or_else(|| self.video.front())
        else {
            return Vec::new();
        };
        let start = first.pts_ns;
        let mut packets: Vec<_> = self
            .video
            .iter()
            .chain(self.audio.values().flatten())
            .filter(|p| p.pts_ns + p.duration_ns > start && p.pts_ns < end)
            .cloned()
            .collect();
        for packet in &mut packets {
            if packet.track != 1 {
                // Trim boundary PCM to the same video interval, preserving sample alignment.
                let skip = ((start.saturating_sub(packet.pts_ns) as u128 * u128::from(AUDIO_RATE))
                    .div_ceil(u128::from(NS_PER_SECOND))) as usize;
                let available = packet.data.len() / AUDIO_FRAME_BYTES;
                let keep = (((end.saturating_sub(packet.pts_ns)) as u128 * u128::from(AUDIO_RATE))
                    / u128::from(NS_PER_SECOND)) as usize;
                let stop = keep.min(available);
                if skip > 0 || stop < available {
                    packet.data = packet.data
                        [skip.min(stop) * AUDIO_FRAME_BYTES..stop * AUDIO_FRAME_BYTES]
                        .into();
                    packet.pts_ns += skip.min(stop) as u64 * NS_PER_SECOND / u64::from(AUDIO_RATE);
                    packet.duration_ns = packet.data.len() as u64 / AUDIO_FRAME_BYTES as u64
                        * NS_PER_SECOND
                        / u64::from(AUDIO_RATE);
                }
            }
        }
        packets.retain(|p| !p.data.is_empty());
        packets.sort_by_key(|p| (p.pts_ns, p.track));
        packets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_defaults_preserve_old_tracks_and_exact_opaque_ids() {
        let old = r#"{"id":3,"name":"Microphone","source":"Microphone"}"#;
        let track: AudioTrack = serde_json::from_str(old).unwrap();
        assert!(track.endpoint_id.is_none());
        assert_eq!(serde_json::to_string(&track).unwrap(), old);
        for track in [
            AudioTrack::system(),
            AudioTrack::microphone(),
            AudioTrack::process(42).unwrap(),
        ] {
            assert!(track.endpoint_id.is_none());
            track.validate_endpoint_selection().unwrap();
        }
        let id = "  Opaque-Unicode-设备-😀  ";
        let selected = AudioTrack::microphone().with_endpoint(id).unwrap();
        assert_eq!(selected.endpoint_id.as_deref(), Some(id));
        assert_eq!(selected.source, AudioSource::Microphone);
        let restored: AudioTrack =
            serde_json::from_str(&serde_json::to_string(&selected).unwrap()).unwrap();
        assert_eq!(restored.endpoint_id, selected.endpoint_id);
        assert!(AudioTrack::process(42).unwrap().with_endpoint(id).is_err());
        let mut forged = AudioTrack::process(42).unwrap();
        forged.endpoint_id = Some(id.into());
        assert!(forged.validate_endpoint_selection().is_err());
    }
    #[test]
    fn endpoint_validation_bounds_utf16_without_normalization() {
        assert!(validate_audio_endpoint_id("").is_err());
        assert!(validate_audio_endpoint_id("a\0b").is_err());
        assert!(validate_audio_endpoint_id(&"x".repeat(MAX_AUDIO_ENDPOINT_ID_UNITS)).is_ok());
        assert!(validate_audio_endpoint_id(&"x".repeat(MAX_AUDIO_ENDPOINT_ID_UNITS + 1)).is_err());
        assert!(validate_audio_endpoint_id(&"😀".repeat(MAX_AUDIO_ENDPOINT_ID_UNITS / 2)).is_ok());
        assert!(
            validate_audio_endpoint_id(&"😀".repeat(MAX_AUDIO_ENDPOINT_ID_UNITS / 2 + 1)).is_err()
        );
    }
    #[test]
    fn application_track_has_its_own_label_and_preserves_other_source_defaults() {
        let track = AudioTrack::process(42).unwrap();
        assert_eq!(track.id, AudioTrack::system().id);
        assert_ne!(track.id, AudioTrack::microphone().id);
        assert!(track.name.contains("Application") && track.name.contains("42"));
        assert!(AudioTrack::process(0).is_err());
        assert!(AudioTrack::process_instance(42, 0).is_err());
        assert_eq!(
            AudioTrack::process_instance(42, 7).unwrap().source,
            AudioSource::Process {
                pid: 42,
                created_at_hns: Some(7)
            }
        );
        assert_eq!(
            serde_json::to_string(&AudioSource::System).unwrap(),
            "\"System\""
        );
        assert_eq!(
            serde_json::to_string(&AudioSource::Microphone).unwrap(),
            "\"Microphone\""
        );
    }
    fn packet(track: u8, pts: u64, keyframe: bool, bytes: usize) -> MediaPacket {
        MediaPacket {
            track,
            pts_ns: pts,
            duration_ns: 10_000_000,
            keyframe,
            data: vec![0; bytes].into(),
        }
    }
    #[test]
    fn multitrack_eviction_stays_decodable_and_counts_audio() {
        let mut ring = AvReplayBuffer::new(600, 120).unwrap();
        ring.push(packet(1, 0, true, 100));
        ring.push(packet(2, 0, true, 80));
        ring.push(packet(1, 10_000_000, false, 100));
        ring.push(packet(1, 20_000_000, true, 100));
        ring.push(packet(3, 20_000_000, true, 80));
        let snapshot = ring.snapshot(120);
        assert_eq!(snapshot[0].track, 1);
        assert!(snapshot[0].keyframe);
        assert_eq!(snapshot[0].pts_ns, 20_000_000);
        assert!(ring.bytes() <= 600);
        assert!(!snapshot.iter().any(|p| p.track == 2));
    }
    #[test]
    fn snapshot_trims_audio_at_video_boundary() {
        let mut ring = AvReplayBuffer::new(100_000, 120).unwrap();
        ring.push(packet(1, 5_000_000, true, 100));
        ring.push(packet(2, 0, true, 3840));
        let snapshot = ring.snapshot(120);
        let audio = snapshot.iter().find(|p| p.track == 2).unwrap();
        assert_eq!(audio.pts_ns, 5_000_000);
        assert_eq!(audio.data.len(), 1920);
    }
    #[test]
    fn oversized_gop_and_audio_are_released() {
        let mut ring = AvReplayBuffer::new(100, 120).unwrap();
        ring.push(packet(1, 0, true, 101));
        ring.push(packet(2, 0, true, 80));
        assert_eq!(ring.bytes(), 0);
        assert!(ring.snapshot(120).is_empty());
    }
    #[test]
    fn audio_before_encoder_completion_is_retained_without_an_unbounded_prefix() {
        let mut ring = AvReplayBuffer::new(100_000, 120).unwrap();
        ring.push(packet(2, 0, true, 80));
        ring.push(packet(3, 0, true, 80));
        assert!(ring.snapshot(120).is_empty());
        ring.push(packet(1, 0, true, 100));
        let snapshot = ring.snapshot(120);
        assert!(snapshot.iter().any(|p| p.track == 2 && p.pts_ns == 0));
        assert!(snapshot.iter().any(|p| p.track == 3 && p.pts_ns == 0));
        let mut waiting = AvReplayBuffer::new(1000, 120).unwrap();
        for i in 0..1000 {
            waiting.push(packet(2, i * 10_000_000, true, 80));
            assert!(waiting.bytes() <= 1000);
        }
        assert!(waiting.bytes() > 0);
        assert!(waiting.snapshot(120).is_empty());
    }
}
