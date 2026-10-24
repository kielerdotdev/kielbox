//! Borderless frozen-desktop capture: select, annotate, then copy/save in the same surface.
use crate::theme::*;
use gpui::{prelude::*, *};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use rcr_toolkit::{
    annotation as a,
    core::workflow::CancelToken,
    image_editor::{Actor, Command, Event, Preview},
    selection::{Point as PixelPoint, Selection},
    types::{Frame, Rect},
    worker::{CompletionSender, OverlayAction, OverlayCompletion, OverlayResult},
};
use std::{cell::Cell, rc::Rc, sync::mpsc, time::Duration};

#[path = "overlay_proof.rs"]
mod visual_fixture;
pub use visual_fixture::capture_fixture;

pub struct CaptureOverlay {
    actor: Actor,
    ui_waker: async_channel::Sender<()>,
    preview: Option<Preview>,
    image: crate::annotation_surface::Surface,
    focus: FocusHandle,
    toolbar_focus: FocusHandle,
    amount_plus_focus: FocusHandle,
    duplicate_focus: FocusHandle,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    properties_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    amount_plus_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    selection: Selection,
    targets: crate::region_targets::TargetSnapshot,
    hovered: Option<Rect>,
    cursor_pixel: Option<(u32, u32)>,
    target_press: Option<(PixelPoint, Rect)>,
    tool: Option<a::Tool>,
    drawing: bool,
    draft: Option<(a::PointF, a::PointF, a::Modifiers)>,
    color: u32,
    width: f32,
    font: f32,
    more: bool,
    toolbar_visible: bool,
    help_visible: bool,
    error: Option<String>,
    reply: Option<mpsc::SyncSender<Option<OverlayResult>>>,
    pending: Option<(Rect, OverlayAction)>,
    completion: Option<mpsc::Receiver<OverlayCompletion>>,
    cancel: CancelToken,
    physical: (i32, i32, i32, i32),
    interactive: bool,
    shown: bool,
    configured: bool,
    startup: Option<std::time::Instant>,
    startup_preview_logged: bool,
    startup_painted: Rc<Cell<bool>>,
}
fn screenshot_startup_clock() -> Option<std::time::Instant> {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    ENABLED.get_or_init(|| std::env::var_os("RCR_TRACE_SCREENSHOT").is_some_and(|value| value == "1"))
        .then(std::time::Instant::now)
}
fn trace_overlay_startup(start: Option<std::time::Instant>, stage: &str, worker_render_us: Option<u64>) {
    if let Some(start) = start {
        use std::io::Write;
        let line = format!("Screenshot overlay startup: {}\n", serde_json::json!({"stage":stage,
            "elapsed_ms":start.elapsed().as_secs_f64() * 1000.,"worker_render_us":worker_render_us,
            "scope":"Overlay entry through named stage; scene paint is not GPU completion"}));
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }
}
struct Hint(SharedString);
fn local_target(rect: Rect, physical: (i32, i32, i32, i32)) -> Option<Rect> {
    let left = (i64::from(rect.x) - i64::from(physical.0)).max(0);
    let top = (i64::from(rect.y) - i64::from(physical.1)).max(0);
    let right = (i64::from(rect.x) + i64::from(rect.width) - i64::from(physical.0)).min(i64::from(physical.2));
    let bottom = (i64::from(rect.y) + i64::from(rect.height) - i64::from(physical.1)).min(i64::from(physical.3));
    (right > left && bottom > top).then(|| Rect::new(left as i32, top as i32, (right - left) as u32, (bottom - top) as u32))
}
fn lens_bounds(bounds: Bounds<Pixels>, source: (u32, u32), cursor: (u32, u32)) -> Bounds<Pixels> {
    let pointer = bounds.origin + point(bounds.size.width * cursor.0 as f32 / source.0.max(1) as f32,
        bounds.size.height * cursor.1 as f32 / source.1.max(1) as f32);
    let width = px(100.); let height = px(122.);
    let left = if pointer.x + px(20.) + width <= bounds.right() { pointer.x + px(20.) } else { pointer.x - width - px(20.) };
    let top = if pointer.y + px(20.) + height <= bounds.bottom() { pointer.y + px(20.) } else { pointer.y - height - px(20.) };
    Bounds::new(point(left.clamp(bounds.left(), (bounds.right() - width).max(bounds.left())),
        top.clamp(bounds.top(), (bounds.bottom() - height).max(bounds.top()))), size(width, height))
}
fn dashed_region(window: &mut Window, bounds: Bounds<Pixels>) {
    border(window, bounds, rgb(0x000000).into());
    for horizontal in [true, false] {
        let length = if horizontal { bounds.size.width } else { bounds.size.height };
        let step = px(12.).max(length / 512.);
        let count = (f32::from(length) / f32::from(step)).ceil() as usize;
        for index in 0..count {
            let offset = step * index as f32;
            let dash = (step / 2.).min(length - offset);
            for far in [false, true] {
                let r = if horizontal { Bounds::new(point(bounds.left() + offset, if far { bounds.bottom() - px(1.) } else { bounds.top() }), size(dash, px(1.))) }
                    else { Bounds::new(point(if far { bounds.right() - px(1.) } else { bounds.left() }, bounds.top() + offset), size(px(1.), dash)) };
                window.paint_quad(fill(r, rgb(0xffffff)));
            }
        }
    }
}
impl Render for Hint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(PANEL))
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .child(self.0.clone())
    }
}
fn tooltip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |_, cx| cx.new(|_| Hint(text.into())).into()
}
pub(crate) fn native_place(
    window: &Window,
    rect: (i32, i32, i32, i32),
    show: bool,
    cx: &mut App,
) -> anyhow::Result<()> {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{
            HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos,
        },
    };
    let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)
        .map_err(|e| anyhow::anyhow!("Capture overlay handle: {e}"))?
        .as_raw()
    else {
        anyhow::bail!("Expected a Windows overlay");
    };
    let hwnd = handle.hwnd.get();
    let owner = window.window_handle();
    // Win32 synchronously emits WM_SIZE/WM_MOVE. Run outside App/window leases so GPUI's
    // callbacks can update its viewport; doing this inside a view update loses the resize.
    cx.spawn(async move |cx| {
        if !cx
            .update(|cx| cx.windows().contains(&owner))
            .unwrap_or(false)
        {
            return;
        }
        let result = unsafe {
            let result = SetWindowPos(
                HWND(hwnd as *mut _),
                Some(HWND_TOPMOST),
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                if show { SWP_SHOWWINDOW } else { SWP_NOACTIVATE },
            );
            if show && result.is_ok() {
                let _ = SetForegroundWindow(HWND(hwnd as *mut _));
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(HWND(
                    hwnd as *mut _,
                )));
            }
            result
        };
        if let Err(error) = result {
            eprintln!("Capture overlay placement: {error}");
            let _ = owner.update(cx, |root, window, cx| {
                if let Ok(view) = root.downcast::<CaptureOverlay>() {
                    view.update(cx, |this, _| this.cancel(window));
                }
            });
        }
    })
    .detach();
    Ok(())
}
pub fn open(
    frame: Frame,
    choose_region: bool,
    configured: bool,
    reply: mpsc::SyncSender<Option<OverlayResult>>,
    cancel: CancelToken,
    interactive: bool,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<CaptureOverlay>> {
    open_with_targets(frame, choose_region, configured, reply, cancel, interactive,
        crate::region_targets::TargetSnapshot::default(), cx)
}
// Keep the existing capture/fixture entry point compatible while supplying
// the immutable geometry collected before the overlay exists.
#[allow(clippy::too_many_arguments)]
pub fn open_with_targets(
    frame: Frame,
    choose_region: bool,
    configured: bool,
    reply: mpsc::SyncSender<Option<OverlayResult>>,
    cancel: CancelToken,
    interactive: bool,
    targets: crate::region_targets::TargetSnapshot,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<CaptureOverlay>> {
    let startup = screenshot_startup_clock();
    let physical = (
        frame.origin.x,
        frame.origin.y,
        i32::try_from(frame.width())?,
        i32::try_from(frame.height())?,
    );
    let (width, height) = (frame.width(), frame.height());
    let actor = Actor::desktop(frame)?;
    let (ui_waker, notifications) = async_channel::bounded(1);
    let actor_waker = ui_waker.clone();
    actor.set_waker(move || {
        let _ = actor_waker.try_send(());
    });
    actor.send(Command::Stroke(a::Color::rgb(239, 68, 68)))?;
    actor.send(Command::Width(3.))?;
    let window = cx.open_window(
        WindowOptions {
            show: false,
            focus: false,
            titlebar: None,
            kind: WindowKind::PopUp,
            is_resizable: false,
            is_minimizable: false,
            is_movable: false,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(physical.0 as f32), px(physical.1 as f32)),
                size(px(width as f32), px(height as f32)),
            ))),
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window);
                cx.spawn_in(window, async move |view, cx| {
                    while notifications.recv().await.is_ok() {
                        if view
                            .update_in(cx, |this: &mut CaptureOverlay, window, cx| {
                                this.poll(window, cx)
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
                cx.spawn_in(window, async move |view, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        if view
                            .update_in(cx, |this: &mut CaptureOverlay, window, _| {
                                if this.cancel.is_cancelled() { this.cancel(window); }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
                CaptureOverlay {
                    actor,
                    ui_waker,
                    preview: None,
                    image: Default::default(),
                    focus,
                    toolbar_focus: cx.focus_handle().tab_stop(true),
                    amount_plus_focus: cx.focus_handle().tab_stop(true),
                    duplicate_focus: cx.focus_handle().tab_stop(true),
                    bounds: Rc::new(Cell::new(None)),
                    properties_bounds: Rc::new(Cell::new(None)),
                    amount_plus_bounds: Rc::new(Cell::new(None)),
                    selection: Selection::new(width, height, !choose_region),
                    targets,
                    hovered: None,
                    cursor_pixel: None,
                    target_press: None,
                    tool: None,
                    drawing: false,
                    draft: None,
                    color: 0xef4444,
                    width: 3.,
                    font: 24.,
                    more: false,
                    toolbar_visible: true,
                    help_visible: false,
                    error: None,
                    reply: Some(reply),
                    pending: None,
                    completion: None,
                    cancel,
                    physical,
                    interactive,
                    shown: false,
                    configured,
                    startup,
                    startup_preview_logged: false,
                    startup_painted: Rc::new(Cell::new(false)),
                }
            })
        },
    )?;
    trace_overlay_startup(startup, "native_window_created", None);
    window.update(cx, |_, window, cx| {
        native_place(window, physical, false, cx)
    })??;
    Ok(window)
}
fn mods(m: gpui::Modifiers) -> a::Modifiers {
    a::Modifiers {
        ctrl: m.control || m.platform,
        alt: m.alt,
        shift: m.shift,
    }
}
impl CaptureOverlay {
    pub const SMOKE_STAGES: usize = 21;
    fn draft_rect(&self) -> Option<a::RectF> {
        let (start, end, mods) = self.draft?;
        Some(a::session::geometry::drag_rect(start, end, mods))
    }
    fn change_amount(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(p) = self.preview.as_mut().and_then(|p| p.properties.as_mut())
            && let Some(amount) = p.amount.as_mut()
        {
            *amount = (*amount + delta).clamp(1., 128.);
            let amount = *amount;
            self.send(Command::EffectAmount(amount), cx);
        }
    }
    fn change_opacity(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(p) = self.preview.as_mut().and_then(|p| p.properties.as_mut()) {
            p.opacity = (p.opacity + delta).clamp(0.05, 1.);
            let opacity = p.opacity;
            self.send(Command::Opacity(opacity), cx);
        }
    }
    fn change_zoom(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(p) = self.preview.as_mut().and_then(|p| p.properties.as_mut())
            && let Some(zoom) = p.zoom.as_mut()
        {
            *zoom = (*zoom + delta).clamp(1., 8.);
            let zoom = *zoom;
            self.send(Command::MagnifyZoom(zoom), cx);
        }
    }
    fn properties_bar(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let measure = self.properties_bounds.clone();
        let mut bar = div()
            .id("overlay-properties")
            .relative()
            .flex_shrink_0()
            .rounded_lg()
            .bg(rgba(0x17191df5))
            .border_1()
            .border_color(rgb(0x363a40))
            .shadow_lg()
            .p_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                canvas(move |b, _, _| measure.set(Some(b)), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            );
        let Some(p) = self.preview.as_ref().and_then(|p| p.properties.as_ref()) else {
            return bar.child(muted("Select an annotation to edit its properties"));
        };
        bar = bar.child(muted(format!(
            "{}{}",
            tool_label(p.tool),
            if p.selected > 0 {
                " · selected"
            } else {
                " · defaults"
            }
        )));
        if !matches!(
            p.tool,
            a::Tool::Blur | a::Tool::Pixelate | a::Tool::Spotlight
        ) {
            for (i, color) in [
                0xef4444, 0xf97316, 0xfacc15, 0x3ecf8e, 0x3b82f6, 0xa855f7, 0xffffff, 0x111111,
            ]
            .into_iter()
            .enumerate()
            {
                bar = bar.child(
                    div()
                        .id(("overlay-swatch", i))
                        .tab_index(0)
                        .focus(|s| s.border_color(rgb(TEXT)))
                        .size(px(20.))
                        .rounded_full()
                        .bg(rgb(color))
                        .border_2()
                        .border_color(rgb(if color == self.color {
                            ACCENT
                        } else {
                            0x60646c
                        }))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.color = color;
                            this.send(
                                Command::Stroke(a::Color::rgb(
                                    (color >> 16) as u8,
                                    (color >> 8) as u8,
                                    color as u8,
                                )),
                                cx,
                            );
                        })),
                );
            }
        }
        if !matches!(
            p.tool,
            a::Tool::Blur
                | a::Tool::Pixelate
                | a::Tool::Text
                | a::Tool::Spotlight
                | a::Tool::Highlight
        ) {
            bar = bar
                .child(
                    button("width-minus", "−").on_click(cx.listener(|this, _, _, cx| {
                        this.width = (this.width - 1.).max(1.);
                        this.send(Command::Width(this.width), cx);
                    })),
                )
                .child(muted(format!("Stroke {} px", self.width)))
                .child(
                    button("width-plus", "+").on_click(cx.listener(|this, _, _, cx| {
                        this.width = (this.width + 1.).min(64.);
                        this.send(Command::Width(this.width), cx);
                    })),
                );
        }
        if let Some(amount) = p.amount {
            let measure = self.amount_plus_bounds.clone();
            bar = bar
                .child(
                    button("amount-minus", "−")
                        .on_click(cx.listener(|this, _, _, cx| this.change_amount(-1., cx))),
                )
                .child(muted(format!(
                    "{} {amount:.0} px",
                    if p.tool == a::Tool::Blur {
                        "Strength"
                    } else {
                        "Block"
                    }
                )))
                .child(
                    button("amount-plus", "+")
                        .track_focus(&self.amount_plus_focus)
                        .relative()
                        .child(
                            canvas(move |b, _, _| measure.set(Some(b)), |_, _, _, _| {})
                                .absolute()
                                .inset_0(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.change_amount(1., cx))),
                );
        }
        if p.font.is_some() {
            bar = bar
                .child(
                    button("font-minus", "A−").on_click(cx.listener(|this, _, _, cx| {
                        this.font = (this.font - 2.).max(6.);
                        this.send(Command::FontSize(this.font), cx);
                    })),
                )
                .child(muted(format!("Font {} px", self.font)))
                .child(
                    button("font-plus", "A+").on_click(cx.listener(|this, _, _, cx| {
                        this.font = (this.font + 2.).min(256.);
                        this.send(Command::FontSize(this.font), cx);
                    })),
                );
        }
        if !matches!(
            p.tool,
            a::Tool::Blur | a::Tool::Pixelate | a::Tool::Spotlight
        ) {
            bar = bar
                .child(
                    button("opacity-minus", "−")
                        .on_click(cx.listener(|this, _, _, cx| this.change_opacity(-0.1, cx))),
                )
                .child(muted(format!("Opacity {:.0}%", p.opacity * 100.)))
                .child(
                    button("opacity-plus", "+")
                        .on_click(cx.listener(|this, _, _, cx| this.change_opacity(0.1, cx))),
                );
        }
        if let Some(zoom) = p.zoom {
            bar = bar
                .child(
                    button("zoom-minus", "−")
                        .on_click(cx.listener(|this, _, _, cx| this.change_zoom(-0.5, cx))),
                )
                .child(muted(format!("Zoom {zoom:.1}×")))
                .child(
                    button("zoom-plus", "+")
                        .on_click(cx.listener(|this, _, _, cx| this.change_zoom(0.5, cx))),
                );
        }
        if matches!(
            p.tool,
            a::Tool::Rectangle | a::Tool::Ellipse | a::Tool::Balloon
        ) {
            bar = bar.child(
                button(
                    "overlay-fill",
                    if p.fill { "Fill: on" } else { "Fill: off" },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(p) = this.preview.as_mut().and_then(|p| p.properties.as_mut()) {
                        p.fill = !p.fill;
                        let fill = p.fill;
                        this.send(Command::Fill(fill), cx);
                    }
                })),
            );
        }
        bar
    }
    fn send(&mut self, command: Command, cx: &mut Context<Self>) {
        if self.pending.is_some() { return; }
        let motion = matches!(command, Command::Move(..) | Command::Pan(..));
        if let Err(e) = self.actor.send(command) {
            self.error = Some(e.to_string());
            cx.notify();
        }
        if !motion {
            cx.notify();
        }
    }
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cancel.is_cancelled() {
            self.cancel(window);
            return;
        }
        if let Some(result) = self.completion.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => Some(OverlayCompletion::Failed(
                "Capture workflow stopped before confirming completion. Your annotations remain open.".into())),
            Err(mpsc::TryRecvError::Empty) => None,
        }) {
            self.completion = None;
            match result {
                OverlayCompletion::Succeeded => {
                    self.reply.take();
                    window.remove_window();
                    return;
                }
                OverlayCompletion::Failed(detail) => {
                    if self.pending.is_some_and(|(_, action)| action == OverlayAction::Configured) {
                        self.configured = false;
                    }
                    self.pending = None;
                    self.error = Some(detail.chars().take(4_200).collect());
                    self.toolbar_visible = true;
                    self.focus.focus(window);
                    cx.notify();
                }
            }
        }
        if let Some(mut preview) = self.actor.take_preview() {
            if !self.startup_preview_logged {
                trace_overlay_startup(self.startup, "first_preview_ready", Some(preview.render_us));
                self.startup_preview_logged = true;
            }
            if let Some(p) = &preview.properties {
                self.color = (p.color.r as u32) << 16 | (p.color.g as u32) << 8 | p.color.b as u32;
                self.width = p.width;
                if let Some(font) = p.font {
                    self.font = font;
                }
            }
            self.image.apply(&mut preview);
            self.preview = Some(preview);
            cx.notify();
            if !self.shown && self.interactive {
                match native_place(window, self.physical, true, cx) {
                    Ok(()) => {
                        self.shown = true;
                    }
                    Err(e) => {
                        self.error = Some(e.to_string());
                        self.cancel(window);
                    }
                }
            }
        }
        for event in self.actor.events.try_iter().collect::<Vec<_>>() {
            match event {
                Event::Accepted(frame) => {
                    if let Some((region, action)) = self.pending {
                        let (completed, completion) = mpsc::sync_channel(1);
                        let wake = self.ui_waker.clone();
                        let completed = CompletionSender::new(completed, move || { let _ = wake.try_send(()); });
                        let delivered = self.reply.as_ref().is_some_and(|reply| {
                            reply.try_send(Some(OverlayResult {
                                frame,
                                region,
                                action,
                                completion: completed,
                            })).is_ok()
                        });
                        if delivered {
                            self.completion = Some(completion);
                        } else {
                            self.pending = None;
                            self.error = Some("Capture workflow is no longer available. Your annotations remain open.".into());
                        }
                        cx.notify();
                    }
                }
                Event::Error(e) => {
                    self.pending = None;
                    self.error = Some(e.chars().take(4_200).collect());
                    cx.notify();
                }
                _ => {}
            }
        }
    }
    fn cancel(&mut self, window: &mut Window) {
        self.cancel.cancel();
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(None);
        }
        window.remove_window();
    }
    fn choose_tool(&mut self, tool: Option<a::Tool>, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_some() { return; }
        self.selection.up();
        self.hovered = None;
        self.cursor_pixel = None;
        self.target_press = None;
        self.drawing = false;
        self.draft = None;
        self.tool = tool;
        self.more = false;
        if let Some(tool) = tool {
            if tool != a::Tool::Select {
                self.send(Command::ClearSelection, cx);
            }
            self.send(Command::Tool(tool), cx);
        }
        self.focus.focus(window);
        cx.notify();
    }
    fn point(&self, p: Point<Pixels>) -> Option<PixelPoint> {
        let b = self.bounds.get()?;
        Some(PixelPoint {
            x: f32::from(p.x - b.left()) / f32::from(b.size.width).max(1.)
                * self.selection.size.0 as f32,
            y: f32::from(p.y - b.top()) / f32::from(b.size.height).max(1.)
                * self.selection.size.1 as f32,
        })
    }
    fn finish(&mut self, action: OverlayAction, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            return;
        }
        if self.selection.rect.is_none() {
            self.selection.full();
        }
        if let Some(rect) = self.selection.rect {
            let admitted = self.actor.send(Command::AcceptRegion(rect));
            self.finish_admission(rect, action, admitted, cx);
        }
    }
    fn finish_admission(
        &mut self,
        rect: Rect,
        action: OverlayAction,
        admitted: anyhow::Result<()>,
        cx: &mut Context<Self>,
    ) {
        // A rejected command produces no actor completion event. Enter pending
        // only after admission, so a full queue cannot strand Copy/Save forever.
        match admitted {
            Ok(()) => {
                self.pending = Some((rect, action));
                self.error = None;
            }
            Err(error) => {
                self.pending = None;
                self.error = Some(error.to_string());
            }
        }
        cx.notify();
    }
    fn down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            return;
        }
        self.focus.focus(window);
        self.more = false;
        if let Some(p) = self.point(e.position) {
            if self.tool.is_none() {
                self.target_press = if self.selection.rect.is_none() {
                    self.targets.hit_test(rcr_toolkit::types::Point {
                        x: self.physical.0.saturating_add(p.x.floor() as i32),
                        y: self.physical.1.saturating_add(p.y.floor() as i32),
                    }).and_then(|rect| local_target(rect, self.physical)).map(|rect| (p, rect))
                } else { None };
                let tolerance = self.bounds.get().map_or(8., |b| {
                    8. * self.selection.size.0 as f32 / f32::from(b.size.width).max(1.)
                });
                self.selection.down(p, tolerance);
            } else {
                if self.selection.rect.is_none() {
                    self.selection.full();
                }
                let r = self.selection.rect.unwrap();
                if p.x < r.x as f32
                    || p.y < r.y as f32
                    || p.x > r.x as f32 + r.width as f32
                    || p.y > r.y as f32 + r.height as f32
                {
                    return;
                }
                self.drawing = true;
                let point = a::PointF::new(p.x, p.y);
                let handle = self
                    .preview
                    .as_ref()
                    .is_some_and(|p| p.overlay.handles.iter().any(|h| h.pos.distance(point) < 8.));
                if !handle
                    && matches!(
                        self.tool,
                        Some(
                            a::Tool::Blur
                                | a::Tool::Pixelate
                                | a::Tool::Highlight
                                | a::Tool::Spotlight
                                | a::Tool::Magnify
                                | a::Tool::Rectangle
                                | a::Tool::Ellipse
                        )
                    )
                {
                    self.draft = Some((point, point, mods(e.modifiers)));
                }
                self.send(
                    Command::Down(a::PointF::new(p.x, p.y), mods(e.modifiers)),
                    cx,
                );
            }
        }
        cx.notify();
    }
    fn motion(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.pending.is_some() { return; }
        if let Some(p) = self.point(e.position) {
            if self.tool.is_none() && !self.drawing && (self.selection.rect.is_none() || self.selection.dragging()) {
                let pixel = (p.x.floor().clamp(0., self.selection.size.0.saturating_sub(1) as f32) as u32,
                    p.y.floor().clamp(0., self.selection.size.1.saturating_sub(1) as f32) as u32);
                if self.cursor_pixel != Some(pixel) {
                    self.cursor_pixel = Some(pixel);
                    self.hovered = if self.selection.dragging() { None } else {
                        self.targets.hit_test(rcr_toolkit::types::Point { x: self.physical.0.saturating_add(pixel.0 as i32), y: self.physical.1.saturating_add(pixel.1 as i32) })
                            .and_then(|rect| local_target(rect, self.physical))
                    };
                    cx.notify();
                }
            } else if self.hovered.take().is_some() | self.cursor_pixel.take().is_some() { cx.notify(); }
            if self.target_press.is_some_and(|(start, _)| (p.x - start.x).abs().max((p.y - start.y).abs()) > 3.) { self.target_press = None; }
            if self.selection.dragging() {
                self.selection.update(p, e.modifiers.shift);
                cx.notify();
            } else if self.drawing {
                if let Some((start, _, _)) = self.draft {
                    self.draft = Some((start, a::PointF::new(p.x, p.y), mods(e.modifiers)));
                    cx.notify();
                }
                self.send(
                    Command::Move(a::PointF::new(p.x, p.y), mods(e.modifiers)),
                    cx,
                );
            }
        }
    }
    fn up(&mut self, e: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.pending.is_some() { return; }
        if self.selection.dragging() {
            if let Some(p) = self.point(e.position) {
                self.selection.update(p, e.modifiers.shift);
            }
            self.selection.up();
            if self.tool.is_none()
                && let Some((start, rect)) = self.target_press.take()
                && self.point(e.position).is_some_and(|p| (p.x - start.x).abs().max((p.y - start.y).abs()) <= 3.) {
                self.selection.rect = Some(rect);
            }
            self.hovered = None;
            self.cursor_pixel = None;
        }
        if self.drawing {
            self.drawing = false;
            self.draft = None;
            if let Some(p) = self.point(e.position) {
                self.send(Command::Up(a::PointF::new(p.x, p.y), mods(e.modifiers)), cx);
            }
        }
        cx.notify();
    }
    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ctrl = e.keystroke.modifiers.control || e.keystroke.modifiers.platform;

        let shift = e.keystroke.modifiers.shift;
        let key = e.keystroke.key.as_str();
        if self.pending.is_some() {
            if key == "escape" || (ctrl && key == "q") {
                self.cancel(window);
            } else if key == "tab" {
                if shift { window.focus_prev(); } else { window.focus_next(); }
            } else if matches!(key, "enter" | "space") {
                return; // Native activation of the remaining Cancel button.
            }
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if key == "f6" && !ctrl && !e.keystroke.modifiers.alt {
            if self.focus.is_focused(window) {
                self.toolbar_visible = true;
                self.toolbar_focus.focus(window);
            } else {
                self.focus.focus(window);
            }
            window.prevent_default();
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // A focused toolbar/property button activates on GPUI's native key-up.
        // Canvas shortcuts must not copy the capture or expand its region first.
        if !ctrl && matches!(key, "enter" | "space") && !self.focus.is_focused(window) {
            return;
        }
        if ctrl {
            match key {
                "c" => self.finish(OverlayAction::Copy, cx),
                "s" => self.finish(OverlayAction::Save, cx),
                "z" => self.send(if shift { Command::Redo } else { Command::Undo }, cx),
                "y" => self.send(Command::Redo, cx),
                "q" => self.cancel(window),
                "h" => {
                    self.toolbar_visible = !self.toolbar_visible;
                    cx.notify();
                }
                _ => {
                    if let Some(k) = key.chars().next() {
                        self.send(
                            Command::Key(a::Key::Char(k), mods(e.keystroke.modifiers)),
                            cx,
                        );
                    }
                }
            }
            cx.stop_propagation();
            return;
        }
        if key == "escape" {
            if self.help_visible {
                self.help_visible = false;
                cx.notify();
            } else if self.more {
                self.more = false;
                cx.notify();
            } else if self.pending.is_none() && self.selection.dragging() {
                // Stop the region gesture without discarding its current bounds.
                self.selection.up();
                cx.notify();
            } else if self.pending.is_none() && (self.drawing || self.preview.as_ref().is_some_and(|p| {
                p.overlay.caret.is_some() || p.overlay.creation.is_some() ||
                    !p.overlay.selection.is_empty() || p.overlay.marquee.is_some() ||
                    p.overlay.crop.is_some() || p.overlay.cut_strip.is_some()
            })) {
                // SSX commits active text, cancels a live annotation gesture, or
                // clears selection before Escape is allowed to leave capture.
                self.drawing = false;
                self.draft = None;
                self.send(Command::Key(a::Key::Escape, a::Modifiers::NONE), cx);
                self.focus.focus(window);
            } else {
                self.cancel(window);
            }
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        // Region selection must remain reachable while a text tool owns character input.
        if key == "tab" {
            if self.focus.is_focused(window) {
                self.choose_tool(None, window, cx);
            } else if shift {
                window.focus_prev();
            } else {
                window.focus_next();
            }
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if key == "f1" {
            self.help_visible = !self.help_visible;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // Toolbar typing must not insert annotation text or change canvas tools.
        if !self.focus.is_focused(window) {
            return;
        }
        if self.tool == Some(a::Tool::Text) || self.tool == Some(a::Tool::Balloon) {
            let special = match key {
                "enter" => Some(a::Key::Enter),
                "backspace" => Some(a::Key::Backspace),
                "delete" => Some(a::Key::Delete),
                "left" => Some(a::Key::Left),
                "right" => Some(a::Key::Right),
                "up" => Some(a::Key::Up),
                "down" => Some(a::Key::Down),
                _ => None,
            };
            if let Some(k) = special {
                self.send(Command::Key(k, mods(e.keystroke.modifiers)), cx);
            } else if let Some(text) = &e.keystroke.key_char {
                self.send(Command::Text(text.clone()), cx);
            }
            cx.stop_propagation();
            return;
        }
        match key {
            "enter" => self.finish(OverlayAction::Copy, cx),
            "tab" => self.choose_tool(None, window, cx),
            "space" => {
                self.selection.full();
                cx.notify();
            }
            "delete" | "backspace" => {
                if self.tool.is_none() {
                    self.selection.clear();
                    cx.notify();
                } else {
                    self.send(Command::Delete, cx);
                }
            }
            "left" | "right" | "up" | "down" => {
                let (dx, dy) = match key {
                    "left" => (-1, 0),
                    "right" => (1, 0),
                    "up" => (0, -1),
                    _ => (0, 1),
                };
                if self.tool.is_none() {
                    self.selection.nudge(dx, dy, shift);
                    cx.notify();
                } else {
                    let k = match key {
                        "left" => a::Key::Left,
                        "right" => a::Key::Right,
                        "up" => a::Key::Up,
                        _ => a::Key::Down,
                    };
                    self.send(Command::Key(k, mods(e.keystroke.modifiers)), cx);
                }
            }
            "a" => self.choose_tool(Some(a::Tool::Arrow), window, cx),
            "r" => self.choose_tool(Some(a::Tool::Rectangle), window, cx),
            "e" | "c" => self.choose_tool(Some(a::Tool::Ellipse), window, cx),
            "f" => self.choose_tool(Some(a::Tool::Freehand), window, cx),
            "p" => self.choose_tool(Some(a::Tool::Pixelate), window, cx),
            "l" => self.choose_tool(Some(a::Tool::Line), window, cx),
            "t" => self.choose_tool(Some(a::Tool::Text), window, cx),
            "b" => self.choose_tool(Some(a::Tool::Blur), window, cx),
            "v" => self.choose_tool(Some(a::Tool::Select), window, cx),
            "h" => self.choose_tool(Some(a::Tool::Highlight), window, cx),
            "n" => self.choose_tool(Some(a::Tool::Step), window, cx),
            "o" => self.choose_tool(Some(a::Tool::Balloon), window, cx),
            "m" => self.choose_tool(Some(a::Tool::Magnify), window, cx),
            "s" => self.choose_tool(Some(a::Tool::Spotlight), window, cx),
            _ => {}
        }
        cx.stop_propagation();
    }
    pub fn smoke_step(
        view: Entity<Self>,
        stage: &mut usize,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<bool> {
        use anyhow::{Context, ensure};
        fn shortcut(window: &mut Window, cx: &mut App, key: &str, shift: bool) {
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: Keystroke {
                        key: key.into(),
                        modifiers: gpui::Modifiers {
                            control: true,
                            shift,
                            ..Default::default()
                        },
                        key_char: None,
                    },
                    is_held: false,
                }),
                cx,
            );
        }
        window.draw(cx).clear();
        if !view.read(cx).image.ready() {
            return Ok(false);
        }
        fn drag(window: &mut Window, cx: &mut App, from: Point<Pixels>, to: Point<Pixels>) {
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
                    button: MouseButton::Left,
                    position: from,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: to,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Default::default(),
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: to,
                    modifiers: Default::default(),
                    click_count: 1,
                }),
                cx,
            );
        }
        fn begin(window: &mut Window, cx: &mut App, from: Point<Pixels>, to: Point<Pixels>) {
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: from,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: to,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Default::default(),
                }),
                cx,
            );
        }
        fn release(window: &mut Window, cx: &mut App, at: Point<Pixels>) {
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers: Default::default(),
                    click_count: 1,
                }),
                cx,
            );
        }
        let b = view
            .read(cx)
            .bounds
            .get()
            .context("Overlay canvas not painted")?;
        let expected = view.read(cx).selection.size;
        let painted = (
            f32::from(b.size.width) * window.scale_factor(),
            f32::from(b.size.height) * window.scale_factor(),
        );
        ensure!(
            (painted.0 - expected.0 as f32).abs() < 2.
                && (painted.1 - expected.1 as f32).abs() < 2.,
            "Overlay canvas is not native desktop size: {painted:?}, expected {expected:?}"
        );
        let at = |x: f32, y: f32| b.origin + point(b.size.width * x, b.size.height * y);
        if *stage == 0 {
            drag(window, cx, at(0.2, 0.15), at(0.7, 0.55));
            ensure!(
                view.read(cx).selection.rect.is_some(),
                "Region was not selected"
            );
            ensure!(
                view.read(cx).reply.is_some(),
                "Mouse-up closed capture instead of keeping overlay open"
            );
            *stage = 1;
        } else if *stage == 1 {
            let before = view.read(cx).selection.rect.unwrap();
            drag(window, cx, at(0.4, 0.3), at(0.45, 0.35));
            let after = view.read(cx).selection.rect.unwrap();
            ensure!(
                after.x > before.x && after.width == before.width,
                "Selection did not move"
            );
            *stage = 2;
        } else if *stage == 2 {
            let r = view.read(cx).selection.rect.unwrap();
            let pos = |x: f32, y: f32| {
                b.origin
                    + point(
                        b.size.width * x / view.read(cx).selection.size.0 as f32,
                        b.size.height * y / view.read(cx).selection.size.1 as f32,
                    )
            };
            let start = pos(r.x as f32 + r.width as f32, r.y as f32 + r.height as f32);
            drag(window, cx, start, start + point(px(30.), px(20.)));
            ensure!(
                !view.read(cx).selection.dragging(),
                "Toolbar swallowed selection drag release"
            );
            ensure!(
                view.read(cx).selection.rect.unwrap().width > r.width,
                "Selection handle did not resize"
            );
            view.update(cx, |this, cx| {
                this.choose_tool(Some(a::Tool::Arrow), window, cx)
            });
            *stage = 3;
        } else if *stage == 3 {
            drag(window, cx, at(0.35, 0.3), at(0.55, 0.45));
            *stage = 4;
        } else if *stage == 4
            && view
                .read(cx)
                .preview
                .as_ref()
                .is_some_and(|p| p.objects == 1 && p.can_undo)
        {
            shortcut(window, cx, "z", false);
            *stage = 5;
        } else if *stage == 5
            && view
                .read(cx)
                .preview
                .as_ref()
                .is_some_and(|p| p.objects == 0 && p.can_redo)
        {
            shortcut(window, cx, "z", true);
            *stage = 6;
        } else if *stage == 6
            && view
                .read(cx)
                .preview
                .as_ref()
                .is_some_and(|p| p.objects == 1)
        {
            ensure!(
                view.read(cx).properties_bounds.get().is_some(),
                "Properties should be visible without right click"
            );
            view.update(cx, |this, cx| {
                this.choose_tool(Some(a::Tool::Blur), window, cx)
            });
            begin(window, cx, at(0.42, 0.36), at(0.50, 0.45));
            ensure!(
                view.read(cx).draft_rect().is_some(),
                "Missing immediate blur drag indication"
            );
            *stage = 7;
        } else if *stage == 7
            && view
                .read(cx)
                .preview
                .as_ref()
                .is_some_and(|p| p.overlay.creation.is_some())
        {
            ensure!(
                view.read(cx)
                    .preview
                    .as_ref()
                    .unwrap()
                    .properties
                    .as_ref()
                    .is_some_and(|p| p.tool == a::Tool::Blur),
                "Blur defaults missing"
            );
            release(window, cx, at(0.50, 0.45));
            *stage = 8;
        } else if *stage == 8
            && view.read(cx).preview.as_ref().is_some_and(|p| {
                p.properties
                    .as_ref()
                    .is_some_and(|p| p.tool == a::Tool::Blur && p.selected == 1)
            })
        {
            ensure!(
                view.read(cx).properties_bounds.get().is_some(),
                "Blur properties hidden after release"
            );
            view.update(cx, |this, cx| {
                this.choose_tool(Some(a::Tool::Pixelate), window, cx)
            });
            begin(window, cx, at(0.57, 0.36), at(0.66, 0.45));
            *stage = 9;
        } else if *stage == 9
            && view.read(cx).preview.as_ref().is_some_and(|p| {
                p.overlay.creation.is_some()
                    && p.properties
                        .as_ref()
                        .is_some_and(|p| p.tool == a::Tool::Pixelate)
            })
        {
            ensure!(
                view.read(cx).draft_rect().is_some(),
                "Missing pixelate drag indication"
            );
            release(window, cx, at(0.66, 0.45));
            *stage = 10;
        } else if *stage == 10
            && view.read(cx).preview.as_ref().is_some_and(|p| {
                p.properties
                    .as_ref()
                    .is_some_and(|p| p.tool == a::Tool::Pixelate && p.selected == 1)
            })
        {
            view.update(cx, |this, cx| {
                this.choose_tool(Some(a::Tool::Select), window, cx)
            });
            drag(window, cx, at(0.46, 0.40), at(0.46, 0.40));
            *stage = 11;
        } else if *stage == 11
            && view.read(cx).preview.as_ref().is_some_and(|p| {
                p.properties
                    .as_ref()
                    .is_some_and(|p| p.tool == a::Tool::Blur && p.selected == 1)
            })
        {
            let plus = view
                .read(cx)
                .amount_plus_bounds
                .get()
                .context("Selected blur strength control not painted")?
                .center();
            begin(window, cx, plus, plus);
            release(window, cx, plus);
            *stage = 12;
        } else if *stage == 12
            && view.read(cx).preview.as_ref().is_some_and(|p| {
                p.properties
                    .as_ref()
                    .is_some_and(|p| p.tool == a::Tool::Blur && p.amount == Some(11.))
            })
        {
            ensure!(
                !view.read(cx).focus.is_focused(window),
                "Strength button did not receive native focus after click"
            );
            let navigation = |window: &mut Window, cx: &mut App, key: &str, shift: bool| {
                window.dispatch_event(
                    PlatformInput::KeyDown(KeyDownEvent {
                        keystroke: Keystroke {
                            key: key.into(),
                            modifiers: Modifiers {
                                shift,
                                ..Default::default()
                            },
                            key_char: None,
                        },
                        is_held: false,
                    }),
                    cx,
                );
                window.draw(cx).clear();
            };
            navigation(window, cx, "f6", false);
            ensure!(
                view.read(cx).focus.is_focused(window),
                "F6 did not restore canvas focus"
            );
            navigation(window, cx, "f6", false);
            ensure!(
                view.read(cx).toolbar_focus.is_focused(window),
                "F6 did not focus toolbar"
            );
            let tool = view.read(cx).tool;
            // Actual key events traverse the painted GPUI tab stops, rather than
            // assigning focus directly to the property under test.
            for _ in 0..48 {
                if view.read(cx).amount_plus_focus.is_focused(window) {
                    break;
                }
                navigation(window, cx, "tab", false);
            }
            ensure!(
                view.read(cx).amount_plus_focus.is_focused(window),
                "Tab could not reach the strength property from toolbar"
            );
            navigation(window, cx, "tab", true);
            ensure!(
                !view.read(cx).amount_plus_focus.is_focused(window),
                "Shift+Tab did not move focus"
            );
            navigation(window, cx, "tab", false);
            ensure!(
                view.read(cx).amount_plus_focus.is_focused(window),
                "Tab did not return to strength property"
            );
            navigation(window, cx, "t", false);
            ensure!(
                view.read(cx).tool == tool,
                "Toolbar typing changed the canvas tool"
            );
            let region = view.read(cx).selection.rect;
            for key in ["enter", "space"] {
                let keystroke = Keystroke {
                    key: key.into(),
                    modifiers: Default::default(),
                    key_char: None,
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
                ensure!(
                    view.read(cx).pending.is_none() && view.read(cx).reply.is_some(),
                    "Focused property activation tried to finish the capture"
                );
                ensure!(
                    view.read(cx).selection.rect == region,
                    "Focused Space activation changed the capture region"
                );
            }
            navigation(window, cx, "f6", false);
            ensure!(
                view.read(cx).focus.is_focused(window),
                "F6 did not restore canvas after property activation"
            );
            *stage = 13;
        } else if *stage == 13
            && view.read(cx).preview.as_ref().is_some_and(|p| {
                p.properties
                    .as_ref()
                    .is_some_and(|p| p.tool == a::Tool::Blur && p.amount == Some(13.))
            })
        {
            shortcut(window, cx, "z", false);
            *stage = 14;
        } else if *stage == 14
            && view.read(cx).preview.as_ref().is_some_and(|p| {
                p.properties
                    .as_ref()
                    .is_some_and(|p| p.tool == a::Tool::Blur && p.amount == Some(10.))
            })
        {
            ensure!(
                view.read(cx)
                    .preview
                    .as_ref()
                    .is_some_and(|p| p.objects == 3),
                "Unexpected smoke object count before duplicate"
            );
            view.update(cx, |this, cx| {
                this.more = true;
                cx.notify();
            });
            window.draw(cx).clear();
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: Keystroke {
                        key: "f6".into(),
                        modifiers: Default::default(),
                        key_char: None,
                    },
                    is_held: false,
                }),
                cx,
            );
            for _ in 0..48 {
                if view.read(cx).duplicate_focus.is_focused(window) {
                    break;
                }
                window.dispatch_event(
                    PlatformInput::KeyDown(KeyDownEvent {
                        keystroke: Keystroke {
                            key: "tab".into(),
                            modifiers: Default::default(),
                            key_char: None,
                        },
                        is_held: false,
                    }),
                    cx,
                );
                window.draw(cx).clear();
            }
            ensure!(
                view.read(cx).duplicate_focus.is_focused(window),
                "Selected annotation actions are not keyboard reachable"
            );
            let keystroke = Keystroke {
                key: "enter".into(),
                modifiers: Default::default(),
                key_char: None,
            };
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                }),
                cx,
            );
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
            *stage = 15;
        } else if *stage == 15
            && view
                .read(cx)
                .preview
                .as_ref()
                .is_some_and(|p| p.objects == 4)
        {
            ensure!(
                view.read(cx).pending.is_none(),
                "Duplicate action finished capture"
            );
            shortcut(window, cx, "z", false);
            *stage = 16;
        } else if *stage == 16
            && view
                .read(cx)
                .preview
                .as_ref()
                .is_some_and(|p| p.objects == 3)
        {
            view.update(cx, |this, cx| this.choose_tool(Some(a::Tool::Text), window, cx));
            drag(window, cx, at(0.65, 0.25), at(0.65, 0.25));
            window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke { key: "x".into(), key_char: Some("x".into()), modifiers: Default::default() },
                is_held: false,
            }), cx);
            *stage = 17;
        } else if *stage == 17 && view.read(cx).preview.as_ref().is_some_and(|p| {
            p.objects == 4 && p.overlay.caret.is_some()
        }) {
            window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke { key: "escape".into(), key_char: None, modifiers: Default::default() },
                is_held: false,
            }), cx);
            ensure!(view.read(cx).reply.is_some(), "Escape during text editing discarded capture");
            *stage = 18;
        } else if *stage == 18 && view.read(cx).preview.as_ref().is_some_and(|p| {
            p.objects == 4 && p.overlay.caret.is_none() && !p.overlay.selection.is_empty()
        }) {
            window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke { key: "escape".into(), key_char: None, modifiers: Default::default() },
                is_held: false,
            }), cx);
            ensure!(view.read(cx).reply.is_some(), "Escape on annotation selection discarded capture");
            *stage = 19;
        } else if *stage == 19 && view.read(cx).preview.as_ref().is_some_and(|p| {
            p.objects == 4 && p.overlay.caret.is_none() && p.overlay.selection.is_empty()
        }) {
            ensure!(view.read(cx).reply.is_some() && view.read(cx).selection.rect.is_some(),
                "Staged Escape lost frozen capture or region");
            *stage = 20;
        } else if *stage == 20 && view.read(cx).preview.as_ref().is_some_and(|p| p.objects == 4) {
            view.read(cx).focus.focus(window);
            // Consecutive property changes form one coalesced gesture in SSX history.
            // Undo must restore the original strength, rather than each repeated click.
            // Exercise keyboard routing through the actual GPUI surface: text entry must
            // not trap region selection, and hidden tools must always be recoverable.
            let key = |window: &mut Window, cx: &mut App, value: &str| {
                window.dispatch_event(
                    PlatformInput::KeyDown(KeyDownEvent {
                        keystroke: Keystroke {
                            key: value.into(),
                            modifiers: Default::default(),
                            key_char: None,
                        },
                        is_held: false,
                    }),
                    cx,
                );
            };
            key(window, cx, "t");
            ensure!(
                view.read(cx).tool == Some(a::Tool::Text),
                "Text shortcut failed"
            );
            key(window, cx, "tab");
            ensure!(
                view.read(cx).tool.is_none(),
                "Text tool trapped Tab shortcut"
            );
            key(window, cx, "p");
            ensure!(
                view.read(cx).tool == Some(a::Tool::Pixelate),
                "Pixelate shortcut failed"
            );
            key(window, cx, "f1");
            ensure!(view.read(cx).help_visible, "Shortcut help did not open");
            key(window, cx, "escape");
            ensure!(
                !view.read(cx).help_visible && view.read(cx).reply.is_some(),
                "Closing help cancelled capture"
            );
            shortcut(window, cx, "h", false);
            ensure!(!view.read(cx).toolbar_visible, "Toolbar did not hide");
            shortcut(window, cx, "h", false);
            ensure!(view.read(cx).toolbar_visible, "Toolbar did not restore");
            println!(
                "Annotation properties smoke: F6 toolbar/canvas focus, Tab/Shift+Tab traversal, text-safe controls, strength and selected Duplicate button keyboard activation with undo, stable region/no finish, tool shortcuts and recoverable toolbar passed"
            );
            // Deterministically exercise the same admission-failure completion
            // path as a full actor queue, without racing its rendering thread.
            let retained_region = view.read(cx).selection.rect;
            view.update(cx, |this, cx| {
                this.finish_admission(retained_region.unwrap(), OverlayAction::Copy,
                    Err(anyhow::anyhow!("Image editor is busy; try again after rendering finishes")), cx);
            });
            window.draw(cx).clear();
            ensure!(view.read(cx).pending.is_none() && view.read(cx).reply.is_some(),
                "Rejected acceptance stranded the capture or dropped its reply");
            ensure!(view.read(cx).error.is_some() && view.read(cx).selection.rect == retained_region,
                "Rejected acceptance lost the error or selected region");
            shortcut(window, cx, "c", false);
            ensure!(
                view.read(cx).pending.is_some(),
                "Ctrl+C did not confirm overlay"
            );
            ensure!(view.read(cx).error.is_none(), "Successful retry retained a stale admission error");
            *stage = Self::SMOKE_STAGES;
        }
        Ok(*stage == Self::SMOKE_STAGES)
    }
    pub fn smoke_recover_retry(view: Entity<Self>, window: &mut Window, cx: &mut App) -> anyhow::Result<bool> {
        window.draw(cx).clear();
        if view.read(cx).pending.is_some() { return Ok(false); }
        anyhow::ensure!(view.read(cx).error.is_some() && view.read(cx).reply.is_some(),
            "Failed operation lost its error or capture reply");
        anyhow::ensure!(view.read(cx).preview.as_ref().is_some_and(|p| p.objects == 4) &&
            view.read(cx).selection.rect.is_some(), "Failed operation discarded the annotation Actor or region");
        let keystroke = Keystroke { key: "s".into(), key_char: None,
            modifiers: gpui::Modifiers { control: true, ..Default::default() } };
        window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent { keystroke: keystroke.clone(), is_held: false }), cx);
        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
        anyhow::ensure!(view.read(cx).pending.is_some_and(|(_, action)| action == OverlayAction::Save) &&
            view.read(cx).error.is_none(), "Explicit Save retry was not accepted after failed Copy");
        Ok(true)
    }
}
fn handle_points(r: Bounds<Pixels>) -> [Point<Pixels>; 8] {
    [
        r.origin,
        point(r.center().x, r.top()),
        point(r.right(), r.top()),
        point(r.right(), r.center().y),
        point(r.right(), r.bottom()),
        point(r.center().x, r.bottom()),
        point(r.left(), r.bottom()),
        point(r.left(), r.center().y),
    ]
}
fn border(window: &mut Window, r: Bounds<Pixels>, color: Hsla) {
    for b in [
        Bounds::new(r.origin, size(r.size.width, px(1.))),
        Bounds::new(point(r.left(), r.bottom()), size(r.size.width, px(1.))),
        Bounds::new(r.origin, size(px(1.), r.size.height)),
        Bounds::new(point(r.right(), r.top()), size(px(1.), r.size.height)),
    ] {
        window.paint_quad(fill(b, color));
    }
}
fn tool_label(tool: a::Tool) -> &'static str {
    match tool {
        a::Tool::Select => "Select",
        a::Tool::Arrow => "Arrow",
        a::Tool::Rectangle => "Rectangle",
        a::Tool::Ellipse => "Ellipse",
        a::Tool::Line => "Line",
        a::Tool::Freehand => "Pen",
        a::Tool::Text => "Text",
        a::Tool::Step => "Step",
        a::Tool::Highlight => "Highlight",
        a::Tool::Blur => "Blur",
        a::Tool::Pixelate => "Pixelate",
        a::Tool::Balloon => "Balloon",
        a::Tool::Magnify => "Magnify",
        a::Tool::Spotlight => "Spotlight",
        _ => "Tool",
    }
}
fn tool_shortcut(tool: a::Tool) -> &'static str {
    match tool {
        a::Tool::Select => "Select · V",
        a::Tool::Arrow => "Arrow · A",
        a::Tool::Rectangle => "Rectangle · R",
        a::Tool::Ellipse => "Ellipse · E",
        a::Tool::Line => "Line · L",
        a::Tool::Freehand => "Pen · F",
        a::Tool::Text => "Text · T",
        a::Tool::Step => "Numbered step · N",
        a::Tool::Highlight => "Highlight · H",
        a::Tool::Blur => "Blur · B",
        a::Tool::Pixelate => "Pixelate · P",
        a::Tool::Balloon => "Speech balloon · O",
        a::Tool::Magnify => "Magnify · M",
        a::Tool::Spotlight => "Spotlight · S",
        _ => "Tool",
    }
}
impl Render for CaptureOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.properties_bounds.set(None);
        self.amount_plus_bounds.set(None);
        self.image.release(window);
        let image = self.image.snapshot();
        let startup_ready = self.image.ready();
        let startup = self.startup;
        let startup_painted = self.startup_painted.clone();
        let hover = (self.tool.is_none() && self.pending.is_none() && !self.drawing && !self.selection.dragging() && self.selection.rect.is_none())
            .then_some(self.hovered).flatten();
        let selection = self.selection.rect.or(hover);
        let cursor = (self.tool.is_none() && self.pending.is_none() && !self.drawing && (self.selection.rect.is_none() || self.selection.dragging()))
            .then_some(self.cursor_pixel).flatten();
        let source = self.selection.size;
        let measure = self.bounds.clone();
        let handles = self.tool.is_none() && hover.is_none();
        let annotations = self
            .preview
            .as_ref()
            .filter(|_| self.tool.is_some())
            .map(|p| p.overlay.clone());
        let draft = self.draft_rect();
        let mut root = div()
            .id("capture-overlay")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(rgb(0x000000))
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .font_family("Geist")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e, w, cx| this.key(e, w, cx)))
            .on_mouse_move(cx.listener(|this, e, w, cx| {
                let _ = w;
                if this.selection.dragging() || this.drawing { this.motion(e, cx); }
                else if this.hovered.take().is_some() | this.cursor_pixel.take().is_some() { cx.notify(); }
            }))
            .capture_any_mouse_up(cx.listener(|this, e: &MouseUpEvent, _, cx| {
                if e.button == MouseButton::Left {
                    this.up(e, cx);
                }
            }))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, e, _, cx| this.up(e, cx)),
            )
            .child(
                div()
                    .id("overlay-canvas")
                    .absolute()
                    .inset_0()
                    .cursor(if self.tool == Some(a::Tool::Text) {
                        CursorStyle::IBeam
                    } else {
                        CursorStyle::Crosshair
                    })
                    .on_mouse_move(cx.listener(|this, e, _, cx| { this.motion(e, cx); cx.stop_propagation(); }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e, w, cx| {
                            this.down(e, w, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                        if this.pending.is_some() { return; }
                        let dy = f32::from(e.delta.pixel_delta(px(20.)).y);
                        if dy == 0. || this.tool.is_none() {
                            return;
                        }
                        let properties = this.preview.as_ref().and_then(|p| p.properties.as_ref());
                        if properties.is_some_and(|p| p.amount.is_some()) {
                            this.change_amount(dy.signum(), cx);
                        } else if properties.is_some_and(|p| p.font.is_some()) {
                            this.font = (this.font + dy.signum() * 2.).clamp(6., 256.);
                            this.send(Command::FontSize(this.font), cx);
                        } else if properties.is_some_and(|p| p.zoom.is_some()) {
                            this.change_zoom(dy.signum() * 0.5, cx);
                        } else {
                            this.width = (this.width + dy.signum()).clamp(1., 64.);
                            this.send(Command::Width(this.width), cx);
                        }
                    }))
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                measure.set(Some(bounds));
                                bounds
                            },
                            move |_, bounds, w, _| {
                                let lens = cursor.map(|(x, y)| {
                                    let pixels: [[Option<[u8; 4]>; 11]; 11] = std::array::from_fn(|row| std::array::from_fn(|column| {
                                        let sx = i64::from(x) + column as i64 - 5;
                                        let sy = i64::from(y) + row as i64 - 5;
                                        if sx < 0 || sy < 0 { None } else { image.sample_bgra(sx as u32, sy as u32) }
                                    }));
                                    (lens_bounds(bounds, source, (x, y)), pixels)
                                });
                                image.draw(bounds, w);
                                if startup.is_some() && startup_ready && !startup_painted.replace(true) {
                                    trace_overlay_startup(startup, "first_scene_paint", None);
                                }
                                let map = |x: f32, y: f32| {
                                    bounds.origin
                                        + point(
                                            bounds.size.width * x / source.0 as f32,
                                            bounds.size.height * y / source.1 as f32,
                                        )
                                };
                                let selected = selection.map(|r| {
                                    Bounds::new(
                                        map(r.x as f32, r.y as f32),
                                        size(
                                            bounds.size.width * r.width as f32 / source.0 as f32,
                                            bounds.size.height * r.height as f32 / source.1 as f32,
                                        ),
                                    )
                                });
                                if let Some(r) = selected {
                                    for b in [
                                        Bounds::new(
                                            bounds.origin,
                                            size(bounds.size.width, r.top() - bounds.top()),
                                        ),
                                        Bounds::new(
                                            point(bounds.left(), r.bottom()),
                                            size(bounds.size.width, bounds.bottom() - r.bottom()),
                                        ),
                                        Bounds::new(
                                            point(bounds.left(), r.top()),
                                            size(r.left() - bounds.left(), r.size.height),
                                        ),
                                        Bounds::new(
                                            point(r.right(), r.top()),
                                            size(bounds.right() - r.right(), r.size.height),
                                        ),
                                    ] {
                                        w.paint_quad(fill(b, rgba(0x00000077)));
                                    }
                                    dashed_region(w, r);
                                    if handles {
                                        for p in handle_points(r) {
                                            let b = Bounds::new(
                                                p - point(px(3.), px(3.)),
                                                size(px(6.), px(6.)),
                                            );
                                            w.paint_quad(fill(b, rgb(TEXT)));
                                            border(w, b, rgb(BG).into());
                                        }
                                    }
                                } else {
                                    w.paint_quad(fill(bounds, rgba(0x00000066)));
                                }
                                if let Some((lens, pixels)) = lens {
                                    w.paint_quad(fill(lens, rgb(0x111417)));
                                    let grid = Bounds::new(lens.origin + point(px(6.), px(6.)), size(px(88.), px(88.)));
                                    for (row, colors) in pixels.iter().enumerate() { for (column, color) in colors.iter().enumerate() {
                                        let color = color.map_or(0x33373d, |[b, g, r, _]| u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b));
                                        w.paint_quad(fill(Bounds::new(grid.origin + point(px(column as f32 * 8.), px(row as f32 * 8.)), size(px(7.), px(7.))), rgb(color)));
                                    }}
                                    let center = Bounds::new(grid.origin + point(px(40.), px(40.)), size(px(8.), px(8.)));
                                    border(w, center, rgb(0xffffff).into());
                                    border(w, lens, rgb(0xffffff).into());
                                }
                                if let Some(overlay) = annotations {
                                    let mut outlines = overlay.selection;
                                    if let Some(creation) =
                                        overlay.creation.filter(|_| draft.is_none())
                                    {
                                        outlines.push(creation);
                                    }
                                    if let Some(rect) = draft {
                                        outlines
                                            .push(a::input::OrientedRect { rect, rotation: 0. });
                                    }
                                    for s in outlines {
                                        let r = s.rect;
                                        crate::annotation_surface::outline(
                                            w,
                                            Bounds::new(
                                                map(r.x, r.y),
                                                size(
                                                    bounds.size.width * r.w / source.0 as f32,
                                                    bounds.size.height * r.h / source.1 as f32,
                                                ),
                                            ),
                                        );
                                    }
                                    if let Some(c) = overlay.caret {
                                        w.paint_quad(fill(
                                            Bounds::new(
                                                map(c.caret.x, c.caret.y),
                                                size(
                                                    px(1.),
                                                    bounds.size.height * c.caret.h
                                                        / source.1 as f32,
                                                ),
                                            ),
                                            rgb(TEXT),
                                        ));
                                    }
                                }
                            },
                        )
                        .size_full(),
                    ),
            );
        let area = self
            .bounds
            .get()
            .unwrap_or(Bounds::new(point(px(0.), px(0.)), window.viewport_size()));
        let r = selection.map(|s| {
            Bounds::new(
                area.origin
                    + point(
                        area.size.width * s.x as f32 / source.0 as f32,
                        area.size.height * s.y as f32 / source.1 as f32,
                    ),
                size(
                    area.size.width * s.width as f32 / source.0 as f32,
                    area.size.height * s.height as f32 / source.1 as f32,
                ),
            )
        });
        if let Some(draft) = draft
            && let Some(tool) = self.tool
            && matches!(
                tool,
                a::Tool::Blur
                    | a::Tool::Pixelate
                    | a::Tool::Highlight
                    | a::Tool::Spotlight
                    | a::Tool::Magnify
            )
        {
            let left = area.left() + area.size.width * draft.x / source.0 as f32;
            let top = area.top() + area.size.height * draft.y / source.1 as f32;
            root = root.child(
                div()
                    .absolute()
                    .left(left.clamp(
                        area.left() + px(8.),
                        (area.right() - px(200.)).max(area.left() + px(8.)),
                    ))
                    .top((top - px(28.)).clamp(
                        area.top() + px(8.),
                        (area.bottom() - px(28.)).max(area.top() + px(8.)),
                    ))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(0x111417))
                    .border_1()
                    .border_color(rgb(ACCENT))
                    .text_color(rgb(TEXT))
                    .child(format!(
                        "{} · {:.0} × {:.0}",
                        tool_label(tool),
                        draft.w,
                        draft.h
                    )),
            );
        }
        if let Some(r) = r {
            let label_width = px(320.).min((area.size.width - px(16.)).max(px(1.)));
            root = root.child(
                div()
                    .absolute()
                    .max_w(label_width)
                    .left(r.left().clamp(area.left() + px(8.), (area.right() - label_width - px(8.)).max(area.left() + px(8.))))
                    .top((r.top() - px(28.)).max(px(8.)))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgba(0x111417ee))
                    .child(format!("X: {}  Y: {}  W: {}  H: {}",
                        i64::from(self.physical.0) + i64::from(selection.unwrap().x),
                        i64::from(self.physical.1) + i64::from(selection.unwrap().y),
                        selection.unwrap().width, selection.unwrap().height)),
            );
        }
        if let Some(cursor) = cursor {
            let lens = lens_bounds(area, source, cursor);
            root = root.child(div().absolute().left(lens.left() + px(6.)).top(lens.top() + px(99.))
                .text_size(px(11.)).text_color(rgb(0xffffff))
                .child(format!("{}, {}", i64::from(self.physical.0) + i64::from(cursor.0), i64::from(self.physical.1) + i64::from(cursor.1))));
        }
        if self.toolbar_visible && self.pending.is_none() && !self.selection.dragging() && !self.drawing {
            // Hover is only a guide; controls move beside a region after it is committed.
            let r = self.selection.rect.and(r);
            let toolbar_width = px(800.).min((area.size.width - px(16.)).max(px(1.)));
            let x = r
                .map_or((area.size.width - toolbar_width) / 2., |r| r.left())
                .clamp(
                    px(8.),
                    (area.size.width - toolbar_width - px(8.)).max(px(8.)),
                );
            let (position, anchor) = if let Some(rect) = r {
                if rect.center().y > area.size.height / 2. {
                    (point(x, rect.top() - px(12.)), Corner::BottomLeft)
                } else {
                    (point(x, rect.bottom() + px(12.)), Corner::TopLeft)
                }
            } else {
                (point(x, area.bottom() - px(8.)), Corner::BottomLeft)
            };
            let bar = |id: &'static str| {
                div()
                    .id(id)
                    .rounded_lg()
                    .bg(rgba(0x17191df5))
                    .border_1()
                    .border_color(rgb(0x363a40))
                    .shadow_lg()
                    .p_1()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_1()
            };
            let mut tools = bar("overlay-tools").flex_wrap();
            tools = tools.child(
                button("region-tool", "Region")
                    .track_focus(&self.toolbar_focus)
                    .h(px(30.))
                    .tooltip(tooltip("Select or resize capture region · Tab"))
                    .bg(rgb(if self.tool.is_none() {
                        0x244234
                    } else {
                        0x17191d
                    }))
                    .on_click(cx.listener(|this, _, w, cx| this.choose_tool(None, w, cx))),
            );
            for (i, (tool, name)) in [
                (a::Tool::Select, "capture-select"),
                (a::Tool::Arrow, "capture-arrow"),
                (a::Tool::Rectangle, "capture-rect"),
                (a::Tool::Ellipse, "capture-ellipse"),
                (a::Tool::Freehand, "capture-pen"),
                (a::Tool::Text, "capture-text"),
                (a::Tool::Step, "capture-step"),
                (a::Tool::Highlight, "capture-highlight"),
                (a::Tool::Blur, "capture-blur"),
                (a::Tool::Pixelate, "capture-pixel"),
            ]
            .into_iter()
            .enumerate()
            {
                tools = tools.child(
                    button(("overlay-tool", i), "")
                        .child(icon(name).text_color(rgb(if self.tool == Some(tool) {
                            ACCENT
                        } else {
                            TEXT
                        })))
                        .h(px(30.))
                        .min_w(px(30.))
                        .px_2()
                        .bg(rgb(if self.tool == Some(tool) {
                            0x244234
                        } else {
                            0x17191d
                        }))
                        .tooltip(tooltip(tool_shortcut(tool)))
                        .on_click(
                            cx.listener(move |this, _, w, cx| this.choose_tool(Some(tool), w, cx)),
                        ),
                );
            }
            tools = tools
                .child(
                    button("overlay-more", "More")
                        .h(px(30.))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.more = !this.more;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("overlay-color")
                        .size(px(22.))
                        .rounded_full()
                        .bg(rgb(self.color))
                        .border_2()
                        .border_color(rgb(TEXT))
                        .tooltip(tooltip("Properties are shown below")),
                )
                .child(
                    button("overlay-undo", "")
                        .child(icon("undo"))
                        .h(px(30.))
                        .tooltip(tooltip("Undo · Ctrl+Z"))
                        .on_click(cx.listener(|this, _, _, cx| this.send(Command::Undo, cx))),
                )
                .child(
                    button("overlay-redo", "")
                        .child(icon("redo"))
                        .h(px(30.))
                        .tooltip(tooltip("Redo · Ctrl+Shift+Z"))
                        .on_click(cx.listener(|this, _, _, cx| this.send(Command::Redo, cx))),
                );
            let actions = bar("overlay-actions")
                .child(muted(if self.focus.is_focused(window) {
                    "F6: toolbar · Tab: region"
                } else {
                    "F6: canvas · Tab: controls"
                }))
                .child(div().flex_1())
                .child(
                    button("overlay-help", "?")
                        .tooltip(tooltip("Capture shortcuts · F1"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.help_visible = !this.help_visible;
                            cx.notify();
                        })),
                )
                .child(
                    button("overlay-cancel", "")
                        .child(icon("close"))
                        .tooltip(tooltip("Cancel capture"))
                        .on_click(cx.listener(|this, _, w, _| this.cancel(w))),
                )
                .when(self.configured, |s| {
                    s.child(button("overlay-workflow", "Run workflow").on_click(
                        cx.listener(|this, _, _, cx| this.finish(OverlayAction::Configured, cx)),
                    ))
                })
                .child(
                    button("overlay-save", "Save")
                        .child(icon("download"))
                        .tooltip(tooltip("Save selection · Ctrl+S"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.finish(OverlayAction::Save, cx)),
                        ),
                )
                .child(
                    button("overlay-copy", "Copy")
                        .child(icon("copy"))
                        .bg(rgb(ACCENT))
                        .text_color(rgb(BG))
                        .tooltip(tooltip("Copy selection · Ctrl+C / Enter"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.finish(OverlayAction::Copy, cx)),
                        ),
                );
            let mut floating = div()
                .id("floating-overlay-toolbar")
                .w(toolbar_width)
                .max_h((area.size.height - px(16.)).max(px(1.)))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_2()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(tools);
            if self.tool.is_some() {
                floating = floating.child(self.properties_bar(cx));
            }
            if self.more {
                let mut more = bar("overlay-more-tools").flex_wrap();
                for (i, tool) in [
                    a::Tool::Line,
                    a::Tool::Balloon,
                    a::Tool::Magnify,
                    a::Tool::Spotlight,
                ]
                .into_iter()
                .enumerate()
                {
                    more = more.child(button(("extra-tool", i), tool_shortcut(tool)).on_click(
                        cx.listener(move |this, _, w, cx| this.choose_tool(Some(tool), w, cx)),
                    ));
                }
                floating = floating.child(more);
                if self
                    .preview
                    .as_ref()
                    .and_then(|p| p.properties.as_ref())
                    .is_some_and(|p| p.selected > 0)
                {
                    floating = floating.child(
                        bar("overlay-selection-actions")
                            .flex_wrap()
                            .child(muted("Selected annotation"))
                            .child(
                                button("overlay-duplicate", "Duplicate")
                                    .track_focus(&self.duplicate_focus)
                                    .tooltip(tooltip("Duplicate selected annotation · Ctrl+D"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(Command::Duplicate, cx)
                                    })),
                            )
                            .child(button("overlay-front", "Bring to front").on_click(
                                cx.listener(|this, _, _, cx| this.send(Command::Front, cx)),
                            ))
                            .child(button("overlay-back", "Send to back").on_click(
                                cx.listener(|this, _, _, cx| this.send(Command::Back, cx)),
                            )),
                    );
                }
            }
            floating = floating.when_some(self.error.clone(), |s, error| {
                s.child(bar("overlay-error-details").min_w_0().max_h(px(120.)).overflow_y_scroll()
                    .text_color(rgb(0xffa0a0))
                    .child(div().flex_1().min_w_0().whitespace_normal().child(error)))
            }).child(actions);
            // GPUI measures the actual wrapped controls before snapping. Fixed height
            // estimates strand Save/Copy below the viewport at high DPI or narrow widths.
            root = root.child(
                anchored()
                    .position(position)
                    .anchor(anchor)
                    .snap_to_window_with_margin(px(8.))
                    .child(floating),
            );
        }
        if selection.is_none() {
            root = root.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(30.))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .rounded_lg()
                            .px_4()
                            .py_2()
                            .bg(rgba(0x17191def))
                            .child("Drag to select · Space: full screen · Esc: cancel"),
                    ),
            );
        }
        if self.help_visible && self.pending.is_none() && !self.drawing && !self.selection.dragging() {
            root = root.child(
                div()
                    .id("overlay-shortcuts")
                    .absolute()
                    .right_4()
                    .top_4()
                    .w(px(360.).min(area.size.width - px(32.)))
                    .max_h((area.size.height - px(32.)).max(px(100.)))
                    .overflow_y_scroll()
                    .p_4()
                    .rounded_lg()
                    .bg(rgba(0x17191df5))
                    .border_1()
                    .border_color(rgb(0x363a40))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child("Capture shortcuts")
                            .child(div().flex_1())
                            .child(button("close-overlay-help", "Close · F1").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.help_visible = false;
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(muted("Drag a region, then annotate, copy or save."))
                    .child("Tab  —  Return to region selection")
                    .child("F6  —  Focus toolbar / return to canvas")
                    .child("Tab / Shift+Tab  —  Move between toolbar controls")
                    .child("Arrows  —  Move selection by 1 px")
                    .child("Shift + arrows  —  Resize selection")
                    .child("Shift + drag  —  Constrain shape")
                    .child("Space  —  Select the full desktop")
                    .child("V select · A arrow · R rectangle · E ellipse")
                    .child("F pen · L line · T text · N numbered step")
                    .child("H highlight · B blur · P pixelate")
                    .child("O balloon · M magnify · S spotlight")
                    .child("Scroll  —  Adjust the active tool's property")
                    .child("Ctrl + Z  —  Undo · Ctrl + Shift + Z  —  Redo")
                    .child("Ctrl + D  —  Duplicate selected annotation")
                    .child("More  —  Selected annotation's layer order")
                    .child("Ctrl + C / Enter  —  Copy selection")
                    .child("Ctrl + S  —  Save selection")
                    .child("Ctrl + H  —  Hide or show toolbar")
                    .child("Esc  —  Finish edit / clear selection, then cancel")
                    .child("Ctrl + Q  —  Cancel capture"),
            );
        }
        if !self.toolbar_visible && self.pending.is_none() && !self.drawing && !self.selection.dragging() {
            root = root.child(
                div().absolute().left_4().bottom_4().child(
                    button("restore-overlay-toolbar", "Show tools · Ctrl+H")
                        .bg(rgba(0x17191df5))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toolbar_visible = true;
                            cx.notify();
                        })),
                ),
            );
        }
        root = root.when(
            self.preview
                .as_ref()
                .is_some_and(|preview| preview.detail_limited),
            |s| {
                s.child(
                    div()
                        .absolute()
                        .right_4()
                        .bottom_4()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgba(0x17191df5))
                        .child(muted(
                            "Preview detail reduced · saved capture stays full resolution",
                        )),
                )
            },
        );
        if let Some((_, action)) = self.pending {
            root = root.child(div().absolute().left_4().bottom_4().p_3().rounded_lg()
                .bg(rgb(PANEL)).flex().items_center().gap_3()
                .child(if self.completion.is_none() { "Preparing selection…" } else { match action {
                    OverlayAction::Copy => "Copying selection…",
                    OverlayAction::Save => "Saving selection…",
                    OverlayAction::Configured => "Running workflow…",
                } })
                .child(button("cancel-pending-overlay", "Cancel · Esc")
                    .on_click(cx.listener(|this, _, window, _| this.cancel(window)))));
        }
        root.when_some(self.error.clone().filter(|_| self.pending.is_none() &&
            (!self.toolbar_visible || self.drawing || self.selection.dragging())), |s, e| {
            s.child(
                div()
                    .id("overlay-error")
                    .absolute()
                    .left_4()
                    .bottom_4()
                    .p_3()
                    .max_w((area.size.width - px(32.)).max(px(1.)).min(px(640.)))
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .text_color(rgb(0xffa0a0))
                    .child(e),
            )
        })
    }
}
impl Drop for CaptureOverlay {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            self.cancel.cancel();
            let _ = reply.try_send(None);
        }
    }
}

