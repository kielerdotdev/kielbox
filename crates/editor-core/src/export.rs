//! Export graph generation and supervised FFmpeg jobs.
#[cfg(test)]
#[path = "export_fidelity_tests.rs"]
mod fidelity_tests;
use crate::{media_command, Clip, Content, Project, TrackKind, Transform};
use anyhow::{ensure, Context, Result};
use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioExport {
    Mix,
    Separate,
    MixAndSeparate,
}

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub output: PathBuf,
    pub start: u64,
    pub end: Option<u64>,
    pub audio: AudioExport,
    pub bitrate: u32,
    pub video_codec: ExportVideoCodec,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportVideoCodec {
    #[default]
    Av1,
    H264,
}
impl ExportVideoCodec {
    fn encoder(self) -> &'static str {
        match self { Self::Av1 => "av1_nvenc", Self::H264 => "h264_nvenc" }
    }
}

pub struct RenderPlan {
    pub input_args: Vec<OsString>,
    pub filter: String,
    pub audio: Vec<(String, String)>,
    pub seconds: f64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    directory: tempfile::TempDir,
}

fn path_for_tool(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix("\\\\?\\UNC\\") {
            return Ok(PathBuf::from(format!("\\\\{rest}")));
        }
        if let Some(rest) = text.strip_prefix("\\\\?\\") {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(path)
}
fn number(value: f64) -> String {
    format!("{value:.9}")
}

fn expression(clip: &Clip, field: fn(Transform) -> f64, frame: &str) -> String {
    let mut points = vec![(0, field(clip.transform))];
    for key in &clip.keyframes {
        if key.frame == 0 {
            points[0].1 = field(key.transform);
        } else {
            points.push((key.frame, field(key.transform)));
        }
    }
    let mut result = number(points.last().unwrap().1);
    for pair in points.windows(2).rev() {
        let (a, av) = pair[0];
        let (b, bv) = pair[1];
        result = format!(
            "if(lte({frame},{b}),{}+({})*clip((({frame})-{a})/{},0,1),{result})",
            number(av),
            number(bv - av),
            b - a
        );
    }
    result
}
fn tempo(mut speed: f64) -> String {
    // Even at 1x, WSOLA can change the last samples of a finite clip. A normal
    // recording must retain its PCM instead of passing through time stretching.
    if speed == 1. { return "anull".into(); }
    let mut parts = Vec::new();
    while speed > 2. {
        parts.push("atempo=2".to_owned());
        speed /= 2.;
    }
    while speed < 0.5 {
        parts.push("atempo=0.5".to_owned());
        speed /= 0.5;
    }
    parts.push(format!("atempo={}", number(speed)));
    parts.join(",")
}

impl RenderPlan {
    pub fn build(
        project: &Project,
        start: u64,
        end: u64,
        width: u32,
        height: u32,
        fps: u32,
        audio_mode: Option<AudioExport>,
    ) -> Result<Self> {
        Self::build_mode(
            project,
            start,
            end,
            (width, height, fps),
            audio_mode,
            true,
            &HashMap::new(),
        )
    }
    fn build_mode(
        project: &Project,
        start: u64,
        end: u64,
        dimensions: (u32, u32, u32),
        audio_mode: Option<AudioExport>,
        include_video: bool,
        audio_overrides: &HashMap<u64, PathBuf>,
    ) -> Result<Self> {
        let (width, height, fps) = dimensions;
        project.validate()?;
        ensure!(
            start < end && end <= project.duration(),
            "invalid render range"
        );
        ensure!(
            width > 0
                && height > 0
                && width.is_multiple_of(2)
                && height.is_multiple_of(2)
                && fps > 0,
            "invalid render dimensions"
        );
        let directory = tempfile::tempdir()?;
        let seconds = project.seconds(end - start);
        let time = |frames: u64| project.seconds(frames);
        let mut args = Vec::<OsString>::new();
        let mut inputs = 0;
        let mut graph = Vec::new();
        let mut audio_outputs = Vec::new();
        graph.push(format!(
            "color=c=0x0b0e14:s={width}x{height}:r={fps}:d={},format=rgba[bg0]",
            number(seconds)
        ));
        let mut layer = 0;
        let solo_video = project
            .tracks
            .iter()
            .any(|t| t.kind == TrackKind::Video && t.solo);
        let solo_audio = project
            .tracks
            .iter()
            .any(|t| t.kind == TrackKind::Audio && t.solo);
        let mut add_input =
            |clip: &Clip, asset_id: u64, stream: u32| -> Result<(usize, f64, f64, u32)> {
                let asset = project
                    .assets
                    .iter()
                    .find(|a| a.id == asset_id)
                    .context("missing source asset")?;
                let first = clip.start.max(start);
                let last = clip.end().min(end);
                let skip = time(first - clip.start);
                let read_duration = time(last - first) * clip.speed;
                let source = clip.source_in_ns as f64 / 1e9 + skip * clip.speed;
                let prepared = audio_overrides.get(&clip.id);
                if prepared.is_some() {
                    args.extend([
                        "-f".into(),
                        "f32le".into(),
                        "-ar".into(),
                        "48000".into(),
                        "-ac".into(),
                        "2".into(),
                    ]);
                }
                args.extend([
                    "-threads".into(),
                    "2".into(),
                    "-ss".into(),
                    number(if prepared.is_some() { 0. } else { source }).into(),
                    "-t".into(),
                    number(read_duration).into(),
                    "-i".into(),
                    path_for_tool(prepared.unwrap_or(&asset.path))?.into_os_string(),
                ]);
                let input = inputs;
                inputs += 1;
                Ok((
                    input,
                    skip,
                    time(last - first),
                    if prepared.is_some() { 0 } else { stream },
                ))
            };
        let mut scratch_pixels = 0f64;
        for track in project.tracks.iter().filter(|t| {
            include_video && t.kind == TrackKind::Video && !t.muted && (!solo_video || t.solo)
        }) {
            let mut clips = project
                .clips
                .iter()
                .filter(|c| c.track == track.id && c.start < end && c.end() > start)
                .collect::<Vec<_>>();
            clips.sort_by_key(|c| (c.start, c.id));
            for clip in clips {
                let relative_start = time(clip.start) - time(start);
                let relative_end = time(clip.end()) - time(start);
                let local_frame = format!("t*{}", project.fps);
                let global_frame = format!("(t-({}))*{}", number(relative_start), project.fps);
                let sx = expression(clip, |t| t.scale_x, &local_frame);
                let sy = expression(clip, |t| t.scale_y, &local_frame);
                let opacity = expression(clip, |t| t.opacity, &format!("T*{}", project.fps));
                let rotation = expression(clip, |t| t.rotation, &local_frame);
                let transform = clip.transform;
                let mut filters = Vec::new();
                let (source_width, source_height) = match &clip.content {
                    Content::Media { asset, stream } => {
                        let (input, skip, duration, stream) = add_input(clip, *asset, *stream)?;
                        let media = project.assets.iter().find(|a| a.id == *asset).unwrap();
                        ensure!(
                            media.width > 0 && media.height > 0,
                            "video dimensions missing"
                        );
                        filters.push(format!("[{input}:{stream}]trim=duration={},setpts=(PTS-STARTPTS)/{}+{}/TB,fps={fps}",number(duration*clip.speed),number(clip.speed),number(skip)));
                        filters.push(format!(
                            "crop=iw*{}:ih*{}:iw*{}:ih*{}",
                            number(1. - transform.crop_left - transform.crop_right),
                            number(1. - transform.crop_top - transform.crop_bottom),
                            number(transform.crop_left),
                            number(transform.crop_top)
                        ));
                        (
                            f64::from(media.width)
                                * (1. - transform.crop_left - transform.crop_right),
                            f64::from(media.height)
                                * (1. - transform.crop_top - transform.crop_bottom),
                        )
                    }
                    Content::Title {
                        text,
                        color,
                        font_size,
                    } => {
                        let title = format!("title-{}.txt", clip.id);
                        fs::write(directory.path().join(&title), text)?;
                        let windows_font = PathBuf::from(
                            std::env::var_os("SystemRoot").unwrap_or_else(|| "C:/Windows".into()),
                        )
                        .join("Fonts/segoeui.ttf");
                        let font = if windows_font.exists() {
                            if !directory.path().join("font.ttf").exists() {
                                fs::copy(windows_font, directory.path().join("font.ttf"))?;
                            }
                            "fontfile=font.ttf".to_owned()
                        } else {
                            "font=Sans".to_owned()
                        };
                        let first = clip.start.max(start);
                        let skip = time(first - clip.start);
                        let duration = time(clip.end().min(end) - first);
                        let scaled_font = (f64::from(*font_size) * f64::from(width)
                            / f64::from(project.width))
                        .max(8.);
                        filters.push(format!("color=c=black@0:s={width}x{height}:r={fps}:d={},format=rgba,drawtext={font}:textfile={title}:expansion=none:fontcolor=0x{color}:fontsize={}:x=(w-text_w)/2:y=(h-text_h)/2,setpts=PTS+{}/TB",number(duration),number(scaled_font),number(skip)));
                        (f64::from(width), f64::from(height))
                    }
                };
                let fit = (f64::from(width) / source_width).min(f64::from(height) / source_height);
                let sw = source_width * fit;
                let sh = source_height * fit;
                let max_x = clip
                    .keyframes
                    .iter()
                    .map(|k| k.transform.scale_x)
                    .fold(transform.scale_x, f64::max);
                let max_y = clip
                    .keyframes
                    .iter()
                    .map(|k| k.transform.scale_y)
                    .fold(transform.scale_y, f64::max);
                scratch_pixels += (sw * max_x) * (sh * max_y);
                ensure!(
                    scratch_pixels <= 80_000_000.,
                    "composition exceeds the current render memory budget; reduce layers or scale"
                );
                filters.push(format!("scale=w='max(2,2*trunc({}*({sx})/2))':h='max(2,2*trunc({}*({sy})/2))':eval=frame:flags=bilinear,format=rgba",number(sw),number(sh)));
                if transform.rotation != 0.
                    || clip.keyframes.iter().any(|k| k.transform.rotation != 0.)
                {
                    let diagonal =
                        (((sw * max_x).hypot(sh * max_y) / 2.).ceil() * 2.).max(2.) as u32;
                    ensure!(
                        u64::from(diagonal) * u64::from(diagonal) <= 80_000_000,
                        "rotation exceeds render memory budget"
                    );
                    filters.push(format!(
                        "rotate=a='({rotation})*PI/180':ow={diagonal}:oh={diagonal}:c=none"
                    ));
                }
                if clip
                    .keyframes
                    .iter()
                    .any(|k| k.transform.opacity != transform.opacity)
                {
                    filters.push(format!(
                        "geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='alpha(X,Y)*({opacity})'"
                    ));
                } else if transform.opacity < 1. {
                    filters.push(format!(
                        "colorchannelmixer=aa={}",
                        number(transform.opacity)
                    ));
                }
                if clip.fade_in > 0 {
                    filters.push(format!(
                        "fade=t=in:st=0:d={}:alpha=1",
                        number(time(clip.fade_in))
                    ));
                }
                if clip.fade_out > 0 {
                    filters.push(format!(
                        "fade=t=out:st={}:d={}:alpha=1",
                        number(time(clip.duration - clip.fade_out)),
                        number(time(clip.fade_out))
                    ));
                }
                filters.push(format!(
                    "setpts=PTS+({})/TB[layer{layer}]",
                    number(relative_start)
                ));
                graph.push(filters.join(","));
                let x = expression(clip, |t| t.x, &global_frame);
                let y = expression(clip, |t| t.y, &global_frame);
                graph.push(format!("[bg{layer}][layer{layer}]overlay=x='(W-w)/2+{width}*({x})':y='(H-h)/2+{height}*({y})':eval=frame:eof_action=pass:repeatlast=0:enable='gte(t,{})*lt(t,{})'[bg{}]",number(relative_start.max(0.)),number(relative_end),layer+1));
                layer += 1;
            }
        }
        graph.push(format!(
            "[bg{layer}]trim=duration={},setpts=PTS-STARTPTS,format=yuv420p[vout]",
            number(seconds)
        ));
        if let Some(audio_mode) = audio_mode {
            let mut stems = Vec::new();
            for track in project
                .tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Audio && !t.muted && (!solo_audio || t.solo))
            {
                let mut labels = Vec::new();
                for clip in project
                    .clips
                    .iter()
                    .filter(|c| c.track == track.id && c.start < end && c.end() > start)
                {
                    let Content::Media { asset, stream } = clip.content else {
                        continue;
                    };
                    let (input, skip, duration, stream) = add_input(clip, asset, stream)?;
                    let label = format!("ac{}", clip.id);
                    let envelope = expression(clip, |t| t.opacity, &format!("t*{}", project.fps));
                    // Count samples instead of trusting demuxer/filter timestamps. In
                    // particular, atempo + adelay can retain an offset after a seek;
                    // a duration-based atrim then drops the leading silence and
                    // overlaps pieces that were reordered on the strip.
                    let samples = (duration * 48_000.).round() as u64;
                    let source_samples = (duration * clip.speed * 48_000.).round() as u64;
                    let whole_samples = (seconds * 48_000.).round() as u64;
                    let mut filters=vec![format!("[{input}:{stream}]aresample=48000:async=1:first_pts=0,aformat=sample_fmts=fltp:channel_layouts=stereo,atrim=end_sample={source_samples},asetpts=N/SR/TB,{},apad=whole_len={samples},atrim=end_sample={samples},asetpts=N/SR/TB,asetpts=PTS+{}/TB,volume='{}*({envelope})':eval=frame",
                        tempo(clip.speed),number(skip),number(clip.gain*track.gain))];
                    if clip.fade_in > 0 {
                        filters.push(format!("afade=t=in:st=0:d={}", number(time(clip.fade_in))));
                    }
                    if clip.fade_out > 0 {
                        filters.push(format!(
                            "afade=t=out:st={}:d={}",
                            number(time(clip.duration - clip.fade_out)),
                            number(time(clip.fade_out))
                        ));
                    }
                    let delay = (time(clip.start.saturating_sub(start)) * 48_000.).round() as u64;
                    filters.push(format!("asetpts=N/SR/TB,adelay={delay}S:all=1,apad=whole_len={whole_samples},atrim=end_sample={whole_samples},asetpts=N/SR/TB[{label}]"));
                    graph.push(filters.join(","));
                    labels.push(format!("[{label}]"));
                }
                if labels.is_empty() {
                    continue;
                }
                let stem = format!("stem{}", track.id);
                graph.push(format!("{}amix=inputs={}:normalize=0:dropout_transition=0,alimiter=limit=0.98:level=false:latency=1[{stem}]",labels.join(""),labels.len()));
                stems.push((track.name.clone(), stem));
            }
            match audio_mode {
                AudioExport::Separate => audio_outputs = stems,
                AudioExport::Mix | AudioExport::MixAndSeparate => {
                    let mut mixes = Vec::new();
                    for (name, stem) in stems {
                        if audio_mode == AudioExport::MixAndSeparate {
                            graph.push(format!("[{stem}]asplit=2[{stem}s][{stem}m]"));
                            audio_outputs.push((name, format!("{stem}s")));
                            mixes.push(format!("[{stem}m]"));
                        } else {
                            mixes.push(format!("[{stem}]"));
                        }
                    }
                    if !mixes.is_empty() {
                        graph.push(format!("{}amix=inputs={}:normalize=0:dropout_transition=0,alimiter=limit=0.98:level=false:latency=1[mix]",mixes.join(""),mixes.len()));
                        audio_outputs.insert(0, ("Mix".into(), "mix".into()));
                    }
                }
            }
        }
        Ok(Self {
            input_args: args,
            filter: graph.join(";\n"),
            audio: audio_outputs,
            seconds,
            width,
            height,
            fps,
            directory,
        })
    }
    pub(crate) fn command(&self) -> std::process::Command {
        let mut command = media_command("ffmpeg");
        command
            .current_dir(self.directory.path())
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-nostdin",
                "-filter_complex_threads",
                "2",
            ])
            .args(&self.input_args)
            .arg("-filter_complex")
            .arg(&self.filter);
        command
    }
}

