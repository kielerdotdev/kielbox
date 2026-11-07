use crate::{ACCENT, LINE, MUTED, PANEL, recordings, theme};
#[path = "editor_playback_status.rs"]
mod playback_status;
#[path = "editor_watch_share.rs"]
mod watch_share;
#[path = "editor_watch_fullscreen.rs"]
mod watch_fullscreen;
use gpui::{prelude::*, *};
use editor_core::{
    self as core, AudioExport, Content, Edit, Editor, ExportOptions, Id, JobProgress, MediaAsset,
    PlaybackOutput, PlaybackSession, PreviewFrame, Project, QuickEdit, ScrubSession, TrackKind,
    WaveBin,
};
struct PendingFrame {
    frame: PreviewFrame,
    seek: Option<core::ScrubMetadata>,
    trace: Option<(core::ScrubTrace, Instant)>,
}
#[derive(Clone, Copy)]
struct CpuFrameTiming {
    core: core::ScrubTrace,
    received_at: Instant,
    bind_started_at: Instant,
    bound_at: Instant,
}
impl CpuFrameTiming {
    fn report(self, identity: CpuFrameIdentity, paint_started_at: Instant, painted_at: Instant) -> serde_json::Value {
        let ms = |later: Instant, earlier: Instant| later.saturating_duration_since(earlier).as_secs_f64() * 1000.;
        serde_json::json!({
            "serial":identity.seek.map(|metadata| metadata.request_serial),"frame":identity.frame,
            "width":identity.size.0,"height":identity.size.1,
            "original":identity.seek.is_some_and(|metadata| metadata.original),
            "queue_ms":ms(self.core.started_at,self.core.queued_at),
            "core_decode_ms":ms(self.core.decoded_at,self.core.started_at),
            "cache_copy_ms":ms(self.core.cached_ready_at,self.core.decoded_at),
            "ready_to_ui_ms":ms(self.received_at,self.core.cached_ready_at),
            "ui_to_bind_ms":ms(self.bind_started_at,self.received_at),
            "bind_ms":ms(self.bound_at,self.bind_started_at),
            "bind_to_paint_ms":ms(paint_started_at,self.bound_at),
            "paint_call_ms":ms(painted_at,paint_started_at),
            "total_ms":ms(painted_at,self.core.queued_at),
            "scope":"Successful GPUI image paint call; not GPU completion or monitor scanout",
        })
    }
    fn log(self, identity: CpuFrameIdentity, paint_started_at: Instant, painted_at: Instant) {
        static LOGGED: AtomicU64 = AtomicU64::new(0);
        if LOGGED.try_update(Ordering::Relaxed, Ordering::Relaxed, |count| (count < 256).then_some(count + 1)).is_ok() {
            core::write_seek_trace("Seek pipeline trace", self.report(identity, paint_started_at, painted_at));
        }
    }
}
const FAST_SEEK_MAX_SIZE: (u32, u32) = (1920, 1080);
fn fast_seek_bounds(viewport: (u32, u32)) -> (u32, u32) {
    (viewport.0.min(FAST_SEEK_MAX_SIZE.0), viewport.1.min(FAST_SEEK_MAX_SIZE.1))
}
fn fast_seek_size(project: &Project, viewport: (u32, u32)) -> (u32, u32) {
    core::preview_size(project, fast_seek_bounds(viewport))
}
impl From<PreviewFrame> for PendingFrame {
    fn from(frame: PreviewFrame) -> Self { Self { frame, seek: None, trace: None } }
}
impl From<core::ScrubFrame> for PendingFrame {
    fn from(frame: core::ScrubFrame) -> Self { Self { frame: frame.frame, seek: Some(frame.metadata),
        trace: frame.trace.map(|trace| (trace, Instant::now())) } }
}
#[cfg(test)]
mod seek_pipeline_trace_tests {
    use super::{CpuFrameIdentity, CpuFrameTiming, PendingFrame};
    use gpui::ImageId;
    use editor_core::{PreviewFrame, ScrubFrame, ScrubMetadata, ScrubTrace};
    use std::time::{Duration, Instant};
    #[test]
    fn trace_phase_report_separates_queue_decode_copy_handoff_bind_and_actual_paint() {
        let base = Instant::now();
        let at = |ms| base + Duration::from_millis(ms);
        let timing = CpuFrameTiming { core: ScrubTrace { queued_at: at(0), started_at: at(1), decoded_at: at(3), cached_ready_at: at(6) },
            received_at: at(10), bind_started_at: at(15), bound_at: at(21) };
        let identity = CpuFrameIdentity { frame: 12, size: (1920, 1080), image_id: ImageId(7),
            seek: Some(ScrubMetadata { request_serial: 42, requested_frame: 12, exact: true, original: false }) };
        let report = timing.report(identity, at(28), at(36));
        for (key, expected) in [("queue_ms", 1.), ("core_decode_ms", 2.), ("cache_copy_ms", 3.),
            ("ready_to_ui_ms", 4.), ("ui_to_bind_ms", 5.), ("bind_ms", 6.), ("bind_to_paint_ms", 7.), ("paint_call_ms", 8.), ("total_ms", 36.)] {
            assert_eq!(report[key], expected, "Wrong stage accounting for {key}");
        }
        assert_eq!(report["serial"], 42);
        assert_eq!(report["frame"], 12);
        assert_eq!(report["width"], 1920);
    }
    #[test]
    fn untraced_handoff_retains_no_timing_and_traced_handoff_retains_owned_core_timestamps() {
        let frame = || PreviewFrame { frame: 12, width: 2, height: 2, bgra: vec![0; 16] };
        let metadata = ScrubMetadata { request_serial: 42, requested_frame: 12, exact: true, original: true };
        let pending = PendingFrame::from(ScrubFrame { frame: frame(), metadata, trace: None });
        assert!(pending.trace.is_none());
        assert_eq!(pending.seek, Some(metadata));
        let base = Instant::now() - Duration::from_millis(20);
        let trace = ScrubTrace { queued_at: base, started_at: base, decoded_at: base, cached_ready_at: base };
        let pending = PendingFrame::from(ScrubFrame { frame: frame(), metadata, trace: Some(trace) });
        let (owned, received) = pending.trace.unwrap();
        assert_eq!(owned.queued_at, base);
        assert!(received >= owned.cached_ready_at);
        assert_eq!(pending.seek, Some(metadata));
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CpuFrameIdentity {
    frame: u64,
    seek: Option<core::ScrubMetadata>,
    size: (u32, u32),
    image_id: ImageId,
}
#[derive(Clone, Copy, Debug)]
struct SeekRefinement {
    quick_serial: u64,
    requested_at: Instant,
    target: u64,
    bounds: (u32, u32),
    size: (u32, u32),
    quick_painted: Option<Instant>,
    original: Option<(u64, Instant)>,
}
impl SeekRefinement {
    fn accepts(&self, frame: &core::ScrubFrame) -> bool {
        let metadata = frame.metadata;
        metadata.exact && metadata.requested_frame == self.target && frame.frame.frame == self.target
            && if let Some((serial, _)) = self.original {
                metadata.original && metadata.request_serial == serial
                    && (frame.frame.width, frame.frame.height) == self.size
            } else { !metadata.original && metadata.request_serial == self.quick_serial }
    }
    fn quick_is_painted(&self, painted: PaintedSeek, current: Option<CpuFrameIdentity>) -> bool {
        painted.at >= self.requested_at && current == Some(painted.identity)
            && painted.identity.frame == self.target
            && painted.identity.seek.is_some_and(|metadata| metadata.exact && !metadata.original
                && metadata.request_serial == self.quick_serial && metadata.requested_frame == self.target)
    }
}
#[derive(Clone, Copy)]
struct PaintedSeek {
    identity: CpuFrameIdentity,
    at: Instant,
}
#[cfg(test)]
mod seek_identity_tests {
    use super::{CpuFrameIdentity, PaintedSeek, SeekRefinement, core, fast_seek_bounds, fast_seek_size};
    use gpui::ImageId;
    use std::time::{Duration, Instant};
    #[test]
    fn replaced_cpu_images_do_not_alias_restarted_session_serials() {
        let first = CpuFrameIdentity { frame: 12, seek: Some(core::ScrubMetadata {
            request_serial: 1, requested_frame: 12, exact: true, original: false,
        }), size: (1280, 720), image_id: ImageId(10) };
        let replacement = CpuFrameIdentity { image_id: ImageId(11), ..first };
        assert_ne!(first, replacement, "A new CPU image must receive a new successful-paint receipt even when the seek session restarts its serial");
        assert_eq!(first.frame, replacement.frame, "Image ownership must not alter the source frame identity");
    }
    #[test]
    fn source_refinement_rejects_fast_pixels_wrong_serial_and_wrong_dimensions() {
        let now = Instant::now();
        let mut refinement = SeekRefinement { quick_serial: 3, requested_at: now, target: 12,
            bounds: (2560, 1440), size: (2560, 1440), quick_painted: None, original: None };
        let mut frame = core::ScrubFrame { frame: core::PreviewFrame { frame: 12, width: 1280, height: 720, bgra: Vec::new() },
            metadata: core::ScrubMetadata { request_serial: 3, requested_frame: 12, exact: true, original: false }, trace: None };
        assert!(refinement.accepts(&frame));
        frame.metadata.exact = false;
        assert!(!refinement.accepts(&frame));
        frame.metadata.exact = true;
        refinement.original = Some((4, now));
        assert!(!refinement.accepts(&frame), "An already buffered proxy must not overwrite a source refinement");
        frame.metadata.request_serial = 4;
        frame.frame.width = 2560; frame.frame.height = 1440;
        assert!(!refinement.accepts(&frame), "Matching geometry does not prove original-source provenance");
        frame.metadata.original = true;
        assert!(refinement.accepts(&frame));
        frame.frame.width = 1280;
        assert!(!refinement.accepts(&frame));
        frame.frame.width = 2560; frame.frame.frame = 11;
        assert!(!refinement.accepts(&frame), "A nearby frame cannot receive the released target identity");
    }
    #[test]
    fn source_refinement_requires_a_new_successful_paint_of_its_quick_image() {
        let now = Instant::now();
        let refinement = SeekRefinement { quick_serial: 1, requested_at: now, target: 12,
            bounds: (2560, 1440), size: (2560, 1440), quick_painted: None, original: None };
        let identity = CpuFrameIdentity { frame: 12, size: (1280, 720), image_id: ImageId(10),
            seek: Some(core::ScrubMetadata { request_serial: 1, requested_frame: 12, exact: true, original: false }) };
        assert!(!refinement.quick_is_painted(PaintedSeek { identity, at: now - Duration::from_millis(1) }, Some(identity)),
            "Recycled request serials cannot reuse an old retained image receipt");
        let painted = PaintedSeek { identity, at: now };
        assert!(refinement.quick_is_painted(painted, Some(identity)));
        assert!(!refinement.quick_is_painted(painted, Some(CpuFrameIdentity { image_id: ImageId(11), ..identity })));
        let original = CpuFrameIdentity { size: (2560, 1440), seek: identity.seek.map(|metadata| core::ScrubMetadata { original: true, ..metadata }), ..identity };
        assert_ne!(identity, original, "Source provenance and preview geometry belong to the actual painted image identity");
    }
    #[test]
    fn fast_seek_preserves_1080p_proxy_detail_without_promoting_all_drags_to_source_size() {
        let project = core::Project { width: 2560, height: 1440, ..Default::default() };
        assert_eq!(fast_seek_size(&project, (2560, 1440)), (1920, 1080));
        assert_eq!(fast_seek_size(&project, (1920, 1080)), (1920, 1080));
        assert_eq!(fast_seek_size(&project, (960, 540)), (960, 540));
        let viewport = (1742, 981);
        assert_eq!(fast_seek_bounds(viewport), viewport, "The core API must receive unfitted bounds");
        let expected = core::preview_size(&project, fast_seek_bounds(viewport));
        assert_eq!(fast_seek_size(&project, viewport), expected);
        assert_ne!(core::preview_size(&project, expected), expected, "Noncanonical viewport exposes a second-fit regression");
        assert_eq!(core::preview_size(&project, (2560, 1440)), (2560, 1440), "Settled source refinement retains full viewport resolution");
    }
    #[test]
    fn source_refinement_does_not_normalize_even_rounded_viewport_dimensions_twice() {
        let project = core::Project { width: 2560, height: 1440, ..Default::default() };
        let bounds = (2300, 1294);
        let expected = core::preview_size(&project, bounds);
        assert_ne!(core::preview_size(&project, expected), expected,
            "Fixture must expose even-rounding drift from repeated aspect fitting");
        let refinement = SeekRefinement { quick_serial: 1, requested_at: Instant::now(), target: 12,
            bounds, size: expected, quick_painted: None, original: Some((2, Instant::now())) };
        let delivered = core::preview_size(&project, refinement.bounds);
        let frame = core::ScrubFrame { frame: core::PreviewFrame { frame: 12,
            width: delivered.0, height: delivered.1, bgra: Vec::new() },
            metadata: core::ScrubMetadata { request_serial: 2, requested_frame: 12, exact: true, original: true }, trace: None };
        assert!(refinement.accepts(&frame), "An original frame normalized once by the core API must not be discarded");
    }
}
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

actions!(
    rcr_editor,
    [
        TogglePlay,
        SplitClip,
        DeleteClip,
        UndoEdit,
        RedoEdit,
        StepBack,
        StepForward,
        SecondBack,
        SecondForward,
        TrimStart,
        TrimEnd,
        ExportClip,
        ZoomTimelineIn,
        ZoomTimelineOut,
        FitTimeline,
        ToggleClipShow,
        PreviousRecording,
        NextRecording,
        ExitClipShow,
        ToggleViewerMute,
        SeekViewerStart,
        SeekViewerEnd
    ]
);
pub struct BackToLibrary;
pub struct ShareClip(pub PathBuf);
pub const MAX_LIBRARY_CHOICES: usize = 64;
fn adjacent_recording(choices: &[(PathBuf, String)], current: Option<&std::path::Path>, forward: bool) -> Option<usize> {
    let current = current?;
    let index = choices.iter().position(|(path, _)| path == current)?;
    if forward { index.checked_add(1).filter(|next| *next < choices.len()) }
    else { index.checked_sub(1) }
}
fn clip_show_seek_target(fraction: f32, duration: u64) -> u64 {
    (f64::from(fraction.clamp(0., 1.)) * duration.saturating_sub(1) as f64).round() as u64
}
#[cfg(test)]
mod clip_show_tests {
    use super::adjacent_recording;
    use std::path::PathBuf;

    #[test]
    fn clip_show_follows_library_order_and_stops_at_both_ends() {
        let choices = ["third.mkv", "first.mp4", "second.mkv"].into_iter()
            .map(|name| (PathBuf::from(name), name.to_owned())).collect::<Vec<_>>();
        let first = Some(choices[0].0.as_path());
        let middle = Some(choices[1].0.as_path());
        let last = Some(choices[2].0.as_path());
        assert_eq!(adjacent_recording(&choices, first, false), None);
        assert_eq!(adjacent_recording(&choices, first, true), Some(1));
        assert_eq!(adjacent_recording(&choices, middle, false), Some(0));
        assert_eq!(adjacent_recording(&choices, middle, true), Some(2));
        assert_eq!(adjacent_recording(&choices, last, true), None);
        assert_eq!(adjacent_recording(&choices, None, true), None);
        assert_eq!(adjacent_recording(&choices, Some(std::path::Path::new("missing.mp4")), true), None);
        assert_eq!(adjacent_recording(&[], first, true), None);
    }
    #[test]
    fn clip_show_seek_clamps_to_project_frames_without_timeline_zoom() {
        assert_eq!(super::clip_show_seek_target(0., 120), 0);
        assert_eq!(super::clip_show_seek_target(0.5, 121), 60);
        assert_eq!(super::clip_show_seek_target(1., 120), 119);
        assert_eq!(super::clip_show_seek_target(-2., 120), 0);
        assert_eq!(super::clip_show_seek_target(3., 120), 119);
        assert_eq!(super::clip_show_seek_target(1., 0), 0);
    }
}
impl EventEmitter<ShareClip> for EditorView {}
impl EventEmitter<BackToLibrary> for EditorView {}
struct ExportFailure {
    detail: String,
    partial: Option<PathBuf>,
}
const MAX_EXPORT_DETAIL_CHARS: usize = 16_384;
fn bounded_export_detail(detail: &str) -> String {
    let mut chars = detail.chars();
    let mut bounded: String = chars.by_ref().take(MAX_EXPORT_DETAIL_CHARS).collect();
    if chars.next().is_some() {
        bounded.push_str("\n\n[Additional export details omitted]");
    }
    bounded
}
impl From<anyhow::Error> for ExportFailure {
    fn from(error: anyhow::Error) -> Self {
        Self {
            partial: error
                .downcast_ref::<core::RetainedExport>()
                .map(|retained| retained.partial.clone()),
            detail: bounded_export_detail(&format!("{error:#}")),
        }
    }
}
enum Event {
    MediaCompleted(Box<Event>, MediaAdmission),
    LoadPhase(u64, &'static str, Instant),
    Resolved(u64, PathBuf, PathBuf),
    Restored(u64, PathBuf, Box<Editor>, bool),
    RestoreFailed(u64, u64, PathBuf, Box<Editor>, String),
    Added(u64, MediaAsset),
    Waveform(u64, PathBuf, u32, Arc<Vec<WaveBin>>),
    Thumbnails(u64, PathBuf, Vec<PreviewFrame>),
    Failed(u64, u64, String),
    EditFailed(u64, u64, String),
    Exported(u64, Result<PathBuf, ExportFailure>),
    ExportSize(PathBuf, Option<u64>),
    SaveFinished(PathBuf, u64, Result<(), String>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SaveStatus {
    #[default]
    Saving,
    Saved,
    Failed,
}

/// One in-flight revision per sidecar; a newer edit must receive its own ACK.
#[derive(Default)]
struct AutosaveState {
    revision: u64,
    in_flight: Option<u64>,
    status: SaveStatus,
    label: String,
}

fn save_label(title: &str) -> String {
    title
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect::<String>()
        .trim()
        .to_owned()
}
fn completed_export_source(source: &str) -> String {
    let source = save_label(source);
    if source.is_empty() { "Previous clip".into() } else { source }
}
#[cfg(test)]
mod export_receipt_tests {
    use super::completed_export_source;
    #[test]
    fn export_receipt_keeps_source_identity_bounded_and_single_line() {
        assert_eq!(completed_export_source("  Earlier clip\n\t 🎮  "), "Earlier clip 🎮");
        assert_eq!(completed_export_source("\n\t"), "Previous clip");
        assert_eq!(completed_export_source(&"ø".repeat(500)).chars().count(), 80);
    }
}

/// Autosave notices remain available after an unrelated media error is dismissed.
/// The flag controls whether the displayed notice offers the save retry action.
fn editor_notice(media: Option<&str>, autosave: Option<String>) -> Option<(String, bool)> {
    media
        .map(|message| (message.to_owned(), false))
        .or_else(|| autosave.map(|message| (message, true)))
}
impl AutosaveState {
    fn changed(&mut self) {
        self.revision += 1;
        self.status = SaveStatus::Saving;
    }
    fn can_submit(&self) -> bool {
        self.status == SaveStatus::Saving && self.in_flight.is_none()
    }
    /// Returns true only when the latest revision's visible outcome changed.
    fn finished(&mut self, revision: u64, success: bool) -> bool {
        if self.in_flight != Some(revision) {
            return false;
        }
        self.in_flight = None;
        if revision != self.revision {
            return false;
        }
        self.status = if success {
            SaveStatus::Saved
        } else {
            SaveStatus::Failed
        };
        true
    }
}
enum Task {
    Resolve(u64, PathBuf),
    Open(u64, PathBuf, PathBuf),
    Restore(u64, PathBuf, Box<Editor>),
    OpenOriginal(u64, PathBuf, Option<Project>),
    Add(u64, PathBuf),
    Waveform(u64, PathBuf, u32),
    Thumbnails(u64, MediaAsset),
}
// Count admitted work, not the persistent media thread. The admission travels
// with queued work and retires on a full/disconnected send or any worker exit.
struct MediaAdmission(Arc<AtomicUsize>);
impl Drop for MediaAdmission {
    fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); }
}
struct MediaTask {
    task: Task,
    _admission: MediaAdmission,
}
impl MediaTask {
    fn new(task: Task, pending: &Arc<AtomicUsize>) -> Self {
        pending.fetch_add(1, Ordering::AcqRel);
        Self { task, _admission: MediaAdmission(pending.clone()) }
    }
}
#[cfg(test)]
mod update_media_admission_tests {
    use super::{Event, MediaTask, Task};
    use std::{path::PathBuf, sync::{Arc, atomic::{AtomicUsize, Ordering}, mpsc}};

    #[test]
    fn admitted_media_stays_busy_after_worker_exit_until_ui_reply_is_retired() {
        let pending = Arc::new(AtomicUsize::new(0));
        let work = MediaTask::new(Task::Resolve(1, PathBuf::from("owned-fixture.mkv")), &pending);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let MediaTask { task: _, _admission } = work;
            let reply = Event::MediaCompleted(Box::new(Event::Resolved(1, PathBuf::new(), PathBuf::new())), _admission);
            assert!(tx.send(reply).is_ok());
        }).join().unwrap();
        assert_eq!(pending.load(Ordering::Acquire), 1, "Worker completion does not retire unapplied user intent");
        let reply = rx.try_recv().unwrap();
        assert_eq!(pending.load(Ordering::Acquire), 1);
        drop(reply);
        assert_eq!(pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn full_or_disconnected_media_channels_retire_only_their_owned_admissions() {
        let pending = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::sync_channel(1);
        assert!(tx.try_send(MediaTask::new(Task::Resolve(1, PathBuf::new()), &pending)).is_ok());
        let full = tx.try_send(MediaTask::new(Task::Resolve(2, PathBuf::new()), &pending));
        assert!(matches!(full, Err(mpsc::TrySendError::Full(_))));
        drop(full);
        assert_eq!(pending.load(Ordering::Acquire), 1);
        drop(rx);
        assert_eq!(pending.load(Ordering::Acquire), 0);
        let disconnected = tx.try_send(MediaTask::new(Task::Resolve(3, PathBuf::new()), &pending));
        assert!(matches!(disconnected, Err(mpsc::TrySendError::Disconnected(_))));
        drop(disconnected);
        assert_eq!(pending.load(Ordering::Acquire), 0);
    }
}
fn restore_event(
    g: u64,
    epoch: u64,
    path: PathBuf,
    mut editor: Box<Editor>,
    cancelled: impl Fn() -> bool,
) -> Event {
    match editor.validate_sources_cancellable(cancelled) {
        Ok(()) => Event::Restored(g, path, editor, false),
        Err(error) => Event::RestoreFailed(g, epoch, path, editor, format!("{error:#}")),
    }
}
impl Task {
    fn generation(&self) -> u64 {
        match self {
            Self::Resolve(g, ..)
            | Self::Open(g, ..)
            | Self::Restore(g, ..)
            | Self::OpenOriginal(g, ..)
            | Self::Add(g, ..)
            | Self::Waveform(g, ..)
            | Self::Thumbnails(g, ..) => *g,
        }
    }
}
#[derive(Clone, Copy)]
enum DragKind {
    Move,
    Left,
    Right,
    Volume,
    Reframe,
}
struct Drag {
    base: Project,
    id: Id,
    kind: DragKind,
    x: f32,
    width: f32,
    total: u64,
    span: u64,
    edit: Option<Edit>,
}
// Match the editor's rounded nanosecond conversion, including fractional-frame timestamps.
fn available_source_frames(nanoseconds: u64, fps: u32, speed: f64) -> u64 {
    let max_frames = u64::from(fps) * 86_400;
    let ns_for = |frames: u64| (frames as f64 / f64::from(fps) * speed * 1e9).round() as u64;
    let mut frames = ((((nanoseconds as f64 + 0.5) / 1e9 * f64::from(fps) / speed).floor()) as u64).min(max_frames);
    while frames > 0 && ns_for(frames) > nanoseconds { frames -= 1; }
    while frames < max_frames && ns_for(frames + 1) <= nanoseconds { frames += 1; }
    frames
}
// Bound linked edits by source availability and the existing neighbors on each track.
fn drag_frame_limits(project: &Project, selected: &core::Clip, kind: DragKind) -> (i64, i64) {
    let mut low = i128::MIN;
    let mut high = i128::MAX;
    for clip in project.clips.iter().filter(|clip| clip.group == selected.group) {
        let previous = project.clips.iter().filter(|other| other.track == clip.track && other.group != clip.group && other.end() <= clip.start)
            .map(core::Clip::end).max().unwrap_or(0);
        let next = project.clips.iter().filter(|other| other.track == clip.track && other.group != clip.group && other.start >= clip.end())
            .map(|other| other.start).min();
        match kind {
            DragKind::Left => {
                let restore = available_source_frames(clip.source_in_ns, project.fps, clip.speed);
                low = low.max(i128::from(previous.max(clip.start.saturating_sub(restore))) - i128::from(clip.start));
                high = high.min(i128::from(clip.duration) - 1);
            }
            DragKind::Right => {
                let available = match clip.content {
                    Content::Media { asset, .. } => project.assets.iter().find(|asset_item| asset_item.id == asset)
                        .map(|asset_item| available_source_frames(asset_item.duration_ns.saturating_sub(clip.source_in_ns), project.fps, clip.speed).max(1)),
                    _ => None,
                };
                low = low.max(1 - i128::from(clip.duration));
                if let Some(available) = available { high = high.min(i128::from(available) - i128::from(clip.duration)); }
                if let Some(next) = next { high = high.min(i128::from(next) - i128::from(clip.end())); }
            }
            DragKind::Move => {
                low = low.max(-i128::from(clip.start));
            }
            _ => {}
        }
    }
    (low.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
        high.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}
fn moved_group_overlaps(project: &Project, group: Id) -> bool {
    project.clips.iter().filter(|clip| clip.group == group).any(|clip|
        project.clips.iter().any(|other| other.group != group && other.track == clip.track
            && clip.start < other.end() && other.start < clip.end()))
}
fn same_trim_target(previous: Option<&Edit>, next: &Edit) -> bool {
    match (previous, next) {
        (Some(Edit::Sequence(QuickEdit::TrimStart { id: a, to: x })), Edit::Sequence(QuickEdit::TrimStart { id: b, to: y })) => a == b && x == y,
        (Some(Edit::Sequence(QuickEdit::TrimEnd { id: a, to: x })), Edit::Sequence(QuickEdit::TrimEnd { id: b, to: y })) => a == b && x == y,
        _ => false,
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Preset {
    Original,
    FullHd,
    Small,
}
impl Preset {
    fn name(self) -> &'static str {
        match self {
            Self::Original => "Original size",
            Self::FullHd => "1080p",
            Self::Small => "Small file",
        }
    }
    fn size(self, p: &Project) -> (u32, u32) {
        match self {
            Self::Original => (p.width, p.height),
            Self::FullHd => core::preview_size(
                p,
                if p.width < p.height {
                    (1080, 1920)
                } else {
                    (1920, 1080)
                },
            ),
            Self::Small => core::preview_size(
                p,
                if p.width < p.height {
                    (720, 1280)
                } else {
                    (1280, 720)
                },
            ),
        }
    }
    fn bitrate(self, p: &Project) -> u32 {
        match self {
            Self::Original => ((u64::from(p.width) * u64::from(p.height) * u64::from(p.fps)) / 9)
                .clamp(5_000_000, 80_000_000) as u32,
            Self::FullHd => 10_000_000,
            Self::Small => 3_500_000,
        }
    }
}
struct Tip(SharedString);
impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(0x26272b))
            .text_color(rgb(0xededef))
            .text_xs()
            .child(self.0.clone())
    }
}
fn button(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    tip: &'static str,
) -> Stateful<Div> {
    button_with_hover(id, text, tip, 0x2a2b30)
}
fn button_with_hover(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    tip: &'static str,
    hover_color: u32,
) -> Stateful<Div> {
    div()
        .id(id)
        .tab_index(0)
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .h(px(32.))
        .px_3()
        .rounded(px(8.))
        .bg(rgb(0x1f2024))
        .border_1()
        .border_color(rgba(0x00000000))
        .focus(|s| s.border_color(rgb(ACCENT)))
        .text_size(px(13.))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(hover_color)))
        .child(text.into())
        .tooltip(move |_, cx| cx.new(|_| Tip(tip.into())).into())
}
fn label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .child(text.into())
}
fn clock(frame: u64, fps: u32) -> String {
    let t = frame * 10 / u64::from(fps);
    format!("{}:{:02}.{}", t / 600, t / 10 % 60, t % 10)
}
fn image(frame: PreviewFrame) -> Option<Arc<RenderImage>> {
    // GPUI's Windows atlas consumes BGRA bytes.
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}
fn log_error(operation: &str, error: &str) {
    crate::diagnostics_io::log_editor_error(operation, error);
}
type TrimHandleBounds = HashMap<(Id, bool), Bounds<Pixels>>;

