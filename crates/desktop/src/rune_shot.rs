//! Region screenshots in place: the monitor under the pointer freezes, a drag selects, and
//! the tools sit right under the selection. Annotate, then copy, save or open in the editor
//! without leaving the overlay. The annotation actor works on the whole frozen monitor, so
//! the selection can still move or resize after drawing; accepting crops to it.
#![cfg_attr(not(windows), allow(dead_code))]
use crate::{
    rune_annotation::{Annotation, tool_icon, tool_name},
    rune_theme::{self as theme, c, icons},
    rune_ui::State as AppState,
};
use rune::{
    Align, AppHandle, Cursor, Cx, DisplayTarget, El, Key, KeyEvent, KeyPolicy, OverlayHandle,
    OverlayInput, OverlayOptions, OverlayPlacement, OverlaySpec, ScreenEdge, Tx, col, el, row,
    text,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static OPEN_CAPTURES: AtomicUsize = AtomicUsize::new(0);

/// Guard quit/restart while a frozen capture or its delivery is still owned.
pub fn shutdown_blocked() -> bool {
    OPEN_CAPTURES.load(Ordering::Acquire) != 0
}
use toolkit::{
    annotation as a,
    image_editor::{Command, Source},
    types::{Frame, Rect},
};

/// Tools offered under the selection, in toolbar order.
const TOOLS: [a::Tool; 11] = [
    a::Tool::Rectangle,
    a::Tool::Ellipse,
    a::Tool::Arrow,
    a::Tool::Line,
    a::Tool::Freehand,
    a::Tool::Text,
    a::Tool::Step,
    a::Tool::Highlight,
    a::Tool::Blur,
    a::Tool::Pixelate,
    a::Tool::Magnify,
];
const BAR_H: f32 = 44.0;
const GAP: f32 = 10.0;
/// Grab distance for selection edges and corners, in points.
const GRAB: f32 = 8.0;

#[derive(Clone, Copy, PartialEq)]
enum Finish {
    Copy,
    Save,
    Edit,
}

#[derive(Clone, Copy)]
enum Gesture {
    /// Dragging out a new selection from this corner (monitor pixels).
    New(f32, f32),
    /// Moving the selection; the grab point and the selection when it started.
    Move(f32, f32, [f32; 4]),
    /// Resizing: which edges move (left, top, right, bottom) and the starting selection.
    Resize([bool; 4], [f32; 4]),
    /// Drawing with the active tool.
    Draw,
}

struct Session {
    annotation: Annotation,
    /// The frozen monitor, drawn under the annotation preview until the first one arrives.
    frozen: rune::Pixels,
    /// Monitor size in pixels.
    size: (f32, f32),
    /// Overlay size in points, from layout.
    view: (f32, f32),
    /// Selection in monitor pixels: x, y, width, height.
    selection: Option<[f32; 4]>,
    gesture: Option<Gesture>,
    /// The drawing tool, or `None` while the pointer moves and resizes the selection.
    tool: Option<a::Tool>,
    viewport_sent: bool,
    bar_width: f32,
    finishing: Option<Finish>,
    folder: PathBuf,
    output_path: Option<PathBuf>,
    choosing_path: bool,
    error: String,
    destination: CaptureDestination,
}

/// Utility callers capture their request/session identity in this callback and reject
/// delivery if that origin has disappeared. Called once, on the main app thread.
pub enum CaptureDestination {
    Library,
    Utility(Box<dyn FnOnce(&mut AppState, PathBuf) -> Result<(), String> + Send>),
}

type Delivery = Result<Option<PathBuf>, String>;

struct DeliveryWorker(std::thread::JoinHandle<Delivery>);
impl Drop for Shot {
    fn drop(&mut self) {
        // The worker never queues UI updates or waits for the UI thread.
        if let Some(worker) = self.worker.take() {
            let _ = worker.0.join();
        }
        if self.session.is_some() {
            OPEN_CAPTURES.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[derive(Default)]
pub struct Shot {
    handle: Option<OverlayHandle<Shot>>,
    app: Option<AppHandle<AppState>>,
    wake: Option<rune::Waker>,
    session: Option<Session>,
    /// Scripted fixture runs wait for the annotation worker each frame, so virtual time
    /// never outruns it.
    patient: bool,
    worker: Option<DeliveryWorker>,
}

/// Adds the overlay, hidden until a region capture starts.
pub fn install(app: &mut rune::App<AppState>) -> OverlayHandle<Shot> {
    let spec = OverlaySpec {
        name: "Kielbox screenshot",
        size: (640.0, 480.0),
        edge: ScreenEdge::Top,
        margin: 0.0,
        align: Align::Center,
        inset: 0.0,
        interactive: true,
        view,
    }
    .options(OverlayOptions {
        placement: Some(OverlayPlacement::FullScreen),
        display: DisplayTarget::UnderPointer,
        keys: KeyPolicy::OnDemand,
        input: OverlayInput::All,
        level: rune::OverlayLevel::AboveOverlays,
        exclude_from_capture: true,
        ..Default::default()
    })
    .on_key(on_key)
    .poll(poll)
    .setup(|s: &mut Shot, wake| s.wake = Some(wake));
    let overlay = app.overlay(spec, Shot::default());
    let handle = overlay.clone();
    let main = app.handle();
    overlay.update(move |s, _| {
        s.handle = Some(handle);
        s.app = Some(main);
    });
    overlay
}

/// The monitor under the pointer, in desktop pixels.
#[cfg(windows)]
pub fn monitor_under_pointer() -> Option<Rect> {
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
        UI::WindowsAndMessaging::GetCursorPos,
    };
    unsafe {
        let mut point = POINT::default();
        GetCursorPos(&mut point).ok()?;
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }
        let r = info.rcMonitor;
        Some(Rect::new(
            r.left,
            r.top,
            (r.right - r.left).max(1) as u32,
            (r.bottom - r.top).max(1) as u32,
        ))
    }
}

/// Freeze `frame` (the monitor under the pointer) and start selecting. Any thread.
pub fn begin(handle: &OverlayHandle<Shot>, frame: Frame, folder: PathBuf) {
    begin_with_destination(handle, frame, folder, CaptureDestination::Library);
}

pub fn begin_for_utility(
    handle: &OverlayHandle<Shot>,
    frame: Frame,
    folder: PathBuf,
    complete: impl FnOnce(&mut AppState, PathBuf) -> Result<(), String> + Send + 'static,
) {
    begin_with_destination(
        handle,
        frame,
        folder,
        CaptureDestination::Utility(Box::new(complete)),
    );
}

fn begin_with_destination(
    handle: &OverlayHandle<Shot>,
    frame: Frame,
    folder: PathBuf,
    destination: CaptureDestination,
) {
    let rgba = frame
        .clone()
        .into_rgba8()
        .map(|f| (f.width(), f.height(), f.data().to_vec()));
    let show = handle.clone();
    handle.update(move |s, tx| {
        if start(s, frame, rgba.ok(), folder) {
            s.session.as_mut().unwrap().destination = destination;
            show.show();
            show.request_keys();
            tx.instant();
        }
    });
}

fn start(s: &mut Shot, frame: Frame, rgba: Option<(u32, u32, Vec<u8>)>, folder: PathBuf) -> bool {
    if s.session.is_some() || s.worker.is_some() {
        notify(s, "Finish or cancel the current screenshot first.".into());
        return false;
    }
    let Some(wake) = s.wake.clone() else {
        return false;
    };
    let size = (frame.width() as f32, frame.height() as f32);
    let frozen = rune::Pixels::new();
    if let Some((w, h, rgba)) = rgba {
        frozen.set(w, h, rgba);
    }
    match Annotation::new(Source::Image(frame), wake) {
        Ok(annotation) => {
            OPEN_CAPTURES.fetch_add(1, Ordering::AcqRel);
            s.session = Some(Session {
                annotation,
                frozen,
                size,
                view: (0.0, 0.0),
                selection: None,
                gesture: None,
                tool: None,
                viewport_sent: false,
                bar_width: 560.0,
                finishing: None,
                folder,
                output_path: None,
                choosing_path: false,
                error: String::new(),
                destination: CaptureDestination::Library,
            });
            true
        }
        Err(e) => {
            notify(s, format!("Screenshot failed: {e:#}"));
            false
        }
    }
}

/// `--shot-fixture`: the overlay as a headless app over `KIELBOX_SHOT_FRAME` (a PNG standing
/// in for the frozen monitor). Saves land in `KIELBOX_FIXTURES/saved`.
pub fn run_fixture() {
    let spec = rune::AppSpec {
        setup: Some(|s: &mut Shot, wake| {
            s.wake = Some(wake);
            s.patient = true;
            let path = std::env::var_os("KIELBOX_SHOT_FRAME").map(PathBuf::from);
            let Some(image) = path.and_then(|p| image::open(p).ok()) else {
                eprintln!("Set KIELBOX_SHOT_FRAME to a PNG.");
                return;
            };
            let rgba = image.to_rgba8();
            let raw = (rgba.width(), rgba.height(), rgba.as_raw().clone());
            let frame = Frame::from_image(rgba);
            let folder = std::env::var_os("KIELBOX_FIXTURES")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join("saved");
            start(s, frame, Some(raw), folder);
        }),
        poll: Some(poll),
        on_key: Some(on_key),
        ..rune::AppSpec::new("Kielbox screenshot", (1200.0, 800.0), Shot::default, view)
    };
    rune::App::new(spec).run();
}

fn notify(s: &Shot, message: String) {
    if let Some(app) = &s.app {
        app.update(move |s, _| s.notice = message);
    }
}

impl Session {
    /// Points per monitor pixel.
    fn k(&self) -> f32 {
        if self.size.0 > 0.0 && self.view.0 > 0.0 {
            self.view.0 / self.size.0
        } else {
            1.0
        }
    }
    fn contains(&self, x: f32, y: f32) -> bool {
        self.selection
            .is_some_and(|[sx, sy, w, h]| x >= sx && y >= sy && x <= sx + w && y <= sy + h)
    }
    /// The edges near a pixel point, as grabbed for resizing.
    fn edges(&self, x: f32, y: f32) -> Option<[bool; 4]> {
        let [sx, sy, w, h] = self.selection?;
        let g = GRAB / self.k();
        let inside_x = x > sx - g && x < sx + w + g;
        let inside_y = y > sy - g && y < sy + h + g;
        let edges = [
            inside_y && (x - sx).abs() < g,
            inside_x && (y - sy).abs() < g,
            inside_y && (x - sx - w).abs() < g,
            inside_x && (y - sy - h).abs() < g,
        ];
        edges.iter().any(|e| *e).then_some(edges)
    }
    fn clamp(&self, r: [f32; 4]) -> [f32; 4] {
        let w = r[2].clamp(1.0, self.size.0);
        let h = r[3].clamp(1.0, self.size.1);
        [
            r[0].clamp(0.0, self.size.0 - w),
            r[1].clamp(0.0, self.size.1 - h),
            w,
            h,
        ]
    }
    fn region(&self) -> Option<Rect> {
        let [x, y, w, h] = self.selection?;
        let r = Rect::new(
            x.round() as i32,
            y.round() as i32,
            w.round().max(1.0) as u32,
            h.round().max(1.0) as u32,
        );
        (r.width >= 2 && r.height >= 2).then_some(r)
    }
    fn choose(&mut self, tool: Option<a::Tool>) {
        self.tool = if self.tool == tool { None } else { tool };
        self.annotation
            .send(Command::Tool(self.tool.unwrap_or(a::Tool::Select)));
    }
}

/// Main-window shutdown/restart integration should defer while this is true.
pub fn busy(s: &Shot) -> bool {
    s.worker.is_some()
        || s.session
            .as_ref()
            .is_some_and(|session| session.finishing.is_some() || session.choosing_path)
}

fn close(s: &mut Shot) {
    if busy(s) {
        return;
    }
    if s.session
        .as_ref()
        .is_some_and(|session| matches!(&session.destination, CaptureDestination::Utility(_)))
    {
        notify(
            s,
            "Screenshot cancelled. No utility input was added.".into(),
        );
    }
    if s.session.take().is_some() {
        OPEN_CAPTURES.fetch_sub(1, Ordering::AcqRel);
    }
    if let Some(h) = &s.handle {
        h.hide();
    }
}

fn finish(s: &mut Shot, how: Finish) {
    let Some(session) = &mut s.session else {
        return;
    };
    let Some(region) = session.region() else {
        return;
    };
    if session.finishing.is_none() {
        session.annotation.message.clear();
        session.error.clear();
        session.finishing = Some(how);
        session.annotation.send(Command::AcceptRegion(region));
    }
}

fn poll(s: &mut Shot, _: f64, tx: &mut Tx) -> bool {
    if s.worker.as_ref().is_some_and(|w| w.0.is_finished()) {
        let result = s
            .worker
            .take()
            .unwrap()
            .0
            .join()
            .unwrap_or_else(|_| Err("Screenshot delivery worker stopped unexpectedly.".into()));
        let Some(session) = s.session.as_mut() else {
            return true;
        };
        let how = session.finishing.take().unwrap_or(Finish::Save);
        match result {
            Ok(path) => {
                let destination =
                    std::mem::replace(&mut session.destination, CaptureDestination::Library);
                if let Some(app) = &s.app {
                    app.update(move |s, _| {
                        if let Some(path) = path {
                            s.refresh();
                            match destination {
                                _ if how == Finish::Edit => {
                                    s.open(path);
                                    if let Some(h) = &s.handle {
                                        h.open();
                                    }
                                }
                                CaptureDestination::Utility(complete) => {
                                    if let Err(e) = complete(s, path.clone()) {
                                        s.notice =
                                            format!("Screenshot saved at {}. {e}", path.display());
                                    }
                                }
                                _ => s.notice = "Screenshot saved.".into(),
                            }
                        } else {
                            s.notice = "Screenshot copied. No file was saved.".into();
                        }
                    });
                }
                close(s);
            }
            Err(e) => {
                session.error = format!(
                    "{e} Your capture and edits are still here. Retry Save, Save elsewhere, or Copy."
                );
                notify(s, s.session.as_ref().unwrap().error.clone());
            }
        }
        return true;
    }
    let Some(session) = &mut s.session else {
        return false;
    };
    if s.patient {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let changed = session.annotation.poll();
    if !session.annotation.message.is_empty() && session.finishing.is_some() && s.worker.is_none() {
        session.error = std::mem::take(&mut session.annotation.message);
        session.finishing = None;
        return true;
    }
    if s.worker.is_none() {
        if let (Some(how), Some(frame)) = (session.finishing, session.annotation.take_accepted()) {
            let folder = session.folder.clone();
            let path = session.output_path.clone();
            match std::thread::Builder::new()
                .name("kielbox-screenshot-delivery".into())
                .spawn(move || deliver(frame, how, folder, path))
            {
                Ok(worker) => s.worker = Some(DeliveryWorker(worker)),
                Err(e) => {
                    session.finishing = None;
                    session.error = format!(
                        "Could not start delivery: {e}. Your capture and edits are still here."
                    );
                }
            }
        }
    }
    if session.finishing.is_some() {
        tx.wake_after(std::time::Duration::from_millis(16));
    }
    changed
}

/// Copy deliberately performs no filesystem operations. Save uses the shared
/// no-clobber publisher; errors leave the original annotation actor alive.
fn deliver(frame: Frame, how: Finish, folder: PathBuf, path: Option<PathBuf>) -> Delivery {
    deliver_with_clipboard(frame, how, folder, path, |frame| {
        use toolkit::core::workflow::Clipboard;
        toolkit::services::SystemClipboard::new(Default::default())
            .set_image(frame)
            .map_err(|e| format!("Clipboard unavailable: {e}"))
    })
}

fn deliver_with_clipboard(
    frame: Frame,
    how: Finish,
    folder: PathBuf,
    path: Option<PathBuf>,
    copy: impl FnOnce(&Frame) -> Result<(), String>,
) -> Delivery {
    if how == Finish::Copy {
        copy(&frame)?;
        return Ok(None);
    }
    let path = match path {
        Some(path) => path,
        None => {
            std::fs::create_dir_all(&folder)
                .map_err(|e| format!("Could not create capture folder: {e}"))?;
            folder.join(format!(
                "screenshot-{}.png",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ))
        }
    };
    toolkit::utilities::save_image(&path, &frame)
        .map_err(|e| format!("Could not save screenshot: {e:#}"))?;
    Ok(Some(path))
}

/// Arrows create/move a region; Alt+arrows resize its right/bottom edge.
fn keyboard_region(
    selection: Option<[f32; 4]>,
    size: (f32, f32),
    dx: f32,
    dy: f32,
    resize: bool,
) -> [f32; 4] {
    let [mut x, mut y, mut w, mut h] =
        selection.unwrap_or([0.0, 0.0, size.0.min(100.0), size.1.min(100.0)]);
    if resize {
        w = (w + dx).clamp(2.0, size.0.max(2.0));
        h = (h + dy).clamp(2.0, size.1.max(2.0));
    } else {
        x += dx;
        y += dy;
    }
    [
        x.clamp(0.0, (size.0 - w).max(0.0)),
        y.clamp(0.0, (size.1 - h).max(0.0)),
        w,
        h,
    ]
}

fn on_key(s: &mut Shot, event: &KeyEvent, tx: &mut Tx) -> bool {
    let Some(session) = &mut s.session else {
        return false;
    };
    if session.finishing.is_some() || session.choosing_path {
        return true;
    }
    if session.annotation.text_editing {
        if matches!(event.key, Key::Escape) {
            session.annotation.send(Command::ClearSelection);
            return true;
        }
        return session.annotation.key(event);
    }
    if session.tool.is_some()
        && matches!(
            event.key,
            Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End | Key::Enter
        )
    {
        return session.annotation.key(event);
    }
    if let Key::Text(text) = &event.key {
        if matches!(text.as_str(), "[" | "]") {
            session.tool = Some(a::Tool::Select);
            session.annotation.send(Command::Tool(a::Tool::Select));
            session
                .annotation
                .send(Command::CycleSelection(text == "["));
            return true;
        }
    }
    match &event.key {
        Key::Escape => {
            if session.tool.is_some() {
                session.choose(None);
            } else {
                close(s);
            }
        }
        Key::Left | Key::Right | Key::Up | Key::Down if session.tool.is_none() => {
            let step = if event.mods.shift { 10.0 } else { 1.0 };
            let (dx, dy) = match event.key {
                Key::Left => (-step, 0.0),
                Key::Right => (step, 0.0),
                Key::Up => (0.0, -step),
                _ => (0.0, step),
            };
            session.selection = Some(keyboard_region(
                session.selection,
                session.size,
                dx,
                dy,
                event.mods.alt,
            ));
        }
        Key::Enter => finish(s, Finish::Save),
        Key::Command('c') => finish(s, Finish::Copy),
        Key::Command('s') if event.mods.shift => save_elsewhere(s, tx),
        Key::Command('s') => finish(s, Finish::Save),
        Key::Command('e') => finish(s, Finish::Edit),
        Key::Command('a') => {
            session.selection = Some([0.0, 0.0, session.size.0, session.size.1]);
        }
        Key::Command('z') => session.annotation.send(if event.mods.shift {
            Command::Redo
        } else {
            Command::Undo
        }),
        Key::Command('y') => session.annotation.send(Command::Redo),
        Key::Delete | Key::Backspace => session.annotation.send(Command::Delete),
        Key::Text(t) => {
            let tool = match t.as_str() {
                "v" => Some(None),
                "r" => Some(Some(a::Tool::Rectangle)),
                "e" | "c" => Some(Some(a::Tool::Ellipse)),
                "a" => Some(Some(a::Tool::Arrow)),
                "l" => Some(Some(a::Tool::Line)),
                "f" | "p" => Some(Some(a::Tool::Freehand)),
                "t" => Some(Some(a::Tool::Text)),
                "n" => Some(Some(a::Tool::Step)),
                "h" => Some(Some(a::Tool::Highlight)),
                "b" => Some(Some(a::Tool::Blur)),
                "x" => Some(Some(a::Tool::Pixelate)),
                "m" => Some(Some(a::Tool::Magnify)),
                _ => None,
            };
            match tool {
                Some(None) => {
                    session.tool = None;
                    session.annotation.send(Command::Tool(a::Tool::Select));
                }
                Some(tool) if session.selection.is_some() => {
                    session.tool = None;
                    session.choose(tool);
                }
                _ => return false,
            }
        }
        _ => return false,
    }
    true
}

fn pointer(s: &mut Shot, d: &mut rune::Drag, tx: &mut Tx) {
    tx.instant();
    let Some(session) = &mut s.session else {
        return;
    };
    if session.finishing.is_some() || session.choosing_path {
        return;
    }
    let k = session.k();
    let (px, py) = (d.x / k, d.y / k);
    let mods = tx.mods();
    let m = a::Modifiers {
        ctrl: mods.cmd,
        shift: mods.shift,
        alt: mods.alt,
    };
    match d.phase {
        rune::Phase::Down => {
            session.gesture = if session.tool.is_some() && session.contains(px, py) {
                let p = session.annotation.image_point(d.x, d.y, (d.w, d.h));
                session.annotation.send(Command::Down(p, m));
                Some(Gesture::Draw)
            } else if session.tool.is_some() {
                None
            } else if let Some(edges) = session.edges(px, py) {
                Some(Gesture::Resize(
                    edges,
                    session.selection.unwrap_or_default(),
                ))
            } else if session.contains(px, py) {
                Some(Gesture::Move(px, py, session.selection.unwrap_or_default()))
            } else {
                session.selection = None;
                Some(Gesture::New(px, py))
            };
        }
        rune::Phase::Move | rune::Phase::Up => {
            let up = d.phase == rune::Phase::Up;
            match session.gesture {
                Some(Gesture::Draw) => {
                    let p = session.annotation.image_point(d.x, d.y, (d.w, d.h));
                    session.annotation.send(if up {
                        Command::Up(p, m)
                    } else {
                        Command::Move(p, m)
                    });
                }
                Some(Gesture::New(x0, y0)) => {
                    let (x1, y1) = (px.clamp(0.0, session.size.0), py.clamp(0.0, session.size.1));
                    let r = [x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs()];
                    session.selection = (r[2] >= 1.0 && r[3] >= 1.0).then_some(r);
                }
                Some(Gesture::Move(gx, gy, r)) => {
                    session.selection =
                        Some(session.clamp([r[0] + px - gx, r[1] + py - gy, r[2], r[3]]));
                }
                Some(Gesture::Resize(edges, r)) => {
                    let (mut l, mut t, mut rr, mut b) = (r[0], r[1], r[0] + r[2], r[1] + r[3]);
                    let (x, y) = (px.clamp(0.0, session.size.0), py.clamp(0.0, session.size.1));
                    if edges[0] {
                        l = x;
                    }
                    if edges[1] {
                        t = y;
                    }
                    if edges[2] {
                        rr = x;
                    }
                    if edges[3] {
                        b = y;
                    }
                    session.selection = Some([
                        l.min(rr),
                        t.min(b),
                        (rr - l).abs().max(1.0),
                        (b - t).abs().max(1.0),
                    ]);
                }
                None => {}
            }
            if up {
                session.gesture = None;
                if session.region().is_none() && session.tool.is_none() {
                    session.selection = None;
                }
            }
        }
    }
}

fn icon_button(
    key: String,
    label: String,
    icon: &'static rune::Icon,
    on: bool,
    press: impl Fn(&mut Shot) + 'static,
) -> El {
    let mut b = el()
        .key(key)
        .role("button")
        .label(label)
        .checked(on)
        .focusable(true)
        .size(32.0, 32.0)
        .radius(7.0)
        .center()
        .cursor(Cursor::Pointer)
        .hover(theme::white(0.08))
        .on_press(move |s: &mut Shot| press(s))
        .child(theme::glyph(
            icon,
            17.0,
            c(if on { theme::INK } else { theme::T1 }),
        ));
    if on {
        b = b.fill(c(theme::ACCENT)).hover(c(theme::ACCENT_HOVER));
    }
    b
}

fn text_button(
    key: &str,
    label: &str,
    icon: &'static rune::Icon,
    primary: bool,
    how: Finish,
) -> El {
    let fg = c(if primary { theme::INK } else { theme::T1 });
    let mut b = row()
        .key(key.to_string())
        .role("button")
        .label(label.to_string())
        .focusable(true)
        .h(32.0)
        .px(10.0)
        .gap(6.0)
        .radius(7.0)
        .center_y()
        .cursor(Cursor::Pointer)
        .on_press(move |s: &mut Shot| finish(s, how))
        .child(theme::glyph(icon, 15.0, fg))
        .child(
            text(label.to_string())
                .font(theme::UI)
                .color(fg)
                .single_line(),
        );
    b = if primary {
        b.fill(c(theme::ACCENT)).hover(c(theme::ACCENT_HOVER))
    } else {
        b.hover(theme::white(0.08))
    };
    b
}

fn divider() -> El {
    el().w(1.0).h(20.0).mx(4.0).fill(theme::white(0.14))
}

fn save_elsewhere_button() -> El {
    el().role("button")
        .label("Save screenshot elsewhere (Ctrl+Shift+S)")
        .focusable(true)
        .h(32.0)
        .px(10.0)
        .cursor(Cursor::Pointer)
        .hover(theme::white(0.08))
        .child(theme::small("Save elsewhere…"))
        .on_press_tx(save_elsewhere)
}

fn save_elsewhere(s: &mut Shot, tx: &mut Tx) {
    if busy(s) {
        return;
    }
    if let Some(session) = &mut s.session {
        session.choosing_path = true;
    }
    tx.save_file(
        rune::FileDialogOptions::default().filter("PNG", &["png"]),
        |s: &mut Shot, result, _| {
            if let Some(session) = &mut s.session {
                session.choosing_path = false;
            }
            match result {
                Ok(Some(path)) => {
                    if let Some(session) = &mut s.session {
                        session.output_path = Some(path);
                    }
                    finish(s, Finish::Save);
                }
                Err(e) => {
                    if let Some(session) = &mut s.session {
                        session.error = format!("Could not choose destination: {e}");
                    }
                }
                _ => {}
            }
        },
    );
}

fn toolbar(session: &Session) -> El {
    let mut bar = row()
        .key("shot-toolbar")
        .role("toolbar")
        .label("Screenshot tools")
        .h(BAR_H)
        .px(6.0)
        .gap(2.0)
        .center_y()
        .on_layout(|s: &mut Shot, r| {
            if let Some(session) = &mut s.session
                && (session.bar_width - r[2]).abs() > 0.5
            {
                session.bar_width = r[2];
            }
        })
        // Presses between buttons stay on the bar, not the selection under it.
        .on_press(|_: &mut Shot| {});
    bar = bar.child(icon_button(
        "shot-move".into(),
        "Move and resize the selection (V)".into(),
        &icons::SELECT,
        session.tool.is_none(),
        |s| {
            if busy(s) {
                return;
            }
            if let Some(session) = &mut s.session {
                session.tool = None;
                session.annotation.send(Command::Tool(a::Tool::Select));
            }
        },
    ));
    for tool in TOOLS {
        bar = bar.child(icon_button(
            format!("shot-{tool:?}"),
            tool_name(tool),
            tool_icon(tool),
            session.tool == Some(tool),
            move |s| {
                if busy(s) {
                    return;
                }
                if let Some(session) = &mut s.session {
                    session.choose(Some(tool));
                }
            },
        ));
    }
    bar.child(divider())
        .child(icon_button(
            "shot-undo".into(),
            "Undo (Ctrl+Z)".into(),
            &icons::UNDO,
            false,
            |s| {
                if busy(s) {
                    return;
                }
                if let Some(session) = &mut s.session {
                    session.annotation.send(Command::Undo);
                }
            },
        ))
        .child(icon_button(
            "shot-redo".into(),
            "Redo (Ctrl+Y)".into(),
            &icons::REDO,
            false,
            |s| {
                if busy(s) {
                    return;
                }
                if let Some(session) = &mut s.session {
                    session.annotation.send(Command::Redo);
                }
            },
        ))
        .child(divider())
        .child(text_button(
            "shot-edit",
            "Edit",
            &icons::EXTERNAL,
            false,
            Finish::Edit,
        ))
        .child(text_button(
            "shot-save",
            if !session.error.is_empty() {
                "Retry Save"
            } else if matches!(&session.destination, CaptureDestination::Utility(_)) {
                "Use capture"
            } else {
                "Save"
            },
            &icons::DOWNLOAD,
            false,
            Finish::Save,
        ))
        .child(text_button(
            "shot-copy",
            "Copy",
            &icons::CLIPBOARD,
            true,
            Finish::Copy,
        ))
        .child(icon_button(
            "shot-close".into(),
            "Cancel (Esc)".into(),
            &icons::CLOSE,
            false,
            close,
        ))
}

pub fn view(s: &Shot, _: &mut Cx<Shot>) -> El {
    let root = el()
        .key("shot")
        .w_full()
        .h_full()
        .on_layout(|s: &mut Shot, r| {
            if let Some(session) = &mut s.session {
                session.view = (r[2], r[3]);
                if !session.viewport_sent && r[2] > 0.0 {
                    session.viewport_sent = true;
                    let (w, h) = (session.size.0 as u32, session.size.1 as u32);
                    session.annotation.send(Command::Viewport(w, h));
                }
            }
        });
    let Some(session) = &s.session else {
        return root;
    };
    let (vw, vh) = session.view;
    let k = session.k();
    let mut root = root
        .cursor(match session.tool {
            Some(a::Tool::Text) => Cursor::Text,
            Some(_) => Cursor::Crosshair,
            None if session.selection.is_none() => Cursor::Crosshair,
            None => Cursor::Default,
        })
        .on_drag(pointer)
        .child(
            el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .pixels_fit(&session.frozen, rune::SurfaceFit::Fit),
        )
        .child(
            el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .native_surface(&session.annotation.surface, rune::SurfaceFit::Fit),
        );
    let dim = theme::black(0.5);
    let Some([sx, sy, sw, sh]) = session.selection.map(|r| r.map(|v| v * k)) else {
        return root
            .child(
                el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                    .fill(dim),
            )
            .child(
                row().pin(Some(0.0), Some(28.0), Some(0.0), None).justify_center().items_start().child(
                    theme::floating(row().px(14.0).py(8.0).gap(10.0).center_y())
                        .child(theme::glyph(&icons::REGION, 16.0, c(theme::ACCENT)))
                        .child(theme::small("Drag or use arrows to select. Shift: 10 px. Alt+arrows: resize.").color(c(theme::T1)))
                        .child(theme::kbd("Ctrl A"))
                        .child(theme::small("whole screen"))
                        .child(theme::kbd("Esc"))
                        .child(theme::small("cancel")),
                ),
            );
    };
    // Dim everything outside the selection.
    root = root
        .child(el().absolute(0.0, 0.0).size(vw, sy.max(0.0)).fill(dim))
        .child(
            el().absolute(0.0, sy + sh)
                .size(vw, (vh - sy - sh).max(0.0))
                .fill(dim),
        )
        .child(el().absolute(0.0, sy).size(sx.max(0.0), sh).fill(dim))
        .child(
            el().absolute(sx + sw, sy)
                .size((vw - sx - sw).max(0.0), sh)
                .fill(dim),
        )
        .child(
            el().absolute(sx - 1.0, sy - 1.0)
                .size(sw + 2.0, sh + 2.0)
                .border(1.0, c(theme::ACCENT))
                .a11y_hidden(),
        );
    if session.tool.is_none() {
        for (hx, hy) in [
            (sx, sy),
            (sx + sw / 2.0, sy),
            (sx + sw, sy),
            (sx, sy + sh / 2.0),
            (sx + sw, sy + sh / 2.0),
            (sx, sy + sh),
            (sx + sw / 2.0, sy + sh),
            (sx + sw, sy + sh),
        ] {
            root = root.child(
                el().absolute(hx - 4.0, hy - 4.0)
                    .size(8.0, 8.0)
                    .radius(2.0)
                    .fill(c(theme::T1))
                    .border(1.0, c(theme::ACCENT))
                    .a11y_hidden(),
            );
        }
    }
    // The size, just above the selection's corner (inside it when there's no room).
    let [_, _, pw, ph] = session.selection.unwrap_or_default();
    let label_y = if sy >= 30.0 { sy - 28.0 } else { sy + 6.0 };
    root = root.child(
        text(format!("{} × {} · Arrows move region · Alt resizes · Tool + Enter draws · [ / ] select annotations", pw.round(), ph.round()))
            .font(theme::NUM)
            .color(c(theme::T1))
            .px(7.0)
            .py(2.0)
            .radius(5.0)
            .fill(theme::black(0.7))
            .absolute(sx.max(4.0), label_y)
            .label(format!("Region width {} height {} pixels. Arrows move; Alt arrows resize; Shift increases step.", pw.round(), ph.round())),
    );
    if session.gesture.is_some_and(|g| !matches!(g, Gesture::Draw)) && session.tool.is_none() {
        return root;
    }
    // The toolbar: under the selection, else above it, else inside its bottom edge.
    let bw = session.bar_width;
    let bx = (sx + sw / 2.0 - bw / 2.0).clamp(8.0, (vw - bw - 8.0).max(8.0));
    let by = if sy + sh + GAP + BAR_H <= vh - 8.0 {
        sy + sh + GAP
    } else if sy - GAP - BAR_H >= 8.0 {
        sy - GAP - BAR_H
    } else {
        (sy + sh - GAP - BAR_H).max(8.0)
    };
    root = root.child(theme::floating(toolbar(session)).absolute(bx, by));
    if !session.error.is_empty() {
        root = root.child(
            theme::floating(
                col()
                    .px(14.0)
                    .py(10.0)
                    .child(
                        theme::small(session.error.clone())
                            .color(c(theme::T1))
                            .role("status"),
                    )
                    .child(save_elsewhere_button()),
            )
            .absolute(16.0, 16.0),
        );
    }
    if session.finishing.is_some() {
        root = root.child(
            col().absolute(sx, sy).size(sw, sh).center().child(
                theme::small(if session.finishing == Some(Finish::Copy) {
                    "Copying…"
                } else {
                    "Saving…"
                })
                .color(c(theme::T1)),
            ),
        );
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit::image_editor::{Actor, Event};

    fn frame() -> Frame {
        Frame::from_image(image::RgbaImage::from_pixel(
            32,
            24,
            image::Rgba([20, 40, 60, 255]),
        ))
    }

    #[test]
    fn clipboard_success_and_failure_never_touch_disk() {
        let temp = tempfile::tempdir().unwrap();
        let blocked_folder = temp.path().join("a-file");
        std::fs::write(&blocked_folder, b"unchanged").unwrap();
        let output = temp.path().join("must-not-exist.png");
        let result = deliver_with_clipboard(
            frame(),
            Finish::Copy,
            blocked_folder.clone(),
            Some(output.clone()),
            |_| Ok(()),
        );
        assert_eq!(result.unwrap(), None);
        let result = deliver_with_clipboard(
            frame(),
            Finish::Copy,
            blocked_folder.clone(),
            Some(output.clone()),
            |_| Err("clipboard busy".into()),
        );
        assert_eq!(result.unwrap_err(), "clipboard busy");
        assert!(!output.exists());
        assert_eq!(std::fs::read(blocked_folder).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    fn accepted(actor: &Actor, rect: Rect) -> Frame {
        actor.send(Command::AcceptRegion(rect)).unwrap();
        loop {
            match actor
                .events
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap()
            {
                Event::Accepted(frame) => return frame,
                Event::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
    }

    #[test]
    fn failed_save_preserves_annotation_actor_for_retry_elsewhere() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("existing.png");
        std::fs::write(&existing, b"keep existing file").unwrap();
        let original = frame();
        let actor = Actor::new(Source::Image(original.clone())).unwrap();
        actor.send(Command::Tool(a::Tool::Rectangle)).unwrap();
        actor
            .send(Command::Down(
                a::PointF::new(4.0, 4.0),
                a::Modifiers::default(),
            ))
            .unwrap();
        actor
            .send(Command::Up(
                a::PointF::new(20.0, 18.0),
                a::Modifiers::default(),
            ))
            .unwrap();
        let rect = Rect::new(0, 0, 32, 24);
        let edited = accepted(&actor, rect);
        assert_ne!(edited.data(), original.data());
        assert!(
            deliver(
                edited.clone(),
                Finish::Save,
                temp.path().into(),
                Some(existing.clone())
            )
            .is_err()
        );
        assert_eq!(std::fs::read(existing).unwrap(), b"keep existing file");
        let retry = accepted(&actor, rect);
        assert_eq!(retry.data(), edited.data());
        let elsewhere = temp.path().join("elsewhere.png");
        assert_eq!(
            deliver(
                retry,
                Finish::Save,
                temp.path().into(),
                Some(elsewhere.clone())
            )
            .unwrap(),
            Some(elsewhere.clone())
        );
        assert_eq!(
            image::open(elsewhere)
                .unwrap()
                .to_rgba8()
                .as_raw()
                .as_slice(),
            edited.data()
        );
    }

    #[test]
    fn keyboard_region_can_be_created_moved_and_resized_within_monitor() {
        let first = keyboard_region(None, (640.0, 480.0), 1.0, 0.0, false);
        assert_eq!(first, [1.0, 0.0, 100.0, 100.0]);
        let edge = keyboard_region(Some(first), (640.0, 480.0), 1000.0, -10.0, false);
        assert_eq!(edge, [540.0, 0.0, 100.0, 100.0]);
        let resized = keyboard_region(Some(edge), (640.0, 480.0), 10.0, -1000.0, true);
        assert_eq!(resized, [530.0, 0.0, 110.0, 2.0]);
    }

    #[test]
    fn dropping_shot_joins_owned_delivery() {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_done = done.clone();
        let worker = std::thread::spawn(move || {
            worker_done.store(true, Ordering::Release);
            Ok(None)
        });
        let mut shot = Shot::default();
        shot.worker = Some(DeliveryWorker(worker));
        drop(shot);
        assert!(done.load(Ordering::Acquire));
    }
}