/// Quick edits are a sequence, so preview does not need the export compositor's
/// RGBA layers, full-canvas overlays or intermediate YUV conversion.
pub(crate) fn playback_video_plan(
    project: &Project,
    start: u64,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<RenderPlan> {
    let clips = crate::pieces(project);
    let simple = clips.first().is_some_and(|c| c.start == 0)
        && clips.iter().all(|c| {
            matches!(c.content, Content::Media { .. })
                && c.transform == Transform::default()
                && c.keyframes.is_empty()
                && c.fade_in == 0
                && c.fade_out == 0
        })
        && clips.windows(2).all(|pair| pair[0].end() == pair[1].start)
        && project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .all(|t| !t.muted)
        && project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .count()
            == 1;
    if !simple {
        return RenderPlan::build(project, start, project.duration(), width, height, fps, None);
    }
    project.validate()?;
    let mut args = Vec::new();
    let mut graph = Vec::new();
    let mut labels = Vec::new();
    let mut end = start;
    // Bound AV1 decoder contexts even after many splits of the same recording.
    // The transport opens the next batch while queued frames are being shown.
    for clip in clips.into_iter().filter(|c| c.end() > start).take(2) {
        let Content::Media { asset, stream } = clip.content else {
            unreachable!()
        };
        let asset = project
            .assets
            .iter()
            .find(|a| a.id == asset)
            .context("Missing recording")?;
        let first = clip.start.max(start);
        let seconds = project.seconds(clip.end() - first);
        let source =
            clip.source_in_ns as f64 / 1e9 + project.seconds(first - clip.start) * clip.speed;
        let index = labels.len();
        args.extend([
            "-threads".into(),
            "4".into(),
            "-analyzeduration".into(),
            "0".into(),
            "-probesize".into(),
            "262144".into(),
            "-ss".into(),
            number(source).into(),
            "-t".into(),
            number(seconds * clip.speed).into(),
            "-i".into(),
            path_for_tool(&asset.path)?.into_os_string(),
        ]);
        let count = (seconds * f64::from(fps)).round().max(1.) as u64;
        graph.push(format!("[{index}:{stream}]setpts=(PTS-STARTPTS)/{},fps={fps},scale={width}:{height}:force_original_aspect_ratio=decrease:flags=fast_bilinear,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,format=bgra,tpad=stop_mode=clone:stop_duration=1,trim=end_frame={count},setpts=N/({fps}*TB)[piece{index}]",number(clip.speed)));
        labels.push(format!("[piece{index}]"));
        end = clip.end();
    }
    ensure!(!labels.is_empty(), "No video after the playhead");
    graph.push(if labels.len() == 1 {
        format!("{}null[vout]", labels[0])
    } else {
        format!("{}concat=n={}:v=1:a=0[vout]", labels.join(""), labels.len())
    });
    Ok(RenderPlan {
        input_args: args,
        filter: graph.join(";"),
        audio: Vec::new(),
        seconds: project.seconds(end - start),
        width,
        height,
        fps,
        directory: tempfile::tempdir()?,
    })
}

pub(crate) fn playback_audio_plan(project: &Project, start: u64, track: u64) -> Result<RenderPlan> {
    let mut source = project.clone();
    for t in &mut source.tracks {
        if t.kind == TrackKind::Audio {
            t.muted = t.id != track;
            t.solo = false;
            t.gain = 1.;
        }
    }
    let mut plan = RenderPlan::build_mode(
        &source,
        start,
        source.duration(),
        (2, 2, 1),
        Some(AudioExport::Mix),
        false,
        &HashMap::new(),
    )?;
    let mut args = Vec::with_capacity(plan.input_args.len());
    for arg in plan.input_args {
        if arg == "-i" {
            args.extend([
                "-analyzeduration".into(),
                "0".into(),
                "-probesize".into(),
                "262144".into(),
                "-vn".into(),
            ]);
        }
        args.push(arg);
    }
    plan.input_args = args;
    plan.filter.push_str(";[vout]nullsink");
    Ok(plan)
}

#[derive(Default)]
pub struct JobProgress {
    pub microseconds: AtomicU64,
    pub done: AtomicBool,
    pub cancel: AtomicBool,
    pub suppressing_audio: AtomicBool,
}

struct MediaProcess(Child);
impl Drop for MediaProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct SuppressionStage<'a>(&'a AtomicBool);
impl<'a> SuppressionStage<'a> {
    fn begin(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::Release);
        Self(flag)
    }
}
impl Drop for SuppressionStage<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Error context exposes only an exclusively owned unfinished export, never a
/// destination belonging to another writer. Callers may offer Show in folder.
#[derive(Debug)]
pub struct RetainedExport {
    pub partial: PathBuf,
}
impl std::fmt::Display for RetainedExport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "partial export retained at {}", self.partial.display())
    }
}
impl std::error::Error for RetainedExport {}

