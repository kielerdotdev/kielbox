//! GPUI annotation canvas backed by SSX's UI-independent editing session.
use crate::{text_input::TextInput, theme::*};
use gpui::{prelude::*, *};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use rcr_toolkit::{
    annotation as a,
    image_editor::{Actor, CanvasAction, CanvasClipboardOperation, CanvasClipboardReply, Command, Event, Preview, Source, canvas_clipboard_key},
    types::Frame,
    worker::{CompletionSender, StandaloneCompletion},
};
use std::{
    cell::Cell,
    collections::VecDeque,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, mpsc, atomic::{AtomicBool, Ordering}},
};

pub enum ImageEvent {
    Ready,
    Finished,
    Cancel,
}
#[derive(Default)]
struct ClipboardLifetime {
    generation: u64,
    busy: Arc<AtomicBool>,
    render: Option<ClipboardRequest>,
}
struct ClipboardRequest { generation: u64, _lease: ClipboardLease }
struct ClipboardLease(Arc<AtomicBool>);
#[derive(Default)]
struct PropertySmoke { stage: u8, amount: f32, compact_tools: bool, baseline: Option<[u8; 32]>, text_a: Option<a::ObjectId>, stale_rejected: bool }
#[derive(Clone)]
struct LabelDraft {
    target: Option<a::ObjectId>,
    tool: a::Tool,
    baseline: String,
    value: String,
    confirmed_revision: u64,
    pending: Option<(u64, String)>,
    conflict: bool,
}
impl LabelDraft {
    fn unresolved(&self, value: &str, observed_revision: u64) -> bool {
        self.pending.is_some() || self.conflict || value != self.baseline
            || self.confirmed_revision > observed_revision
    }
}
fn retained_draft_payload(drafts: &[LabelDraft], target: Option<a::ObjectId>, tool: a::Tool, expected_session: u64, current_session: u64) -> Option<String> {
    (expected_session == current_session).then(|| drafts.iter().find(|draft| draft.target == target && draft.tool == tool))
        .flatten().map(|draft| draft.value.clone())
}
fn canvas_clipboard_request_current(requests: &VecDeque<(u64, u64, u64, bool)>, request: u64, session: u64, open: bool) -> bool {
    open && requests.iter().any(|(pending, owner_session, _, clipboard)| *pending == request && *owner_session == session && *clipboard)
}
struct TextClipboardJob {
    owner: Option<isize>,
    operation: CanvasClipboardOperation,
    reply: CanvasClipboardReply,
    lease: ClipboardLease,
    wake: async_channel::Sender<()>,
}
struct TextClipboardWorker { sender: mpsc::SyncSender<TextClipboardJob> }
impl TextClipboardWorker {
    fn start(mut io: impl FnMut(Option<isize>, CanvasClipboardOperation) -> Result<Option<String>, String> + Send + 'static) -> Result<Self, String> {
        let (sender, jobs) = mpsc::sync_channel::<TextClipboardJob>(1);
        std::thread::Builder::new().name("rcr-text-clipboard".into()).spawn(move || {
            for job in jobs {
                let TextClipboardJob { owner, operation, reply, lease, wake } = job;
                // Claim on the worker, before OS access. A queued expired reply
                // never touches the clipboard. A running OS call retains its lease
                // even if the Actor times out, closes, or starts a new session.
                reply.respond(|| {
                    let result = io(owner, operation);
                    drop(lease);
                    result
                });
                let _ = wake.try_send(());
            }
        }).map_err(|error| format!("Couldn't start text clipboard worker: {error}"))?;
        Ok(Self { sender })
    }
    fn submit(&self, job: TextClipboardJob) -> Result<(), TextClipboardJob> {
        self.sender.try_send(job).map_err(|error| match error {
            mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job) => job,
        })
    }
}
fn queue_canvas_clipboard(owner: Option<isize>, operation: CanvasClipboardOperation, reply: CanvasClipboardReply,
    busy: Arc<AtomicBool>, wake: async_channel::Sender<()>) {
    if busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        reply.respond(|| Err("Wait for the current clipboard operation to finish, then retry the text shortcut.".into()));
        return;
    }
    let lease = ClipboardLease(busy);
    static WORKER: std::sync::OnceLock<Result<TextClipboardWorker, String>> = std::sync::OnceLock::new();
    match WORKER.get_or_init(|| TextClipboardWorker::start(canvas_system_clipboard)) {
        Ok(worker) => {
            if let Err(job) = worker.submit(TextClipboardJob { owner, operation, reply, lease, wake }) {
                job.reply.respond(|| Err("Text clipboard worker is busy or stopped; retry the shortcut.".into()));
            }
        }
        Err(error) => reply.respond(|| Err(error.clone())),
    }
}
/// One immediate text-only attempt. Never retries or copies image/file formats.
fn canvas_system_clipboard(owner: Option<isize>, operation: CanvasClipboardOperation) -> Result<Option<String>, String> {
    use windows::Win32::{
        Foundation::{HANDLE, HGLOBAL, HWND, GlobalFree},
        System::{DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData},
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock}},
    };
    const UNICODE_TEXT: u32 = 13; // CF_UNICODETEXT
    const LIMIT: usize = rcr_toolkit::image_editor::CANVAS_CLIPBOARD_LIMIT;
    struct Open;
    impl Drop for Open { fn drop(&mut self) { let _ = unsafe { CloseClipboard() }; } }
    struct Memory(Option<HGLOBAL>);
    impl Drop for Memory { fn drop(&mut self) { if let Some(handle) = self.0.take() { let _ = unsafe { GlobalFree(Some(handle)) }; } } }
    struct Locked(HGLOBAL);
    impl Drop for Locked { fn drop(&mut self) { let _ = unsafe { GlobalUnlock(self.0) }; } }
    let owner = owner.filter(|owner| *owner != 0).ok_or("The annotation window is no longer available")?;
    let owner = HWND(owner as *mut std::ffi::c_void);
    // Allocation/lock failure happens before emptying the clipboard. Every
    // failure preserves the annotation selection; a later transfer failure may
    // already have cleared the previous clipboard contents.
    let mut storage = if let CanvasClipboardOperation::Write(text) = &operation {
        if text.len() > LIMIT || text.contains('\0') { return Err("Clipboard text is too large or contains a null character".into()); }
        let wide = text.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) }.map_err(|error| format!("Allocate clipboard text: {error}"))?;
        let memory = Memory(Some(memory));
        let pointer = unsafe { GlobalLock(memory.0.unwrap()) };
        if pointer.is_null() { return Err("Couldn't lock clipboard text memory".into()); }
        let locked = Locked(memory.0.unwrap());
        unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), pointer.cast::<u16>(), wide.len()); }
        drop(locked);
        memory
    } else { Memory(None) };
    unsafe { OpenClipboard(Some(owner)) }.map_err(|error| format!("Clipboard is busy; retry the shortcut: {error}"))?;
    let open = Open;
    let result = match operation {
        CanvasClipboardOperation::Write(_) => {
            unsafe { EmptyClipboard() }.map_err(|error| format!("Clear clipboard: {error}"))?;
            let memory = storage.0.unwrap();
            unsafe { SetClipboardData(UNICODE_TEXT, Some(HANDLE(memory.0))) }.map_err(|error| format!("Write clipboard text: {error}"))?;
            storage.0 = None; // Windows owns the allocation after successful transfer.
            Ok(None)
        }
        CanvasClipboardOperation::Read => {
            if unsafe { IsClipboardFormatAvailable(UNICODE_TEXT) }.is_err() { Ok(None) }
            else {
                let memory = HGLOBAL(unsafe { GetClipboardData(UNICODE_TEXT) }.map_err(|error| format!("Read clipboard text: {error}"))?.0);
                let bytes = unsafe { GlobalSize(memory) };
                if bytes < 2 || !bytes.is_multiple_of(2) || bytes > (LIMIT + 1) * 2 { return Err("Clipboard text exceeds the 1 MiB limit or has an invalid size".into()); }
                let pointer = unsafe { GlobalLock(memory) };
                if pointer.is_null() { return Err("Couldn't lock clipboard text".into()); }
                let locked = Locked(memory);
                let wide = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), bytes / 2) };
                let end = wide.iter().position(|unit| *unit == 0).ok_or("Clipboard text is missing its terminator")?;
                let text = String::from_utf16(&wide[..end]).map_err(|error| format!("Invalid clipboard text: {error}"))?;
                drop(locked);
                if text.len() > LIMIT { return Err("Clipboard text exceeds the 1 MiB limit".into()); }
                Ok(Some(text))
            }
        }
    };
    // Close failure is conservative: Cut must retain its selection on any error.
    let closed = unsafe { CloseClipboard() }.map_err(|error| format!("Close clipboard: {error}"));
    std::mem::forget(open);
    closed?;
    result
}
fn single_line_label(text: &str) -> bool { !text.contains(['\r', '\n', '\u{2028}', '\u{2029}']) }
impl Drop for ClipboardLease {
    fn drop(&mut self) { self.0.store(false, Ordering::Release); }
}
impl ClipboardLifetime {
    fn begin(&mut self) -> bool {
        if self.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() { return false; }
        self.generation = self.generation.wrapping_add(1);
        self.render = Some(ClipboardRequest { generation: self.generation, _lease: ClipboardLease(self.busy.clone()) });
        true
    }
    fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        // A request still being rendered can be abandoned with its old Actor.
        // A lease already moved to a clipboard worker survives closing/reopening.
        self.render.take();
    }
    fn is_current(&self, generation: u64) -> bool { self.generation == generation }
}
pub struct ImageEditor {
    shell_operation: u64,
    focus: FocusHandle,
    actor: Option<Actor>,
    ui_waker: async_channel::Sender<()>,
    preview: Option<Preview>,
    image: crate::annotation_surface::Surface,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    area: Rc<Cell<Option<Bounds<Pixels>>>>,
    pan: Option<Point<Pixels>>,
    viewport: (u32, u32),
    tool: a::Tool,
    label: Entity<TextInput>,
    fill: bool,
    zoom: Option<f32>,
    error: Option<String>,
    notice: Option<String>,
    dragging: bool,
    project_reply: Option<PathBuf>,
    focus_requested: bool,
    clipboard: ClipboardLifetime,
    accepted: Option<Frame>,
    finishing: bool,
    completion: Option<mpsc::Receiver<StandaloneCompletion>>,
    standalone: bool,
    saved_path: Option<PathBuf>,
    finish_focus: FocusHandle,
    toolbar_focus: FocusHandle,
    tools_scroll: ScrollHandle,
    help_visible: bool,
    canvas_request: u64,
    canvas_requests: VecDeque<(u64, u64, u64, bool)>,
    canvas_interaction: u64,
    clipboard_window: Option<isize>,
    canvas_revision: u64,
    amount_plus_focus: FocusHandle,
    font_plus_focus: FocusHandle,
    label_apply_focus: FocusHandle,
    label_reload_focus: FocusHandle,
    draft_review_focus: FocusHandle,
    session_generation: u64,
    property_smoke: Option<PropertySmoke>,
    label_draft: Option<LabelDraft>,
    label_request: u64,
    retained_labels: Vec<LabelDraft>,
}
impl EventEmitter<ImageEvent> for ImageEditor {}
pub fn render_image(width: u32, height: u32, bgra: Vec<u8>) -> Option<Arc<RenderImage>> {
    image::RgbaImage::from_raw(width, height, bgra)
        .map(|b| Arc::new(RenderImage::new(vec![image::Frame::new(b)])))
}
pub fn frame_image(frame: Frame) -> Option<Arc<RenderImage>> {
    let frame = frame.into_rgba8().ok()?;
    let (width, height) = (frame.width(), frame.height());
    let mut pixels = frame.into_data();
    for p in pixels.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    render_image(width, height, pixels)
}
fn modifiers(mods: gpui::Modifiers) -> a::Modifiers {
    a::Modifiers {
        shift: mods.shift,
        ctrl: mods.control || mods.platform,
        alt: mods.alt,
    }
}
impl ImageEditor {
    fn reveal_saved_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let session = self.session_generation;
        let previous_error = self.error.clone();
        match crate::recordings::begin_shell_action(crate::recordings::ShellAction::Reveal, path, cx,
            move |this, operation, path, result, cx| {
                if this.shell_operation != operation { return; }
                this.shell_operation = 0;
                if this.session_generation == session && this.saved_path.as_deref() == Some(path)
                    && this.error == previous_error
                    && let Err(error) = result { this.error = Some(error); }
                cx.notify();
            }) {
            Ok(operation) => self.shell_operation = operation,
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
    pub fn new(cx: &mut Context<Self>) -> Self {
        let label = cx.new(|cx| TextInput::new(cx, "Text"));
        cx.observe(&label, |_, _, cx| cx.notify()).detach();
        let (ui_waker, notifications) = async_channel::bounded(1);
        cx.spawn(async move |view, cx| {
            while notifications.recv().await.is_ok() {
                if view
                    .update(cx, |this: &mut Self, cx| { this.poll(cx); cx.notify(); })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            shell_operation: 0,
            focus: cx.focus_handle(),
            actor: None,
            ui_waker,
            preview: None,
            image: Default::default(),
            bounds: Rc::new(Cell::new(None)),
            area: Rc::new(Cell::new(None)),
            pan: None,
            viewport: (0, 0),
            tool: a::Tool::Select,
            label,
            fill: false,
            zoom: None,
            error: None,
            notice: None,
            dragging: false,
            project_reply: None,
            focus_requested: false,
            clipboard: ClipboardLifetime::default(),
            accepted: None,
            finishing: false,
            completion: None,
            standalone: false,
            saved_path: None,
            finish_focus: cx.focus_handle().tab_stop(true),
            toolbar_focus: cx.focus_handle().tab_stop(true),
            tools_scroll: ScrollHandle::new(),
            help_visible: false,
            canvas_request: 0,
            canvas_requests: VecDeque::new(),
            canvas_interaction: 0,
            clipboard_window: None,
            canvas_revision: 0,
            amount_plus_focus: cx.focus_handle().tab_stop(true),
            font_plus_focus: cx.focus_handle().tab_stop(true),
            label_apply_focus: cx.focus_handle().tab_stop(true),
            label_reload_focus: cx.focus_handle().tab_stop(true),
            draft_review_focus: cx.focus_handle().tab_stop(true),
            session_generation: 0,
            property_smoke: None,
            label_draft: None,
            label_request: 0,
            retained_labels: Vec::new(),
        }
    }
    pub fn open(&mut self, source: Source, cx: &mut Context<Self>) {
        self.close(cx);
        self.actor = match Actor::new(source) {
            Ok(actor) => {
                let wake = self.ui_waker.clone();
                actor.set_waker(move || {
                    let _ = wake.try_send(());
                });
                Some(actor)
            }
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        };
        self.focus_requested = true;
        self.tool = a::Tool::Select;
        self.zoom = None;
        self.viewport = (0, 0);
        cx.notify();
    }
    pub fn close(&mut self, cx: &mut Context<Self>) {
        // Open calls close too: prompt results belong to the Actor that was
        // active when the prompt opened, even if another image opens meanwhile.
        self.session_generation = self.session_generation.wrapping_add(1);
        self.clipboard.reset();
        self.actor.take();
        self.preview = None;
        self.image.clear_deferred(cx);
        self.error = None;
        self.notice = None;
        self.dragging = false;
        self.accepted = None;
        self.finishing = false;
        self.completion = None;
        self.standalone = false;
        self.saved_path = None;
        self.property_smoke = None;
        self.label_draft = None;
        self.retained_labels.clear();
        self.help_visible = false;
        self.canvas_requests.clear();
        self.clipboard_window = None;
        self.canvas_interaction = 0;
        self.canvas_revision = 0;
        self.tools_scroll.set_offset(point(px(0.), px(0.)));
    }
    pub fn open_standalone(&mut self, source: Source, cx: &mut Context<Self>) {
        self.open(source, cx);
        self.standalone = true;
    }
    pub fn smoke_open_standalone(&mut self, source: Source, cx: &mut Context<Self>) -> anyhow::Result<()> {
        self.open_standalone(Source::Image(rcr_toolkit::effects::solid_frame(16, 9, [20, 30, 40, 255])), cx);
        let previous = self.session_generation;
        self.close(cx);
        anyhow::ensure!(!self.complete_dialog(previous, Command::Project(PathBuf::new()), cx),
            "A project dialog result was accepted after its image closed");
        self.open_standalone(source, cx);
        // Invalid paths deliberately require no filesystem output even if a
        // regression accidentally admits one of these simulated prompt results.
        anyhow::ensure!(!self.complete_dialog(previous, Command::Project(PathBuf::new()), cx),
            "A stale project dialog targeted the reopened image");
        anyhow::ensure!(!self.complete_dialog(previous, Command::InsertImage(PathBuf::new()), cx),
            "A stale insertion dialog targeted the reopened image");
        anyhow::ensure!(self.actor.is_some() && self.error.is_none(),
            "Stale dialog handling damaged the reopened image session");
        Ok(())
    }
    /// Synthetic own-window visual fixture: no save or clipboard service runs.
    pub fn seed_retry_preview(&mut self, cx: &mut Context<Self>) {
        self.open_standalone(Source::Image(rcr_toolkit::effects::solid_frame(960, 540, [225, 234, 240, 255])), cx);
        self.send(Command::Tool(a::Tool::Rectangle), cx);
        self.send(Command::Down(a::PointF::new(160., 120.), Default::default()), cx);
        self.send(Command::Move(a::PointF::new(760., 420.), Default::default()), cx);
        self.send(Command::Up(a::PointF::new(760., 420.), Default::default()), cx);
        self.saved_path = Some(PathBuf::from("C:/Screenshots/Screenshot_2026-10-24_14-23-05.png"));
        self.finish_failed("Couldn't copy the saved image: another application kept the clipboard busy. Your annotated image is saved; retry will copy it without saving another file. Synthetic preview only; no file was written.".into(), cx);
    }
    pub fn retry_preview_ready(&self) -> bool {
        self.image.ready() && self.preview.as_ref().is_some_and(|p| p.objects == 1)
    }
    /// Owned-window visual fixture only. Select an effect over generated pixels;
    /// no Accept command, filesystem, clipboard, or preference service runs.
    pub fn seed_property_preview(&mut self, tool: a::Tool, cx: &mut Context<Self>) -> anyhow::Result<()> {
        anyhow::ensure!(matches!(tool, a::Tool::Blur | a::Tool::Pixelate | a::Tool::Magnify),
            "Property preview supports Blur, Pixelate, or Magnify");
        let mut pixels = Vec::with_capacity(320 * 180 * 4);
        for y in 0..180 { for x in 0..320 {
            pixels.extend_from_slice(if (x / 24 + y / 24) % 2 == 0 { &[15, 50, 90, 255] } else { &[240, 215, 165, 255] });
        } }
        self.open_standalone(Source::Image(Frame::from_rgba8(320, 180, pixels)?), cx);
        self.tool = tool;
        self.send(Command::Tool(tool), cx);
        self.send(Command::Down(a::PointF::new(30., 25.), Default::default()), cx);
        self.send(Command::Move(a::PointF::new(280., 150.), Default::default()), cx);
        self.send(Command::Up(a::PointF::new(280., 150.), Default::default()), cx);
        Ok(())
    }
    pub fn property_preview_ready(&self, tool: a::Tool) -> bool {
        self.image.ready() && !self.finishing && self.accepted.is_none()
            && self.preview.as_ref().is_some_and(|preview| preview.objects == 1
                && preview.properties.as_ref().is_some_and(|p| p.tool == tool && p.selected == 1))
    }
    pub fn take_accepted(&mut self) -> Option<Frame> { self.accepted.take() }
    pub fn completion_channel(&mut self, cx: &mut Context<Self>) -> CompletionSender<StandaloneCompletion> {
        let (sender, completion) = mpsc::sync_channel(1);
        self.completion = Some(completion);
        cx.notify();
        let wake = self.ui_waker.clone();
        CompletionSender::new(sender, move || { let _ = wake.try_send(()); })
    }
    pub fn finish_failed(&mut self, detail: String, cx: &mut Context<Self>) {
        self.finishing = false;
        self.completion = None;
        self.error = Some(detail.chars().take(4_000).collect());
        cx.notify();
    }
    pub fn smoke_annotate(view: Entity<Self>, window: &mut Window, cx: &mut App) -> anyhow::Result<bool> {
        if !Self::smoke_context_properties(view.clone(), window, cx)? { return Ok(false); }
        window.draw(cx).clear();
        // Preview dimensions are the fitted image, not the available canvas
        // dimensions: the synthetic 320x180 source must remain at native size.
        if !view.read(cx).image.ready() || view.read(cx).bounds.get().is_none() ||
            !view.read(cx).preview.as_ref().is_some_and(|p| (p.width, p.height) == (320, 180)) { return Ok(false); }
        let bounds = view.read(cx).bounds.get().unwrap();
        view.update(cx, |this, cx| { this.tool = a::Tool::Rectangle; this.send(Command::Tool(a::Tool::Rectangle), cx); });
        let from = bounds.origin + point(bounds.size.width * 0.2, bounds.size.height * 0.25);
        let to = bounds.origin + point(bounds.size.width * 0.7, bounds.size.height * 0.75);
        window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left,
            position: from, modifiers: Default::default(), click_count: 1, first_mouse: false }), cx);
        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: to,
            pressed_button: Some(MouseButton::Left), modifiers: Default::default() }), cx);
        window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left,
            position: to, modifiers: Default::default(), click_count: 1 }), cx);
        Ok(true)
    }
    fn smoke_context_properties(view: Entity<Self>, window: &mut Window, cx: &mut App) -> anyhow::Result<bool> {
        use anyhow::{Context as _, ensure};
        fn key(window: &mut Window, cx: &mut App, key: &str) {
            let key = Keystroke::parse(key).unwrap();
            window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent { keystroke: key.clone(), is_held: false }), cx);
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke: key }), cx);
            window.draw(cx).clear();
        }
        fn text(window: &mut Window, cx: &mut App, text: &str) {
            for ch in text.chars() { window.dispatch_keystroke(Keystroke { key: ch.to_string(), key_char: Some(ch.to_string()), modifiers: Default::default() }, cx); }
        }
        fn click(view: &Entity<ImageEditor>, window: &mut Window, cx: &mut App, x: f32, y: f32) {
            let b = view.read(cx).bounds.get().unwrap();
            let p = b.origin + point(b.size.width * (x / 320.), b.size.height * (y / 180.));
            window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: p, modifiers: Default::default(), click_count: 1, first_mouse: false }), cx);
            window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: p, modifiers: Default::default(), click_count: 1 }), cx);
        }
        window.draw(cx).clear();
        let stage = view.read(cx).property_smoke.as_ref().map_or(0, |proof| proof.stage);
        ensure!(view.read(cx).error.is_none() || (matches!(stage, 41 | 43)
            && view.read(cx).error.as_deref().is_some_and(|error| error.contains("Apply or reload"))),
            "Contextual property proof: {:?}", view.read(cx).error);
        match stage {
            0 => {
                let mut pixels = Vec::with_capacity(320 * 180 * 4);
                for y in 0..180 { for x in 0..320 {
                    pixels.extend_from_slice(if (x / 24 + y / 24) % 2 == 0 { &[15, 50, 90, 255] } else { &[240, 215, 165, 255] });
                } }
                let frame = Frame::from_rgba8(320, 180, pixels)?;
                view.update(cx, |this, cx| {
                    this.open(Source::Image(frame), cx);
                    this.property_smoke = Some(PropertySmoke { stage: 1, ..Default::default() });
                    this.tool = a::Tool::Blur;
                    this.send(Command::Tool(a::Tool::Blur), cx);
                    this.send(Command::Down(a::PointF::new(30., 25.), Default::default()), cx);
                    this.send(Command::Move(a::PointF::new(280., 150.), Default::default()), cx);
                    this.send(Command::Up(a::PointF::new(280., 150.), Default::default()), cx);
                });
            }
            1 => {
                let Some(p) = view.read(cx).preview.as_ref().and_then(|p| p.properties.as_ref()).filter(|p| p.tool == a::Tool::Blur && p.selected == 1).cloned() else { return Ok(false); };
                view.update(cx, |this, _| -> anyhow::Result<()> {
                    let proof = this.property_smoke.as_mut().unwrap(); proof.amount = p.amount.unwrap(); proof.stage = 2;
                    this.actor.as_ref().unwrap().send(Command::Accept)
                })?;
            }
            3 => {
                view.read(cx).amount_plus_focus.focus(window);
                window.draw(cx).clear();
                let key = Keystroke::parse("enter").unwrap();
                window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent { keystroke: key.clone(), is_held: false }), cx);
                window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke: key }), cx);
                ensure!(!view.read(cx).finishing, "Property Enter also finished the workflow");
                view.update(cx, |this, _| this.property_smoke.as_mut().unwrap().stage = 4);
            }
            4 => {
                let expected = view.read(cx).property_smoke.as_ref().unwrap().amount + 1.;
                if !view.read(cx).preview.as_ref().and_then(|p| p.properties.as_ref()).is_some_and(|p| p.amount == Some(expected)) { return Ok(false); }
                view.update(cx, |this, _| -> anyhow::Result<()> {
                    this.property_smoke.as_mut().unwrap().stage = 5;
                    this.actor.as_ref().unwrap().send(Command::Accept)
                })?;
            }
            6 => {
                view.update(cx, |this, cx| { this.property_smoke.as_mut().unwrap().stage = 7; this.send(Command::Undo, cx); });
            }
            7 => {
                let expected = view.read(cx).property_smoke.as_ref().unwrap().amount;
                if !view.read(cx).preview.as_ref().and_then(|p| p.properties.as_ref()).is_some_and(|p| p.amount == Some(expected)) { return Ok(false); }
                view.update(cx, |this, _| -> anyhow::Result<()> {
                    this.property_smoke.as_mut().unwrap().stage = 8;
                    this.actor.as_ref().unwrap().send(Command::Accept)
                })?;
            }
            9 => {
                let mut session = a::EditorSession::from_frame(rcr_toolkit::effects::solid_frame(320, 180, [235, 240, 245, 255]))?;
                let text_a = session.add_object(a::Object::new(a::ObjectId(0), Default::default(), a::ObjectKind::Text(a::object::TextShape {
                    rect: a::RectF::new(20., 20., 130., 40.), content: a::object::TextContent { text: "Alpha".into(), ..Default::default() }, ..Default::default() })));
                session.add_object(a::Object::new(a::ObjectId(0), Default::default(), a::ObjectKind::Balloon(a::object::BalloonShape {
                    rect: a::RectF::new(170., 80., 130., 70.), tail: a::PointF::new(160., 160.),
                    content: a::object::TextContent { text: "Bravo".into(), ..Default::default() }, ..Default::default() })));
                let document = session.document().clone();
                view.update(cx, |this, cx| {
                    this.open(Source::Project(document), cx);
                    this.property_smoke = Some(PropertySmoke { stage: 10, text_a: Some(text_a), ..Default::default() });
                });
            }
            10 => {
                if !view.read(cx).image.ready() || !view.read(cx).preview.as_ref().is_some_and(|p| p.objects == 2) { return Ok(false); }
                click(&view, window, cx, 35., 35.);
                view.update(cx, |this, _| this.property_smoke.as_mut().unwrap().stage = 11);
            }
            11 => {
                if !view.read(cx).preview.as_ref().and_then(|p| p.properties.as_ref()).is_some_and(|p| p.text.as_deref() == Some("Alpha")) { return Ok(false); }
                ensure!(view.read(cx).label.read(cx).value() == "Alpha", "Selected text did not populate its input");
                view.read(cx).label.read(cx).focus_handle(cx).focus(window);
                key(window, cx, "ctrl-a"); text(window, cx, "Draft A"); key(window, cx, "left");
                window.draw(cx).clear();
                view.read(cx).font_plus_focus.focus(window); key(window, cx, "enter");
                view.read(cx).label.read(cx).focus_handle(cx).focus(window);
                view.update(cx, |this, _| this.property_smoke.as_mut().unwrap().stage = 12);
            }
            12 => {
                if !view.read(cx).preview.as_ref().and_then(|p| p.properties.as_ref()).is_some_and(|p| p.font == Some(26.)) { return Ok(false); }
                text(window, cx, "X");
                ensure!(view.read(cx).label.read(cx).value() == "Draft XA", "Same-target preview reset typed draft or caret");
                view.update(cx, |this, cx| {
                    this.send(Command::Copy, cx);
                    this.send(Command::Accept, cx);
                });
                ensure!(view.read(cx).error.as_deref().is_some_and(|error| error.contains("Apply or reload")), "Dirty draft did not explain blocked output");
                ensure!(!view.read(cx).clipboard.busy.load(Ordering::Acquire) && !view.read(cx).finishing && view.read(cx).accepted.is_none(),
                    "Dirty draft admitted clipboard or completion work");
                view.update(cx, |this, cx| { this.error = None; this.property_smoke.as_mut().unwrap().stage = 30; cx.notify(); });
                click(&view, window, cx, 200., 100.);
            }
            30 => {
                if !view.read(cx).preview.as_ref().and_then(|p| p.properties.as_ref()).is_some_and(|p| p.text.as_deref() == Some("Bravo")) { return Ok(false); }
                ensure!(view.read(cx).retained_labels.iter().any(|draft| draft.value == "Draft XA"), "Switching A to B lost A's unapplied draft");
                let editor = view.read(cx);
                ensure!(retained_draft_payload(&editor.retained_labels, editor.property_smoke.as_ref().unwrap().text_a, a::Tool::Text,
                    editor.session_generation, editor.session_generation).as_deref() == Some("Draft XA"), "Retained draft recovery payload lost user text");
                view.update(cx, |this, cx| { this.send(Command::Copy, cx); this.send(Command::Accept, cx); });
                ensure!(!view.read(cx).clipboard.busy.load(Ordering::Acquire) && !view.read(cx).finishing
                    && view.read(cx).error.is_some(), "Hidden retained draft did not block output");
                view.update(cx, |this, _| { this.error = None; this.property_smoke.as_mut().unwrap().stage = 31; });
                window.draw(cx).clear();
                view.read(cx).draft_review_focus.focus(window); key(window, cx, "enter");
            }
            31 => {
                if !view.read(cx).preview.as_ref().and_then(|p| p.properties.as_ref()).is_some_and(|p| p.text.as_deref() == Some("Alpha")) { return Ok(false); }
                ensure!(view.read(cx).label.read(cx).value() == "Draft XA", "Returning B to A lost A's retained draft");
                window.draw(cx).clear();
                view.read(cx).label_reload_focus.focus(window); key(window, cx, "enter");
                ensure!(view.read(cx).label.read(cx).value() == "Alpha" && view.read(cx).error.is_none()
                    && view.read(cx).label_draft.as_ref().is_some_and(|draft| draft.baseline == "Alpha" && draft.pending.is_none() && !draft.conflict),
                    "Native Reload did not restore the selected text baseline safely");
                ensure!(!view.read(cx).has_unresolved_labels(cx), "Reload left a hidden duplicate draft");
                click(&view, window, cx, 200., 100.);
                view.update(cx, |this, _| this.property_smoke.as_mut().unwrap().stage = 13);
            }
            13 => {
                if !view.read(cx).preview.as_ref().and_then(|p| p.properties.as_ref()).is_some_and(|p| p.text.as_deref() == Some("Bravo")) { return Ok(false); }
                ensure!(view.read(cx).label.read(cx).value() == "Bravo", "Switching to Balloon retained another object's draft");
                view.update(cx, |this, _| -> anyhow::Result<()> { this.property_smoke.as_mut().unwrap().stage = 14; this.actor.as_ref().unwrap().send(Command::Accept) })?;
            }
            15 => {
                view.update(cx, |this, _| -> anyhow::Result<()> {
                    let proof = this.property_smoke.as_mut().unwrap(); proof.stage = 16;
                    let actor = this.actor.as_ref().unwrap();
                    actor.send(Command::ApplyLabel { request: u64::MAX, target: proof.text_a, tool: a::Tool::Text, expected: "Alpha".into(), text: "Wrong target".into() })?;
                    actor.send(Command::Accept)
                })?;
            }
            17 => {
                ensure!(view.read(cx).property_smoke.as_ref().unwrap().stale_rejected, "Stale text target was not explicitly rejected");
                ensure!(view.read(cx).label.read(cx).value() == "Bravo", "Stale ACK changed the current input");
                view.read(cx).label.read(cx).focus_handle(cx).focus(window);
                key(window, cx, "ctrl-a"); text(window, cx, "Updated Bravo");
                view.read(cx).label_apply_focus.focus(window); key(window, cx, "enter");
                ensure!(view.read(cx).label_draft.as_ref().is_some_and(|draft| draft.pending.is_some()), "Native Apply did not queue a guarded request");
                view.read(cx).label.read(cx).focus_handle(cx).focus(window); text(window, cx, "X");
                view.update(cx, |this, _| this.property_smoke.as_mut().unwrap().stage = 18);
            }
            18 => {
                if !view.read(cx).label_draft.as_ref().is_some_and(|draft| draft.pending.is_none() && draft.baseline == "Updated Bravo") { return Ok(false); }
                ensure!(view.read(cx).label.read(cx).value() == "Updated BravoX", "Apply ACK erased newer typing/caret");
                view.update(cx, |this, _| -> anyhow::Result<()> { this.property_smoke.as_mut().unwrap().stage = 19; this.actor.as_ref().unwrap().send(Command::Accept) })?;
            }
            20 => { view.update(cx, |this, cx| { this.property_smoke.as_mut().unwrap().stage = 21; this.send(Command::Undo, cx); }); }
            21 => {
                if !view.read(cx).preview.as_ref().and_then(|p| p.properties.as_ref()).is_some_and(|p| p.text.as_deref() == Some("Bravo")) { return Ok(false); }
                ensure!(view.read(cx).label.read(cx).value() == "Updated BravoX", "Undo erased the separate unapplied draft");
                view.update(cx, |this, _| -> anyhow::Result<()> { this.property_smoke.as_mut().unwrap().stage = 22; this.actor.as_ref().unwrap().send(Command::Accept) })?;
            }
            23 => {
                view.update(cx, |this, cx| this.reload_label(cx));
                window.draw(cx).clear();
                view.read(cx).label.read(cx).focus_handle(cx).focus(window);
                key(window, cx, "ctrl-a"); text(window, cx, "Batch text");
                view.read(cx).label_apply_focus.focus(window); key(window, cx, "enter");
                view.update(cx, |this, cx| { this.send(Command::Undo, cx); this.property_smoke.as_mut().unwrap().stage = 25; });
            }
            25 => {
                if !view.read(cx).label_draft.as_ref().is_some_and(|draft| draft.pending.is_none())
                    || !view.read(cx).preview.as_ref().and_then(|p| p.properties.as_ref()).is_some_and(|p| p.text.as_deref() == Some("Bravo")) { return Ok(false); }
                ensure!(view.read(cx).label.read(cx).value() == "Batch text"
                    && view.read(cx).label_draft.as_ref().is_some_and(|draft| draft.conflict),
                    "Apply/Undo command ordering falsely confirmed or erased the newer text draft");
                view.update(cx, |this, cx| { this.send(Command::Copy, cx); this.send(Command::Accept, cx); });
                ensure!(view.read(cx).error.is_some() && !view.read(cx).clipboard.busy.load(Ordering::Acquire)
                    && !view.read(cx).finishing, "Apply/Undo conflict admitted stale output");
                view.update(cx, |this, cx| { this.error = None; this.reload_label(cx); });
                ensure!(view.read(cx).label.read(cx).value() == "Bravo" && !view.read(cx).has_unresolved_labels(cx),
                    "Reload after Apply/Undo did not resolve current canvas text");
                // The native smoke window is intentionally large and has a
                // minimum client size. Constrain only this owned tools viewport
                // to compact height, using the production scroll/layout path.
                view.update(cx, |this, cx| { this.property_smoke.as_mut().unwrap().compact_tools = true; cx.notify(); });
                window.draw(cx).clear();
                let select_index = 1 + a::Tool::ALL.iter().position(|tool| *tool == a::Tool::Select).unwrap();
                view.read(cx).tools_scroll.scroll_to_bottom();
                window.draw(cx).clear();
                {
                    let scroll = &view.read(cx).tools_scroll;
                    let select = scroll.bounds_for_item(select_index).context("Select tool layout missing")?;
                    ensure!(scroll.bounds().size.height > px(0.) && scroll.bounds().size.height <= px(360.),
                        "Compact tools proof did not apply its owned viewport constraint");
                    ensure!(scroll.offset().y < px(-20.) && select.bottom() + scroll.offset().y < scroll.bounds().top(),
                        "F6 visibility proof did not begin with Select scrolled offscreen");
                }
                view.read(cx).focus.focus(window);
                key(window, cx, "f6");
                ensure!(view.read(cx).toolbar_focus.is_focused(window), "F6 did not focus standalone tools");
                {
                    let scroll = &view.read(cx).tools_scroll;
                    let select = scroll.bounds_for_item(select_index).context("Focused Select tool layout missing")?;
                    let offset = scroll.offset(); let viewport = scroll.bounds();
                    ensure!(select.top() + offset.y >= viewport.top() - px(1.)
                        && select.bottom() + offset.y <= viewport.bottom() + px(1.)
                        && select.left() + offset.x >= viewport.left() - px(1.)
                        && select.right() + offset.x <= viewport.right() + px(1.),
                        "F6 focused Select without scrolling its complete control into view");
                }
                key(window, cx, "f6");
                ensure!(view.read(cx).focus.is_focused(window), "Second F6 did not return to canvas");
                key(window, cx, "f6");
                key(window, cx, "enter");
                ensure!(view.read(cx).focus.is_focused(window) && !view.read(cx).finishing,
                    "Native toolbar activation did not return to canvas safely");
                view.update(cx, |this, cx| { this.property_smoke.as_mut().unwrap().compact_tools = false; cx.notify(); });
                window.draw(cx).clear();
                ensure!(view.read(cx).focus.is_focused(window), "Restoring tools height moved focus away from canvas");
                key(window, cx, "r");
                view.update(cx, |this, _| this.property_smoke.as_mut().unwrap().stage = 40);
            }
            40 => {
                if !view.read(cx).preview.as_ref().is_some_and(|preview| preview.active_tool == a::Tool::Rectangle)
                    || !view.read(cx).canvas_requests.is_empty() { return Ok(false); }
                view.read(cx).label.read(cx).focus_handle(cx).focus(window);
                key(window, cx, "ctrl-a"); text(window, cx, "Property r");
                ensure!(view.read(cx).label.read(cx).value() == "Property r" && view.read(cx).tool == a::Tool::Rectangle,
                    "Property field typing was stolen by canvas tool shortcuts");
                key(window, cx, "f6");
                ensure!(view.read(cx).focus.is_focused(window), "F6 did not return property input to canvas");
                key(window, cx, "ctrl-s");
                view.update(cx, |this, _| this.property_smoke.as_mut().unwrap().stage = 41);
            }
            41 => {
                if !view.read(cx).canvas_requests.is_empty() || view.read(cx).error.is_none() { return Ok(false); }
                ensure!(view.read(cx).actor.is_some() && !view.read(cx).finishing
                    && !view.read(cx).clipboard.busy.load(Ordering::Acquire), "Dirty Ctrl+S admitted output");
                view.update(cx, |this, _| this.error = None);
                key(window, cx, "escape");
                view.update(cx, |this, _| this.property_smoke.as_mut().unwrap().stage = 42);
            }
            42 => {
                if !view.read(cx).canvas_requests.is_empty()
                    || !view.read(cx).preview.as_ref().is_some_and(|preview| preview.overlay.selection.is_empty()) { return Ok(false); }
                ensure!(view.read(cx).retained_labels.iter().any(|draft| draft.value == "Property r"), "Escape selection stage lost the dirty draft");
                key(window, cx, "escape");
                view.update(cx, |this, _| this.property_smoke.as_mut().unwrap().stage = 43);
            }
            43 => {
                if !view.read(cx).canvas_requests.is_empty() || view.read(cx).error.is_none() { return Ok(false); }
                ensure!(view.read(cx).actor.is_some() && !view.read(cx).finishing, "Idle Escape discarded an unresolved text draft");
                view.update(cx, |this, _| this.error = None);
                window.draw(cx).clear();
                view.read(cx).draft_review_focus.focus(window); key(window, cx, "enter");
                view.update(cx, |this, _| this.property_smoke.as_mut().unwrap().stage = 44);
            }
            44 => {
                if !view.read(cx).preview.as_ref().and_then(|preview| preview.properties.as_ref()).is_some_and(|p| p.text.as_deref() == Some("Bravo")) { return Ok(false); }
                ensure!(view.read(cx).label.read(cx).value() == "Property r", "Review after idle Escape lost retained text");
                view.update(cx, |this, cx| this.reload_label(cx));
                view.read(cx).focus.focus(window); key(window, cx, "f1");
                ensure!(view.read(cx).help_visible, "Standalone shortcut help did not open");
                key(window, cx, "escape");
                ensure!(!view.read(cx).help_visible && view.read(cx).actor.is_some() && !view.read(cx).finishing,
                    "Closing shortcut help exited the editor");
                view.update(cx, |this, cx| {
                    this.open_standalone(Source::Image(rcr_toolkit::effects::solid_frame(320, 180, [235, 240, 245, 255])), cx);
                    this.property_smoke = Some(PropertySmoke { stage: 24, ..Default::default() });
                });
                println!("Standalone contextual properties smoke: native Blur strength/Undo pixels; Text/Balloon selection and retained drafts; guarded Apply/Undo; F6 reveals offscreen Select and returns to canvas, native activation; keyboard R; property text ownership; dirty Ctrl+S and staged idle Escape retain drafts; F1/Escape help; accepted pixels/Undo restored; no output services");
            }
            24 => return Ok(true),
            _ => {}
        }
        Ok(false)
    }
    pub fn smoke_finish(view: Entity<Self>, window: &mut Window, cx: &mut App, retry: bool) -> anyhow::Result<bool> {
        window.draw(cx).clear();
        if view.read(cx).finishing || !view.read(cx).preview.as_ref().is_some_and(|p| p.objects == 1) { return Ok(false); }
        anyhow::ensure!(view.read(cx).error.is_none() || retry,
            "Synthetic Actor received an unexpected command/error: {:?}", view.read(cx).error);
        if retry {
            anyhow::ensure!(view.read(cx).actor.is_some() && view.read(cx).saved_path.is_some() && view.read(cx).error.is_some(),
                "Standalone failure discarded its Actor, saved path, or error");
        }
        view.read(cx).finish_focus.focus(window);
        window.draw(cx).clear();
        let keystroke = Keystroke { key: "enter".into(), key_char: None, modifiers: Default::default() };
        window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent { keystroke: keystroke.clone(), is_held: false }), cx);
        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
        anyhow::ensure!(view.read(cx).finishing, "Native Enter did not request standalone completion");
        Ok(true)
    }
    pub fn smoke_closed(&self) -> bool { self.actor.is_none() && !self.finishing }
    /// Owned lifecycle fixtures can check that old CPU pixel allocations die
    /// after closure and parent-scene replacement, without keeping them alive.
    pub fn smoke_surface_images(&self) -> Vec<std::sync::Weak<RenderImage>> {
        self.image.image_weak_refs()
    }
    fn send(&mut self, command: Command, cx: &mut Context<Self>) {
        if self.finishing { return; }
        if matches!(&command, Command::Accept | Command::AcceptRegion(_) | Command::Copy | Command::Project(_))
            && self.has_unresolved_labels(cx)
        {
            self.error = Some("Apply or reload the text draft before saving, copying, or continuing. Use Review retained draft to return to another annotation.".into());
            cx.notify();
            return;
        }
        let Some(actor) = self.actor.as_ref() else { return; };
        let accept = matches!(command, Command::Accept);
        if accept && self.clipboard.busy.load(Ordering::Acquire) {
            self.error = Some("Wait for the current clipboard copy to finish before continuing.".into());
            cx.notify();
            return;
        }
        let copy = matches!(command, Command::Copy);
        if copy {
            if self.canvas_requests.iter().any(|(_, _, _, clipboard)| *clipboard) {
                self.error = Some("Wait for the text clipboard operation to finish before copying the image.".into());
                cx.notify(); return;
            }
            if !self.clipboard.begin() { return; }
            self.error = None;
            self.notice = None;
        }
        let motion = matches!(command, Command::Move(..) | Command::Pan(..));
        let interaction = !matches!(&command, Command::Viewport(..) | Command::Zoom(..) | Command::Pan(..));
        let admitted = actor.send(command);
        if let Err(e) = admitted {
            if copy { self.clipboard.render.take(); }
            self.error = Some(e.to_string());
            cx.notify();
        } else {
            if interaction { self.canvas_interaction = self.canvas_interaction.wrapping_add(1); }
            if accept {
            self.finishing = true;
            self.error = None;
            self.notice = None;
            }
        }
        if !motion {
            cx.notify();
        }
    }
    fn point(&self, position: Point<Pixels>) -> Option<a::PointF> {
        let p = self.preview.as_ref()?;
        let b = self.bounds.get()?;
        Some(p.image_point(
            f32::from(position.x - b.left()) / f32::from(b.size.width).max(1.) * p.width as f32,
            f32::from(position.y - b.top()) / f32::from(b.size.height).max(1.) * p.height as f32,
        ))
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        if let Some(completion) = self.completion.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => Some(StandaloneCompletion::Failed {
                detail: "The save workflow stopped before confirming completion. Your edits remain open.".into(),
                saved_path: self.saved_path.clone(),
            }),
            Err(mpsc::TryRecvError::Empty) => None,
        }) {
            self.completion = None;
            match completion {
                StandaloneCompletion::Succeeded => { cx.emit(ImageEvent::Finished); return; }
                StandaloneCompletion::Failed { detail, saved_path } => {
                    self.saved_path = saved_path;
                    self.finish_failed(detail, cx);
                }
            }
        }
        let Some(actor) = &self.actor else {
            return;
        };
        let preview = actor.take_preview();
        let events = actor.events.try_iter().collect::<Vec<_>>();
        if let Some(mut preview) = preview {
            self.tool = preview.active_tool;
            if let Some(p) = &preview.properties {
                self.fill = p.fill;
                self.sync_label(p, preview.command_revision, cx);
            } else {
                self.retain_active_label(cx);
            }
            self.image.apply(&mut preview);
            self.preview = Some(preview);
            cx.notify();
        }
        for event in events {
            match event {
                Event::CanvasClipboard { request, operation, reply } => {
                    let current = canvas_clipboard_request_current(&self.canvas_requests, request, self.session_generation, self.actor.is_some());
                    if !current {
                        reply.respond(|| Err("The editor session changed; clipboard operation cancelled".into()));
                    } else {
                        queue_canvas_clipboard(self.clipboard_window, operation, reply, self.clipboard.busy.clone(), self.ui_waker.clone());
                    }
                }
                Event::CanvasClipboardError { request, detail } => {
                    if self.canvas_requests.iter().any(|(pending, session, _, _)| *pending == request && *session == self.session_generation) {
                        self.error = Some(detail); cx.notify();
                    }
                }
                Event::Accepted(frame) => {
                    if let Some(proof) = self.property_smoke.as_mut()
                        && matches!(proof.stage, 2 | 5 | 8 | 14 | 16 | 19 | 22)
                    {
                        use sha2::Digest;
                        let hash: [u8; 32] = sha2::Sha256::digest(frame.data()).into();
                        match proof.stage {
                            2 | 14 => proof.baseline = Some(hash),
                            5 if proof.baseline == Some(hash) => self.error = Some("Changing blur strength did not change accepted pixels".into()),
                            8 if proof.baseline != Some(hash) => self.error = Some("Undo did not restore exact accepted pixels".into()),
                            16 if proof.baseline != Some(hash) => self.error = Some("Stale text Apply changed accepted pixels".into()),
                            19 if proof.baseline == Some(hash) => self.error = Some("Text Apply did not change accepted pixels".into()),
                            22 if proof.baseline != Some(hash) => self.error = Some("Text Undo did not restore exact accepted pixels".into()),
                            _ => {}
                        }
                        proof.stage += 1;
                        cx.notify();
                        continue;
                    }
                    if self.finishing {
                        self.accepted = Some(frame);
                        cx.emit(ImageEvent::Ready);
                    }
                }
                Event::Copied(frame) => {
                    let Some(request) = self.clipboard.render.take() else { continue; };
                    let generation = request.generation;
                    // Clipboard access may wait for another process, so keep it off the UI thread.
                    let (tx, rx) = async_channel::bounded(1);
                    let spawned = std::thread::Builder::new().name("rcr-image-clipboard".into()).spawn(move || {
                        use rcr_toolkit::core::workflow::Clipboard;
                        let result =
                            rcr_toolkit::services::SystemClipboard::new(Default::default())
                                .set_image(&frame)
                                .map_err(|e| e.to_string());
                        drop(request); // Release admission before waking the UI.
                        let _ = tx.try_send(result);
                    });
                    if let Err(error) = spawned {
                        self.error = Some(format!("Couldn't start clipboard worker: {error}"));
                        cx.notify();
                        continue;
                    }
                    cx.spawn(async move |view, cx| {
                        let result = rx.recv().await.unwrap_or_else(|_| Err("Clipboard worker stopped before completing the copy".into()));
                        let _ = view.update(cx, |this, cx| {
                            if this.clipboard.is_current(generation) {
                                match result {
                                    Ok(()) => this.notice = Some("Image copied".into()),
                                    Err(error) => this.error = Some(error.chars().take(4_000).collect()),
                                }
                            }
                            // A previous editor's completed worker may have freed
                            // admission, but must not replace this editor's status.
                            cx.notify();
                        });
                    })
                    .detach();
                }
                Event::Saved(path) => {
                    self.project_reply = Some(path);
                    self.notice = Some("Editable project saved".into());
                    cx.notify();
                }
                Event::Error(e) => {
                    if self.completion.is_none() { self.finishing = false; }
                    self.error = Some(e);
                    cx.notify();
                }
                Event::CanvasResult { request, command_revision, action } => {
                    let Some(index) = self.canvas_requests.iter().position(|(pending, session, _, _)| *pending == request && *session == self.session_generation) else { continue; };
                    let (_, _, interaction, _) = self.canvas_requests.remove(index).unwrap();
                    if command_revision <= self.canvas_revision || self.finishing { continue; }
                    if action.is_some() && interaction != self.canvas_interaction { continue; }
                    self.canvas_revision = command_revision;
                    match action {
                        Some(CanvasAction::CopyImage) => self.send(Command::Copy, cx),
                        Some(CanvasAction::Continue) => self.send(Command::Accept, cx),
                        Some(CanvasAction::Cancel) => self.request_cancel(cx),
                        None => {}
                    }
                    cx.notify();
                }
                Event::LabelResult { request, target, command_revision, result } => {
                    if request == u64::MAX && let Some(proof) = self.property_smoke.as_mut() {
                        proof.stale_rejected = result.is_err();
                        if result.is_ok() { self.error = Some("Stale text Apply was accepted".into()); }
                        continue;
                    }
                    let is_active = self.label_draft.as_ref().is_some_and(|draft| draft.target == target
                        && draft.pending.as_ref().is_some_and(|(pending, _)| *pending == request));
                    let active = self.label_draft.as_mut().filter(|draft| draft.target == target
                        && draft.pending.as_ref().is_some_and(|(pending, _)| *pending == request));
                    let draft = active.or_else(|| self.retained_labels.iter_mut().find(|draft| draft.target == target
                        && draft.pending.as_ref().is_some_and(|(pending, _)| *pending == request)));
                    if let Some(draft) = draft {
                        let submitted = draft.pending.take().unwrap().1;
                        match result {
                            Ok(text) if text == submitted => {
                                draft.baseline = text; draft.confirmed_revision = command_revision;
                                // A hidden target has no current text observation.
                                // Review it before treating the confirmation as clean.
                                draft.conflict = !is_active;
                            }
                            Ok(_) => { draft.conflict = true; self.error = Some("Text confirmation did not match the submitted draft. Reload the selected text.".into()); }
                            Err(error) => { draft.conflict = true; self.error = Some(error); }
                        }
                        // A newer preview may already contain Undo or another edit.
                        // Never let this older ACK describe that newer canvas state.
                        if let Some(preview) = self.preview.as_ref()
                            && preview.command_revision >= command_revision
                            && let Some(p) = preview.properties.as_ref()
                            && p.selected_id == draft.target && p.tool == draft.tool
                            && p.text.as_deref() != Some(draft.baseline.as_str())
                        { draft.conflict = true; }
                        cx.notify();
                    }
                }
            }
        }
    }
    fn request_cancel(&mut self, cx: &mut Context<Self>) {
        if self.has_unresolved_labels(cx) {
            self.error = Some("Apply or reload the current text draft, or review/copy/discard retained drafts before closing.".into());
            cx.notify(); return;
        }
        cx.emit(ImageEvent::Cancel);
    }
    fn focus_tools(&self, window: &mut Window, cx: &mut Context<Self>) {
        // The first row is shortcut help; tool rows follow Tool::ALL's order.
        let select = 1 + a::Tool::ALL.iter().position(|tool| *tool == a::Tool::Select).unwrap_or(0);
        self.tools_scroll.scroll_to_item(select);
        self.toolbar_focus.focus(window);
        cx.notify();
    }
    fn canvas_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing { return; }
        let raw = event.keystroke.key.as_str();
        let mods = modifiers(event.keystroke.modifiers);
        if raw == "f6" && !mods.ctrl && !mods.alt {
            if self.focus.is_focused(window) { self.focus_tools(window, cx); }
            else { self.focus.focus(window); }
        } else if raw == "tab" && !mods.ctrl && !mods.alt {
            if self.focus.is_focused(window) { self.focus_tools(window, cx); }
            else if mods.shift { window.focus_prev(); } else { window.focus_next(); }
        } else if raw == "f1" && !mods.ctrl && !mods.alt {
            self.help_visible = !self.help_visible; cx.notify();
        } else if raw == "escape" && self.help_visible {
            self.help_visible = false; self.focus.focus(window); cx.notify();
        } else {
            let canvas_focused = self.focus.is_focused(window);
            // TextInput owns its editing shortcuts and printable text. Native
            // property/tool buttons retain their Enter/Space key-up activation.
            if !canvas_focused && self.label.read(cx).focus_handle(cx).is_focused(window) {
                if raw == "escape" { self.focus.focus(window); } else { return; }
            } else if !canvas_focused && raw == "escape" { self.focus.focus(window); }
            else {
                if !canvas_focused && (!mods.ctrl || !matches!(raw, "c" | "s" | "enter" | "q" | "z" | "y" | "d")) { return; }
                let key = match raw {
                    "left" => a::Key::Left, "right" => a::Key::Right,
                    "up" => a::Key::Up, "down" => a::Key::Down,
                    "home" => a::Key::Home, "end" => a::Key::End,
                    "delete" => a::Key::Delete, "backspace" => a::Key::Backspace,
                    "enter" => a::Key::Enter, "escape" => a::Key::Escape,
                    "space" => a::Key::Char(' '),
                    _ => { let mut chars = raw.chars(); let Some(character) = chars.next() else { return; };
                        if chars.next().is_some() { return; } a::Key::Char(character) }
                };
                if self.canvas_requests.len() >= 256 {
                    self.error = Some("The annotation editor is busy. Wait for the current input to finish.".into()); cx.notify();
                } else if let Some(actor) = self.actor.as_ref() {
                    self.canvas_request = self.canvas_request.wrapping_add(1);
                    let request = self.canvas_request;
                    if canvas_clipboard_key(key, mods) {
                        self.clipboard_window = window.window_handle().ok().and_then(|handle| match handle.as_raw() {
                            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()), _ => None,
                        });
                    }
                    match actor.send(Command::CanvasInput { request, key, text: event.keystroke.key_char.clone(), modifiers: mods, repeat: event.is_held }) {
                        Ok(()) => {
                            self.canvas_interaction = self.canvas_interaction.wrapping_add(1);
                            self.canvas_requests.push_back((request, self.session_generation, self.canvas_interaction, canvas_clipboard_key(key, mods)));
                            if key == a::Key::Escape { self.dragging = false; self.pan = None; }
                        }
                        Err(error) => { self.error = Some(error.to_string()); cx.notify(); }
                    }
                }
            }
        }
        window.prevent_default(); cx.stop_propagation();
    }
    fn project_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing { return; }
        self.canvas_interaction = self.canvas_interaction.wrapping_add(1);
        let session = self.session_generation;
        let selected = cx.prompt_for_new_path(
            &std::env::current_dir().unwrap_or_default(),
            Some("image.ssxe"),
        );
        cx.spawn_in(window, async move |view, cx| {
            if let Ok(Ok(Some(path))) = selected.await {
                let _ = view.update(cx, |this, cx| this.complete_dialog(session, Command::Project(path), cx));
            }
        })
        .detach();
    }
    fn insert_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finishing { return; }
        self.canvas_interaction = self.canvas_interaction.wrapping_add(1);
        let session = self.session_generation;
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Insert image".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = view.update(cx, |this, cx| this.complete_dialog(session, Command::InsertImage(path), cx));
            }
        })
        .detach();
    }
    fn complete_dialog(&mut self, session: u64, command: Command, cx: &mut Context<Self>) -> bool {
        if session != self.session_generation || self.actor.is_none() || self.finishing { return false; }
        self.send(command, cx);
        true
    }
    fn has_unresolved_labels(&self, cx: &App) -> bool {
        let observed_revision = self.preview.as_ref().map_or(0, |preview| preview.command_revision);
        self.label_draft.as_ref().is_some_and(|draft| draft.unresolved(&self.label.read(cx).value(), observed_revision))
            || self.retained_labels.iter().any(|draft| draft.unresolved(&draft.value, observed_revision))
    }
    /// Keep at most 128 inactive drafts, plus the active input. At capacity we
    /// retain the active draft in place instead of evicting the user's text.
    fn retain_active_label(&mut self, cx: &App) -> bool {
        let Some(mut draft) = self.label_draft.take() else { return true; };
        draft.value = self.label.read(cx).value();
        let observed_revision = self.preview.as_ref().map_or(0, |preview| preview.command_revision);
        if draft.unresolved(&draft.value, observed_revision) {
            // A later preview for another selection cannot verify this target.
            if draft.confirmed_revision > observed_revision { draft.conflict = true; }
            if self.retained_labels.len() >= 128 {
                self.label_draft = Some(draft);
                return false;
            }
            self.retained_labels.push(draft);
        }
        true
    }
    fn sync_label(&mut self, properties: &rcr_toolkit::image_editor::Properties, revision: u64, cx: &mut Context<Self>) {
        let Some(text) = properties.text.as_ref().filter(|text| single_line_label(text)) else {
            self.retain_active_label(cx); return;
        };
        let same = self.label_draft.as_ref().is_some_and(|draft| draft.target == properties.selected_id && draft.tool == properties.tool);
        if !same {
            // Remove the requested retained draft first, so switching between
            // retained targets still works when all 128 slots are occupied.
            let retained = self.retained_labels.iter().position(|draft| draft.target == properties.selected_id && draft.tool == properties.tool)
                .map(|index| self.retained_labels.remove(index));
            if !self.retain_active_label(cx) {
                if let Some(draft) = retained { self.retained_labels.push(draft); }
                return;
            }
            let draft = retained.unwrap_or_else(|| LabelDraft { target: properties.selected_id, tool: properties.tool,
                baseline: text.clone(), value: text.clone(), confirmed_revision: 0, pending: None, conflict: false });
            self.label.update(cx, |input, cx| input.set_value(draft.value.clone(), cx));
            self.label_draft = Some(draft);
        }
        let value = self.label.read(cx).value();
        let draft = self.label_draft.as_mut().unwrap();
        if draft.pending.is_none() && revision >= draft.confirmed_revision {
            if draft.baseline == *text { draft.conflict = false; }
            else if value == draft.baseline && draft.confirmed_revision == 0 && !draft.conflict {
                draft.baseline = text.clone(); draft.value = text.clone();
                self.label.update(cx, |input, cx| input.set_value(text.clone(), cx));
            } else { draft.conflict = true; }
        }
    }
    fn review_retained_label(&mut self, target: Option<a::ObjectId>, tool: a::Tool, session: u64, cx: &mut Context<Self>) {
        if session == self.session_generation && self.retained_labels.iter().any(|draft| draft.target == target && draft.tool == tool) {
            self.send(Command::ReviewLabel { target, tool }, cx);
        }
    }
    fn discard_retained_label(&mut self, target: Option<a::ObjectId>, tool: a::Tool, session: u64, cx: &mut Context<Self>) {
        if session != self.session_generation { return; }
        let observed_revision = self.preview.as_ref().map_or(0, |preview| preview.command_revision);
        if let Some(index) = self.retained_labels.iter().position(|draft| draft.target == target && draft.tool == tool
            && draft.pending.is_none() && draft.confirmed_revision <= observed_revision) {
            self.retained_labels.remove(index); self.error = None; cx.notify();
        }
    }
    fn copy_retained_label(&mut self, target: Option<a::ObjectId>, tool: a::Tool, session: u64, cx: &mut Context<Self>) {
        if self.finishing { return; }
        let Some(text) = retained_draft_payload(&self.retained_labels, target, tool, session, self.session_generation) else { return; };
        if self.clipboard.busy.load(Ordering::Acquire) || self.canvas_requests.iter().any(|(_, _, _, clipboard)| *clipboard) {
            self.error = Some("Wait for the current clipboard operation before copying draft text.".into()); cx.notify(); return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.notice = Some("Draft text copied. The retained draft is still available.".into());
        cx.notify();
    }
    fn apply_label(&mut self, cx: &mut Context<Self>) {
        if self.finishing { return; }
        let Some(draft) = self.label_draft.as_mut() else { return; };
        if !self.preview.as_ref().and_then(|preview| preview.properties.as_ref()).is_some_and(|p| p.selected_id == draft.target && p.tool == draft.tool) {
            self.error = Some("Return to this draft's annotation before applying it.".into()); cx.notify(); return;
        }
        if draft.pending.is_some() || draft.confirmed_revision > self.preview.as_ref().map_or(0, |preview| preview.command_revision) { return; }
        if draft.conflict {
            self.error = Some("Selected text changed. Reload it before applying your draft.".into());
            cx.notify(); return;
        }
        let text = self.label.read(cx).value();
        if text == draft.baseline { return; }
        self.label_request = self.label_request.wrapping_add(1);
        let request = self.label_request;
        let command = Command::ApplyLabel { request, target: draft.target, tool: draft.tool,
            expected: draft.baseline.clone(), text: text.clone() };
        match self.actor.as_ref().map(|actor| actor.send(command)) {
            Some(Ok(())) => {
                draft.pending = Some((request, text)); self.error = None;
                self.canvas_interaction = self.canvas_interaction.wrapping_add(1);
            }
            Some(Err(error)) => self.error = Some(error.to_string()),
            None => return,
        }
        cx.notify();
    }
    fn reload_label(&mut self, cx: &mut Context<Self>) {
        if self.finishing || self.label_draft.as_ref().is_some_and(|draft| draft.pending.is_some()
            || draft.confirmed_revision > self.preview.as_ref().map_or(0, |preview| preview.command_revision)) { return; }
        self.label_draft = None;
        if let Some((properties, revision)) = self.preview.as_ref().and_then(|p| p.properties.clone().map(|properties| (properties, p.command_revision))) { self.sync_label(&properties, revision, cx); }
        self.error = None;
        cx.notify();
    }
    fn change_stroke_width(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(p) = self.preview.as_mut().and_then(|preview| preview.properties.as_mut()) {
            let width = (p.width + delta).clamp(0.5, 64.);
            if width == p.width { return; }
            p.width = width;
            self.send(Command::Width(width), cx);
        }
    }
    fn change_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(font) = self.preview.as_mut().and_then(|preview| preview.properties.as_mut()).and_then(|p| p.font.as_mut()) {
            let size = (*font + delta).clamp(6., 256.);
            if size == *font { return; }
            *font = size;
            self.send(Command::FontSize(size), cx);
        }
    }
    fn change_effect_amount(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(amount) = self.preview.as_mut().and_then(|p| p.properties.as_mut()).and_then(|p| p.amount.as_mut()) {
            *amount = (*amount + delta).clamp(1., 128.);
            let value = *amount;
            self.send(Command::EffectAmount(value), cx);
        }
    }
    fn change_magnification(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(zoom) = self.preview.as_mut().and_then(|p| p.properties.as_mut()).and_then(|p| p.zoom.as_mut()) {
            *zoom = (*zoom + delta).clamp(1., 8.);
            let value = *zoom;
            self.send(Command::MagnifyZoom(value), cx);
        }
    }
    fn change_annotation_opacity(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(properties) = self.preview.as_mut().and_then(|p| p.properties.as_mut()) {
            properties.opacity = (properties.opacity + delta).clamp(0.05, 1.);
            let value = properties.opacity;
            self.send(Command::Opacity(value), cx);
        }
    }
}
fn property_button(id: impl Into<ElementId>, label: impl Into<SharedString>, enabled: bool) -> Stateful<Div> {
    button_with_hover(id, label, if enabled { 0x2a2b30 } else { SOFT })
        .opacity(if enabled { 1. } else { 0.4 })
        .tab_stop(enabled)
        .when(!enabled, |button| button.cursor_default())
}
fn tool_name(tool: a::Tool) -> &'static str {
    match tool {
        a::Tool::Select => "Select",
        a::Tool::Rectangle => "Rectangle",
        a::Tool::Ellipse => "Ellipse",
        a::Tool::Line => "Line",
        a::Tool::Arrow => "Arrow",
        a::Tool::Freehand => "Pen",
        a::Tool::FreehandArrow => "Arrow pen",
        a::Tool::Text => "Text",
        a::Tool::Balloon => "Speech balloon",
        a::Tool::Step => "Numbered step",
        a::Tool::Magnify => "Magnifier",
        a::Tool::Spotlight => "Spotlight",
        a::Tool::Blur => "Blur",
        a::Tool::Pixelate => "Pixelate",
        a::Tool::Highlight => "Highlighter",
        a::Tool::HighlightPen => "Highlighter pen",
        a::Tool::Image => "Insert image",
        a::Tool::Sticker => "Sticker",
        a::Tool::Cursor => "Cursor stamp",
        a::Tool::Grid => "Grid",
        a::Tool::Eraser => "Eraser",
        a::Tool::Crop => "Crop",
        a::Tool::CropEllipse => "Ellipse crop",
        a::Tool::CropFreeform => "Freeform crop",
        a::Tool::CutOut => "Cut out",
    }
}
fn cursor(hint: a::input::CursorHint) -> CursorStyle {
    match hint {
        a::input::CursorHint::Text => CursorStyle::IBeam,
        a::input::CursorHint::ResizeEw => CursorStyle::ResizeLeftRight,
        a::input::CursorHint::ResizeNs => CursorStyle::ResizeUpDown,
        a::input::CursorHint::Move | a::input::CursorHint::Grabbing => CursorStyle::ClosedHand,
        a::input::CursorHint::Default => CursorStyle::Arrow,
        _ => CursorStyle::Crosshair,
    }
}
impl Render for ImageEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.image.release(window);
        if self.focus_requested {
            self.focus.focus(window);
            self.focus_requested = false;
        }
        if self.finishing {
            return div().id("image-save-pending").track_focus(&self.focus).size_full()
                .flex().flex_col().items_center().justify_center().gap_3().bg(rgb(BG))
                .text_color(rgb(TEXT))
                .child(if self.completion.is_some() { "Saving and copying image…" } else { "Preparing image…" })
                .child(muted("Your annotations remain open until completion."))
                .child(button("cancel-image-save", "Cancel · Esc").on_click(cx.listener(|_, _, _, cx| cx.emit(ImageEvent::Cancel))))
                .on_key_down(cx.listener(|_, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" { cx.emit(ImageEvent::Cancel); }
                    else if event.keystroke.key == "tab" {
                        if event.keystroke.modifiers.shift { window.focus_prev(); } else { window.focus_next(); }
                    } else { return; }
                    window.prevent_default(); cx.stop_propagation();
                })).into_any_element();
        }
        let mut tools = div()
            .id("annotation-tools")
            .w(px(165.))
            .h_full()
            .when(self.property_smoke.as_ref().is_some_and(|proof| proof.compact_tools), |tools| tools.max_h(px(360.)))
            .overflow_y_scroll()
            .track_scroll(&self.tools_scroll)
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .bg(rgb(PANEL))
            .child(button("image-shortcuts", "Shortcuts (F1)").justify_start()
                .on_click(cx.listener(|this, _, _, cx| { this.help_visible = !this.help_visible; cx.notify(); })));
        for (i, tool) in a::Tool::ALL.into_iter().enumerate() {
            tools = tools.child(
                button(("image-tool", i), tool_name(tool))
                    .justify_start()
                    .h(px(28.))
                    .when(tool == a::Tool::Select, |button| button.track_focus(&self.toolbar_focus))
                    .bg(rgb(if tool == self.tool { 0x173025 } else { PANEL }))
                    .text_color(rgb(if tool == self.tool { ACCENT } else { MUTED }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.tool = tool;
                        this.send(Command::Tool(tool), cx);
                        if tool == a::Tool::Image {
                            this.insert_dialog(window, cx);
                        }
                        if matches!(tool, a::Tool::Text | a::Tool::Balloon) {
                            this.send(Command::Label(this.label.read(cx).value()), cx);
                        }
                        this.focus.focus(window);
                    })),
            )
        }
        let image = self.image.snapshot();
        let display_scale = window.scale_factor();
        let measure = self.bounds.clone();
        let measure_area = self.area.clone();
        let (iw, ih) = self
            .preview
            .as_ref()
            .map_or((16, 9), |p| (p.width, p.height));
        let aspect = iw as f32 / ih.max(1) as f32;
        let overlay = self
            .preview
            .as_ref()
            .map(|p| (p.overlay.clone(), p.scale, p.offset, p.padding));
        let canvas_el = div()
            .id("annotation-canvas")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(0x08090b))
            .cursor(
                self.preview
                    .as_ref()
                    .map_or(CursorStyle::Crosshair, |p| cursor(p.cursor)),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, e: &MouseDownEvent, _, _| {
                    this.pan = Some(e.position);
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _, _, _| {
                    this.pan = None;
                }),
            )
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                if this.zoom.is_some() {
                    let delta = e.delta.pixel_delta(px(20.));
                    this.send(Command::Pan(-f32::from(delta.x), -f32::from(delta.y)), cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, w, cx| {
                    this.focus.focus(w);
                    if let Some(p) = this.point(e.position) {
                        this.dragging = true;
                        this.send(Command::Down(p, modifiers(e.modifiers)), cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .child(
                canvas(
                    move |area, _, _| {
                        measure_area.set(Some(area));
                        let width = area
                            .size
                            .width
                            .min(area.size.height * aspect)
                            .min(px(iw as f32 / display_scale));
                        let height = width / aspect;
                        let b = Bounds::new(
                            point(
                                area.left() + (area.size.width - width) / 2.,
                                area.top() + (area.size.height - height) / 2.,
                            ),
                            size(width, height),
                        );
                        measure.set(Some(b));
                        b
                    },
                    move |_, target, w, _| {
                        image.draw(target, w);
                        if let Some((overlay, scale, offset, padding)) = overlay {
                            let k = f32::from(target.size.width) / iw.max(1) as f32;
                            let map = |p: a::PointF| {
                                point(
                                    target.left()
                                        + px(((p.x + padding.0 as f32) * scale - offset.0) * k),
                                    target.top()
                                        + px(((p.y + padding.1 as f32) * scale - offset.1) * k),
                                )
                            };
                            let mut boxes =
                                overlay.selection.iter().map(|s| s.rect).collect::<Vec<_>>();
                            if let Some(c) = overlay.creation {
                                boxes.push(c.rect);
                            }
                            if let Some(c) = overlay.crop {
                                boxes.push(c.rect);
                            }
                            if let Some(m) = overlay.marquee {
                                boxes.push(m);
                            }
                            for rect in boxes {
                                let p = map(a::PointF::new(rect.x, rect.y));
                                let width = px(rect.w * scale * k);
                                let height = px(rect.h * scale * k);
                                crate::annotation_surface::outline(
                                    w,
                                    Bounds::new(p, size(width, height)),
                                );
                            }
                            for handle in overlay.handles {
                                let p = map(handle.pos);
                                w.paint_quad(fill(
                                    Bounds::new(p - point(px(3.), px(3.)), size(px(6.), px(6.))),
                                    rgb(TEXT),
                                ));
                            }
                            if let Some(caret) = overlay.caret {
                                let p = map(a::PointF::new(caret.caret.x, caret.caret.y));
                                w.paint_quad(fill(
                                    Bounds::new(p, size(px(1.), px(caret.caret.h * scale * k))),
                                    rgb(TEXT),
                                ));
                            }
                        }
                    },
                )
                .size_full(),
            );
        // The actor's raster size follows the displayed physical area, with bounded buffers.
        if let Some(b) = self.area.get() {
            let viewport = (
                (f32::from(b.size.width) * window.scale_factor()) as u32,
                (f32::from(b.size.height) * window.scale_factor()) as u32,
            );
            if viewport.0 > 0 && viewport.1 > 0 && viewport != self.viewport {
                self.viewport = viewport;
                self.send(Command::Viewport(viewport.0, viewport.1), cx);
            }
        }
        let context = self.preview.as_ref().and_then(|preview| preview.properties.clone());
        let has_selection = context.as_ref().is_some_and(|p| p.selected > 0);
        let can_undo = self.preview.as_ref().is_some_and(|preview| preview.can_undo);
        let can_redo = self.preview.as_ref().is_some_and(|preview| preview.can_redo);
        let current_color = context.as_ref().map(|p| (u32::from(p.color.r) << 16) | (u32::from(p.color.g) << 8) | u32::from(p.color.b));
        let mut properties = div()
            .w(px(210.))
            .flex_shrink_0()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .bg(rgb(PANEL))
            .child(muted("APPEARANCE"))
            .child(muted(context.as_ref().map_or_else(
                || "Select an annotation to edit its properties".into(),
                |p| format!("{} · {}", tool_name(p.tool), if p.selected > 0 { "selected" } else { "defaults" }),
            )));
        if self.help_visible {
            properties = properties.child(muted("KEYBOARD"))
                .child(muted("V select; A arrow; R rectangle; E ellipse; F pen; T text"))
                .child(muted("B blur; P pixelate; H highlight; N step; O balloon; M magnify; S spotlight"))
                .child(muted("Enter edits selected text or confirms crop, otherwise continues. Ctrl+Enter / Ctrl+S continues the current image workflow."))
                .child(muted("Ctrl+C / Ctrl+Shift+C copies the image outside canvas text editing. Ctrl+Z undo; Ctrl+Shift+Z redo."))
                .child(muted("Escape finishes text, cancels a gesture/crop, clears selection, then closes. Unapplied drafts remain protected."))
                .child(muted("F6 switches canvas/tools. Tab moves between controls. Text fields keep their own shortcuts."))
                .child(button("close-image-shortcuts", "Close shortcuts (F1)")
                    .on_click(cx.listener(|this, _, window, cx| { this.help_visible = false; this.focus.focus(window); cx.notify(); })));
        }
        let label_session = self.session_generation;
        if !self.retained_labels.is_empty() {
            properties = properties.child(muted(format!("{} retained text draft(s)", self.retained_labels.len())));
            for index in 0..self.retained_labels.len().min(8) {
                let draft = &self.retained_labels[index];
                let pending = draft.pending.is_some()
                    || draft.confirmed_revision > self.preview.as_ref().map_or(0, |preview| preview.command_revision);
                let target = draft.target; let tool = draft.tool;
                let name: String = draft.value.chars().take(28).collect();
                properties = properties.child(div().flex().flex_col().gap_1()
                    .child(muted(name))
                    .child(button(("review-text-draft", index), "Review retained draft")
                        .when(index == 0, |button| button.track_focus(&self.draft_review_focus))
                        .on_click(cx.listener(move |this, _, _, cx| this.review_retained_label(target, tool, label_session, cx))))
                    .child(button(("copy-text-draft", index), "Copy draft text")
                        .on_click(cx.listener(move |this, _, _, cx| this.copy_retained_label(target, tool, label_session, cx))))
                    .child(button(("discard-text-draft", index), if pending { "Applying..." } else { "Discard draft" })
                        .opacity(if pending { 0.5 } else { 1. })
                        .when(!pending, |button| button.on_click(cx.listener(move |this, _, _, cx| this.discard_retained_label(target, tool, label_session, cx))))));
            }
            if self.retained_labels.len() > 8 { properties = properties.child(muted("Resolve a draft to reveal more retained drafts.")); }
        }
        if let Some(draft) = self.label_draft.as_ref()
            && !context.as_ref().is_some_and(|p| p.selected_id == draft.target && p.tool == draft.tool)
        {
            let target = draft.target; let tool = draft.tool;
            properties = properties.child(muted("Text drafts are full. Resolve the current retained draft before editing another text annotation."))
                .child(button("review-active-draft", "Review retained draft")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if label_session == this.session_generation { this.send(Command::ReviewLabel { target, tool }, cx); }
                    })))
                .child(button("discard-active-draft", "Discard draft")
                    .when(draft.pending.is_none() && draft.confirmed_revision <= self.preview.as_ref().map_or(0, |preview| preview.command_revision), |button| button.on_click(cx.listener(move |this, _, _, cx| {
                        if label_session != this.session_generation || !this.label_draft.as_ref().is_some_and(|draft| draft.target == target && draft.tool == tool && draft.pending.is_none()
                            && draft.confirmed_revision <= this.preview.as_ref().map_or(0, |preview| preview.command_revision)) { return; }
                        this.label_draft = None;
                        if let Some((p, revision)) = this.preview.as_ref().and_then(|p| p.properties.clone().map(|properties| (properties, p.command_revision))) { this.sync_label(&p, revision, cx); }
                        cx.notify();
                    }))));
        }
        let mut colors = div().flex().flex_wrap().gap_2();
        for (i, color) in [0xef4444, 0x3ecf8e, 0x3b82f6, 0xfacc15, 0xffffff, 0x111111]
            .into_iter()
            .enumerate()
        {
            colors = colors.child(
                div()
                    .id(("image-color", i))
                    .tab_index(0)
                    .focus(|s| s.border_color(rgb(TEXT)))
                    .size(px(22.))
                    .rounded_full()
                    .bg(rgb(color))
                    .border_2()
                    .border_color(rgb(if current_color == Some(color) { TEXT } else { LINE }))
                    .flex().items_center().justify_center()
                    .when(current_color == Some(color), |swatch| swatch.child(div().size(px(6.)).rounded_full()
                        .bg(rgb(if matches!(color, 0xffffff | 0xfacc15 | 0x3ecf8e) { BG } else { TEXT }))))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send(
                            Command::Stroke(a::Color::rgb(
                                (color >> 16) as u8,
                                (color >> 8) as u8,
                                color as u8,
                            )),
                            cx,
                        )
                    })),
            );
        }
        if let Some(p) = context {
            let redaction = matches!(p.tool, a::Tool::Blur | a::Tool::Pixelate | a::Tool::Spotlight);
            if !redaction {
                properties = properties.child(div().flex().items_center().justify_between()
                    .child(muted("Color"))
                    .child(mono(format!("#{:06X}", current_color.unwrap_or(0)))))
                    .child(colors);
            }
            if !redaction && !matches!(p.tool, a::Tool::Text | a::Tool::Highlight) {
                properties = properties.child(muted("Stroke width"))
                    .child(div().flex().items_center().justify_between().gap_2()
                        .child(property_button("stroke-width-less", "-", p.width > 0.5)
                            .when(p.width > 0.5, |button| button.on_click(cx.listener(|this, _, _, cx| this.change_stroke_width(-1., cx)))))
                        .child(mono(format!("{} px", p.width)))
                        .child(property_button("stroke-width-more", "+", p.width < 64.)
                            .when(p.width < 64., |button| button.on_click(cx.listener(|this, _, _, cx| this.change_stroke_width(1., cx))))));
            }
            if matches!(p.tool, a::Tool::Rectangle | a::Tool::Ellipse | a::Tool::Balloon | a::Tool::Step) {
                properties = properties.child(
                    button(
                        "shape-fill",
                        if self.fill { "Fill: on" } else { "Fill: off" },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.fill = !this.fill;
                        this.send(Command::Fill(this.fill), cx);
                    })),
                );
            }
            if let Some(amount) = p.amount {
                properties = properties.child(muted(if p.tool == a::Tool::Blur { "Blur strength" } else { "Pixel block size" }))
                    .child(div().flex().items_center().justify_between().gap_2()
                        .child(button("image-effect-less", "−").on_click(cx.listener(|this, _, _, cx| this.change_effect_amount(-1., cx))))
                        .child(mono(format!("{amount:.0} px")))
                        .child(button("image-effect-more", "+").track_focus(&self.amount_plus_focus)
                            .on_click(cx.listener(|this, _, _, cx| this.change_effect_amount(1., cx)))));
            }
            if let Some(zoom) = p.zoom {
                properties = properties.child(muted("Magnifier zoom"))
                    .child(div().flex().items_center().justify_between().gap_2()
                        .child(button("image-magnifier-less", "−").on_click(cx.listener(|this, _, _, cx| this.change_magnification(-0.5, cx))))
                        .child(mono(format!("{zoom:.1}×")))
                        .child(button("image-magnifier-more", "+").on_click(cx.listener(|this, _, _, cx| this.change_magnification(0.5, cx)))));
            }
            if let Some(font) = p.font {
                properties = properties.child(muted("Text / balloon content"));
                if p.selected > 1 {
                    properties = properties.child(muted("Select one text annotation to edit its content."));
                } else if p.text.as_ref().is_some_and(|text| !single_line_label(text)) {
                    properties = properties.child(muted("Edit multi-line text on the canvas: select it and press Enter. Shift+Enter adds a line."));
                } else if !self.label_draft.as_ref().is_some_and(|draft| draft.target == p.selected_id && draft.tool == p.tool) {
                    properties = properties.child(muted("Review or discard a retained draft to edit this text."));
                } else {
                    let pending = self.label_draft.as_ref().is_some_and(|draft| draft.pending.is_some()
                        || draft.confirmed_revision > self.preview.as_ref().map_or(0, |preview| preview.command_revision));
                    let conflict = self.label_draft.as_ref().is_some_and(|draft| draft.conflict);
                    let dirty = self.label_draft.as_ref().is_some_and(|draft| self.label.read(cx).value() != draft.baseline);
                    properties = properties.child(self.label.clone())
                        .child(button("apply-text", if pending { "Applying…" } else { "Apply text" }).track_focus(&self.label_apply_focus).opacity(if pending { 0.5 } else { 1. })
                            .on_click(cx.listener(|this, _, _, cx| this.apply_label(cx))))
                        .when(conflict, |properties| properties.child(muted("Selected text changed. Reload it before applying your draft.")))
                        .when(dirty || conflict, |properties| properties
                            .child(button("reload-selected-text", "Reload selected text").track_focus(&self.label_reload_focus)
                                .opacity(if pending { 0.5 } else { 1. })
                                .when(!pending, |button| button.on_click(cx.listener(|this, _, _, cx| this.reload_label(cx))))));
                }
                properties = properties.child(muted("Font size"))
                        .child(div().flex().items_center().justify_between().gap_2()
                            .child(property_button("font-size-less", "-", font > 6.)
                                .when(font > 6., |button| button.on_click(cx.listener(|this, _, _, cx| this.change_font_size(-2., cx)))))
                            .child(mono(format!("{font} px")))
                            .child(property_button("font-size-more", "+", font < 256.)
                                .when(font < 256., |button| button.track_focus(&self.font_plus_focus)
                                    .on_click(cx.listener(|this, _, _, cx| this.change_font_size(2., cx))))));
            }
            if !redaction {
                properties = properties.child(muted("Opacity"))
                    .child(div().flex().items_center().justify_between().gap_2()
                        .child(button("image-opacity-less", "−").on_click(cx.listener(|this, _, _, cx| this.change_annotation_opacity(-0.1, cx))))
                        .child(mono(format!("{:.0}%", p.opacity * 100.)))
                        .child(button("image-opacity-more", "+").on_click(cx.listener(|this, _, _, cx| this.change_annotation_opacity(0.1, cx)))));
            }
        }
        properties = properties.child(muted("SELECTED ANNOTATION").mt_4())
            .when(!has_selection, |properties| properties.child(muted("Select an annotation to use these actions.")))
            .child(property_button("duplicate-object", "Duplicate", has_selection)
                .when(has_selection, |button| button.on_click(cx.listener(|this, _, _, cx| this.send(Command::Duplicate, cx)))))
            .child(property_button("front-object", "Bring to front", has_selection)
                .when(has_selection, |button| button.on_click(cx.listener(|this, _, _, cx| this.send(Command::Front, cx)))))
            .child(property_button("back-object", "Send to back", has_selection)
                .when(has_selection, |button| button.on_click(cx.listener(|this, _, _, cx| this.send(Command::Back, cx)))))
            .child(property_button("remove-object", "Delete selection", has_selection)
                .when(has_selection, |button| button.on_click(cx.listener(|this, _, _, cx| this.send(Command::Delete, cx)))))
            .child(muted("IMAGE").mt_4())
            .child(button("rotate-image", "Rotate image 90\u{b0}")
                .on_click(cx.listener(|this, _, _, cx| this.send(Command::Rotate, cx))))
            .child(button("flip-image", "Flip image horizontally")
                .on_click(cx.listener(|this, _, _, cx| this.send(Command::Flip, cx))))
            .child(button("grayscale-image", "Make image grayscale")
                .on_click(cx.listener(|this, _, _, cx| this.send(Command::Effect(rcr_toolkit::effects::Effect::Grayscale), cx))));
        let status = self
            .preview
            .as_ref()
            .map_or("Preparing image…".to_owned(), |p| {
                format!(
                    "{} × {} · {} annotations",
                    p.canvas.0, p.canvas.1, p.objects
                )
            });
        let mut root = div()
            .id("image-editor")
            .key_context("ImageEditor")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .font_family("Geist")
            .text_size(px(13.))
            .text_color(rgb(TEXT))
            .bg(rgb(BG))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if let Some(previous) = this.pan {
                    if e.pressed_button == Some(MouseButton::Middle) {
                        this.pan = Some(e.position);
                        this.send(
                            Command::Pan(
                                f32::from(previous.x - e.position.x),
                                f32::from(previous.y - e.position.y),
                            ),
                            cx,
                        );
                    } else {
                        this.pan = None;
                    }
                }
                if this.dragging
                    && let Some(p) = this.point(e.position)
                {
                    this.send(Command::Move(p, modifiers(e.modifiers)), cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, cx| {
                    if this.dragging {
                        this.dragging = false;
                        if let Some(p) = this.point(e.position) {
                            this.send(Command::Up(p, modifiers(e.modifiers)), cx);
                        }
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, cx| {
                    if this.dragging {
                        this.dragging = false;
                        if let Some(p) = this.point(e.position) {
                            this.send(Command::Up(p, modifiers(e.modifiers)), cx);
                        }
                    }
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.canvas_key(event, window, cx)))
            .child(
                div()
                    .h(px(56.))
                    .flex_shrink_0()
                    .px_5()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(logo())
                    .child(
                        button("close-image", "Back")
                            .child(icon("arrow-left"))
                            .on_click(cx.listener(|this, _, _, cx| this.request_cancel(cx))),
                    )
                    .child("Image editor")
                    .child(div().flex_1())
                    .child(
                        property_button("image-undo", "Undo", can_undo)
                            .when(can_undo, |button| button.on_click(cx.listener(|this, _, _, cx| this.send(Command::Undo, cx)))),
                    )
                    .child(
                        property_button("image-redo", "Redo", can_redo)
                            .when(can_redo, |button| button.on_click(cx.listener(|this, _, _, cx| this.send(Command::Redo, cx)))),
                    )
                    .child(
                        button(
                            "image-zoom",
                            if self.zoom.is_some() { "100%" } else { "Fit" },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.zoom = if this.zoom.is_some() { None } else { Some(1.) };
                            this.send(Command::Zoom(this.zoom), cx);
                        })),
                    )
                    .child(
                        button("copy-image", if self.clipboard.busy.load(Ordering::Acquire) { "Copying…" } else { "Copy image" })
                            .when(self.clipboard.busy.load(Ordering::Acquire), |s| s.opacity(0.5))
                            .on_click(cx.listener(|this, _, _, cx| this.send(Command::Copy, cx))),
                    )
                    .child(
                        button("save-image-project", "Save project")
                            .on_click(cx.listener(|this, _, w, cx| this.project_dialog(w, cx))),
                    )
                    .child(
                        button("finish-image", if !self.standalone { "Continue workflow" }
                            else if self.error.is_some() { "Retry and continue" } else { "Save and continue" })
                            .bg(rgb(ACCENT))
                            .track_focus(&self.finish_focus)
                            .text_color(rgb(BG))
                            .on_click(cx.listener(|this, _, _, cx| this.send(Command::Accept, cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(tools)
                    .child(canvas_el)
                    .child(
                        div()
                            .id("image-properties")
                            .h_full()
                            .overflow_y_scroll()
                            .child(properties),
                    ),
            )
            .child(
                div()
                    .h(px(28.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(mono(status))
                    .child(muted(
                        "Shift: constrain · Ctrl: snap · Middle-drag / scroll: pan at 100%",
                    )),
            );
        if self.preview.as_ref().is_some_and(|p| p.pending_crop) {
            root = root.child(
                button("confirm-image-crop", "Apply crop · Enter")
                    .on_click(cx.listener(|this, _, _, cx| this.send(Command::ApplyCrop, cx))),
            );
        }
        root.when(
            self.preview
                .as_ref()
                .is_some_and(|preview| preview.detail_limited),
            |s| {
                s.child(div().px_4().py_1().child(muted(
                    "Preview detail reduced for large effects · saved images keep full resolution",
                )))
            },
        )
        .when_some(self.error.clone(), |s, e| {
            s.child(div().id("image-error-details").max_h(px(110.)).overflow_y_scroll()
                .p_2().text_color(rgb(0xffa0a0)).child(e))
        })
        .when_some(self.saved_path.clone(), |s, path| {
            s.child(div().px_3().py_2().flex().flex_col().gap_1()
                .child(muted("Image saved. Retry finishes missing steps; edited pixels create a new file."))
                .child(div().flex().items_center().gap_2().min_w_0()
                    .child(mono(path.display().to_string()).flex_1().min_w_0().truncate())
                    .child(button("show-partial-image-save", if self.shell_operation != 0 { "Opening…" } else { "Show saved file" })
                        .on_click(cx.listener(move |this, _, _, cx| this.reveal_saved_path(path.clone(), cx))))))
        })
        .when_some(self.notice.clone(), |s, n| s.child(muted(n)))
        .into_any_element()
    }
}

#[cfg(test)]
mod clipboard_tests {
    fn owned_clipboard_request(key: char) -> (super::Actor, super::CanvasClipboardOperation, super::CanvasClipboardReply) {
        use super::{a, Actor, Command, Event, Source};
        let actor = Actor::new(Source::Image(rcr_toolkit::effects::solid_frame(160, 100, [255; 4]))).unwrap();
        for command in [Command::Tool(a::Tool::Text),
            Command::Down(a::PointF::new(10., 10.), a::Modifiers::NONE),
            Command::Up(a::PointF::new(100., 60.), a::Modifiers::NONE), Command::Text("Keep".into()),
            Command::Key(a::Key::Char('a'), a::Modifiers::CTRL),
            Command::CanvasInput { request: 1, key: a::Key::Char(key), text: None, modifiers: a::Modifiers::CTRL, repeat: false }] {
            actor.send(command).unwrap();
        }
        let Event::CanvasClipboard { operation, reply, .. } = actor.events.recv_timeout(std::time::Duration::from_secs(5)).unwrap() else { panic!("Missing owned clipboard request"); };
        (actor, operation, reply)
    }
    #[test]
    fn text_worker_holds_actual_io_lease_after_close_and_skips_expired_queued_job() {
        use super::{ClipboardLease, TextClipboardJob, TextClipboardWorker};
        use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}, mpsc};
        use std::time::{Duration, Instant};
        let (entered, started) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = calls.clone();
        let worker = TextClipboardWorker::start(move |_, _| {
            worker_calls.fetch_add(1, Ordering::AcqRel);
            entered.send(()).unwrap(); gate.recv().unwrap(); Ok(None)
        }).unwrap();
        let (first, operation, reply) = owned_clipboard_request('v');
        let busy = Arc::new(AtomicBool::new(true));
        let (wake, _woken) = async_channel::bounded(2);
        assert!(worker.submit(TextClipboardJob { owner: None, operation, reply, lease: ClipboardLease(busy.clone()), wake: wake.clone() }).is_ok());
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let (second, operation, reply) = owned_clipboard_request('v');
        let second_preview = second.preview.clone();
        let second_busy = Arc::new(AtomicBool::new(true));
        assert!(worker.submit(TextClipboardJob { owner: None, operation, reply, lease: ClipboardLease(second_busy.clone()), wake }).is_ok());
        drop(first); drop(second);
        let mut lifetime = ClipboardLifetime { busy: busy.clone(), ..Default::default() };
        lifetime.reset();
        assert!(!lifetime.begin(), "Closing/reopening admitted image copy over blocked text IO");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Arc::strong_count(&second_preview) != 1 {
            assert!(Instant::now() < deadline, "Queued clipboard Actor failed to cancel"); std::thread::yield_now();
        }
        assert!(busy.load(Ordering::Acquire), "Actor cancellation released active OS admission");
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while busy.load(Ordering::Acquire) || second_busy.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "Completed worker retained clipboard admission"); std::thread::yield_now();
        }
        assert_eq!(calls.load(Ordering::Acquire), 1, "Expired queued reply reached clipboard IO");
        assert!(lifetime.begin());
    }
    #[test]
    fn failed_text_worker_write_does_not_cut_annotation_text() {
        use super::{ClipboardLease, Event, TextClipboardJob, TextClipboardWorker};
        use std::sync::{Arc, atomic::AtomicBool};
        let worker = TextClipboardWorker::start(|_, _| Err("Injected OpenClipboard failure".into())).unwrap();
        let (actor, operation, reply) = owned_clipboard_request('x');
        let busy = Arc::new(AtomicBool::new(true));
        let (wake, _) = async_channel::bounded(1);
        assert!(worker.submit(TextClipboardJob { owner: None, operation, reply, lease: ClipboardLease(busy), wake }).is_ok());
        let Event::CanvasClipboardError { detail, .. } = actor.events.recv_timeout(std::time::Duration::from_secs(5)).unwrap() else { panic!("Missing write failure"); };
        assert_eq!(detail, "Injected OpenClipboard failure");
        assert!(matches!(actor.events.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), Event::CanvasResult { .. }));
        let preview = actor.take_preview().unwrap();
        assert_eq!(preview.properties.unwrap().text.as_deref(), Some("Keep"));
    }

    #[test]
    fn canvas_clipboard_admission_preserves_fifo_and_rejects_reopened_or_closed_sessions() {
        let requests = std::collections::VecDeque::from([(1, 7, 10, true), (2, 7, 11, true), (3, 7, 12, false)]);
        assert!(super::canvas_clipboard_request_current(&requests, 1, 7, true), "Later input must not revoke an earlier ordered paste");
        assert!(super::canvas_clipboard_request_current(&requests, 2, 7, true));
        assert!(!super::canvas_clipboard_request_current(&requests, 1, 8, true));
        assert!(!super::canvas_clipboard_request_current(&requests, 1, 7, false));
        assert!(!super::canvas_clipboard_request_current(&requests, 3, 7, true));
        assert!(!super::canvas_clipboard_request_current(&requests, 4, 7, true));
    }

    use super::{ClipboardLifetime, LabelDraft, retained_draft_payload};
    use rcr_toolkit::annotation as a;

    #[test]
    fn apply_ack_before_preview_keeps_output_blocked_until_observed_and_reconciled() {
        let mut draft = LabelDraft { target: Some(a::ObjectId(7)), tool: a::Tool::Text,
            baseline: "Alpha".into(), value: "Updated".into(), confirmed_revision: 0,
            pending: Some((17, "Updated".into())), conflict: false };
        assert!(draft.unresolved("Updated", 40));
        // Exact state after a successful Apply ACK, before its preview arrives.
        draft.baseline = draft.pending.take().unwrap().1;
        draft.confirmed_revision = 41;
        assert!(draft.unresolved("Updated", 40), "ACK with an older preview admitted output");
        assert!(!draft.unresolved("Updated", 41), "Matching observed Apply should become clean");
        // A batched Undo is revision 42; reconciliation marks its different text.
        draft.conflict = true;
        assert!(draft.unresolved("Updated", 42), "Newer Undo conflict admitted stale output");
    }

    #[test]
    fn deleted_target_draft_text_is_recoverable_without_object_or_clipboard_access() {
        let target = Some(a::ObjectId(999));
        let draft = LabelDraft { target, tool: a::Tool::Text, baseline: "Old".into(), value: "Unsaved user text".into(),
            confirmed_revision: 0, pending: None, conflict: false };
        let drafts = [draft];
        assert_eq!(retained_draft_payload(&drafts, target, a::Tool::Text, 8, 8).as_deref(), Some("Unsaved user text"));
        assert!(retained_draft_payload(&drafts, target, a::Tool::Text, 7, 8).is_none(), "Old editor session exposed another draft");
        assert!(retained_draft_payload(&drafts, Some(a::ObjectId(1000)), a::Tool::Text, 8, 8).is_none());
        assert!(retained_draft_payload(&drafts, target, a::Tool::Balloon, 8, 8).is_none());
        assert_eq!(drafts[0].value, "Unsaved user text", "Copy payload consumed the retained text");
    }

    #[test]
    fn reopening_does_not_admit_another_clipboard_worker_or_accept_its_old_result() {
        let mut lifetime = ClipboardLifetime::default();
        assert!(lifetime.begin());
        assert!(!lifetime.begin());
        let worker_request = lifetime.render.take().unwrap();
        let old_generation = worker_request.generation;
        lifetime.reset();
        assert!(!lifetime.is_current(old_generation));
        assert!(!lifetime.begin(), "Reopening released an active OS clipboard worker");
        drop(worker_request);
        assert!(lifetime.begin());
        assert!(lifetime.is_current(lifetime.render.as_ref().unwrap().generation));
    }

    #[test]
    fn rejected_or_abandoned_render_does_not_strand_clipboard_admission() {
        let mut lifetime = ClipboardLifetime::default();
        assert!(lifetime.begin());
        lifetime.render.take(); // Actor queue admission failed.
        assert!(lifetime.begin());
        lifetime.reset(); // Old Actor closed before it rendered a copy.
        assert!(lifetime.begin());
    }

    #[test]
    fn a_new_copy_also_rejects_delayed_results_from_the_previous_copy() {
        let mut lifetime = ClipboardLifetime::default();
        assert!(lifetime.begin());
        let previous = lifetime.render.take().unwrap();
        let generation = previous.generation;
        drop(previous); // Worker completed before its UI callback was serviced.
        assert!(lifetime.begin());
        assert!(!lifetime.is_current(generation));
    }

    #[test]
    fn clipboard_worker_unwind_releases_admission() {
        let mut lifetime = ClipboardLifetime::default();
        assert!(lifetime.begin());
        let request = lifetime.render.take().unwrap();
        assert!(std::panic::catch_unwind(move || {
            let _request = request;
            panic!("Synthetic clipboard worker failure");
        }).is_err());
        assert!(lifetime.begin());
    }
}
