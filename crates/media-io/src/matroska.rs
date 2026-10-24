//! Minimal Matroska muxer for the recorder's AV1 + independent float PCM tracks.
//! No raw video pixels, subprocess, or external runtime in the recording path.
use anyhow::{Context, Result, bail, ensure};
use rcr_media_core::{AUDIO_RATE, AudioTrack, MediaPacket, NS_PER_SECOND, VideoConfig};
use std::{
    fs::{self, File},
    io::{BufWriter, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
pub const RCR_METADATA_TAG: &str = "_RCR_METADATA";
pub const RCR_METADATA_SCHEMA_VERSION: u32 = 1;
pub const MAX_METADATA_JSON_BYTES: usize = 1024 * 1024;
const CUES_ID: u32 = 0x1c53bb6b;
const TAGS_ID: u32 = 0x1254c367;

struct BoundedJson(Vec<u8>);
impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > MAX_METADATA_JSON_BYTES)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "recording metadata exceeds 1 MiB",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn metadata_tags(json: &[u8]) -> Vec<u8> {
    // Empty Targets applies to the Segment globally. Explicit language/default
    // values match Matroska's normal defaults, independent of caller locale.
    master(
        TAGS_ID,
        [master(
            0x7373,
            [
                element(0x63c0, &[]),
                master(
                    0x67c8,
                    [
                        text(0x45a3, RCR_METADATA_TAG),
                        text(0x447a, "und"),
                        uint(0x4484, 1),
                        element(0x4487, json),
                    ],
                ),
            ],
        )],
    )
}

fn vint(value: u64) -> Vec<u8> {
    for bytes in 1..=8 {
        if value < (1u64 << (bytes * 7)) - 1 {
            let encoded = value | (1u64 << (bytes * 7));
            return encoded.to_be_bytes()[8 - bytes..].to_vec();
        }
    }
    unreachable!("element larger than EBML limit")
}
fn id(value: u32) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    bytes[bytes.iter().position(|b| *b != 0).unwrap_or(3)..].to_vec()
}
fn element(tag: u32, data: &[u8]) -> Vec<u8> {
    let mut result = id(tag);
    result.extend(vint(data.len() as u64));
    result.extend(data);
    result
}
fn uint(tag: u32, value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    element(
        tag,
        &bytes[bytes.iter().position(|b| *b != 0).unwrap_or(7)..],
    )
}
fn text(tag: u32, value: &str) -> Vec<u8> {
    element(tag, value.as_bytes())
}
fn master(tag: u32, children: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    element(tag, &children.into_iter().flatten().collect::<Vec<_>>())
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}
impl Bits<'_> {
    fn read(&mut self, count: usize) -> Result<u64> {
        ensure!(
            count <= 64 && self.at + count <= self.data.len() * 8,
            "truncated AV1 sequence header"
        );
        let mut result = 0;
        for _ in 0..count {
            result = (result << 1) | u64::from((self.data[self.at / 8] >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Ok(result)
    }
    fn skip(&mut self, count: usize) -> Result<()> {
        self.read(count).map(|_| ())
    }
    fn uvlc(&mut self) -> Result<()> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            ensure!(zeros <= 32, "invalid AV1 UVLC");
        }
        self.skip(zeros)
    }
}