fn publish_export(partial: &Path, destination: &Path, cancelled: impl Fn() -> bool) -> Result<()> {
    ensure!(!cancelled(), "export cancelled before publication");
    // std::fs::rename replaces existing files, including on Windows. Tempfile's
    // no-clobber publication performs the destination check inside the OS move.
    // Disable cleanup before consuming the path: a collision must preserve both
    // the other writer's destination and our recoverable partial.
    let mut path = tempfile::TempPath::try_from_path(partial.to_path_buf())?;
    path.disable_cleanup(true);
    path.persist_noclobber(destination)
        .map_err(|error| error.error)
        .with_context(|| {
            format!(
                "could not publish export without replacing {}",
                destination.display()
            )
        })
}

fn publish_verified_export(
    partial: &Path,
    destination: &Path,
    project: &Project,
    baseline: &[crate::SourceStamp],
    cancelled: impl Fn() -> bool,
) -> Result<()> {
    ensure!(baseline.len() == project.assets.len(), "Export source baseline is incomplete");
    for (asset, expected) in project.assets.iter().zip(baseline) {
        ensure!(!cancelled(), "export cancelled before source verification");
        // Resolve the original path again: checking only the old canonical path
        // would miss an alias retargeted while FFmpeg was reading the source.
        let current = crate::SourceStamp::read(&asset.path)?;
        ensure!(&current == expected,
            "Source changed during export; output was not published. Review the source and retry: {}",
            asset.path.display());
    }
    publish_export(partial, destination, cancelled)
}
fn export_diagnostic(path: &Path) -> String {
    (|| -> std::io::Result<String> {
        let mut file = fs::File::open(path)?;
        let bytes = file.metadata()?.len().min(4000);
        file.seek(SeekFrom::End(-(bytes as i64)))?;
        let mut tail = Vec::with_capacity(bytes as usize);
        file.take(bytes).read_to_end(&mut tail)?;
        Ok(String::from_utf8_lossy(&tail).into_owned())
    })()
    .unwrap_or_default()
}

