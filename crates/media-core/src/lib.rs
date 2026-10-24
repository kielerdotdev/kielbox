//! Platform-neutral media contracts. Raw pixels never pass through this API.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::Arc};
mod av;
pub use av::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub keyframe_interval: u32,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            width: 2560,
            height: 1440,
            fps: 60,
            bitrate: 25_000_000,
            keyframe_interval: 120,
        }
    }
}

impl VideoConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.width > 0
                && self.height > 0
                && self.width.is_multiple_of(2)
                && self.height.is_multiple_of(2),
            "4:2:0 dimensions must be positive and even"
        );
        ensure!(
            self.width <= 8192 && self.height <= 8192,
            "dimensions exceed supported limit"
        );
        ensure!(
            (1..=240).contains(&self.fps),
            "FPS must be between 1 and 240"
        );
        ensure!(
            (1_000_000..=200_000_000).contains(&self.bitrate),
            "bitrate must be between 1 and 200 Mbit/s"
        );
        ensure!(
            self.keyframe_interval > 0,
            "keyframe interval must be nonzero"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    Av1,
    H264,
    Hevc,
}

#[derive(Clone, Debug)]
pub struct EncoderCapabilities {
    pub backend: &'static str,
    pub codecs: Vec<Codec>,
    pub gpu_resident_input: bool,
}

#[derive(Clone, Debug)]
pub struct EncodedPacket {
    pub data: Arc<[u8]>,
    /// Frame ticks at the configured FPS. No reorder is used in the initial backend.
    pub pts: u64,
    pub keyframe: bool,
}

/// Backend-owned GPU resource; ownership and completion stay with the backend.
pub trait VideoEncoder {
    type Surface;
    fn capabilities(&self) -> EncoderCapabilities;
    fn submit(&mut self, surface: &Self::Surface, pts: u64, force_keyframe: bool) -> Result<()>;
    fn receive(&mut self, wait: bool) -> Result<Option<EncodedPacket>>;
    fn flush(&mut self) -> Result<()>;
}

pub trait VideoCapture {
    type Frame;
    fn next_frame(&mut self, timeout_ms: u32) -> Result<Option<Self::Frame>>;
}

/// Drops complete GOPs only. A snapshot shares packets instead of copying payloads.
/// Snapshot owners must enforce a separate retention budget before accepting saves.
pub struct ReplayBuffer {
    packets: VecDeque<EncodedPacket>,
    bytes: usize,
    max_bytes: usize,
    max_ticks: u64,
}

impl ReplayBuffer {
    pub fn new(max_bytes: usize, max_ticks: u64) -> Result<Self> {
        ensure!(
            max_bytes > 0 && max_ticks > 0,
            "replay limits must be positive"
        );
        Ok(Self {
            packets: VecDeque::new(),
            bytes: 0,
            max_bytes,
            max_ticks,
        })
    }
    pub fn push(&mut self, packet: EncodedPacket) {
        if self.packets.is_empty() && !packet.keyframe {
            return;
        }
        self.bytes += packet.data.len();
        self.packets.push_back(packet);
        while self.over_budget() {
            // If even a single GOP exceeds the byte cap, discard it and await a keyframe.
            if let Some(p) = self.packets.pop_front() {
                self.bytes -= p.data.len();
            }
            while self.packets.front().is_some_and(|p| !p.keyframe) {
                self.bytes -= self.packets.pop_front().unwrap().data.len();
            }
        }
    }
    fn over_budget(&self) -> bool {
        self.bytes > self.max_bytes
            || self
                .packets
                .front()
                .zip(self.packets.back())
                .is_some_and(|(a, b)| b.pts.saturating_sub(a.pts) > self.max_ticks)
    }
    pub fn snapshot(&self) -> Vec<EncodedPacket> {
        self.packets.iter().cloned().collect()
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn len(&self) -> usize {
        self.packets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PipelineStats {
    pub adapter: String,
    pub captured: u64,
    pub submitted: u64,
    pub encoded: u64,
    pub repeated: u64,
    pub missed_deadlines: u64,
    pub preview_published: u64,
    pub compressed_bytes: u64,
    pub elapsed_seconds: f64,
    pub finalization_seconds: f64,
    pub raw_cpu_readbacks: u64,
    pub encode_wait_ms_total: f64,
    pub replay_bytes: usize,
    pub audio_packets: u64,
    pub audio_discontinuities: u64,
    pub audio_errors: Vec<String>,
    pub replay_seconds: f64,
    pub replay_saves: u64,
    pub output: String,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(pts: u64, keyframe: bool, bytes: usize) -> EncodedPacket {
        EncodedPacket {
            data: vec![0; bytes].into(),
            pts,
            keyframe,
        }
    }
    #[test]
    fn replay_evicts_dependencies_and_snapshots_share_payload() {
        let mut b = ReplayBuffer::new(30, 100).unwrap();
        b.push(packet(0, true, 10));
        b.push(packet(1, false, 10));
        let snap = b.snapshot();
        assert!(Arc::ptr_eq(&snap[0].data, &b.snapshot()[0].data));
        b.push(packet(2, true, 10));
        b.push(packet(3, false, 10));
        assert_eq!(
            b.snapshot().iter().map(|p| p.pts).collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(snap.len(), 2);
    }
    #[test]
    fn oversized_gop_is_dropped_until_next_keyframe() {
        let mut b = ReplayBuffer::new(5, 100).unwrap();
        b.push(packet(0, true, 6));
        b.push(packet(1, false, 1));
        assert!(b.is_empty());
        b.push(packet(2, true, 2));
        assert_eq!(b.bytes(), 2);
    }
    #[test]
    fn time_limit_keeps_a_decodable_start() {
        let mut b = ReplayBuffer::new(1000, 2).unwrap();
        for i in 0..6 {
            b.push(packet(i, i % 2 == 0, 1));
        }
        assert_eq!(b.snapshot()[0].pts, 4);
    }
}
