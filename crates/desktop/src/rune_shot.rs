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
use std::path::PathBuf;
use toolkit::{
    annotation as a,
    image_editor::Command,
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
    bar_width: f32,
    finishing: Option<Finish>,
    folder: PathBuf,
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
    let rgba = frame
        .clone()
        .into_rgba8()
        .map(|f| (f.width(), f.height(), f.data().to_vec()));
    let show = handle.clone();
    handle.update(move |s, tx| {
        if start(s, frame, rgba.ok(), folder) {
            show.show();
            show.request_keys();
            tx.instant();
        }
    });
}

fn start(s: &mut Shot, frame: Frame, rgba: Option<(u32, u32, Vec<u8>)>, folder: PathBuf) -> bool {
    let Some(wake) = s.wake.clone() else {
        return false;
    };
    let size = (frame.width() as f32, frame.height() as f32);
    let frozen = rune::Pixels::new();
    if let Some((w, h, rgba)) = rgba {
        frozen.set(w, h, rgba);
    }
    match Annotation::desktop(frame, wake) {
        Ok(annotation) => {
            s.session = Some(Session {
                annotation,
                frozen,
                size,
                view: (0.0, 0.0),
                selection: None,
                gesture: None,
                tool: None,
                bar_width: 560.0,
                finishing: None,
                folder,
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

fn close(s: &mut Shot) {
    s.session = None;
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
        session.finishing = Some(how);
        session.annotation.send(Command::AcceptRegion(region));
    }
}

fn poll(s: &mut Shot, _: f64, tx: &mut Tx) -> bool {
    let Some(session) = &mut s.session else {
        return false;
    };
    if s.patient {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let changed = session.annotation.poll();
    if !session.annotation.message.is_empty() && session.finishing.is_some() {
        let message = std::mem::take(&mut session.annotation.message);
        session.finishing = None;
        notify(s, message);
        return true;
    }
    if let (Some(how), Some(frame)) = (session.finishing, session.annotation.take_accepted()) {
        let folder = session.folder.clone();
        let app = s.app.clone();
        close(s);
        let worker = std::thread::spawn(move || deliver(frame, how, folder, app));
        if s.patient {
            let _ = worker.join();
        }
        return true;
    }
    if session.finishing.is_some() {
        tx.wake_after(std::time::Duration::from_millis(16));
    }
    changed
}

/// Save the capture to the library, copy it when asked, and tell the app.
fn deliver(frame: Frame, how: Finish, folder: PathBuf, app: Option<AppHandle<AppState>>) {
    let result = (|| -> Result<(PathBuf, Option<String>), String> {
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let path = folder.join(format!(
            "screenshot-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        toolkit::utilities::save_image(&path, &frame).map_err(|e| format!("{e:#}"))?;
        let copied = if how == Finish::Copy {
            use toolkit::core::workflow::Clipboard;
            Some(
                match toolkit::services::SystemClipboard::new(Default::default()).set_image(&frame)
                {
                    Ok(()) => "Screenshot copied and saved to the library.".into(),
                    Err(e) => format!("Saved, but the clipboard is unavailable: {e}"),
                },
            )
        } else {
            None
        };
        Ok((path, copied))
    })();
    let Some(app) = app else {
        return;
    };
    app.update(move |s, _| match result {
        Ok((path, copied)) => {
            s.refresh();
            if how == Finish::Edit {
                s.open(path);
                if let Some(h) = &s.handle {
                    h.open();
                }
            } else {
                s.notice = copied.unwrap_or_else(|| "Screenshot saved to the library.".into());
            }
        }
        Err(e) => s.notice = format!("Screenshot could not be saved: {e}"),
    });
}

fn on_key(s: &mut Shot, event: &KeyEvent, _: &mut Tx) -> bool {
    let Some(session) = &mut s.session else {
        return false;
    };
    if session.annotation.text_editing {
        if matches!(event.key, Key::Escape) {
            session.annotation.send(Command::ClearSelection);
            return true;
        }
        return session.annotation.key(event);
    }
    match &event.key {
        Key::Escape => {
            if session.tool.is_some() {
                session.choose(None);
            } else {
                close(s);
            }
        }
        Key::Enter => finish(s, Finish::Save),
        Key::Command('c') => finish(s, Finish::Copy),
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
    if session.finishing.is_some() {
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
            "Save",
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
                        .child(theme::small("Drag to capture an area").color(c(theme::T1)))
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
        text(format!("{} × {}", pw.round(), ph.round()))
            .font(theme::NUM)
            .color(c(theme::T1))
            .px(7.0)
            .py(2.0)
            .radius(5.0)
            .fill(theme::black(0.7))
            .absolute(sx.max(4.0), label_y)
            .a11y_hidden(),
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
    if session.finishing.is_some() {
        root = root.child(
            col()
                .absolute(sx, sy)
                .size(sw, sh)
                .center()
                .child(theme::small("Saving…").color(c(theme::T1))),
        );
    }
    root
}