/// Prepare only the requested source intervals of explicitly selected audio
/// tracks. Preview and source media stay raw; gain/speed/fades apply afterwards.
fn prepare_audio(
    project: &Project,
    start: u64,
    end: u64,
    directory: &Path,
    progress: &JobProgress,
) -> Result<HashMap<u64, PathBuf>> {
    let solo = project
        .tracks
        .iter()
        .any(|t| t.kind == TrackKind::Audio && t.solo);
    let clips = project
        .clips
        .iter()
        .filter(|c| {
            c.start < end
                && c.end() > start
                && project.tracks.iter().any(|t| {
                    t.id == c.track
                        && t.kind == TrackKind::Audio
                        && t.suppress_noise
                        && !t.muted
                        && (!solo || t.solo)
                })
        })
        .collect::<Vec<_>>();
    let mut overrides = HashMap::new();
    if clips.is_empty() {
        return Ok(overrides);
    }
    ensure!(!progress.cancel.load(Ordering::Relaxed), "export cancelled");
    let _suppression_stage = SuppressionStage::begin(&progress.suppressing_audio);
    let mut denoiser = rcr_dpdfnet::Denoiser::new()?;
    let mut cache = HashMap::new();
    for clip in clips {
        ensure!(
            !progress.cancel.load(Ordering::Relaxed),
            "export cancelled during microphone suppression"
        );
        let Content::Media { asset, stream } = clip.content else {
            continue;
        };
        let media = project
            .assets
            .iter()
            .find(|a| a.id == asset)
            .context("missing audio asset")?;
        let skip = project.seconds(start.saturating_sub(clip.start));
        let source = clip.source_in_ns as f64 / 1e9 + skip * clip.speed;
        let seconds = project.seconds(clip.end().min(end) - clip.start.max(start)) * clip.speed;
        let key = (
            asset,
            stream,
            (source * 1e9).round() as u64,
            (seconds * 1e9).round() as u64,
        );
        if let Some(path) = cache.get(&key) {
            overrides.insert(clip.id, PathBuf::clone(path));
            continue;
        }
        let path = directory.join(format!("microphone-{}.f32", clip.id));
        let log = directory.join(format!("decode-{}.log", clip.id));
        let mut command = media_command("ffmpeg");
        command
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-nostdin",
                "-threads",
                "2",
                "-ss",
                &number(source),
                "-t",
                &number(seconds),
                "-i",
            ])
            .arg(path_for_tool(&media.path)?)
            .args([
                "-map",
                &format!("0:{stream}"),
                "-vn",
                "-af",
                &format!(
                    "aresample=48000:async=1:first_pts=0,apad=whole_dur={},atrim=duration={}",
                    number(seconds),
                    number(seconds)
                ),
                "-ac",
                "2",
                "-ar",
                "48000",
                "-f",
                "f32le",
                "pipe:1",
            ])
            .stderr(Stdio::from(fs::File::create(&log)?));
        let mut writer = BufWriter::new(fs::File::create_new(&path)?);
        denoiser.reset();
        let mut emit = |samples: &[f32]| -> Result<()> {
            ensure!(
                !progress.cancel.load(Ordering::Relaxed),
                "export cancelled during microphone suppression"
            );
            for value in samples {
                writer.write_all(&value.to_le_bytes())?;
                writer.write_all(&value.to_le_bytes())?;
            }
            Ok(())
        };
        let status = crate::tools::stream_media_output(
            &mut command,
            480 * 8,
            || progress.cancel.load(Ordering::Relaxed),
            |bytes| {
                let count = bytes.len();
                ensure!(
                    count.is_multiple_of(8),
                    "unaligned decoded microphone samples"
                );
                let count = count / 8;
                let mut mono = [0f32; 480];
                for (value, raw) in mono[..count]
                    .iter_mut()
                    .zip(bytes[..count * 8].as_chunks::<8>().0)
                {
                    *value = (f32::from_le_bytes(raw[..4].try_into()?)
                        + f32::from_le_bytes(raw[4..].try_into()?))
                        * 0.5;
                    ensure!(value.is_finite(), "Non-finite decoded microphone sample");
                }
                denoiser.process(&mono[..count], &mut emit)?;
                Ok(())
            },
        )
        .context("decode microphone source for DPDFNet")?;
        denoiser.finish(&mut emit)?;
        writer.flush()?;
        ensure!(
            status.success(),
            "microphone decode failed: {}",
            export_diagnostic(&log)
        );
        cache.insert(key, path.clone());
        overrides.insert(clip.id, path);
    }
    Ok(overrides)
}