#[cfg(test)]
mod region_guide_tests {
    use super::{lens_bounds, local_target};
    use gpui::{Bounds, point, px, size};
    use rcr_toolkit::types::Rect;
    #[test]
    fn physical_targets_translate_clip_and_preserve_negative_monitor_origins() {
        assert_eq!(local_target(Rect::new(-900, -250, 400, 200), (-640, -180, 900, 600)), Some(Rect::new(0, 0, 140, 130)));
        assert_eq!(local_target(Rect::new(-400, -60, 450, 200), (-640, -180, 900, 600)), Some(Rect::new(240, 120, 450, 200)));
        assert_eq!(local_target(Rect::new(i32::MAX - 10, i32::MAX - 10, u32::MAX, u32::MAX), (-640, -180, 900, 600)), None);
    }
    #[test]
    fn magnifier_stays_inside_each_corner_at_two_source_pixels_per_ui_pixel() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(900.), px(600.)));
        for cursor in [(0, 0), (1799, 0), (0, 1199), (1799, 1199)] {
            let lens = lens_bounds(bounds, (1800, 1200), cursor);
            assert!(lens.left() >= bounds.left() && lens.top() >= bounds.top());
            assert!(lens.right() <= bounds.right() && lens.bottom() <= bounds.bottom());
        }
        assert!(lens_bounds(bounds, (1800, 1200), (1799, 1199)).right() < px(899.5));
    }
}
