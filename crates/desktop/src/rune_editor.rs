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
        if self.editor.is_some() && self.save_revision != self.saved_revision {
            self.message="Wait for edits to finish saving before opening another clip. If saving failed, use Retry save.".into();
            return;
        }
        self.stop();
        let (tx, rx) = mpsc::sync_channel(1);
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
                        let source = if path.extension().is_some_and(|e| e == "ssxe") {
                            toolkit::image_editor::Source::Project(
                                toolkit::annotation::project::load(&path)?,
                            )
                        } else {
                            toolkit::image_editor::Source::Image(toolkit::types::Frame::from_image(
                                image::open(&path)?.into_rgba8(),
                            ))
                        };
                        Loaded::Image(crate::rune_annotation::Annotation::new(
                            source,
                            wake.clone(),
                        )?)
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
            self.message = e.to_string();
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
            Key::Command('z') => self.undo(false),
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
                    Ok(f) => {
                        if let Err(e) = self.pixels(f.width, f.height, f.bgra, true) {
                            self.message = e;
                        }
                    }
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
    /// Close the open clip. Refused while edits are unsaved or an export runs.
    pub fn close(&mut self) -> bool {
        if self.export.is_some() {
            self.message = "Cancel the export before closing this clip.".into();
            return false;
        }
        if self.editor.is_some() && self.save_revision != self.saved_revision {
            self.message = "Wait for edits to finish saving before closing. If saving failed, use Retry save.".into();
            return false;
        }
        self.stop();
        self.loading = None;
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

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0);
    let whole = s as u64;
    format!("{}:{:04.1}", whole / 60, s - (whole / 60 * 60) as f64)
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
        bar = bar
            .child(theme::small("Exporting…"))
            .child(
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
    let mut main = col()
        .grow(1.0)
        .min_h(0.0)
        .px(24.0)
        .pb(18.0)
        .gap(14.0);
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
        // The strip: one segment per piece, sized by its length; drag anywhere to seek.
        let selected = e.selected();
        let mut strip = row()
            .key("seek")
            .role("slider")
            .label("Seek")
            .range_value(position as f64, 0.0, duration as f64)
            .w_full()
            .h(68.0)
            .gap(2.0)
            .radius(6.0)
            .clip_rounded(6.0)
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
            let mut segment = col()
                .key(format!("piece-{id}"))
                .role("button")
                .label(format!(
                    "Piece {}, {:.1} s",
                    i + 1,
                    project.seconds(piece.duration)
                ))
                .grow(piece.duration.max(1) as f32)
                .min_w(4.0)
                .h_full()
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
            if on {
                segment = segment.border(2.0, c(theme::T1));
            }
            strip = strip.child(segment);
        }
        let x = (position as f32 / duration as f32) * e.strip_width;
        strip = strip.child(
            el().key("playhead")
                .w(2.0)
                .h(68.0)
                .fill(c(theme::ACCENT))
                .absolute((x - 1.0).max(0.0), 0.0),
        );
        main = main.child(strip);
        let mut ticks = row().w_full().justify_between();
        for step in 0..=4 {
            ticks = ticks.child(
                rune::text(ui::clock(project.seconds(duration) * step as f64 / 4.0))
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
                            s.editor.execute(Edit::TrimLeft {
                                id,
                                to: s.editor.position,
                            });
                        }
                    })
                    .build(),
            )
            .child(
                Btn::new("trim-end", "End here")
                    .disabled(selected.is_none())
                    .on(|s, _| {
                        if let Some(id) = s.editor.selected() {
                            s.editor.execute(Edit::TrimRight {
                                id,
                                to: s.editor.position,
                            });
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