/// Inactive editors retain their bounded undo/redo stacks without serializing history.
/// Taking an entry makes it active; returning it updates its place in the LRU.
#[derive(Default)]
struct EditorSessions {
    entries: VecDeque<(PathBuf, Editor)>,
}
impl EditorSessions {
    fn insert(&mut self, path: PathBuf, editor: Editor) {
        let path = crate::preferences::lookup_key(&path);
        self.remove(&path);
        if self.entries.len() >= 8 {
            self.entries.pop_front();
        }
        self.entries.push_back((path, editor));
    }
    fn remove(&mut self, path: &std::path::Path) -> Option<Editor> {
        let path = crate::preferences::lookup_key(path);
        let index = self
            .entries
            .iter()
            .position(|(key, _)| key.as_path() == path.as_path())?;
        self.entries.remove(index).map(|(_, editor)| editor)
    }
}

#[derive(Default)]
pub struct ScrubSmoke {
    pub random: bool,
    targets: VecDeque<u64>,
    pending: Option<(u64, Instant)>,
    warmed: Option<Instant>,
    samples: Vec<f64>,
    started: bool,
    redundant_pointer_checked: bool,
}

#[path = "scrub_pressure_proof.rs"]
mod scrub_pressure_proof;
pub use scrub_pressure_proof::ScrubPressureSmoke;

#[cfg(windows)]
struct EditorGpuPreview {
    enabled: bool,
    environment_override: bool,
    adapter: Option<(u32, i32)>,
    directory: Option<PathBuf>,
    surface: Option<(core::GpuPlaybackOutput, WindowsGpuSurface)>,
    status: Option<core::GpuPlaybackStatus>,
    published: u64,
    failed: bool,
}

/// State for the explicitly invoked, owned-media native preview fixture.
#[derive(Clone, Copy, Default)]
pub struct GpuPlaybackTickTiming {
    pub requested_timer: Duration,
    /// Measured through a background timer continuation, before UI dispatch.
    pub timer_wait: Duration,
    pub foreground_dispatch: Duration,
    pub scene_construction: Duration,
    pub native_present: Duration,
}

#[derive(serde::Serialize)]
struct GpuTimingObservation {
    elapsed_ms: f64,
    observation_gap_ms: f64,
    requested_timer_ms: f64,
    timer_wait_ms: f64,
    foreground_dispatch_ms: f64,
    scene_construction_ms: f64,
    native_present_ms: f64,
    clock_frame: Option<u64>,
    clock_advance: Option<u64>,
    published_id: u64,
    published_advance: u64,
    submitted_id: u64,
    submitted_advance: u64,
}

#[derive(Default)]
pub struct GpuPlaybackSmoke {
    /// Sustained draw-submission/EOF proof; normal pause/fallback proof is default.
    pub until_eof: bool,
    started: Option<Instant>,
    phase: u8,
    since: Option<Instant>,
    frame: u64,
    position: u64,
    first_id: u64,
    duration_seconds: f64,
    original_project: Vec<u8>,
    initial_cpu_image: Option<Arc<RenderImage>>,
    initial_cpu_frame: Option<u64>,
    output_publication: Option<Arc<AtomicU64>>,
    observed_draws: u64,
    skipped_ids: u64,
    last_draw: Option<Instant>,
    draw_intervals_ms: Vec<f64>,
    draw_interval_ids: Vec<(u64, u64, f64)>,
    pending_timing: Option<GpuPlaybackTickTiming>,
    timings: Vec<GpuTimingObservation>,
    timing_overflow: u64,
    last_timing: Option<Instant>,
}
impl GpuPlaybackSmoke {
    /// Only explicitly enabled EOF fixtures retain samples; normal playback never calls this.
    pub fn record_tick(&mut self, timing: GpuPlaybackTickTiming) {
        if self.until_eof { self.pending_timing = Some(timing); }
    }
    fn observe_tick(&mut self, clock_frame: Option<u64>, published_id: u64, submitted_id: u64) {
        let Some(timing) = self.pending_timing.take() else { return; };
        if self.timings.len() == 900 {
            self.timing_overflow += 1;
            return;
        }
        if self.timings.is_empty() { self.timings.reserve_exact(900); }
        let now = Instant::now();
        let previous = self.timings.last();
        let ms = |duration: Duration| duration.as_secs_f64() * 1000.;
        let sample = GpuTimingObservation {
            elapsed_ms: self.started.map_or(0., |start| ms(now.duration_since(start))),
            observation_gap_ms: self.last_timing.map_or(0., |before| ms(now.duration_since(before))),
            requested_timer_ms: ms(timing.requested_timer),
            timer_wait_ms: ms(timing.timer_wait),
            foreground_dispatch_ms: ms(timing.foreground_dispatch),
            scene_construction_ms: ms(timing.scene_construction),
            native_present_ms: ms(timing.native_present),
            clock_frame,
            clock_advance: clock_frame.zip(previous.and_then(|sample| sample.clock_frame)).map(|(now, before)| now.saturating_sub(before)),
            published_id,
            published_advance: previous.map_or(0, |before| published_id.saturating_sub(before.published_id)),
            submitted_id,
            submitted_advance: previous.map_or(0, |before| submitted_id.saturating_sub(before.submitted_id)),
        };
        self.timings.push(sample);
        self.last_timing = Some(now);
    }
    fn timing_report(&self) -> serde_json::Value {
        let summary = |field: fn(&GpuTimingObservation) -> f64| {
            let mut values = self.timings.iter().map(field).collect::<Vec<_>>();
            values.sort_by(f64::total_cmp);
            let at = |q: f64| values.get((values.len().saturating_sub(1) as f64 * q).round() as usize).copied().unwrap_or(0.);
            serde_json::json!({"median":at(0.5),"p95":at(0.95),"max":at(1.)})
        };
        let mut gaps = self.timings.iter().collect::<Vec<_>>();
        gaps.sort_by(|a, b| b.observation_gap_ms.total_cmp(&a.observation_gap_ms));
        gaps.truncate(8);
        serde_json::json!({"samples":self.timings.len(),"discarded_after_cap":self.timing_overflow,"cap":900,
            "requested_timer_ms":summary(|s| s.requested_timer_ms),"actual_timer_wait_ms":summary(|s| s.timer_wait_ms),
            "foreground_dispatch_ms":summary(|s| s.foreground_dispatch_ms),"scene_construction_ms":summary(|s| s.scene_construction_ms),
            "native_present_ms":summary(|s| s.native_present_ms),"observation_gap_ms":summary(|s| s.observation_gap_ms),
            "largest_observation_gaps":gaps,"scope":"Explicit hidden-window fixture phases and post-present frame-ID observations; not monitor cadence"})
    }
}
#[cfg(windows)]
impl EditorGpuPreview {
    fn new() -> Self {
        // Configuration only: DLL existence/loading is checked on the video worker.
        let environment_override = std::env::var_os("RCR_GPU_PLAYBACK").as_deref()
            == Some(std::ffi::OsStr::new("1"));
        let directory = std::env::var_os("RCR_FFMPEG_LIB_DIR")
            .map(PathBuf::from).filter(|path| path.is_absolute());
        Self {
            enabled: environment_override,
            environment_override,
            adapter: None,
            directory,
            surface: None,
            status: None,
            published: 0,
            failed: false,
        }
    }
    fn reset_session(&mut self) {
        self.surface = None;
        self.status = None;
        self.published = 0;
        self.failed = false;
    }
}

