//! A transient desktop picker shared by screenshots and independent screencasts.
use super::menu_proof::Probe;
use crate::{image_editor::frame_image, theme::*};
use gpui::{prelude::*, *};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use recording_tools::source::CaptureTarget;
use ::toolkit::{
    core::workflow::{CancelToken, Captured},
    selection::{Point as PixelPoint, Selection},
    types::{Frame, Monitor, Rect, WindowInfo},
};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Settings {
    pub video: bool,
    pub cursor: bool,
    pub sound: bool,
    pub microphone: bool,
    pub fps: u32,
    pub countdown: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            video: false,
            cursor: false,
            sound: false,
            microphone: false,
            fps: 30,
            countdown: false,
        }
    }
}
pub(crate) struct Desktop {
    pub frame: Frame,
    pub with_cursor: Frame,
    pub monitors: Vec<Monitor>,
    pub windows: Vec<WindowInfo>,
}
/// Pixel copies and RGBA→BGRA conversion belong on the snapshot worker. Only
/// window construction and image handles cross into the GPUI callback.
pub(crate) struct PreparedDesktop {
    desktop: Desktop,
    image: Arc<RenderImage>,
    cursor_image: Arc<RenderImage>,
}
impl PreparedDesktop {
    pub(crate) fn new(desktop: Desktop, cancel: &CancelToken) -> anyhow::Result<Self> {
        cancel.check()?;
        let image = frame_image(desktop.frame.clone())
            .ok_or_else(|| anyhow::anyhow!("Could not prepare screenshot preview"))?;
        cancel.check()?;
        let cursor_image = frame_image(desktop.with_cursor.clone())
            .ok_or_else(|| anyhow::anyhow!("Could not prepare cursor preview"))?;
        cancel.check()?;
        Ok(Self { desktop, image, cursor_image })
    }
}
pub(crate) struct Choice {
    pub settings: Settings,
    pub target: CaptureTarget,
    pub captured: Option<Captured>,
}
pub(crate) struct Host {
    pub hidden: bool,
    pub allow_video: bool,
    pub probe: Option<Probe>,
}
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kind {
    Selection,
    Screen,
    Window,
}
pub(crate) struct Picker {
    desktop: Desktop,
    image: Option<Arc<RenderImage>>,
    cursor_image: Option<Arc<RenderImage>>,
    settings: Settings,
    kind: Kind,
    selection: Selection,
    selected: Option<usize>,
    focus: FocusHandle,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    reply: Option<mpsc::SyncSender<Option<Choice>>>,
    cancel: CancelToken,
    allow_video: bool,
    more: bool,
    notice: String,
    deadline: Option<Instant>,
    probe: Option<Probe>,
}
pub(crate) fn open(
    prepared: PreparedDesktop,
    settings: Settings,
    reply: mpsc::SyncSender<Option<Choice>>,
    cancel: CancelToken,
    host: Host,
    cx: &mut App,
) -> anyhow::Result<AnyWindowHandle> {
    let PreparedDesktop { desktop, image, cursor_image } = prepared;
    let Host {
        hidden,
        allow_video,
        probe,
    } = host;
    let scale = desktop.frame.scale_factor.max(0.5) as f32;
    let physical = desktop.frame.rect();
    let bounds = Bounds::new(
        point(px(physical.x as f32 / scale), px(physical.y as f32 / scale)),
        size(
            px(physical.width as f32 / scale),
            px(physical.height as f32 / scale),
        ),
    );
    let handle = cx.open_window(
        WindowOptions {
            show: !hidden,
            focus: !hidden,
            titlebar: None,
            // Hidden popup HWNDs can report a 1×1 client area on Windows. The
            // generated-content fixture uses an ordinary hidden host instead.
            kind: if hidden {
                WindowKind::Normal
            } else {
                WindowKind::PopUp
            },
            is_resizable: hidden,
            is_minimizable: false,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window);
                let mut selection = Selection::new(physical.width, physical.height, false);
                // Start with handles around a useful area on the primary display.
                let monitor = desktop
                    .monitors
                    .iter()
                    .find(|m| m.primary)
                    .or(desktop.monitors.first())
                    .map(|m| m.rect)
                    .unwrap_or(physical);
                let rect = monitor.intersect(physical).unwrap_or(physical);
                selection.rect = Some(Rect::new(
                    rect.x - physical.x + (rect.width / 5) as i32,
                    rect.y - physical.y + (rect.height / 5) as i32,
                    (rect.width * 3 / 5).max(1),
                    (rect.height * 2 / 5).max(1),
                ));
                cx.spawn_in(window, async move |view, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                        if view
                            .update_in(cx, |this: &mut Picker, window, cx| {
                                if this.cancel.is_cancelled() {
                                    this.dismiss(window);
                                } else if this.deadline.is_some_and(|at| Instant::now() >= at) {
                                    this.publish(window);
                                } else if this.deadline.is_some() {
                                    cx.notify();
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
                Picker {
                    image: Some(image),
                    cursor_image: Some(cursor_image),
                    desktop,
                    settings,
                    kind: Kind::Selection,
                    selection,
                    selected: None,
                    focus,
                    bounds: Rc::new(Cell::new(None)),
                    reply: Some(reply),
                    cancel,
                    allow_video,
                    more: false,
                    notice: String::new(),
                    deadline: None,
                    probe,
                }
            })
        },
    )?;
    if !hidden {
        let positioned = handle.update(cx, |_, window, _| -> anyhow::Result<()> {
            let RawWindowHandle::Win32(raw) = HasWindowHandle::window_handle(window)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .as_raw()
            else {
                anyhow::bail!("Expected Windows capture overlay");
            };
            use windows::Win32::{
                Foundation::HWND,
                UI::WindowsAndMessaging::{HWND_TOPMOST, SWP_SHOWWINDOW, SetWindowPos},
            };
            unsafe {
                SetWindowPos(
                    HWND(raw.hwnd.get() as *mut _),
                    Some(HWND_TOPMOST),
                    physical.x,
                    physical.y,
                    i32::try_from(physical.width)?,
                    i32::try_from(physical.height)?,
                    SWP_SHOWWINDOW,
                )?;
            }
            Ok(())
        })?;
        if let Err(error) = positioned {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            return Err(error);
        }
    }
    Ok(handle.into())
}
impl Picker {
    // Text buttons insert a text node even for an empty label. Use a dedicated
    // primitive for icon controls so padding/gaps cannot offset their contents.
    fn control(&self, id: impl Into<ElementId>, children: &[&'static str]) -> Stateful<Div> {
        self.row(children)
            .id(id)
            .tab_index(0)
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .min_w_0()
            .p_0()
            .gap_0()
            .border_1()
            .border_color(rgba(0x00000000))
            .focus(|s| s.border_color(rgb(ACCENT)))
            .rounded_lg()
            .bg(rgb(0x242529))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(0x3a3b42)))
    }
    fn row(&self, keys: &[&'static str]) -> Div {
        let keys = keys.to_vec();
        let probe = self.probe.clone();
        div().on_children_prepainted(move |bounds, _, _| {
            if let Some(probe) = &probe {
                for (key, bounds) in keys.iter().zip(bounds) {
                    if !key.is_empty() {
                        probe.borrow_mut().insert(key, bounds);
                    }
                }
            }
        })
    }
    fn point(&self, point: Point<Pixels>) -> Option<PixelPoint> {
        let bounds = self.bounds.get()?;
        Some(PixelPoint {
            x: (f32::from(point.x - bounds.left()) / f32::from(bounds.size.width).max(1.)
                * self.desktop.frame.width() as f32)
                .clamp(0., self.desktop.frame.width() as f32),
            y: (f32::from(point.y - bounds.top()) / f32::from(bounds.size.height).max(1.)
                * self.desktop.frame.height() as f32)
                .clamp(0., self.desktop.frame.height() as f32),
        })
    }
    fn physical(&self, p: PixelPoint) -> (i32, i32) {
        (
            self.desktop.frame.origin.x + p.x as i32,
            self.desktop.frame.origin.y + p.y as i32,
        )
    }
    fn hit(&mut self, p: PixelPoint) {
        let (x, y) = self.physical(p);
        let contains = |r: Rect| {
            x >= r.x
                && y >= r.y
                && i64::from(x) < i64::from(r.x) + i64::from(r.width)
                && i64::from(y) < i64::from(r.y) + i64::from(r.height)
        };
        self.selected = match self.kind {
            Kind::Screen => self.desktop.monitors.iter().position(|m| contains(m.rect)),
            Kind::Window => self.desktop.windows.iter().position(|w| contains(w.rect)),
            Kind::Selection => None,
        };
    }
    fn choose(&mut self, kind: Kind) {
        self.kind = kind;
        self.notice.clear();
        self.more = false;
        self.selected = match kind {
            Kind::Screen => self
                .desktop
                .monitors
                .iter()
                .position(|m| m.primary)
                .or_else(|| (!self.desktop.monitors.is_empty()).then_some(0)),
            Kind::Window => self
                .desktop
                .windows
                .iter()
                .position(|w| w.focused)
                .or_else(|| (!self.desktop.windows.is_empty()).then_some(0)),
            Kind::Selection => None,
        };
    }
    pub(crate) fn selected_rect(&self) -> Option<Rect> {
        let full = self.desktop.frame.rect();
        let rect = match self.kind {
            Kind::Selection => self
                .selection
                .rect
                .map(|r| Rect::new(full.x + r.x, full.y + r.y, r.width, r.height)),
            Kind::Screen => self
                .selected
                .and_then(|i| self.desktop.monitors.get(i))
                .map(|m| m.rect),
            Kind::Window => self
                .selected
                .and_then(|i| self.desktop.windows.get(i))
                .map(|w| w.rect),
        }?;
        rect.intersect(full)
    }
    fn video_fits(&self) -> bool {
        self.selected_rect().is_some_and(|r| {
            self.kind != Kind::Selection
                || self
                    .desktop
                    .monitors
                    .iter()
                    .any(|m| m.rect.intersect(r) == Some(r))
        })
    }
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.deadline.is_some() {
            return;
        }
        if self.selected_rect().is_none() {
            self.notice = "Choose something to capture.".into();
        } else if self.settings.video && (!self.allow_video || !self.video_fits()) {
            self.notice = "Keep a video selection inside one screen.".into();
        } else if self.settings.video && self.settings.countdown {
            self.deadline = Some(Instant::now() + Duration::from_secs(3));
        } else {
            self.publish(window);
        }
        cx.notify();
    }
    fn publish(&mut self, window: &mut Window) {
        let Some(rect) = self.selected_rect() else {
            return;
        };
        let target = match self.kind {
            Kind::Selection => CaptureTarget::Region(rect),
            Kind::Screen => {
                CaptureTarget::Monitor(self.desktop.monitors[self.selected.unwrap()].id.clone())
            }
            Kind::Window => {
                CaptureTarget::Window(self.desktop.windows[self.selected.unwrap()].id.clone())
            }
        };
        let captured = if self.settings.video || self.kind == Kind::Window {
            None
        } else {
            let source = if self.settings.cursor {
                &self.desktop.with_cursor
            } else {
                &self.desktop.frame
            };
            match source.crop_desktop(rect) {
                Ok(frame) => {
                    let info = if self.kind == Kind::Window {
                        self.selected.and_then(|i| self.desktop.windows.get(i))
                    } else {
                        None
                    };
                    Some(Captured {
                        frame,
                        window_title: info.map(|w| w.title.clone()),
                        process_name: info.and_then(|w| w.app_name.clone()),
                    })
                }
                Err(error) => {
                    self.notice = error.to_string();
                    return;
                }
            }
        };
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(Some(Choice {
                settings: self.settings,
                target,
                captured,
            }));
        }
        window.remove_window();
    }
    fn dismiss(&mut self, window: &mut Window) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(None);
        }
        window.remove_window();
    }
    fn toolbar(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut root = div()
            .id("desktop-capture-toolbar")
            .w(px(440.))
            .flex_shrink_0()
            .p_3()
            .rounded(px(22.))
            .bg(rgb(0x242529))
            .border_1()
            .border_color(rgb(0x55565c))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_2()
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation());
        if let Some(deadline) = self.deadline {
            return root.child(
                self.row(&["", "cancel"])
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(format!(
                        "Recording in {}…",
                        deadline.saturating_duration_since(Instant::now()).as_secs() + 1
                    ))
                    .child(
                        button("picker-cancel-countdown", "Cancel")
                            .on_click(cx.listener(|this, _, w, _| this.dismiss(w))),
                    ),
            );
        }
        if self.more && self.settings.video {
            root = root.child(
                self.row(&["", "fps30", "fps60", "countdown"])
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .pb_2()
                    .child(muted("Video"))
                    .children([30, 60].into_iter().map(|fps| {
                        button(("picker-fps", fps as usize), format!("{fps} FPS"))
                            .bg(rgb(if self.settings.fps == fps {
                                0x51525b
                            } else {
                                0x303136
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settings.fps = fps;
                                cx.notify();
                            }))
                    }))
                    .child(
                        button(
                            "picker-delay",
                            if self.settings.countdown {
                                "Delay: 3s"
                            } else {
                                "No delay"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.countdown = !this.settings.countdown;
                            cx.notify();
                        })),
                    ),
            );
        }
        let mut targets = self
            .row(&["region", "screen", "window"])
            .w_full()
            .flex()
            .gap_2();
        for (kind, key, label, glyph) in [
            (Kind::Selection, "region", "Selection", "capture-area"),
            (Kind::Screen, "screen", "Screen", "monitor"),
            (Kind::Window, "window", "Window", "capture-window"),
        ] {
            let parts = match kind {
                Kind::Selection => ["region-glyph", "region-label"],
                Kind::Screen => ["screen-glyph", "screen-label"],
                Kind::Window => ["window-glyph", "window-label"],
            };
            targets = targets.child(
                self.control(key, &parts)
                    .flex_1()
                    .min_w_0()
                    .h(px(72.))
                    .flex_col()
                    .gap_1()
                    .bg(rgb(if self.kind == kind {
                        0x4b4c54
                    } else {
                        0x242529
                    }))
                    .rounded_lg()
                    .child(icon(glyph).size(px(24.)).text_color(rgb(TEXT)))
                    .child(
                        div()
                            .h(px(16.))
                            .line_height(px(16.))
                            .text_size(px(12.))
                            .child(label),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose(kind);
                        cx.notify();
                    })),
            );
        }
        root = root.child(targets);
        let mut modes = self
            .row(&["mode-screenshot", "mode-record"])
            .flex_1()
            .min_w_0()
            .h(px(56.))
            .flex()
            .items_center()
            .justify_center()
            .gap_2();
        for (video, key, glyph) in [
            (false, "mode-screenshot", "camera"),
            (true, "mode-record", "video-camera"),
        ] {
            modes = modes.child(
                self.control(key, &[if video { "video-glyph" } else { "photo-glyph" }])
                    .size(px(40.))
                    .rounded_lg()
                    .bg(rgb(if self.settings.video == video {
                        0x51525b
                    } else {
                        0x242529
                    }))
                    .opacity(if video && !self.allow_video { 0.3 } else { 1. })
                    .child(icon(glyph).size(px(20.)).text_color(rgb(TEXT)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !video || this.allow_video {
                            this.settings.video = video;
                            this.more = false;
                            this.notice.clear();
                            cx.notify();
                        }
                    })),
            );
        }
        let shutter = self
            .control("picker-shutter", &["shutter-fill"])
            .size(px(56.))
            .rounded_full()
            .border_2()
            .border_color(rgb(TEXT))
            .p_1()
            .bg(rgb(0x242529))
            .child(
                div()
                    .size(px(42.))
                    .rounded_full()
                    .bg(rgb(if self.settings.video { 0xf45b69 } else { TEXT })),
            )
            .on_click(cx.listener(|this, _, w, cx| this.confirm(w, cx)));
        let utility = self
            .row(&["cursor", "more"])
            .flex_1()
            .min_w_0()
            .h(px(56.))
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                self.control("picker-pointer", &["pointer-glyph"])
                    .size(px(40.))
                    .bg(rgb(if self.settings.cursor {
                        0x51525b
                    } else {
                        0x242529
                    }))
                    .child(icon("capture-arrow").size(px(20.)).text_color(rgb(TEXT)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.cursor = !this.settings.cursor;
                        cx.notify();
                    })),
            )
            .when(self.settings.video, |s| {
                s.child(
                    self.control("picker-more", &["more-glyph"])
                        .size(px(40.))
                        .bg(rgb(0x242529))
                        .child(icon("capture-settings").size(px(20.)).text_color(rgb(TEXT)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.more = !this.more;
                            cx.notify();
                        })),
                )
            });
        root = root.child(
            self.row(&["mode-group", "shutter-group", "utility-group"])
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .child(modes)
                .child(
                    self.row(&["start"])
                        .flex_1()
                        .min_w_0()
                        .h(px(56.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(shutter),
                )
                .child(utility),
        );
        if self.settings.video {
            root = root.child(
                self.row(&["audio", "mic"])
                    .w_full()
                    .flex()
                    .gap_2()
                    .children(
                        [
                            (false, "audio", "speaker", "Sound", self.settings.sound),
                            (true, "mic", "mic", "Mic", self.settings.microphone),
                        ]
                        .into_iter()
                        .map(|(mic, key, glyph, label, on)| {
                            self.control(
                                key,
                                if mic {
                                    &["mic-glyph", "mic-label"]
                                } else {
                                    &["audio-glyph", "audio-label"]
                                },
                            )
                            .flex_1()
                            .h(px(32.))
                            .gap_2()
                            .text_size(px(12.))
                            .child(icon(glyph).size(px(16.)).text_color(rgb(if on {
                                TEXT
                            } else {
                                MUTED
                            })))
                            .child(
                                div()
                                    .h(px(16.))
                                    .line_height(px(16.))
                                    .child(format!("{label} {}", if on { "on" } else { "off" })),
                            )
                            .bg(rgb(if on { 0x51525b } else { 0x303136 }))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if mic {
                                        this.settings.microphone = !this.settings.microphone;
                                    } else {
                                        this.settings.sound = !this.settings.sound;
                                    }
                                    cx.notify();
                                },
                            ))
                        }),
                    ),
            );
        } else {
            // Keep the shutter and mode buttons stationary when switching modes.
            root = root.child(
                self.row(&["capture-hint"])
                    .w_full()
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(muted("Enter to capture").h(px(16.)).line_height(px(16.))),
            );
        }
        root
    }
}
impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let source = self.desktop.frame.rect();
        let rect = self.selected_rect();
        let handles = self.kind == Kind::Selection;
        let image = if self.settings.cursor {
            self.cursor_image.clone()
        } else {
            self.image.clone()
        };
        let bounds = self.bounds.clone();
        let label = if !self.notice.is_empty() {
            self.notice.clone()
        } else {
            match self.kind {
                Kind::Selection => "Drag to select · Move or resize with the handles".into(),
                Kind::Screen => "Click a screen to capture".into(),
                Kind::Window => "Click a window to capture".into(),
            }
        };
        div()
            .id("desktop-capture-picker")
            .size_full()
            .relative()
            .bg(rgb(0x000000))
            .font_family("Geist")
            .text_color(rgb(TEXT))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, w, cx| {
                let key = e.keystroke.key.as_str();
                if key == "escape" {
                    this.dismiss(w);
                } else if this.deadline.is_some() {
                    return;
                } else {
                    match key {
                        "s" => this.choose(Kind::Selection),
                        "c" => this.choose(Kind::Screen),
                        "w" => this.choose(Kind::Window),
                        "v" if this.allow_video => {
                            this.settings.video = !this.settings.video;
                            this.more = false;
                        }
                        "p" => this.settings.cursor = !this.settings.cursor,
                        "enter" | "space" => this.confirm(w, cx),
                        "left" => this.selection.nudge(-1, 0, e.keystroke.modifiers.shift),
                        "right" => this.selection.nudge(1, 0, e.keystroke.modifiers.shift),
                        "up" => this.selection.nudge(0, -1, e.keystroke.modifiers.shift),
                        "down" => this.selection.nudge(0, 1, e.keystroke.modifiers.shift),
                        _ => return,
                    }
                }
                cx.notify();
                w.prevent_default();
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    if this.deadline.is_some() {
                        return;
                    }
                    if let Some(p) = this.point(e.position) {
                        if this.kind == Kind::Selection {
                            let tolerance = this.bounds.get().map_or(8., |b| {
                                8. * this.desktop.frame.width() as f32
                                    / f32::from(b.size.width).max(1.)
                            });
                            this.selection.down(p, tolerance);
                        } else {
                            this.hit(p);
                        }
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.selection.dragging()
                    && let Some(p) = this.point(e.position)
                {
                    this.selection.update(p, e.modifiers.shift);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, cx| {
                    if this.selection.dragging() {
                        if let Some(p) = this.point(e.position) {
                            this.selection.update(p, e.modifiers.shift);
                        }
                        this.selection.up();
                        cx.notify();
                    }
                }),
            )
            .child(
                canvas(
                    move |area, _, _| bounds.set(Some(area)),
                    move |area, _, w, _| {
                        if let Some(image) = image {
                            let _ = w.paint_image(area, Corners::default(), image, 0, false);
                        }
                        if let Some(rect) = rect {
                            let left = area.left()
                                + area.size.width
                                    * ((rect.x - source.x) as f32 / source.width as f32);
                            let top = area.top()
                                + area.size.height
                                    * ((rect.y - source.y) as f32 / source.height as f32);
                            let right =
                                left + area.size.width * (rect.width as f32 / source.width as f32);
                            let bottom = top
                                + area.size.height * (rect.height as f32 / source.height as f32);
                            for mask in [
                                Bounds::new(area.origin, size(area.size.width, top - area.top())),
                                Bounds::new(
                                    point(area.left(), bottom),
                                    size(area.size.width, area.bottom() - bottom),
                                ),
                                Bounds::new(
                                    point(area.left(), top),
                                    size(left - area.left(), bottom - top),
                                ),
                                Bounds::new(
                                    point(right, top),
                                    size(area.right() - right, bottom - top),
                                ),
                            ] {
                                w.paint_quad(fill(mask, rgba(0x00000077)));
                            }
                            for edge in [
                                Bounds::new(point(left, top), size(right - left, px(2.))),
                                Bounds::new(point(left, bottom), size(right - left, px(2.))),
                                Bounds::new(point(left, top), size(px(2.), bottom - top)),
                                Bounds::new(point(right, top), size(px(2.), bottom - top)),
                            ] {
                                w.paint_quad(fill(edge, rgb(TEXT)));
                            }
                            if handles {
                                for (x, y) in [
                                    (left, top),
                                    ((left + right) * 0.5, top),
                                    (right, top),
                                    (left, (top + bottom) * 0.5),
                                    (right, (top + bottom) * 0.5),
                                    (left, bottom),
                                    ((left + right) * 0.5, bottom),
                                    (right, bottom),
                                ] {
                                    w.paint_quad(fill(
                                        Bounds::new(
                                            point(x - px(4.), y - px(4.)),
                                            size(px(8.), px(8.)),
                                        ),
                                        rgb(TEXT),
                                    ));
                                }
                            }
                        } else {
                            w.paint_quad(fill(area, rgba(0x00000077)));
                        }
                    },
                )
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top(px(22.))
                    .left_0()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .max_w(px(620.))
                            .px_4()
                            .py_2()
                            .rounded_lg()
                            .bg(rgba(0x242529ee))
                            .text_size(px(12.))
                            .child(label),
                    ),
            )
            .child(
                button("picker-close", "Esc")
                    .child(icon("close"))
                    .absolute()
                    .right(px(22.))
                    .top(px(22.))
                    .bg(rgb(0x242529))
                    .on_click(cx.listener(|this, _, w, _| this.dismiss(w))),
            )
            .child(
                self.row(&["toolbar"])
                    .absolute()
                    .bottom(px(28.))
                    .left_0()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(self.toolbar(cx)),
            )
    }
}
impl Drop for Picker {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(None);
        }
    }
}

