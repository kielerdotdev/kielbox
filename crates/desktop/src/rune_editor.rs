//! Non-destructive editor model presented through Rune; decoding/export never run in a view.
use crate::{
    recordings,
    rune_theme::{self as theme, Btn, c, icons},
    rune_ui::{self as ui, State as AppState, color},
};
use editor_core::{
    self as core, Edit, Editor, PlaybackOutput, PlaybackSession, Project, ScrubSession,
};
use rune::{El, Key, KeyEvent, NativeSurface, SurfaceFit, TextStyle, col, el, row};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, atomic::Ordering, mpsc},
};
enum Loaded {
    Video(Editor, ScrubSession),
    Image(crate::rune_annotation::Annotation),
}
struct Opened {
    path: PathBuf,
    loaded: Loaded,
}
#[derive(Default)]
pub struct State {
    pub editor: Option<Editor>,
    pub path: Option<PathBuf>,
    loading: Option<mpsc::Receiver<Result<Opened, String>>>,
    opening_image: Option<bool>,
    scrub: Option<ScrubSession>,
    playback: Option<PlaybackSession>,
    surface: NativeSurface,
    pub position: u64,
    selected: Option<core::Id>,
    pub message: String,
    export: Option<(
        Arc<core::JobProgress>,
        mpsc::Receiver<Result<PathBuf, String>>,
    )>,
    save: Option<SaveWorker>,
    export_worker: Option<std::thread::JoinHandle<()>>,
    save_revision: u64,
    saved_revision: u64,
    pub annotation: Option<crate::rune_annotation::Annotation>,
    /// Drawn width of the timeline strip, for placing the playhead.
    pub strip_width: f32,
    /// A trim grip being dragged: previewed on the strip, applied on release.
    trim: Option<TrimDrag>,
    /// Preview frames shown so far.
    frames: u64,
}
#[derive(Clone, Copy)]
struct TrimDrag {
    id: core::Id,
    left: bool,
    /// The edge's frame when the drag began.
    from: i64,
    to: i64,
    min: i64,
    max: i64,
}
struct SaveWorker {
    pending: Arc<Mutex<Option<(u64, Project)>>>,
    wake: Option<mpsc::SyncSender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
    results: mpsc::Receiver<(u64, Result<(), String>)>,
}
impl SaveWorker {
    fn new(path: PathBuf) -> Result<Self, String> {
        let pending = Arc::new(Mutex::new(None::<(u64, Project)>));
        let jobs = pending.clone();
        let (wake, rx) = mpsc::sync_channel(1);
        let (tx, results) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("kielbox-edit-save".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    loop {
                        let Some((revision, project)) = jobs.lock().unwrap().take() else {
                            break;
                        };
                        let result = project.save(&path).map_err(|e| format!("{e:#}"));
                        let _ = tx.send((revision, result));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            pending,
            wake: Some(wake),
            worker: Some(worker),
            results,
        })
    }
    fn submit(&self, revision: u64, project: Project) {
        *self.pending.lock().unwrap() = Some((revision, project));
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }
}
impl Drop for SaveWorker {
    fn drop(&mut self) {
        self.wake.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl State {
    pub fn open(&mut self, path: PathBuf, wake: rune::Waker) {
        if !self.prepare_to_leave() {
            return;
        }
        self.stop();
        let (tx, rx) = mpsc::sync_channel(1);
        self.opening_image = Some(recordings::is_image(&path));
        self.loading = Some(rx);
        self.message = "Opening…".into();
        // A prior save worker drains its final pending project even after this observer is dropped.
        self.editor = None;
        self.scrub = None;
        self.save = None;
        self.path = None;
        self.selected = None;
        self.annotation = None;
        if let Err(e) = std::thread::Builder::new()
            .name("kielbox-open-media".into())
            .spawn(move || {
                let result = (|| -> anyhow::Result<Opened> {
                    let loaded = if recordings::is_image(&path) {
                        let recovery = image_recovery_path(&path);
                        let recovered = recovery.exists();
                        let source = if recovered {
                            toolkit::image_editor::Source::Project(toolkit::annotation::project::load(&recovery)?)
                        } else if path.extension().is_some_and(|e| e == "ssxe") {
                            toolkit::image_editor::Source::Project(
                                toolkit::annotation::project::load(&path)?,
                            )
                        } else {
                            toolkit::image_editor::Source::Image(toolkit::types::Frame::from_image(
                                image::open(&path)?.into_rgba8(),
                            ))
                        };
                        let mut annotation = crate::rune_annotation::Annotation::new(source, wake.clone())?;
                        annotation.set_recovery_path(recovery);
                        if recovered { annotation.mark_recovered(); annotation.message = "Restored editable recovery. Original pixels are retained; export PNG for flattened sharing.".into(); }
                        Loaded::Image(annotation)
                    } else {
                        let saved = recordings::edit_path_for_open(&path);
                        let mut editor = if saved.exists() {
                            Editor::new(Project::load(&saved)?)
                        } else {
                            let mut e = Editor::default();
                            e.execute(Edit::Sequence(core::QuickEdit::Add(core::probe(&path)?)))?;
                            e
                        };
                        editor.validate_sources_cancellable(|| false)?;
                        let scrub = ScrubSession::new(editor.project.clone(), (1280, 720))?;
                        let w = wake.clone();
                        scrub.set_waker(move || w());
                        scrub.request_exact(0);
                        Loaded::Video(editor, scrub)
                    };
                    Ok(Opened { path, loaded })
                })();
                let _ = tx.send(result.map_err(|e| format!("{e:#}")));
                wake();
            })
        {
            self.loading = None;
            self.opening_image = None;
            self.message = e.to_string();
        }
    }
    pub fn has_frame(&self) -> bool {
        self.frames > 0
    }
    pub(crate) fn split_at_position(&mut self) {
        self.split();
    }
    /// Wait for the preview to show the current position, for scripted fixtures.
    pub(crate) fn settle(&mut self, timeout: std::time::Duration) {
        let (seen, start) = (self.frames, std::time::Instant::now());
        while self.frames == seen && start.elapsed() < timeout {
            self.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    pub fn playing(&self) -> bool {
        self.playback.is_some()
    }
    fn stop(&mut self) {
        if let Some(p) = self.playback.take() {
            p.stop();
        }
    }
    pub fn seek(&mut self, frame: u64) {
        self.stop();
        self.position = self
            .editor
            .as_ref()
            .map_or(0, |e| frame.min(e.project.duration().saturating_sub(1)));
        if let Some(s) = &self.scrub {
            s.request_exact(self.position);
        }
    }
    pub fn play(&mut self) {
        if self.playback.is_some() {
            self.stop();
            return;
        }
        if let Some(e) = &self.editor {
            match PlaybackSession::new(
                e.project.clone(),
                self.position,
                (1280, 720),
                PlaybackOutput::Device,
            ) {
                Ok(p) => {
                    p.play();
                    self.playback = Some(p);
                }
                Err(e) => self.message = format!("{e:#}"),
            }
        }
    }
    pub fn execute(&mut self, edit: Edit) {
        self.stop();
        if let Some(e) = &mut self.editor {
            match e.execute(edit) {
                Ok(()) => self.changed(),
                Err(e) => self.message = format!("{e:#}"),
            }
        }
    }
    fn changed(&mut self) {
        if let Some(e) = &self.editor {
            self.save_revision += 1;
            if let Some(save) = &self.save {
                save.submit(self.save_revision, e.project.clone());
            }
            if let Some(scrub) = &self.scrub {
                if let Err(error) = scrub.replace_project(e.project.clone(), (1280, 720)) {
                    self.message = error.to_string();
                }
                scrub.request_exact(self.position.min(e.project.duration().saturating_sub(1)));
            }
        }
    }
    fn undo(&mut self, redo: bool) {
        self.stop();
        if let Some(e) = &mut self.editor {
            if if redo { e.redo() } else { e.undo() } {
                self.changed();
            }
        }
    }
    fn selected(&self) -> Option<core::Id> {
        let e = self.editor.as_ref()?;
        self.selected
            .filter(|id| e.project.clips.iter().any(|c| c.id == *id))
            .or_else(|| core::pieces(&e.project).first().map(|c| c.id))
    }
    fn split(&mut self) {
        if let Some(id) = self.selected() {
            self.execute(Edit::Split {
                id,
                at: self.position,
            });
        }
    }
    fn delete(&mut self) {
        if let Some(id) = self.selected() {
            self.execute(Edit::Delete { id, ripple: false });
        }
    }
    /// How far each edge of a piece may move, in timeline frames: as far as its source media
    /// reaches. The left edge may go below zero when restoring the start of the first piece;
    /// the ripple in `trim` keeps the result on the timeline.
    fn trim_limits(&self, id: core::Id) -> Option<(i64, i64)> {
        let project = &self.editor.as_ref()?.project;
        let clip = core::pieces(project).into_iter().find(|c| c.id == id)?;
        let fps = f64::from(project.fps);
        let frames = |ns: u64| (ns as f64 / 1e9 * fps / clip.speed.max(1e-6)).floor() as i64;
        let (before, after) = match clip.content {
            core::Content::Media { asset, .. } => {
                let total = project
                    .assets
                    .iter()
                    .find(|a| a.id == asset)
                    .map_or(0, |a| a.duration_ns);
                let used = (clip.duration as f64 / fps * clip.speed * 1e9) as u64;
                (
                    frames(clip.source_in_ns),
                    frames(total.saturating_sub(clip.source_in_ns + used)),
                )
            }
            core::Content::Title { .. } => (0, i64::from(u32::MAX)),
        };
        Some((clip.start as i64 - before, clip.end() as i64 + after))
    }
    /// Move one edge of a piece to `to` and ripple the pieces after it, as one undo step, so
    /// the timeline stays packed: cutting closes the gap, restoring makes room.
    fn trim(&mut self, id: core::Id, left: bool, to: i64) {
        let Some(project) = self.editor.as_ref().map(|e| &e.project) else {
            return;
        };
        let pieces = core::pieces(project);
        let Some(clip) = pieces.iter().find(|c| c.id == id) else {
            return;
        };
        let (start, end) = (clip.start as i64, clip.end() as i64);
        let (min, max) = self.trim_limits(id).unwrap_or((start, end));
        let to = if left {
            to.clamp(min, end - 1)
        } else {
            to.clamp(start + 1, max)
        };
        let later: Vec<(core::Id, i64)> = pieces
            .iter()
            .filter(|c| c.id != id && c.start as i64 >= end)
            .map(|c| (c.id, c.start as i64))
            .collect();
        // Frames the piece loses (negative: frames it regains).
        let cut = if left { to - start } else { end - to };
        if cut == 0 || (left && to >= end) || (!left && to <= start) {
            return;
        }
        let shift = |edits: &mut Vec<Edit>, by: i64| {
            // Moving right goes last-first and moving left first-last, so pieces never cross.
            let mut moves: Vec<_> = later
                .iter()
                .map(|(q, at)| Edit::Move {
                    id: *q,
                    to: (at + by).max(0) as u64,
                })
                .collect();
            if by > 0 {
                moves.reverse();
            }
            edits.extend(moves);
        };
        let mut edits = Vec::new();
        if cut > 0 {
            if left {
                edits.push(Edit::TrimLeft { id, to: to as u64 });
                edits.push(Edit::Move {
                    id,
                    to: start as u64,
                });
            } else {
                edits.push(Edit::TrimRight { id, to: to as u64 });
            }
            shift(&mut edits, -cut);
        } else {
            let grow = -cut;
            shift(&mut edits, grow);
            if left {
                // Slide right first so the restored start never precedes frame zero.
                edits.push(Edit::Move {
                    id,
                    to: (start + grow) as u64,
                });
                edits.push(Edit::TrimLeft {
                    id,
                    to: start as u64,
                });
            } else {
                edits.push(Edit::TrimRight { id, to: to as u64 });
            }
        }
        self.selected = Some(id);
        self.execute(Edit::Batch(edits));
        let new_end = end - cut;
        self.seek(if left {
            start as u64
        } else {
            (new_end - 1).max(0) as u64
        });
    }
    pub fn key(&mut self, event: &KeyEvent) -> bool {
        if let Some(a) = &mut self.annotation {
            return a.key(event);
        }
        match &event.key {
            Key::Text(t) if t == " " => self.play(),
            Key::Text(t) if t == "s" => self.split(),
            Key::Backspace | Key::Delete => self.delete(),
            Key::Left => self.seek(self.position.saturating_sub(1)),
            Key::Right => self.seek(self.position + 1),
            Key::Command('z') => self.undo(event.mods.shift),
            Key::Command('y') => self.undo(true),
            Key::Home => self.seek(0),
            Key::End => self.seek(u64::MAX),
            _ => return false,
        }
        true
    }
    pub fn export(&mut self, path: PathBuf, wake: rune::Waker) {
        if self.export.is_some() {
            return;
        }
        let Some(e) = &self.editor else {
            return;
        };
        let project = e.project.clone();
        let progress = Arc::new(core::JobProgress::default());
        let p = progress.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("kielbox-export".into())
            .spawn(move || {
                let options = core::ExportOptions {
                    output: path,
                    start: 0,
                    end: None,
                    audio: core::AudioExport::Mix,
                    bitrate: 16_000_000,
                    video_codec: core::ExportVideoCodec::H264,
                };
                let result = core::export(&project, &options, &p).map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
                wake();
            }) {
            Ok(worker) => {
                self.export_worker = Some(worker);
                self.export = Some((progress, rx));
                self.message = "Exporting…".into();
            }
            Err(e) => self.message = e.to_string(),
        }
    }
    fn pixels(&self, w: u32, h: u32, mut rgba: Vec<u8>, bgra: bool) -> Result<(), String> {
        if !bgra {
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        self.surface.push_bytes(
            rune::native_surface::ByteFrame {
                width: w,
                height: h,
                format: rune::native_surface::Format::Bgra,
                color: Default::default(),
                planes: [rgba, vec![]],
            },
            self.position,
        )
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.loading {
            match rx.try_recv() {
                Ok(result) => {
                    self.loading = None;
                    self.opening_image = None;
                    changed = true;
                    match result {
                        Ok(Opened { path, loaded }) => {
                            self.position = 0;
                            self.save_revision = 0;
                            self.saved_revision = 0;
                            self.message = String::new();
                            match loaded {
                                Loaded::Video(editor, scrub) => {
                                    self.editor = Some(editor);
                                    self.scrub = Some(scrub);
                                    match SaveWorker::new(recordings::edit_path(&path)) {
                                        Ok(save) => self.save = Some(save),
                                        Err(e) => self.message = e,
                                    }
                                }
                                Loaded::Image(a) => self.annotation = Some(a),
                            }
                            self.path = Some(path);
                        }
                        Err(e) => self.message = e,
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.loading = None;
                    self.opening_image = None;
                    self.message = "Media worker stopped unexpectedly.".into();
                    changed = true;
                }
                _ => {}
            }
        }
        if let Some(save) = &self.save {
            for (revision, result) in save.results.try_iter() {
                changed = true;
                match result {
                    Ok(()) => self.saved_revision = revision,
                    Err(e) => self.message = format!("Edits could not be saved: {e}"),
                }
            }
        }
        if let Some(p) = &self.playback {
            self.position = p.position();
            changed = true;
            if let Some(frame) = p.frame() {
                if let Err(e) = self.pixels(frame.width, frame.height, frame.bgra, true) {
                    self.message = e;
                }
            }
            if let Ok(e) = p.errors.try_recv() {
                self.message = e;
                self.stop();
            } else if p.finished() {
                self.stop();
            }
        } else if let Some(scrub) = &self.scrub {
            if let Some(result) = scrub.frame() {
                changed = true;
                match result {
                    Ok(f) => match self.pixels(f.width, f.height, f.bgra, true) {
                        Ok(()) => self.frames += 1,
                        Err(e) => self.message = e,
                    },
                    Err(e) => self.message = e,
                }
            }
        }
        if let Some((_, rx)) = &self.export {
            match rx.try_recv() {
                Ok(result) => {
                    self.export = None;
                    changed = true;
                    self.message = match result {
                        Ok(path) => format!("Saved {}", path.display()),
                        Err(e) => e,
                    };
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.export = None;
                    self.message = "Export worker stopped unexpectedly.".into();
                    changed = true;
                }
                _ => {}
            }
        }
        if let Some(a) = &mut self.annotation {
            changed |= a.poll();
        }
        changed
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.stop();
        if let Some((p, _)) = &self.export {
            p.cancel.store(true, Ordering::Release);
        }
        if let Some(worker) = self.export_worker.take() {
            let _ = worker.join();
        }
    }
}
impl State {
    pub fn owns_feature(&self, feature: crate::features::Feature) -> bool {
        let image = self.path.as_deref().map(recordings::is_image).or(self.opening_image);
        match feature {
            crate::features::Feature::ImageEditing => image == Some(true),
            crate::features::Feature::VideoEditing => image == Some(false),
            _ => false,
        }
    }
    pub fn feature_change_blocked(&self) -> bool {
        self.loading.is_some() || self.export.is_some() || self.playing()
            || self.save_revision != self.saved_revision
            || self.annotation.as_ref().is_some_and(|a| a.dirty())
    }
    /// Shared gate for close, replacement and application quit. On false keep the
    /// window/editor alive, poll normally, and show editor/annotation messages.
    /// Image checkpoints are asynchronous; the user can retry after acknowledgement.
    pub fn prepare_to_leave(&mut self) -> bool {
        if let Some(a) = &mut self.annotation {
            return a.prepare_to_leave();
        }
        if self.export.is_some() {
            self.message = "Cancel the export before closing this clip.".into();
            return false;
        }
        if self.editor.is_some() && self.save_revision != self.saved_revision {
            self.message =
                "Wait for edits to finish saving before closing. If saving failed, use Retry save."
                    .into();
            return false;
        }
        true
    }
    /// Close the open clip only after its editable work is durable.
    pub fn close(&mut self) -> bool {
        if !self.prepare_to_leave() {
            return false;
        }
        self.stop();
        self.loading = None;
        self.opening_image = None;
        self.editor = None;
        self.scrub = None;
        self.save = None;
        self.path = None;
        self.selected = None;
        self.annotation = None;
        self.message.clear();
        true
    }
}

use crate::recordings::image_recovery_path;

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0);
    let whole = s as u64;
    format!("{}:{:04.1}", whole / 60, s - (whole / 60 * 60) as f64)
}

const STRIP_H: f32 = 68.0;

/// Ruler labels: tenths of a second when the clip is short enough for whole seconds to repeat.
fn tick(length: f64, step: u32) -> String {
    let at = length * f64::from(step) / 4.0;
    if length < 20.0 {
        format!("{at:.1} s")
    } else {
        ui::clock(at)
    }
}

/// A trim grip straddling a piece edge at `x`: drag it to move that edge.
fn trim_grip(id: core::Id, left: bool, x: f32, value: i64, min: i64, max: i64) -> El {
    const W: f32 = 14.0;
    el().key(if left {
        "trim-grip-start"
    } else {
        "trim-grip-end"
    })
    .role("slider")
    .label(if left {
        "Trim start. Left and right adjust one frame."
    } else {
        "Trim end. Left and right adjust one frame."
    })
    .focusable(true)
    .range_value(value as f64, min as f64, max as f64)
    .on_key(move |s: &mut AppState, event, _| {
        let step = if event.mods.shift { 10 } else { 1 };
        let delta = match event.key {
            Key::Left => -step,
            Key::Right => step,
            _ => return false,
        };
        if let Some(piece) = s
            .editor
            .editor
            .as_ref()
            .and_then(|e| core::pieces(&e.project).into_iter().find(|p| p.id == id))
        {
            let at = if left { piece.start } else { piece.end() } as i64;
            s.editor.trim(id, left, at + delta);
        }
        true
    })
    .w(W)
    .h(STRIP_H)
    .absolute(x - W / 2.0, 0.0)
    .center()
    .cursor(rune::Cursor::ResizeColumn)
    .child(
        el().w(4.0)
            .h(28.0)
            .radius(2.0)
            .fill(c(theme::T1))
            .box_shadow(rune::Shadow {
                blur: 3.0,
                color: theme::black(0.5),
                ..rune::Shadow::default()
            }),
    )
    .on_drag(move |s: &mut AppState, d, tx| {
        tx.instant();
        let e = &mut s.editor;
        match d.phase {
            rune::Phase::Down => {
                let Some(project) = e.editor.as_ref().map(|e| &e.project) else {
                    return;
                };
                let Some(piece) = core::pieces(project).into_iter().find(|p| p.id == id) else {
                    return;
                };
                let from = if left { piece.start } else { piece.end() } as i64;
                let (min, max) = e.trim_limits(id).unwrap_or((from, from));
                let (min, max) = if left {
                    (min, piece.end() as i64 - 1)
                } else {
                    (piece.start as i64 + 1, max)
                };
                e.selected = Some(id);
                e.trim = Some(TrimDrag {
                    id,
                    left,
                    from,
                    to: from,
                    min,
                    max,
                });
            }
            rune::Phase::Move => {
                let Some(total) = e.editor.as_ref().map(|e| e.project.duration().max(1)) else {
                    return;
                };
                let per_point = total as f32 / e.strip_width.max(1.0);
                if let Some(t) = &mut e.trim {
                    t.to = (t.from + (d.dx * per_point).round() as i64).clamp(t.min, t.max);
                    // Show the frame the cut lands on.
                    let at = if t.left { t.to } else { t.to - 1 };
                    let t = *t;
                    if (0..total as i64).contains(&at) {
                        e.seek(at as u64);
                    }
                    e.trim = Some(t);
                }
            }
            rune::Phase::Up => {
                if let Some(t) = e.trim.take()
                    && !d.cancelled()
                    && t.to != t.from
                {
                    e.trim(t.id, t.left, t.to);
                }
            }
        }
    })
}

pub fn view(s: &AppState) -> El {
    if s.editor.annotation.is_some() {
        return crate::rune_annotation::view(s);
    }
    let e = &s.editor;
    let saved = e.save_revision == e.saved_revision;
    let title = e
        .path
        .as_deref()
        .map(|p| ui::clip_title(s, p))
        .unwrap_or_else(|| "Editor".into());
    let mut bar = row()
        .w_full()
        .px(24.0)
        .py(12.0)
        .gap(12.0)
        .center_y()
        .child(
            Btn::new("editor-back", "Library")
                .ghost()
                .icon(&icons::BACK)
                .on(|s, _| s.go(ui::Page::Library))
                .build(),
        )
        .child(
            rune::text(title)
                .font(theme::H2)
                .color(c(theme::T1))
                .role("heading")
                .level(1)
                .single_line()
                .truncate()
                .min_w(0.0),
        );
    if e.editor.is_some() {
        bar = bar.child(theme::small(if saved { "Saved" } else { "Saving…" }));
    }
    bar = bar.child(theme::spacer());
    if e.editor.is_some() {
        bar = bar
            .child(
                Btn::icon_only("undo-edit", "Undo", &icons::UNDO)
                    .on(|s, _| s.editor.undo(false))
                    .build(),
            )
            .child(
                Btn::icon_only("redo-edit", "Redo", &icons::REDO)
                    .on(|s, _| s.editor.undo(true))
                    .build(),
            );
    }
    if e.export.is_some() {
        bar = bar.child(theme::small("Exporting…")).child(
            Btn::new("cancel-export", "Cancel export")
                .on(|s, _| {
                    if let Some((p, _)) = &s.editor.export {
                        p.cancel.store(true, Ordering::Release);
                    }
                })
                .build(),
        );
    } else {
        bar = bar.child(
            Btn::new("editor-export", "Export")
                .primary()
                .disabled(e.editor.is_none())
                .on(|_, tx| {
                    tx.save_file(
                        rune::FileDialogOptions {
                            file_name: Some("clip.mp4".into()),
                            ..Default::default()
                        }
                        .filter("MP4", &["mp4"]),
                        |s: &mut AppState, result, _| match result {
                            Ok(Some(path)) => s.editor.export(path, s.wake.clone()),
                            Err(e) => s.notice = e.to_string(),
                            _ => {}
                        },
                    )
                })
                .build(),
        );
    }
    let mut main = col().grow(1.0).min_h(0.0).px(24.0).pb(18.0).gap(14.0);
    let mut preview = el()
        .key("media-preview")
        .grow(1.0)
        .min_h(120.0)
        .w_full()
        .radius(8.0)
        .clip_rounded(8.0)
        .fill(color(0x000000))
        .native_surface(&e.surface, SurfaceFit::Fit);
    if e.editor.is_none() {
        preview = preview.center().child(
            col()
                .gap(10.0)
                .items_center()
                .child(theme::glyph(&icons::FILM, 28.0, c(theme::T3)))
                .child(theme::small(if e.loading.is_some() {
                    "Opening…"
                } else {
                    "Open a recording or image from your Library."
                }))
                .child(
                    Btn::new("editor-open", "Open file…")
                        .hint("Ctrl O")
                        .on(|s, tx| s.browse(tx))
                        .build(),
                ),
        );
    }
    main = main.child(preview);
    if let Some(editor) = &e.editor {
        let project = &editor.project;
        let duration = project.duration().max(1);
        let position = e.position;
        main = main.child(
            row()
                .w_full()
                .gap(4.0)
                .center_y()
                .child(
                    Btn::icon_only("step-back", "Previous frame", &icons::PREV)
                        .on(|s, _| s.editor.seek(s.editor.position.saturating_sub(1)))
                        .build(),
                )
                .child(
                    Btn::icon_only(
                        "play",
                        if e.playing() { "Pause" } else { "Play" },
                        if e.playing() {
                            &icons::PAUSE
                        } else {
                            &icons::PLAY
                        },
                    )
                    .on(|s, _| s.editor.play())
                    .build(),
                )
                .child(
                    Btn::icon_only("step-next", "Next frame", &icons::NEXT)
                        .on(|s, _| s.editor.seek(s.editor.position + 1))
                        .build(),
                )
                .child(
                    rune::text(clock(project.seconds(position)))
                        .font(theme::NUM)
                        .color(c(theme::T1))
                        .ml(6.0),
                )
                .child(theme::spacer())
                .child(theme::small("Length "))
                .child(
                    rune::text(clock(project.seconds(duration)))
                        .font(theme::NUM)
                        .color(c(theme::T1)),
                ),
        );
        // The strip: pieces at their timeline positions; drag the strip to seek, or the grips
        // on the selected piece's edges to trim it.
        let selected = e.selected();
        let width = e.strip_width.max(1.0);
        let x_of = |frame: i64| frame as f32 / duration as f32 * width;
        let mut strip = el()
            .key("seek")
            .role("slider")
            .label("Seek. Arrow keys move one frame; Home and End jump to the ends.")
            .focusable(true)
            .on_key(|s: &mut AppState, event, _| {
                match event.key {
                    Key::Left => {
                        s.editor
                            .seek(s.editor.position.saturating_sub(if event.mods.shift {
                                10
                            } else {
                                1
                            }))
                    }
                    Key::Right => {
                        s.editor
                            .seek(s.editor.position.saturating_add(if event.mods.shift {
                                10
                            } else {
                                1
                            }))
                    }
                    Key::Home => s.editor.seek(0),
                    Key::End => s.editor.seek(u64::MAX),
                    _ => return false,
                }
                true
            })
            .range_value(position as f64, 0.0, duration.saturating_sub(1) as f64)
            .w_full()
            .h(STRIP_H)
            .radius(6.0)
            .fill(c(theme::DEEP))
            .on_layout(|s: &mut AppState, r| {
                if (s.editor.strip_width - r[2]).abs() > 0.5 {
                    s.editor.strip_width = r[2];
                }
            })
            .on_drag(|s: &mut AppState, d, tx| {
                tx.instant();
                if let Some(editor) = &s.editor.editor {
                    let total = editor.project.duration();
                    let fraction = (d.x / d.w.max(1.0)).clamp(0.0, 1.0);
                    s.editor.seek((total as f32 * fraction) as u64);
                }
            });
        for (i, piece) in core::pieces(project).iter().enumerate() {
            let id = piece.id;
            let at = piece.start;
            let on = selected == Some(id);
            let left = x_of(piece.start as i64);
            let right = x_of(piece.end() as i64);
            let segment = col()
                .key(format!("piece-{id}"))
                .role("button")
                .focusable(true)
                .on_key(move |s: &mut AppState, event, _| {
                    if matches!(&event.key, Key::Enter)
                        || matches!(&event.key, Key::Text(t) if t == " ")
                    {
                        s.editor.selected = Some(id);
                        s.editor.seek(at);
                        return true;
                    }
                    false
                })
                .label(format!(
                    "Piece {}, {:.1} s",
                    i + 1,
                    project.seconds(piece.duration)
                ))
                .w((right - left - 2.0).max(4.0))
                .h(STRIP_H)
                .absolute(left + 1.0, 0.0)
                .radius(5.0)
                .padding(8.0)
                .justify_end()
                .fill(c(if on { 0x34363c } else { theme::BG2 }))
                .hover(c(0x303136))
                .on_press(move |s: &mut AppState| {
                    s.editor.selected = Some(id);
                    s.editor.seek(at);
                })
                .child(
                    rune::text(format!("{:.1} s", project.seconds(piece.duration)))
                        .font(theme::NUM)
                        .color(c(theme::T2))
                        .single_line(),
                );
            strip = strip.child(segment);
        }
        // The selected piece's grips, and the part a drag in progress would cut or restore.
        if let Some(piece) = core::pieces(project)
            .into_iter()
            .find(|p| Some(p.id) == selected)
        {
            let (start, end) = (piece.start as i64, piece.end() as i64);
            let drag = e.trim.filter(|t| t.id == piece.id);
            let shown_start = drag.filter(|t| t.left).map_or(start, |t| t.to);
            let shown_end = drag.filter(|t| !t.left).map_or(end, |t| t.to);
            if let Some(t) = drag {
                let (a, b) = if t.left {
                    (t.to.min(start), t.to.max(start))
                } else {
                    (t.to.min(end), t.to.max(end))
                };
                let restoring = if t.left { t.to < start } else { t.to > end };
                strip = strip.child(
                    el().key("trim-preview")
                        .w((x_of(b) - x_of(a)).max(1.0))
                        .h(STRIP_H)
                        .absolute(x_of(a).clamp(0.0, width), 0.0)
                        .fill(if restoring {
                            theme::white(0.12)
                        } else {
                            theme::black(0.6)
                        })
                        .a11y_hidden(),
                );
            }
            let frame = el()
                .key("trim-frame")
                .w((x_of(shown_end) - x_of(shown_start)).max(2.0))
                .h(STRIP_H)
                .absolute(x_of(shown_start).clamp(0.0, width), 0.0)
                .radius(6.0)
                .border(2.0, c(theme::T1))
                .a11y_hidden();
            let (min, max) = e.trim_limits(piece.id).unwrap_or((start, end));
            strip = strip
                .child(frame)
                .child(trim_grip(
                    piece.id,
                    true,
                    x_of(shown_start),
                    shown_start,
                    min,
                    end - 1,
                ))
                .child(trim_grip(
                    piece.id,
                    false,
                    x_of(shown_end),
                    shown_end,
                    start + 1,
                    max,
                ));
        }
        main = main.child(strip);
        let mut ticks = row().w_full().justify_between();
        for step in 0..=4 {
            ticks = ticks.child(
                rune::text(tick(project.seconds(duration), step))
                    .font(TextStyle::new(11.5, 440.0).lh(14.0).features("'tnum' 1"))
                    .color(c(theme::T2)),
            );
        }
        main = main.child(ticks.mt(-8.0).a11y_hidden());
        let mut actions = row()
            .w_full()
            .gap(8.0)
            .center_y()
            .flex_wrap()
            .child(
                Btn::new("trim-start", "Start here")
                    .disabled(selected.is_none())
                    .on(|s, _| {
                        if let Some(id) = s.editor.selected() {
                            let at = s.editor.position as i64;
                            s.editor.trim(id, true, at);
                        }
                    })
                    .build(),
            )
            .child(
                Btn::new("trim-end", "End here")
                    .disabled(selected.is_none())
                    .on(|s, _| {
                        if let Some(id) = s.editor.selected() {
                            let at = s.editor.position as i64 + 1;
                            s.editor.trim(id, false, at);
                        }
                    })
                    .build(),
            )
            .child(el().w(12.0))
            .child(
                Btn::new("split", "Split")
                    .icon(&icons::SPLIT)
                    .hint("S")
                    .on(|s, _| s.editor.split())
                    .build(),
            )
            .child(
                Btn::new("delete-piece", "Delete")
                    .icon(&icons::TRASH)
                    .disabled(selected.is_none())
                    .on(|s, _| s.editor.delete())
                    .build(),
            )
            .child(theme::spacer());
        for track in project
            .tracks
            .iter()
            .filter(|t| t.kind == core::TrackKind::Audio)
        {
            let id = track.id;
            let mic = track.name.to_lowercase().contains("mic");
            actions = actions.child(
                row()
                    .gap(8.0)
                    .center_y()
                    .child(theme::glyph(
                        if mic { &icons::MIC } else { &icons::SPEAKER },
                        15.0,
                        c(theme::T2),
                    ))
                    .child(theme::switch(
                        format!("mute-{id}"),
                        format!("{} audio", track.name),
                        !track.muted,
                        move |s, _| s.editor.execute(Edit::MuteTrack { id }),
                    ))
                    .child(theme::small(track.name.clone())),
            );
        }
        main = main.child(actions);
    }
    if !e.message.is_empty() || !saved {
        let mut line = row()
            .w_full()
            .gap(10.0)
            .center_y()
            .child(theme::small(e.message.clone()).grow(1.0).min_w(0.0));
        if !saved {
            line = line.child(
                Btn::new("retry-save", "Retry save")
                    .on(|s, _| s.editor.changed())
                    .build(),
            );
        }
        main = main.child(line);
    }
    col().grow(1.0).min_h(0.0).child(bar).child(main)
}

#[cfg(test)]
mod rune_tests {
    use super::*;
    fn editor() -> State {
        let mut model = Editor::default();
        let asset = core::MediaAsset {
            id: 0,
            path: "fixture.mkv".into(),
            source_stamp: None,
            duration_ns: 10_000_000_000,
            width: 1920,
            height: 1080,
            streams: vec![core::MediaStream {
                index: 0,
                kind: core::TrackKind::Video,
                name: "Video".into(),
            }],
        };
        model
            .execute(Edit::Sequence(core::QuickEdit::Add(asset)))
            .unwrap();
        let mut state = State::default();
        state.editor = Some(model);
        state
    }
    #[test]
    fn rune_shortcuts_edit_the_real_timeline_and_undo_restores_it() {
        let mut s = editor();
        let duration = s.editor.as_ref().unwrap().project.duration();
        s.position = duration / 2;
        assert!(s.key(&KeyEvent {
            key: Key::Text("s".into()),
            mods: Default::default()
        }));
        assert_eq!(core::pieces(&s.editor.as_ref().unwrap().project).len(), 2);
        assert!(s.key(&KeyEvent {
            key: Key::Command('z'),
            mods: rune::Mods {
                cmd: true,
                ctrl: true,
                ..Default::default()
            }
        }));
        assert_eq!(core::pieces(&s.editor.as_ref().unwrap().project).len(), 1);
        assert_eq!(s.editor.as_ref().unwrap().project.duration(), duration);
    }
    #[test]
    fn modifier_aware_redo_restores_the_split_and_unsaved_video_blocks_leaving() {
        let mut s = editor();
        s.position = s.editor.as_ref().unwrap().project.duration() / 2;
        s.split();
        s.undo(false);
        assert!(s.key(&KeyEvent {
            key: Key::Command('z'),
            mods: rune::Mods {
                cmd: true,
                shift: true,
                ..Default::default()
            }
        }));
        assert_eq!(core::pieces(&s.editor.as_ref().unwrap().project).len(), 2);
        assert!(!s.prepare_to_leave());
        assert!(!s.close());
        assert_eq!(core::pieces(&s.editor.as_ref().unwrap().project).len(), 2);
        s.saved_revision = s.save_revision;
        assert!(s.prepare_to_leave());
    }

    #[test]
    fn feature_changes_only_wait_for_the_editor_that_owns_the_work() {
        use crate::features::Feature;
        let mut app = crate::rune_ui::State::with_headless(true);
        let (_sender, receiver) = mpsc::sync_channel(1);
        app.editor.loading = Some(receiver);
        app.editor.opening_image = Some(false);
        app.page = crate::rune_ui::Page::Editor;
        app.set_feature(Feature::StorageAnalysis, false);
        app.set_feature(Feature::ImageEditing, false);
        assert!(!app.allows(Feature::StorageAnalysis));
        assert!(!app.allows(Feature::ImageEditing));
        assert!(app.page == crate::rune_ui::Page::Editor);
        app.set_feature(Feature::VideoEditing, false);
        assert!(app.allows(Feature::VideoEditing));
        app.editor.loading = None;
        app.editor.opening_image = None;
        app.editor.path = Some(PathBuf::from("shadowplay.mp4"));
        app.editor.save_revision = 1;
        app.set_feature(Feature::VideoEditing, false);
        assert!(app.allows(Feature::VideoEditing));
        app.editor.saved_revision = 1;
        app.set_feature(Feature::VideoEditing, false);
        assert!(!app.allows(Feature::VideoEditing));
        assert!(app.editor.path.is_none());
    }

    #[test]
    fn feature_disable_waits_for_loading_and_unsaved_work() {
        let mut s = State::default();
        let (_sender, receiver) = mpsc::sync_channel(1);
        s.loading = Some(receiver);
        assert!(s.feature_change_blocked());
        s.loading = None;
        s.save_revision = 1;
        assert!(s.feature_change_blocked());
        s.saved_revision = 1;
        assert!(!s.feature_change_blocked());
    }

    fn spans(s: &State) -> Vec<(u64, u64)> {
        core::pieces(&s.editor.as_ref().unwrap().project)
            .iter()
            .map(|c| (c.start, c.end()))
            .collect()
    }
    #[test]
    fn dragging_trim_grips_ripples_the_timeline_and_restores_within_the_source() {
        let mut s = editor();
        let total = s.editor.as_ref().unwrap().project.duration();
        s.position = total / 2;
        s.split();
        let first = core::pieces(&s.editor.as_ref().unwrap().project)[0].id;
        let half = total / 2;
        // Cut 10 frames off the first piece's start: everything stays packed from zero.
        s.trim(first, true, 10);
        assert_eq!(spans(&s), vec![(0, half - 10), (half - 10, total - 10)]);
        // Cut 5 off its end: the second piece follows.
        s.trim(first, false, (half - 15) as i64);
        assert_eq!(spans(&s), vec![(0, half - 15), (half - 15, total - 15)]);
        // Restore the start beyond frame zero; the source still has those frames.
        s.trim(first, true, -10);
        assert_eq!(spans(&s), vec![(0, half - 5), (half - 5, total - 5)]);
        // The second piece already ends at its source's last frame: extending clamps to it.
        let second = core::pieces(&s.editor.as_ref().unwrap().project)[1].id;
        s.trim(second, false, total as i64 + 1_000);
        assert_eq!(spans(&s), vec![(0, half - 5), (half - 5, total - 5)]);
        // Each drag is one undo step.
        s.undo(false);
        assert_eq!(spans(&s), vec![(0, half - 15), (half - 15, total - 15)]);
    }
    #[test]
    fn save_worker_drains_final_pending_edits_after_ui_owner_is_dropped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("edits.json");
        let s = editor();
        let worker = SaveWorker::new(path.clone()).unwrap();
        worker.submit(7, s.editor.as_ref().unwrap().project.clone());
        drop(worker);
        assert_eq!(core::pieces(&Project::load(&path).unwrap()).len(), 1);
    }
}