/// Low-overhead AV1 OBU parser; returns the complete sequence OBU and its payload.
fn sequence_obu(data: &[u8]) -> Result<(&[u8], &[u8])> {
    let mut at = 0;
    while at < data.len() {
        let start = at;
        let header = data[at];
        at += 1;
        ensure!(header & 0x81 == 0, "invalid AV1 OBU header");
        let kind = (header >> 3) & 15;
        if header & 4 != 0 {
            ensure!(at < data.len(), "truncated OBU extension");
            at += 1;
        }
        let len = if header & 2 != 0 {
            let mut value = 0usize;
            let mut done = false;
            for shift in (0..56).step_by(7) {
                let byte = *data.get(at).context("truncated OBU length")?;
                at += 1;
                value |= usize::from(byte & 0x7f) << shift;
                if byte & 0x80 == 0 {
                    done = true;
                    break;
                }
            }
            ensure!(done, "oversized OBU length");
            value
        } else {
            data.len() - at
        };
        let end = at.checked_add(len).context("OBU size overflow")?;
        ensure!(end <= data.len(), "truncated OBU payload");
        if kind == 1 {
            return Ok((&data[start..end], &data[at..end]));
        }
        at = end;
    }
    bail!("AV1 keyframe does not contain a sequence header")
}

pub fn av1_codec_private(data: &[u8]) -> Result<Vec<u8>> {
    let (obu, payload) = sequence_obu(data)?;
    let mut bits = Bits {
        data: payload,
        at: 0,
    };
    let profile = bits.read(3)? as u8;
    ensure!(
        profile == 0,
        "initial recorder muxer supports AV1 Main / NV12 only"
    );
    bits.skip(1)?; // still_picture
    let reduced = bits.read(1)? != 0;
    let mut tier = 0;
    let level = if reduced {
        bits.read(5)? as u8
    } else {
        let timing = bits.read(1)? != 0;
        if timing {
            bits.skip(64)?;
            if bits.read(1)? != 0 {
                bits.uvlc()?;
            }
            if bits.read(1)? != 0 {
                bits.skip(5 + 32 + 5 + 5)?;
            }
        }
        bits.skip(1)?; // initial_display_delay_present_flag
        bits.skip(5)?; // operating_points_cnt_minus_1
        bits.skip(12)?; // operating_point_idc[0]
        let level = bits.read(5)? as u8;
        if level > 7 {
            tier = bits.read(1)? as u8;
        }
        level
    };
    // This pipeline explicitly produces 8-bit, non-monochrome 4:2:0 video.
    let mut private = vec![0x81, (profile << 5) | level, (tier << 7) | 0x0c, 0];
    private.extend_from_slice(obu);
    Ok(private)
}

pub fn contains_sequence_header(data: &[u8]) -> bool {
    sequence_obu(data).is_ok()
}

pub struct MatroskaWriter {
    file: BufWriter<File>,
    path: PathBuf,
    partial: PathBuf,
    position: u64,
    segment_start: u64,
    duration_offset: u64,
    cues_offset: u64,
    tags_offset: u64,
    base_ns: u64,
    end_ns: u64,
    cues: Vec<(u64, u64)>,
    tracks: Vec<u8>,
    video: VideoConfig,
    audio: Vec<AudioTrack>,
    packet_counts: Vec<u64>,
    video_frames: u64,
    video_keyframes: u64,
    video_bytes: u64,
}

