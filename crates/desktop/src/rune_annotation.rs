//! Existing annotation actor, with Rune pointer/key input and a bounded raster preview.
use crate::{
    rune_theme::{self as theme, Btn, c, icons},
    rune_ui::{JobResult, State as AppState, color},
};
use rune::{El, Key, KeyEvent, NativeSurface, TextEdit, col, el, row};
use std::{path::PathBuf, sync::mpsc};
use toolkit::{
    annotation as a,
    image_editor::{Actor, Command, Event, Source},
    types::Frame,
};
pub struct Annotation {
    actor: Actor,
    pub surface: NativeSurface,
    raster: Vec<u8>,
    size: (u32, u32),
    viewport: (u32, u32),
    active: a::Tool,
    save_path: Option<PathBuf>,
    pub message: String,
    pub text: TextEdit,
    serial: u64,
    accepted: Option<Frame>,
    /// The last preview's mapping from raster pixels to image pixels.
    geometry: Geometry,
    pub text_editing: bool,
    pub can_undo: bool,
    revision: u64,
    saved_revision: u64,
    recovered_revision: u64,
    checkpoint_pending: bool,
    recovery_path: PathBuf,
    cursor: a::PointF,
    anchor: bool,
    image_size: (u32, u32),
    checkpoints: Vec<(u64, PathBuf, bool, mpsc::Receiver<Result<(), String>>)>,
}
#[derive(Clone, Copy, Default)]
struct Geometry {
    raster: (f32, f32),
    offset: (f32, f32),
    padding: (f32, f32),
    scale: f32,
}
impl Annotation {
    pub fn new(source: Source, wake: rune::Waker) -> anyhow::Result<Self> {
        static NEXT_RECOVERY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NEXT_RECOVERY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Reserve a session name without writing: opening/viewing still works when the
        // capture folder is unwritable, so Save editable elsewhere remains available.
        let recovery_path = crate::recordings::root()
            .join("image-recovery")
            .join(format!(
                "image-{time}-{}-{sequence}.ssxe",
                std::process::id()
            ));
        Self::with_recovery(source, wake, recovery_path)
    }
    fn with_recovery(
        source: Source,
        wake: rune::Waker,
        recovery_path: PathBuf,
    ) -> anyhow::Result<Self> {
        let saved_revision = if matches!(&source, Source::Project(_)) {
            1
        } else {
            0
        };
        let actor = Actor::new(source)?;
        actor.set_waker(move || wake());
        Ok(Self {
            actor,
            surface: NativeSurface::new(),
            raster: vec![],
            size: (0, 0),
            viewport: (0, 0),
            active: a::Tool::Select,
            save_path: None,
            message: String::new(),
            text: TextEdit::new(""),
            serial: 0,
            accepted: None,
            geometry: Geometry::default(),
            text_editing: false,
            can_undo: false,
            revision: 1,
            saved_revision,
            recovered_revision: 0,
            checkpoint_pending: false,
            recovery_path,
            cursor: a::PointF::new(0.0, 0.0),
            anchor: false,
            image_size: (1, 1),
            checkpoints: vec![],
        })
    }
    /// A point on an element showing the preview with `SurfaceFit::Fit`, in image pixels:
    /// undoes the letterbox and the fit's scale, then the actor's zoom and padding.
    pub fn image_point(&self, x: f32, y: f32, element: (f32, f32)) -> a::PointF {
        let g = self.geometry;
        let (w, h) = (g.raster.0.max(1.0), g.raster.1.max(1.0));
        let k = (element.0 / w).min(element.1 / h).max(1e-6);
        let view = (
            (x - (element.0 - w * k) / 2.0) / k,
            (y - (element.1 - h * k) / 2.0) / k,
        );
        let scale = if g.scale > 0.0 { g.scale } else { 1.0 };
        a::PointF::new(
            (view.0 + g.offset.0) / scale - g.padding.0,
            (view.1 + g.offset.1) / scale - g.padding.1,
        )
    }
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn take_accepted(&mut self) -> Option<Frame> {
        self.accepted.take()
    }
    pub fn set_recovery_path(&mut self, path: PathBuf) {
        self.recovery_path = path;
    }
    pub fn mark_recovered(&mut self) {
        self.saved_revision = 0;
    }
    pub fn dirty(&self) -> bool {
        self.revision != self.saved_revision
    }
    /// Returns true only when the current editable revision is on disk.
    /// Call again after poll has processed the checkpoint acknowledgement.
    pub fn prepare_to_leave(&mut self) -> bool {
        if !self.finish_placement() {
            return false;
        }
        if !self.dirty() || self.recovered_revision == self.revision {
            return true;
        }
        if !self.checkpoint_pending {
            self.checkpoint_pending = true;
            self.checkpoint(self.recovery_path.clone(), true);
            self.message = "Saving editable recovery before leaving. Retry close or quit when recovery is saved.".into();
        }
        false
    }
    fn save_editable(&mut self, path: PathBuf) {
        self.checkpoint(path, false);
    }
    fn checkpoint(&mut self, path: PathBuf, recovery: bool) {
        if !self.finish_placement() {
            return;
        }
        let (reply, result) = mpsc::channel();
        self.checkpoints
            .push((self.revision, path.clone(), recovery, result));
        self.send(Command::Checkpoint {
            path,
            recovery,
            reply,
        });
    }
    fn finish_placement(&mut self) -> bool {
        if self.anchor {
            if !self.queue(Command::Up(self.cursor, a::Modifiers::NONE)) {
                return false;
            }
            self.anchor = false;
        }
        true
    }
    pub(crate) fn send(&mut self, command: Command) {
        self.queue(command);
    }
    fn queue(&mut self, command: Command) -> bool {
        let reset_anchor = matches!(&command, Command::Tool(..) | Command::Undo | Command::Redo);
        // Conservatively retain every operation that can alter editable work, including
        // commands queued ahead of the next actor preview. Selection/view changes are clean.
        if !matches!(
            &command,
            Command::Viewport(..)
                | Command::Zoom(..)
                | Command::Pan(..)
                | Command::Tool(..)
                | Command::Label(..)
                | Command::Project(..)
                | Command::Checkpoint { .. }
                | Command::CycleSelection(..)
                | Command::Copy
                | Command::ClearSelection
        ) {
            self.revision = self.revision.saturating_add(1);
        }
        if let Err(e) = self.actor.send(command) {
            self.checkpoint_pending = false;
            self.message = format!("{e:#}");
            return false;
        }
        if reset_anchor {
            self.anchor = false;
        }
        true
    }
    pub fn key(&mut self, event: &KeyEvent) -> bool {
        let mods = a::Modifiers {
            shift: event.mods.shift,
            ctrl: event.mods.cmd,
            alt: event.mods.alt,
        };
        match &event.key {
            Key::Command('z') => self.send(if event.mods.shift {
                Command::Redo
            } else {
                Command::Undo
            }),
            Key::Command('y') => self.send(Command::Redo),
            Key::Left | Key::Right | Key::Up | Key::Down if !self.text_editing => {
                if self.active == a::Tool::Select {
                    let key = match event.key {
                        Key::Left => a::Key::Left,
                        Key::Right => a::Key::Right,
                        Key::Up => a::Key::Up,
                        _ => a::Key::Down,
                    };
                    self.send(Command::Key(key, mods));
                } else {
                    let step = if event.mods.shift { 10.0 } else { 1.0 };
                    match event.key {
                        Key::Left => self.cursor.x -= step,
                        Key::Right => self.cursor.x += step,
                        Key::Up => self.cursor.y -= step,
                        _ => self.cursor.y += step,
                    }
                    self.cursor.x = self
                        .cursor
                        .x
                        .clamp(0.0, self.image_size.0.saturating_sub(1) as f32);
                    self.cursor.y = self
                        .cursor
                        .y
                        .clamp(0.0, self.image_size.1.saturating_sub(1) as f32);
                    if self.anchor {
                        self.send(Command::Move(self.cursor, mods));
                    }
                    self.message = format!(
                        "Placement X {:.0}, Y {:.0}. Enter sets {}.",
                        self.cursor.x,
                        self.cursor.y,
                        if self.anchor { "end" } else { "start" }
                    );
                }
            }
            Key::Home if !self.text_editing => {
                self.cursor = a::PointF::new(0.0, 0.0);
            }
            Key::Enter if self.active == a::Tool::Select || self.text_editing => {
                self.send(Command::Key(a::Key::Enter, mods))
            }
            Key::Enter => {
                if self.queue(if self.anchor {
                    Command::Up(self.cursor, mods)
                } else {
                    Command::Down(self.cursor, mods)
                }) {
                    self.anchor = !self.anchor;
                }
            }
            Key::Escape => {
                self.anchor = false;
                self.send(Command::Key(a::Key::Escape, mods));
            }
            Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End
                if self.text_editing =>
            {
                let key = match event.key {
                    Key::Left => a::Key::Left,
                    Key::Right => a::Key::Right,
                    Key::Up => a::Key::Up,
                    Key::Down => a::Key::Down,
                    Key::Home => a::Key::Home,
                    _ => a::Key::End,
                };
                self.send(Command::Key(key, mods));
            }
            Key::Delete | Key::Backspace => self.send(Command::Key(
                if matches!(event.key, Key::Delete) {
                    a::Key::Delete
                } else {
                    a::Key::Backspace
                },
                mods,
            )),
            Key::Text(text) => self.send(Command::Text(text.clone())),
            _ => return false,
        }
        true
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(preview) = self.actor.take_preview() {
            changed = true;
            self.active = preview.active_tool;
            self.image_size = preview.canvas;
            self.text_editing = preview.text_editing;
            self.can_undo = preview.can_undo;
            self.geometry = Geometry {
                raster: (preview.width as f32, preview.height as f32),
                offset: preview.offset,
                padding: (preview.padding.0 as f32, preview.padding.1 as f32),
                scale: preview.scale,
            };
            let size = (preview.width, preview.height);
            if preview.reset || self.size != size {
                self.size = size;
                self.raster = vec![0; size.0 as usize * size.1 as usize * 4];
            }
            for tile in preview.tiles {
                let r = tile.rect;
                for y in 0..r.height as usize {
                    let source = y * r.width as usize * 4;
                    let destination = ((r.y as usize + y) * size.0 as usize + r.x as usize) * 4;
                    let len = r.width as usize * 4;
                    if source + len <= tile.bgra.len() && destination + len <= self.raster.len() {
                        self.raster[destination..destination + len]
                            .copy_from_slice(&tile.bgra[source..source + len]);
                    }
                }
            }
            self.serial += 1;
            if size.0 > 0 && size.1 > 0 {
                if let Err(e) = self.surface.push_bytes(
                    rune::native_surface::ByteFrame {
                        width: size.0,
                        height: size.1,
                        format: rune::native_surface::Format::Bgra,
                        color: Default::default(),
                        planes: [self.raster.clone(), vec![]],
                    },
                    self.serial,
                ) {
                    self.message = e;
                }
            }
        }
        let mut pending = Vec::new();
        for (revision, path, recovery, rx) in std::mem::take(&mut self.checkpoints) {
            let result = match rx.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => {
                    pending.push((revision, path, recovery, rx));
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("Image worker stopped before saving".into())
                }
            };
            changed = true;
            if recovery {
                self.checkpoint_pending = false;
            }
            match result {
                Ok(()) if recovery => {
                    self.recovered_revision = revision;
                    self.message = format!(
                        "Editable recovery saved at {}. Close or quit again to leave. Recovery retains original pixels.",
                        path.display()
                    );
                }
                Ok(()) => {
                    self.saved_revision = revision;
                    self.message = format!(
                        "Editable project saved at {}. Original pixels are retained.",
                        path.display()
                    );
                    if !self.dirty() {
                        let _ = std::fs::remove_file(&self.recovery_path);
                    }
                }
                Err(error) => {
                    self.message = format!(
                        "Editable save failed: {error}. Your work is still open. Retry Save editable or close to retry recovery."
                    )
                }
            }
        }
        self.checkpoints = pending;
        for event in self.actor.events.try_iter() {
            changed = true;
            match event {
                Event::Accepted(frame) | Event::Copied(frame) => self.accepted = Some(frame),
                Event::Saved(path) => self.message = format!("Saved {}", path.display()),
                Event::Error(e) => self.message = e,
                _ => {}
            }
        }
        changed
    }
    pub fn take_save(&mut self) -> Option<(PathBuf, Frame)> {
        let frame = self.accepted.take()?;
        Some((self.save_path.take()?, frame))
    }
}
pub(crate) fn tool_icon(tool: a::Tool) -> &'static rune::Icon {
    use a::Tool::*;
    match tool {
        Select | Cursor => &icons::SELECT,
        Rectangle => &icons::RECT,
        Ellipse | CropEllipse => &icons::ELLIPSE,
        Line => &icons::LINE,
        Arrow => &icons::ARROW,
        Freehand | FreehandArrow | CropFreeform => &icons::FREEHAND,
        Text | Balloon => &icons::TEXT,
        Step => &icons::STEP,
        Magnify => &icons::SEARCH,
        Spotlight => &icons::EFFECTS,
        Blur | Pixelate => &icons::BLUR,
        Highlight | HighlightPen => &icons::HIGHLIGHT,
        Image => &icons::IMAGE,
        Sticker => &icons::STAR,
        Grid => &icons::THUMBNAILS,
        Eraser => &icons::TRASH,
        Crop => &icons::CROP,
        CutOut => &icons::SPLIT,
    }
}
pub(crate) fn tool_name(tool: a::Tool) -> String {
    // `HighlightPen` → `Highlight pen`.
    let raw = format!("{tool:?}");
    let mut name = String::new();
    for (i, ch) in raw.chars().enumerate() {
        if i > 0 && ch.is_uppercase() {
            name.push(' ');
            name.extend(ch.to_lowercase());
        } else {
            name.push(ch);
        }
    }
    name
}
fn send(command: fn() -> Command) -> impl Fn(&mut AppState, &mut rune::Tx) + 'static {
    move |s, _| {
        if let Some(a) = &mut s.editor.annotation {
            a.send(command());
        }
    }
}
pub fn view(s: &AppState) -> El {
    let Some(a) = &s.editor.annotation else {
        return col();
    };
    let title = s
        .editor
        .path
        .as_deref()
        .map(|p| crate::rune_ui::clip_title(s, p))
        .unwrap_or_else(|| "Image".into());
    let bar = row()
        .w_full()
        .px(24.0)
        .py(12.0)
        .gap(8.0)
        .center_y()
        .child(
            Btn::new("annotation-back", "Library")
                .ghost()
                .icon(&icons::BACK)
                .on(|s, _| s.go(crate::rune_ui::Page::Library))
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
        )
        .child(theme::small(if a.dirty() {
            "Unsaved editable work"
        } else {
            "Editable saved"
        }))
        .child(theme::spacer())
        .child(
            Btn::icon_only("annotation-undo", "Undo", &icons::UNDO)
                .on(send(|| Command::Undo))
                .build(),
        )
        .child(
            Btn::icon_only("annotation-redo", "Redo", &icons::REDO)
                .on(send(|| Command::Redo))
                .build(),
        )
        .child(
            Btn::new("annotation-rotate", "Rotate")
                .ghost()
                .icon(&icons::ROTATE)
                .on(send(|| Command::Rotate))
                .build(),
        )
        .child(
            Btn::new("annotation-crop", "Apply crop")
                .ghost()
                .icon(&icons::CROP)
                .on(send(|| Command::ApplyCrop))
                .build(),
        )
        .child(
            Btn::new("annotation-project", "Save editable…")
                .on(|_, tx| {
                    tx.save_file(
                        rune::FileDialogOptions::default().filter("Annotation project", &["ssxe"]),
                        |s: &mut AppState, result, _| {
                            if let Ok(Some(path)) = result
                                && let Some(a) = &mut s.editor.annotation
                            {
                                a.save_editable(path);
                            }
                        },
                    )
                })
                .build(),
        )
        .child(
            Btn::new("annotation-save", "Export PNG…")
                .primary()
                .on(|_, tx| {
                    tx.save_file(
                        rune::FileDialogOptions::default().filter("PNG", &["png"]),
                        |s: &mut AppState, result, _| match result {
                            Ok(Some(path)) => {
                                if let Some(a) = &mut s.editor.annotation {
                                    a.save_path = Some(path);
                                    a.send(Command::Accept);
                                }
                            }
                            Err(e) => s.notice = e.to_string(),
                            _ => {}
                        },
                    )
                })
                .build(),
        );
    let mut tools = row()
        .key("annotation-tools")
        .role("toolbar")
        .label("Annotation tools")
        .gap(2.0)
        .padding(5.0)
        .center_y();
    for tool in a::Tool::ALL {
        let on = a.active == tool;
        let mut b = Btn::icon_only(
            format!("annotation-{tool:?}"),
            tool_name(tool),
            tool_icon(tool),
        )
        .checked(on)
        .on(move |s, _| {
            if let Some(a) = &mut s.editor.annotation {
                a.send(Command::Tool(tool));
            }
        });
        if on {
            b = b.variant(theme::Variant::Neutral);
        }
        tools = tools.child(b.build());
    }
    let label = theme::field_shell(
        Some(theme::glyph(&icons::TEXT, 14.0, c(theme::T3))),
        theme::input("annotation-label", "Label text", &a.text, |s| {
            &mut s.editor.annotation.as_mut().unwrap().text
        })
        .on_input_change(|s: &mut AppState, _| {
            if let Some(a) = &mut s.editor.annotation {
                a.send(Command::Label(a.text.text.clone()));
            }
        }),
    )
    .w(240.0);
    let dock = row()
        .w_full()
        .px(24.0)
        .gap(12.0)
        .center_y()
        .child(
            theme::floating(tools.radius(9.0))
                .scroll_x()
                .min_w(0.0)
                .grow(1.0),
        )
        .child(label);
    let canvas = el()
        .key("annotation-canvas")
        .role("group")
        .label("Annotation canvas. Arrow keys position the active tool; Enter starts or finishes placement.")
        .focusable(true)
        .on_key(|s: &mut AppState, event, _| s.editor.annotation.as_mut().is_some_and(|a| a.key(event)))
        .grow(1.0)
        .min_h(100.0)
        .w_full()
        .radius(8.0)
        .clip_rounded(8.0)
        .fill(color(0x121316))
        .native_surface(&a.surface, rune::SurfaceFit::Fit)
        .on_layout(|s: &mut AppState, r| {
            if let Some(a) = &mut s.editor.annotation {
                let size = (r[2].max(1.0) as u32, r[3].max(1.0) as u32);
                if a.viewport != size {
                    a.viewport = size;
                    a.send(Command::Viewport(size.0, size.1));
                }
            }
        })
        .on_drag(|s: &mut AppState, d, tx| {
            if let Some(a) = &mut s.editor.annotation {
                let p = a.image_point(d.x, d.y, (d.w, d.h));
                a.cursor = p;
                let mods = tx.mods();
                let m = a::Modifiers {
                    ctrl: mods.cmd,
                    shift: mods.shift,
                    alt: mods.alt,
                };
                let command = match d.phase {
                    rune::Phase::Down => Command::Down(p, m),
                    rune::Phase::Move => Command::Move(p, m),
                    rune::Phase::Up => Command::Up(p, m),
                };
                if a.queue(command) { a.anchor = !matches!(d.phase, rune::Phase::Up); }
            }
        });
    let mut page = col()
        .grow(1.0)
        .min_h(0.0)
        .child(bar)
        .child(dock)
        .child(theme::small("Editable projects and recovery retain original pixels, including cropped or covered areas. Export PNG for flattened sharing; export does not save editable work.").px(24.0).pt(8.0))
        .child(row().px(24.0).gap(8.0).center_y()
            .child(Btn::new("annotation-select-next", "Select next annotation")
                .on(send(|| Command::CycleSelection(false))).build())
            .child(Btn::new("annotation-select-previous", "Select previous annotation")
                .on(send(|| Command::CycleSelection(true))).build())
            .child(theme::small("Tab to canvas. Arrows move selection or placement; Shift moves 10 px. Enter sets start/end; Home resets placement.")))
        .child(col().grow(1.0).min_h(0.0).px(24.0).pt(14.0).pb(18.0).child(canvas));
    if !a.message.is_empty() {
        page = page.child(theme::small(a.message.clone()).px(24.0).pb(14.0));
    }
    page
}
pub fn flush_save(s: &mut AppState) {
    if let Some((path, frame)) = s.editor.annotation.as_mut().and_then(Annotation::take_save) {
        s.job(move || {
            toolkit::utilities::save_image(&path, &frame)
                .map(|_| JobResult::Notice(format!("Saved {}", path.display())))
                .map_err(|e| format!("{e:#}"))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settle_checkpoints(a: &mut Annotation) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !a.checkpoints.is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "Checkpoint worker did not reply"
            );
            a.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[test]
    fn leaving_requires_current_durable_editable_revision_and_exports_do_not_clean_it() {
        let dir = tempfile::tempdir().unwrap();
        let recovery = dir.path().join("recovery.ssxe");
        let frame = toolkit::effects::solid_frame(32, 24, [255; 4]);
        let mut a = Annotation::with_recovery(
            Source::Image(frame),
            std::sync::Arc::new(|| {}),
            recovery.clone(),
        )
        .unwrap();
        assert!(a.dirty());
        assert!(!a.prepare_to_leave());
        settle_checkpoints(&mut a);
        assert!(a.prepare_to_leave());
        assert!(toolkit::annotation::project::load(&recovery).is_ok());
        a.send(Command::Rotate);
        assert!(
            !a.prepare_to_leave(),
            "An older recovery cannot cover a later edit"
        );
        settle_checkpoints(&mut a);
        let project = dir.path().join("saved.ssxe");
        a.save_editable(project.clone());
        settle_checkpoints(&mut a);
        assert!(!a.dirty());
        a.send(Command::Accept);
        assert!(
            a.dirty(),
            "Flattened export must not mark editable work saved"
        );
        a.save_editable(project.clone());
        settle_checkpoints(&mut a);
        assert!(
            a.dirty(),
            "A failed no-clobber save must not clean editable work"
        );
        assert!(toolkit::annotation::project::load(&project).is_ok());
        // Failed recovery also leaves the dirty session open for retry or Save elsewhere.
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"existing").unwrap();
        a.set_recovery_path(blocked.join("recovery.ssxe"));
        assert!(!a.prepare_to_leave());
        settle_checkpoints(&mut a);
        assert!(!a.prepare_to_leave());
        assert!(a.dirty());
    }
}