#[cfg(test)]
mod preparation_tests {
    use super::{CancelToken, Desktop, Frame, Instant, PreparedDesktop};

    fn desktop(width: u32, height: u32) -> Desktop {
        let mut frame = Frame::from_rgba8(width, height,
            [12, 34, 56, 255].into_iter().cycle().take((width * height * 4) as usize).collect()).unwrap();
        frame.origin = ::toolkit::types::Point::new(-1920, 120);
        let mut with_cursor = frame.clone();
        if !with_cursor.data().is_empty() {
            with_cursor.data_mut()[..4].copy_from_slice(&[210, 180, 90, 255]);
        }
        Desktop { frame, with_cursor, monitors: vec![], windows: vec![] }
    }

    #[test]
    fn worker_prepares_both_cursor_previews_without_changing_frozen_pixels() {
        let source = desktop(2, 1);
        let original = source.frame.clone();
        let cursor = source.with_cursor.clone();
        // Creation needs neither an App nor a Window and is safe on the snapshot worker.
        let prepared = std::thread::spawn(move || PreparedDesktop::new(source, &CancelToken::new()).unwrap())
            .join().unwrap();
        assert_eq!(prepared.desktop.frame, original);
        assert_eq!(prepared.desktop.with_cursor, cursor);
        assert_eq!(prepared.image.as_bytes(0).unwrap(), &[56, 34, 12, 255, 56, 34, 12, 255]);
        assert_eq!(prepared.cursor_image.as_bytes(0).unwrap(), &[90, 180, 210, 255, 56, 34, 12, 255]);
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(PreparedDesktop::new(desktop(2, 1), &cancel).is_err());
    }