impl MatroskaWriter {
    pub fn create(
        path: &Path,
        video: &VideoConfig,
        audio: &[AudioTrack],
        first: &MediaPacket,
    ) -> Result<Self> {
        video.validate()?;
        ensure!(
            first.track == 1 && first.keyframe,
            "recording must begin with a video keyframe"
        );
        ensure!(!path.exists(), "output exists: {}", path.display());
        let mut track_ids = vec![1];
        for track in audio {
            ensure!(
                (2..=126).contains(&track.id) && !track_ids.contains(&track.id),
                "invalid/duplicate track number"
            );
            track_ids.push(track.id);
        }
        let private = av1_codec_private(&first.data)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut partial_name = path.as_os_str().to_os_string();
        partial_name.push(".partial");
        let partial = PathBuf::from(partial_name);
        let file = BufWriter::with_capacity(1024 * 1024, File::create_new(&partial)?);
        let mut this = Self {
            file,
            path: path.to_owned(),
            partial,
            position: 0,
            segment_start: 0,
            duration_offset: 0,
            cues_offset: 0,
            tags_offset: 0,
            base_ns: first.pts_ns,
            end_ns: 0,
            cues: Vec::new(),
            packet_counts: vec![0; track_ids.len()],
            tracks: track_ids,
            video: video.clone(),
            audio: audio.to_vec(),
            video_frames: 0,
            video_keyframes: 0,
            video_bytes: 0,
        };
        this.write(&master(
            0x1a45dfa3,
            [
                uint(0x4286, 1),
                uint(0x42f7, 1),
                uint(0x42f2, 4),
                uint(0x42f3, 8),
                text(0x4282, "matroska"),
                uint(0x4287, 4),
                uint(0x4285, 2),
            ],
        ))?;
        this.write(&id(0x18538067))?;
        this.write(&[0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])?;
        this.segment_start = this.position;
        let cue_seek = master(
            0x4dbb,
            [element(0x53ab, &id(CUES_ID)), element(0x53ac, &[0; 8])],
        );
        let tags_seek = master(
            0x4dbb,
            [element(0x53ab, &id(TAGS_ID)), element(0x53ac, &[0; 8])],
        );
        let seek = master(0x114d9b74, [cue_seek.clone(), tags_seek.clone()]);
        let seek_header = seek.len() - cue_seek.len() - tags_seek.len();
        this.cues_offset = this.position + (seek_header + cue_seek.len() - 8) as u64;
        this.tags_offset = this.position + seek.len() as u64 - 8;
        this.write(&seek)?;
        let info = master(
            0x1549a966,
            [
                uint(0x2ad7b1, 1_000_000),
                text(0x4d80, "rcr"),
                text(0x5741, "rcr Rust AV1 recorder"),
                element(0x4489, &0f64.to_be_bytes()),
            ],
        );
        this.duration_offset = this.position + info.len() as u64 - 8;
        this.write(&info)?;
        let mut entries = vec![master(
            0xae,
            [
                uint(0xd7, 1),
                uint(0x73c5, 1),
                uint(0x83, 1),
                uint(0x9c, 0),
                text(0x536e, "Screen"),
                text(0x86, "V_AV1"),
                element(0x63a2, &private),
                uint(0x23e383, NS_PER_SECOND / u64::from(video.fps)),
                master(
                    0xe0,
                    [
                        uint(0xb0, u64::from(video.width)),
                        uint(0xba, u64::from(video.height)),
                        master(
                            0x55b0,
                            [
                                uint(0x55b1, 1),
                                uint(0x55b9, 1),
                                uint(0x55ba, 1),
                                uint(0x55bb, 1),
                            ],
                        ),
                    ],
                ),
            ],
        )];
        for (i, track) in audio.iter().enumerate() {
            entries.push(master(
                0xae,
                [
                    uint(0xd7, u64::from(track.id)),
                    uint(0x73c5, u64::from(track.id)),
                    uint(0x83, 2),
                    uint(0x9c, 0),
                    uint(0x88, u64::from(i == 0)),
                    text(0x536e, &track.name),
                    text(0x86, "A_PCM/FLOAT/IEEE"),
                    master(
                        0xe1,
                        [
                            element(0xb5, &f64::from(AUDIO_RATE).to_be_bytes()),
                            uint(0x9f, 2),
                            uint(0x6264, 32),
                        ],
                    ),
                ],
            ));
        }
        this.write(&master(0x1654ae6b, entries))?;
        Ok(this)
    }
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.file.write_all(data)?;
        self.position += data.len() as u64;
        Ok(())
    }
    pub fn packet(&mut self, packet: &MediaPacket) -> Result<()> {
        ensure!(
            self.tracks.contains(&packet.track),
            "packet references unknown track"
        );
        if packet.pts_ns < self.base_ns || packet.data.is_empty() {
            return Ok(());
        }
        let ns = packet.pts_ns - self.base_ns;
        let ms = (ns + 500_000) / 1_000_000;
        let position = self.position - self.segment_start;
        let stamp = uint(0xe7, ms);
        let block_len = packet.data.len() as u64 + 4;
        let block_size = vint(block_len);
        let cluster_len = stamp.len() as u64 + 1 + block_size.len() as u64 + block_len;
        self.write(&id(0x1f43b675))?;
        self.write(&vint(cluster_len))?;
        self.write(&stamp)?;
        self.write(&[0xa3])?;
        self.write(&block_size)?;
        self.write(&[
            0x80 | packet.track,
            0,
            0,
            if packet.keyframe { 0x80 } else { 0 },
        ])?;
        self.write(&packet.data)?;
        if packet.track == 1 && packet.keyframe {
            self.cues.push((ms, position));
        }
        self.end_ns = self.end_ns.max(ns + packet.duration_ns);
        if let Some(index) = self.tracks.iter().position(|track| *track == packet.track) {
            self.packet_counts[index] = self.packet_counts[index].saturating_add(1);
        }
        if packet.track == 1 {
            self.video_frames = self.video_frames.saturating_add(1);
            self.video_keyframes = self
                .video_keyframes
                .saturating_add(u64::from(packet.keyframe));
            self.video_bytes = self.video_bytes.saturating_add(packet.data.len() as u64);
        }
        Ok(())
    }
    pub fn finish(self) -> Result<PathBuf> {
        self.finish_inner("recording", None)
    }
    /// Add capture/replay statistics before publishing the completed file. Mux
    /// facts remain authoritative; caller data lives under the `session` key.
    pub fn finish_with_metadata(self, metadata: &serde_json::Value) -> Result<PathBuf> {
        self.finish_inner("recording", Some(metadata))
    }
    fn metadata_json(&self, kind: &str, session: Option<&serde_json::Value>) -> Result<Vec<u8>> {
        #[derive(serde::Serialize)]
        struct Video<'a> {
            codec: &'static str,
            #[serde(flatten)]
            config: &'a VideoConfig,
        }
        #[derive(serde::Serialize)]
        struct Audio<'a> {
            track_id: u8,
            name: &'a str,
            source_type: &'static str,
            sample_rate: u32,
            channels: u16,
            codec: &'static str,
            packet_count: u64,
        }
        #[derive(serde::Serialize)]
        struct Clip {
            duration_ns: u64,
            video_frames: u64,
            video_keyframes: u64,
            video_bytes: u64,
            packet_count: u64,
        }
        #[derive(serde::Serialize)]
        struct Metadata<'a> {
            schema_version: u32,
            kind: &'a str,
            video: Video<'a>,
            audio_tracks: Vec<Audio<'a>>,
            clip: Clip,
            session: Option<&'a serde_json::Value>,
        }
        let audio_tracks = self
            .audio
            .iter()
            .map(|track| Audio {
                track_id: track.id,
                name: &track.name,
                source_type: match &track.source {
                    rcr_media_core::AudioSource::System => "system",
                    rcr_media_core::AudioSource::Microphone => "microphone",
                    rcr_media_core::AudioSource::Process { .. } => "application",
                },
                sample_rate: AUDIO_RATE,
                channels: 2,
                codec: "A_PCM/FLOAT/IEEE",
                packet_count: self
                    .tracks
                    .iter()
                    .position(|id| *id == track.id)
                    .map_or(0, |index| self.packet_counts[index]),
            })
            .collect();
        let metadata = Metadata {
            schema_version: RCR_METADATA_SCHEMA_VERSION,
            kind,
            video: Video {
                codec: "AV1",
                config: &self.video,
            },
            audio_tracks,
            clip: Clip {
                duration_ns: self.end_ns,
                video_frames: self.video_frames,
                video_keyframes: self.video_keyframes,
                video_bytes: self.video_bytes,
                packet_count: self
                    .packet_counts
                    .iter()
                    .copied()
                    .fold(0u64, u64::saturating_add),
            },
            session,
        };
        let mut output = BoundedJson(Vec::new());
        serde_json::to_writer(&mut output, &metadata)
            .context("serialize bounded recording metadata")?;
        Ok(output.0)
    }
    fn finish_inner(mut self, kind: &str, metadata: Option<&serde_json::Value>) -> Result<PathBuf> {
        // Validate/serialize before writing final elements or publishing. The
        // bounded writer never clones an arbitrarily large caller JSON value.
        let tags = metadata_tags(&self.metadata_json(kind, metadata)?);
        let cues_position = self.position - self.segment_start;
        let cues = master(
            CUES_ID,
            self.cues.iter().map(|(time, pos)| {
                master(
                    0xbb,
                    [
                        uint(0xb3, *time),
                        master(0xb7, [uint(0xf7, 1), uint(0xf1, *pos)]),
                    ],
                )
            }),
        );
        self.write(&cues)?;
        let tags_position = self.position - self.segment_start;
        self.write(&tags)?;
        self.file.flush()?;
        self.file.seek(SeekFrom::Start(self.duration_offset))?;
        self.file
            .write_all(&(self.end_ns as f64 / 1_000_000.0).to_be_bytes())?;
        self.file.seek(SeekFrom::Start(self.cues_offset))?;
        self.file.write_all(&cues_position.to_be_bytes())?;
        self.file.seek(SeekFrom::Start(self.tags_offset))?;
        self.file.write_all(&tags_position.to_be_bytes())?;
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        drop(self.file);
        // A destination can appear after create() checked it. Never replace
        // that file, including on Windows where ordinary rename overwrites.
        // Keep our completed partial on failure so the recording is recoverable.
        let mut partial = tempfile::TempPath::try_from_path(&self.partial)?;
        partial.disable_cleanup(true);
        partial.persist_noclobber(&self.path).with_context(|| {
            format!(
                "could not publish recording to {}; completed recording remains at {}",
                self.path.display(),
                self.partial.display()
            )
        })?;
        Ok(self.path)
    }
}

