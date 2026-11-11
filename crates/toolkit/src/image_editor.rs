//! The SSX annotation session and renderer live on a worker, never the GPUI event thread.
use crate::{annotation as a, effects, types::Frame};
use anyhow::{Context, Result, bail};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

pub enum Source {
    Image(Frame),
    Project(a::Document),
}
pub enum Command {
    Tool(a::Tool),
    Down(a::PointF, a::Modifiers),
    Move(a::PointF, a::Modifiers),
    Up(a::PointF, a::Modifiers),
    Key(a::Key, a::Modifiers),
    /// Keyboard interpretation belongs to the Actor, never to a stale preview.
    CanvasInput { request: u64, key: a::Key, text: Option<String>, modifiers: a::Modifiers, repeat: bool },
    Text(String),
    Label(String),
    ReviewLabel { target: Option<a::ObjectId>, tool: a::Tool },
    ApplyLabel { request: u64, target: Option<a::ObjectId>, tool: a::Tool, expected: String, text: String },
    Undo,
    Redo,
    Delete,
    ClearSelection,
    Duplicate,
    Front,
    Back,
    Stroke(a::Color),
    Width(f32),
    FontSize(f32),
    Fill(bool),
    EffectAmount(f32),
    Opacity(f32),
    MagnifyZoom(f32),
    Viewport(u32, u32),
    Zoom(Option<f32>),
    Pan(f32, f32),
    ApplyCrop,
    Rotate,
    Flip,
    Effect(effects::Effect),
    InsertImage(PathBuf),
    Project(PathBuf),
    /// Ordered durable editable snapshot, acknowledged even when writing fails.
    Checkpoint { path: PathBuf, recovery: bool, reply: mpsc::Sender<std::result::Result<(), String>> },
    CycleSelection(bool),
    Copy,
    Accept,
    AcceptRegion(crate::types::Rect),
}
pub const TILE_SIZE: u32 = 512;
pub struct RasterTile {
    pub rect: crate::types::Rect,
    pub bgra: Vec<u8>,
}
pub struct Preview {
    /// Actor command ordering, independent of raster cache identity.
    pub command_revision: u64,
    pub active_tool: a::Tool,
    pub text_editing: bool,
    pub properties: Option<Properties>,
    pub render_us: u64,
    pub width: u32,
    pub height: u32,
    pub tiles: Vec<RasterTile>,
    pub reset: bool,
    pub canvas: (u32, u32),
    pub scale: f32,
    /// Raster density is independent of logical zoom and tool coordinates.
    pub raster_density: f32,
    /// Raster cover origin in logical preview pixels (may be slightly negative).
    pub raster_offset: (f32, f32),
    pub detail_limited: bool,
    pub offset: (f32, f32),
    pub padding: (u32, u32),
    pub overlay: a::input::Overlay,
    pub cursor: a::input::CursorHint,
    pub objects: usize,
    pub can_undo: bool,
    pub can_redo: bool,
    pub pending_crop: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Properties {
    pub selected: usize,
    pub selected_id: Option<a::ObjectId>,
    pub text: Option<String>,
    pub tool: a::Tool,
    pub color: a::Color,
    pub width: f32,
    pub font: Option<f32>,
    pub fill: bool,
    pub opacity: f32,
    pub amount: Option<f32>,
    pub zoom: Option<f32>,
}
fn properties(session: &a::EditorSession) -> Option<Properties> {
    let preset = session.styles().get(session.tool());
    let selected = session.selection();
    let (style, kind) = if let Some(object) = selected
        .first()
        .and_then(|id| session.document().object(*id))
    {
        (&object.style, &object.kind)
    } else {
        let preset = preset.as_ref()?;
        (&preset.style, &preset.kind)
    };
    let tool = a::Tool::for_kind(kind)?;
    let color = match kind {
        a::ObjectKind::Text(t) => t.content.color,
        a::ObjectKind::Step(_) | a::ObjectKind::Highlight(_) => {
            style.solid_fill().unwrap_or(style.stroke)
        }
        _ => style.stroke,
    };
    Some(Properties {
        selected: selected.len(),
        selected_id: (selected.len() == 1).then(|| selected[0]),
        text: (selected.len() <= 1).then(|| kind.text_content().map(|content| content.text.clone())).flatten(),
        tool,
        color,
        width: style.stroke_width,
        font: match kind {
            a::ObjectKind::Text(t) => Some(t.content.font.size),
            a::ObjectKind::Balloon(t) => Some(t.content.font.size),
            _ => None,
        },
        fill: !style.fill.is_none(),
        opacity: style.opacity,
        amount: match kind {
            a::ObjectKind::Blur(s) | a::ObjectKind::Pixelate(s) => Some(s.amount),
            _ => None,
        },
        zoom: match kind {
            a::ObjectKind::Magnify(m) => Some(m.zoom),
            _ => None,
        },
    })
}
impl Preview {
    pub fn image_point(&self, x: f32, y: f32) -> a::PointF {
        a::PointF::new(
            (x + self.offset.0) / self.scale - self.padding.0 as f32,
            (y + self.offset.1) / self.scale - self.padding.1 as f32,
        )
    }
    pub fn output_point(&self, p: a::PointF) -> a::PointF {
        a::PointF::new(
            (p.x + self.padding.0 as f32) * self.scale - self.offset.0,
            (p.y + self.padding.1 as f32) * self.scale - self.offset.1,
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasAction { CopyImage, Continue, Cancel }
pub const CANVAS_CLIPBOARD_LIMIT: usize = 1024 * 1024;
#[derive(Debug)]
pub enum CanvasClipboardOperation { Read, Write(String) }
type ClipboardResponse = std::result::Result<Option<String>, String>;
pub struct CanvasClipboardReply {
    active: Arc<AtomicBool>,
    reply: mpsc::SyncSender<ClipboardResponse>,
}
impl CanvasClipboardReply {
    /// Claim before touching the OS: expired/closed requests have no side effects.
    pub fn respond(self, operation: impl FnOnce() -> ClipboardResponse) {
        if self.active.swap(false, Ordering::AcqRel) { let _ = self.reply.try_send(operation()); }
    }
}
struct ClipboardWait<'a> { queue: &'a Queue, active: Arc<AtomicBool> }
impl Drop for ClipboardWait<'_> {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.queue.clipboard_cancel.lock().unwrap_or_else(|error| error.into_inner()).take();
    }
}
pub enum Event {
    /// One ordered canvas operation; its reply never enters the normal command queue.
    CanvasClipboard { request: u64, operation: CanvasClipboardOperation,
        reply: CanvasClipboardReply },
    CanvasClipboardError { request: u64, detail: String },
    Accepted(Frame),
    Copied(Frame),
    Saved(PathBuf),
    Error(String),
    CanvasResult { request: u64, command_revision: u64, action: Option<CanvasAction> },
    LabelResult { request: u64, target: Option<a::ObjectId>, command_revision: u64, result: std::result::Result<String, String> },
}
type UiWaker = Arc<dyn Fn() + Send + Sync>;
struct Queue {
    items: Mutex<VecDeque<Command>>,
    wake: Condvar,
    closed: AtomicBool,
    preserve_moves: AtomicBool,
    ui_waker: Mutex<Option<UiWaker>>,
    clipboard_cancel: Mutex<Option<mpsc::SyncSender<ClipboardResponse>>>,
}
impl Queue {
    fn notify_ui(&self) {
        let wake = self.ui_waker.lock().unwrap_or_else(|error| error.into_inner()).clone();
        if let Some(wake) = wake { wake(); }
    }
}
pub struct Actor {
    queue: Arc<Queue>,
    pub preview: Arc<Mutex<Option<Preview>>>,
    pub events: mpsc::Receiver<Event>,
}
impl Actor {
    pub fn new(source: Source) -> Result<Self> {
        Self::with_view(source, View::default())
    }
    /// Preserve native desktop pixels for common multi-monitor layouts, with a 64 MiB raster budget.
    pub fn desktop(frame: Frame) -> Result<Self> {
        let mut width = frame.width().clamp(1, 16384);
        let mut height = frame.height().clamp(1, 16384);
        let pixels = width as u64 * height as u64;
        if pixels > 16_777_216 {
            let scale = (16_777_216.0 / pixels as f64).sqrt();
            width = (width as f64 * scale).floor().max(1.) as u32;
            height = (height as f64 * scale).floor().max(1.) as u32;
        }
        Self::with_view(
            Source::Image(frame),
            View {
                width,
                height,
                ..View::default()
            },
        )
    }
    fn with_view(source: Source, mut view: View) -> Result<Self> {
        // Bound image effects so annotation work cannot occupy every recorder CPU core.
        let render_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .thread_name(|i| format!("rcr-image-render-{i}"))
            .build()?;
        let queue = Arc::new(Queue {
            items: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            closed: AtomicBool::new(false),
            preserve_moves: AtomicBool::new(false),
            ui_waker: Mutex::new(None),
            clipboard_cancel: Mutex::new(None),
        });
        let preview = Arc::new(Mutex::new(None));
        let (tx, events) = mpsc::channel();
        let q = queue.clone();
        let latest = preview.clone();
        thread::Builder::new()
            .name("image-editor".into())
            .spawn(move || {
                render_pool.install(|| {
                    let result = (|| -> Result<()> {
                        let mut session = match source {
                            Source::Image(frame) => a::EditorSession::from_frame(frame)?,
                            Source::Project(doc) => a::EditorSession::new(doc),
                        };
                        session.set_history_limits(a::HistoryLimits {
                            max_entries: 128,
                            max_bytes: 128 * 1024 * 1024,
                        });
                        let mut raster = RasterCache::default();
                        publish(&mut session, &view, &latest, &mut raster);
                        q.notify_ui();
                        let cadence = std::time::Duration::from_millis(8);
                        let mut next_frame = std::time::Instant::now();
                        loop {
                            let mut items = q.items.lock().unwrap_or_else(|e| e.into_inner());
                            while items.is_empty() && !q.closed.load(Ordering::Acquire) {
                                items = q.wake.wait(items).unwrap_or_else(|e| e.into_inner());
                            }
                            if q.closed.load(Ordering::Acquire) {
                                break;
                            }
                            while let Some(delay) =
                                next_frame.checked_duration_since(std::time::Instant::now())
                            {
                                let (guard, _) = q
                                    .wake
                                    .wait_timeout(items, delay)
                                    .unwrap_or_else(|e| e.into_inner());
                                items = guard;
                                if q.closed.load(Ordering::Acquire) {
                                    return Ok(());
                                }
                            }
                            next_frame = std::time::Instant::now() + cadence;
                            let commands = items.drain(..).collect::<Vec<_>>();
                            drop(items);
                            let (batch_tx, batch_events) = mpsc::channel();
                            for command in commands {
                                if q.closed.load(Ordering::Acquire) { return Ok(()); }
                                let keyboard = matches!(&command, Command::CanvasInput { .. });
                                let emits_event = matches!(&command, Command::Project(..) | Command::Checkpoint { .. } | Command::Copy | Command::Accept | Command::AcceptRegion(..) | Command::ApplyLabel { .. });
                                let sink = if keyboard { &batch_tx } else { &tx };
                                if let Err(error) = apply_with_clipboard(&mut session, &mut view, command, sink, Some((&q, &tx))) {
                                    let _ = tx.send(Event::Error(format!("{error:#}")));
                                    q.notify_ui();
                                } else if emits_event { q.notify_ui(); }
                            }
                            // Publish state before keyboard acknowledgements. A finishing
                            // shortcut cannot use an older preview, or close work
                            // introduced by a later command in this same batch.
                            publish(&mut session, &view, &latest, &mut raster);
                            for mut event in batch_events.try_iter() {
                                if let Event::CanvasResult { command_revision, action, .. } = &mut event
                                    && *command_revision < view.last_interaction_revision
                                { *action = None; }
                                let _ = tx.send(event);
                            }
                            q.notify_ui();
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        let _ = tx.send(Event::Error(format!("{error:#}")));
                        q.notify_ui();
                    }
                })
            })?;
        Ok(Self {
            queue,
            preview,
            events,
        })
    }
    pub fn send(&self, command: Command) -> Result<()> {
        if self.queue.closed.load(Ordering::Acquire) {
            bail!("Image editor has closed");
        }
        let mut queue = self.queue.items.lock().unwrap_or_else(|e| e.into_inner());
        // Only consecutive moves/resize observations coalesce; button/key order stays exact.
        if matches!(
            (&command, queue.back()),
            (Command::Viewport(..), Some(Command::Viewport(..)))
        ) || (!self.queue.preserve_moves.load(Ordering::Relaxed)
            && matches!(
                (&command, queue.back()),
                (Command::Move(..), Some(Command::Move(..)))
            ))
        {
            queue.pop_back();
        }
        if queue.len() >= 256 {
            bail!("Image editor is busy; try again after rendering finishes");
        }
        // A keyboard F may be queued immediately before its first stroke,
        // before the Actor can publish its tool. Preserve those samples
        // conservatively until the next explicit toolbar Tool command.
        if matches!(&command, Command::CanvasInput { key: a::Key::Char('f' | 'F'), modifiers, .. } if !modifiers.ctrl && !modifiers.alt) {
            self.queue.preserve_moves.store(true, Ordering::Relaxed);
        }
        if let Command::Tool(tool) = &command {
            self.queue.preserve_moves.store(
                matches!(
                    tool,
                    a::Tool::Freehand
                        | a::Tool::FreehandArrow
                        | a::Tool::HighlightPen
                        | a::Tool::CropFreeform
                        | a::Tool::Eraser
                ),
                Ordering::Relaxed,
            );
        }
        queue.push_back(command);
        drop(queue);
        self.queue.wake.notify_one();
        Ok(())
    }
    /// Wake a UI consumer after preview/event publication. Registration also
    /// flushes work completed before the callback was attached. Callbacks must
    /// enqueue a nonblocking signal; they run outside actor/preview mutexes.
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        let wake: UiWaker = Arc::new(wake);
        *self.queue.ui_waker.lock().unwrap_or_else(|error| error.into_inner()) = Some(wake.clone());
        wake();
    }
    pub fn take_preview(&self) -> Option<Preview> {
        self.preview
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        self.queue.closed.store(true, Ordering::Release);
        if let Some(reply) = self.queue.clipboard_cancel.lock().unwrap_or_else(|error| error.into_inner()).take() {
            let _ = reply.try_send(Err("The editor closed".into()));
        }
        self.queue.ui_waker.lock().unwrap_or_else(|error| error.into_inner()).take();
        self.queue.wake.notify_one();
    }
}
struct View {
    command_revision: u64,
    last_interaction_revision: u64,
    width: u32,
    height: u32,
    zoom: Option<f32>,
    pan: (f32, f32),
}
impl Default for View {
    fn default() -> Self {
        Self {
            command_revision: 0,
            last_interaction_revision: 0,
            width: 1280,
            height: 800,
            zoom: None,
            pan: (0., 0.),
        }
    }
}
#[derive(Default)]
struct RasterCache {
    last: Option<RasterView>,
    global_effect: bool,
}
#[derive(PartialEq)]
struct RasterView {
    canvas: (u32, u32),
    padding: (u32, u32),
    scale: u32,
    density: u32,
    viewport: crate::types::Rect,
    base_revision: u64,
}
// Bound the extra returned RGBA batch, in addition to the existing dirty-tile
// payload. Renderer-managed effect halos retain their existing work-rect rules.
const RASTER_BATCH_BYTES: usize = 16 * 1024 * 1024;
// Every effect workspace/layer is at most32MiB. Source caches and blur scratch
// are additional; exports do not use this interactive-only density safeguard.
const PREVIEW_WORK_PIXELS: u64 = 8 * 1024 * 1024;

fn raster_batches(dirty: crate::types::Rect, output: (u32, u32), budget: usize) -> Vec<crate::types::Rect> {
    let first_x = dirty.x as u32 / TILE_SIZE * TILE_SIZE;
    let first_y = dirty.y as u32 / TILE_SIZE * TILE_SIZE;
    let right = (dirty.x as u32 + dirty.width).div_ceil(TILE_SIZE).saturating_mul(TILE_SIZE).min(output.0);
    let bottom = (dirty.y as u32 + dirty.height).div_ceil(TILE_SIZE).saturating_mul(TILE_SIZE).min(output.1);
    let pixels = (budget / 4).max(TILE_SIZE as usize * TILE_SIZE as usize);
    let columns = (pixels / (TILE_SIZE as usize * TILE_SIZE as usize)).max(1) as u32;
    let batch_width = (right - first_x).min(columns.saturating_mul(TILE_SIZE));
    if batch_width == 0 || bottom <= first_y { return vec![]; }
    let rows = (pixels / (batch_width as usize * TILE_SIZE as usize)).max(1) as u32;
    let batch_height = rows.saturating_mul(TILE_SIZE);
    let mut batches = Vec::new();
    for y in (first_y..bottom).step_by(batch_height as usize) {
        for x in (first_x..right).step_by(batch_width as usize) {
            batches.push(crate::types::Rect::new(x as i32, y as i32,
                batch_width.min(right - x), batch_height.min(bottom - y)));
        }
    }
    batches
}

fn raster_tiles(
    session: &mut a::EditorSession, scale: f32, viewport: crate::types::Rect,
    dirty: crate::types::Rect, budget: usize,
) -> Vec<RasterTile> {
    let mut tiles = Vec::new();
    for batch in raster_batches(dirty, (viewport.width, viewport.height), budget) {
        // Let the renderer expand this viewport for effect source pixels and halos.
        // Adjacent dirty tiles now share that work instead of redoing it per tile.
        let mut bgra = session.render(&a::RenderOptions {
            scale,
            viewport: Some(batch.translate(viewport.x, viewport.y)),
            ..Default::default()
        }).into_data();
        for pixel in bgra.as_chunks_mut::<4>().0 { pixel.swap(0, 2); }
        if batch.width <= TILE_SIZE && batch.height <= TILE_SIZE {
            tiles.push(RasterTile { rect: batch, bgra });
            continue;
        }
        for y in (0..batch.height).step_by(TILE_SIZE as usize) {
            for x in (0..batch.width).step_by(TILE_SIZE as usize) {
                let rect = crate::types::Rect::new(batch.x + x as i32, batch.y + y as i32,
                    TILE_SIZE.min(batch.width - x), TILE_SIZE.min(batch.height - y));
                let mut pixels = Vec::with_capacity(rect.width as usize * rect.height as usize * 4);
                for row in y..y + rect.height {
                    let start = (row as usize * batch.width as usize + x as usize) * 4;
                    pixels.extend_from_slice(&bgra[start..start + rect.width as usize * 4]);
                }
                tiles.push(RasterTile { rect, bgra: pixels });
            }
        }
    }
    tiles
}
fn publish(
    session: &mut a::EditorSession,
    view: &View,
    slot: &Mutex<Option<Preview>>,
    cache: &mut RasterCache,
) {
    publish_with_work_budget(session, view, slot, cache, PREVIEW_WORK_PIXELS);
}
fn publish_with_work_budget(
    session: &mut a::EditorSession,
    view: &View,
    slot: &Mutex<Option<Preview>>,
    cache: &mut RasterCache,
    work_budget: u64,
) {
    let start = std::time::Instant::now();
    let (cw, ch) = session.document().canvas_size();
    let fit = (view.width as f32 / cw.max(1) as f32)
        .min(view.height as f32 / ch.max(1) as f32)
        .min(1.);
    let scale = view.zoom.unwrap_or(fit).clamp(0.02, 8.);
    let width = ((cw as f32 * scale).ceil() as u32).min(view.width).max(1);
    let height = ((ch as f32 * scale).ceil() as u32).min(view.height).max(1);
    let ox = ((cw as f32 * scale - width as f32) / 2. + view.pan.0)
        .clamp(0., (cw as f32 * scale - width as f32).max(0.));
    let oy = ((ch as f32 * scale - height as f32) / 2. + view.pan.1)
        .clamp(0., (ch as f32 * scale - height as f32).max(0.));
    session.set_view_scale(scale);
    let padding = session.document().canvas_offset();
    let viewport = crate::types::Rect::new(ox.floor() as i32, oy.floor() as i32, width, height);
    let raster = a::Renderer::plan_preview_raster(session.document(), scale, viewport, work_budget);
    let density = raster.density;
    let layout = RasterView {
        canvas: (cw, ch),
        padding,
        scale: scale.to_bits(),
        density: density.to_bits(),
        viewport,
        base_revision: session.document().base_revision(),
    };
    let reset = cache.last.as_ref() != Some(&layout);
    // Consume session notifications every frame; retaining them grows memory during long drags.
    let dirty = session.take_dirty();
    session.take_events();
    let global_effect = session
        .document()
        .objects()
        .iter()
        .any(|o| o.visible && matches!(o.kind, a::ObjectKind::Spotlight(_)));
    let output = crate::types::Rect::new(0, 0, width, height);
    let dirty = if reset || (dirty.is_some() && (global_effect || cache.global_effect)) {
        Some(output)
    } else {
        dirty.and_then(|r| {
            let left = ((r.x as f32 + padding.0 as f32) * scale - ox.floor()).floor() as i32 - 2;
            let top = ((r.y as f32 + padding.1 as f32) * scale - oy.floor()).floor() as i32 - 2;
            let right = (((r.x as f32 + r.width as f32) + padding.0 as f32) * scale - ox.floor())
                .ceil() as i32
                + 2;
            let bottom = (((r.y as f32 + r.height as f32) + padding.1 as f32) * scale - oy.floor())
                .ceil() as i32
                + 2;
            crate::types::Rect::new(
                left,
                top,
                (right - left).max(0) as u32,
                (bottom - top).max(0) as u32,
            )
            .intersect(output)
        })
    };
    let tiles = dirty.map_or_else(Vec::new, |dirty| {
        // Convert dirty edges on the same global grid as the preview cover.
        let d = f64::from(density);
        let left = ((f64::from(viewport.x) + f64::from(dirty.x)) * d).floor() as i32 - raster.viewport.x;
        let top = ((f64::from(viewport.y) + f64::from(dirty.y)) * d).floor() as i32 - raster.viewport.y;
        let right = ((f64::from(viewport.x) + f64::from(dirty.x) + f64::from(dirty.width)) * d).ceil() as i32 - raster.viewport.x;
        let bottom = ((f64::from(viewport.y) + f64::from(dirty.y) + f64::from(dirty.height)) * d).ceil() as i32 - raster.viewport.y;
        let dirty = crate::types::Rect::new(left, top, (right - left).max(0) as u32, (bottom - top).max(0) as u32);
        raster_tiles(session, scale * density, raster.viewport, dirty, RASTER_BATCH_BYTES)
    });
    cache.last = Some(layout);
    cache.global_effect = global_effect;
    let preview = Preview {
        command_revision: view.command_revision,
        active_tool: session.tool(),
        text_editing: session.is_editing_text(),
        properties: properties(session),
        render_us: start.elapsed().as_micros() as u64,
        width,
        height,
        tiles,
        reset,
        canvas: (cw, ch),
        scale,
        raster_density: density,
        raster_offset: ((f64::from(raster.viewport.x) / f64::from(density) - f64::from(viewport.x)) as f32,
            (f64::from(raster.viewport.y) / f64::from(density) - f64::from(viewport.y)) as f32),
        detail_limited: density < 1.,
        offset: (ox.floor(), oy.floor()),
        padding: session.document().canvas_offset(),
        overlay: session.overlay(),
        cursor: session.cursor_hint(),
        objects: session.document().objects().len(),
        can_undo: session.can_undo(),
        can_redo: session.can_redo(),
        pending_crop: session.pending_crop().is_some(),
    };
    merge_preview(&mut slot.lock().unwrap_or_else(|e| e.into_inner()), preview);
}
fn merge_preview(slot: &mut Option<Preview>, mut preview: Preview) {
    // The UI may skip frames. Keep the latest version of *every* dirty tile until consumed.
    if !preview.reset
        && let Some(previous) = slot.take()
    {
        preview.reset = previous.reset;
        let mut tiles = std::collections::BTreeMap::new();
        for tile in previous.tiles.into_iter().chain(preview.tiles) {
            tiles.insert((tile.rect.x, tile.rect.y), tile);
        }
        preview.tiles = tiles.into_values().collect();
    }
    *slot = Some(preview);
}
/// Common quick-annotation tools; text ownership is checked before this map.
pub fn annotation_tool_shortcut(key: char) -> Option<a::Tool> {
    Some(match key.to_ascii_lowercase() {
        'v' => a::Tool::Select, 'a' => a::Tool::Arrow, 'r' => a::Tool::Rectangle,
        'e' | 'c' => a::Tool::Ellipse, 'f' => a::Tool::Freehand, 'l' => a::Tool::Line,
        't' => a::Tool::Text, 'n' => a::Tool::Step, 'h' => a::Tool::Highlight,
        'b' => a::Tool::Blur, 'p' => a::Tool::Pixelate, 'o' => a::Tool::Balloon,
        'm' => a::Tool::Magnify, 's' => a::Tool::Spotlight, _ => return None,
    })
}
fn canvas_input(session: &mut a::EditorSession, key: a::Key, text: Option<&str>, mods: a::Modifiers, repeat: bool) -> Option<CanvasAction> {
    // Holding Return/Escape must not advance several editing/exit stages.
    if repeat && (matches!(key, a::Key::Enter | a::Key::Escape)
        || (mods.ctrl && matches!(key, a::Key::Char('s' | 'S' | 'q' | 'Q')))) { return None; }
    // Explicit completion works even while a selected text annotation owns Enter.
    if mods.ctrl && (key == a::Key::Enter || matches!(key, a::Key::Char('s' | 'S'))) { return Some(CanvasAction::Continue); }
    if mods.ctrl && matches!(key, a::Key::Char('q' | 'Q')) { return Some(CanvasAction::Cancel); }
    if mods.ctrl && mods.shift && matches!(key, a::Key::Char('c' | 'C')) { return (!repeat).then_some(CanvasAction::CopyImage); }
    if session.is_editing_text() {
        if !mods.ctrl && !mods.alt && matches!(key, a::Key::Char(_)) && let Some(text) = text { session.text_insert(text); }
        else { session.key_down(key, mods); }
        return None;
    }
    if mods.ctrl && matches!(key, a::Key::Char('c' | 'C')) { return (!repeat).then_some(CanvasAction::CopyImage); }
    if !mods.ctrl && !mods.alt && let a::Key::Char(character) = key
        && let Some(tool) = annotation_tool_shortcut(character)
    { session.set_tool(tool); return None; }
    if session.key_down(key, mods) { return None; }
    match key {
        a::Key::Enter => Some(CanvasAction::Continue),
        a::Key::Escape => Some(CanvasAction::Cancel),
        _ => None,
    }
}
/// Ctrl+Shift+C remains the explicit image-copy shortcut.
pub fn canvas_clipboard_key(key: a::Key, mods: a::Modifiers) -> bool {
    mods.ctrl && !mods.alt && matches!(key, a::Key::Char('c' | 'C' | 'x' | 'X' | 'v' | 'V'))
        && !(mods.shift && matches!(key, a::Key::Char('c' | 'C')))
}
fn canvas_clipboard(session: &mut a::EditorSession, request: u64, key: a::Key,
    modifiers: a::Modifiers, queue: &Queue, events: &mpsc::Sender<Event>) -> std::result::Result<(), String> {
    let owner = session.text_edit_owner().ok_or("Canvas text editing ended")?;
    let paste = matches!(key, a::Key::Char('v' | 'V'));
    let operation = if paste { CanvasClipboardOperation::Read } else {
        let content = session.document().object(owner.0).and_then(|object| object.kind.text_content())
            .ok_or("The text annotation no longer exists")?;
        let (lo, hi) = (owner.1.min(owner.2), owner.1.max(owner.2));
        if lo == hi { return Ok(()); }
        let selected = content.text.get(lo..hi).ok_or("Text selection changed")?;
        if selected.len() > CANVAS_CLIPBOARD_LIMIT { return Err("The selected text exceeds the 1 MiB clipboard limit".into()); }
        CanvasClipboardOperation::Write(selected.to_owned())
    };
    let (reply, completed) = mpsc::sync_channel(1);
    *queue.clipboard_cancel.lock().unwrap_or_else(|error| error.into_inner()) = Some(reply.clone());
    let active = Arc::new(AtomicBool::new(true));
    let _wait = ClipboardWait { queue, active: active.clone() };
    events.send(Event::CanvasClipboard { request, operation, reply: CanvasClipboardReply { active, reply } }).map_err(|_| "The editor closed")?;
    queue.notify_ui();
    // The worker owns the session throughout this barrier. Later input remains
    // ordered in the bounded command queue; no queue/UI mutex is held while waiting.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let payload = loop {
        if queue.closed.load(Ordering::Acquire) { return Err("The editor closed".into()); }
        match completed.recv_timeout(std::time::Duration::from_millis(10)) {
            Ok(result) => break result?,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("The clipboard operation was cancelled".into()),
            Err(mpsc::RecvTimeoutError::Timeout) if std::time::Instant::now() >= deadline => return Err("The clipboard operation timed out; retry the shortcut".into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    if queue.closed.load(Ordering::Acquire) || session.text_edit_owner() != Some(owner) {
        return Err("The text selection changed before clipboard completion".into());
    }
    if paste {
        if let Some(text) = payload {
            if text.len() > CANVAS_CLIPBOARD_LIMIT { return Err("Clipboard text exceeds the 1 MiB limit".into()); }
            session.text_insert(&text);
        }
    } else {
        // Cut deletes only after the UI acknowledged its clipboard write.
        session.key_down(key, modifiers);
    }
    Ok(())
}
#[cfg(test)]
fn apply(session: &mut a::EditorSession, view: &mut View, command: Command, events: &mpsc::Sender<Event>) -> Result<()> {
    apply_with_clipboard(session, view, command, events, None)
}
fn apply_with_clipboard(
    session: &mut a::EditorSession,
    view: &mut View,
    command: Command,
    events: &mpsc::Sender<Event>,
    clipboard: Option<(&Queue, &mpsc::Sender<Event>)>,
) -> Result<()> {
    view.command_revision = view.command_revision.saturating_add(1);
    if !matches!(&command, Command::Viewport(..) | Command::Zoom(..) | Command::Pan(..)) {
        view.last_interaction_revision = view.command_revision;
    }
    match command {
        Command::Tool(tool) => session.set_tool(tool),
        Command::Down(point, mods) => session.pointer_down(point, mods, None),
        Command::Move(point, mods) => session.pointer_move(point, mods, None),
        Command::Up(point, mods) => session.pointer_up(point, mods),
        Command::Key(key, mods) => {
            session.key_down(key, mods);
        }
        Command::CanvasInput { request, key, text, modifiers, repeat } => {
            let clipboard_key = canvas_clipboard_key(key, modifiers) && session.is_editing_text();
            let action = if clipboard_key && let Some((queue, notifications)) = clipboard {
                if (!repeat || matches!(key, a::Key::Char('v' | 'V')))
                    && let Err(detail) = canvas_clipboard(session, request, key, modifiers, queue, notifications) {
                    let _ = notifications.send(Event::CanvasClipboardError { request, detail });
                }
                None
            } else { canvas_input(session, key, text.as_deref(), modifiers, repeat) };
            let _ = events.send(Event::CanvasResult { request, command_revision: view.command_revision, action });
        }
        Command::Text(text) => session.text_insert(&text),
        Command::Label(text) => {
            // Creation defaults must never overwrite a selected annotation.
            let tool = session.tool();
            if let Some(mut preset) = session.styles().get(tool)
                && let Some(content) = preset.kind.text_content_mut()
            { content.text = text; session.styles_mut().remember(tool, preset); }
        }
        Command::ReviewLabel { target, tool } => {
            if let Some(id) = target {
                let object = session.document().object(id).context("This annotation was deleted. Undo its deletion to review the retained draft, or discard the draft.")?;
                anyhow::ensure!(a::Tool::for_kind(&object.kind) == Some(tool), "Annotation type changed. Discard this retained draft.");
                session.set_tool(a::Tool::Select);
                session.select(&[id]);
            } else {
                session.clear_selection();
                session.set_tool(tool);
            }
        }
        Command::ApplyLabel { request, target, tool, expected, text } => {
            let matches = properties(session).is_some_and(|p| p.selected <= 1 && p.selected_id == target
                && p.tool == tool && p.text.as_deref() == Some(expected.as_str()));
            let result = if matches {
                session.commit_text_edit();
                session.set_kind_props(|kind| { if let Some(content) = kind.text_content_mut() { content.text = text.clone(); } });
                Ok(text)
            } else { Err("Text or selection changed before Apply. Reload the selected text and try again.".into()) };
            let _ = events.send(Event::LabelResult { request, target, command_revision: view.command_revision, result });
        }
        Command::Undo => session.undo(),
        Command::Redo => session.redo(),
        Command::Delete => session.delete_selection(),
        Command::ClearSelection => session.clear_selection(),
        Command::Duplicate => session.duplicate(),
        Command::Front => session.bring_to_front(),
        Command::Back => session.send_to_back(),
        Command::Stroke(color) => {
            let fill_color = properties(session)
                .is_some_and(|p| matches!(p.tool, a::Tool::Highlight | a::Tool::Step));
            session.set_style(|s| {
                s.stroke = color;
                if fill_color {
                    s.fill = a::Fill::solid(color);
                }
            });
            session.set_kind_props(|kind| {
                if let a::ObjectKind::Text(shape) = kind {
                    shape.content.color = color
                }
            });
        }
        Command::Width(width) => session.set_style(|s| s.stroke_width = width.clamp(0.5, 64.)),
        Command::EffectAmount(value) => session.set_kind_props(|kind| match kind {
            a::ObjectKind::Blur(s) | a::ObjectKind::Pixelate(s) => s.amount = value.clamp(1., 128.),
            _ => {}
        }),
        Command::Opacity(value) => session.set_style(|s| s.opacity = value.clamp(0.05, 1.)),
        Command::MagnifyZoom(value) => session.set_kind_props(|kind| {
            if let a::ObjectKind::Magnify(m) = kind {
                m.zoom = value.clamp(1., 8.);
            }
        }),
        Command::Fill(fill) => session.set_style(|s| {
            s.fill = if fill {
                a::Fill::solid(s.stroke)
            } else {
                a::Fill::None
            }
        }),
        Command::FontSize(value) => session.set_kind_props(|kind| match kind {
            a::ObjectKind::Text(shape) => shape.content.font.size = value.clamp(6., 256.),
            a::ObjectKind::Balloon(shape) => shape.content.font.size = value.clamp(6., 256.),
            _ => {}
        }),
        Command::Viewport(w, h) => {
            view.width = w.clamp(1, 4096);
            view.height = h.clamp(1, 4096);
        }
        Command::Zoom(value) => {
            view.zoom = value;
            view.pan = (0., 0.);
        }
        Command::Pan(x, y) => {
            view.pan.0 += x;
            view.pan.1 += y;
        }
        Command::ApplyCrop => session.apply_crop()?,
        Command::Rotate => session.orient(a::object::Orient::Rotate90)?,
        Command::Flip => session.orient(a::object::Orient::FlipH)?,
        Command::Effect(effect) => session.apply_effect(&effect, None)?,
        Command::InsertImage(path) => {
            let frame = Frame::decode(&std::fs::read(path)?)?;
            session.insert_image(frame, None);
        }
        Command::CycleSelection(backwards) => {
            let ids: Vec<_> = session.document().objects().iter()
                .filter(|o| o.visible && !o.locked).map(|o| o.id).collect();
            if !ids.is_empty() {
                let current = session.selection().first().and_then(|id| ids.iter().position(|i| i == id));
                let next = current.map_or(if backwards { ids.len() - 1 } else { 0 }, |i|
                    if backwards { (i + ids.len() - 1) % ids.len() } else { (i + 1) % ids.len() });
                session.set_tool(a::Tool::Select);
                session.select(&[ids[next]]);
            }
        }
        Command::Checkpoint { path, recovery, reply } => {
            let result = (|| -> Result<()> {
                session.commit_text_edit();
                let parent = path.parent().context("Project has no parent directory")?;
                std::fs::create_dir_all(parent)?;
                let text = session.document().to_json()?;
                if recovery {
                    crate::core::settings::atomic_write(&path, text.as_bytes())?;
                } else {
                    crate::utilities::save_new(&path, text.as_bytes())?;
                }
                Ok(())
            })().map_err(|e| format!("{e:#}"));
            let _ = reply.send(result);
        }
        Command::Project(path) => {
            session.commit_text_edit();
            let parent = path.parent().context("Project has no parent directory")?;
            std::fs::create_dir_all(parent)?;
            // Keep project writes atomic even though upstream's convenience save is direct.
            let text = session.document().to_json()?;
            crate::core::settings::atomic_write(&path, text.as_bytes())?;
            let _ = events.send(Event::Saved(path));
        }
        Command::Copy => {
            let _ = events.send(Event::Copied(session.render(&Default::default())));
        }
        Command::Accept => {
            session.commit_text_edit();
            if session.pending_crop().is_some() {
                session.apply_crop()?;
            }
            let _ = events.send(Event::Accepted(session.render(&Default::default())));
        }
        Command::AcceptRegion(rect) => {
            session.commit_text_edit();
            let frame = session.render(&Default::default()).crop(rect)?;
            let _ = events.send(Event::Accepted(frame));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editable_checkpoint_acknowledges_failure_without_losing_document_or_clobbering_output() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("editable.ssxe");
        let mut session = a::EditorSession::from_frame(effects::solid_frame(80, 60, [255; 4])).unwrap();
        session.set_tool(a::Tool::Rectangle);
        session.pointer_down(a::PointF::new(5., 5.), a::Modifiers::NONE, None);
        session.pointer_move(a::PointF::new(30., 25.), a::Modifiers::NONE, None);
        session.pointer_up(a::PointF::new(30., 25.), a::Modifiers::NONE);
        let expected = session.document().clone();
        let (events, _) = mpsc::channel();
        let mut view = View::default();
        let mut checkpoint = |session: &mut a::EditorSession, path: PathBuf, recovery| {
            let (reply, result) = mpsc::channel();
            apply(session, &mut view, Command::Checkpoint { path, recovery, reply }, &events).unwrap();
            result.try_recv().unwrap()
        };
        checkpoint(&mut session, path.clone(), false).unwrap();
        assert_eq!(a::project::load(&path).unwrap(), expected);
        let bytes = std::fs::read(&path).unwrap();
        session.delete_selection();
        assert!(checkpoint(&mut session, path.clone(), false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        session.undo();
        assert_eq!(session.document(), &expected, "Failed save must retain the editable session and undo history");
        // A file in place of the parent gives a deterministic storage failure on every OS.
        let blocker = directory.path().join("blocked");
        std::fs::write(&blocker, b"keep me").unwrap();
        assert!(checkpoint(&mut session, blocker.join("recovery.ssxe"), true).is_err());
        assert_eq!(session.document(), &expected);
        let recovery = directory.path().join("recovery.ssxe");
        checkpoint(&mut session, recovery.clone(), true).unwrap();
        assert_eq!(a::project::load(&recovery).unwrap(), expected);
    }

    #[test]
    fn keyboard_annotation_selection_cycles_and_arrow_nudge_is_undoable() {
        let mut session = a::EditorSession::from_frame(effects::solid_frame(80, 60, [255; 4])).unwrap();
        session.set_tool(a::Tool::Rectangle);
        for x in [5., 40.] {
            session.pointer_down(a::PointF::new(x, 5.), a::Modifiers::NONE, None);
            session.pointer_move(a::PointF::new(x + 20., 25.), a::Modifiers::NONE, None);
            session.pointer_up(a::PointF::new(x + 20., 25.), a::Modifiers::NONE);
        }
        let ids: Vec<_> = session.document().objects().iter().map(|o| o.id).collect();
        session.clear_selection();
        let (events, _) = mpsc::channel();
        let mut view = View::default();
        apply(&mut session, &mut view, Command::CycleSelection(false), &events).unwrap();
        assert_eq!(session.selection(), &[ids[0]]);
        apply(&mut session, &mut view, Command::CycleSelection(false), &events).unwrap();
        assert_eq!(session.selection(), &[ids[1]]);
        let before = session.document().clone();
        apply(&mut session, &mut view, Command::Key(a::Key::Right, a::Modifiers::SHIFT), &events).unwrap();
        assert_ne!(session.document(), &before);
        session.undo();
        assert_eq!(session.document(), &before);
        apply(&mut session, &mut view, Command::CycleSelection(true), &events).unwrap();
        assert_eq!(session.selection(), &[ids[0]]);
    }

    #[test]
    fn preview_workspace_planner_bounds_8k_effects_without_changing_logical_zoom() {
        let mut document = a::Document::new(effects::solid_frame(32, 32, [255; 4])).unwrap();
        document.set_padding(a::Padding { left: 0, top: 0, right: 7648, bottom: 4288 });
        let logical = crate::types::Rect::new(29003, 17005, 1920, 1080);
        let plain = a::Renderer::plan_preview_raster(&document, 8., logical, PREVIEW_WORK_PIXELS);
        assert_eq!(plain.density, 1., "An ordinary small high-zoom view keeps pixel detail");
        let id = document.alloc_id();
        document.insert_object(0, a::Object::new(id,
            a::Style { shadow: Some(a::style::Shadow::default()), ..Default::default() },
            a::ObjectKind::Rectangle(a::object::BoxShape {
                rect: a::RectF::new(0., 0., 7680., 4320.), rotation: 0.,
            })));
        for viewport in [logical, crate::types::Rect::new(0, 0, 7680, 4320),
            crate::types::Rect::new(-3001, -2003, 16000, 9000)] {
            let plan = a::Renderer::plan_preview_raster(&document, 8., viewport, PREVIEW_WORK_PIXELS);
            assert!(plan.density.is_finite() && plan.density > 0. && plan.density < 1.);
            assert!(plan.work_pixels <= PREVIEW_WORK_PIXELS);
            let d = f64::from(plan.density);
            assert!(f64::from(plan.viewport.x) <= f64::from(viewport.x) * d);
            assert!(f64::from(plan.viewport.y) <= f64::from(viewport.y) * d);
            assert!(f64::from(plan.viewport.x) + f64::from(plan.viewport.width)
                >= (f64::from(viewport.x) + f64::from(viewport.width)) * d);
            assert!(f64::from(plan.viewport.y) + f64::from(plan.viewport.height)
                >= (f64::from(viewport.y) + f64::from(viewport.height)) * d);
        }
    }

    #[test]
    fn reduced_density_preserves_hit_testing_and_global_raster_phase() {
        let mut frame = effects::solid_frame(800, 600, [20, 30, 40, 255]);
        for y in 0..600 {
            for (x, pixel) in frame.row_mut(y).as_chunks_mut::<4>().0.iter_mut().enumerate() {
                pixel[0] = (x % 251) as u8;
                pixel[1] = (y % 239) as u8;
            }
        }
        let mut session = a::EditorSession::from_frame(frame).unwrap();
        let view = View { width: 2000, height: 1500, zoom: Some(4.), pan: (13., 17.), ..Default::default() };
        let slot = Mutex::new(None);
        let mut cache = RasterCache::default();
        let budget = 1024 * 1024;
        publish_with_work_budget(&mut session, &view, &slot, &mut cache, budget);
        let before = slot.lock().unwrap().take().unwrap();
        session.set_tool(a::Tool::Rectangle);
        session.set_style(|style| {
            style.shadow = Some(a::style::Shadow { blur: 0., ..Default::default() });
        });
        session.pointer_down(a::PointF::new(1., 1.), a::Modifiers::NONE, None);
        session.pointer_up(a::PointF::new(799., 599.), a::Modifiers::NONE);
        publish_with_work_budget(&mut session, &view, &slot, &mut cache, budget);
        let after = slot.lock().unwrap().take().unwrap();
        assert!(after.detail_limited && after.reset);
        assert!(after.raster_density < before.raster_density);
        assert_eq!((after.width, after.height, after.scale, after.offset),
            (before.width, before.height, before.scale, before.offset));
        for (x, y) in [(0., 0.), (73.25, 99.5), (1999., 1499.)] {
            let image = after.image_point(x, y);
            assert_eq!(image, before.image_point(x, y), "Density changes cannot move pointer targets");
            let output = after.output_point(image);
            assert!((output.x - x).abs() < 0.001 && (output.y - y).abs() < 0.001);
        }
        let logical = crate::types::Rect::new(after.offset.0 as i32, after.offset.1 as i32, after.width, after.height);
        let plan = a::Renderer::plan_preview_raster(session.document(), after.scale, logical, budget);
        assert!(plan.work_pixels <= budget);
        assert_eq!(plan.density, after.raster_density);
        let mut expected = session.render(&a::RenderOptions {
            scale: after.scale * plan.density, viewport: Some(plan.viewport), ..Default::default()
        }).into_data();
        for pixel in expected.as_chunks_mut::<4>().0 { pixel.swap(0, 2); }
        let mut actual = vec![0; expected.len()];
        for tile in &after.tiles {
            for row in 0..tile.rect.height as usize {
                let destination = ((tile.rect.y as usize + row) * plan.viewport.width as usize + tile.rect.x as usize) * 4;
                let source = row * tile.rect.width as usize * 4;
                actual[destination..destination + tile.rect.width as usize * 4]
                    .copy_from_slice(&tile.bgra[source..source + tile.rect.width as usize * 4]);
            }
        }
        assert_eq!(actual.iter().zip(&expected).position(|(left, right)| left != right), None,
            "Fractional cover edges must share full-render sampling phase");
        let export = session.render(&Default::default());
        assert_eq!((export.width(), export.height()), (800, 600), "Interactive density must not resize exports");
    }

    #[test]
    fn live_effect_bounds_and_properties_are_ui_only_and_undoable() {
        for tool in [
            a::Tool::Blur,
            a::Tool::Pixelate,
            a::Tool::Highlight,
            a::Tool::Spotlight,
            a::Tool::Magnify,
        ] {
            let mut session =
                a::EditorSession::from_frame(effects::solid_frame(160, 100, [90, 120, 150, 255]))
                    .unwrap();
            session.set_tool(tool);
            session.pointer_down(a::PointF::new(20., 20.), a::Modifiers::NONE, None);
            session.pointer_move(a::PointF::new(100., 70.), a::Modifiers::NONE, None);
            let before = session.render(&Default::default());
            let overlay = session.overlay();
            assert_eq!(
                overlay.creation.unwrap().rect,
                a::RectF::new(20., 20., 80., 50.)
            );
            assert_eq!(
                session.render(&Default::default()),
                before,
                "UI guides must never enter exported image pixels"
            );
            session.pointer_up(a::PointF::new(100., 70.), a::Modifiers::NONE);
            assert!(session.overlay().creation.is_none());
            assert_eq!(session.overlay().selection.len(), 1);
            let p = properties(&session).unwrap();
            assert_eq!(p.tool, tool);
            assert_eq!(p.selected, 1);
            if p.amount.is_some() {
                let (tx, _) = mpsc::channel();
                apply(
                    &mut session,
                    &mut View::default(),
                    Command::EffectAmount(24.),
                    &tx,
                )
                .unwrap();
                assert_eq!(properties(&session).unwrap().amount, Some(24.));
                session.undo();
                assert_eq!(properties(&session).unwrap().amount, Some(10.));
                session.redo();
                assert_eq!(properties(&session).unwrap().amount, Some(24.));
            }
        }
    }
    #[test]
    fn selection_properties_follow_object_instead_of_active_tool() {
        let mut session =
            a::EditorSession::from_frame(effects::solid_frame(160, 100, [255; 4])).unwrap();
        let (tx, _) = mpsc::channel();
        let mut view = View::default();
        session.set_tool(a::Tool::Rectangle);
        session.pointer_down(a::PointF::new(10., 10.), a::Modifiers::NONE, None);
        session.pointer_up(a::PointF::new(50., 50.), a::Modifiers::NONE);
        let first = session.selection()[0];
        apply(
            &mut session,
            &mut view,
            Command::Stroke(a::Color::rgb(10, 20, 30)),
            &tx,
        )
        .unwrap();
        apply(&mut session, &mut view, Command::Width(7.), &tx).unwrap();
        session.clear_selection();
        session.set_tool(a::Tool::Text);
        session.pointer_down(a::PointF::new(80., 20.), a::Modifiers::NONE, None);
        session.pointer_up(a::PointF::new(140., 80.), a::Modifiers::NONE);
        session.select(&[first]);
        let p = properties(&session).unwrap();
        assert_eq!(p.tool, a::Tool::Rectangle);
        assert_eq!(p.color, a::Color::rgb(10, 20, 30));
        assert_eq!(p.width, 7.);
        assert_eq!(p.font, None);
        session.clear_selection();
        session.set_tool(a::Tool::Highlight);
        apply(
            &mut session,
            &mut view,
            Command::Stroke(a::Color::rgb(1, 2, 3)),
            &tx,
        )
        .unwrap();
        let p = properties(&session).unwrap();
        assert_eq!(p.color, a::Color::rgb(1, 2, 3));
        assert!(p.fill);
    }
    #[test]
    fn text_apply_guards_identity_baseline_and_multiselection_and_preserves_creation_defaults() {
        let mut session = a::EditorSession::from_frame(effects::solid_frame(320, 180, [235, 240, 245, 255])).unwrap();
        let first = session.add_object(a::Object::new(a::ObjectId(0), Default::default(), a::ObjectKind::Text(a::object::TextShape {
            rect: a::RectF::new(20., 20., 130., 40.), content: a::object::TextContent { text: "Alpha".into(), ..Default::default() }, ..Default::default() })));
        let second = session.add_object(a::Object::new(a::ObjectId(0), Default::default(), a::ObjectKind::Balloon(a::object::BalloonShape {
            rect: a::RectF::new(170., 80., 130., 70.), tail: a::PointF::new(160., 160.),
            content: a::object::TextContent { text: "Bravo".into(), ..Default::default() }, ..Default::default() })));
        let (tx, rx) = mpsc::channel();
        let mut view = View::default();
        session.set_tool(a::Tool::Text);
        apply(&mut session, &mut view, Command::Label("Future label".into()), &tx).unwrap();
        assert_eq!(properties(&session).unwrap().text.as_deref(), Some("Bravo"), "Creation default overwrote selected Balloon");
        assert_eq!(session.styles().get(a::Tool::Text).unwrap().kind.text_content().unwrap().text, "Future label");
        let baseline = session.render(&Default::default());
        for (request, target, tool, expected) in [(1, first, a::Tool::Text, "Alpha"), (2, second, a::Tool::Balloon, "Outdated text")] {
            apply(&mut session, &mut view, Command::ApplyLabel { request, target: Some(target), tool, expected: expected.into(), text: "Wrong".into() }, &tx).unwrap();
            assert!(matches!(rx.try_recv().unwrap(), Event::LabelResult { result: Err(_), .. }));
            assert_eq!(session.render(&Default::default()), baseline);
        }
        apply(&mut session, &mut view, Command::ApplyLabel { request: 3, target: Some(second), tool: a::Tool::Balloon,
            expected: "Bravo".into(), text: "Updated Bravo".into() }, &tx).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Event::LabelResult { request: 3, target: Some(id), result: Ok(text), .. } if id == second && text == "Updated Bravo"));
        assert_ne!(session.render(&Default::default()), baseline);
        session.undo();
        assert_eq!(session.render(&Default::default()), baseline);
        session.key_down(a::Key::Char('a'), a::Modifiers { ctrl: true, ..Default::default() });
        let p = properties(&session).unwrap();
        assert_eq!(p.selected, 2); assert_eq!(p.selected_id, None); assert_eq!(p.text, None);
        apply(&mut session, &mut view, Command::ApplyLabel { request: 4, target: None, tool: a::Tool::Balloon,
            expected: "Bravo".into(), text: "Wrong".into() }, &tx).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Event::LabelResult { result: Err(_), .. }));
        assert_eq!(session.render(&Default::default()), baseline);
    }
    #[test]
    fn canvas_shortcut_and_first_text_letters_share_one_actor_batch_without_losing_text() {
        use std::time::{Duration, Instant};
        let blank = effects::solid_frame(320, 180, [235, 240, 245, 255]);
        let actor = Actor::new(Source::Image(blank.clone())).unwrap();
        let input = |request, key, text: Option<&str>, modifiers| Command::CanvasInput {
            request, key, text: text.map(str::to_owned), modifiers, repeat: false };
        {
            let mut commands = actor.queue.items.lock().unwrap();
            commands.extend([
                input(1, a::Key::Char('t'), Some("t"), a::Modifiers::NONE),
                Command::Down(a::PointF::new(20., 20.), a::Modifiers::NONE),
                Command::Up(a::PointF::new(120., 60.), a::Modifiers::NONE),
                input(2, a::Key::Char('r'), Some("r"), a::Modifiers::NONE),
                input(3, a::Key::Enter, None, a::Modifiers { shift: true, ..Default::default() }),
                input(4, a::Key::Char('p'), Some("p"), a::Modifiers::NONE),
                input(5, a::Key::Enter, None, a::Modifiers::NONE), Command::Accept,
            ]);
        }
        actor.queue.wake.notify_one();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut replies = 0;
        let mut accepted = false;
        loop {
            match actor.events.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("Canvas batch did not complete") {
                Event::CanvasResult { request, action, .. } => { replies += 1; assert_eq!(request, replies); assert_eq!(action, None, "Typing or committing text invoked an image action"); }
                Event::Accepted(frame) => { assert_ne!(frame, blank); accepted = true; }
                Event::Error(error) => panic!("Canvas batch failed: {error}"),
                _ => panic!("Unexpected canvas event"),
            }
            if replies == 5 && accepted { break; }
        }
        assert_eq!(replies, 5);
        let preview = loop {
            if let Some(preview) = actor.take_preview() && preview.command_revision >= 8 { break preview; }
            assert!(Instant::now() < deadline, "Canvas batch did not publish");
            thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(preview.active_tool, a::Tool::Text);
        assert!(!preview.text_editing);
        assert_eq!(preview.properties.unwrap().text.as_deref(), Some("r\np"));
    }
    #[test]
    fn canvas_clipboard_is_ordered_repeated_and_failed_cut_keeps_text() {
        use std::time::Duration;
        let actor = Actor::new(Source::Image(effects::solid_frame(200, 100, [255; 4]))).unwrap();
        let key = |request, character| Command::CanvasInput { request, key: a::Key::Char(character), text: None,
            modifiers: a::Modifiers::CTRL, repeat: false };
        {
            let mut commands = actor.queue.items.lock().unwrap();
            commands.extend([Command::Tool(a::Tool::Text),
                Command::Down(a::PointF::new(10., 10.), a::Modifiers::NONE),
                Command::Up(a::PointF::new(100., 50.), a::Modifiers::NONE),
                Command::Text("Seed".into()), key(1, 'a'), key(2, 'c'), key(3, 'v'), key(4, 'v'),
                key(5, 'a'), key(6, 'x')]);
        }
        actor.queue.wake.notify_one();
        let mut clipboard = String::new();
        let mut operations = Vec::new();
        let mut acknowledged = Vec::new();
        let mut failed_cut = false;
        while acknowledged.len() < 6 {
            match actor.events.recv_timeout(Duration::from_secs(5)).expect("Ordered clipboard Actor stalled") {
                Event::CanvasClipboard { request, operation, reply } => {
                    operations.push(request);
                    match operation {
                        CanvasClipboardOperation::Write(text) if request == 2 => {
                            assert_eq!(text, "Seed"); clipboard = text; reply.respond(|| Ok(None));
                        }
                        CanvasClipboardOperation::Write(text) if request == 6 => {
                            assert_eq!(text, "SeedSeed"); reply.respond(|| Err("Injected write failure".into()));
                        }
                        CanvasClipboardOperation::Read => reply.respond(|| Ok(Some(clipboard.clone()))),
                        _ => panic!("Unexpected clipboard operation"),
                    }
                }
                Event::CanvasClipboardError { request: 6, detail } => { assert_eq!(detail, "Injected write failure"); failed_cut = true; }
                Event::CanvasResult { request, action, .. } => { assert_eq!(action, None); acknowledged.push(request); }
                Event::Error(error) => panic!("Clipboard Actor error: {error}"),
                _ => panic!("Unexpected clipboard event"),
            }
        }
        assert_eq!(operations, [2, 3, 4, 6]);
        assert_eq!(acknowledged, [1, 2, 3, 4, 5, 6]);
        assert!(failed_cut);
        let preview = actor.take_preview().expect("Clipboard completion preceded its preview");
        assert_eq!(preview.properties.unwrap().text.as_deref(), Some("SeedSeed"));
        assert!(preview.text_editing);
    }
    #[test]
    fn queued_target_change_cannot_redirect_an_earlier_paste() {
        use std::time::Duration;
        let actor = Actor::new(Source::Image(effects::solid_frame(240, 160, [255; 4]))).unwrap();
        let input = |request, key| Command::CanvasInput { request, key, text: None, modifiers: a::Modifiers::CTRL, repeat: false };
        {
            let mut commands = actor.queue.items.lock().unwrap();
            commands.extend([Command::Tool(a::Tool::Text),
                Command::Down(a::PointF::new(10., 10.), a::Modifiers::NONE),
                Command::Up(a::PointF::new(80., 40.), a::Modifiers::NONE), Command::Text("A".into()),
                input(1, a::Key::Char('v')), Command::Key(a::Key::Char('a'), a::Modifiers::CTRL), input(2, a::Key::Char('c')),
                Command::Key(a::Key::Enter, a::Modifiers::NONE), Command::Tool(a::Tool::Text),
                Command::Down(a::PointF::new(130., 100.), a::Modifiers::NONE),
                Command::Up(a::PointF::new(200., 140.), a::Modifiers::NONE), Command::Text("B".into()),
                input(3, a::Key::Char('v')), Command::Key(a::Key::Char('a'), a::Modifiers::CTRL), input(4, a::Key::Char('c'))]);
        }
        actor.queue.wake.notify_one();
        let mut acknowledgements = 0;
        let mut copied = Vec::new();
        while acknowledgements < 4 {
            match actor.events.recv_timeout(Duration::from_secs(5)).expect("Target-change clipboard barrier stalled") {
                Event::CanvasClipboard { request, operation: CanvasClipboardOperation::Read, reply } =>
                    reply.respond(|| Ok(Some(if request == 1 { "X" } else { "Y" }.into()))),
                Event::CanvasClipboard { operation: CanvasClipboardOperation::Write(text), reply, .. } => {
                    copied.push(text); reply.respond(|| Ok(None));
                }
                Event::CanvasResult { .. } => acknowledgements += 1,
                Event::CanvasClipboardError { detail, .. } | Event::Error(detail) => panic!("{detail}"),
                _ => panic!("Unexpected target-change event"),
            }
        }
        assert_eq!(copied, ["AX", "BY"]);
        let preview = actor.take_preview().unwrap();
        assert_eq!(preview.objects, 2);
        assert_eq!(preview.properties.unwrap().text.as_deref(), Some("BY"));
    }
    #[test]
    fn expired_clipboard_reply_has_no_external_side_effect() {
        let active = Arc::new(AtomicBool::new(false));
        let (reply, completed) = mpsc::sync_channel(1);
        CanvasClipboardReply { active, reply }.respond(|| panic!("Expired operation accessed clipboard"));
        assert!(matches!(completed.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
    }
    #[test]
    fn dropping_actor_cancels_held_clipboard_without_late_read() {
        use std::time::{Duration, Instant};
        let actor = Actor::new(Source::Image(effects::solid_frame(200, 100, [255; 4]))).unwrap();
        {
            let mut commands = actor.queue.items.lock().unwrap();
            commands.extend([Command::Tool(a::Tool::Text),
                Command::Down(a::PointF::new(10., 10.), a::Modifiers::NONE),
                Command::Up(a::PointF::new(100., 50.), a::Modifiers::NONE),
                Command::CanvasInput { request: 1, key: a::Key::Char('v'), text: None, modifiers: a::Modifiers::CTRL, repeat: false }]);
        }
        actor.queue.wake.notify_one();
        let Event::CanvasClipboard { reply, .. } = actor.events.recv_timeout(Duration::from_secs(5)).unwrap() else { panic!("Missing paste request"); };
        let active = reply.active.clone();
        drop(actor);
        let deadline = Instant::now() + Duration::from_secs(1);
        while active.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "Closing editor failed to cancel clipboard barrier");
            thread::yield_now();
        }
        reply.respond(|| panic!("Closed editor accessed clipboard"));
    }
    #[test]
    fn idle_escape_cannot_close_later_batched_work_but_view_changes_do_not_revoke_it() {
        use std::time::{Duration, Instant};
        for later_work in [false, true] {
            let actor = Actor::new(Source::Image(effects::solid_frame(200, 100, [255; 4]))).unwrap();
            {
                let mut commands = actor.queue.items.lock().unwrap();
                commands.push_back(Command::CanvasInput { request: 1, key: a::Key::Escape, text: None, modifiers: a::Modifiers::NONE, repeat: false });
                if later_work {
                    commands.extend([Command::Tool(a::Tool::Text),
                        Command::Down(a::PointF::new(10., 10.), a::Modifiers::NONE),
                        Command::Up(a::PointF::new(100., 50.), a::Modifiers::NONE),
                        Command::Text("Newer live work".into())]);
                }
                commands.extend([Command::Viewport(300, 180), Command::Zoom(None), Command::Pan(0., 0.)]);
            }
            actor.queue.wake.notify_one();
            let Event::CanvasResult { request, command_revision, action } = actor.events.recv_timeout(Duration::from_secs(5)).expect("Missing idle Escape result") else { panic!("Unexpected Escape event"); };
            assert_eq!(request, 1); assert_eq!(command_revision, 1);
            assert_eq!(action, if later_work { None } else { Some(CanvasAction::Cancel) }, "Later canvas edits must revoke idle Cancel; view changes must not");
            let deadline = Instant::now() + Duration::from_secs(5);
            let preview = loop {
                if let Some(preview) = actor.take_preview() && preview.command_revision >= (if later_work { 8 } else { 4 }) { break preview; }
                assert!(Instant::now() < deadline, "Missing final Escape batch preview");
                thread::sleep(Duration::from_millis(1));
            };
            if later_work {
                assert!(preview.text_editing);
                assert_eq!(preview.properties.unwrap().text.as_deref(), Some("Newer live work"));
            } else { assert_eq!(preview.objects, 0); }
        }
    }
    #[test]
    fn canvas_escape_is_authoritative_staged_and_never_repeats_exit_actions() {
        let mut session = a::EditorSession::from_frame(effects::solid_frame(200, 100, [255; 4])).unwrap();
        let target = session.add_object(a::Object::new(a::ObjectId(0), Default::default(), a::ObjectKind::Text(a::object::TextShape {
            rect: a::RectF::new(10., 10., 120., 40.), content: a::object::TextContent { text: "Kept".into(), ..Default::default() }, ..Default::default() })));
        session.begin_text_edit(target);
        assert_eq!(canvas_input(&mut session, a::Key::Escape, None, a::Modifiers::NONE, false), None);
        assert!(!session.is_editing_text()); assert_eq!(session.selection(), &[target]);
        assert_eq!(canvas_input(&mut session, a::Key::Escape, None, a::Modifiers::NONE, true), None);
        assert_eq!(session.selection(), &[target], "Held Escape skipped the selection stage");
        assert_eq!(canvas_input(&mut session, a::Key::Escape, None, a::Modifiers::NONE, false), None);
        assert!(session.selection().is_empty());
        assert_eq!(canvas_input(&mut session, a::Key::Escape, None, a::Modifiers::NONE, true), None);
        assert_eq!(canvas_input(&mut session, a::Key::Escape, None, a::Modifiers::NONE, false), Some(CanvasAction::Cancel));
        session.set_tool(a::Tool::Crop);
        session.pointer_down(a::PointF::new(20., 20.), a::Modifiers::NONE, None);
        session.pointer_move(a::PointF::new(100., 70.), a::Modifiers::NONE, None);
        session.pointer_up(a::PointF::new(100., 70.), a::Modifiers::NONE);
        assert!(session.pending_crop().is_some());
        assert_eq!(canvas_input(&mut session, a::Key::Escape, None, a::Modifiers::NONE, false), None);
        assert!(session.pending_crop().is_none());
        assert_eq!(session.document().objects().len(), 1);
        assert_eq!(canvas_input(&mut session, a::Key::Enter, None, a::Modifiers::NONE, true), None);
        assert_eq!(canvas_input(&mut session, a::Key::Enter, None, a::Modifiers::NONE, false), Some(CanvasAction::Continue));
        session.select(&[target]); session.begin_text_edit(target);
        assert_eq!(canvas_input(&mut session, a::Key::Char('s'), None, a::Modifiers::CTRL, false), Some(CanvasAction::Continue));
        assert_eq!(canvas_input(&mut session, a::Key::Char('s'), None, a::Modifiers::CTRL, true), None);
        assert_eq!(canvas_input(&mut session, a::Key::Enter, None, a::Modifiers::CTRL, false), Some(CanvasAction::Continue));
        assert!(session.is_editing_text(), "Action routing must not apply or finish before the UI admits it");
    }
    #[test]
    fn batched_text_apply_then_undo_publishes_newer_revision_than_apply_ack() {
        let mut session = a::EditorSession::from_frame(effects::solid_frame(320, 180, [235, 240, 245, 255])).unwrap();
        let target = session.add_object(a::Object::new(a::ObjectId(0), Default::default(), a::ObjectKind::Text(a::object::TextShape {
            rect: a::RectF::new(20., 20., 130., 40.), content: a::object::TextContent { text: "Alpha".into(), ..Default::default() }, ..Default::default() })));
        let baseline = session.render(&Default::default());
        let actor = Actor::new(Source::Project(session.document().clone())).unwrap();
        // Admit one indivisible queue batch, independent of worker scheduling.
        {
            let mut commands = actor.queue.items.lock().unwrap();
            commands.extend([Command::ReviewLabel { target: Some(target), tool: a::Tool::Text },
                Command::ApplyLabel { request: 17, target: Some(target), tool: a::Tool::Text,
                    expected: "Alpha".into(), text: "Updated".into() }, Command::Undo, Command::Accept]);
        }
        actor.queue.wake.notify_one();
        let mut acknowledgement = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let accepted = loop {
            match actor.events.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).expect("Actor batch did not finish") {
                Event::LabelResult { request, target: acknowledged, command_revision, result } => {
                    assert_eq!(request, 17); assert_eq!(acknowledged, Some(target));
                    assert_eq!(result.unwrap(), "Updated"); acknowledgement = Some(command_revision);
                }
                Event::Accepted(frame) => break frame,
                Event::Error(error) => panic!("Actor batch failed: {error}"),
                _ => panic!("Unexpected Actor event"),
            }
        };
        let preview = loop {
            if let Some(preview) = actor.take_preview() && preview.command_revision >= 4 { break preview; }
            assert!(std::time::Instant::now() < deadline, "Actor batch did not publish");
            thread::sleep(std::time::Duration::from_millis(1));
        };
        assert!(preview.command_revision > acknowledgement.unwrap(), "Undo preview must supersede the Apply ACK");
        assert_eq!(preview.properties.unwrap().text.as_deref(), Some("Alpha"));
        assert_eq!(accepted, baseline);
        let mut view = View::default();
        let (tx, _) = mpsc::channel();
        apply(&mut session, &mut view, Command::ReviewLabel { target: Some(a::ObjectId(u64::MAX)), tool: a::Tool::Text }, &tx).unwrap_err();
        assert_eq!(session.selection(), &[target]);
        assert_eq!(session.render(&Default::default()), baseline);
    }
    fn apply_tiles(buffer: &mut Vec<u8>, width: &mut u32, height: &mut u32, p: Preview) {
        if p.reset {
            *width = p.width;
            *height = p.height;
            *buffer = vec![0; (p.width * p.height * 4) as usize];
        }
        for tile in p.tiles {
            for y in 0..tile.rect.height as usize {
                let dst = ((tile.rect.y as usize + y) * *width as usize + tile.rect.x as usize) * 4;
                let src = y * tile.rect.width as usize * 4;
                buffer[dst..dst + tile.rect.width as usize * 4]
                    .copy_from_slice(&tile.bgra[src..src + tile.rect.width as usize * 4]);
            }
        }
    }
    fn check_tiles(
        session: &mut a::EditorSession,
        view: &View,
        cache: &mut RasterCache,
        buffer: &mut Vec<u8>,
        width: &mut u32,
        height: &mut u32,
    ) -> usize {
        let slot = Mutex::new(None);
        publish(session, view, &slot, cache);
        let p = slot.into_inner().unwrap().unwrap();
        let bytes = p.tiles.iter().map(|t| t.bgra.len()).sum();
        let mut expected = session
            .render(&a::RenderOptions {
                scale: p.scale,
                viewport: Some(crate::types::Rect::new(
                    p.offset.0 as i32,
                    p.offset.1 as i32,
                    p.width,
                    p.height,
                )),
                ..Default::default()
            })
            .into_data();
        for p in expected.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
        }
        apply_tiles(buffer, width, height, p);
        assert_eq!(buffer.len(), expected.len());
        assert_eq!(
            buffer.iter().zip(&expected).position(|(a, b)| a != b),
            None,
            "Incremental preview differs from full reference at byte"
        );
        bytes
    }
    #[test]
    fn dirty_tiles_match_full_render_across_edits_effects_undo_and_zoom() {
        let mut base = effects::solid_frame(1400, 800, [30, 60, 90, 255]);
        for y in 0..800 {
            for (x, p) in base
                .row_mut(y)
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .enumerate()
            {
                *p = [
                    (x % 251) as u8,
                    (y % 239) as u8,
                    ((x + y as usize) % 233) as u8,
                    255,
                ];
            }
        }
        let mut session = a::EditorSession::from_frame(base).unwrap();
        let mut view = View {
            width: 1400,
            height: 800,
            ..Default::default()
        };
        let mut cache = RasterCache::default();
        let (mut buffer, mut width, mut height) = (vec![], 0, 0);
        let (tx, _) = mpsc::channel();
        assert_eq!(
            check_tiles(
                &mut session,
                &view,
                &mut cache,
                &mut buffer,
                &mut width,
                &mut height
            ),
            1400 * 800 * 4
        );
        session.set_tool(a::Tool::Rectangle);
        assert_eq!(
            check_tiles(
                &mut session,
                &view,
                &mut cache,
                &mut buffer,
                &mut width,
                &mut height
            ),
            0,
            "Tool selection must not rasterize"
        );
        for command in [
            Command::Down(a::PointF::new(100., 100.), a::Modifiers::NONE),
            Command::Move(a::PointF::new(250., 230.), a::Modifiers::NONE),
            Command::Up(a::PointF::new(250., 230.), a::Modifiers::NONE),
        ] {
            apply(&mut session, &mut view, command, &tx).unwrap();
        }
        let bytes = check_tiles(
            &mut session,
            &view,
            &mut cache,
            &mut buffer,
            &mut width,
            &mut height,
        );
        assert!(bytes < 1400 * 800 * 4 / 2);
        for command in [
            Command::Undo,
            Command::Redo,
            Command::Effect(effects::Effect::Grayscale),
            Command::Rotate,
        ] {
            apply(&mut session, &mut view, command, &tx).unwrap();
            check_tiles(
                &mut session,
                &view,
                &mut cache,
                &mut buffer,
                &mut width,
                &mut height,
            );
        }
        for tool in [
            a::Tool::Blur,
            a::Tool::Pixelate,
            a::Tool::Magnify,
            a::Tool::Spotlight,
        ] {
            session.set_tool(tool);
            session.pointer_down(a::PointF::new(490., 100.), a::Modifiers::NONE, None);
            session.pointer_move(a::PointF::new(620., 220.), a::Modifiers::NONE, None);
            session.pointer_up(a::PointF::new(620., 220.), a::Modifiers::NONE);
            check_tiles(
                &mut session,
                &view,
                &mut cache,
                &mut buffer,
                &mut width,
                &mut height,
            );
            session.delete_selection();
            check_tiles(
                &mut session,
                &view,
                &mut cache,
                &mut buffer,
                &mut width,
                &mut height,
            );
        }
        view.zoom = Some(0.5);
        check_tiles(
            &mut session,
            &view,
            &mut cache,
            &mut buffer,
            &mut width,
            &mut height,
        );
    }
    #[test]
    fn raster_batches_bound_large_desktops_without_gaps_or_unaligned_tiles() {
        for (width, height) in [(7680, 4320), (16384, 1024), (513, 16384), (9001, 1013)] {
            let dirty = crate::types::Rect::new(0, 0, width, height);
            let batches = raster_batches(dirty, (width, height), RASTER_BATCH_BYTES);
            assert!(batches.len() > 1);
            let mut area = 0u64;
            for (index, batch) in batches.iter().enumerate() {
                assert!(batch.width as usize * batch.height as usize * 4 <= RASTER_BATCH_BYTES);
                assert_eq!(batch.x as u32 % TILE_SIZE, 0);
                assert_eq!(batch.y as u32 % TILE_SIZE, 0);
                assert!(batch.x as u32 + batch.width <= width);
                assert!(batch.y as u32 + batch.height <= height);
                for other in &batches[..index] { assert!(batch.intersect(*other).is_none()); }
                area += u64::from(batch.width) * u64::from(batch.height);
            }
            assert_eq!(area, u64::from(width) * u64::from(height));
        }
    }
    #[test]
    fn batched_effect_tiles_match_full_reference_across_budgets_zoom_pan_and_edges() {
        let mut base = effects::solid_frame(1400, 900, [0, 0, 0, 255]);
        for y in 0..900 {
            for (x, pixel) in base.row_mut(y).as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *pixel = [(x % 251) as u8, (y % 239) as u8, ((x * 3 + y as usize) % 233) as u8, 255];
            }
        }
        let mut session = a::EditorSession::from_frame(base).unwrap();
        let mut scenes = vec![("base", session.document().clone())];
        // Cross 512px boundaries with overlapping sampling effects and a pen path.
        for tool in [a::Tool::Rectangle, a::Tool::Blur, a::Tool::Pixelate, a::Tool::Magnify, a::Tool::Spotlight] {
            session.set_tool(tool);
            session.pointer_down(a::PointF::new(475.5, 470.25), a::Modifiers::NONE, None);
            session.pointer_move(a::PointF::new(1120.5, 825.75), a::Modifiers::NONE, None);
            session.pointer_up(a::PointF::new(1120.5, 825.75), a::Modifiers::NONE);
            scenes.push((match tool {
                a::Tool::Rectangle => "rectangle", a::Tool::Blur => "blur",
                a::Tool::Pixelate => "pixelate", a::Tool::Magnify => "magnify", _ => "spotlight",
            }, session.document().clone()));
        }
        session.set_tool(a::Tool::Freehand);
        session.pointer_down(a::PointF::new(390., 450.), a::Modifiers::NONE, None);
        for i in 0..40 {
            session.pointer_move(a::PointF::new(400. + i as f32 * 20.,
                if i % 2 == 0 { 440. } else { 780. }), a::Modifiers::NONE, None);
        }
        session.pointer_up(a::PointF::new(1180., 780.), a::Modifiers::NONE);
        scenes.push(("pen", session.document().clone()));
        session.clear_selection();
        session.set_tool(a::Tool::Rectangle);
        session.set_style(|style| {
            style.dash = a::DashStyle::Dash;
            style.opacity = 0.55;
            style.stroke = a::Color::rgba(40, 180, 230, 160);
            style.stroke_width = 8.5;
        });
        session.pointer_down(a::PointF::new(300., 190.), a::Modifiers::NONE, None);
        session.pointer_up(a::PointF::new(980., 610.), a::Modifiers::NONE);
        session.set_kind_props(|kind| {
            if let a::ObjectKind::Rectangle(rectangle) = kind { rectangle.rotation = 0.37; }
        });
        scenes.push(("rotated_translucent_dashed_stroke", session.document().clone()));
        for (scene, document) in scenes {
        let mut session = a::EditorSession::new(document);
        for scale in [0.65, 1., 1.25] {
            session.set_view_scale(scale);
            let width = (1400. * scale) as u32 - 13;
            let height = (900. * scale) as u32 - 19;
            // A non-tile-aligned source offset exercises viewport translation and halos.
            let viewport = crate::types::Rect::new(7, 11, width, height);
            let mut expected = session.render(&a::RenderOptions {
                scale, viewport: Some(viewport), ..Default::default()
            }).into_data();
            for pixel in expected.as_chunks_mut::<4>().0 { pixel.swap(0, 2); }
            for budget in [TILE_SIZE as usize * TILE_SIZE as usize * 4, RASTER_BATCH_BYTES] {
                let dirty = crate::types::Rect::new(0, 0, width, height);
                let tiles = raster_tiles(&mut session, scale, viewport, dirty, budget);
                let mut actual = vec![0; expected.len()];
                for tile in tiles {
                    assert_eq!(tile.rect.x as u32 % TILE_SIZE, 0);
                    assert_eq!(tile.rect.y as u32 % TILE_SIZE, 0);
                    assert_eq!(tile.bgra.len(), tile.rect.width as usize * tile.rect.height as usize * 4);
                    for row in 0..tile.rect.height as usize {
                        let dest = ((tile.rect.y as usize + row) * width as usize + tile.rect.x as usize) * 4;
                        let src = row * tile.rect.width as usize * 4;
                        actual[dest..dest + tile.rect.width as usize * 4]
                            .copy_from_slice(&tile.bgra[src..src + tile.rect.width as usize * 4]);
                    }
                }
                let difference = actual.iter().zip(&expected).position(|(a, b)| a != b);
                if let Some(at) = difference
                    && let Some(directory) = std::env::var_os("RCR_ANNOTATION_DEBUG_DIR")
                {
                    let directory = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    for (name, bytes) in [("actual", &actual), ("expected", &expected)] {
                        let mut rgba = bytes.to_vec();
                        for pixel in rgba.as_chunks_mut::<4>().0 { pixel.swap(0, 2); }
                        let frame = crate::types::Frame::from_rgba8(width, height, rgba).unwrap();
                        std::fs::write(directory.join(format!("{scene}-{scale}-{budget}-{name}.png")),
                            frame.encode(Default::default()).unwrap()).unwrap();
                    }
                    let pixel = at / 4;
                    eprintln!("exact mismatch scene={scene}, dimensions={width}x{height}, viewport={viewport:?}, output_pixel=({},{}), channel={}, actualBGRA={:?}, expectedBGRA={:?}, objects={:?}",
                        pixel % width as usize, pixel / width as usize, at % 4,
                        &actual[pixel * 4..pixel * 4 + 4], &expected[pixel * 4..pixel * 4 + 4],
                        session.document().objects().iter().map(|o| (&o.kind, &o.style)).collect::<Vec<_>>());
                }
                assert_eq!(difference, None,
                    "Batched effect output differs after {scene}, scale {scale}, budget {budget}; actual/expected {:?}",
                    difference.map(|at| (actual[at], expected[at])));
            }
        }
        }
    }
    #[test]
    fn queued_freehand_keeps_intermediate_stroke_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stroke.ssxe");
        let actor = Actor::desktop(effects::solid_frame(400, 200, [255; 4])).unwrap();
        actor.send(Command::Tool(a::Tool::Freehand)).unwrap();
        actor
            .send(Command::Down(a::PointF::new(10., 20.), a::Modifiers::NONE))
            .unwrap();
        for i in 0..100 {
            actor
                .send(Command::Move(
                    a::PointF::new(12. + i as f32 * 3., if i % 2 == 0 { 20. } else { 100. }),
                    a::Modifiers::NONE,
                ))
                .unwrap();
        }
        actor
            .send(Command::Up(a::PointF::new(309., 100.), a::Modifiers::NONE))
            .unwrap();
        actor.send(Command::Project(path.clone())).unwrap();
        assert!(matches!(
            actor
                .events
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap(),
            Event::Saved(_)
        ));
        let project = a::project::load(&path).unwrap();
        let a::ObjectKind::Freehand(stroke) = &project.objects()[0].kind else {
            panic!("Expected freehand stroke")
        };
        assert!(
            stroke.points.len() >= 100,
            "Queued mouse samples were lost: {}",
            stroke.points.len()
        );
        assert!(
            stroke.points.iter().any(|p| p.y == 20.) && stroke.points.iter().any(|p| p.y == 100.)
        );
    }
    #[test]
    fn skipped_preview_frames_merge_tiles_without_losing_initial_upload() {
        let mut session =
            a::EditorSession::from_frame(effects::solid_frame(1100, 600, [10, 30, 50, 255]))
                .unwrap();
        let view = View {
            width: 1100,
            height: 600,
            ..Default::default()
        };
        let mut cache = RasterCache::default();
        let slot = Mutex::new(None);
        publish(&mut session, &view, &slot, &mut cache);
        session.set_tool(a::Tool::Arrow);
        session.pointer_down(a::PointF::new(900., 100.), a::Modifiers::NONE, None);
        session.pointer_move(a::PointF::new(1000., 250.), a::Modifiers::NONE, None);
        session.pointer_up(a::PointF::new(1000., 250.), a::Modifiers::NONE);
        publish(&mut session, &view, &slot, &mut cache);
        session.undo();
        publish(&mut session, &view, &slot, &mut cache);
        let p = slot.into_inner().unwrap().unwrap();
        assert!(p.reset);
        assert_eq!(p.tiles.len(), 6);
        let (mut buffer, mut w, mut h) = (vec![], 0, 0);
        apply_tiles(&mut buffer, &mut w, &mut h, p);
        assert!(
            buffer
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [50, 30, 10, 255])
        );
    }
    #[test]
    fn desktop_preview_preserves_native_pixels_beyond_single_monitor_width() {
        let actor = Actor::desktop(effects::solid_frame(5120, 10, [30, 60, 90, 255])).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(preview) = actor.take_preview() {
                assert_eq!((preview.width, preview.height), (5120, 10));
                assert_eq!(preview.scale, 1.);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[test]
    fn capture_output_is_exact_crop_and_rejects_invalid_bounds() {
        let mut original = effects::solid_frame(80, 50, [30, 60, 90, 255]);
        original.row_mut(15)[20 * 4..21 * 4].copy_from_slice(&[240, 20, 30, 255]);
        let actor = Actor::new(Source::Image(original.clone())).unwrap();
        let rect = crate::types::Rect::new(10, 8, 30, 20);
        actor.send(Command::AcceptRegion(rect)).unwrap();
        let Event::Accepted(frame) = actor
            .events
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
        else {
            panic!("Expected cropped capture")
        };
        assert_eq!(frame.data(), original.crop(rect).unwrap().data());
        assert_eq!((frame.width(), frame.height()), (30, 20));
        actor
            .send(Command::AcceptRegion(crate::types::Rect::new(
                79, 49, 20, 20,
            )))
            .unwrap();
        assert!(matches!(
            actor
                .events
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap(),
            Event::Error(_)
        ));
    }
    #[test]
    fn saved_projects_keep_objects_editable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.ssxe");
        let actor = Actor::new(Source::Image(effects::solid_frame(
            80,
            50,
            [240, 240, 240, 255],
        )))
        .unwrap();
        actor.send(Command::Tool(a::Tool::Arrow)).unwrap();
        actor
            .send(Command::Down(a::PointF::new(5., 5.), a::Modifiers::NONE))
            .unwrap();
        actor
            .send(Command::Up(a::PointF::new(60., 35.), a::Modifiers::NONE))
            .unwrap();
        actor.send(Command::Project(path.clone())).unwrap();
        assert!(matches!(
            actor
                .events
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap(),
            Event::Saved(_)
        ));
        let project = a::project::load(&path).unwrap();
        assert_eq!(project.objects().len(), 1);
        assert_eq!(project.canvas_size(), (80, 50));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn worker_annotations_are_editable_and_accept_preserves_resolution() {
        let actor = Actor::new(Source::Image(effects::solid_frame(
            120,
            80,
            [255, 255, 255, 255],
        )))
        .unwrap();
        actor.send(Command::Tool(a::Tool::Rectangle)).unwrap();
        actor
            .send(Command::Down(a::PointF::new(10., 10.), a::Modifiers::NONE))
            .unwrap();
        actor
            .send(Command::Move(a::PointF::new(90., 60.), a::Modifiers::NONE))
            .unwrap();
        actor
            .send(Command::Up(a::PointF::new(90., 60.), a::Modifiers::NONE))
            .unwrap();
        actor.send(Command::Undo).unwrap();
        actor.send(Command::Redo).unwrap();
        actor.send(Command::Accept).unwrap();
        let event = actor
            .events
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        let Event::Accepted(frame) = event else {
            panic!("Expected accepted image")
        };
        assert_eq!((frame.width(), frame.height()), (120, 80));
        assert_ne!(frame, effects::solid_frame(120, 80, [255, 255, 255, 255]));
    }

    #[test]
    fn ui_waker_flushes_ready_preview_and_delivers_final_edits_events_and_errors() {
        use std::time::{Duration, Instant};
        let actor = Actor::desktop(effects::solid_frame(100, 80, [255; 4])).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while actor.preview.lock().unwrap().is_none() {
            assert!(Instant::now() < deadline, "Initial preview was not published");
            thread::sleep(Duration::from_millis(1));
        }
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let queue = Arc::downgrade(&actor.queue);
        let preview = actor.preview.clone();
        let (attached_tx, attached_rx) = mpsc::channel();
        thread::spawn(move || {
            actor.set_waker(move || {
                // Reenter both locks: registration/publication must release them
                // before calling UI code, including an already-ready flush.
                if let Some(queue) = queue.upgrade() {
                    drop(queue.ui_waker.lock().unwrap());
                }
                drop(preview.lock().unwrap());
                let _ = wake_tx.try_send(());
            });
            let _ = attached_tx.send(actor);
        });
        let actor = attached_rx.recv_timeout(Duration::from_secs(5)).expect("Waker registration deadlocked");
        wake_rx.recv_timeout(Duration::from_secs(5)).expect("Already-ready preview was not flushed");
        assert!(actor.take_preview().is_some());
        while wake_rx.try_recv().is_ok() {}

        actor.send(Command::Tool(a::Tool::Rectangle)).unwrap();
        actor.send(Command::Down(a::PointF::new(10., 10.), a::Modifiers::NONE)).unwrap();
        actor.send(Command::Up(a::PointF::new(70., 50.), a::Modifiers::NONE)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            wake_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("Final edit produced no UI wake");
            if actor.take_preview().is_some_and(|preview| preview.objects == 1 && preview.can_undo) { break; }
        }

        actor.send(Command::Accept).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            wake_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("Accepted frame produced no UI wake");
            if let Ok(Event::Accepted(frame)) = actor.events.try_recv() {
                assert_eq!((frame.width(), frame.height()), (100, 80)); break;
            }
        }
        let directory = tempfile::tempdir().unwrap();
        actor.send(Command::InsertImage(directory.path().join("missing.png"))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            wake_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("Worker error produced no UI wake");
            if matches!(actor.events.try_recv(), Ok(Event::Error(_))) { break; }
        }
        let queue = actor.queue.clone();
        let (closed_tx, closed_rx) = mpsc::channel();
        thread::spawn(move || { drop(actor); let _ = closed_tx.send(()); });
        closed_rx.recv_timeout(Duration::from_secs(5)).expect("Closing actor with a UI waker deadlocked");
        assert!(queue.closed.load(Ordering::Acquire));
        assert!(queue.ui_waker.lock().unwrap().is_none(), "Closed actor retained its UI consumer");
    }
}