/// Blocks its worker, never the GPUI thread. Partial output is never published.
pub fn export(
    project: &Project,
    options: &ExportOptions,
    progress: &JobProgress,
) -> Result<PathBuf> {
    ensure!(
        !options.output.exists(),
        "export destination already exists"
    );
    ensure!(
        (1_000_000..=200_000_000).contains(&options.bitrate),
        "invalid export bitrate"
    );
    let extension = options
        .output
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    ensure!(
        extension == "mp4" || extension == "mkv",
        "export requires .mp4 or .mkv"
    );
    let parent = options
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let verified_sources = crate::validate_sources_cancellable(project, || progress.cancel.load(Ordering::Relaxed))?;
    fs::create_dir_all(parent)?;
    let final_path = path_for_tool(parent)?.join(
        options
            .output
            .file_name()
            .context("export filename missing")?,
    );
    // Each export owns a private adjacent directory. Concurrent writers cannot
    // race over the same .partial path, and publication stays on one filesystem.
    let staging = tempfile::Builder::new()
        .prefix(".rcr-export-")
        .tempdir_in(final_path.parent().context("export parent missing")?)?;
    let mut partial_name = final_path
        .file_name()
        .context("export filename missing")?
        .to_os_string();
    partial_name.push(".partial");
    let partial = staging.path().join(partial_name);
    let end = options.end.unwrap_or_else(|| project.duration());
    project.validate()?;
    ensure!(
        options.start < end && end <= project.duration(),
        "invalid export range"
    );
    let prepared_dir = tempfile::tempdir()?;
    let prepared = prepare_audio(project, options.start, end, prepared_dir.path(), progress)?;
    let plan = RenderPlan::build_mode(
        project,
        options.start,
        end,
        (project.width, project.height, project.fps),
        Some(options.audio),
        true,
        &prepared,
    )?;
    let stderr = fs::File::create(plan.directory.path().join("ffmpeg.log"))?;
    let mut command = plan.command();
    command.args(["-map", "[vout]"]);
    for (_, label) in &plan.audio {
        command.args(["-map", &format!("[{label}]")]);
    }
    command.args([
        "-c:v",
        options.video_codec.encoder(),
        "-preset",
        "p3",
        "-rc",
        "vbr",
        "-b:v",
        &options.bitrate.to_string(),
        "-g",
        &(project.fps * 2).to_string(),
        "-pix_fmt",
        "yuv420p",
    ]);
    if !plan.audio.is_empty() {
        command.args(["-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2"]);
    }
    for (i, (name, _)) in plan.audio.iter().enumerate() {
        command
            .arg(format!("-metadata:s:a:{i}"))
            .arg(format!("title={name}"));
        command
            .arg(format!("-metadata:s:a:{i}"))
            .arg(format!("handler_name={name}"));
        command
            .arg(format!("-disposition:a:{i}"))
            .arg(if i == 0 { "default" } else { "0" });
    }
    command.args([
        "-t",
        &number(plan.seconds),
        "-progress",
        "pipe:1",
        "-nostats",
        "-n",
    ]);
    if extension == "mp4" {
        command.args(["-movflags", "+faststart", "-f", "mp4"]);
    } else {
        command.args(["-f", "matroska"]);
    }
    // Own the child through every error path, including a failed status query
    // or missing progress pipe, so failed exports cannot retain an encoder.
    let mut child = MediaProcess(
        command
            .arg(&partial)
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .spawn()
            .context(
                "FFmpeg is required for editor export; set RCR_FFMPEG if it is outside PATH",
            )?,
    );
    // Preserve partials after rendering starts, following the existing recovery
    // policy. Empty staging directories are removed after failure or success.
    let staging = staging.keep();
    let result = (|| -> Result<PathBuf> {
        let stdout = child
            .0
            .stdout
            .take()
            .context("export progress pipe missing")?;
        let out_time = Arc::new(AtomicU64::new(0));
        let reader_time = out_time.clone();
        let reader_failed = Arc::new(AtomicBool::new(false));
        let failed = reader_failed.clone();
        let reader = thread::Builder::new()
            .name("rcr-export-progress".into())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    for line in BufReader::new(stdout).lines() {
                        let line = line?;
                        if let Some(value) = line.strip_prefix("out_time_us=") {
                            if let Ok(value) = value.parse() {
                                reader_time.store(value, Ordering::Relaxed);
                            }
                        }
                    }
                    Ok(())
                })();
                if result.is_err() {
                    failed.store(true, Ordering::Release);
                }
                result
            })?;
        let status = (|| -> Result<_> {
            loop {
                progress
                    .microseconds
                    .store(out_time.load(Ordering::Relaxed), Ordering::Relaxed);
                ensure!(!progress.cancel.load(Ordering::Relaxed), "export cancelled");
                ensure!(
                    !reader_failed.load(Ordering::Acquire),
                    "export progress reader failed"
                );
                if let Some(status) = child.0.try_wait()? {
                    return Ok(status);
                }
                thread::sleep(Duration::from_millis(50));
            }
        })();
        if status.is_err() {
            let _ = child.0.kill();
            let _ = child.0.wait();
        }
        let reader_result = reader
            .join()
            .map_err(|_| anyhow::anyhow!("Export progress reader panicked"))?;
        let status = status?;
        reader_result?;
        let error = export_diagnostic(&plan.directory.path().join("ffmpeg.log"));
        ensure!(status.success(), "export failed: {}", error);
        publish_verified_export(&partial, &final_path, project, &verified_sources, || {
            progress.cancel.load(Ordering::Relaxed)
        })?;
        progress
            .microseconds
            .store((plan.seconds * 1e6) as u64, Ordering::Relaxed);
        progress.done.store(true, Ordering::Release);
        Ok(final_path)
    })();
    drop(child); // Reap before examining retained files, even on setup failures.
    let _ = fs::remove_dir(&staging); // Only our empty directory; never recursive.
    if result.is_err() && partial.is_file() {
        result.context(RetainedExport { partial })
    } else {
        result
    }
}