pub struct EditorView {
    load_timing: HashMap<&'static str, Instant>,
    clip_show: bool,
    clip_show_bounds: Rc<RefCell<[Option<Bounds<Pixels>>; 4]>>,
    clip_show_seek_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    clip_show_seek_focus: FocusHandle,
    clip_show_seek_resume: bool,
    clip_show_volume: f32,
    clip_show_muted: bool,
    playback_details: playback_status::Details,
    shell_operation: u64,
    copy_operation: u64,
    ignore_preview_click: bool,
    model: Editor,
    recording: Option<PathBuf>,
    sidecar: Option<PathBuf>,
    title: String,
    focus: FocusHandle,
    selected: Option<Id>,
    playhead: u64,
    timeline_frames: u64,
    viewport: crate::timeline_view::TimelineViewport,
    follow_focus: FocusHandle,
    trim_bounds: Rc<RefCell<TrimHandleBounds>>,
    visible: bool,
    loading: bool,
    ready: bool,
    edit_recovery: bool,
    source_recovery: bool,
    focus_requested: bool,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    tasks: Option<mpsc::SyncSender<MediaTask>>,
    media_worker: Option<thread::JoinHandle<()>>,
    media_pending: Arc<AtomicUsize>,
    pending: VecDeque<Task>,
    generation: Arc<AtomicU64>,
    media_active: Arc<AtomicBool>,
    media_epoch: Arc<AtomicU64>,
    save_tx: Option<mpsc::SyncSender<(PathBuf, u64, Project)>>,
    save_worker: Option<thread::JoinHandle<()>>,
    pending_saves: HashMap<PathBuf, Project>,
    save_states: HashMap<PathBuf, AutosaveState>,
    autosave_notice_dismissed: bool,
    sessions: EditorSessions,
    restoring: HashSet<PathBuf>,
    save_due: Option<Instant>,
    waveforms: HashMap<(PathBuf, u32), Arc<Vec<WaveBin>>>,
    thumbnails: HashMap<PathBuf, Vec<Arc<RenderImage>>>,
    requested: HashSet<PathBuf>,
    retired: Vec<Arc<RenderImage>>,
    preview: Option<PlaybackSession>,
    #[cfg(windows)]
    gpu_preview: EditorGpuPreview,
    scrub: Option<ScrubSession>,
    scrub_dirty: bool,
    seek_refinement: Option<SeekRefinement>,
    seek_preparing: bool,
    scrubbing: bool,
    scrub_activation: Option<Subscription>,
    playing: bool,
    preview_output: PlaybackOutput,
    preview_due: Option<(Instant, bool)>,
    image: Option<Arc<RenderImage>>,
    // GPU submissions update shown_frame without replacing this CPU image.
    // Keep its identity attached to its pixels through seek/fallback transitions.
    cpu_image_frame: Option<CpuFrameIdentity>,
    cpu_image_timing: Option<CpuFrameTiming>,
    next_frame: Option<PendingFrame>,
    shown_frame: Option<u64>,
    painted_frame: Rc<Cell<Option<(u64, Instant)>>>,
    painted_seek: Rc<Cell<Option<PaintedSeek>>>,
    frame_waker: async_channel::Sender<()>,
    preview_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    strip_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    ruler_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    pixel_scale: f32,
    preview_size: (u32, u32),
    drag: Option<Drag>,
    audio_refresh: Option<Instant>,
    picker: bool,
    pending_dialogs: usize,
    library_choices: Vec<(PathBuf, String)>,
    menu_focus: FocusHandle,
    preset_menu: bool,
    preset: Preset,
    export: Option<Arc<JobProgress>>,
    export_cancelled: bool,
    export_partial: Option<PathBuf>,
    export_detail: Option<String>,
    export_details_open: bool,
    export_details_copied: bool,
    export_details_copy_operation: u64,
    export_details_copy_error: Option<String>,
    export_details_reveal_error: Option<String>,
    export_details_focus: FocusHandle,
    export_details_reveal_focus: FocusHandle,
    export_details_close_focus: FocusHandle,
    export_details_close_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    export_details_previous_focus: Option<FocusHandle>,
    export_details_scroll: ScrollHandle,
    export_worker: Option<thread::JoinHandle<()>>,
    export_source_title: String,
    export_total_us: f64,
    saved: Option<PathBuf>,
    saved_source_title: Option<String>,
    watch_share: watch_share::State,
    watch_fullscreen: watch_fullscreen::State,
    copied: bool,
    error: Option<String>,
}
impl EditorView {
    /// Pure, nonblocking activity query for the updater's restart gate. A live
    /// media/autosave thread alone does not imply an operation is active.
    pub fn update_blocker(&self) -> Option<&'static str> {
        if self.export.is_some() || self.export_worker.is_some() {
            Some("Wait for the video export to finish.")
        } else if !self.pending_saves.is_empty() || self.save_states.values().any(|state|
            state.in_flight.is_some() || state.status != SaveStatus::Saved) {
            Some("Wait for clip edits to save; retry any failed saves first.")
        } else if self.drag.is_some() || self.scrubbing {
            Some("Finish the current editor gesture first.")
        } else if !self.pending.is_empty() || self.media_pending.load(Ordering::Acquire) != 0
            || !self.restoring.is_empty() || (self.visible && self.loading) {
            Some("Wait for clip loading and media preparation to finish.")
        } else if self.scrub.as_ref().is_some_and(ScrubSession::seek_proxy_preparing) {
            Some("Wait for seek preparation to finish.")
        } else if self.pending_dialogs != 0 || self.picker {
            Some("Close the editor file picker first.")
        } else if self.playing {
            Some("Pause playback before restarting.")
        } else if self.copy_operation != 0 || self.shell_operation != 0 || self.export_details_copy_operation != 0 {
            Some("Wait for the editor file or clipboard operation to finish.")
        } else { None }
    }
    pub(crate) fn load_diagnostics(&self) -> serde_json::Value {
        let Some(started) = self.load_timing.get("requested") else { return serde_json::Value::Null; };
        let phases = self.load_timing.iter().map(|(name, at)| ((*name).to_owned(),
            serde_json::json!(at.saturating_duration_since(*started).as_secs_f64() * 1000.))).collect::<serde_json::Map<_, _>>();
        let seek = self.cpu_image_timing.map(|timing| {
            let ms = |a: Instant, b: Instant| a.saturating_duration_since(b).as_secs_f64() * 1000.;
            serde_json::json!({"queue_ms":ms(timing.core.started_at,timing.core.queued_at),
                "decode_ms":ms(timing.core.decoded_at,timing.core.started_at),
                "cache_ms":ms(timing.core.cached_ready_at,timing.core.decoded_at),
                "handoff_ms":ms(timing.received_at,timing.core.cached_ready_at),
                "bind_ms":ms(timing.bound_at,timing.received_at),
                "paint_ms":self.painted_seek.get().map(|painted| ms(painted.at,timing.bound_at))})
        });
        serde_json::json!({"phases_ms":phases,"generation":self.generation.load(Ordering::Relaxed),
            "playback":self.preview.as_ref().map(PlaybackSession::startup_diagnostics),
            "audio":self.preview.as_ref().map(PlaybackSession::audio_diagnostics),
            "seek":seek,
            "scrub":self.scrub.as_ref().map(|scrub| serde_json::json!({"native":scrub.native_decoder(),
                "proxy_enabled":scrub.proxy_preparation_enabled(), "proxy_preparing":scrub.seek_proxy_preparing(),
                "proxy_ready":scrub.seek_proxy_ready()})),
            "painted_ms":self.painted_frame.get().map(|(_, at)| at.saturating_duration_since(*started).as_secs_f64() * 1000.)})
    }
    fn viewer_gain(&self) -> f32 {
        if !self.clip_show { 1. } else if self.clip_show_muted { 0. } else { self.clip_show_volume }
    }
    fn apply_viewer_gain(&self) {
        if let Some(session) = &self.preview { session.set_monitor_gain(self.viewer_gain()); }
    }
    fn toggle_viewer_mute(&mut self, cx: &mut Context<Self>) {
        self.clip_show_muted = !self.clip_show_muted;
        self.apply_viewer_gain();
        cx.notify();
    }
    fn adjust_viewer_volume(&mut self, amount: f32, cx: &mut Context<Self>) {
        self.clip_show_volume = (self.clip_show_volume + amount).clamp(0., 1.);
        self.clip_show_muted = false;
        self.apply_viewer_gain();
        cx.notify();
    }
    pub(crate) fn snapshot_status(&self) -> String {
        format!("ready={}, loading={}, visible={}, image={}, queued_frame={}, source={:?}, error={:?}",
            self.ready, self.loading, self.visible, self.image.is_some(), self.next_frame.is_some(), self.recording, self.error)
    }
    pub(crate) fn open_for_clip_show(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.clip_show = true;
        self.open_media(path, cx);
    }
    pub(crate) fn seed_clip_show_snapshot(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.clip_show {
            self.clip_show = true;
            self.pause();
            cx.notify();
        }
        if self.ready && (self.playing || self.preview_due.is_some_and(|(_, play)| play)) {
            self.request_preview(false);
        }
        self.ready && self.image.is_some()
    }
    pub(crate) fn verify_clip_show_snapshot(&self, window: &Window) -> anyhow::Result<()> {
        let bounds = self.clip_show_bounds.borrow().iter().flatten().copied().collect::<Vec<_>>();
        anyhow::ensure!(bounds.len() == 4, "Clip show header did not paint all four controls");
        for area in bounds.iter() {
            anyhow::ensure!(area.left() >= px(0.) && area.right() <= window.viewport_size().width
                && area.top() >= px(0.) && area.bottom() <= px(56.), "Clip show control overflowed its header: {area:?}");
        }
        for pair in bounds.windows(2) {
            anyhow::ensure!(pair[0].right() <= pair[1].left(), "Clip show controls overlap");
        }
        let preview = self.preview_bounds.get().ok_or_else(|| anyhow::anyhow!("Clip show preview was not painted"))?;
        anyhow::ensure!(preview.size.height > window.viewport_size().height * 0.6, "Clip show did not give most height to playback");
        let seek = self.clip_show_seek_bounds.get().ok_or_else(|| anyhow::anyhow!("Clip show seek bar was not painted"))?;
        anyhow::ensure!(seek.size.width > window.viewport_size().width * 0.8
            && seek.left() >= px(0.) && seek.right() <= window.viewport_size().width,
            "Clip show seek bar overflowed or did not fill the player width");
        self.verify_watch_share_layout(window)?;
        Ok(())
    }
    fn navigate_recording(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(index) = adjacent_recording(&self.library_choices, self.recording.as_deref(), forward) else { return; };
        let (path, title) = self.library_choices[index].clone();
        self.open_media(path, cx);
        self.set_title(title);
    }
    fn toggle_clip_show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.clip_show && !self.prepare_watch_mode_exit(window, cx) { return; }
        self.finish_scrubbing(cx);
        self.finish_drag(cx);
        self.clip_show = !self.clip_show;
        // Retire the old mode's seek worker: watching cancels any edit proxy,
        // and returning to editing restores its normal preparation policy.
        self.scrub.take();
        self.seek_refinement = None;
        self.apply_viewer_gain();
        self.picker = false;
        self.preset_menu = false;
        self.focus.focus(window);
        if self.clip_show {
            if self.ready && !self.playing { self.toggle_play(cx); }
        } else {
            self.queue_media();
            if self.ready && !self.playing { self.request_preview(false); }
        }
        cx.notify();
    }
    /// Applies to the next session; never restarts active audio or edits the project.
    pub fn set_gpu_preview_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            let effective = enabled || self.gpu_preview.environment_override;
            if self.gpu_preview.enabled != effective {
                self.gpu_preview.enabled = effective;
                cx.notify();
            }
        }
        #[cfg(not(windows))]
        { let _ = (enabled, cx); }
    }
    fn clear_export_details(&mut self) {
        self.export_detail = None;
        self.export_details_open = false;
        self.export_details_copied = false;
        self.export_details_copy_operation = 0;
        self.export_details_copy_error = None;
        self.export_details_reveal_error = None;
        self.export_details_scroll.set_offset(point(px(0.), px(0.)));
        self.export_details_previous_focus = None;
    }
    fn show_export_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.playback_details.open = false;
        self.export_details_previous_focus = window.focused(cx);
        self.export_details_open = true;
        self.export_details_copied = false;
        self.preset_menu = false;
        self.picker = false;
        if self.export_details_copy_operation == 0 { self.export_details_focus.focus(window); }
        else { self.export_details_close_focus.focus(window); }
        cx.notify();
    }
    #[cfg(windows)]
    fn copy_export_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if self.export_details_copy_operation != 0 { return; }
        let Some(detail) = self.export_detail.clone() else { return; };
        let owner = HasWindowHandle::window_handle(window).ok().and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()), _ => None,
        });
        let Some(owner) = owner else {
            self.export_details_copy_error = Some("Couldn’t copy details: the editor window is unavailable. Try again.".into());
            cx.notify();
            return;
        };
        let session = self.shell_identity();
        self.export_details_copy_error = None;
        self.export_details_copied = false;
        let expected = detail.clone();
        match recordings::begin_shell_action(recordings::ShellAction::CopyText { owner, text: detail }, PathBuf::new(), cx,
            move |this, operation, _, result, cx| {
                this.finish_export_details_copy(operation, &session, &expected, result);
                cx.notify();
            }) {
            Ok(operation) => {
                self.export_details_copy_operation = operation;
                self.export_details_close_focus.focus(window);
            }
            Err(error) => self.export_details_copy_error = Some(error),
        }
        cx.notify();
    }
    fn finish_export_details_copy(&mut self, operation: u64, session: &(u64, bool, Option<PathBuf>), expected: &str, result: Result<(), String>) {
        if self.export_details_copy_operation != operation { return; }
        self.export_details_copy_operation = 0;
        if self.visible && self.export_details_open && &self.shell_identity() == session
            && self.export_detail.as_deref() == Some(expected) {
            match result {
                Ok(()) => { self.export_details_copied = true; self.export_details_copy_error = None; }
                Err(error) => self.export_details_copy_error = Some(format!("Couldn’t copy details: {}. Try again.", error.chars().take(1_024).collect::<String>())),
            }
        }
    }
    fn close_export_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.export_details_open = false;
        if let Some(focus) = self.export_details_previous_focus.take() {
            focus.focus(window);
        } else {
            self.focus.focus(window);
        }
        cx.notify();
    }
    fn export_details_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let detail = self.export_detail.clone().unwrap_or_default();
        let close_bounds = self.export_details_close_bounds.clone();
        div().id("export-details-modal").absolute().inset_0().occlude()
            .bg(rgba(0x000000a8)).flex().items_center().justify_center().px_4()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if key == "escape" {
                    this.close_export_details(window, cx);
                } else if key == "tab" {
                    let mut stops = Vec::with_capacity(3);
                    if this.export_partial.is_some() && this.shell_operation == 0 { stops.push(&this.export_details_reveal_focus); }
                    if this.export_details_copy_operation == 0 { stops.push(&this.export_details_focus); }
                    stops.push(&this.export_details_close_focus);
                    let current = stops.iter().position(|focus| focus.is_focused(window));
                    let next = if event.keystroke.modifiers.shift {
                        current.map_or(stops.len() - 1, |index| (index + stops.len() - 1) % stops.len())
                    } else { current.map_or(0, |index| (index + 1) % stops.len()) };
                    stops[next].focus(window);
                } else if matches!(key, "up" | "down" | "pageup" | "pagedown" | "home" | "end") {
                    let mut offset = this.export_details_scroll.offset();
                    offset.y = match key {
                        "up" => offset.y + px(32.),
                        "down" => offset.y - px(32.),
                        "pageup" => offset.y + px(200.),
                        "pagedown" => offset.y - px(200.),
                        "home" => px(0.),
                        _ => px(-1_000_000.),
                    };
                    this.export_details_scroll.set_offset(offset);
                    cx.notify();
                } else if matches!(key, "enter" | "space") {
                    // Leave native focused-button activation intact.
                    return;
                }
                window.prevent_default();
                cx.stop_propagation();
            }))
            .on_key_down(|_, _, cx| cx.stop_propagation())
            .child(div().w(px(620.)).max_w_full().h(px(560.)).max_h(relative(0.9)).min_h_0().p_5().rounded_lg()
                .bg(rgb(0x17181b)).border_1().border_color(rgb(LINE)).shadow_lg()
                .flex().flex_col().gap_3()
                .child(div().flex_shrink_0().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child("Export details"))
                .child(div().id("export-details-body").flex_1().min_h_0().min_w_0().flex().flex_col().gap_3()
                    .overflow_y_scroll().track_scroll(&self.export_details_scroll)
                .child(div().flex_shrink_0().text_sm().text_color(rgb(theme::MUTED))
                    .child("The export reported this reason. Copy details to keep the diagnostic text."))
                .when_some(self.export_partial.as_ref(), |s, path| s.child(
                    div().flex_shrink_0().p_3().rounded_md().bg(rgb(theme::BG)).flex().flex_col().gap_1()
                        .child(div().text_sm().child("An incomplete export was kept. It may not play correctly. Your original recording is unchanged."))
                        .child(div().text_size(px(11.)).text_color(rgb(MUTED)).whitespace_normal().child(path.display().to_string()))))
                .child(div().id("export-diagnostic-text").flex_shrink_0()
                    .p_3().rounded_md().bg(rgb(theme::BG)).text_size(px(12.)).font_family("Consolas")
                    .children(detail.lines().map(|line| div().min_w_0().whitespace_normal().child(if line.is_empty() { " " } else { line }.to_owned()))))
                .when_some(self.export_details_copy_error.clone(), |s, error| s.child(
                    div().flex_shrink_0().text_sm().text_color(rgb(0xffb0b0)).child(error)))
                .when_some(self.export_details_reveal_error.clone(), |s, error| s.child(
                    div().flex_shrink_0().text_sm().text_color(rgb(0xffb0b0)).child(error))))
                .child(div().flex_shrink_0().text_size(px(11.)).text_color(rgb(theme::MUTED)).child("↑ ↓ / Page Up / Page Down to scroll"))
                .child(div().flex_shrink_0().flex().flex_wrap().items_center().justify_end().gap_2()
                    .when_some(self.export_partial.clone(), |s, path| s.child(
                        button("reveal-incomplete-export", if self.shell_operation != 0 { "Opening…" } else { "Show incomplete file" }, "Find the retained incomplete export in File Explorer")
                            .track_focus(&self.export_details_reveal_focus)
                            .when(self.shell_operation != 0, |s| s.opacity(0.5))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if this.shell_operation != 0 { return; }
                                this.reveal_output(path.clone(), cx);
                                if this.shell_operation != 0 { this.export_details_close_focus.focus(window); }
                            }))))
                    .child(button("copy-export-details", if self.export_details_copy_operation != 0 { "Copying…" } else if self.export_details_copied {
                        if cfg!(windows) { "Copied" } else { "Copy requested" }
                    } else { "Copy details" }, "Copy export diagnostics")
                        .track_focus(&self.export_details_focus)
                        .when(self.export_details_copy_operation != 0, |s| s.opacity(0.5))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.export_details_copy_operation != 0 { return; }
                            #[cfg(windows)]
                            this.copy_export_details(window, cx);
                            #[cfg(not(windows))]
                            if let Some(detail) = &this.export_detail {
                                let _ = window;
                                cx.write_to_clipboard(ClipboardItem::new_string(detail.clone()));
                                this.export_details_copied = true;
                                cx.notify();
                            }
                        })))
                    .child(button("close-export-details", "Close · Esc", "Close export details")
                        .relative()
                        .child(canvas(move |area, _, _| close_bounds.set(Some(area)), |_, _, _, _| {}).absolute().inset_0())
                        .track_focus(&self.export_details_close_focus)
                        .on_click(cx.listener(|this, _, window, cx| this.close_export_details(window, cx))))))
    }
    pub fn seed_export_failure_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.export_partial = Some(PathBuf::from("C:\\Captures\\exports\\review clip.mp4.partial"));
        self.export_detail = Some(concat!(
            "Export could not be completed.\n\n",
            "Encoder initialization failed: the requested video encoder is unavailable.\n\n",
            "Requested encoder: av1_nvenc\nOutput: C:\\Captures\\exports\\review clip.mp4\n\n",
            "No completed export was published. This is a synthetic diagnostic preview; no encoder was started.",
        ).into());
        self.show_export_details(window, cx);
    }
    pub(crate) fn shell_identity(&self) -> (u64, bool, Option<PathBuf>) {
        (self.generation.load(Ordering::Relaxed), self.visible, self.recording.clone())
    }
    #[cfg(windows)]
    fn copy_saved_file(&mut self, path: PathBuf, window: &Window, cx: &mut Context<Self>) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if self.copy_operation != 0 { return; }
        let owner = HasWindowHandle::window_handle(window).ok().and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()), _ => None,
        });
        let Some(owner) = owner else {
            self.error = Some("Could not copy the clip: the editor window is unavailable.".into());
            cx.notify();
            return;
        };
        let session = self.shell_identity();
        let previous_error = self.error.clone();
        match recordings::begin_shell_action(recordings::ShellAction::CopyFile { owner }, path, cx,
            move |this, operation, path, result, cx| {
                if this.copy_operation != operation { return; }
                this.copy_operation = 0;
                if this.visible && this.shell_identity() == session && this.saved.as_deref() == Some(path) {
                    match result {
                        Ok(()) => this.copied = true,
                        Err(error) => {
                            log_error("Copy", &error);
                            if this.error == previous_error { this.error = Some(format!("Couldn’t copy the clip: {error}. Try again.")); }
                        }
                    }
                }
                cx.notify();
            }) {
            Ok(operation) => { self.copy_operation = operation; self.copied = false; }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
    fn reveal_output(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let session = self.shell_identity();
        let previous_error = self.error.clone();
        let details_action = self.export_details_open && self.export_partial.as_ref() == Some(&path);
        if details_action { self.export_details_reveal_error = None; }
        match recordings::begin_shell_action(recordings::ShellAction::Reveal, path, cx, move |this, operation, path, result, cx| {
            if this.shell_operation != operation { return; }
            this.shell_operation = 0;
            if this.visible && this.shell_identity() == session && (this.saved.as_deref() == Some(path) || this.export_partial.as_deref() == Some(path))
                && let Err(error) = result {
                if details_action && this.export_details_open && this.export_partial.as_deref() == Some(path) {
                    this.export_details_reveal_error = Some(error.chars().take(1_024).collect());
                }
                if this.error == previous_error { this.error = Some(error); }
            }
            cx.notify();
        }) {
            Ok(operation) => self.shell_operation = operation,
            Err(error) => {
                if details_action { self.export_details_reveal_error = Some(error.clone()); }
                self.error = Some(error);
            }
        }
        cx.notify();
    }
    pub fn set_title(&mut self, title: String) {
        self.title = title;
        // Keep outstanding save failures tied to the title actually shown.
        if let Some(path) = &self.sidecar
            && let Some(state) = self.save_states.get_mut(path)
        {
            state.label = save_label(&self.title);
        }
    }
    pub fn set_library_choices(&mut self, choices: Vec<(PathBuf, String)>) -> bool {
        let choices = choices
            .into_iter()
            .filter(|(path, _)| recordings::is_supported(path) && !recordings::is_image(path))
            .collect::<Vec<_>>();
        if self.library_choices == choices {
            return false;
        }
        self.library_choices = choices;
        true
    }
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (frame_waker, frame_notifications) = async_channel::bounded(1);
        cx.spawn(async move |view, cx| {
            while frame_notifications.recv().await.is_ok() {
                if view.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let (tx, rx) = mpsc::channel();
        let (task_tx, task_rx) = mpsc::sync_channel::<MediaTask>(8);
        let media_pending = Arc::new(AtomicUsize::new(0));
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = generation.clone();
        let media_active = Arc::new(AtomicBool::new(false));
        let worker_active = media_active.clone();
        let media_epoch = Arc::new(AtomicU64::new(0));
        let worker_epoch = media_epoch.clone();
        let events = tx.clone();
        let media_worker = thread::Builder::new()
            .name("clip-media".into())
            .spawn(move || {
                for MediaTask { task, _admission } in task_rx {
                    let g = task.generation();
                    let epoch = worker_epoch.load(Ordering::Acquire);
                    let owns_history = matches!(&task, Task::Restore(..));
                    if !owns_history
                        && (g != worker_generation.load(Ordering::Relaxed)
                            || !worker_active.load(Ordering::Relaxed))
                    {
                        continue;
                    }
                    let cancelled = || {
                        g != worker_generation.load(Ordering::Relaxed)
                            || !worker_active.load(Ordering::Relaxed)
                            || epoch != worker_epoch.load(Ordering::Acquire)
                    };
                    let phase = match &task { Task::Resolve(..) => Some("resolve_worker_started"),
                        Task::Open(..) | Task::OpenOriginal(..) => Some("open_worker_started"),
                        Task::Restore(..) => Some("restore_worker_started"), _ => None };
                    if let Some(phase) = phase { let _ = events.send(Event::LoadPhase(g, phase, Instant::now())); }
                    let result: anyhow::Result<Event> = (|| match task {
                        Task::Resolve(g, path) => {
                            // Resolution also works for offline paths, allowing their
                            // cached session/sidecar to reach the source recovery UI.
                            let path = path.canonicalize().unwrap_or(path);
                            let sidecar = recordings::edit_path_for_open(&path);
                            Ok(Event::Resolved(g, path, sidecar))
                        }
                        Task::Restore(g, path, editor) => {
                            Ok(restore_event(g, epoch, path, editor, cancelled))
                        }
                        Task::Open(g, path, saved) => {
                            let project = if saved.exists() {
                                match Project::load(&saved) {
                                    Ok(project) => project,
                                    Err(error) => {
                                        return Ok(Event::EditFailed(g, epoch, format!("{error:#}")));
                                    }
                                }
                            } else {
                                let mut e = Editor::default();
                                let _ = events.send(Event::LoadPhase(g, "probe_started", Instant::now()));
                                e.execute(Edit::Sequence(QuickEdit::Add(
                                    core::probe_cancellable(&path, || {
                                        g != worker_generation.load(Ordering::Relaxed)
                                            || !worker_active.load(Ordering::Relaxed)
                                    })?,
                                )))?;
                                let _ = events.send(Event::LoadPhase(g, "probe_finished", Instant::now()));
                                e.project
                            };
                            Ok(restore_event(
                                g,
                                epoch,
                                path,
                                Box::new(Editor::new(project)),
                                cancelled,
                            ))
                        }
                        Task::OpenOriginal(g, path, pending) => {
                            recordings::backup_edits(&path)?;
                            if let Some(project) = pending {
                                recordings::backup_pending_edits(&path, &project)?;
                            }
                            let mut editor = Editor::default();
                            editor.execute(Edit::Sequence(QuickEdit::Add(
                                core::probe_cancellable(&path, || {
                                    g != worker_generation.load(Ordering::Relaxed)
                                        || !worker_active.load(Ordering::Relaxed)
                                })?,
                            )))?;
                            Ok(Event::Restored(
                                g,
                                path,
                                Box::new(Editor::new(editor.project)),
                                true,
                            ))
                        }
                        Task::Add(g, path) => Ok(Event::Added(
                            g,
                            core::probe_cancellable(&path, || {
                                g != worker_generation.load(Ordering::Relaxed)
                                    || !worker_active.load(Ordering::Relaxed)
                            })?,
                        )),
                        Task::Waveform(g, path, s) => Ok(Event::Waveform(
                            g,
                            path.clone(),
                            s,
                            Arc::new(core::waveform_cancellable(&path, s, || {
                                g != worker_generation.load(Ordering::Relaxed)
                                    || !worker_active.load(Ordering::Relaxed)
                            })?),
                        )),
                        Task::Thumbnails(g, a) => Ok(Event::Thumbnails(
                            g,
                            a.path.clone(),
                            core::thumbnails_cancellable(&a, || {
                                g != worker_generation.load(Ordering::Relaxed)
                                    || !worker_active.load(Ordering::Relaxed)
                            })?,
                        )),
                    })();
                    if phase.is_some() { let _ = events.send(Event::LoadPhase(g, "media_worker_finished", Instant::now())); }
                    let returns_history =
                        matches!(&result, Ok(Event::Restored(..) | Event::RestoreFailed(..)));
                    if !returns_history
                        && (g != worker_generation.load(Ordering::Relaxed)
                            || !worker_active.load(Ordering::Relaxed)
                            || epoch != worker_epoch.load(Ordering::Acquire))
                    {
                        continue;
                    }
                    if events
                        .send(Event::MediaCompleted(Box::new(result.unwrap_or_else(|e| Event::Failed(g, epoch, format!("{e:#}")))), _admission))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("start clip media worker");
        let (save_tx, save_rx) = mpsc::sync_channel::<(PathBuf, u64, Project)>(4);
        let save_events = tx.clone();
        let save_worker = thread::Builder::new()
            .name("clip-autosave".into())
            .spawn(move || {
                while let Ok((path, revision, project)) = save_rx.recv() {
                    let result = project.save(&path).map_err(|error| format!("{error:#}"));
                    if let Err(detail) = &result {
                        log_error("Autosave", detail);
                    }
                    // The receiver remains alive until Drop joins this worker.
                    let _ = save_events.send(Event::SaveFinished(path, revision, result));
                }
            })
            .expect("start clip autosave worker");
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(8))
                    .await;
                if view.update(cx, |v, cx| v.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            load_timing: HashMap::new(),
            clip_show: false,
            clip_show_bounds: Rc::new(RefCell::new([None; 4])),
            clip_show_seek_bounds: Rc::new(Cell::new(None)),
            clip_show_seek_focus: cx.focus_handle().tab_stop(true),
            clip_show_seek_resume: false,
            clip_show_volume: 1.,
            clip_show_muted: false,
            playback_details: playback_status::Details::new(cx),
            ignore_preview_click: false,
            model: Editor::default(),
            recording: None,
            sidecar: None,
            title: "Choose a recording".into(),
            focus: cx.focus_handle(),
            selected: None,
            playhead: 0,
            timeline_frames: 0,
            viewport: Default::default(),
            follow_focus: cx.focus_handle().tab_stop(true),
            trim_bounds: Rc::new(RefCell::new(HashMap::new())),
            visible: false,
            loading: false,
            ready: false,
            edit_recovery: false,
            source_recovery: false,
            focus_requested: false,
            tx,
            rx,
            tasks: Some(task_tx),
            media_worker: Some(media_worker),
            media_pending,
            pending: VecDeque::new(),
            generation,
            media_active,
            media_epoch,
            save_tx: Some(save_tx),
            save_worker: Some(save_worker),
            pending_saves: HashMap::new(),
            save_states: HashMap::new(),
            autosave_notice_dismissed: false,
            sessions: EditorSessions::default(),
            restoring: HashSet::new(),
            save_due: None,
            waveforms: HashMap::new(),
            thumbnails: HashMap::new(),
            requested: HashSet::new(),
            retired: Vec::new(),
            preview: None,
            #[cfg(windows)]
            gpu_preview: EditorGpuPreview::new(),
            scrub: None,
            scrub_dirty: false,
            seek_refinement: None,
            seek_preparing: false,
            scrubbing: false,
            scrub_activation: None,
            playing: false,
            preview_output: PlaybackOutput::Device,
            preview_due: None,
            image: None,
            cpu_image_frame: None,
            cpu_image_timing: None,
            next_frame: None,
            shown_frame: None,
            painted_frame: Rc::new(Cell::new(None)),
            painted_seek: Rc::new(Cell::new(None)),
            frame_waker,
            preview_bounds: Rc::new(Cell::new(None)),
            strip_bounds: Rc::new(Cell::new(None)),
            ruler_bounds: Rc::new(Cell::new(None)),
            pixel_scale: 1.,
            preview_size: (1280, 720),
            drag: None,
            audio_refresh: None,
            picker: false,
            pending_dialogs: 0,
            library_choices: Vec::new(),
            menu_focus: cx.focus_handle().tab_stop(true),
            preset_menu: false,
            preset: Preset::Original,
            export: None,
            export_cancelled: false,
            export_partial: None,
            export_detail: None,
            export_details_open: false,
            export_details_copied: false,
            export_details_copy_operation: 0,
            export_details_copy_error: None,
            export_details_reveal_error: None,
            export_details_focus: cx.focus_handle().tab_stop(true),
            export_details_reveal_focus: cx.focus_handle().tab_stop(true),
            export_details_close_focus: cx.focus_handle().tab_stop(true),
            export_details_close_bounds: Rc::new(Cell::new(None)),
            export_details_previous_focus: None,
            export_details_scroll: ScrollHandle::new(),
            export_worker: None,
            export_source_title: String::new(),
            export_total_us: 0.,
            saved: None,
            saved_source_title: None,
            watch_share: Default::default(),
            watch_fullscreen: Default::default(),
            shell_operation: 0,
            copy_operation: 0,
            copied: false,
            error: None,
        }
    }
    pub fn open_media(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.saved.is_some() && self.saved_source_title.is_none() {
            self.saved_source_title = Some(completed_export_source(&self.title));
        }
        self.load_timing.clear();
        self.load_timing.insert("requested", Instant::now());
        self.clip_show_seek_resume = false;
        self.visible = true;
        self.playback_details.open = false;
        self.playback_details.reset_session();
        self.playback_details.clear_snapshot();
        self.remember();
        self.pause();
        // Move history once when leaving a clip, rather than clone the entire undo
        // stack on every edit. Disk autosaves continue to contain only Project.
        if self.ready
            && let Some(previous) = &self.recording
        {
            let editor = std::mem::take(&mut self.model);
            self.sessions.insert(previous.clone(), editor);
        }
        self.preview.take();
        #[cfg(windows)]
        self.gpu_preview.reset_session();
        self.drag = None;
        self.scrub.take();
        self.scrubbing = false;
        self.shown_frame = None;
        self.cpu_image_frame = None;
        self.cpu_image_timing = None;
        self.painted_frame.set(None);
        self.painted_seek.set(None);
        let g = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.media_active.store(true, Ordering::Relaxed);
        self.park_pending_restores();
        self.waveforms.clear();
        for (_, images) in self.thumbnails.drain() {
            self.retired.extend(images);
        }
        self.requested.clear();
        self.title = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.sidecar = None;
        self.recording = Some(path.clone());
        self.ready = false;
        self.edit_recovery = false;
        self.source_recovery = false;
        self.focus_requested = true;
        self.selected = None;
        self.playhead = 0;
        self.error = None;
        self.export_cancelled = false;
        self.clear_export_details();
        self.picker = false;
        self.preset_menu = false;
        if let Some(old) = self.image.take() {
            self.retired.push(old);
        }
        self.model = Editor::default();
        self.timeline_frames = 0;
        self.viewport.fit();
        self.loading = true;
        self.pending.push_back(Task::Resolve(g, path));
        cx.notify();
    }
    pub fn smoke_ready(&self) -> bool {
        self.ready
            && self.image.is_some()
            && self.error.is_none()
            && self
                .strip_bounds
                .get()
                .is_some_and(|b| b.size.width > px(300.))
            && self
                .preview_bounds
                .get()
                .is_some_and(|b| b.size.width > px(300.) && b.size.height > px(100.))
    }
    /// Exercises real GPUI drawing and session fallback without saving any edits.
    pub fn smoke_gpu_playback(
        view: Entity<Self>,
        probe: &mut GpuPlaybackSmoke,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<bool> {
        #[cfg(not(windows))]
        {
            let _ = (view, probe, window, cx);
            anyhow::bail!("GPU playback proof requires Windows");
        }
        #[cfg(windows)]
        {
            use anyhow::{Context, ensure};
            let started = *probe.started.get_or_insert_with(Instant::now);
            if probe.until_eof {
                let this = view.read(cx);
                let clock = this.preview.as_ref().map(PlaybackSession::position);
                let published = this.preview.as_ref().and_then(PlaybackSession::gpu_stats).map_or(0, |stats| stats.last_published_id);
                let submitted = this.gpu_preview.surface.as_ref().map_or(0, |(_, surface)| surface.submitted_frame_id());
                probe.observe_tick(clock, published, submitted);
            }
            let deadline = if probe.until_eof {
                Duration::from_secs_f64(if probe.duration_seconds > 0. {
                    probe.duration_seconds + 5.
                } else {
                    20.
                })
            } else {
                Duration::from_secs(18)
            };
            if started.elapsed() >= deadline {
                let this = view.read(cx);
                anyhow::bail!(
                    "GPU desktop fixture timed out: phase={}, ready={}, loading={}, visible={}, playing={}, image={}, cpu_frame={:?}, shown={:?}, error={:?}, playback={:?}, surface={:?}",
                    probe.phase,
                    this.ready,
                    this.loading,
                    this.visible,
                    this.playing,
                    this.image.is_some(),
                    this.cpu_image_frame.map(|identity| identity.frame),
                    this.shown_frame,
                    this.error,
                    this.preview.as_ref().map(|job| (
                        job.ready(),
                        job.position(),
                        job.finished(),
                        job.gpu_status(),
                        job.gpu_stats()
                    )),
                    this.gpu_preview.surface.as_ref().map(|(_, surface)| (
                        surface.submitted_frame_id(),
                        surface.draw_count(),
                        surface.error()
                    ))
                );
            }
            ensure!(
                started.elapsed() < Duration::from_secs(25),
                "GPU desktop preview fixture timed out at phase {}",
                probe.phase
            );
            if probe.phase == 0 {
                let this = view.read(cx);
                ensure!(
                    this.error.is_none(),
                    "GPU fixture could not open its source: {:?}",
                    this.error
                );
                ensure!(
                    started.elapsed() < Duration::from_secs(10)
                        || (this.ready && this.preview_bounds.get().is_some()),
                    "GPU fixture initial editor not ready: ready={}, loading={}, bounds={:?}, pending={}, worker_finished={:?}, recording={:?}",
                    this.ready,
                    this.loading,
                    this.preview_bounds.get(),
                    this.pending.len(),
                    this.media_worker
                        .as_ref()
                        .map(|worker| worker.is_finished()),
                    this.recording
                );
                if !this.ready || this.preview_bounds.get().is_none() || this.image.is_none() {
                    return Ok(false);
                }
                ensure!(
                    this.gpu_preview.enabled,
                    "Enable GPU playback in Settings or set RCR_GPU_PLAYBACK=1"
                );
                ensure!(
                    this.gpu_preview.adapter.is_some(),
                    "Presentation adapter LUID unavailable"
                );
                ensure!(
                    this.model.project.duration() > u64::from(this.model.project.fps) * 3,
                    "GPU fixture requires at least three seconds of owned video"
                );
                if probe.until_eof {
                    let frames = this.model.project.duration();
                    probe.duration_seconds = frames as f64 / f64::from(this.model.project.fps);
                    ensure!(
                        (300..=900).contains(&frames) && probe.duration_seconds <= 15.,
                        "EOF fixture requires 300..900 frames and at most 15 seconds"
                    );
                    probe.original_project = serde_json::to_vec(&this.model.project)?;
                    probe.initial_cpu_image = this.image.clone();
                    probe.initial_cpu_frame = this.cpu_image_frame.map(|identity| identity.frame);
                }
                view.update(cx, |this, cx| {
                    this.pause();
                    this.preview.take();
                    this.scrub.take();
                    this.audio_refresh = None;
                    this.playhead = 0;
                    // Only the session clone is muted. Model, undo and autosave stay intact.
                    let muted: Vec<_> = this.model.project.tracks.iter().map(|t| t.muted).collect();
                    for track in &mut this.model.project.tracks {
                        if track.kind == TrackKind::Audio {
                            track.muted = true;
                        }
                    }
                    this.start_preview(true);
                    for (track, muted) in this.model.project.tracks.iter_mut().zip(muted) {
                        track.muted = muted;
                    }
                    this.focus.focus(window);
                    cx.notify();
                });
                probe.phase = 1;
                return Ok(false);
            }
            let this = view.read(cx);
            let job = this
                .preview
                .as_ref()
                .context("GPU fixture lost playback session")?;
            ensure!(
                this.error.is_none(),
                "GPU fixture editor error: {:?}",
                this.error
            );
            let status = job.gpu_status();
            if probe.phase < 6 {
                ensure!(
                    !matches!(
                        status,
                        Some(
                            core::GpuPlaybackStatus::Fallback(_)
                                | core::GpuPlaybackStatus::Failed(_)
                        )
                    ),
                    "Expected GPU playback, received {status:?}"
                );
            }
            let submitted = this
                .gpu_preview
                .surface
                .as_ref()
                .map_or(0, |(_, surface)| surface.submitted_frame_id());
            if probe.until_eof {
                ensure!(
                    submitted >= probe.frame,
                    "GPU draw-submission IDs moved backwards"
                );
                ensure!(
                    submitted <= this.model.project.duration(),
                    "GPU submission exceeded the project duration"
                );
                ensure!(
                    submitted == 0 || submitted - 1 <= job.position(),
                    "GPU submitted a future clock frame"
                );
                ensure!(
                    this.cpu_image_frame.map(|identity| identity.frame) == probe.initial_cpu_frame
                        && this
                            .image
                            .as_ref()
                            .zip(probe.initial_cpu_image.as_ref())
                            .is_some_and(|(now, before)| Arc::ptr_eq(now, before)),
                    "Sustained GPU playback replaced the CPU image"
                );
                if submitted > 0 {
                    let (output, surface) = this
                        .gpu_preview
                        .surface
                        .as_ref()
                        .context("Submitted GPU image disappeared")?;
                    ensure!(
                        !this.gpu_preview.failed && surface.error().is_none(),
                        "GPU presentation failed before EOF"
                    );
                    if let Some(previous) = &probe.output_publication {
                        ensure!(
                            Arc::ptr_eq(previous, &output.published_frame),
                            "GPU output identity changed during sustained playback"
                        );
                    } else {
                        probe.output_publication = Some(output.published_frame.clone());
                    }
                }
                if submitted > probe.frame {
                    let at = Instant::now();
                    if let Some(previous) = probe.last_draw {
                        ensure!(
                            probe.draw_intervals_ms.len() < 900,
                            "Draw observation budget exceeded"
                        );
                        probe
                            .draw_intervals_ms
                            .push(at.duration_since(previous).as_secs_f64() * 1000.);
                        probe.draw_interval_ids.push((probe.frame, submitted,
                            at.duration_since(previous).as_secs_f64() * 1000.));
                    } else {
                        probe.first_id = submitted;
                    }
                    probe.skipped_ids += submitted - probe.frame - 1;
                    probe.observed_draws += 1;
                    probe.frame = submitted;
                    probe.last_draw = Some(at);
                }
                if matches!(status, Some(core::GpuPlaybackStatus::Finished))
                    && job.finished()
                    && submitted == this.model.project.duration()
                {
                    if probe.since.is_none() {
                        probe.since = Some(Instant::now());
                        return Ok(false);
                    }
                    if probe
                        .since
                        .is_some_and(|at| at.elapsed() >= Duration::from_millis(100))
                    {
                        ensure!(
                            probe.observed_draws >= 300,
                            "Only {} distinct GPU draw submissions observed; require at least 300",
                            probe.observed_draws
                        );
                        ensure!(
                            serde_json::to_vec(&this.model.project)? == probe.original_project,
                            "GPU fixture changed project edits"
                        );
                        let stats = job
                            .gpu_stats()
                            .context("Missing sustained GPU statistics")?;
                        ensure!(
                            stats.max_future_frames <= 3 && stats.max_inflight_reads <= 3,
                            "GPU retained-frame bounds exceeded"
                        );
                        let draws = this
                            .gpu_preview
                            .surface
                            .as_ref()
                            .context("Final GPU surface disappeared")?
                            .1
                            .draw_count();
                        let mut intervals = probe.draw_intervals_ms.clone();
                        intervals.sort_by(f64::total_cmp);
                        let percentile = |fraction: f64| {
                            intervals
                                .get(
                                    ((intervals.len().saturating_sub(1)) as f64 * fraction).round()
                                        as usize,
                                )
                                .copied()
                                .unwrap_or(0.)
                        };
                        let mut largest_intervals = probe.draw_interval_ids.clone();
                        largest_intervals.sort_by(|a, b| b.2.total_cmp(&a.2));
                        largest_intervals.truncate(8);
                        let mut steady_intervals = probe.draw_interval_ids.iter()
                            .filter(|(from, _, _)| *from >= u64::from(this.model.project.fps))
                            .map(|(_, _, interval)| *interval).collect::<Vec<_>>();
                        steady_intervals.sort_by(f64::total_cmp);
                        let steady = |q: f64| steady_intervals.get(
                            (steady_intervals.len().saturating_sub(1) as f64 * q).round() as usize)
                            .copied().unwrap_or(0.);
                        println!(
                            "GPU playback smoke passed: {}",
                            serde_json::json!({"gpu_desktop_preview":"passed", "mode":"until_eof", "first_submitted_id":probe.first_id, "final_submitted_id":submitted, "project_frames":this.model.project.duration(), "observed_draw_submissions":probe.observed_draws, "skipped_submission_ids":probe.skipped_ids, "draw_commands_including_redraws":draws, "draw_observation_interval_ms":{"median":percentile(0.5),"p95":percentile(0.95),"max":intervals.last().copied().unwrap_or(0.)}, "largest_draw_intervals_from_to_ms":largest_intervals,"draw_intervals_after_first_second_ms":{"samples":steady_intervals.len(),"median":steady(0.5),"p95":steady(0.95),"max":steady(1.)},"audio_diagnostics":job.audio_diagnostics(),"timing_scope":"UI observations of GPU draw submissions; not monitor scanout", "published":stats.published, "decoded":stats.decoded, "dropped":stats.dropped, "pool_size":stats.pool_size, "max_future_frames":stats.max_future_frames, "max_inflight_reads":stats.max_inflight_reads,"gpu_busy_retries":stats.busy_skips,"gpu_busy_release_notifications":stats.busy_release_notifications,"gpu_queue_full_retries":stats.queue_full_skips,"gpu_busy_wait_ms":stats.busy_wait_ns as f64/1e6,"gpu_queue_full_wait_ms":stats.queue_full_wait_ns as f64/1e6,"gpu_clock_wait_ms":stats.clock_wait_ns as f64/1e6, "final_retained_ms":probe.since.map_or(0., |at| at.elapsed().as_secs_f64()*1000.), "cpu_replacements":0,"fixture_edits":false,"pixel_readback":false,"elapsed_ms":started.elapsed().as_secs_f64()*1000.})
                        );
                        println!("GPU playback phase timings: {}", probe.timing_report());
                        return Ok(true);
                    }
                } else {
                    ensure!(
                        probe.since.is_none(),
                        "Final GPU frame or EOF status was not retained"
                    );
                }
                return Ok(false);
            }
            match probe.phase {
                1 if submitted > 0 => {
                    probe.first_id = submitted;
                    probe.frame = submitted;
                    probe.phase = 2;
                }
                2 if submitted >= probe.frame + 6 => {
                    window.dispatch_action(Box::new(TogglePlay), cx);
                    probe.since = Some(Instant::now());
                    probe.phase = 3;
                }
                3 if probe
                    .since
                    .is_some_and(|at| at.elapsed() >= Duration::from_millis(100)) =>
                {
                    probe.frame = submitted;
                    probe.position = job.position();
                    probe.since = Some(Instant::now());
                    probe.phase = 4;
                }
                4 if probe
                    .since
                    .is_some_and(|at| at.elapsed() >= Duration::from_millis(180)) =>
                {
                    ensure!(submitted == probe.frame, "Paused GPU preview advanced");
                    ensure!(
                        this.cpu_image_frame
                            .is_some_and(|identity| identity.frame + 1 < submitted),
                        "Fixture must retain an older CPU image behind the GPU preview"
                    );
                    ensure!(
                        this.shown_frame == submitted.checked_sub(1),
                        "GPU draw identity was lost"
                    );
                    ensure!(
                        job.position().abs_diff(probe.position) <= 1,
                        "Paused playback clock advanced"
                    );
                    window.dispatch_action(Box::new(TogglePlay), cx);
                    probe.phase = 5;
                }
                5 if submitted >= probe.frame + 4 => {
                    probe.position = job.position();
                    job.disable_gpu();
                    probe.phase = 6;
                }
                6 if this.gpu_preview.surface.is_none()
                    && job.gpu_output().is_none()
                    && this.image.is_some()
                    && this
                        .shown_frame
                        .is_some_and(|frame| frame >= probe.position) =>
                {
                    ensure!(
                        matches!(status, Some(core::GpuPlaybackStatus::Fallback(_))),
                        "CPU replacement must follow explicit GPU fallback"
                    );
                    ensure!(
                        job.position() >= probe.position,
                        "Fallback reset the playback clock"
                    );
                    let stats = job.gpu_stats().context("Missing GPU statistics")?;
                    let final_frame = this.shown_frame;
                    ensure!(
                        this.cpu_image_frame.map(|identity| identity.frame) == final_frame,
                        "CPU fallback pixels retained a GPU frame identity"
                    );
                    ensure!(
                        this.painted_frame
                            .get()
                            .is_some_and(|(frame, _)| Some(frame) == final_frame),
                        "CPU replacement was not submitted for drawing"
                    );
                    view.update(cx, |this, cx| {
                        this.pause();
                        cx.notify();
                    });
                    println!(
                        "GPU playback smoke passed: {}",
                        serde_json::json!({"gpu_desktop_preview":"passed", "first_submitted_id":probe.first_id,"paused_submitted_id":probe.frame,"cpu_replacement_frame":final_frame,"published":stats.published,"pool_size":stats.pool_size,"max_future_frames":stats.max_future_frames,"max_inflight_reads":stats.max_inflight_reads,"gpu_busy_retries":stats.busy_skips,"gpu_busy_release_notifications":stats.busy_release_notifications,"gpu_queue_full_retries":stats.queue_full_skips,"gpu_busy_wait_ms":stats.busy_wait_ns as f64/1e6,"gpu_queue_full_wait_ms":stats.queue_full_wait_ns as f64/1e6,"gpu_clock_wait_ms":stats.clock_wait_ns as f64/1e6,"elapsed_ms":started.elapsed().as_secs_f64()*1000.,"fixture_edits":false,"pixel_readback":false})
                    );
                    return Ok(true);
                }
                _ => {}
            }
            Ok(false)
        }
    }
    /// Owned-window visual fixture only; no source is opened or edits queued.
    pub fn snapshot_source_recovery(&mut self, cx: &mut Context<Self>) {
        self.visible = true;
        self.title = "Screen recording · source review".into();
        self.recording = Some(PathBuf::from("source-review-proof.mkv"));
        self.source_recovery = true;
        self.edit_recovery = true;
        self.error = Some("A source file changed. Your edits are preserved. Open the original to review the current recording; the old edits will be kept in a recovery copy.".into());
        cx.notify();
    }
    pub fn dismiss_snapshot_notice(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }
    /// Pointer-to-render latency, using the real GPUI event/poll/paint path.
    pub fn smoke_scrub(
        view: Entity<Self>,
        probe: &mut ScrubSmoke,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<bool> {
        use anyhow::{Context, ensure};
        let send =
            |target: u64, down: bool, window: &mut Window, cx: &mut App| -> anyhow::Result<()> {
                let editor = view.read(cx);
                let bounds = editor
                    .ruler_bounds
                    .get()
                    .context("Seek ruler not painted")?;
                let x = bounds.left()
                    + bounds.size.width
                        * ((target as f32 + 0.25) / editor.timeline_extent() as f32);
                let position = point(x, bounds.center().y);
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: if down { None } else { Some(MouseButton::Left) },
                        modifiers: Default::default(),
                    }),
                    cx,
                );
                if down {
                    window.dispatch_event(
                        PlatformInput::MouseDown(MouseDownEvent {
                            position,
                            button: MouseButton::Left,
                            click_count: 1,
                            first_mouse: false,
                            modifiers: Default::default(),
                        }),
                        cx,
                    );
                }
                Ok(())
            };
        if !probe.started {
            if probe.random
                && !view
                    .read(cx)
                    .scrub
                    .as_ref()
                    .is_some_and(|s| s.seek_proxy_ready())
            {
                return Ok(false);
            }
            let duration = view.read(cx).model.project.duration();
            ensure!(
                duration > 100,
                "Scrub smoke needs at least two seconds of video"
            );
            let middle = duration / 2;
            probe.targets = if probe.random {
                (0..61)
                    .map(|i| (duration - 1) * ((i * 23) % 61) / 61)
                    .chain((0..120).map(|i| (duration - 1) * i / 120))
                    .chain((0..120).rev().map(|i| (duration - 1) * i / 120))
                    .collect()
            } else {
                (0..24)
                    .rev()
                    .chain(0..45)
                    .chain((0..24).rev())
                    .map(|offset| middle - 24 + offset)
                    .collect()
            };
            send(middle, true, window, cx)?;
            probe.pending = Some((view.read(cx).playhead, Instant::now()));
            probe.started = true;
            return Ok(false);
        }
        if let Some((target, started)) = probe.pending {
            ensure!(
                started.elapsed() < Duration::from_secs(5),
                "Scrub frame never reached GPUI render"
            );
            let Some((painted, painted_at)) = view.read(cx).painted_frame.get() else {
                return Ok(false);
            };
            if painted != target {
                return Ok(false);
            }
            ensure!(painted_at >= started, "Seek fixture observed a stale paint");
            if !probe.redundant_pointer_checked {
                let this = view.read(cx);
                let strip = this.strip_bounds.get().context("Seek strip disappeared")?;
                let ruler = this.ruler_bounds.get().context("Seek ruler disappeared")?;
                let viewport = this.timeline_window();
                ensure!(this.scrubbing && this.scrub.is_some() && !this.scrub_dirty, "Repeated-pointer check requires an active configured scrub drag");
                let marker = vec![3, 7, 11, 255];
                let marker_identity = marker.as_ptr() as usize;
                let previous = view.update(cx, |this, _| this.next_frame.replace(PreviewFrame { frame: target, width: 1, height: 1, bgra: marker }.into()));
                for offset in [0.15, 0.25, 0.35, 0.45, 0.55, 0.65] {
                    let fraction = (target.saturating_sub(viewport.start) as f64 + offset) / viewport.span as f64;
                    window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent {
                        position: point(strip.left() + strip.size.width * fraction as f32, ruler.center().y),
                        pressed_button: Some(MouseButton::Left), modifiers: Default::default(),
                    }), cx);
                    ensure!(view.read(cx).playhead == target && view.read(cx).next_frame.as_ref().is_some_and(|frame| frame.frame.bgra.as_ptr() as usize == marker_identity), "Subframe pointer motion discarded a ready seek frame");
                }
                // A new down is an explicit retry even when the integer frame matches.
                let position = point(strip.left() + strip.size.width * ((target.saturating_sub(viewport.start) as f64 + 0.25) / viewport.span as f64) as f32, ruler.center().y);
                window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { position, button: MouseButton::Left, click_count: 1, first_mouse: false, modifiers: Default::default() }), cx);
                ensure!(view.read(cx).next_frame.is_none(), "Explicit same-frame pointer down did not retry seeking");
                // The owned marker is never painted; restore the real pending result.
                view.update(cx, |this, _| { this.next_frame = previous; });
                probe.redundant_pointer_checked = true;
            }
            probe.pending = None;
            if probe.warmed.is_none() {
                probe.warmed = Some(Instant::now());
            } else {
                probe.samples.push(
                    painted_at
                        .checked_duration_since(started)
                        .unwrap_or_default()
                        .as_secs_f64()
                        * 1000.,
                );
            }
        }
        if probe
            .warmed
            .is_some_and(|at| at.elapsed() < Duration::from_millis(400))
        {
            return Ok(false);
        }
        while probe
            .targets
            .front()
            .is_some_and(|at| *at == view.read(cx).playhead)
        {
            probe.targets.pop_front();
        }
        if let Some(target) = probe.targets.pop_front() {
            let started = Instant::now();
            let click = probe.random && probe.samples.len() < 61;
            send(
                target,
                click || (probe.random && probe.samples.len() == 61),
                window,
                cx,
            )?;
            ensure!(
                view.read(cx).playhead.abs_diff(target) <= 1,
                "Pointer seek did not follow its target"
            );
            if click {
                let position = view.read(cx).ruler_bounds.get().unwrap().center();
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        position,
                        button: MouseButton::Left,
                        click_count: 1,
                        modifiers: Default::default(),
                    }),
                    cx,
                );
            }
            probe.pending = Some((view.read(cx).playhead, started));
            return Ok(false);
        }
        let bounds = view
            .read(cx)
            .ruler_bounds
            .get()
            .context("Seek ruler disappeared")?;
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                position: bounds.center(),
                button: MouseButton::Left,
                click_count: 1,
                modifiers: Default::default(),
            }),
            cx,
        );
        probe.samples.sort_by(f64::total_cmp);
        let median = probe.samples[probe.samples.len() / 2];
        let p95 = probe.samples[probe.samples.len() * 95 / 100];
        let max = probe.samples.last().copied().unwrap_or_default();
        println!(
            "Drag render smoke: samples={}, median_ms={median:.2}, p95_ms={p95:.2}, max_ms={max:.2}",
            probe.samples.len()
        );
        ensure!(
            p95 < 50. && max < 100.,
            "GPUI continuous dragging remains slow"
        );
        ensure!(
            view.read(cx).scrub.as_ref().is_some_and(
                |s| s.native_decoder() && (s.hardware_decoder() || s.seek_proxy_ready())
            ),
            "Native scrub path was not active"
        );
        Ok(true)
    }
    /// Dispatch through painted handles; preserve the original edit after the check.
    pub fn smoke_trim(view: Entity<Self>, window: &mut Window, cx: &mut App) -> anyhow::Result<()> {
        use anyhow::{Context, ensure};
        fn drag_handle(
            view: &Entity<EditorView>,
            window: &mut Window,
            cx: &mut App,
            left: bool,
            fraction: f32,
            check_position: bool,
        ) -> anyhow::Result<()> {
            window.draw(cx).clear();
            let editor = view.read(cx);
            let strip = editor
                .strip_bounds
                .get()
                .context("Timeline has not been laid out")?;
            let clip = core::pieces(&editor.model.project)[0];
            let id = clip.id;
            let handle = *editor
                .trim_bounds
                .borrow()
                .get(&(id, left))
                .context("Handle has not been painted")?;
            let start = handle.center();
            let tolerance =
                (f32::from(strip.size.width) / editor.timeline_extent() as f32).max(1.) + 2.;
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: start,
                    pressed_button: None,
                    modifiers: Default::default(),
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: start,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            ensure!(
                view.read(cx).drag.as_ref().is_some_and(|d| if left {
                    matches!(d.kind, DragKind::Left)
                } else {
                    matches!(d.kind, DragKind::Right)
                }),
                "Trim handle did not receive mouse down (left={left})"
            );
            for step in 1..=3 {
                let position =
                    start + point(strip.size.width * fraction * step as f32 / 3., px(0.));
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Default::default(),
                    }),
                    cx,
                );
                let editor = view.read(cx);
                let drag = editor.drag.as_ref().context("Trim drag disappeared")?;
                let snapped = (f64::from((f32::from(position.x) - drag.x) / drag.width) * drag.span as f64).round();
                let stable_project = editor.model.project.clips.as_ptr() as usize;
                let base_x = drag.x;
                let frame_width = f64::from(drag.width) / drag.span.max(1) as f64;
                if drag.edit.is_some() {
                    for offset in [0.05, 0.15, 0.25] {
                        let repeated = point(px(base_x + ((snapped + offset) * frame_width) as f32), position.y);
                        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: repeated, pressed_button: Some(MouseButton::Left), modifiers: Default::default() }), cx);
                        ensure!(view.read(cx).model.project.clips.as_ptr() as usize == stable_project, "Same quantized trim target rebuilt the project");
                    }
                }
                window.draw(cx).clear();
                if check_position {
                    let actual = view
                        .read(cx)
                        .trim_bounds
                        .borrow()
                        .get(&(id, left))
                        .context("Trim handle disappeared")?
                        .center();
                    ensure!(
                        f32::from(actual.x - position.x).abs() <= tolerance,
                        "Painted handle did not follow pointer (left={left}, pointer={position:?}, handle={actual:?})"
                    );
                }
            }
            let end = start + point(strip.size.width * fraction, px(0.));
            let before_release = view.read(cx).trim_bounds.borrow().get(&(id, left)).copied()
                .context("Trim handle missing before release")?.center();
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: end,
                    modifiers: Default::default(),
                    click_count: 1,
                }),
                cx,
            );
            ensure!(
                view.read(cx).drag.is_none(),
                "Trim was not committed on release"
            );
            window.draw(cx).clear();
            let after_release = view.read(cx).trim_bounds.borrow().get(&(id, left)).copied()
                .context("Trim handle missing after release")?.center();
            ensure!(f32::from(after_release.x - before_release.x).abs() <= tolerance,
                "Releasing trim moved the painted edge: before={before_release:?}, after={after_release:?}");
            Ok(())
        }
        let untouched = view.read(cx).model.project.clone();
        let mut cases = vec![untouched.clone()];
        let first = core::pieces(&untouched)[0];
        if core::pieces(&untouched).len() == 1 && first.duration >= 8 {
            let mut split = Editor::new(untouched.clone());
            split.execute(Edit::Sequence(QuickEdit::Split {
                id: first.id,
                at: first.start + first.duration / 2,
            }))?;
            cases.push(split.project);
        }
        let result = (|| {
            for original in cases {
                view.update(cx, |editor, cx| {
                    editor.model = Editor::new(original.clone());
                    editor.timeline_frames = original.duration();
                    editor.changed(false, cx);
                });
                let piece_count = core::pieces(&original).len();
                for left in [true, false] {
                    let before = core::pieces(&original)[0].duration;
                    let extent = view.read(cx).timeline_extent();
                    let fraction = before as f32 / extent as f32 * 0.15;
                    drag_handle(
                        &view,
                        window,
                        cx,
                        left,
                        if left { fraction } else { -fraction },
                        true,
                    )?;
                    let shortened = view.read(cx).model.project.clone();
                    let after = core::pieces(&shortened)[0].duration;
                    ensure!(
                        after < before,
                        "Dragging the edge did not shorten the clip (left={left})"
                    );
                    ensure!(
                        view.read(cx).timeline_extent() == extent,
                        "Trim changed the timeline scale"
                    );
                    for clip in &shortened.clips {
                        if clip.group == core::pieces(&shortened)[0].group {
                            ensure!(clip.duration == after, "Audio did not follow video trim");
                            let source_clip = original.clips.iter().find(|before| before.id == clip.id).context("Missing linked source")?;
                            ensure!(clip.start == source_clip.start + if left { before - after } else { 0 }, "Linked trim changed timeline placement incorrectly");
                            let source_delta = ((before - after) as f64 / f64::from(original.fps) * source_clip.speed * 1e9).round() as u64;
                            ensure!(clip.source_in_ns == source_clip.source_in_ns + if left { source_delta } else { 0 }, "Linked source trim did not match timeline trim");
                        } else {
                            ensure!(serde_json::to_value(clip)? == serde_json::to_value(original.clips.iter().find(|before| before.id == clip.id).context("Missing neighbor")?)?,
                                "Trimming moved or changed a neighboring clip");
                        }
                    }
                    if left {
                        window.draw(cx).clear();
                        let editor = view.read(cx);
                        let clip = core::pieces(&editor.model.project)[0];
                        let id = clip.id;
                        let group = clip.group;
                        let start_frame = clip.start;
                        let strip = editor.strip_bounds.get().context("Missing strip for body move")?;
                        let handles = editor.trim_bounds.borrow();
                        let left_handle = handles.get(&(id, true)).context("Missing left handle")?.center();
                        let right_handle = handles.get(&(id, false)).context("Missing right handle")?.center();
                        let from = point((left_handle.x + right_handle.x) / 2., left_handle.y);
                        let to = from - point(strip.size.width * start_frame as f32 / editor.timeline_window().span as f32, px(0.));
                        drop(handles);
                        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: from, pressed_button: None, modifiers: Default::default() }), cx);
                        window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { position: from, button: MouseButton::Left, modifiers: Default::default(), click_count: 1, first_mouse: false }), cx);
                        ensure!(view.read(cx).drag.as_ref().is_some_and(|drag| matches!(drag.kind, DragKind::Move)), "Clip body did not begin manual movement");
                        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: to, pressed_button: Some(MouseButton::Left), modifiers: Default::default() }), cx);
                        window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { position: to, button: MouseButton::Left, modifiers: Default::default(), click_count: 1 }), cx);
                        let moved = view.read(cx).model.project.clone();
                        ensure!(moved.clips.iter().filter(|clip| clip.group == group).all(|clip| clip.start == 0), "Manual body drag did not move linked clip to zero");
                        for clip in &moved.clips {
                            let previous = shortened.clips.iter().find(|before| before.id == clip.id).context("Missing pre-move clip")?;
                            let mut expected = previous.clone();
                            if expected.group == group { expected.start = 0; }
                            ensure!(serde_json::to_value(clip)? == serde_json::to_value(expected)?, "Body movement changed source, duration or neighboring clips");
                        }
                        view.update(cx, |editor, cx| editor.undo(cx));
                        ensure!(serde_json::to_value(&view.read(cx).model.project)? == serde_json::to_value(&shortened)?, "Undo manual body movement failed");
                        view.update(cx, |editor, cx| editor.redo(cx));
                        ensure!(serde_json::to_value(&view.read(cx).model.project)? == serde_json::to_value(&moved)?, "Redo manual body movement failed");
                        view.update(cx, |editor, cx| editor.undo(cx));
                    }
                    let restore = (before - after) as f32 / extent as f32;
                    drag_handle(
                        &view,
                        window,
                        cx,
                        left,
                        if left { -restore } else { restore },
                        false,
                    )?;
                    ensure!(
                        serde_json::to_value(&view.read(cx).model.project)?
                            == serde_json::to_value(&original)?,
                        "Outward drag did not restore source (left={left}); expected first clip={}, actual first clip={}",
                        serde_json::to_string(&core::pieces(&original)[0])?,
                        serde_json::to_string(&core::pieces(&view.read(cx).model.project)[0])?
                    );
                    view.update(cx, |editor, cx| editor.undo(cx));
                    ensure!(
                        serde_json::to_value(&view.read(cx).model.project)?
                            == serde_json::to_value(&shortened)?,
                        "Undo did not restore shortened clip"
                    );
                    view.update(cx, |editor, cx| editor.redo(cx));
                    ensure!(
                        serde_json::to_value(&view.read(cx).model.project)?
                            == serde_json::to_value(&original)?,
                        "Redo did not restore source"
                    );
                    view.update(cx, |editor, cx| {
                        editor.undo(cx);
                        editor.undo(cx);
                    });
                    println!(
                        "Trim smoke: pieces={piece_count}, left={left}, frames={before}->{after}->{before}, handle follows pointer, audio linked, undo/redo passed"
                    );
                }
            }
            window.draw(cx).clear();
            let bounds = view
                .read(cx)
                .ruler_bounds
                .get()
                .context("Seek ruler not painted")?;
            let from = point(bounds.left() + bounds.size.width * 0.2, bounds.center().y);
            let to = point(bounds.left() + bounds.size.width * 0.65, bounds.center().y);
            let before = serde_json::to_value(&view.read(cx).model.project)?;
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: from,
                    pressed_button: None,
                    modifiers: Default::default(),
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    position: from,
                    button: MouseButton::Left,
                    click_count: 1,
                    first_mouse: false,
                    modifiers: Default::default(),
                }),
                cx,
            );
            ensure!(view.read(cx).scrubbing, "Ruler did not start scrubbing");
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: to,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Default::default(),
                }),
                cx,
            );
            let editor = view.read(cx);
            let expected = ((editor.timeline_extent() as f32 * 0.65) as u64)
                .min(editor.model.project.duration() - 1);
            ensure!(
                editor.playhead.abs_diff(expected) <= 1,
                "Ruler did not follow pointer"
            );
            ensure!(
                editor.scrub.is_some() && editor.preview.is_none() && editor.preview_due.is_none(),
                "Scrubbing restarted transport or queued debounce"
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    position: to,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: Default::default(),
                }),
                cx,
            );
            ensure!(!view.read(cx).scrubbing, "Ruler did not release scrubbing");
            ensure!(
                serde_json::to_value(&view.read(cx).model.project)? == before,
                "Scrubbing edited the timeline"
            );
            println!(
                "Seek UI smoke: painted ruler drag follows pointer, uses scrub worker immediately, leaves timeline unchanged"
            );
            Ok(())
        })();
        view.update(cx, |editor, cx| {
            editor.drag = None;
            editor.model = Editor::new(untouched);
            editor.changed(false, cx);
        });
        result
    }
    pub fn smoke_zoom(view: Entity<Self>, window: &mut Window, cx: &mut App) -> anyhow::Result<()> {
        use anyhow::{Context, ensure};
        fn key(window: &mut Window, cx: &mut App, name: &str) {
            window.draw(cx).clear();
            let keystroke = Keystroke {
                key: name.into(),
                key_char: None,
                modifiers: Default::default(),
            };
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                }),
                cx,
            );
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
            window.draw(cx).clear();
        }
        let project = serde_json::to_vec(&view.read(cx).model.project)?;
        let saved = (view.read(cx).viewport, view.read(cx).playhead);
        let extent = view.read(cx).timeline_extent();
        ensure!(
            extent > u64::from(view.read(cx).model.project.fps) * 2,
            "Zoom fixture needs >2 seconds of video"
        );
        view.update(cx, |this, cx| this.zoom_timeline(true, 0.5, cx));
        window.draw(cx).clear();
        let viewport = view.read(cx).timeline_window();
        ensure!(
            viewport.span < extent && viewport.start > 0,
            "Zoom did not show a smaller time window"
        );
        let bounds = view
            .read(cx)
            .ruler_bounds
            .get()
            .context("No painted ruler")?;
        let position = point(bounds.left() + bounds.size.width * 0.37, bounds.center().y);
        let expected = viewport.at(0.37);
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                first_mouse: false,
                modifiers: Default::default(),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                modifiers: Default::default(),
            }),
            cx,
        );
        ensure!(
            view.read(cx).playhead.abs_diff(expected) <= 1,
            "Zoomed ruler seek used the wrong coordinate scale"
        );
        window.draw(cx).clear();
        let anchor = viewport.at(0.8);
        let zoom_position = point(bounds.left() + bounds.size.width * 0.8, bounds.center().y);
        window.dispatch_event(
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: zoom_position,
                delta: ScrollDelta::Lines(point(0., 3.)),
                modifiers: Modifiers {
                    control: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
            cx,
        );
        let zoomed = view.read(cx).timeline_window();
        ensure!(
            zoomed.span < viewport.span && zoomed.at(0.8).abs_diff(anchor) <= 1,
            "Wheel zoom moved its pointer anchor"
        );
        window.draw(cx).clear();
        window.dispatch_event(
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: zoom_position,
                delta: ScrollDelta::Lines(point(0., -3.)),
                ..Default::default()
            }),
            cx,
        );
        let panned = view.read(cx).timeline_window();
        ensure!(
            panned.start > zoomed.start && panned.span == zoomed.span,
            "Wheel pan changed the zoom or did not move time"
        );
        ensure!(
            !view.read(cx).viewport.is_following(),
            "Manual pan did not suspend following"
        );
        view.update(cx, |this, cx| {
            this.playhead = 0;
            this.follow_focus.focus(window);
            cx.notify();
        });
        window.draw(cx).clear();
        window.focus_prev();
        key(window, cx, "tab");
        ensure!(
            view.read(cx).follow_focus.is_focused(window),
            "Tab did not reach Follow"
        );
        key(window, cx, "enter");
        ensure!(
            view.read(cx).viewport.is_following() && view.read(cx).timeline_window().start == 0,
            "Enter on Follow did not enable following and reveal the current playhead"
        );
        ensure!(
            !view.read(cx).playing && view.read(cx).playhead == 0,
            "Follow unexpectedly started playback or changed its time"
        );
        key(window, cx, "space");
        ensure!(
            !view.read(cx).viewport.is_following() && !view.read(cx).playing,
            "Space on Follow did not disable following independently of playback"
        );
        ensure!(
            serde_json::to_vec(&view.read(cx).model.project)? == project,
            "Viewport operations changed edits"
        );
        view.update(cx, |this, cx| {
            this.viewport = saved.0;
            this.seek(saved.1, cx);
            this.focus.focus(window);
        });
        println!(
            "Zoom smoke: ruler seek, anchored zoom, pan suspends follow, Tab/Enter/Space follow toggle, project unchanged"
        );
        Ok(())
    }
    // Native autosave fixture injects a separately verified Project.
    fn loaded(&mut self, p: Project) {
        self.loaded_session(Editor::new(p));
    }
    fn park_pending_restores(&mut self) {
        for task in self.pending.drain(..) {
            if let Task::Restore(_, path, editor) = task {
                self.restoring
                    .remove(&crate::preferences::lookup_key(&path));
                self.sessions.insert(path, *editor);
            }
        }
    }
    fn loaded_session(&mut self, editor: Editor) {
        self.load_timing.insert("project_loaded", Instant::now());
        self.edit_recovery = false;
        self.source_recovery = false;
        self.timeline_frames = editor.project.duration();
        self.model = editor;
        self.loading = false;
        self.ready = true;
        self.selected = core::pieces(&self.model.project).first().map(|c| c.id);
        self.queue_media();
        self.request_preview(self.clip_show);
        self.remember();
    }
    fn remember(&mut self) {
        if self.ready
            && let Some(sidecar) = &self.sidecar
        {
            let state = self.save_states.entry(sidecar.clone()).or_default();
            state.label = save_label(&self.title);
            state.changed();
            self.pending_saves
                .insert(sidecar.clone(), self.model.project.clone());
            self.save_due = Some(Instant::now() + Duration::from_millis(150));
        }
    }
    fn autosave_status(&self) -> SaveStatus {
        if self.drag.is_some() {
            return SaveStatus::Saving;
        }
        self.sidecar
            .as_ref()
            .and_then(|path| self.save_states.get(path))
            .map_or(SaveStatus::Saving, |state| state.status)
    }
    fn retry_saves(&mut self, cx: &mut Context<Self>) {
        for (path, state) in &mut self.save_states {
            if state.status == SaveStatus::Failed && self.pending_saves.contains_key(path) {
                state.changed();
            }
        }
        self.save_due = Some(Instant::now());
        self.autosave_notice_dismissed = false;
        cx.notify();
    }
    fn autosave_error(&self) -> Option<String> {
        if self.autosave_notice_dismissed {
            return None;
        }
        let failures = self
            .save_states
            .iter()
            .filter(|(_, state)| state.status == SaveStatus::Failed)
            .collect::<Vec<_>>();
        let (path, state) = failures
            .iter()
            .min_by(|(left, _), (right, _)| left.cmp(right))?;
        let title = if state.label.is_empty() {
            save_label(&path.file_stem().unwrap_or_default().to_string_lossy())
        } else {
            state.label.clone()
        };
        let others = failures.len() - 1;
        let suffix = if others == 0 {
            String::new()
        } else {
            format!(
                " and {others} other clip{}",
                if others == 1 { "" } else { "s" }
            )
        };
        Some(format!(
            "Couldn’t save edits for “{title}”{suffix}. Check available space, then retry saving."
        ))
    }
    fn queue_media(&mut self) {
        // Watching clips does not need editor-only waveforms and timeline strips.
        if self.clip_show { return; }
        let g = self.generation.load(Ordering::Relaxed);
        for a in &self.model.project.assets {
            if self.requested.insert(a.path.clone()) {
                self.pending.push_back(Task::Thumbnails(g, a.clone()));
                for s in a.streams.iter().filter(|s| s.kind == TrackKind::Audio) {
                    self.pending
                        .push_back(Task::Waveform(g, a.path.clone(), s.index));
                }
            }
        }
    }
    fn pause(&mut self) {
        self.seek_refinement = None;
        self.preview_due = None;
        if let Some(session) = &self.preview {
            session.pause();
        }
        self.playing = false;
        self.next_frame = None;
    }
    pub fn is_visible(&self) -> bool { self.visible }
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if !self.visible { return; }
        self.clip_show = false;
        self.restore_watch_fullscreen_on_hide(cx);
        // Stop metadata readers as soon as Library returns, releasing source handles.
        // Export and autosave lifetimes remain independent of view visibility.
        self.media_active.store(false, Ordering::Relaxed);
        self.media_epoch.fetch_add(1, Ordering::AcqRel);
        self.park_pending_restores();
        self.visible = false;
        self.playback_details.open = false;
        self.playback_details.reset_session();
        self.playback_details.clear_snapshot();
        self.pause();
        self.preview.take();
        #[cfg(windows)]
        self.gpu_preview.reset_session();
        self.scrub.take();
        self.scrubbing = false;
        self.drag = None;
        self.audio_refresh = None;
        let mut images = std::mem::take(&mut self.retired);
        images.extend(self.image.take());
        for (_, frames) in self.thumbnails.drain() { images.extend(frames); }
        crate::annotation_surface::retire_deferred(images, cx);
        self.waveforms.clear();
        self.requested.clear();
        self.cpu_image_frame = None;
        self.cpu_image_timing = None;
        self.shown_frame = None;
        self.painted_frame.set(None);
        self.painted_seek.set(None);
        // Preserve restore/history, export and save acknowledgements; derived pixels can be rebuilt.
        let events: Vec<_> = self.rx.try_iter().collect();
        for event in events {
            let derived = match &event {
                Event::MediaCompleted(event, _) => matches!(**event, Event::Waveform(..) | Event::Thumbnails(..)),
                event => matches!(event, Event::Waveform(..) | Event::Thumbnails(..)),
            };
            if !derived { let _ = self.tx.send(event); }
        }
        self.remember();
    }
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if self.visible { return; }
        self.visible = true;
        self.media_active.store(true, Ordering::Relaxed);
        if self.ready { self.queue_media(); }
        else if self.loading && let Some(path) = &self.recording {
            self.pending.push_back(Task::Resolve(self.generation.load(Ordering::Relaxed), path.clone()));
        }
        self.request_preview(false);
        cx.notify();
    }
    fn request_preview(&mut self, resume: bool) {
        self.load_timing.entry("preview_requested").or_insert_with(Instant::now);
        self.pause();
        self.preview.take();
        self.playback_details.reset_session();
        #[cfg(windows)]
        self.gpu_preview.reset_session();
        if self.visible && self.model.project.duration() > 0 {
            if resume {
                if let Some(scrub) = &self.scrub { scrub.cancel_original_refinement(); }
                let delay = if self.clip_show { Duration::ZERO } else { Duration::from_millis(60) };
                self.preview_due = Some((Instant::now() + delay, true));
            } else {
                // Preserve the sharper 1080p proxy while bounding drag uploads.
                // Settled refinement and playback retain physical preview size.
                // ScrubSession fits these bounds once. Passing fitted output
                // dimensions instead would shrink noncanonical viewports again.
                let size = fast_seek_bounds(self.preview_size);
                if self.scrub_dirty
                    && let Some(scrub) = &self.scrub
                    && let Err(e) = scrub.replace_project(self.model.project.clone(), size)
                {
                    log_error("Seek configuration", &format!("{e:#}"));
                    self.scrub.take();
                }
                self.scrub_dirty = false;
                if self.scrub.is_none() {
                    let scrub = if self.clip_show { ScrubSession::new_for_viewer(self.model.project.clone(), size) }
                        else { ScrubSession::new(self.model.project.clone(), size) };
                    match scrub {
                        Ok(scrub) => {
                            let wake = self.frame_waker.clone();
                            scrub.set_waker(move || {
                                let _ = wake.try_send(());
                            });
                            self.scrub = Some(scrub);
                        }
                        Err(e) => {
                            log_error("Seek", &format!("{e:#}"));
                            self.error = Some("Couldn't seek this recording.".into());
                        }
                    }
                }
                if let Some(scrub) = &self.scrub {
                    if self.scrubbing { scrub.request_interactive(self.playhead); }
                    else {
                        let requested_at = Instant::now();
                        let serial = scrub.request_exact(self.playhead);
                        self.seek_refinement = Some(SeekRefinement {
                            quick_serial: serial, requested_at, target: self.playhead,
                            bounds: self.preview_size,
                            size: core::preview_size(&self.model.project, self.preview_size),
                            quick_painted: None, original: None,
                        });
                    }
                }
            }
        }
    }
    fn changed(&mut self, audio_only: bool, cx: &mut Context<Self>) {
        self.timeline_frames = self.timeline_frames.max(self.model.project.duration());
        self.playhead = self
            .playhead
            .min(self.model.project.duration().saturating_sub(1));
        let pieces = core::pieces(&self.model.project);
        if !pieces.iter().any(|c| Some(c.id) == self.selected) {
            self.selected = pieces.first().map(|c| c.id);
        }
        self.remember();
        self.saved = None;
        self.watch_share.clear_receipt();
        self.error = None;
        if audio_only {
            self.restart_audio();
        } else {
            self.scrub_dirty = true;
            self.request_preview(false);
        }
        cx.notify();
    }
    fn edit(&mut self, edit: Edit, audio_only: bool, cx: &mut Context<Self>) {
        if !self.ready {
            return;
        }
        match self.model.execute(edit) {
            Ok(()) => self.changed(audio_only, cx),
            Err(e) => {
                log_error("Edit", &format!("{e:#}"));
                self.error = Some("Choose a cut inside the selected piece.".into());
                cx.notify();
            }
        }
    }
    fn quick(&mut self, edit: QuickEdit, cx: &mut Context<Self>) {
        self.edit(Edit::Sequence(edit), false, cx);
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let preparing = self
            .scrub
            .as_ref()
            .is_some_and(|s| s.seek_proxy_preparing());
        if preparing != self.seek_preparing {
            self.seek_preparing = preparing;
            cx.notify();
        }
        for event in self.rx.try_iter().collect::<Vec<_>>() {
            // User intents (notably Add/Restore) remain busy until their UI
            // result has been applied or parked, not just until I/O returns.
            let (event, _media_admission) = match event {
                Event::MediaCompleted(event, admission) => (*event, Some(admission)),
                event => (event, None),
            };
            let g = self.generation.load(Ordering::Relaxed);
            match event {
                Event::LoadPhase(generation, phase, at) if generation == g => { self.load_timing.entry(phase).or_insert(at); }
                Event::Resolved(generation, path, sidecar) if generation == g && self.visible => {
                    self.recording = Some(path.clone());
                    self.sidecar = Some(sidecar.clone());
                    let key = crate::preferences::lookup_key(&path);
                    // Reopening while this source is still being validated must
                    // wait for its owned history, rather than load disk without it.
                    if self.restoring.contains(&key) {
                        continue;
                    }
                    let editor = self
                        .sessions
                        .remove(&path)
                        .or_else(|| self.pending_saves.get(&sidecar).cloned().map(Editor::new));
                    self.pending.push_front(match editor {
                        Some(editor) => {
                            self.restoring.insert(key);
                            Task::Restore(g, path, Box::new(editor))
                        }
                        None => Task::Open(g, path, sidecar),
                    });
                }
                Event::Restored(generation, path, editor, fresh) => {
                    let key = crate::preferences::lookup_key(&path);
                    self.restoring.remove(&key);
                    if !fresh
                        && generation != g
                        && self.visible
                        && self.loading
                        && self.sidecar.is_some()
                        && self
                            .recording
                            .as_ref()
                            .is_some_and(|active| crate::preferences::lookup_key(active) == key)
                    {
                        self.restoring.insert(key);
                        self.pending.push_front(Task::Restore(g, path, editor));
                        continue;
                    }
                    if generation == g && self.visible {
                        if fresh {
                            self.sessions.remove(&path);
                        }
                        self.loaded_session(*editor);
                    } else if !fresh
                        && !(self.ready
                            && self.recording.as_ref().is_some_and(|active| {
                                crate::preferences::lookup_key(active)
                                    == crate::preferences::lookup_key(&path)
                            }))
                    {
                        // A cancelled restore still owns the only copy of history.
                        // Never replace a more recent parked session with an old reply.
                        let key = crate::preferences::lookup_key(&path);
                        if !self.sessions.entries.iter().any(|(p, _)| p == &key) {
                            self.sessions.insert(path, *editor);
                        }
                    }
                }
                Event::RestoreFailed(generation, epoch, path, editor, detail) => {
                    let key = crate::preferences::lookup_key(&path);
                    self.restoring.remove(&key);
                    // A hide/show can cancel validation without changing the
                    // source generation. Retry its owned history in the new view.
                    if (generation != g || epoch != self.media_epoch.load(Ordering::Acquire))
                        && self.visible
                        && self.loading
                        && self.sidecar.is_some()
                        && self
                            .recording
                            .as_ref()
                            .is_some_and(|active| crate::preferences::lookup_key(active) == key)
                    {
                        self.restoring.insert(key);
                        self.pending.push_front(Task::Restore(g, path, editor));
                        continue;
                    }
                    let active_ready = self.ready
                        && self
                            .recording
                            .as_ref()
                            .is_some_and(|active| crate::preferences::lookup_key(active) == key);
                    if !active_ready && !self.sessions.entries.iter().any(|(p, _)| p == &key) {
                        self.sessions.insert(path, *editor);
                    }
                    if generation == g && epoch == self.media_epoch.load(Ordering::Acquire) && self.visible {
                        log_error("Source validation", &detail);
                        self.loading = false;
                        self.edit_recovery = true;
                        self.source_recovery = true;
                        self.error = Some(if detail.contains("Source unavailable") {
                            "A source file is unavailable. Your edits are preserved. Reconnect the source and reopen this clip, or open the available original recording.".into()
                        } else if detail.contains("Source changed") {
                            "A source file changed. Your edits are preserved. Open the original to review the current recording; the old edits will be kept in a recovery copy.".into()
                        } else {
                            "Couldn’t verify a source file. Your edits are preserved. Check that the source is readable, then reopen this clip or open the original recording.".into()
                        });
                    }
                }
                Event::Added(generation, a) if generation == g => {
                    self.quick(QuickEdit::Add(a), cx);
                    self.selected = core::pieces(&self.model.project).last().map(|c| c.id);
                    self.queue_media();
                }
                Event::Waveform(generation, p, s, b) if generation == g && self.visible => {
                    let bytes = self
                        .waveforms
                        .values()
                        .map(|b| b.len() * std::mem::size_of::<WaveBin>())
                        .sum::<usize>();
                    if bytes + b.len() * std::mem::size_of::<WaveBin>() <= 64 * 1024 * 1024 {
                        self.waveforms.insert((p, s), b);
                    }
                }
                Event::Thumbnails(generation, p, frames) if generation == g && self.visible => {
                    if let Some(previous) = self.thumbnails.insert(p, frames.into_iter().filter_map(image).collect()) {
                        crate::annotation_surface::retire_deferred(previous, cx);
                    }
                }
                Event::Failed(generation, epoch, detail) if generation == g && self.visible && epoch == self.media_epoch.load(Ordering::Acquire) => {
                    log_error("Read", &detail);
                    self.error = Some(
                        "Couldn’t read this recording. Check that it is still available.".into(),
                    );
                    self.loading = false;
                }
                Event::EditFailed(generation, epoch, detail) if generation == g && self.visible && epoch == self.media_epoch.load(Ordering::Acquire) => {
                    log_error("Saved edits", &detail);
                    self.loading = false;
                    self.edit_recovery = true;
                    self.source_recovery = false;
                    self.error = Some("Saved edits couldn’t be opened. You can open the original recording; the old edits will be kept in a recovery copy.".into());
                }
                Event::SaveFinished(path, revision, result) => {
                    if let Some(state) = self.save_states.get_mut(&path)
                        && state.finished(revision, result.is_ok())
                    {
                        if result.is_ok() {
                            self.pending_saves.remove(&path);
                        } else {
                            // Keep the latest Project pending for explicit retry and quit flush.
                            self.autosave_notice_dismissed = false;
                        }
                    }
                }
                Event::ExportSize(path, bytes) => self.watch_share.set_size(&path, bytes),
                Event::Exported(generation, result) => {
                    let source_title = completed_export_source(&self.export_source_title);
                    self.export_source_title.clear();
                    let cancelled = self
                        .export
                        .take()
                        .is_some_and(|job| job.cancel.load(Ordering::Relaxed));
                    if let Some(worker) = self.export_worker.take() {
                        let _ = worker.join();
                    }
                    self.export_partial = result
                        .as_ref()
                        .err()
                        .and_then(|error| error.partial.clone());
                    self.export_detail = result
                        .as_ref()
                        .err()
                        .map(|error| bounded_export_detail(&error.detail));
                    self.export_cancelled = cancelled && result.is_err();
                    self.export_details_open = false;
                    self.export_details_copied = false;
                    if generation != g && result.is_err() {
                        // The source clip may have changed while this job ran.
                        // Retained files still need a discoverable recovery action.
                        cx.notify();
                        continue;
                    }
                    match result {
                        Ok(p) => {
                            self.watch_share.completed(&p);
                            self.saved = Some(p);
                            self.saved_source_title = Some(source_title);
                            self.copied = false;
                        }
                        Err(e) => {
                            if cancelled {
                                self.export_cancelled = true;
                            } else {
                                log_error("Export", &e.detail);
                            }
                        }
                    }
                }
                _ => {}
            }
            cx.notify();
        }
        if let Some(task) = self.pending.pop_front() && let Some(tx) = &self.tasks {
            match tx.try_send(MediaTask::new(task, &self.media_pending)) {
                Err(mpsc::TrySendError::Full(MediaTask { task, .. })) => self.pending.push_front(task),
                Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => {}
            }
        }
        if self.save_due.is_some_and(|t| Instant::now() >= t)
            && let Some(tx) = &self.save_tx
        {
            for (path, project) in &self.pending_saves {
                let Some(state) = self.save_states.get_mut(path) else {
                    continue;
                };
                if !state.can_submit() {
                    continue;
                }
                match tx.try_send((path.clone(), state.revision, project.clone())) {
                    Ok(()) => state.in_flight = Some(state.revision),
                    Err(mpsc::TrySendError::Full(_)) => break,
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        state.status = SaveStatus::Failed;
                        self.autosave_notice_dismissed = false;
                        cx.notify();
                        break;
                    }
                }
            }
            if self.pending_saves.is_empty() {
                self.save_due = None;
            }
        }
        if self.visible
            && let Some(b) = self.preview_bounds.get()
        {
            let size = core::preview_size(
                &self.model.project,
                (
                    (f32::from(b.size.width) * self.pixel_scale) as u32,
                    (f32::from(b.size.height) * self.pixel_scale) as u32,
                ),
            );
            if size != self.preview_size {
                self.preview_size = size;
                self.scrub_dirty = true;
                let resume = self.playing || self.preview_due.is_some_and(|(_, play)| play);
                self.request_preview(resume);
            }
        }
        if let Some((when, play)) = self.preview_due
            && Instant::now() >= when
        {
            self.preview_due = None;
            self.start_preview(play);
        }
        let mut advance_show = false;
        if let Some(job) = &self.preview {
            if self.playing {
                self.playhead = job
                    .position()
                    .min(self.model.project.duration().saturating_sub(1));
            }
            if let Some(frame) = job.frame() {
                self.load_timing.entry("first_frame_received").or_insert_with(Instant::now);
                self.next_frame = Some(frame.into());
            }
            #[cfg(windows)]
            {
                let gpu = &mut self.gpu_preview;
                if !gpu.failed {
                    if let Some(detail) = gpu
                        .surface
                        .as_ref()
                        .and_then(|(_, surface)| surface.error())
                    {
                        log_error("GPU preview; switching to CPU", &detail);
                        gpu.failed = true;
                        job.disable_gpu();
                        cx.notify();
                    } else if let Some(output) = job.gpu_output() {
                        if gpu
                            .surface
                            .as_ref()
                            .is_none_or(|(old, _)| old.texture != output.texture)
                        {
                            match WindowsGpuSurface::new(output.texture.clone()) {
                                Ok(surface) => {
                                    let surface =
                                        surface.with_frame_counter(output.published_frame.clone())
                                            .with_consumer_release(output.consumer_release.clone());
                                    gpu.surface = Some((output.clone(), surface));
                                    cx.notify();
                                }
                                Err(error) => {
                                    log_error(
                                        "GPU preview; switching to CPU",
                                        &format!("{error:#}"),
                                    );
                                    gpu.failed = true;
                                    job.disable_gpu();
                                }
                            }
                        }
                        let published = output.published_frame.load(Ordering::Acquire);
                        if published != gpu.published {
                            gpu.published = published;
                            cx.notify();
                        }
                    }
                }
                // frame() clears the core output only when a CPU replacement exists.
                if self.next_frame.is_some() {
                    gpu.surface = None;
                }
                if let Some((_, surface)) = &gpu.surface
                    && self.visible
                    && !gpu.failed
                    && surface.error().is_none()
                {
                    let id = surface.submitted_frame_id();
                    if id > 0 {
                        let frame = id - 1;
                        self.shown_frame = Some(frame);
                        if self.painted_frame.get().is_none_or(|(old, _)| old != frame) {
                            self.painted_frame.set(Some((frame, Instant::now())));
                        }
                    }
                }
                let status = job.gpu_status();
                if status != gpu.status {
                    gpu.status = status;
                    cx.notify();
                }
            }
            for detail in job.errors.try_iter() {
                log_error("Playback", &detail);
                self.error = Some("Couldn’t play this recording.".into());
                self.playing = false;
                job.pause();
                cx.notify();
            }
            if self.playing && job.finished() {
                self.playing = false;
                job.pause();
                advance_show = self.clip_show;
                cx.notify();
            }
        }
        if advance_show { self.navigate_recording(true, cx); }
        if self.playing && !self.scrubbing && self.drag.is_none() {
            self.viewport.follow_playhead(
                self.playhead,
                self.timeline_extent(),
                self.model.project.fps,
            );
        }
        self.refine_painted_seek();
        if let Some(scrub) = &self.scrub
            && let Some(result) = scrub.frame_with_metadata()
            && self.preview.is_none()
            && !self.playing
            && self.visible
        {
            match result {
                Ok(frame) if if self.scrubbing { !frame.metadata.original }
                    else { self.seek_refinement.as_ref().map_or(frame.metadata.exact, |refinement| refinement.accepts(&frame)) }
                    => self.next_frame = Some(frame.into()),
                Ok(_) => {},
                Err(detail) => {
                    log_error("Seek", &detail);
                    self.error = Some(if self.seek_refinement.as_ref().is_some_and(|refinement| refinement.original.is_some()) {
                        "Couldn’t sharpen the preview. The quick preview is still visible. Seek again to retry."
                    } else { "Couldn't seek this recording." }.into());
                    self.seek_refinement = None;
                    cx.notify();
                }
            }
        }
        if self.audio_refresh.is_some_and(|t| Instant::now() >= t) {
            self.audio_refresh = None;
            self.restart_audio();
        }
        if self.next_frame.is_some() || self.export.is_some() {
            cx.notify();
        }
    }
    fn start_preview(&mut self, play: bool) {
        self.load_timing.entry("preview_session_started").or_insert_with(Instant::now);
        if self.model.project.duration() == 0 {
            return;
        }
        if let Some(scrub) = &self.scrub { scrub.cancel_original_refinement(); }
        self.seek_refinement = None;
        #[cfg(windows)]
        self.gpu_preview.reset_session();
        #[cfg(windows)]
        let config = self.gpu_preview.adapter
            .filter(|_| self.gpu_preview.enabled)
            .map(|adapter_luid| {
                let wake = self.frame_waker.clone();
                let mut config = core::GpuPlaybackConfig::automatic(adapter_luid);
                if let Some(directory) = &self.gpu_preview.directory {
                    config.dll_directory = directory.clone();
                }
                config.wake = Some(Arc::new(move || {
                        let _ = wake.try_send(());
                    }));
                config
            });
        #[cfg(windows)]
        {
            self.playback_details.session_setting = Some(self.gpu_preview.enabled);
            self.playback_details.requested = config.is_some();
        }
        #[cfg(not(windows))]
        {
            self.playback_details.session_setting = Some(false);
            self.playback_details.requested = false;
        }
        #[cfg(windows)]
        let session = if let Some(config) = config {
            PlaybackSession::new_with_gpu(
                self.model.project.clone(),
                self.playhead,
                self.preview_size,
                self.preview_output,
                config,
            )
        } else {
            PlaybackSession::new(
                self.model.project.clone(),
                self.playhead,
                self.preview_size,
                self.preview_output,
            )
        };
        #[cfg(not(windows))]
        let session = PlaybackSession::new(
            self.model.project.clone(),
            self.playhead,
            self.preview_size,
            self.preview_output,
        );
        match session {
            Ok(session) => {
                session.set_monitor_gain(self.viewer_gain());
                if play {
                    session.play();
                }
                self.preview = Some(session);
                self.playing = play;
            }
            Err(error) => {
                log_error("Playback", &format!("{error:#}"));
                self.error = Some("Couldn’t play this recording.".into());
            }
        }
    }
    fn restart_audio(&mut self) {
        if let Some(session) = &self.preview {
            session.set_audio(&self.model.project);
        }
    }
    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if !self.ready {
            return;
        }
        if self.playing {
            self.pause();
        } else {
            self.preview_due = None;
            if self.playhead >= self.model.project.duration().saturating_sub(1) {
                self.playhead = 0;
                self.preview.take();
            }
            if let Some(session) = &self.preview {
                session.play();
                self.playing = true;
            } else {
                self.start_preview(true);
            }
        }
        cx.notify();
    }
    fn seek(&mut self, to: u64, cx: &mut Context<Self>) {
        self.playhead = to.min(self.model.project.duration().saturating_sub(1));
        self.viewport.reveal(
            self.playhead,
            self.timeline_extent(),
            self.model.project.fps,
        );
        self.request_preview(false);
        cx.notify();
    }
    fn scrub_to(&mut self, position: Point<Pixels>, initial: bool, cx: &mut Context<Self>) {
        let bounds = if self.clip_show { self.clip_show_seek_bounds.get() } else { self.strip_bounds.get() };
        if let Some(bounds) = bounds {
            let fraction = (f32::from(position.x - bounds.left())
                / f32::from(bounds.size.width).max(1.))
            .clamp(0., 1.);
            let target = if self.clip_show { clip_show_seek_target(fraction, self.model.project.duration()) }
                else { self.timeline_window().at(f64::from(fraction)).min(self.model.project.duration().saturating_sub(1)) };
            // Subframe motion must not clear a completed frame waiting to paint.
            // Down/explicit seeks still retry; dirty configuration must still apply.
            if !initial && target == self.playhead && self.scrub.is_some()
                && !self.scrub_dirty && self.preview.is_none() && self.preview_due.is_none()
                && !self.playing {
                return;
            }
            self.seek(target, cx);
        }
    }
    fn finish_scrubbing(&mut self, cx: &mut Context<Self>) -> bool {
        if !std::mem::replace(&mut self.scrubbing, false) { return false; }
        let resume = self.clip_show && std::mem::take(&mut self.clip_show_seek_resume);
        self.request_preview(resume);
        cx.notify();
        true
    }
    fn refine_painted_seek(&mut self) {
        if !self.visible || self.scrubbing || self.playing || self.preview.is_some() { return; }
        let Some(refinement) = &mut self.seek_refinement else { return; };
        if refinement.original.is_some() || refinement.target != self.playhead { return; }
        let Some(painted) = self.painted_seek.get() else { return; };
        if !refinement.quick_is_painted(painted, self.cpu_image_frame) { return; }
        // Let continued pointer/keyboard input keep the fast seek worker. A
        // settled preview gets source detail without starting a costly source
        // seek between every pair of closely spaced input events.
        if painted.at.elapsed() < Duration::from_millis(80) { return; }
        if let Some(scrub) = &self.scrub {
            refinement.quick_painted = Some(painted.at);
            // request_original applies aspect fitting itself. Pass its original
            // bounds, not the already even-rounded expected output, because
            // fitting twice can shrink a noncanonical viewport by two pixels.
            refinement.original = Some((scrub.request_original(refinement.target, refinement.bounds), Instant::now()));
            // Keep the painted quick image, but discard any buffered fast result
            // after establishing the core's original-source publication barrier.
            self.next_frame = None;
        }
    }
    fn split(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected {
            self.quick(
                QuickEdit::Split {
                    id,
                    at: self.playhead,
                },
                cx,
            );
        }
    }
    fn delete(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected {
            self.quick(QuickEdit::Delete(id), cx);
        }
    }
    fn trim_start(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected {
            self.quick(
                QuickEdit::TrimStart {
                    id,
                    to: self.playhead as i64,
                },
                cx,
            );
        }
    }
    fn trim_end(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected {
            self.quick(
                QuickEdit::TrimEnd {
                    id,
                    to: self.playhead + 1,
                },
                cx,
            );
        }
    }
    fn undo(&mut self, cx: &mut Context<Self>) {
        if !self.ready {
            return;
        }
        if self.model.undo() {
            self.queue_media();
            self.changed(false, cx);
        }
    }
    fn redo(&mut self, cx: &mut Context<Self>) {
        if !self.ready {
            return;
        }
        if self.model.redo() {
            self.queue_media();
            self.changed(false, cx);
        }
    }
    fn export(&mut self, cx: &mut Context<Self>) {
        self.begin_export(false, cx);
    }
    fn prepare_watch_mp4(&mut self, cx: &mut Context<Self>) {
        self.begin_export(true, cx);
    }
    fn begin_export(&mut self, compatible: bool, cx: &mut Context<Self>) {
        if !self.ready
            || self.export.is_some()
            || self.loading
            || self.model.project.duration() == 0
        {
            return;
        }
        self.pause();
        self.preset_menu = false;
        self.error = None;
        self.saved = None;
        self.saved_source_title = None;
        self.export_cancelled = false;
        self.export_partial = None;
        self.clear_export_details();
        let (mut p, bitrate) = if compatible { watch_share::compatible_project(&self.model.project) }
            else { let p = self.model.project.framed_export(); let bitrate = self.preset.bitrate(&p); (p, bitrate) };
        if !compatible { (p.width, p.height) = self.preset.size(&p); }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = recordings::root()
            .join("exports")
            .join(format!("{}-{stamp}.mp4", if compatible { "share" } else { "clip" }));
        self.watch_share.started(path.clone(), compatible);
        let progress = Arc::new(JobProgress::default());
        self.export_total_us = p.seconds(p.duration()) * 1e6;
        self.export = Some(progress.clone());
        self.export_source_title = self.title.clone();
        let tx = self.tx.clone();
        let generation = self.generation.load(Ordering::Relaxed);
        self.export_worker = Some(thread::spawn(move || {
            let options = ExportOptions {
                output: path,
                start: 0,
                end: None,
                audio: AudioExport::Mix,
                bitrate,
                video_codec: if compatible { core::ExportVideoCodec::H264 } else { core::ExportVideoCodec::Av1 },
            };
            let result = core::export(&p, &options, &progress).map_err(ExportFailure::from);
            if let Ok(path) = &result {
                let bytes = std::fs::metadata(path).ok().map(|metadata| metadata.len());
                let _ = tx.send(Event::ExportSize(path.clone(), bytes));
            }
            let _ = tx.send(Event::Exported(generation, result));
        }));
        cx.notify();
    }
    fn open_original(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = &self.recording {
            self.pending.push_front(Task::OpenOriginal(
                self.generation.load(Ordering::Relaxed),
                path.clone(),
                self.sidecar
                    .as_ref()
                    .and_then(|sidecar| self.pending_saves.get(sidecar))
                    .cloned(),
            ));
            self.loading = true;
            self.error = None;
            cx.notify();
        }
    }
    fn reset_edits(&mut self, cx: &mut Context<Self>) {
        if let Some(asset) = self
            .model
            .project
            .assets
            .iter()
            .find(|a| Some(&a.path) == self.recording.as_ref())
            .cloned()
        {
            self.edit(
                Edit::Batch(vec![
                    Edit::NewProject,
                    Edit::Sequence(QuickEdit::Add(asset)),
                ]),
                false,
                cx,
            );
            self.fit_timeline(cx);
        }
    }
    fn begin_drag(
        &mut self,
        id: Id,
        kind: DragKind,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window);
        cx.stop_propagation();
        self.pause();
        self.selected = Some(id);
        let width = self
            .strip_bounds
            .get()
            .map_or(1., |b| f32::from(b.size.width))
            .max(1.);
        self.drag = Some(Drag {
            base: self.model.project.clone(),
            id,
            kind,
            x: f32::from(event.position.x),
            width,
            total: self.timeline_extent(),
            span: self.timeline_window().span,
            edit: None,
        });
        if let Some(b) = self.strip_bounds.get() {
            self.playhead = self.timeline_window().at(f64::from(
                (f32::from(event.position.x - b.left()) / width).clamp(0., 1.),
            ));
            self.playhead = self
                .playhead
                .min(self.model.project.duration().saturating_sub(1));
        }
        if matches!(kind, DragKind::Move) {
            self.request_preview(false);
        }
        cx.notify();
    }
    fn drag_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.drag else {
            return;
        };
        let delta = f32::from(event.position.x) - drag.x;
        let edit = if matches!(drag.kind, DragKind::Reframe) {
            Edit::Reframe {
                vertical: true,
                center: (drag.base.reframe.center + f64::from(delta / drag.width)).clamp(0., 1.),
            }
        } else if matches!(drag.kind, DragKind::Volume) {
            let gain = (delta / drag.width * 2.
                + drag
                    .base
                    .tracks
                    .iter()
                    .find(|t| t.id == drag.id)
                    .map_or(1., |t| t.gain) as f32)
                .clamp(0., 2.) as f64;
            Edit::TrackGain {
                id: drag.id,
                value: gain,
            }
        } else {
            let Some(c) = drag.base.clips.iter().find(|c| c.id == drag.id) else {
                return;
            };
            let (low, high) = drag_frame_limits(&drag.base, c, drag.kind);
            if low > high { return; }
            let frames = ((f64::from(delta / drag.width) * drag.span as f64).round() as i64).clamp(low, high);
            match drag.kind {
                DragKind::Left => {
                    Edit::Sequence(QuickEdit::TrimStart {
                        id: drag.id,
                        to: c.start as i64 + frames,
                    })
                }
                DragKind::Right => {
                    Edit::Sequence(QuickEdit::TrimEnd {
                        id: drag.id,
                        to: (c.end() as i64 + frames) as u64,
                    })
                }
                DragKind::Move => {
                    if delta.abs() < 5. {
                        return;
                    }
                    Edit::Move { id: drag.id, to: (c.start as i64 + frames) as u64 }
                }
                DragKind::Volume | DragKind::Reframe => unreachable!(),
            }
        };
        if same_trim_target(drag.edit.as_ref(), &edit) { return; }
        let mut trial = Editor::new(drag.base.clone());
        if trial.execute(edit.clone()).is_ok() {
            if matches!(drag.kind, DragKind::Move)
                && let Some(clip) = trial.project.clips.iter().find(|clip| clip.id == drag.id)
                && moved_group_overlaps(&trial.project, clip.group) { return; }
            self.model.project = trial.project;
            drag.edit = Some(edit);
            let kind = drag.kind;
            let id = drag.id;
            if !matches!(kind, DragKind::Volume | DragKind::Move | DragKind::Reframe) {
                if let Some(c) = self.model.project.clips.iter().find(|c| c.id == id) {
                    self.playhead = if matches!(kind, DragKind::Right) {
                        c.end() - 1
                    } else {
                        c.start
                    };
                }
                self.scrub_dirty = true;
                self.request_preview(false);
            }
            if matches!(kind, DragKind::Volume) && self.playing && self.audio_refresh.is_none() {
                self.audio_refresh = Some(Instant::now() + Duration::from_millis(100));
            }
            cx.notify();
        }
    }
    fn finish_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.drag.take() {
            self.model.project = drag.base;
            if let Some(edit) = drag.edit {
                self.edit(
                    edit,
                    matches!(drag.kind, DragKind::Volume | DragKind::Reframe),
                    cx,
                );
            } else {
                self.request_preview(false);
                cx.notify();
            }
        }
    }
    fn add_clip(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        if self.recording.is_none() {
            self.open_media(path, cx);
            return;
        }
        if !self.ready {
            return;
        }
        self.picker = false;
        if core::pieces(&self.model.project).len() >= 128 {
            self.error =
                Some("This clip already has 128 pieces. Export before adding more.".into());
        } else {
            self.pending
                .push_front(Task::Add(self.generation.load(Ordering::Relaxed), path));
        }
        cx.notify();
    }
    fn browse_clip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = false;
        self.focus.focus(window);
        cx.notify();
        self.pending_dialogs = self.pending_dialogs.saturating_add(1);
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Add a video recording to this clip".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = selected.await;
            let _ = view.update(cx, |editor, cx| {
                editor.pending_dialogs = editor.pending_dialogs.saturating_sub(1);
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            if recordings::is_supported(&path) && !recordings::is_image(&path) {
                                editor.add_clip(path, cx);
                            } else {
                                editor.error = Some(
                                    "Choose a video file. Images and .ssxe projects open from Library."
                                        .into(),
                                );
                                cx.notify();
                            }
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => {
                        editor.error = Some("Couldn’t open the file picker. Try Add clip again.".into());
                        cx.notify();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn timeline_extent(&self) -> u64 {
        self.drag
            .as_ref()
            .map_or(
                self.timeline_frames.max(self.model.project.duration()),
                |d| d.total.max(self.timeline_frames),
            )
            .max(1)
    }
    fn timeline_window(&self) -> crate::timeline_view::TimelineWindow {
        self.viewport
            .window(self.timeline_extent(), self.model.project.fps)
    }
    fn zoom_timeline(&mut self, inside: bool, fraction: f64, cx: &mut Context<Self>) {
        if self.drag.is_none() && !self.scrubbing {
            self.viewport.zoom(
                inside,
                fraction,
                self.timeline_extent(),
                self.model.project.fps,
            );
            cx.notify();
        }
    }
    fn fit_timeline(&mut self, cx: &mut Context<Self>) {
        self.timeline_frames = self.model.project.duration();
        self.viewport.fit();
        cx.notify();
    }
    fn timeline_interval(&self, clip: &core::Clip) -> (i64, u64) {
        (clip.start as i64, clip.duration)
    }
    fn strip(&self, cx: &mut Context<Self>) -> Div {
        let p = &self.model.project;
        let viewport = self.timeline_window();
        let duration = viewport.span as f32;
        let bounds = self.strip_bounds.clone();
        let mut strip = div()
            .id("clip-strip")
            .relative()
            .flex_1()
            .min_w_0()
            .h(px(64.))
            .rounded_md()
            .overflow_hidden()
            .bg(rgb(PANEL))
            .child(
                canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            );
        for c in core::pieces(p) {
            let id = c.id;
            let (display_start, display_duration) = self.timeline_interval(c);
            let selected = self.selected == Some(id);
            let asset = match c.content {
                Content::Media { asset, .. } => p.assets.iter().find(|a| a.id == asset),
                _ => None,
            };
            let thumbs = asset
                .and_then(|a| self.thumbnails.get(&a.path))
                .cloned()
                .unwrap_or_default();
            let source_total = asset.map_or(1, |a| a.duration_ns);
            let source_in = c.source_in_ns;
            let source_len = c.duration as f64 / f64::from(p.fps) * c.speed * 1e9;
            let mut piece = div()
                .id(("piece", id as usize))
                .tab_index(0)
                .absolute()
                .left(relative(viewport.fraction(display_start)))
                .w(relative(display_duration as f32 / duration))
                .h_full()
                .overflow_hidden()
                .rounded_md()
                .border_1()
                .border_color(rgb(if selected { ACCENT } else { LINE }))
                .focus(|s| s.border_color(rgb(0xffffff)))
                .bg(rgb(0x17181b))
                .cursor_pointer()
                .tooltip(|_, cx| {
                    cx.new(|_| {
                        Tip(
                            "Enter to select · Drag to move · Drag edges or use I / O to trim"
                                .into(),
                        )
                    })
                    .into()
                })
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    if matches!(event, ClickEvent::Keyboard(_)) {
                        this.selected = Some(id);
                        cx.notify();
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e, w, cx| {
                        this.begin_drag(id, DragKind::Move, e, w, cx)
                    }),
                );
            if !thumbs.is_empty() {
                piece = piece.child(
                    canvas(
                        |_, _, _| {},
                        move |area, _, window, _| {
                            let count = (f32::from(area.size.width) / 100.).ceil().max(1.) as usize;
                            let width = area.size.width / count as f32;
                            let mask = window.content_mask().bounds;
                            let first = (f32::from(mask.left() - area.left()) / f32::from(width))
                                .floor()
                                .max(0.) as usize;
                            let last = (f32::from(mask.right() - area.left()) / f32::from(width))
                                .ceil()
                                .max(0.) as usize;
                            for i in first..last.min(count) {
                                let at = (source_in as f64
                                    + source_len * (i as f64 + 0.5) / count as f64)
                                    / source_total as f64;
                                let image = thumbs[((at * thumbs.len() as f64).floor() as usize)
                                    .min(thumbs.len() - 1)]
                                .clone();
                                let target = Bounds::new(
                                    point(area.left() + width * i as f32, area.top()),
                                    size(width, area.size.height),
                                );
                                let _ =
                                    window.paint_image(target, Corners::default(), image, 0, false);
                            }
                        },
                    )
                    .absolute()
                    .top(px(20.))
                    .w_full()
                    .h(px(44.)),
                );
            }
            piece = piece.child(
                div()
                    .absolute()
                    .left(px(12.))
                    .top(px(2.))
                    .rounded_sm()
                    .px_1()
                    .bg(rgba(0x080c14cc))
                    .text_xs()
                    .child(clock(c.duration, p.fps)),
            );
            if (c.speed - 1.).abs() > 0.001 {
                piece = piece.child(
                    div()
                        .absolute()
                        .right(px(10.))
                        .top(px(2.))
                        .px_1()
                        .rounded_sm()
                        .bg(rgb(ACCENT))
                        .text_color(rgb(0x0b0c0e))
                        .text_xs()
                        .child(format!("{}×", c.speed)),
                );
            }
            for (left, kind) in [(true, DragKind::Left), (false, DragKind::Right)] {
                let handle_bounds = self.trim_bounds.clone();
                piece = piece.child(
                    div()
                        .id((if left { "trim-start" } else { "trim-end" }, id as usize))
                        .absolute()
                        .top_0()
                        .h_full()
                        .w(px(14.))
                        .when(left, |s| s.left_0())
                        .when(!left, |s| s.right_0())
                        .bg(if selected {
                            rgba(0x3ecf8ecc)
                        } else {
                            rgba(0xffffff66)
                        })
                        .cursor(CursorStyle::ResizeLeftRight)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            canvas(
                                move |area, _, _| {
                                    handle_bounds.borrow_mut().insert((id, left), area);
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .child(div().w(px(2.)).h(px(18.)).rounded_full().bg(rgb(0x0b0c0e)))
                        .tooltip(move |_, cx| {
                            cx.new(|_| {
                                Tip(if left {
                                    "Trim start · I".into()
                                } else {
                                    "Trim end · O".into()
                                })
                            })
                            .into()
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e, w, cx| this.begin_drag(id, kind, e, w, cx)),
                        ),
                );
            }
            strip = strip.child(piece);
        }
        div().flex().gap_2().child(strip).child(
            button(
                "add-clip",
                "+ Add clip",
                "Add a recording from your Library",
            )
            .w(px(110.))
            .h(px(64.))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, window, cx| {
                this.picker = !this.picker;
                this.preset_menu = false;
                if this.picker {
                    this.menu_focus.focus(window);
                } else {
                    this.focus.focus(window);
                }
                cx.notify();
            })),
        )
    }
    fn timeline(&self, cx: &mut Context<Self>) -> Div {
        let p = &self.model.project;
        let ruler_bounds = self.ruler_bounds.clone();
        let mut ruler = div()
            .id("seek-ruler")
            .relative()
            .flex_1()
            .min_w_0()
            .h(px(22.))
            .cursor_pointer()
            .child(
                canvas(
                    move |bounds, _, _| ruler_bounds.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, w, cx| {
                    this.focus.focus(w);
                    this.scrubbing = true;
                    this.scrub_to(e.position, true, cx);
                    cx.stop_propagation();
                }),
            );
        for i in 0..8 {
            let frame = self.timeline_window().at(i as f64 / 7.);
            ruler = ruler.child(
                div()
                    .absolute()
                    .left(relative(i as f32 / 7.))
                    .top_0()
                    .when(i == 7, |s| s.ml(px(-50.)))
                    .child(
                        theme::mono(clock(frame, p.fps))
                            .text_size(px(10.))
                            .text_color(rgb(0x686872)),
                    ),
            );
        }
        let measured = self.strip_bounds.clone();
        let shown_playhead = self
            .drag
            .as_ref()
            .filter(|d| matches!(d.kind, DragKind::Left))
            .and_then(|d| p.clips.iter().find(|c| c.id == d.id))
            .map_or(self.playhead, |c| self.timeline_interval(c).0.max(0) as u64);
        let position = self.timeline_window().fraction(shown_playhead as i64);
        div()
            .relative()
            .px_6()
            .pt_3()
            .pb_5()
            .bg(rgb(0x0f1013))
            .flex()
            .flex_col()
            .gap_1()
            .flex_shrink_0()
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                if this.drag.is_some() || this.scrubbing {
                    return;
                }
                let delta = e.delta.pixel_delta(px(20.));
                if e.modifiers.control || e.modifiers.alt {
                    let fraction = this
                        .strip_bounds
                        .get()
                        .map_or(0.5, |b| {
                            f64::from(
                                f32::from(e.position.x - b.left())
                                    / f32::from(b.size.width).max(1.),
                            )
                        })
                        .clamp(0., 1.);
                    let amount = f32::from(delta.y);
                    if amount.abs() > 0.1 {
                        this.zoom_timeline(amount > 0., fraction, cx);
                    }
                } else {
                    let amount = if f32::from(delta.x).abs() > 0.1 {
                        f32::from(delta.x)
                    } else {
                        f32::from(delta.y)
                    };
                    this.viewport.pan(
                        -f64::from(amount) / 400.,
                        this.timeline_extent(),
                        this.model.project.fps,
                    );
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .child(
                div()
                    .flex()
                    .child(
                        div()
                            .w(px(196.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(theme::muted("TIMELINE").text_size(px(10.)).mr_1())
                            .child(
                                button("timeline-zoom-out", "−", "Zoom out · −")
                                    .h(px(22.))
                                    .w(px(22.))
                                    .px_0()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.zoom_timeline(false, 0.5, cx)
                                    })),
                            )
                            .child(
                                label(format!(
                                    "{:.0}×",
                                    self.timeline_extent() as f64
                                        / self.timeline_window().span as f64
                                ))
                                .w(px(30.))
                                .text_center(),
                            )
                            .child(
                                button("timeline-zoom-in", "+", "Zoom in · + · Ctrl+wheel")
                                    .h(px(22.))
                                    .w(px(22.))
                                    .px_0()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.zoom_timeline(true, 0.5, cx)
                                    })),
                            )
                            .child(
                                button(
                                    "timeline-follow",
                                    if self.viewport.is_following() { "Follow on" } else { "Follow off" },
                                    "Keep the playhead visible during playback. Panning or zooming turns following off; Fit turns it on.",
                                )
                                .track_focus(&self.follow_focus)
                                .h(px(22.))
                                .w(px(52.))
                                .px_0()
                                .text_size(px(10.))
                                .flex_shrink_0()
                                .when(self.viewport.is_following(), |s| s.bg(rgb(0x24372f)).text_color(rgb(ACCENT)))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let following = !this.viewport.is_following();
                                    this.viewport.set_following(following, this.playhead,
                                        this.timeline_extent(), this.model.project.fps);
                                    cx.notify();
                                })),
                            ),
                    )
                    .child(ruler)
                    .child(div().w(px(118.)).flex_shrink_0()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .w(px(196.))
                            .flex_shrink_0()
                            .h(px(64.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(theme::icon("film"))
                            .child(theme::muted("Clips")),
                    )
                    .child(div().flex_1().min_w_0().child(self.strip(cx))),
            )
            .child(self.audio_lanes(cx))
            .child(
                canvas(
                    |_, _, _| {},
                    move |area, _, window, _| {
                        if let Some(strip) = measured.get() {
                            if !(0.0..=1.0).contains(&position) {
                                return;
                            }
                            let x = strip.left() + strip.size.width * position;
                            window.paint_quad(fill(
                                Bounds::new(point(x, area.top()), size(px(1.), area.size.height)),
                                rgb(theme::TEXT),
                            ));
                            window.paint_quad(fill(
                                Bounds::new(point(x - px(4.), area.top()), size(px(9.), px(10.))),
                                rgb(theme::TEXT),
                            ));
                        }
                    },
                )
                .absolute()
                .top(px(12.))
                .bottom(px(20.))
                .left_6()
                .right_6(),
            )
    }
    fn step_track_gain(&mut self, id: Id, amount: f64, cx: &mut Context<Self>) {
        if let Some(track) = self
            .model
            .project
            .tracks
            .iter()
            .find(|track| track.id == id)
        {
            let value = (track.gain + amount).clamp(0., 2.);
            if (value - track.gain).abs() > f64::EPSILON {
                self.edit(Edit::TrackGain { id, value }, true, cx);
            }
        }
    }
    fn audio_lanes(&self, cx: &mut Context<Self>) -> Div {
        let mut lanes = div().flex().flex_col().gap_1();
        let p = &self.model.project;
        let tracks: Vec<_> = p
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio && p.clips.iter().any(|c| c.track == t.id))
            .collect();
        if tracks.is_empty() && !self.loading && p.duration() > 0 {
            return lanes.child(label(
                "This recording has no audio. Enable game audio or microphone when recording.",
            ));
        }
        for t in tracks {
            let id = t.id;
            let muted = t.muted;
            let gain = t.gain;
            let segments: Vec<_> = p
                .clips
                .iter()
                .filter(|c| c.track == id)
                .filter_map(|c| {
                    let Content::Media { asset, stream } = c.content else {
                        return None;
                    };
                    let a = p.assets.iter().find(|a| a.id == asset)?;
                    Some((
                        self.timeline_interval(c).0,
                        c.duration,
                        c.source_in_ns,
                        c.speed,
                        self.waveforms.get(&(a.path.clone(), stream))?.clone(),
                    ))
                })
                .collect();
            let viewport = self.timeline_window();
            let total = viewport.span;
            let fps = p.fps;
            let slider_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
            let measure = slider_bounds.clone();
            let controls = div()
                .w(px(196.))
                .flex_shrink_0()
                .pr_4()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    button(
                        ("audio-toggle", id as usize),
                        "",
                        "Mute / unmute this audio track",
                    )
                    .w(px(24.))
                    .h(px(24.))
                    .px_1()
                    .bg(rgb(0x0f1013))
                    .child(theme::icon(if muted {
                        "muted"
                    } else if t.name.starts_with("Microphone") {
                        "mic"
                    } else {
                        "speaker"
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(Edit::MuteTrack { id }, true, cx)
                    })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.))
                        .truncate()
                        .child(t.name.clone()),
                )
                .child(
                    div()
                        .id(("volume", id as usize))
                        .tab_index(0)
                        .border_1()
                        .border_color(rgba(0x00000000))
                        .rounded_sm()
                        .focus(|s| s.border_color(rgb(ACCENT)))
                        .relative()
                        .w(px(64.))
                        .h(px(24.))
                        .cursor_pointer()
                        .tooltip(move |_, cx| {
                            cx.new(|_| {
                                Tip(format!(
                                    "Volume {:.0}% · drag or ← / → · Shift for fine adjustment",
                                    gain * 100.
                                )
                                .into())
                            })
                            .into()
                        })
                        // Keybindings dispatch before raw key events in GPUI.
                        // Override editor seek actions while this slider has focus.
                        .on_action(cx.listener(move |this, _: &StepBack, _, cx| {
                            this.step_track_gain(id, -0.05, cx)
                        }))
                        .on_action(cx.listener(move |this, _: &StepForward, _, cx| {
                            this.step_track_gain(id, 0.05, cx)
                        }))
                        .on_action(cx.listener(move |this, _: &SecondBack, _, cx| {
                            this.step_track_gain(id, -0.01, cx)
                        }))
                        .on_action(cx.listener(move |this, _: &SecondForward, _, cx| {
                            this.step_track_gain(id, 0.01, cx)
                        }))
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.modifiers.control
                                || event.keystroke.modifiers.alt
                                || event.keystroke.modifiers.platform
                            {
                                return;
                            }
                            let gain = this
                                .model
                                .project
                                .tracks
                                .iter()
                                .find(|t| t.id == id)
                                .map_or(1., |t| t.gain);
                            let step = if event.keystroke.modifiers.shift {
                                0.01
                            } else {
                                0.05
                            };
                            let value = match event.keystroke.key.as_str() {
                                "left" | "down" => (gain - step).max(0.),
                                "right" | "up" => (gain + step).min(2.),
                                "home" => 0.,
                                "end" => 2.,
                                _ => return,
                            };
                            if (value - gain).abs() > f64::EPSILON {
                                this.edit(Edit::TrackGain { id, value }, true, cx);
                            }
                            window.prevent_default();
                            cx.stop_propagation();
                        }))
                        .child(
                            canvas(move |area, _, _| measure.set(Some(area)), |_, _, _, _| {})
                                .absolute()
                                .size_full(),
                        )
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top(px(11.))
                                .h(px(4.))
                                .rounded_full()
                                .bg(rgb(LINE)),
                        )
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top(px(11.))
                                .h(px(4.))
                                .w(relative((gain / 2.) as f32))
                                .rounded_full()
                                .bg(rgb(ACCENT)),
                        )
                        .child(
                            div()
                                .absolute()
                                .left(relative((gain / 2.) as f32))
                                .top(px(7.))
                                .size(px(11.))
                                .rounded_full()
                                .bg(rgb(0xededef)),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                                this.focus.focus(w);
                                cx.stop_propagation();
                                if let Some(b) = slider_bounds.get() {
                                    let value = (f64::from(
                                        f32::from(e.position.x - b.left())
                                            / f32::from(b.size.width),
                                    ) * 2.)
                                        .clamp(0., 2.);
                                    let base = this.model.project.clone();
                                    if let Some(t) =
                                        this.model.project.tracks.iter_mut().find(|t| t.id == id)
                                    {
                                        t.gain = value;
                                    }
                                    this.drag = Some(Drag {
                                        base,
                                        id,
                                        kind: DragKind::Volume,
                                        x: f32::from(e.position.x)
                                            - (value - gain) as f32 / 2. * f32::from(b.size.width),
                                        width: f32::from(b.size.width),
                                        total: 0,
                                        span: 0,
                                        edit: Some(Edit::TrackGain { id, value }),
                                    });
                                    this.restart_audio();
                                    cx.notify();
                                }
                            }),
                        ),
                );
            let waveform = div()
                .flex()
                .flex_1()
                .min_w_0()
                .h(px(28.))
                .rounded_sm()
                .overflow_hidden()
                .relative()
                .children(p.clips.iter().filter(|c| c.track == id).map(|c| {
                    div()
                        .absolute()
                        .h_full()
                        .left(relative(viewport.fraction(self.timeline_interval(c).0)))
                        .w(relative(c.duration as f32 / total as f32))
                        .rounded_sm()
                        .border_r_2()
                        .border_color(rgb(0x0f1013))
                        .bg(rgb(if muted { 0x1b1c20 } else { 0x13291f }))
                }))
                .child(
                    canvas(
                        |_, _, _| {},
                        move |area, _, window, _| {
                            let width = f32::from(area.size.width).max(1.);
                            for x in (0..width as usize).step_by(3) {
                                let frame = viewport.start as f64
                                    + x as f64 / f64::from(width) * total as f64;
                                let bin = segments
                                    .iter()
                                    .find(|(start, duration, ..)| {
                                        frame >= *start as f64
                                            && frame < (*start as f64 + *duration as f64)
                                    })
                                    .and_then(|(start, _, source, speed, bins)| {
                                        let second = *source as f64 / 1e9
                                            + (frame - *start as f64) / f64::from(fps) * speed;
                                        bins.get((second * 100.) as usize)
                                    });
                                let Some(bin) = bin else {
                                    continue;
                                };
                                let amplitude =
                                    (bin.min.abs().max(bin.max) * gain as f32 * 24.).clamp(1., 11.);
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(
                                            area.left() + px(x as f32),
                                            area.top() + px(14. - amplitude),
                                        ),
                                        size(px(2.), px(amplitude * 2.)),
                                    ),
                                    rgb(if muted { MUTED } else { 0x3ecf8e }),
                                ));
                            }
                        },
                    )
                    .flex_1()
                    .min_w_0()
                    .h(px(28.)),
                );
            lanes = lanes.child(
                div()
                    .flex()
                    .items_center()
                    .opacity(if muted { 0.55 } else { 1. })
                    .child(controls)
                    .child(waveform)
                    .child(div().w(px(118.)).flex_shrink_0()),
            );
        }
        lanes
    }
}
impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.observe_watch_fullscreen(window);
        if self.scrub_activation.is_none() {
            self.scrub_activation = Some(cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() { this.finish_scrubbing(cx); }
            }));
        }
        #[cfg(windows)]
        if self.gpu_preview.enabled {
            let adapter = window.windows_gpu_adapter_luid();
            if self.gpu_preview.adapter != adapter {
                if let Some(session) = &self.preview {
                    session.disable_gpu();
                    self.gpu_preview.failed = true;
                    self.gpu_preview.surface = None;
                }
                self.gpu_preview.adapter = adapter;
            }
        }
        self.trim_bounds.borrow_mut().clear();
        if self.playing {
            window.request_animation_frame();
        }
        self.pixel_scale = window.scale_factor();
        if self.focus_requested {
            self.focus.focus(window);
            self.focus_requested = false;
        }
        for old in self.retired.drain(..) {
            let _ = window.drop_image(old);
        }
        if !self.visible {
            self.show(cx);
        }
        if let Some(frame) = self.next_frame.take() {
            self.load_timing.entry("first_frame_bound").or_insert_with(Instant::now);
            #[cfg(windows)]
            {
                self.gpu_preview.surface = None;
            }
            let frame_id = frame.frame.frame;
            let frame_size = (frame.frame.width, frame.frame.height);
            let seek = frame.seek;
            let timing = frame.trace.map(|(core, received_at)| (core, received_at, Instant::now()));
            if let Some(old) = self.image.take() {
                let _ = window.drop_image(old);
            }
            self.image = image(frame.frame);
            self.cpu_image_frame = self.image.as_ref().map(|image| CpuFrameIdentity { frame: frame_id, seek, size: frame_size, image_id: image.id });
            self.cpu_image_timing = self.image.as_ref().and_then(|_| timing.map(|(core, received_at, bind_started_at)|
                CpuFrameTiming { core, received_at, bind_started_at, bound_at: Instant::now() }));
        }
        if self.model.project.duration() == 0
            && let Some(old) = self.image.take()
        {
            let _ = window.drop_image(old);
            self.cpu_image_frame = None;
            self.cpu_image_timing = None;
            self.shown_frame = None;
            self.painted_frame.set(None);
            self.painted_seek.set(None);
        }
        let bounds = self.preview_bounds.clone();
        let image = self.image.clone();
        #[cfg(windows)]
        let gpu_surface = self
            .gpu_preview
            .surface
            .as_ref()
            .filter(|(_, surface)| surface.error().is_none())
            .map(|(_, surface)| surface.clone());
        #[cfg(windows)]
        let has_gpu = gpu_surface.is_some();
        #[cfg(not(windows))]
        let has_gpu = false;
        if !has_gpu {
            self.shown_frame = self.cpu_image_frame.map(|identity| identity.frame);
        }
        let image_identity = self.cpu_image_frame;
        let image_timing = self.cpu_image_timing;
        let image_frame = image_identity.map(|identity| identity.frame);
        let painted_frame = self.painted_frame.clone();
        let painted_seek = self.painted_seek.clone();
        let aspect = self.model.project.width as f32 / self.model.project.height as f32;
        let reframe = self.model.project.reframe;
        let preview = div()
            .id("preview")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .rounded_lg()
            .overflow_hidden()
            .bg(rgb(0x070809))
            .cursor_pointer()
            .tooltip(|_, cx| cx.new(|_| Tip("Play / pause · Space".into())).into())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, w, cx| {
                    if this.clip_show || !this.model.project.reframe.vertical {
                        return;
                    }
                    this.focus.focus(w);
                    cx.stop_propagation();
                    this.pause();
                    if let Some(area) = this.preview_bounds.get() {
                        let aspect =
                            this.model.project.width as f32 / this.model.project.height as f32;
                        let width = area.size.width.min(area.size.height * aspect);
                        let (_, fraction) = this.model.project.reframe.horizontal_window(
                            f64::from(this.model.project.width),
                            f64::from(this.model.project.height),
                        );
                        this.ignore_preview_click = true;
                        this.drag = Some(Drag {
                            base: this.model.project.clone(),
                            id: 0,
                            kind: DragKind::Reframe,
                            x: f32::from(event.position.x),
                            width: (f32::from(width) * (1. - fraction as f32)).max(1.),
                            total: 0,
                            span: 0,
                            edit: None,
                        });
                    }
                }),
            )
            .on_click(cx.listener(|this, _, w, cx| {
                this.focus.focus(w);
                if std::mem::take(&mut this.ignore_preview_click) {
                    return;
                }
                this.toggle_play(cx);
            }))
            .child(
                canvas(
                    move |area, _, _| bounds.set(Some(area)),
                    move |area, _, w, _| {
                        if image.is_some() || has_gpu {
                            let width = area.size.width.min(area.size.height * aspect);
                            let height = width / aspect;
                            let target = Bounds::new(
                                point(
                                    area.left() + (area.size.width - width) / 2.,
                                    area.top() + (area.size.height - height) / 2.,
                                ),
                                size(width, height),
                            );
                            #[cfg(windows)]
                            if let Some(surface) = &gpu_surface {
                                painted_seek.set(None);
                                w.paint_windows_surface(target, surface.clone());
                            }
                            let trace_paint = image_timing.filter(|_| !has_gpu && image_identity.is_some_and(|identity|
                                identity.seek.is_some() && !painted_seek.get().is_some_and(|painted| painted.identity == identity)));
                            let paint_started_at = trace_paint.map(|_| Instant::now());
                            if !has_gpu && image.as_ref().is_some_and(|image| w.paint_image(target, Corners::default(), image.clone(), 0, false).is_ok()) {
                                let at = Instant::now();
                                if let Some(frame) = image_frame
                                    && !painted_frame.get().is_some_and(|(painted, _)| painted == frame) {
                                    painted_frame.set(Some((frame, at)));
                                }
                                if let Some(identity) = image_identity.filter(|identity| identity.seek.is_some()) {
                                    if !painted_seek.get().is_some_and(|painted| painted.identity == identity) {
                                        painted_seek.set(Some(PaintedSeek { identity, at }));
                                        if let Some((timing, started)) = trace_paint.zip(paint_started_at) {
                                            timing.log(identity, started, at);
                                        }
                                    }
                                } else { painted_seek.set(None); }
                            }
                            if reframe.vertical {
                                let (left, fraction) = reframe.horizontal_window(
                                    f64::from(f32::from(width)),
                                    f64::from(f32::from(height)),
                                );
                                let crop_left = target.left() + width * left as f32;
                                let crop_right = crop_left + width * fraction as f32;
                                for (x, span) in [
                                    (target.left(), crop_left - target.left()),
                                    (crop_right, target.right() - crop_right),
                                ] {
                                    w.paint_quad(fill(
                                        Bounds::new(point(x, target.top()), size(span, height)),
                                        rgba(0x000000a6),
                                    ));
                                }
                                for x in [crop_left, crop_right] {
                                    w.paint_quad(fill(
                                        Bounds::new(point(x, target.top()), size(px(1.), height)),
                                        rgb(theme::TEXT),
                                    ));
                                }
                                for y in [target.top(), target.bottom() - px(1.)] {
                                    w.paint_quad(fill(
                                        Bounds::new(
                                            point(crop_left, y),
                                            size(width * fraction as f32, px(1.)),
                                        ),
                                        rgb(theme::TEXT),
                                    ));
                                }
                            }
                        }
                    },
                )
                .size_full(),
            )
            .when(reframe.vertical && !self.clip_show, |s| {
                s.cursor(CursorStyle::ResizeLeftRight).child(
                    div()
                        .absolute()
                        .top_3()
                        .left(relative(0.5))
                        .ml(px(-55.))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgba(0x00000099))
                        .child(theme::muted("Drag to reframe").text_color(rgb(theme::TEXT))),
                )
            })
            .when(self.image.is_none() && !has_gpu, |s| {
                s.child(
                    div()
                        .absolute()
                        .inset_0()
                        .size_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .items_center()
                        .justify_center()
                        .child(if self.loading {
                            "Opening recording…"
                        } else if self.recording.is_none() {
                            "Open a recording from your Library"
                        } else if self.ready && self.model.project.duration() == 0 {
                            "Add a clip to continue"
                        } else if self.source_recovery {
                            "Review the source before continuing with saved edits"
                        } else if self.edit_recovery {
                            "Open the original recording to recover this clip"
                        } else {
                            "Preparing preview…"
                        })
                        .when(self.edit_recovery && !self.loading, |s| {
                            s.child(button("recover-source-center", "Open original", "Keep a recovery copy of previous edits, then open the current source")
                                .on_click(cx.listener(|this, _, _, cx| this.open_original(cx))))
                        }),
                )
            })
            .when(self.clip_show, |s| {
                let previous = adjacent_recording(&self.library_choices, self.recording.as_deref(), false).is_some();
                let next = adjacent_recording(&self.library_choices, self.recording.as_deref(), true).is_some();
                s.child(button("show-previous-side", "‹", "Previous clip · Left arrow")
                    .absolute().left_3().top(relative(0.5)).mt(px(-26.))
                    .w(px(44.)).h(px(52.)).px_0().text_size(px(36.)).rounded_lg()
                    .bg(rgba(0x111216a6)).text_color(rgb(theme::TEXT))
                    .opacity(if previous { 0.85 } else { 0.2 })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.navigate_recording(false, cx); })))
                 .child(button("show-next-side", "›", "Next clip · Right arrow")
                    .absolute().right_3().top(relative(0.5)).mt(px(-26.))
                    .w(px(44.)).h(px(52.)).px_0().text_size(px(36.)).rounded_lg()
                    .bg(rgba(0x111216a6)).text_color(rgb(theme::TEXT))
                    .opacity(if next { 0.85 } else { 0.2 })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.navigate_recording(true, cx); })))
            });
        let compact_controls = f32::from(window.viewport_size().width) < 1280.;
        let controls = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                button("play", "", "Play / pause · Space")
                    .size(px(36.))
                    .px_0()
                    .rounded_full()
                    .bg(rgb(theme::TEXT))
                    .text_color(rgb(theme::BG))
                    .child(
                        theme::icon(if self.playing { "pause" } else { "play" })
                            .text_color(rgb(theme::BG)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))),
            )
            .child(
                label(format!(
                    "{} / {}",
                    clock(self.playhead, self.model.project.fps),
                    clock(self.model.project.duration(), self.model.project.fps)
                ))
                .font_family("Geist Mono")
                .w(px(146.)),
            )
            .child(
                button("split", "Split", "Split at playhead · S")
                    .child(theme::icon("split"))
                    .bg(rgb(theme::BG))
                    .on_click(cx.listener(|this, _, _, cx| this.split(cx))),
            )
            .child(
                button("delete", "Delete", "Remove selected piece · Delete")
                    .child(theme::icon("trash"))
                    .bg(rgb(theme::BG))
                    .on_click(cx.listener(|this, _, _, cx| this.delete(cx))),
            )
            .child(
                button(
                    "fit-timeline",
                    "Fit",
                    "Fit the timeline to the current edit",
                )
                .bg(rgb(theme::BG))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.fit_timeline(cx);
                })),
            )
            .when(self.seek_preparing, |s| {
                s.child(label("Optimizing seeking…"))
            })
            .when_some(self.cpu_image_frame.filter(|identity| !has_gpu && !self.playing
                && identity.seek.is_some_and(|seek| !seek.exact)), |s, identity| {
                s.child(label(if self.scrubbing {
                    format!("Preview near {} · release for exact frame", clock(identity.frame, self.model.project.fps))
                } else { "Seeking exact frame…".into() }))
            })
            .child(self.playback_status_badge(cx))
            .child(div().flex_1());
        let mut output_controls = div()
            .flex()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .child(label("Format"));
        for (vertical, name) in [(false, "16:9"), (true, "9:16")] {
            output_controls = output_controls.child(
                button(
                    if vertical { "portrait" } else { "landscape" },
                    name,
                    "Choose output format; drag the preview to reframe",
                )
                .child(theme::icon(if vertical { "portrait" } else { "landscape" }))
                .when(self.model.project.reframe.vertical == vertical, |s| {
                    s.bg(rgb(0x303137))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let center = this.model.project.reframe.center;
                    this.edit(Edit::Reframe { vertical, center }, true, cx);
                })),
            )
        }
        output_controls = output_controls.child(div().w(px(8.))).child(label("Speed"));
        let speed = self
            .selected
            .and_then(|id| self.model.project.clips.iter().find(|c| c.id == id))
            .map_or(1., |c| c.speed);
        for (i, value) in [0.5, 1., 1.5, 2., 4.].into_iter().enumerate() {
            output_controls = output_controls.child(
                button(
                    ("speed", i),
                    format!("{value}×"),
                    "Change speed of the selected piece",
                )
                .when((speed - value).abs() < 0.001, |s| {
                    s.bg(rgb(0x173025)).text_color(rgb(ACCENT))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(id) = this.selected {
                        this.quick(QuickEdit::Speed { id, value }, cx);
                    }
                })),
            );
        }
        let controls = div()
            .flex()
            .gap_2()
            .when(compact_controls, |s| s.flex_col().justify_center())
            .when(!compact_controls, |s| s.items_center())
            .child(
                controls
                    .when(!compact_controls, |s| s.flex_1())
                    .when(compact_controls, |s| s.w_full()),
            )
            .child(output_controls.when(compact_controls, |s| s.justify_end()));
        let controls = if self.clip_show {
            let bounds = self.clip_show_seek_bounds.clone();
            let progress = self.playhead as f32 / self.model.project.duration().saturating_sub(1).max(1) as f32;
            let seek = div().id("clip-show-seek").relative().w_full().h(px(22.)).cursor_pointer()
                .track_focus(&self.clip_show_seek_focus).key_context("ClipShowSeek")
                .rounded_md().focus(|s| s.bg(rgba(0x3ecf8e1a)))
                .flex().items_center()
                .tooltip(|_, cx| cx.new(|_| Tip("Click or drag to seek · Focused arrows: one second · Home/End · Space: play/pause".into())).into())
                .on_action(cx.listener(|this, _: &TogglePlay, _, cx| this.toggle_play(cx)))
                .child(canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {}).absolute().inset_0())
                .child(div().relative().h(px(4.)).w_full().rounded_full().bg(rgb(0x34363d))
                    .child(div().h_full().w(relative(progress)).rounded_full().bg(rgb(ACCENT)))
                    .child(div().absolute().left(relative(progress)).top(px(-3.)).ml(px(-5.))
                        .size(px(10.)).rounded_full().bg(rgb(theme::TEXT))))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if !this.ready { return; }
                    this.clip_show_seek_focus.focus(window);
                    this.clip_show_seek_resume = this.playing || this.preview_due.is_some_and(|(_, play)| play);
                    this.scrubbing = true;
                    this.scrub_to(event.position, true, cx);
                    cx.stop_propagation();
                }));
            div().flex().flex_col().justify_center().gap_1().child(seek)
                .child(div().flex().items_center().gap_3()
                .child(button("show-play", if self.playing { "Pause" } else { "Play" }, "Play / pause · Space")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))))
                .child(label(format!("{} / {}", clock(self.playhead, self.model.project.fps), clock(self.model.project.duration(), self.model.project.fps))))
                .child(div().flex_1())
                .when(!compact_controls, |s| s.child(label("Space · play/pause    ← / → · previous/next")))
                .child(button("viewer-volume-down", "−", "Lower playback volume")
                    .px_2().on_click(cx.listener(|this, _, _, cx| this.adjust_viewer_volume(-0.1, cx))))
                .child(button("viewer-mute", if self.clip_show_muted { "Unmute" } else { "Mute" }, "Mute playback · M; exports keep their audio")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_viewer_mute(cx))))
                .child(label(format!("{:.0}%", if self.clip_show_muted { 0. } else { self.clip_show_volume * 100. })))
                .child(button("viewer-volume-up", "+", "Raise playback volume")
                    .px_2().on_click(cx.listener(|this, _, _, cx| this.adjust_viewer_volume(0.1, cx))))
                .child(button("viewer-fullscreen", if window.is_fullscreen() { "Windowed" } else { "Fullscreen" }, "Toggle fullscreen · F11")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_watch_fullscreen(window, cx)))))
        } else { controls };
        let mut header = div()
            .h(px(56.))
            .flex_shrink_0()
            .px_5()
            .border_b_1()
            .border_color(rgb(LINE))
            .flex()
            .items_center()
            .gap_3()
            .child(theme::logo())
            .child(
                button("back-library", "Library", "Back to your recordings")
                    .child(theme::icon("arrow-left"))
                    .bg(rgb(theme::BG))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hide(cx);
                        cx.emit(BackToLibrary);
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.))
                    .text_color(rgb(MUTED))
                    .truncate()
                    .child(format!("/   {}", self.title)),
            );
        header = header.child(button("start-clip-show", "Clip show", "Watch Library clips in sequence · Ctrl+Enter; share this window with friends")
            .on_click(cx.listener(|this, _, window, cx| this.toggle_clip_show(window, cx))));
        header = header.when(self.ready, |s| match self.autosave_status() {
            SaveStatus::Failed => s.child(
                button(
                    "retry-autosave-status",
                    "Couldn’t save · Retry",
                    "Retry saving unsaved edits",
                )
                .text_size(px(11.))
                .text_color(rgb(0xffb0b0))
                .on_click(cx.listener(|this, _, _, cx| this.retry_saves(cx))),
            ),
            status => s.child(
                label(if status == SaveStatus::Saved {
                    "Saved"
                } else {
                    "Saving…"
                })
                .text_size(px(11.)),
            ),
        });
        header = header
            .child(
                button("undo", "", "Undo · Ctrl+Z")
                    .when(!self.ready, |s| s.opacity(0.35))
                    .child(theme::icon("undo"))
                    .bg(rgb(theme::BG))
                    .on_click(cx.listener(|this, _, _, cx| this.undo(cx))),
            )
            .child(
                button("redo", "", "Redo · Ctrl+Shift+Z")
                    .when(!self.ready, |s| s.opacity(0.35))
                    .child(theme::icon("redo"))
                    .bg(rgb(theme::BG))
                    .on_click(cx.listener(|this, _, _, cx| this.redo(cx))),
            )
            .when(self.ready, |s| {
                s.child(
                    button(
                        "reset-edits",
                        "Reset edits",
                        "Restore the full original recording · Undo restores your edits",
                    )
                    .bg(rgb(theme::BG))
                    .on_click(cx.listener(|this, _, _, cx| this.reset_edits(cx))),
                )
            });
        let mut completion = None;
        if let Some(job) = &self.export {
            let percent = (job.microseconds.load(Ordering::Relaxed) as f64
                / self.export_total_us.max(1.)
                * 100.)
                .min(100.);
            let cancelling = job.cancel.load(Ordering::Relaxed);
            header = header
                .child(label(if cancelling {
                    "Cancelling export…".into()
                } else if job.suppressing_audio.load(Ordering::Relaxed) {
                    "Cleaning microphone…".into()
                } else {
                    format!("Exporting {percent:.0}%")
                }))
                .child(
                    button(
                        "cancel-export",
                        if cancelling {
                            "Cancelling…"
                        } else {
                            "Cancel export"
                        },
                        "Stop the current export",
                    )
                    .opacity(if cancelling { 0.5 } else { 1. })
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(job) = &this.export {
                            job.cancel.store(true, Ordering::Relaxed);
                        }
                        cx.notify();
                    })),
                );
        } else if self.export_cancelled {
            header = header.child(label("Export cancelled"));
        } else if let Some(path) = self.saved.clone() {
            let copy_path = path.clone();
            let share_path = path.clone();
            completion = Some(
                div()
                    .absolute()
                    .right_5()
                    .top(px(58.))
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(0x17181b))
                    .border_1()
                    .border_color(rgb(LINE))
                    .shadow_lg()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(theme::icon("check").text_color(rgb(ACCENT)))
                    .child(label(self.saved_source_title.as_ref().map_or_else(|| "Saved".to_owned(),
                        |title| format!("Saved · {title}"))).max_w(px(220.)).truncate())
                    .child(
                        button("show-saved", if self.shell_operation != 0 { "Opening…" } else { "Show in folder" }, "Show your saved clip").on_click(
                            cx.listener(move |this, _, _, cx| this.reveal_output(path.clone(), cx)),
                        ),
                    )
                    .child(
                        button(
                            "copy-saved",
                            if self.copy_operation != 0 { "Copying…" } else if self.copied { "Copied" } else { "Copy" },
                            "Copy your clip to paste into a folder or message",
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            #[cfg(not(windows))]
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                copy_path
                                    .canonicalize()
                                    .unwrap_or_else(|_| copy_path.clone())
                                    .to_string_lossy()
                                    .into_owned(),
                            ));
                            #[cfg(windows)]
                            this.copy_saved_file(copy_path.clone(), window, cx);
                            #[cfg(not(windows))]
                            {
                                let _ = window;
                                this.copied = true;
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        button(
                            "share-saved",
                            "Share",
                            "Share with your configured destination",
                        )
                        .on_click(
                            cx.listener(move |_, _, _, cx| cx.emit(ShareClip(share_path.clone()))),
                        ),
                    )
                    .child(
                        button("dismiss-saved", "", "Dismiss saved clip notification")
                            .px_2()
                            .child(theme::icon("close"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.saved = None;
                                cx.notify();
                            })),
                    ),
            );
        }
        if self.export.is_none() && (self.export_partial.is_some() || self.export_detail.is_some())
        {
            completion = Some(
                div()
                    .absolute()
                    .right_5()
                    .top(px(58.))
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(0x17181b))
                    .border_1()
                    .border_color(rgb(LINE))
                    .shadow_lg()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label(if self.export_cancelled {
                        "Export cancelled"
                    } else {
                        "Couldn’t export this clip"
                    }))
                    .when(self.export_detail.is_some(), |s| {
                        s.child(
                            button(
                                "show-export-details",
                                "Show details",
                                "Read the actual export failure",
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| this.show_export_details(window, cx),
                            )),
                        )
                    })
                    .when_some(self.export_partial.clone(), |s, partial| {
                        s.child(
                            button(
                                "show-partial-export",
                                if self.shell_operation != 0 { "Opening…" } else { "Show in folder" },
                                "Find the incomplete export to inspect or remove it",
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.reveal_output(partial.clone(), cx))),
                        )
                    })
                    .child(
                        button(
                            "dismiss-partial-export",
                            "",
                            "Dismiss partial export notification",
                        )
                        .px_2()
                        .child(theme::icon("close"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.export_partial = None;
                            this.clear_export_details();
                            cx.notify();
                        })),
                    ),
            );
        }
        header = header
            .child(
                button_with_hover("export", "Export", "Export your clip · Ctrl+E", 0x62dba4)
                    .relative()
                    .overflow_hidden()
                    .when_some(self.export.as_ref(), |s, job| {
                        s.child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .h(px(2.))
                                .w(relative(
                                    (job.microseconds.load(Ordering::Relaxed) as f64
                                        / self.export_total_us.max(1.))
                                    .clamp(0., 1.) as f32,
                                ))
                                .bg(rgb(theme::TEXT)),
                        )
                    })
                    .bg(rgb(ACCENT))
                    .text_color(rgb(0x0b0c0e))
                    .opacity(if !self.ready || self.export.is_some() {
                        0.35
                    } else {
                        1.
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.export(cx))),
            )
            .child(
                button(
                    "export-options",
                    self.preset.name(),
                    "Choose export size and quality",
                )
                .child(theme::icon("chevron"))
                .when(!self.ready, |s| s.opacity(0.35))
                .on_click(cx.listener(|this, _, window, cx| {
                    if !this.ready {
                        return;
                    }
                    this.preset_menu = !this.preset_menu;
                    this.picker = false;
                    if this.preset_menu {
                        this.menu_focus.focus(window);
                    } else {
                        this.focus.focus(window);
                    }
                    cx.notify();
                })),
            );
        if self.clip_show {
            *self.clip_show_bounds.borrow_mut() = [None; 4];
            let show_button = |index: usize, id: &'static str, text: &'static str, tip: &'static str| {
                let bounds = self.clip_show_bounds.clone();
                button(id, text, tip).relative().child(canvas(
                    move |area, _, _| bounds.borrow_mut()[index] = Some(area), |_, _, _, _| {}).absolute().inset_0())
            };
            let position = self.library_choices.iter().position(|(path, _)| Some(path) == self.recording.as_ref());
            let previous = adjacent_recording(&self.library_choices, self.recording.as_deref(), false).is_some();
            let next = adjacent_recording(&self.library_choices, self.recording.as_deref(), true).is_some();
            header = div().h(px(56.)).flex_shrink_0().px_5().border_b_1().border_color(rgb(LINE))
                .flex().items_center().gap_3()
                .child(show_button(0, "show-library", "Library", "Back to your recordings")
                    .on_click(cx.listener(|this, _, window, cx| {
                        if !this.prepare_watch_mode_exit(window, cx) { return; }
                        this.clip_show = false; this.hide(cx); cx.emit(BackToLibrary);
                    })))
                .child(label("CLIP SHOW").text_color(rgb(ACCENT)))
                .child(div().flex_1().min_w_0().truncate().child(self.title.clone()))
                .when_some(position, |s, index| s.child(label(format!("{} / {}", index + 1, self.library_choices.len()))))
                .child(show_button(1, "previous-recording", "Previous", "Previous Library clip · Left arrow")
                    .opacity(if previous { 1. } else { 0.35 })
                    .on_click(cx.listener(|this, _, _, cx| this.navigate_recording(false, cx))))
                .child(show_button(2, "next-recording", "Next", "Next Library clip · Right arrow")
                    .opacity(if next { 1. } else { 0.35 })
                    .on_click(cx.listener(|this, _, _, cx| this.navigate_recording(true, cx))))
                .child(show_button(3, "end-clip-show", "Open in Editor", "Return to editing · Escape")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_clip_show(window, cx))));
        }
        let mut root = div()
            .id("quick-editor")
            // Bound actions are dispatched before raw key handlers. Removing the
            // editor context keeps modal keystrokes out of timeline commands.
            .key_context(if self.playback_details.open {
                "PlaybackDetails"
            } else if self.export_details_open {
                "ExportDetails"
            } else if self.clip_show {
                "ClipShow"
            } else {
                "Editor"
            })
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .bg(rgb(theme::BG))
            .text_color(rgb(theme::TEXT))
            .font_family("Geist")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.finish_scrubbing(cx) {
                    window.prevent_default();
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "escape" && (this.preset_menu || this.picker) {
                    this.preset_menu = false;
                    this.picker = false;
                    this.focus.focus(window);
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if event.keystroke.key == "tab"
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.alt
                    && !event.keystroke.modifiers.platform
                {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.scrubbing {
                    if e.pressed_button == Some(MouseButton::Left) {
                        this.scrub_to(e.position, false, cx);
                    } else { this.finish_scrubbing(cx); }
                } else {
                    this.drag_move(e, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.finish_scrubbing(cx);
                    this.finish_drag(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.finish_scrubbing(cx);
                    this.finish_drag(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, w, _| this.focus.focus(w)),
            )
            .on_action(cx.listener(|this, _: &TogglePlay, window, cx| {
                if this.focus.is_focused(window) {
                    this.toggle_play(cx);
                } else {
                    // Focused controls activate with the native Space key-up click.
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &SplitClip, _, cx| this.split(cx)))
            .on_action(cx.listener(|this, _: &DeleteClip, _, cx| this.delete(cx)))
            .on_action(cx.listener(|this, _: &TrimStart, _, cx| this.trim_start(cx)))
            .on_action(cx.listener(|this, _: &TrimEnd, _, cx| this.trim_end(cx)))
            .on_action(cx.listener(|this, _: &UndoEdit, _, cx| this.undo(cx)))
            .on_action(cx.listener(|this, _: &RedoEdit, _, cx| this.redo(cx)))
            .on_action(cx.listener(|this, _: &StepBack, _, cx| {
                this.seek(this.playhead.saturating_sub(1), cx)
            }))
            .on_action(cx.listener(|this, _: &StepForward, _, cx| this.seek(this.playhead + 1, cx)))
            .on_action(cx.listener(|this, _: &SecondBack, _, cx| {
                this.seek(
                    this.playhead
                        .saturating_sub(u64::from(this.model.project.fps)),
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &SecondForward, _, cx| {
                this.seek(this.playhead + u64::from(this.model.project.fps), cx)
            }))
            .on_action(cx.listener(|this, _: &ExportClip, _, cx| this.export(cx)))
            .on_action(
                cx.listener(|this, _: &ZoomTimelineIn, _, cx| this.zoom_timeline(true, 0.5, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ZoomTimelineOut, _, cx| this.zoom_timeline(false, 0.5, cx)),
            )
            .on_action(cx.listener(|this, _: &FitTimeline, _, cx| this.fit_timeline(cx)))
            .on_action(cx.listener(|this, _: &ToggleClipShow, window, cx| this.toggle_clip_show(window, cx)))
            .on_action(cx.listener(|this, _: &PreviousRecording, _, cx| this.navigate_recording(false, cx)))
            .on_action(cx.listener(|this, _: &NextRecording, _, cx| this.navigate_recording(true, cx)))
            .on_action(cx.listener(|this, _: &ExitClipShow, window, cx| {
                if this.exit_watch_fullscreen(window, cx) { return; }
                if this.clip_show { this.toggle_clip_show(window, cx); }
            }))
            .on_action(cx.listener(|this, _: &watch_fullscreen::ToggleViewerFullscreen, window, cx| this.toggle_watch_fullscreen(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleViewerMute, _, cx| this.toggle_viewer_mute(cx)))
            .on_action(cx.listener(|this, _: &SeekViewerStart, _, cx| this.seek(0, cx)))
            .on_action(cx.listener(|this, _: &SeekViewerEnd, _, cx| this.seek(this.model.project.duration().saturating_sub(1), cx)))
            .child(header)
            .when(self.clip_show, |root| root.child(self.watch_share_row(cx)));
        let mut popover = None;
        if self.preset_menu {
            let mut menu = div().w_full().flex().flex_col().gap_1();
            for (i, preset) in [Preset::Original, Preset::FullHd, Preset::Small]
                .into_iter()
                .enumerate()
            {
                let output = self.model.project.framed_export();
                let (width, height) = preset.size(&output);
                let mb = output.seconds(output.duration())
                    * f64::from(preset.bitrate(&output) + 192_000)
                    / 8_000_000.;
                menu = menu.child(
                    button(
                        ("preset", i),
                        format!("{} · {width}×{height} · ≈{mb:.1} MB", preset.name()),
                        "Estimated size; actual size varies with the recording",
                    )
                    .when(i == 0, |s| s.track_focus(&self.menu_focus))
                    .when(self.preset == preset, |s| s.text_color(rgb(ACCENT)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.preset = preset;
                        this.preset_menu = false;
                        this.focus.focus(window);
                        cx.notify();
                    })),
                );
            }
            let mut options = div()
                .occlude()
                .absolute()
                .top(px(54.))
                .right_5()
                .w(px(320.))
                .p_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(0x17181b))
                .shadow_lg()
                .flex()
                .flex_col()
                .gap_2()
                .child(menu)
                .child(
                    label(format!(
                        "Save to {}",
                        recordings::root().join("exports").display()
                    ))
                    .truncate(),
                );
            for track in self
                .model
                .project
                .tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Audio && t.name.starts_with("Microphone"))
            {
                let id = track.id;
                options = options.child(
                    button(
                        ("clean-mic", id as usize),
                        if track.suppress_noise {
                            "☑ Clean microphone when exporting"
                        } else {
                            "☐ Clean microphone when exporting"
                        },
                        "Optional noise removal on export; the original recording stays unchanged.",
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(Edit::SuppressNoise { id }, true, cx)
                    })),
                );
            }
            popover = Some(options);
        }
        root = root
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_6()
                    .pt_5()
                    .pb_3()
                    .flex()
                    .child(preview),
            )
            .when(self.ready, |s| {
                s.child(
                    controls
                        .h(px(if self.clip_show { 76. } else if compact_controls { 96. } else { 60. }))
                        .flex_shrink_0()
                        .px_6(),
                )
                .when(!self.clip_show, |s| s.child(self.timeline(cx)))
            });
        root = root
            .when_some(if self.clip_show { None } else { completion }, |s, p| s.child(p))
            .when_some(popover, |s, p| s.child(p));
        if self.picker {
            let mut picker = div()
                .id("recording-picker")
                .occlude()
                .absolute()
                .right_6()
                .bottom(px(170.))
                .w(px(380.))
                .shadow_lg()
                .border_1()
                .border_color(rgb(LINE))
                .rounded_md()
                .bg(rgb(PANEL))
                .p_2()
                .flex()
                .flex_col()
                .gap_1();
            let mut choices = div()
                .id("recording-picker-choices")
                .max_h(px(240.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1();
            if self.library_choices.is_empty() {
                choices = choices.child(label("No recent Library videos yet. Browse to add one."));
            }
            for (i, (path, title)) in self
                .library_choices
                .iter()
                .take(MAX_LIBRARY_CHOICES)
                .enumerate()
            {
                let path = path.clone();
                choices = choices.child(
                    button(
                        ("add-recording", i),
                        title.clone(),
                        "Add this recording to the end of your clip",
                    )
                    .when(i == 0, |s| s.track_focus(&self.menu_focus))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focus.focus(window);
                        this.add_clip(path.clone(), cx);
                    })),
                );
            }
            picker = picker
                .child(label("Recent Library videos"))
                .child(choices)
                .child(
                    div().pt_2().border_t_1().border_color(rgb(LINE)).child(
                        button(
                            "browse-recording",
                            "Browse videos…",
                            "Choose another video file; originals stay in place",
                        )
                        .w_full()
                        .justify_start()
                        .when(self.library_choices.is_empty(), |s| {
                            s.track_focus(&self.menu_focus)
                        })
                        .child(theme::icon("folder"))
                        .on_click(cx.listener(|this, _, window, cx| this.browse_clip(window, cx))),
                    ),
                );
            root = root.child(picker);
        }
        let notice = editor_notice(self.error.as_deref(), self.autosave_error());
        let root = root.when_some(notice, |s, (e, showing_save_error)| {
            s.child(
                div()
                    .absolute()
                    .bottom_4()
                    .right_6()
                    .max_w(px(480.))
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(0x291b20))
                    .border_1()
                    .border_color(rgb(0x73404c))
                    .shadow_lg()
                    .flex()
                    .items_start()
                    .gap_3()
                    .text_sm()
                    .text_color(rgb(0xffb0b0))
                    .child(div().flex_1().min_w_0().child(e))
                    .when(showing_save_error, |s| {
                        s.child(
                            button("retry-autosave", "Retry save", "Retry saving unsaved edits")
                                .on_click(cx.listener(|this, _, _, cx| this.retry_saves(cx))),
                        )
                    })
                    .when(self.edit_recovery && !self.loading, |s| {
                        s.child(
                            button(
                                "recover-original",
                                "Open original",
                                "Keep a recovery copy of old edits and open the source",
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.open_original(cx))),
                        )
                    })
                    .child(
                        button("dismiss-editor-error", "", "Dismiss this message")
                            .w(px(24.))
                            .h(px(24.))
                            .px_0()
                            .bg(rgb(0x291b20))
                            .child(theme::icon("close"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if showing_save_error {
                                    this.autosave_notice_dismissed = true;
                                } else {
                                    this.error = None;
                                }
                                cx.notify();
                            })),
                    ),
            )
        });
        root.when(self.playback_details.open, |root| {
            root.child(self.playback_details_panel(cx))
        }).when(self.export_details_open, |root| {
            root.child(self.export_details_panel(cx))
        })
    }
}
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("space", TogglePlay, Some("Editor")),
        KeyBinding::new("s", SplitClip, Some("Editor")),
        KeyBinding::new("delete", DeleteClip, Some("Editor")),
        KeyBinding::new("i", TrimStart, Some("Editor")),
        KeyBinding::new("o", TrimEnd, Some("Editor")),
        KeyBinding::new("ctrl-z", UndoEdit, Some("Editor")),
        KeyBinding::new("ctrl-shift-z", RedoEdit, Some("Editor")),
        KeyBinding::new("left", StepBack, Some("Editor")),
        KeyBinding::new("right", StepForward, Some("Editor")),
        KeyBinding::new("shift-left", SecondBack, Some("Editor")),
        KeyBinding::new("shift-right", SecondForward, Some("Editor")),
        KeyBinding::new("ctrl-e", ExportClip, Some("Editor")),
        KeyBinding::new("=", ZoomTimelineIn, Some("Editor")),
        KeyBinding::new("shift-=", ZoomTimelineIn, Some("Editor")),
        KeyBinding::new("-", ZoomTimelineOut, Some("Editor")),
        KeyBinding::new("ctrl-0", FitTimeline, Some("Editor")),
        KeyBinding::new("ctrl-enter", ToggleClipShow, Some("Editor")),
        KeyBinding::new("space", TogglePlay, Some("ClipShow")),
        KeyBinding::new("left", PreviousRecording, Some("ClipShow")),
        KeyBinding::new("right", NextRecording, Some("ClipShow")),
        KeyBinding::new("escape", ExitClipShow, Some("ClipShow")),
        KeyBinding::new("m", ToggleViewerMute, Some("ClipShow")),
        KeyBinding::new("f11", watch_fullscreen::ToggleViewerFullscreen, Some("ClipShow")),
        KeyBinding::new("f11", watch_fullscreen::ToggleViewerFullscreen, Some("ClipShowSeek")),
        KeyBinding::new("shift-left", SecondBack, Some("ClipShow")),
        KeyBinding::new("shift-right", SecondForward, Some("ClipShow")),
        KeyBinding::new("home", SeekViewerStart, Some("ClipShow")),
        KeyBinding::new("end", SeekViewerEnd, Some("ClipShow")),
        KeyBinding::new("left", SecondBack, Some("ClipShowSeek")),
        KeyBinding::new("right", SecondForward, Some("ClipShowSeek")),
    ]);
}
impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Drop for EditorView {
    fn drop(&mut self) {
        // Cancel in-flight metadata decoders before waiting on any other workers.
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.tasks.take();
        self.pending.clear();
        self.pause();
        if let Some(scrub) = self.scrub.take() {
            scrub.shutdown();
        }
        if let Some(session) = self.preview.take() {
            session.shutdown();
        }
        if let Some(job) = &self.export {
            job.cancel.store(true, Ordering::Relaxed);
        }
        // Let the supervisor observe cancellation and reap FFmpeg before exit.
        if let Some(worker) = self.export_worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.media_worker.take() {
            let _ = worker.join();
        }
        self.remember();
        if let Some(tx) = self.save_tx.take() {
            for (path, p) in self.pending_saves.drain() {
                let revision = self
                    .save_states
                    .get(&path)
                    .map_or(0, |state| state.revision);
                let _ = tx.send((path, revision, p));
            }
        }
        if let Some(worker) = self.save_worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn export_diagnostics_are_bounded_without_splitting_unicode() {
        let detail = "失敗🎞".repeat(MAX_EXPORT_DETAIL_CHARS);
        let bounded = bounded_export_detail(&detail);
        assert!(bounded.starts_with("失敗🎞"));
        assert!(bounded.ends_with("[Additional export details omitted]"));
        assert!(bounded.chars().count() < MAX_EXPORT_DETAIL_CHARS + 64);
        assert_eq!(
            bounded_export_detail("encoder missing\nsource unchanged"),
            "encoder missing\nsource unchanged"
        );
        let at_limit = "é".repeat(MAX_EXPORT_DETAIL_CHARS);
        assert_eq!(bounded_export_detail(&at_limit), at_limit);
    }
    #[test]
    fn export_failure_keeps_recovery_path_separate_from_diagnostic_text() {
        let dir = tempfile::tempdir().unwrap();
        let partial = dir.path().join(".rcr-export-owned/clip.mp4.partial");
        let error = anyhow::anyhow!("export cancelled").context(core::RetainedExport {
            partial: partial.clone(),
        });
        let failure = ExportFailure::from(error);
        assert_eq!(failure.partial, Some(partial));
        assert!(failure.detail.contains("export cancelled"));
        assert!(failure.detail.contains("partial export retained"));
    }
    #[test]
    fn cancelled_restore_returns_the_entire_editor_history() {
        let mut editor = Editor::default();
        editor.linked_edits = false;
        let original = serde_json::to_value(&editor.project).unwrap();
        editor
            .execute(Edit::Reframe {
                vertical: true,
                center: 0.25,
            })
            .unwrap();
        let changed = serde_json::to_value(&editor.project).unwrap();
        let Event::RestoreFailed(g, epoch, path, mut editor, _) =
            restore_event(7, 3, "owned.mkv".into(), Box::new(editor), || true)
        else {
            panic!("Cancelled validation must return its owned Editor");
        };
        assert_eq!(g, 7);
        assert_eq!(epoch, 3);
        assert_eq!(path, PathBuf::from("owned.mkv"));
        assert!(!editor.linked_edits);
        assert_eq!(serde_json::to_value(&editor.project).unwrap(), changed);
        assert!(editor.undo());
        assert_eq!(serde_json::to_value(&editor.project).unwrap(), original);
        assert!(editor.redo());
        assert_eq!(serde_json::to_value(&editor.project).unwrap(), changed);
    }

    #[test]
    fn autosave_notices_preserve_media_errors_and_only_offer_retry_for_save_failures() {
        let media = "Couldn’t read this recording";
        let save = "Couldn’t save edits for Clip A";
        assert_eq!(
            editor_notice(Some(media), Some(save.into())),
            Some((media.into(), false))
        );
        assert_eq!(
            editor_notice(Some(media), None),
            Some((media.into(), false)),
            "A successful autosave must not clear a media error"
        );
        assert_eq!(
            editor_notice(None, Some(save.into())),
            Some((save.into(), true)),
            "Dismissing the media error must expose the retained save failure and retry"
        );
        assert_eq!(editor_notice(None, None), None);
    }

    #[test]
    fn autosave_failure_names_are_bounded_and_cannot_inject_control_characters() {
        assert_eq!(save_label("  Clip\r\nname\t  "), "Clipname");
        let name = save_label(&"🦀".repeat(200));
        assert_eq!(name.chars().count(), 80);
        assert!(name.len() <= 320);
    }

    #[test]
    fn autosave_never_reports_saved_for_an_older_acknowledgement() {
        let mut state = AutosaveState::default();
        state.changed();
        let first = state.revision;
        state.in_flight = Some(first);
        state.changed();
        let latest = state.revision;
        assert!(
            !state.can_submit(),
            "Only one request may be in flight per sidecar"
        );
        assert!(!state.finished(first, true));
        assert_eq!(state.status, SaveStatus::Saving);
        assert!(state.can_submit());
        state.in_flight = Some(latest);
        assert!(
            !state.finished(first, false),
            "Stale failure must not release the latest request"
        );
        assert_eq!(state.in_flight, Some(latest));
        assert_eq!(state.status, SaveStatus::Saving);
        assert!(state.finished(latest, true));
        assert_eq!(state.status, SaveStatus::Saved);
        assert!(!state.finished(first, false));
        assert_eq!(state.status, SaveStatus::Saved);
    }

    #[test]
    fn autosave_failure_is_retryable_without_accepting_stale_results() {
        let mut state = AutosaveState::default();
        state.changed();
        let failed = state.revision;
        state.in_flight = Some(failed);
        assert!(state.finished(failed, false));
        assert_eq!(state.status, SaveStatus::Failed);
        assert!(
            !state.can_submit(),
            "A failure must wait for an edit or explicit retry"
        );
        state.changed();
        let retry = state.revision;
        assert!(state.can_submit());
        state.in_flight = Some(retry);
        assert!(!state.finished(failed, true));
        assert_eq!(state.status, SaveStatus::Saving);
        assert!(state.finished(retry, true));
        assert_eq!(state.status, SaveStatus::Saved);
    }

    #[test]
    fn autosave_failure_of_superseded_revision_does_not_fail_latest_edits() {
        let mut state = AutosaveState::default();
        state.changed();
        state.in_flight = Some(state.revision);
        let older = state.revision;
        state.changed();
        assert!(!state.finished(older, false));
        assert_eq!(state.status, SaveStatus::Saving);
        assert!(state.can_submit());
    }

    #[test]
    fn switching_sessions_preserves_undo_redo_and_link_mode() {
        let mut editor = Editor::default();
        editor.linked_edits = false;
        let original = serde_json::to_value(&editor.project).unwrap();
        editor
            .execute(Edit::Reframe {
                vertical: true,
                center: 0.25,
            })
            .unwrap();
        let first = serde_json::to_value(&editor.project).unwrap();
        editor
            .execute(Edit::Reframe {
                vertical: true,
                center: 0.75,
            })
            .unwrap();
        let second = serde_json::to_value(&editor.project).unwrap();
        assert!(editor.undo());
        let mut sessions = EditorSessions::default();
        sessions.insert(PathBuf::from("first.mkv"), editor);
        sessions.insert(PathBuf::from("other.mkv"), Editor::default());
        let mut restored = sessions.remove(&PathBuf::from("first.mkv")).unwrap();
        assert!(!restored.linked_edits);
        assert_eq!(serde_json::to_value(&restored.project).unwrap(), first);
        assert!(restored.redo());
        assert_eq!(serde_json::to_value(&restored.project).unwrap(), second);
        assert!(restored.undo());
        assert!(restored.undo());
        assert_eq!(serde_json::to_value(&restored.project).unwrap(), original);
        assert!(!restored.undo());
    }

    #[test]
    fn session_eviction_keeps_recently_used_history_and_other_entries() {
        let mut sessions = EditorSessions::default();
        for index in 0..8 {
            sessions.insert(PathBuf::from(format!("{index}.mkv")), Editor::default());
        }
        let recent = PathBuf::from("0.mkv");
        let editor = sessions.remove(&recent).unwrap();
        sessions.insert(recent.clone(), editor);
        sessions.insert(PathBuf::from("8.mkv"), Editor::default());
        assert_eq!(sessions.entries.len(), 8);
        assert!(sessions.remove(&PathBuf::from("1.mkv")).is_none());
        assert!(sessions.remove(&recent).is_some());
        assert!(sessions.remove(&PathBuf::from("2.mkv")).is_some());
        assert!(sessions.remove(&PathBuf::from("8.mkv")).is_some());
    }
}

#[cfg(test)]
mod gpu_fixture_timing_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn timing_samples_are_opt_in_bounded_and_correlated_to_frame_ids() {
        let timing = GpuPlaybackTickTiming { requested_timer: Duration::from_millis(1), timer_wait: Duration::from_millis(16), ..Default::default() };
        let mut normal = GpuPlaybackSmoke::default();
        normal.record_tick(timing);
        normal.observe_tick(Some(0), 1, 1);
        assert!(normal.timings.is_empty());
        assert_eq!(normal.timings.capacity(), 0);
        let mut eof = GpuPlaybackSmoke { until_eof: true, ..Default::default() };
        for frame in 0..905 {
            eof.record_tick(timing);
            eof.observe_tick(Some(frame), frame + 1, frame + 1);
        }
        assert_eq!(eof.timings.len(), 900);
        assert_eq!(eof.timing_overflow, 5);
        let last = eof.timings.last().unwrap();
        assert_eq!(last.clock_frame, Some(899));
        assert_eq!(last.submitted_id, 900);
        assert_eq!(last.clock_advance, Some(1));
        assert_eq!(last.published_advance, 1);
        let report = eof.timing_report();
        assert_eq!(report["actual_timer_wait_ms"]["median"], 16.);
        assert_eq!(report["requested_timer_ms"]["median"], 1.);
        assert_eq!(report["largest_observation_gaps"].as_array().unwrap().len(), 8);
    }
}