pub fn save_snapshot(
    path: &Path,
    config: &VideoConfig,
    tracks: &[AudioTrack],
    packets: &[MediaPacket],
) -> Result<PathBuf> {
    let first = packets
        .iter()
        .find(|p| p.track == 1)
        .context("replay buffer is empty")?;
    let mut writer = MatroskaWriter::create(path, config, tracks, first)?;
    for packet in packets {
        writer.packet(packet)?;
    }
    writer.finish_inner("replay", None)
}
#[cfg(test)]
mod tests {
    use super::*;
    // Independent tiny EBML walker for emitted test files, including unknown
    // Segment size. Offsets are relative to the parent's payload, as SeekHead is.
    fn elements(mut bytes: &[u8]) -> Vec<(u32, usize, &[u8])> {
        let mut result = Vec::new();
        let mut offset = 0;
        while !bytes.is_empty() {
            let id_len = bytes[0].leading_zeros() as usize + 1;
            assert!((1..=4).contains(&id_len) && bytes.len() > id_len);
            let tag = bytes[..id_len]
                .iter()
                .fold(0u32, |value, byte| (value << 8) | u32::from(*byte));
            let size_len = bytes[id_len].leading_zeros() as usize + 1;
            assert!((1..=8).contains(&size_len) && bytes.len() >= id_len + size_len);
            let mut size = u64::from(bytes[id_len] & ((1u8 << (8 - size_len)) - 1));
            for byte in &bytes[id_len + 1..id_len + size_len] {
                size = (size << 8) | u64::from(*byte);
            }
            let header = id_len + size_len;
            let size = if size == (1u64 << (size_len * 7)) - 1 {
                bytes.len() - header
            } else {
                size as usize
            };
            assert!(size <= bytes.len() - header);
            result.push((tag, offset, &bytes[header..header + size]));
            bytes = &bytes[header + size..];
            offset += header + size;
        }
        result
    }
    fn child(bytes: &[u8], tag: u32) -> &[u8] {
        elements(bytes)
            .into_iter()
            .find(|element| element.0 == tag)
            .unwrap()
            .2
    }
    fn integer(bytes: &[u8]) -> u64 {
        bytes
            .iter()
            .fold(0, |value, byte| (value << 8) | u64::from(*byte))
    }
    fn recording_metadata(bytes: &[u8]) -> serde_json::Value {
        let segment = child(bytes, 0x18538067);
        let tag = child(child(segment, TAGS_ID), 0x7373);
        assert!(
            child(tag, 0x63c0).is_empty(),
            "Metadata was not global to the Segment"
        );
        let simple = child(tag, 0x67c8);
        assert_eq!(child(simple, 0x45a3), RCR_METADATA_TAG.as_bytes());
        assert_eq!(child(simple, 0x447a), b"und");
        assert_eq!(integer(child(simple, 0x4484)), 1);
        let seeks = elements(child(segment, 0x114d9b74));
        for target in [CUES_ID, TAGS_ID] {
            let seek = seeks
                .iter()
                .find(|(_, _, payload)| child(payload, 0x53ab) == id(target))
                .unwrap();
            let offset = integer(child(seek.2, 0x53ac)) as usize;
            assert!(
                segment[offset..].starts_with(&id(target)),
                "SeekHead pointed at the wrong late element"
            );
        }
        serde_json::from_slice(child(simple, 0x4487)).unwrap()
    }
    fn first_packet() -> MediaPacket {
        MediaPacket {
            track: 1,
            pts_ns: 0,
            duration_ns: 1_000_000_000 / 60,
            keyframe: true,
            data: vec![0x12, 0, 0x0a, 2, 0x18, 0].into(),
        }
    }
    #[test]
    fn finish_embeds_versioned_global_json_and_indexes_late_tags_before_publication() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tagged.mkv");
        let mut packet = first_packet();
        packet.pts_ns = 35_000_000_000;
        let track = AudioTrack {
            id: 2,
            name: "Mic \"desk\" 🎙".into(),
            source: rcr_media_core::AudioSource::Microphone,
            endpoint_id: Some("opaque-private-endpoint".into()),
        };
        let config = VideoConfig::default();
        let mut writer = MatroskaWriter::create(&path, &config, &[track], &packet).unwrap();
        writer.packet(&packet).unwrap();
        let audio = MediaPacket {
            track: 2,
            pts_ns: packet.pts_ns,
            duration_ns: 10_000_000,
            keyframe: false,
            data: vec![0; 8].into(),
        };
        writer.packet(&audio).unwrap();
        let payload = serde_json::json!({"statistics":{"encoded":1},"label":"quoted \" UTF-8 ø"});
        writer.finish_with_metadata(&payload).unwrap();
        let bytes = fs::read(&path).unwrap();
        let metadata = recording_metadata(&bytes);
        assert_eq!(metadata["schema_version"], 1);
        assert_eq!(metadata["kind"], "recording");
        assert_eq!(metadata["video"]["codec"], "AV1");
        assert_eq!(metadata["video"]["fps"], config.fps);
        assert_eq!(metadata["clip"]["duration_ns"], packet.duration_ns);
        assert_eq!(metadata["clip"]["video_frames"], 1);
        assert_eq!(metadata["clip"]["video_keyframes"], 1);
        assert_eq!(metadata["clip"]["video_bytes"], packet.data.len());
        assert_eq!(metadata["clip"]["packet_count"], 2);
        assert_eq!(metadata["audio_tracks"][0]["source_type"], "microphone");
        assert_eq!(metadata["audio_tracks"][0]["packet_count"], 1);
        assert_eq!(metadata["session"], payload);
        assert!(!String::from_utf8_lossy(&bytes).contains("opaque-private-endpoint"));
    }
    #[test]
    fn replay_snapshot_always_embeds_mux_facts_without_an_external_stats_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("replay.mkv");
        let packet = first_packet();
        save_snapshot(&path, &VideoConfig::default(), &[], &[packet]).unwrap();
        let metadata = recording_metadata(&fs::read(&path).unwrap());
        assert_eq!(metadata["kind"], "replay");
        assert_eq!(metadata["clip"]["video_frames"], 1);
        assert!(metadata["session"].is_null());
    }
    #[test]
    fn oversized_json_fails_before_publishing_and_retains_the_owned_partial() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversized.mkv");
        let packet = first_packet();
        let mut writer =
            MatroskaWriter::create(&path, &VideoConfig::default(), &[], &packet).unwrap();
        writer.packet(&packet).unwrap();
        let oversized = serde_json::Value::String("x".repeat(MAX_METADATA_JSON_BYTES));
        assert!(writer.finish_with_metadata(&oversized).is_err());
        assert!(
            !path.exists(),
            "Oversized metadata published a completed destination"
        );
        assert!(path.with_extension("mkv.partial").is_file());
        let mut bounded = BoundedJson(Vec::new());
        bounded
            .write_all(&vec![b'x'; MAX_METADATA_JSON_BYTES])
            .unwrap();
        assert!(bounded.write_all(b"x").is_err());
        assert_eq!(bounded.0.len(), MAX_METADATA_JSON_BYTES);
    }

    #[test]
    fn completed_recording_never_overwrites_a_destination_created_during_capture() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recording.mkv");
        let partial = directory.path().join("recording.mkv.partial");
        let packet = first_packet();
        let mut writer =
            MatroskaWriter::create(&path, &VideoConfig::default(), &[], &packet).unwrap();
        writer.packet(&packet).unwrap();
        fs::write(&path, b"another owner's file").unwrap();
        let error = writer.finish().unwrap_err();
        assert_eq!(fs::read(&path).unwrap(), b"another owner's file");
        assert!(fs::metadata(&partial).unwrap().len() > packet.data.len() as u64);
        assert!(format!("{error:#}").contains("completed recording remains at"));
    }

    #[test]
    fn existing_partial_is_preserved_and_successful_publication_removes_owned_partial() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recording.mkv");
        let partial = directory.path().join("recording.mkv.partial");
        let packet = first_packet();
        fs::write(&partial, b"older recoverable recording").unwrap();
        assert!(MatroskaWriter::create(&path, &VideoConfig::default(), &[], &packet).is_err());
        assert_eq!(fs::read(&partial).unwrap(), b"older recoverable recording");
        fs::remove_file(&partial).unwrap();
        let mut writer =
            MatroskaWriter::create(&path, &VideoConfig::default(), &[], &packet).unwrap();
        writer.packet(&packet).unwrap();
        assert_eq!(writer.finish().unwrap(), path);
        assert!(path.is_file());
        assert!(!partial.exists());
    }

    #[test]
    fn ebml_sizes_reserve_unknown_marker() {
        assert_eq!(vint(0), [0x80]);
        assert_eq!(vint(126), [0xfe]);
        assert_eq!(vint(127), [0x40, 0x7f]);
        assert_eq!(vint(16382), [0x7f, 0xfe]);
        assert_eq!(id(0x1a45dfa3), [0x1a, 0x45, 0xdf, 0xa3]);
    }
    #[test]
    fn parses_reduced_sequence_and_rejects_truncation() {
        // Main profile, still picture, reduced header, level 0.
        let private = av1_codec_private(&[0x12, 0, 0x0a, 2, 0x18, 0]).unwrap();
        assert_eq!(&private[..4], &[0x81, 0, 0x0c, 0]);
        assert!(av1_codec_private(&[0x0a, 100, 0]).is_err());
        assert!(av1_codec_private(&[0x0a, 0xff]).is_err());
    }
}