#[derive(Clone)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub frame: u64,
    pub bgra: Vec<u8>,
}
pub struct PreviewJob {
    pub frames: mpsc::Receiver<PreviewFrame>,
    pub errors: mpsc::Receiver<String>,
    pub done: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}
impl PreviewJob {
    /// Bounded software composition fallback. Call `start_sized` from responsive UIs.
    pub fn start(project: Project, start: u64, play: bool) -> Result<Self> {
        let height = (960. * f64::from(project.height) / f64::from(project.width)).round() as u32;
        Self::start_sized(project, start, play, (960, height))
    }
    /// Render at the preview's physical pixel size, preserving source aspect ratio.
    pub fn start_sized(project: Project, start: u64, play: bool, size: (u32, u32)) -> Result<Self> {
        let size = preview_size(&project, size);
        Self::start_fitted(project, start, play, size)
    }
    /// Internal callers already fitted to project aspect must retain exactly
    /// those pixels: fitting even-rounded dimensions twice can shrink them.
    pub(crate) fn start_fitted(project: Project, start: u64, play: bool, size: (u32, u32)) -> Result<Self> {
        validate_fitted_preview_size(size)?;
        ensure!(project.duration() > 0, "the timeline is empty");
        let start = start.min(project.duration() - 1);
        let (tx, frames) = mpsc::sync_channel(1);
        let (error_tx, errors) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let child = Arc::new(Mutex::new(None));
        let worker_child = child.clone();
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = done.clone();
        thread::Builder::new()
            .name("rcr-editor-preview".into())
            .spawn(move || {
                let run = || -> Result<()> {
                    let (width, height) = size;
                    let fps = if play {
                        30.min(project.fps)
                    } else {
                        project.fps
                    };
                    let end = if play {
                        project.duration()
                    } else {
                        (start + u64::from(project.fps)).min(project.duration())
                    };
                    let plan = if play {
                        RenderPlan::build(&project, start, end, width, height, fps, None)?
                    } else {
                        playback_video_plan(&project, start, width, height, fps)?
                    };
                    let mut command = plan.command();
                    command.args([
                        "-map", "[vout]", "-an", "-pix_fmt", "bgra", "-f", "rawvideo",
                    ]);
                    if !play {
                        command.args(["-frames:v", "1"]);
                    }
                    let log = fs::File::create(plan.directory.path().join("preview.log"))?;
                    let mut process = command
                        .arg("pipe:1")
                        .stdout(Stdio::piped())
                        .stderr(Stdio::from(log))
                        .spawn()?;
                    let mut pipe = process
                        .stdout
                        .take()
                        .context("preview output pipe missing")?;
                    *worker_child.lock().unwrap() = Some(process);
                    let mut started = None;
                    let mut frame = 0u64;
                    while !worker_cancel.load(Ordering::Relaxed) {
                        let mut bgra = vec![0; width as usize * height as usize * 4];
                        if let Err(error) = pipe.read_exact(&mut bgra) {
                            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                                break;
                            }
                            return Err(error.into());
                        }
                        let target = *started.get_or_insert_with(std::time::Instant::now)
                            + Duration::from_secs_f64(frame as f64 / f64::from(fps));
                        if let Some(wait) = target.checked_duration_since(std::time::Instant::now())
                        {
                            thread::sleep(wait);
                        }
                        let message = PreviewFrame {
                            width,
                            height,
                            frame: start + frame * u64::from(project.fps) / u64::from(fps),
                            bgra,
                        };
                        let mut pending = message;
                        while !worker_cancel.load(Ordering::Relaxed) {
                            match tx.try_send(pending) {
                                Ok(()) => break,
                                Err(mpsc::TrySendError::Full(message)) => {
                                    pending = message;
                                    thread::sleep(Duration::from_millis(5));
                                }
                                Err(mpsc::TrySendError::Disconnected(_)) => {
                                    worker_cancel.store(true, Ordering::Relaxed);
                                    break;
                                }
                            }
                        }
                        frame += 1;
                    }
                    if let Some(mut process) = worker_child.lock().unwrap().take() {
                        if worker_cancel.load(Ordering::Relaxed) {
                            let _ = process.kill();
                        }
                        let status = process.wait()?;
                        if !worker_cancel.load(Ordering::Relaxed) {
                            ensure!(
                                status.success(),
                                "preview failed: {}",
                                fs::read_to_string(plan.directory.path().join("preview.log"))
                                    .unwrap_or_default()
                            );
                        }
                    }
                    Ok(())
                };
                if let Err(error) = run() {
                    let _ = error_tx.try_send(format!("{error:#}"));
                }
                // Reap on pipe errors as well as normal completion/cancellation.
                if let Some(mut process) = worker_child.lock().unwrap().take() {
                    let _ = process.kill();
                    let _ = process.wait();
                }
                worker_done.store(true, Ordering::Release);
            })?;
        Ok(Self {
            frames,
            errors,
            cancel,
            child,
            done,
        })
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }
}

