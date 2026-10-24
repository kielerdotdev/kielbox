use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub type Id = u64;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: Id,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_stamp: Option<crate::SourceStamp>,
    pub duration_ns: u64,
    pub width: u32,
    pub height: u32,
    pub streams: Vec<MediaStream>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaStream {
    pub index: u32,
    pub kind: TrackKind,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub id: Id,
    pub name: String,
    pub kind: TrackKind,
    pub muted: bool,
    pub solo: bool,
    pub gain: f64,
    #[serde(default)]
    pub suppress_noise: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Content {
    Media {
        asset: Id,
        stream: u32,
    },
    Title {
        text: String,
        color: String,
        font_size: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Translation in fractions of the canvas, measured from its centre.
    pub x: f64,
    pub y: f64,
    pub scale_x: f64,
    pub scale_y: f64,
    pub rotation: f64,
    pub opacity: f64,
    /// Fraction of source dimensions removed from each edge.
    pub crop_left: f64,
    pub crop_top: f64,
    pub crop_right: f64,
    pub crop_bottom: f64,
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.,
            y: 0.,
            scale_x: 1.,
            scale_y: 1.,
            rotation: 0.,
            opacity: 1.,
            crop_left: 0.,
            crop_top: 0.,
            crop_right: 0.,
            crop_bottom: 0.,
        }
    }
}
impl Transform {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [
                self.x,
                self.y,
                self.scale_x,
                self.scale_y,
                self.rotation,
                self.opacity,
                self.crop_left,
                self.crop_top,
                self.crop_right,
                self.crop_bottom
            ]
            .iter()
            .all(|v| v.is_finite()),
            "transform contains a non-finite value"
        );
        ensure!(
            self.x.abs() <= 10.
                && self.y.abs() <= 10.
                && (0.01..=10.).contains(&self.scale_x)
                && (0.01..=10.).contains(&self.scale_y),
            "transform is out of range"
        );
        ensure!(
            self.rotation.abs() <= 3600. && (0.0..=1.).contains(&self.opacity),
            "invalid rotation or opacity"
        );
        ensure!(
            [
                self.crop_left,
                self.crop_top,
                self.crop_right,
                self.crop_bottom
            ]
            .iter()
            .all(|v| (0.0..1.).contains(v))
                && self.crop_left + self.crop_right < 0.99
                && self.crop_top + self.crop_bottom < 0.99,
            "invalid crop"
        );
        Ok(())
    }
    pub fn interpolate(self, other: Self, t: f64) -> Self {
        let lerp = |a: f64, b: f64| a + (b - a) * t.clamp(0., 1.);
        Self {
            x: lerp(self.x, other.x),
            y: lerp(self.y, other.y),
            scale_x: lerp(self.scale_x, other.scale_x),
            scale_y: lerp(self.scale_y, other.scale_y),
            rotation: lerp(self.rotation, other.rotation),
            opacity: lerp(self.opacity, other.opacity),
            crop_left: lerp(self.crop_left, other.crop_left),
            crop_top: lerp(self.crop_top, other.crop_top),
            crop_right: lerp(self.crop_right, other.crop_right),
            crop_bottom: lerp(self.crop_bottom, other.crop_bottom),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Keyframe {
    pub frame: u64,
    pub transform: Transform,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clip {
    pub id: Id,
    pub track: Id,
    pub group: Id,
    pub content: Content,
    pub start: u64,
    pub duration: u64,
    pub source_in_ns: u64,
    pub speed: f64,
    pub gain: f64,
    pub fade_in: u64,
    pub fade_out: u64,
    pub transform: Transform,
    pub keyframes: Vec<Keyframe>,
}
impl Clip {
    pub fn end(&self) -> u64 {
        self.start + self.duration
    }
    fn truncate_keys(&mut self, duration: u64) {
        // Preserve interpolation when its next key lies past the new out point.
        let boundary = duration - 1;
        let value = self.transform_at(self.start + boundary);
        let crosses = self.keyframes.iter().any(|k| k.frame >= duration);
        self.keyframes.retain(|k| k.frame < duration);
        if crosses && !self.keyframes.iter().any(|k| k.frame == boundary) {
            self.keyframes.push(Keyframe {
                frame: boundary,
                transform: value,
            });
        }
    }
    pub fn transform_at(&self, frame: u64) -> Transform {
        let local = frame.saturating_sub(self.start).min(self.duration);
        let mut previous_frame = 0;
        let mut previous = self.transform;
        for key in &self.keyframes {
            if local <= key.frame {
                let t = if key.frame == previous_frame {
                    1.
                } else {
                    (local - previous_frame) as f64 / (key.frame - previous_frame) as f64
                };
                return previous.interpolate(key.transform, t);
            }
            previous_frame = key.frame;
            previous = key.transform;
        }
        previous
    }
    pub fn opacity_at(&self, frame: u64) -> f64 {
        let local = frame.saturating_sub(self.start).min(self.duration);
        let fade_in = if self.fade_in == 0 {
            1.
        } else {
            local as f64 / self.fade_in as f64
        };
        let fade_out = if self.fade_out == 0 {
            1.
        } else {
            (self.duration - local) as f64 / self.fade_out as f64
        };
        self.transform_at(frame).opacity * fade_in.min(fade_out).clamp(0., 1.)
    }
}

/// Output framing is independent of the wide editing preview.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reframe {
    pub vertical: bool,
    pub center: f64,
}
impl Default for Reframe {
    fn default() -> Self {
        Self {
            vertical: false,
            center: 0.5,
        }
    }
}
impl Reframe {
    pub fn horizontal_window(&self, width: f64, height: f64) -> (f64, f64) {
        let fraction = (height * 9. / 16. / width.max(1.)).clamp(0., 1.);
        ((1. - fraction) * self.center, fraction)
    }
    fn apply(&self, t: &mut Transform, width: f64, height: f64) {
        let available_x = 1. - t.crop_left - t.crop_right;
        let available_y = 1. - t.crop_top - t.crop_bottom;
        let w = width * available_x;
        let h = height * available_y;
        if w / h > 9. / 16. {
            let (left, fraction) = self.horizontal_window(w, h);
            t.crop_left += available_x * left;
            t.crop_right += available_x * (1. - left - fraction);
        } else {
            let fraction = (w * 16. / 9. / h).clamp(0., 1.);
            t.crop_top += available_y * (1. - fraction) / 2.;
            t.crop_bottom += available_y * (1. - fraction) / 2.;
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    #[serde(default)]
    pub reframe: Reframe,
    pub assets: Vec<MediaAsset>,
    pub tracks: Vec<Track>,
    pub clips: Vec<Clip>,
    pub next_id: Id,
}
impl Default for Project {
    fn default() -> Self {
        Self {
            version: 1,
            name: "Untitled project".into(),
            width: 2560,
            height: 1440,
            fps: 60,
            reframe: Reframe::default(),
            assets: Vec::new(),
            tracks: Vec::new(),
            clips: Vec::new(),
            next_id: 1,
        }
    }
}
impl Project {
    /// Materialize framing only for export. The editor keeps its original source canvas.
    pub fn framed_export(&self) -> Self {
        let mut p = self.clone();
        if !self.reframe.vertical {
            return p;
        }
        p.width = ((f64::from(p.height) * 9. / 16. / 2.).round() as u32 * 2).max(2);
        for clip in &mut p.clips {
            let Content::Media { asset, .. } = clip.content else {
                continue;
            };
            if !p
                .tracks
                .iter()
                .any(|t| t.id == clip.track && t.kind == TrackKind::Video)
            {
                continue;
            }
            if let Some(a) = p.assets.iter().find(|a| a.id == asset) {
                self.reframe
                    .apply(&mut clip.transform, f64::from(a.width), f64::from(a.height));
                for key in &mut clip.keyframes {
                    self.reframe
                        .apply(&mut key.transform, f64::from(a.width), f64::from(a.height));
                }
            }
        }
        p.reframe.vertical = false;
        p
    }
    pub fn id(&mut self) -> Id {
        let result = self.next_id;
        self.next_id += 1;
        result
    }
    pub fn duration(&self) -> u64 {
        self.clips.iter().map(Clip::end).max().unwrap_or(0)
    }
    pub fn seconds(&self, frames: u64) -> f64 {
        frames as f64 / f64::from(self.fps)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.reframe.center.is_finite() && (0.0..=1.).contains(&self.reframe.center),
            "Invalid reframe center"
        );
        ensure!(self.version == 1, "unsupported project version");
        ensure!(
            (1..1_000_000_000_000).contains(&self.next_id),
            "invalid project ID counter"
        );
        ensure!(
            self.width > 0
                && self.height > 0
                && self.width.is_multiple_of(2)
                && self.height.is_multiple_of(2)
                && self.width <= 8192
                && self.height <= 8192,
            "invalid canvas"
        );
        ensure!((1..=240).contains(&self.fps), "invalid project FPS");
        ensure!(
            self.clips.len() <= 4096 && self.tracks.len() <= 128 && self.assets.len() <= 2048,
            "project exceeds supported size"
        );
        let mut ids = std::collections::HashSet::new();
        for id in self
            .assets
            .iter()
            .map(|a| a.id)
            .chain(self.tracks.iter().map(|t| t.id))
            .chain(self.clips.iter().map(|c| c.id))
        {
            ensure!(
                id > 0 && id < self.next_id && ids.insert(id),
                "duplicate/invalid project ID"
            );
        }
        for track in &self.tracks {
            ensure!(
                !track.suppress_noise || track.kind == TrackKind::Audio,
                "noise suppression is only supported on audio tracks"
            );
            ensure!(
                track.gain.is_finite() && (0.0..=4.).contains(&track.gain),
                "invalid track gain"
            );
        }
        for clip in &self.clips {
            let track = self
                .tracks
                .iter()
                .find(|t| t.id == clip.track)
                .context("clip references missing track")?;
            ensure!(
                clip.duration > 0
                    && clip
                        .start
                        .checked_add(clip.duration)
                        .is_some_and(|end| end <= u64::from(self.fps) * 86400),
                "invalid clip interval"
            );
            ensure!(
                clip.speed.is_finite() && (0.125..=8.).contains(&clip.speed),
                "invalid playback speed"
            );
            ensure!(
                clip.gain.is_finite() && (0.0..=4.).contains(&clip.gain),
                "invalid clip gain"
            );
            ensure!(
                clip.fade_in <= clip.duration && clip.fade_out <= clip.duration,
                "fade exceeds clip length"
            );
            clip.transform.validate()?;
            let mut last = None;
            for key in &clip.keyframes {
                ensure!(
                    key.frame < clip.duration && last.is_none_or(|f| key.frame > f),
                    "unordered/out-of-range keyframe"
                );
                key.transform.validate()?;
                last = Some(key.frame);
                ensure!(
                    [
                        key.transform.crop_left,
                        key.transform.crop_top,
                        key.transform.crop_right,
                        key.transform.crop_bottom
                    ] == [
                        clip.transform.crop_left,
                        clip.transform.crop_top,
                        clip.transform.crop_right,
                        clip.transform.crop_bottom
                    ],
                    "crop is a clip-level property"
                );
            }
            match &clip.content {
                Content::Media { asset, stream } => {
                    let asset = self
                        .assets
                        .iter()
                        .find(|a| a.id == *asset)
                        .context("missing media asset")?;
                    ensure!(
                        asset
                            .streams
                            .iter()
                            .any(|s| s.index == *stream && s.kind == track.kind),
                        "clip/stream kind mismatch"
                    );
                    let used = self.seconds(clip.duration) * clip.speed * 1e9;
                    ensure!(
                        clip.source_in_ns as f64 + used
                            <= asset.duration_ns as f64 + 1e9 / f64::from(self.fps),
                        "clip exceeds source duration"
                    );
                }
                Content::Title {
                    text,
                    color,
                    font_size,
                } => {
                    ensure!(
                        track.kind == TrackKind::Video
                            && text.len() <= 16_384
                            && *font_size > 0
                            && *font_size <= 512,
                        "invalid title"
                    );
                    ensure!(
                        color.len() == 6 && color.chars().all(|c| c.is_ascii_hexdigit()),
                        "invalid title colour"
                    );
                }
            }
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut temporary, self)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        temporary.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(
            fs::metadata(path)?.len() < 32 * 1024 * 1024,
            "project file is too large"
        );
        let mut project: Self = serde_json::from_slice(&fs::read(path)?)?;
        for asset in &mut project.assets {
            if asset.path.is_relative() {
                asset.path = path.parent().unwrap_or(Path::new(".")).join(&asset.path);
            }
        }
        project.validate()?;
        Ok(project)
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Reframe {
        vertical: bool,
        center: f64,
    },
    Sequence(crate::QuickEdit),
    NewProject,
    Batch(Vec<Edit>),
    AddTrack {
        kind: TrackKind,
    },
    MoveToTrack {
        id: Id,
        track: Id,
    },
    ClearKeyframes {
        id: Id,
    },
    Import {
        asset: MediaAsset,
        at: u64,
    },
    Split {
        id: Id,
        at: u64,
    },
    TrimLeft {
        id: Id,
        to: u64,
    },
    TrimRight {
        id: Id,
        to: u64,
    },
    Move {
        id: Id,
        to: u64,
    },
    Delete {
        id: Id,
        ripple: bool,
    },
    Duplicate {
        id: Id,
    },
    Speed {
        id: Id,
        value: f64,
    },
    Gain {
        id: Id,
        value: f64,
    },
    Transform {
        id: Id,
        value: Transform,
    },
    Fade {
        id: Id,
        fade_in: u64,
        fade_out: u64,
    },
    Keyframe {
        id: Id,
        at: u64,
        value: Transform,
    },
    AddTitle {
        text: String,
        at: u64,
        duration: u64,
    },
    SetTitle {
        id: Id,
        text: String,
    },
    TitleStyle {
        id: Id,
        text: String,
        color: String,
        font_size: u32,
    },
    MuteTrack {
        id: Id,
    },
    SuppressNoise {
        id: Id,
    },
    SoloTrack {
        id: Id,
    },
    TrackGain {
        id: Id,
        value: f64,
    },
}

pub struct Editor {
    pub project: Project,
    pub linked_edits: bool,
    undo: Vec<Project>,
    redo: Vec<Project>,
}
impl Default for Editor {
    fn default() -> Self {
        Self::new(Project::default())
    }
}
impl Editor {
    /// Worker-only validation covers history as well as the active project. Adopt
    /// legacy stamps atomically, so Undo cannot reintroduce an unverified source.
    pub fn validate_sources_cancellable(&mut self, cancelled: impl Fn() -> bool) -> Result<()> {
        let assets = std::iter::once(&self.project).chain(self.undo.iter()).chain(self.redo.iter())
            .flat_map(|project| project.assets.iter().cloned()).collect();
        let combined = Project { assets, ..Project::default() };
        let stamps = crate::validate_sources_cancellable(&combined, cancelled)?;
        let mut stamps = stamps.into_iter();
        for project in std::iter::once(&mut self.project).chain(self.undo.iter_mut()).chain(self.redo.iter_mut()) {
            for asset in &mut project.assets {
                asset.source_stamp = stamps.next();
            }
        }
        Ok(())
    }
    pub fn new(project: Project) -> Self {
        Self {
            project,
            linked_edits: true,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
    pub fn undo(&mut self) -> bool {
        if let Some(previous) = self.undo.pop() {
            self.redo
                .push(std::mem::replace(&mut self.project, previous));
            true
        } else {
            false
        }
    }
    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.project, next));
            true
        } else {
            false
        }
    }
    pub fn execute(&mut self, edit: Edit) -> Result<()> {
        let before = self.project.clone();
        if let Err(error) = self.apply(edit).and_then(|_| self.project.validate()) {
            self.project = before;
            return Err(error);
        }
        if self.undo.len() == 128 {
            self.undo.remove(0);
        }
        self.undo.push(before);
        self.redo.clear();
        Ok(())
    }
    fn related(&self, id: Id) -> Result<Vec<usize>> {
        let selected = self
            .project
            .clips
            .iter()
            .find(|c| c.id == id)
            .context("no clip selected")?;
        Ok(self
            .project
            .clips
            .iter()
            .enumerate()
            .filter(|(_, c)| c.id == id || (self.linked_edits && c.group == selected.group))
            .map(|(i, _)| i)
            .collect())
    }
    fn clip(&mut self, id: Id) -> Result<&mut Clip> {
        self.project
            .clips
            .iter_mut()
            .find(|c| c.id == id)
            .context("clip not found")
    }
    fn apply(&mut self, edit: Edit) -> Result<()> {
        let fps = f64::from(self.project.fps);
        match edit {
            Edit::Reframe { vertical, center } => {
                self.project.reframe = Reframe { vertical, center }
            }
            Edit::Sequence(edit) => crate::quick::apply(&mut self.project, edit)?,
            Edit::NewProject => self.project = Project::default(),
            Edit::Batch(edits) => {
                ensure!(edits.len() <= 64, "edit batch too large");
                for edit in edits {
                    ensure!(
                        !matches!(edit, Edit::Batch(_)),
                        "nested batches are unsupported"
                    );
                    self.apply(edit)?;
                }
            }
            Edit::AddTrack { kind } => {
                let id = self.project.id();
                self.project.tracks.push(Track {
                    id,
                    name: format!(
                        "{} {}",
                        if kind == TrackKind::Video {
                            "Video"
                        } else {
                            "Audio"
                        },
                        id
                    ),
                    kind,
                    muted: false,
                    solo: false,
                    suppress_noise: false,
                    gain: 1.,
                });
            }
            Edit::MoveToTrack { id, track } => {
                self.clip(id)?.track = track;
            }
            Edit::ClearKeyframes { id } => self.clip(id)?.keyframes.clear(),
            Edit::Import { mut asset, at } => {
                ensure!(asset.duration_ns > 0, "media has no known duration");
                asset.id = self.project.id();
                let group = self.project.id();
                for stream in &asset.streams {
                    let track = self
                        .project
                        .tracks
                        .iter()
                        .find(|t| {
                            t.kind == stream.kind
                                && (stream.kind == TrackKind::Video || t.name == stream.name)
                        })
                        .map(|t| t.id);
                    let track = track.unwrap_or_else(|| {
                        let id = self.project.id();
                        self.project.tracks.push(Track {
                            id,
                            name: stream.name.clone(),
                            kind: stream.kind,
                            muted: false,
                            solo: false,
                            suppress_noise: false,
                            gain: 1.,
                        });
                        id
                    });
                    let id = self.project.id();
                    self.project.clips.push(Clip {
                        id,
                        track,
                        group,
                        content: Content::Media {
                            asset: asset.id,
                            stream: stream.index,
                        },
                        start: at,
                        duration: (asset.duration_ns as f64 * fps / 1e9).floor().max(1.) as u64,
                        source_in_ns: 0,
                        speed: 1.,
                        gain: 1.,
                        fade_in: 0,
                        fade_out: 0,
                        transform: Default::default(),
                        keyframes: Vec::new(),
                    });
                }
                self.project.assets.push(asset);
            }
            Edit::Split { id, at } => {
                let related = self.related(id)?;
                let group = self.project.id();
                ensure!(
                    related.iter().all(|i| {
                        let c = &self.project.clips[*i];
                        at > c.start && at < c.end()
                    }),
                    "split must lie inside the linked clips"
                );
                for i in related {
                    let mut right = self.project.clips[i].clone();
                    right.id = self.project.id();
                    right.group = group;
                    let split = at - right.start;
                    let value = right.transform_at(at);
                    right.start = at;
                    right.duration -= split;
                    right.source_in_ns += (split as f64 / fps * right.speed * 1e9).round() as u64;
                    right.transform = value;
                    right.keyframes = right
                        .keyframes
                        .into_iter()
                        .filter(|k| k.frame > split)
                        .map(|mut k| {
                            k.frame -= split;
                            k
                        })
                        .collect();
                    right.fade_in = 0;
                    right.fade_out = right.fade_out.min(right.duration);
                    self.project.clips[i].truncate_keys(split);
                    self.project.clips[i].duration = split;
                    self.project.clips[i].fade_out = 0;
                    self.project.clips[i].fade_in = self.project.clips[i].fade_in.min(split);
                    self.project.clips[i].keyframes.retain(|k| k.frame < split);
                    self.project.clips.push(right);
                }
            }
            Edit::TrimLeft { id, to } => {
                for i in self.related(id)? {
                    let clip = &mut self.project.clips[i];
                    ensure!(to < clip.end(), "invalid trim-in");
                    if to < clip.start {
                        let restored = clip.start - to;
                        if matches!(clip.content, Content::Media { .. }) {
                            let rewind = (restored as f64 / fps * clip.speed * 1e9).round() as u64;
                            clip.source_in_ns = clip
                                .source_in_ns
                                .checked_sub(rewind)
                                .context("trim precedes the source media")?;
                        }
                        for key in &mut clip.keyframes {
                            key.frame += restored;
                        }
                        // Only animated clips need an anchor at the old edge.
                        // Adding one to static media would turn a plain trim
                        // restore into animation and disable native preview.
                        if !clip.keyframes.is_empty() && !clip.keyframes.iter().any(|k| k.frame == restored) {
                            clip.keyframes.insert(
                                0,
                                Keyframe {
                                    frame: restored,
                                    transform: clip.transform,
                                },
                            );
                        }
                        clip.start = to;
                        clip.duration += restored;
                        continue;
                    }
                    let removed = to - clip.start;
                    let transform = clip.transform_at(to);
                    clip.source_in_ns += (removed as f64 / fps * clip.speed * 1e9).round() as u64;
                    clip.start = to;
                    clip.duration -= removed;
                    clip.transform = transform;
                    clip.keyframes = clip
                        .keyframes
                        .iter()
                        .filter(|k| k.frame > removed)
                        .cloned()
                        .map(|mut k| {
                            k.frame -= removed;
                            k
                        })
                        .collect();
                    clip.fade_in = clip.fade_in.saturating_sub(removed);
                    clip.fade_out = clip.fade_out.min(clip.duration);
                }
            }
            Edit::TrimRight { id, to } => {
                for i in self.related(id)? {
                    let clip = &mut self.project.clips[i];
                    ensure!(to > clip.start, "invalid trim-out");
                    let duration = to - clip.start;
                    clip.truncate_keys(duration);
                    clip.duration = duration;
                    clip.fade_in = clip.fade_in.min(clip.duration);
                    clip.fade_out = clip.fade_out.min(clip.duration);
                }
            }
            Edit::Move { id, to } => {
                let start = self.clip(id)?.start;
                let delta = i128::from(to) - i128::from(start);
                for i in self.related(id)? {
                    let c = &mut self.project.clips[i];
                    ensure!(i128::from(c.start) + delta >= 0, "move precedes timeline");
                    c.start = (i128::from(c.start) + delta) as u64;
                }
            }
            Edit::Delete { id, ripple } => {
                let clip = self.clip(id)?.clone();
                let remove = self.related(id)?;
                self.project.clips = self
                    .project
                    .clips
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !remove.contains(i))
                    .map(|(_, c)| c.clone())
                    .collect();
                if ripple {
                    for c in &mut self.project.clips {
                        if c.start >= clip.end() {
                            c.start -= clip.duration;
                        }
                    }
                }
            }
            Edit::Duplicate { id } => {
                let indices = self.related(id)?;
                let group = self.project.id();
                for i in indices {
                    let mut c = self.project.clips[i].clone();
                    c.id = self.project.id();
                    c.group = group;
                    c.start += c.duration;
                    self.project.clips.push(c);
                }
            }
            Edit::Speed { id, value } => {
                for i in self.related(id)? {
                    ensure!(
                        value.is_finite() && (0.125..=8.).contains(&value),
                        "speed must be between 0.125 and 8"
                    );
                    let c = &mut self.project.clips[i];
                    let ratio = c.speed / value;
                    c.duration = (c.duration as f64 * ratio).round().max(1.) as u64;
                    c.speed = value;
                    c.fade_in = (c.fade_in as f64 * ratio).round() as u64;
                    c.fade_out = (c.fade_out as f64 * ratio).round() as u64;
                    for key in &mut c.keyframes {
                        key.frame = (key.frame as f64 * ratio).round() as u64;
                    }
                    c.keyframes.retain(|k| k.frame < c.duration);
                    c.keyframes.dedup_by_key(|k| k.frame);
                }
            }
            Edit::Gain { id, value } => self.clip(id)?.gain = value,
            Edit::Transform { id, value } => {
                let clip = self.clip(id)?;
                clip.transform = value;
                for key in &mut clip.keyframes {
                    key.transform.crop_left = value.crop_left;
                    key.transform.crop_top = value.crop_top;
                    key.transform.crop_right = value.crop_right;
                    key.transform.crop_bottom = value.crop_bottom;
                }
            }
            Edit::Fade {
                id,
                fade_in,
                fade_out,
            } => {
                for i in self.related(id)? {
                    self.project.clips[i].fade_in = fade_in;
                    self.project.clips[i].fade_out = fade_out;
                }
            }
            Edit::Keyframe { id, at, value } => {
                let clip = self.clip(id)?;
                ensure!(
                    at >= clip.start && at < clip.end(),
                    "keyframe is outside clip"
                );
                let at = at - clip.start;
                clip.keyframes.retain(|k| k.frame != at);
                clip.keyframes.push(Keyframe {
                    frame: at,
                    transform: value,
                });
                clip.keyframes.sort_by_key(|k| k.frame);
            }
            Edit::AddTitle { text, at, duration } => {
                let track = self.project.id();
                self.project.tracks.push(Track {
                    id: track,
                    name: "Titles".into(),
                    kind: TrackKind::Video,
                    muted: false,
                    solo: false,
                    suppress_noise: false,
                    gain: 1.,
                });
                let id = self.project.id();
                self.project.clips.push(Clip {
                    id,
                    track,
                    group: id,
                    content: Content::Title {
                        text,
                        color: "ffffff".into(),
                        font_size: 48,
                    },
                    start: at,
                    duration,
                    source_in_ns: 0,
                    speed: 1.,
                    gain: 1.,
                    fade_in: 0,
                    fade_out: 0,
                    transform: Default::default(),
                    keyframes: Vec::new(),
                });
            }
            Edit::SetTitle { id, text } => {
                let Content::Title { text: target, .. } = &mut self.clip(id)?.content else {
                    anyhow::bail!("selected clip is not a title")
                };
                *target = text;
            }
            Edit::TitleStyle {
                id,
                text,
                color,
                font_size,
            } => {
                let Content::Title {
                    text: target,
                    color: target_color,
                    font_size: target_size,
                } = &mut self.clip(id)?.content
                else {
                    anyhow::bail!("selected clip is not a title")
                };
                *target = text;
                *target_color = color;
                *target_size = font_size;
            }
            Edit::MuteTrack { id } => {
                let t = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|t| t.id == id)
                    .context("track missing")?;
                t.muted = !t.muted;
            }
            Edit::SuppressNoise { id } => {
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|t| t.id == id)
                    .context("track missing")?;
                ensure!(
                    track.kind == TrackKind::Audio,
                    "select an audio track for noise suppression"
                );
                track.suppress_noise = !track.suppress_noise;
            }
            Edit::SoloTrack { id } => {
                let t = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|t| t.id == id)
                    .context("track missing")?;
                t.solo = !t.solo;
            }
            Edit::TrackGain { id, value } => {
                self.project
                    .tracks
                    .iter_mut()
                    .find(|t| t.id == id)
                    .context("track missing")?
                    .gain = value;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn editor() -> Editor {
        let mut e = Editor::default();
        e.execute(Edit::Import {
            at: 0,
            asset: MediaAsset {
                id: 0,
                source_stamp: None,
                path: "source.mkv".into(),
                duration_ns: 10_000_000_000,
                width: 2560,
                height: 1440,
                streams: vec![
                    MediaStream {
                        index: 0,
                        kind: TrackKind::Video,
                        name: "Video".into(),
                    },
                    MediaStream {
                        index: 1,
                        kind: TrackKind::Audio,
                        name: "System".into(),
                    },
                    MediaStream {
                        index: 2,
                        kind: TrackKind::Audio,
                        name: "Mic".into(),
                    },
                ],
            },
        })
        .unwrap();
        e
    }
    #[test]
    fn reframe_is_undoable_and_export_keeps_audio_and_original_sources() {
        let mut e = editor();
        let before = serde_json::to_value(&e.project).unwrap();
        e.execute(Edit::Reframe {
            vertical: true,
            center: 0.,
        })
        .unwrap();
        let exported = e.project.framed_export();
        exported.validate().unwrap();
        assert_eq!((exported.width, exported.height), (810, 1440));
        assert_eq!(exported.duration(), e.project.duration());
        for (source, clip) in e.project.clips.iter().zip(&exported.clips) {
            assert_eq!(source.source_in_ns, clip.source_in_ns);
            assert_eq!(source.duration, clip.duration);
            if e.project
                .tracks
                .iter()
                .any(|t| t.id == clip.track && t.kind == TrackKind::Audio)
            {
                assert_eq!(
                    serde_json::to_value(source).unwrap(),
                    serde_json::to_value(clip).unwrap()
                );
            } else {
                assert_eq!(clip.transform.crop_left, 0.);
                assert!((clip.transform.crop_right - 0.68359375).abs() < 1e-9);
                assert_eq!(source.transform, Transform::default());
            }
        }
        assert_eq!((e.project.width, e.project.height), (2560, 1440));
        assert!(e.undo());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        assert!(e.redo());
        assert!(e.project.reframe.vertical);
        let saved = serde_json::to_vec(&e.project).unwrap();
        let restored: Project = serde_json::from_slice(&saved).unwrap();
        assert_eq!(
            restored.reframe,
            Reframe {
                vertical: true,
                center: 0.
            }
        );
        assert!(e
            .execute(Edit::Reframe {
                vertical: true,
                center: 1.1
            })
            .is_err());
        assert_eq!(e.project.reframe.center, 0.);
    }
    #[test]
    fn reframe_defaults_for_old_projects_and_clamps_to_source_edges() {
        let e = editor();
        let mut json = serde_json::to_value(&e.project).unwrap();
        json.as_object_mut().unwrap().remove("reframe");
        let old: Project = serde_json::from_value(json).unwrap();
        assert_eq!(old.reframe, Reframe::default());
        let mut p = old;
        p.reframe = Reframe {
            vertical: true,
            center: 1.,
        };
        let video = p.framed_export().clips[0].transform;
        assert!((video.crop_left - 0.68359375).abs() < 1e-9);
        assert!(video.crop_right.abs() < 1e-9);
        let mut portrait = Transform::default();
        p.reframe.apply(&mut portrait, 720., 1600.);
        portrait.validate().unwrap();
        assert!((portrait.crop_top - 0.1).abs() < 1e-9);
        assert!((portrait.crop_bottom - 0.1).abs() < 1e-9);
    }
    #[test]
    fn linked_split_preserves_sources_and_undo_is_transactional() {
        let mut e = editor();
        let id = e.project.clips[0].id;
        e.execute(Edit::Split { id, at: 240 }).unwrap();
        assert_eq!(e.project.clips.len(), 6);
        for c in &e.project.clips[3..] {
            assert_eq!(c.start, 240);
            assert_eq!(c.source_in_ns, 4_000_000_000);
            assert_eq!(c.duration, 360);
        }
        assert!(e.execute(Edit::TrimLeft { id, to: 9999 }).is_err());
        assert_eq!(e.project.clips.len(), 6);
        assert!(e.undo());
        assert_eq!(e.project.clips.len(), 3);
        assert!(e.redo());
        assert_eq!(e.project.clips.len(), 6);
    }
    #[test]
    fn speed_and_ripple_keep_audio_linked() {
        let mut e = editor();
        let id = e.project.clips[0].id;
        e.execute(Edit::Speed { id, value: 2. }).unwrap();
        assert!(e.project.clips.iter().all(|c| c.duration == 300));
        e.execute(Edit::Duplicate { id }).unwrap();
        e.execute(Edit::Delete { id, ripple: true }).unwrap();
        assert_eq!(e.project.clips.len(), 3);
        assert!(e.project.clips.iter().all(|c| c.start == 0));
    }
    #[test]
    fn trim_interpolates_keyframe_boundary() {
        let mut e = editor();
        e.linked_edits = false;
        let id = e.project.clips[0].id;
        e.execute(Edit::Keyframe {
            id,
            at: 120,
            value: Transform {
                x: 1.,
                ..Default::default()
            },
        })
        .unwrap();
        e.execute(Edit::TrimLeft { id, to: 60 }).unwrap();
        assert_eq!(e.project.clips[0].transform.x, 0.5);
        assert_eq!(e.project.clips[0].keyframes[0].frame, 60);
    }
    #[test]
    fn project_save_replaces_atomically_and_round_trips() {
        let mut e = editor();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project.rcr.json");
        e.project.save(&path).unwrap();
        e.project.name = "Second revision".into();
        e.project.save(&path).unwrap();
        let loaded = Project::load(&path).unwrap();
        assert_eq!(loaded.name, "Second revision");
        assert_eq!(loaded.clips.len(), 3);
    }
    #[test]
    fn cut_preserves_animation_on_both_sides() {
        let mut e = editor();
        e.linked_edits = false;
        let id = e.project.clips[0].id;
        e.execute(Edit::Keyframe {
            id,
            at: 180,
            value: Transform {
                x: 1.,
                ..Default::default()
            },
        })
        .unwrap();
        let original = e.project.clips[0].clone();
        e.execute(Edit::Split { id, at: 90 }).unwrap();
        let left = &e.project.clips[0];
        let right = e.project.clips.last().unwrap();
        for frame in [0, 30, 60, 89] {
            assert!((left.transform_at(frame).x - original.transform_at(frame).x).abs() < 1e-9);
        }
        for frame in [90, 120, 180, 240] {
            assert!((right.transform_at(frame).x - original.transform_at(frame).x).abs() < 1e-9);
        }
    }
    #[test]
    fn trim_can_restore_source_and_rejects_extending_past_it() {
        let mut e = editor();
        let id = e.project.clips[0].id;
        e.execute(Edit::TrimLeft { id, to: 120 }).unwrap();
        e.execute(Edit::TrimLeft { id, to: 60 }).unwrap();
        assert!(e
            .project
            .clips
            .iter()
            .all(|c| c.start == 60 && c.source_in_ns == 1_000_000_000));
        e.execute(Edit::TrimRight { id, to: 300 }).unwrap();
        e.execute(Edit::TrimRight { id, to: 600 }).unwrap();
        let before = serde_json::to_string(&e.project).unwrap();
        assert!(e.execute(Edit::TrimRight { id, to: 660 }).is_err());
        assert_eq!(serde_json::to_string(&e.project).unwrap(), before);
    }
    #[test]
    fn static_trim_restore_round_trip_preserves_project_without_animation() {
        for transform in [Transform::default(), Transform { opacity: 0.7, x: 0.2, ..Default::default() }] {
            let mut editor = editor();
            let id = editor.project.clips[0].id;
            editor.execute(Edit::Transform { id, value: transform }).unwrap();
            let before = serde_json::to_value(&editor.project).unwrap();
            for to in [120, 60, 0, 180, 0] {
                editor.execute(Edit::TrimLeft { id, to }).unwrap();
                assert!(editor.project.clips.iter().all(|clip| clip.keyframes.is_empty()),
                    "Static source gained redundant animation after trim restoration");
            }
            assert_eq!(serde_json::to_value(&editor.project).unwrap(), before);
            assert!(editor.undo());
            assert_eq!(editor.project.clips[0].start, 180);
            assert!(editor.redo());
            assert_eq!(serde_json::to_value(&editor.project).unwrap(), before);
        }
    }
    #[test]
    fn animated_trim_restore_keeps_boundary_anchor_and_retained_motion() {
        let mut editor = editor();
        editor.linked_edits = false;
        let id = editor.project.clips[0].id;
        editor.execute(Edit::Keyframe { id, at: 120,
            value: Transform { x: 1., ..Default::default() } }).unwrap();
        let before = editor.project.clips[0].clone();
        editor.execute(Edit::TrimLeft { id, to: 60 }).unwrap();
        editor.execute(Edit::TrimLeft { id, to: 0 }).unwrap();
        let restored = &editor.project.clips[0];
        let anchor = restored.keyframes.iter().find(|key| key.frame == 60).unwrap();
        assert_eq!(anchor.transform.x, before.transform_at(60).x);
        for at in [60, 90, 120, 240] {
            assert!((restored.transform_at(at).x - before.transform_at(at).x).abs() < 1e-9);
        }
    }
    #[test]
    fn failed_batch_restores_project_and_keeps_redo_history() {
        let mut e = editor();
        let id = e.project.clips[0].id;
        e.execute(Edit::Duplicate { id }).unwrap();
        assert!(e.undo());
        let before = serde_json::to_string(&e.project).unwrap();
        assert!(e
            .execute(Edit::Batch(vec![
                Edit::Move { id, to: 20 },
                Edit::Gain {
                    id,
                    value: f64::NAN
                }
            ]))
            .is_err());
        assert_eq!(serde_json::to_string(&e.project).unwrap(), before);
        assert!(e.redo());
        assert_eq!(e.project.clips.len(), 6);
    }
    #[test]
    fn new_project_is_undoable_and_restores_valid_source_ids() {
        let mut e = editor();
        let before = serde_json::to_string(&e.project).unwrap();
        e.execute(Edit::NewProject).unwrap();
        assert!(e.project.clips.is_empty());
        assert!(e.project.assets.is_empty());
        assert!(e.undo());
        assert_eq!(serde_json::to_string(&e.project).unwrap(), before);
        e.project.validate().unwrap();
        assert!(e.redo());
        assert_eq!(e.project.next_id, 1);
    }
    #[test]
    fn noise_suppression_is_opt_in_persisted_and_audio_only() {
        let mut e = editor();
        assert!(e.project.tracks.iter().all(|t| !t.suppress_noise));
        let audio = e
            .project
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Audio)
            .unwrap()
            .id;
        let video = e
            .project
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Video)
            .unwrap()
            .id;
        e.execute(Edit::SuppressNoise { id: audio }).unwrap();
        assert!(e.execute(Edit::SuppressNoise { id: video }).is_err());
        let saved = serde_json::to_string(&e.project).unwrap();
        let loaded: Project = serde_json::from_str(&saved).unwrap();
        assert!(
            loaded
                .tracks
                .iter()
                .find(|t| t.id == audio)
                .unwrap()
                .suppress_noise
        );
        let legacy = saved
            .replace("\"suppress_noise\":true,", "")
            .replace("\"suppress_noise\":false,", "")
            .replace(",\"suppress_noise\":true", "")
            .replace(",\"suppress_noise\":false", "");
        let loaded: Project = serde_json::from_str(&legacy).unwrap();
        assert!(loaded.tracks.iter().all(|t| !t.suppress_noise));
        assert!(e.undo());
        assert!(e.project.tracks.iter().all(|t| !t.suppress_noise));
    }
}