    /// Only generated pixels and image handles; no windows or capture APIs.
    #[test]
    #[ignore = "synthetic picker preparation timing; run with --nocapture"]
    #[allow(clippy::chunks_exact_to_as_chunks)] // Preserve the historical loop being timed.
    fn timing_picker_preparation_handoff() {
        let old_frame_image = |frame: Frame| {
            let frame = frame.into_rgba8().unwrap();
            let (width, height) = (frame.width(), frame.height());
            let mut pixels = frame.into_data();
            // Historical desktop-crate loop, intentionally kept here so the
            // benchmark includes the old unoptimized development-build work.
            for pixel in pixels.chunks_exact_mut(4) { pixel.swap(0, 2); }
            crate::image_editor::render_image(width, height, pixels)
        };
        for (width, height) in [(1920, 1080), (3840, 2160)] {
            let mut old_ui = Vec::new();
            let mut new_ui = Vec::new();
            let mut background = Vec::new();
            for _ in 0..7 {
                let source = desktop(width, height);
                let start = Instant::now();
                let before = (old_frame_image(source.frame.clone()), old_frame_image(source.with_cursor.clone()));
                std::hint::black_box(&before);
                old_ui.push(start.elapsed().as_secs_f64() * 1000.);
                drop(before);
                let start = Instant::now();
                let prepared = PreparedDesktop::new(source, &CancelToken::new()).unwrap();
                background.push(start.elapsed().as_secs_f64() * 1000.);
                let start = Instant::now();
                let PreparedDesktop { desktop, image, cursor_image } = prepared;
                std::hint::black_box((&desktop, &image, &cursor_image));
                new_ui.push(start.elapsed().as_secs_f64() * 1000.);
            }
            old_ui.sort_by(f64::total_cmp);
            new_ui.sort_by(f64::total_cmp);
            background.sort_by(f64::total_cmp);
            println!("Picker synthetic {width}x{height}: median old_ui_pixels_ms={:.3} new_ui_handle_handoff_ms={:.6} background_pixels_ms={:.3}; excludes window creation and GPU upload",
                old_ui[3], new_ui[3], background[3]);
        }
    }
}