#[cfg(test)]
mod pointer_dedup_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn source_frame_capacity_matches_rounded_nanoseconds_without_losing_restorable_frames() {
        for fps in [30, 60, 61, 144, 240] {
            for speed in [0.5, 1., 1.5, 4.] {
                for frames in [1, 61, 1076, 7177] {
                    let ns = (frames as f64 / f64::from(fps) * speed * 1e9).round() as u64;
                    assert_eq!(available_source_frames(ns, fps, speed), frames,
                        "Rounded source availability lost a frame: frames={frames}, fps={fps}, speed={speed}, ns={ns}");
                    assert!(available_source_frames(ns - 1, fps, speed) < frames,
                        "Source availability admitted a rounded rewind beyond the source");
                }
            }
        }
        assert_eq!(available_source_frames(17_933_333_333, 60, 1.), 1076);
        assert_eq!(available_source_frames(0, 60, 1.), 0);
        assert_eq!(available_source_frames(u64::MAX, 240, 0.5), 240 * 86_400);
    }
    #[test]
    fn linked_drag_limits_preserve_gaps_and_respect_every_track_neighbor_and_source() {
        let mut editor = Editor::default();
        editor.execute(Edit::Import { at: 0, asset: MediaAsset { id: 0, path: "drag-limits.mkv".into(), source_stamp: None,
            duration_ns: 10_000_000_000, width: 2560, height: 1440,
            streams: vec![core::MediaStream { index: 0, kind: TrackKind::Video, name: "Video".into() },
                core::MediaStream { index: 1, kind: TrackKind::Audio, name: "Microphone".into() }] } }).unwrap();
        editor.project.fps = 60;
        for clip in &mut editor.project.clips { clip.start = 120; clip.duration = 120; clip.source_in_ns = 2_000_000_000; }
        let selected = core::pieces(&editor.project)[0].clone();
        let original_linked = editor.project.clips.clone();
        for clip in &original_linked {
            let audio = clip.track != selected.track;
            let mut before = clip.clone(); before.id += 100; before.group += 100; before.start = 0; before.duration = if audio { 100 } else { 90 };
            let mut after = clip.clone(); after.id += 200; after.group += 200; after.start = if audio { 280 } else { 300 };
            editor.project.clips.extend([before, after]);
        }
        let before = serde_json::to_value(&editor.project).unwrap();
        assert_eq!(drag_frame_limits(&editor.project, &selected, DragKind::Move), (-120, i64::MAX));
        assert_eq!(drag_frame_limits(&editor.project, &selected, DragKind::Left), (-20, 119));
        assert_eq!(drag_frame_limits(&editor.project, &selected, DragKind::Right), (-119, 40));
        assert_eq!(serde_json::to_value(&editor.project).unwrap(), before);
        let mut moved = editor.project.clone();
        for clip in moved.clips.iter_mut().filter(|clip| clip.group == selected.group) { clip.start = 260; }
        assert!(moved_group_overlaps(&moved, selected.group));
        for clip in moved.clips.iter_mut().filter(|clip| clip.group == selected.group) { clip.start = 500; }
        assert!(!moved_group_overlaps(&moved, selected.group), "Manual movement may cross neighbors into a free gap");
        editor.project.clips.retain(|clip| clip.group == selected.group);
        editor.project.clips[1].source_in_ns = 1_000_000_000;
        assert_eq!(drag_frame_limits(&editor.project, &selected, DragKind::Left), (-60, 119));
        assert_eq!(drag_frame_limits(&editor.project, &selected, DragKind::Move).0, -120);
    }
    #[test]
    fn trim_dedup_requires_same_edge_clip_and_quantized_target() {
        let left = Edit::Sequence(QuickEdit::TrimStart { id: 7, to: -12 });
        assert!(!same_trim_target(None, &left));
        assert!(same_trim_target(Some(&left), &left));
        assert!(!same_trim_target(Some(&left), &Edit::Sequence(QuickEdit::TrimStart { id: 7, to: -11 })));
        assert!(!same_trim_target(Some(&left), &Edit::Sequence(QuickEdit::TrimStart { id: 8, to: -12 })));
        let right = Edit::Sequence(QuickEdit::TrimEnd { id: 7, to: 120 });
        assert!(same_trim_target(Some(&right), &right));
        assert!(!same_trim_target(Some(&left), &right));
        assert!(!same_trim_target(Some(&right), &Edit::Sequence(QuickEdit::TrimEnd { id: 7, to: 121 })));
        let reorder = Edit::Sequence(QuickEdit::Reorder { id: 7, index: 0 });
        assert!(!same_trim_target(Some(&reorder), &reorder));
    }
}

#[path = "editor_recovery_smoke.rs"]
mod recovery_smoke;
pub use recovery_smoke::RecoverySmoke;

#[path = "editor_resource_smoke.rs"]
mod resource_smoke;
pub use resource_smoke::EditorResourceSmoke;

#[path = "editor_autosave_smoke.rs"]
mod autosave_smoke;
pub use autosave_smoke::AutosaveSmoke;

#[path = "editor_keyboard_smoke.rs"]
mod keyboard_smoke;
pub use keyboard_smoke::EditorKeyboardSmoke;

#[path = "editor_clip_show_eval.rs"]
mod clip_show_eval;
pub use clip_show_eval::ClipShowEvaluation;