fn validate_fitted_preview_size(size: (u32, u32)) -> Result<()> {
    ensure!(size.0 >= 2 && size.1 >= 2 && size.0.is_multiple_of(2) && size.1.is_multiple_of(2),
        "Preview dimensions must be positive even pixels");
    ensure!(size.0 <= 16384 && size.1 <= 16384
        && u64::from(size.0) * u64::from(size.1) * 4 <= 64 * 1024 * 1024,
        "Preview image exceeds memory budget");
    Ok(())
}
pub fn preview_size(project: &Project, bounds: (u32, u32)) -> (u32, u32) {
    let scale = (f64::from(bounds.0.max(2)) / f64::from(project.width))
        .min(f64::from(bounds.1.max(2)) / f64::from(project.height))
        .min(1.);
    (
        ((f64::from(project.width) * scale / 2.).floor() as u32 * 2).max(2),
        ((f64::from(project.height) * scale / 2.).floor() as u32 * 2).max(2),
    )
}
impl Drop for PreviewJob {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Audio monitoring stays outside the recording process and follows the same
/// track mute/solo, speed, fade and gain graph as export. FFplay provides the
/// playback device here; the saved stems remain independent.
pub struct AudioPreviewJob {
    cancel: Arc<AtomicBool>,
    children: Arc<Mutex<Option<(Child, Child)>>>,
    pub errors: mpsc::Receiver<String>,
    pub done: Arc<AtomicBool>,
}
impl AudioPreviewJob {
    pub fn start(project: Project, start: u64) -> Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let children = Arc::new(Mutex::new(None));
        let worker_children = children.clone();
        let (tx, errors) = mpsc::sync_channel(1);
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = done.clone();
        thread::Builder::new()
            .name("rcr-editor-audio".into())
            .spawn(move || {
                let run = || -> Result<()> {
                    if !project.tracks.iter().any(|t| t.kind == TrackKind::Audio && !t.muted) {
                        return Ok(());
                    }
                    let end = project.duration();
                    if start >= end { return Ok(()); }
                    let mut plan = RenderPlan::build_mode(
                        &project, start, end, (2, 2, 1), Some(AudioExport::Mix), false,&HashMap::new(),
                    )?;
                    if plan.audio.is_empty() { return Ok(()); }
                    plan.filter.push_str(";[vout]nullsink");
                    let log = fs::File::create(plan.directory.path().join("audio.log"))?;
                    let mut source = plan.command()
                        .args(["-map", "[mix]", "-vn", "-ar", "48000", "-ac", "2", "-f", "f32le", "pipe:1"])
                        .stdout(Stdio::piped()).stderr(Stdio::from(log.try_clone()?)).spawn()?;
                    let pipe = source.stdout.take().context("audio pipe missing")?;
                    // Use PCM demuxer options; recent FFplay removed the -ac alias.
                    let player = match media_command("ffplay")
                        .args(["-hide_banner", "-loglevel", "error", "-nodisp", "-autoexit",
                            "-probesize", "32", "-analyzeduration", "0", "-f", "f32le",
                            "-sample_rate", "48000", "-ch_layout", "stereo", "-i", "pipe:0"])
                        .stdin(Stdio::from(pipe)).stdout(Stdio::null()).stderr(Stdio::from(log)).spawn()
                    {
                        Ok(player) => player,
                        Err(error) => {
                            let _ = source.kill(); let _ = source.wait();
                            return Err(error).context("FFplay is required for editor audio monitoring (or set RCR_FFPLAY)");
                        }
                    };
                    *worker_children.lock().unwrap() = Some((source, player));
                    loop {
                        let ended = {
                            let mut lock = worker_children.lock().unwrap();
                            let Some((source, player)) = lock.as_mut() else { break };
                            if worker_cancel.load(Ordering::Relaxed) {
                                let _ = source.kill(); let _ = player.kill(); true
                            } else { player.try_wait()?.is_some() }
                        };
                        if ended { break; }
                        thread::sleep(Duration::from_millis(20));
                    }
                    if let Some((mut source, mut player)) = worker_children.lock().unwrap().take() {
                        let source_status = source.wait()?;
                        let player_status = player.wait()?;
                        if !worker_cancel.load(Ordering::Relaxed) {
                            ensure!(source_status.success() && player_status.success(),
                                "audio monitor failed: {}", fs::read_to_string(plan.directory.path().join("audio.log")).unwrap_or_default());
                        }
                    }
                    Ok(())
                };
                if let Err(error) = run() { let _ = tx.send(format!("{error:#}")); }
                if let Some((mut source, mut player)) = worker_children.lock().unwrap().take() {
                    let _ = source.kill(); let _ = player.kill();
                    let _ = source.wait(); let _ = player.wait();
                }
                worker_done.store(true, Ordering::Release);
            })?;
        Ok(Self {
            cancel,
            children,
            errors,
            done,
        })
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some((source, player)) = self.children.lock().unwrap().as_mut() {
            let _ = source.kill();
            let _ = player.kill();
        }
    }
}
impl Drop for AudioPreviewJob {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn already_fitted_preview_geometry_is_validated_without_a_second_aspect_fit() {
        let project = crate::Project { width: 2560, height: 1440, ..Default::default() };
        let size = super::preview_size(&project, (1742, 981));
        assert_eq!(size, (1742, 978));
        assert_ne!(super::preview_size(&project, size), size,
            "Fixture must expose double-fitting even rounding");
        assert!(super::validate_fitted_preview_size(size).is_ok());
        for invalid in [(0, 2), (2, 0), (3, 2), (2, 3), (16386, 2), (8192, 8192)] {
            assert!(super::validate_fitted_preview_size(invalid).is_err());
        }
    }
    use super::*;
    fn publication_source(path: &Path) -> (Project, Vec<crate::SourceStamp>) {
        let stamp = crate::SourceStamp::read(path).unwrap();
        let project = Project { assets: vec![crate::MediaAsset {
            id: 1, path: path.to_owned(), source_stamp: None,
            duration_ns: 1_000_000_000, width: 16, height: 16, streams: vec![],
        }], ..Default::default() };
        (project, vec![stamp])
    }

    #[test]
    fn export_publication_rejects_changed_or_missing_sources_and_preserves_partial() {
        for removed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("source.mkv");
            let partial = dir.path().join("completed.partial");
            let destination = dir.path().join("result.mp4");
            fs::write(&source, b"original source").unwrap();
            let (project, stamps) = publication_source(&source);
            fs::write(&partial, b"already rendered media").unwrap();
            if removed { fs::remove_file(&source).unwrap(); }
            else { fs::write(&source, b"source replaced while encoder was running").unwrap(); }
            assert!(publish_verified_export(&partial, &destination, &project, &stamps, || false).is_err());
            assert!(!destination.exists());
            assert_eq!(fs::read(&partial).unwrap(), b"already rendered media");
        }
    }

    #[test]
    fn export_publication_checks_final_cancellation_after_source_verification() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.mkv");
        let partial = dir.path().join("completed.partial");
        let destination = dir.path().join("result.mp4");
        fs::write(&source, b"original source").unwrap();
        let (project, stamps) = publication_source(&source);
        fs::write(&partial, b"already rendered media").unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        assert!(publish_verified_export(&partial, &destination, &project, &stamps,
            || calls.fetch_add(1, Ordering::Relaxed) > 0).is_err());
        assert!(!destination.exists() && partial.exists());
        publish_verified_export(&partial, &destination, &project, &stamps, || false).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"already rendered media");
        assert_eq!(fs::read(&source).unwrap(), b"original source");
    }

    #[test]
    fn completed_export_never_replaces_a_destination_that_appears_before_publication() {
        let dir = tempfile::tempdir().unwrap();
        let partial = dir.path().join("owned.partial");
        let destination = dir.path().join("result.mp4");
        fs::write(&partial, b"completed export").unwrap();
        let result = publish_export(&partial, &destination, || {
            fs::write(&destination, b"another writer's original").unwrap();
            false
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read(&destination).unwrap(),
            b"another writer's original"
        );
        assert_eq!(fs::read(&partial).unwrap(), b"completed export");
    }
    #[test]
    fn concurrent_exports_publish_one_winner_and_keep_the_other_partial() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("result.mp4");
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let workers = [b"first export".as_slice(), b"second export".as_slice()]
            .into_iter()
            .enumerate()
            .map(|(index, bytes)| {
                let partial = dir.path().join(format!("owned-{index}.partial"));
                fs::write(&partial, bytes).unwrap();
                let destination = destination.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let success = publish_export(&partial, &destination, || false).is_ok();
                    (partial, success)
                })
            })
            .collect::<Vec<_>>();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|(_, success)| *success).count(), 1);
        for (index, (partial, success)) in results.iter().enumerate() {
            let expected = if index == 0 {
                b"first export".as_slice()
            } else {
                b"second export".as_slice()
            };
            if *success {
                assert_eq!(fs::read(&destination).unwrap(), expected);
                assert!(!partial.exists());
            } else {
                assert_eq!(fs::read(partial).unwrap(), expected);
            }
        }
    }
    #[test]
    fn cancellation_at_commit_keeps_partial_without_creating_destination() {
        let dir = tempfile::tempdir().unwrap();
        let partial = dir.path().join("owned.partial");
        let destination = dir.path().join("result.mp4");
        fs::write(&partial, b"completed but cancelled").unwrap();
        assert!(publish_export(&partial, &destination, || true).is_err());
        assert!(!destination.exists());
        assert_eq!(fs::read(&partial).unwrap(), b"completed but cancelled");
    }
    #[test]
    fn diagnostic_reads_only_bounded_log_tail() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("tool.log");
        let mut bytes = vec![b'x'; 64 * 1024];
        bytes.extend_from_slice(b"specific final failure");
        fs::write(&log, bytes).unwrap();
        let diagnostic = export_diagnostic(&log);
        assert_eq!(diagnostic.len(), 4000);
        assert!(diagnostic.ends_with("specific final failure"));
    }
    #[test]
    fn suppression_indicator_clears_on_early_failure() {
        let flag = AtomicBool::new(false);
        let fail = || -> Result<()> {
            let _stage = SuppressionStage::begin(&flag);
            assert!(flag.load(Ordering::Acquire));
            anyhow::bail!("decoder failed")
        };
        assert!(fail().is_err());
        assert!(!flag.load(Ordering::Acquire));
    }
    #[test]
    fn tempo_decomposes_extreme_speed_without_out_of_range_stages() {
        assert_eq!(tempo(1.), "anull");
        assert_eq!(tempo(8.), "atempo=2,atempo=2,atempo=2.000000000");
        assert!(tempo(0.125).starts_with("atempo=0.5,atempo=0.5"));
    }
    #[test]
    fn keyframe_expression_interpolates_and_holds_last_value() {
        let clip = Clip {
            id: 1,
            track: 2,
            group: 1,
            content: Content::Title {
                text: "Test".into(),
                color: "ffffff".into(),
                font_size: 48,
            },
            start: 60,
            duration: 180,
            source_in_ns: 0,
            speed: 1.,
            gain: 1.,
            fade_in: 0,
            fade_out: 0,
            transform: Transform::default(),
            keyframes: vec![crate::Keyframe {
                frame: 60,
                transform: Transform {
                    x: 0.5,
                    ..Default::default()
                },
            }],
        };
        let expr = expression(&clip, |t| t.x, "t*60");
        assert!(expr.contains("lte(t*60,60)"));
        assert!(expr.contains("0.500000000"));
    }
}
