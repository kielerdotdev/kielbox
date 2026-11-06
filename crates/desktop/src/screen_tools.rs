//! Frozen-desktop sampling/ruler overlays and independent pinned image/monitor windows.
use crate::{image_editor::render_image, theme::*};
use gpui::{prelude::*, *};
use ::toolkit::{
    core::workflow::{CancelToken, CaptureRequest, CaptureTarget, Capturer},
    services::{LastRegionStore, ScreenCapturer},
    types::Frame,
    utilities,
};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, mpsc},
    time::Duration,
};
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Color,
    Ruler,
    Pin,
    Ocr,
    Qr,
}
pub struct Picked {
    pub mode: Mode,
    pub text: String,
    pub image: Option<Frame>,
}
struct Screen {
    dragging: bool,
    frame: Frame,
    image: Arc<RenderImage>,
    mode: Mode,
    focus: FocusHandle,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    start: Option<(u32, u32)>,
    end: (u32, u32),
    reply: Option<mpsc::Sender<anyhow::Result<Picked>>>,
    physical: (i32, i32, i32, i32),
}
pub fn launch(
    mode: Mode,
    window: &mut Window,
    cx: &mut App,
) -> mpsc::Receiver<anyhow::Result<Picked>> {
    let (tx, rx) = mpsc::channel();
    window.minimize_window();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(220))
            .await;
        let captured = cx
            .background_executor()
            .spawn(async move {
                let f = ScreenCapturer::detect(None, LastRegionStore::default())
                    .capture(
                        &CaptureRequest {
                            target: CaptureTarget::Fullscreen,
                            include_cursor: false,
                            hdr: Default::default(),
                        },
                        &CancelToken::new(),
                    )?
                    .frame
                    .into_rgba8()?;
                anyhow::ensure!(
                    f.width() as u64 * f.height() as u64 <= 32_000_000,
                    "Desktop exceeds the screen-tool pixel budget"
                );
                let mut bgra = f.data().to_vec();
                for p in bgra.as_chunks_mut::<4>().0 {
                    p.swap(0, 2);
                }
                Ok::<_, anyhow::Error>((f, bgra))
            })
            .await;
        match captured {
            Ok((frame, bgra)) => {
                let error_tx = tx.clone();
                if let Ok(Err(e)) = cx.update(|cx| open(frame, bgra, mode, tx, true, cx)) {
                    let _ = error_tx.send(Err(e));
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e));
            }
        }
    })
    .detach();
    rx
}
fn open(
    frame: Frame,
    bgra: Vec<u8>,
    mode: Mode,
    reply: mpsc::Sender<anyhow::Result<Picked>>,
    interactive: bool,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Screen>> {
    let physical = (
        frame.origin.x,
        frame.origin.y,
        frame.width() as i32,
        frame.height() as i32,
    );
    let image = render_image(frame.width(), frame.height(), bgra)
        .ok_or_else(|| anyhow::anyhow!("Invalid screen image"))?;
    let handle = cx.open_window(
        WindowOptions {
            show: false,
            focus: false,
            titlebar: None,
            kind: WindowKind::PopUp,
            is_resizable: false,
            is_movable: false,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(physical.0 as f32), px(physical.1 as f32)),
                size(px(physical.2 as f32), px(physical.3 as f32)),
            ))),
            ..Default::default()
        },
        |w, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(w);
                Screen {
                    dragging: false,
                    frame,
                    image,
                    mode,
                    focus,
                    bounds: Rc::new(Cell::new(None)),
                    start: None,
                    end: (0, 0),
                    reply: Some(reply),
                    physical,
                }
            })
        },
    )?;
    handle.update(cx, |_, w, cx| {
        crate::capture_overlay::native_place(w, physical, false, cx)
    })??;
    if interactive {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(20))
                .await;
            let _ = handle.update(cx, |v, w, cx| {
                crate::capture_overlay::native_place(w, v.physical, true, cx)
            });
        })
        .detach();
    }
    Ok(handle)
}
impl Screen {
    fn point(&self, p: Point<Pixels>) -> (u32, u32) {
        let Some(b) = self.bounds.get() else {
            return (0, 0);
        };
        (
            ((f32::from(p.x - b.left()) / f32::from(b.size.width) * self.frame.width() as f32)
                as u32)
                .min(self.frame.width() - 1),
            ((f32::from(p.y - b.top()) / f32::from(b.size.height) * self.frame.height() as f32)
                as u32)
                .min(self.frame.height() - 1),
        )
    }
    fn rect(&self) -> ::toolkit::types::Rect {
        let a = self.start.unwrap_or(self.end);
        let b = self.end;
        ::toolkit::types::Rect::new(
            a.0.min(b.0) as i32,
            a.1.min(b.1) as i32,
            a.0.abs_diff(b.0) + 1,
            a.1.abs_diff(b.1) + 1,
        )
    }
    fn text(&self) -> String {
        if self.mode == Mode::Color {
            let (x, y) = self.end;
            let p = &self.frame.row(y)[x as usize * 4..x as usize * 4 + 4];
            utilities::color_text(p.try_into().unwrap())
        } else {
            let a = self.start.unwrap_or(self.end);
            let r = self.rect();
            let (dx, dy) = (
                self.end.0 as f64 - a.0 as f64,
                self.end.1 as f64 - a.1 as f64,
            );
            format!(
                "X: {}  Y: {}\nWidth: {} px  Height: {} px\nDistance: {:.2} px  Angle: {:.2}°",
                r.x + self.frame.origin.x,
                r.y + self.frame.origin.y,
                r.width,
                r.height,
                dx.hypot(dy),
                dy.atan2(dx).to_degrees()
            )
        }
    }
    fn finish(&mut self, w: &mut Window) {
        let image = if matches!(self.mode, Mode::Pin | Mode::Ocr | Mode::Qr) {
            self.frame.crop(self.rect()).ok()
        } else {
            None
        };
        if let Some(tx) = self.reply.take() {
            let _ = tx.send(Ok(Picked {
                mode: self.mode,
                text: self.text(),
                image,
            }));
        }
        w.remove_window();
    }
}
impl Render for Screen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = self.bounds.clone();
        let image = self.image.clone();
        let mut root = div()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .cursor(CursorStyle::Crosshair)
            .child(
                canvas(
                    move |b, _, _| bounds.set(Some(b)),
                    move |b, _, w, _| {
                        let _ = w.paint_image(b, Corners::default(), image, 0, false);
                    },
                )
                .size_full(),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.mode == Mode::Color || this.dragging {
                    this.end = this.point(e.position);
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, w, cx| {
                    this.end = this.point(e.position);
                    if this.mode == Mode::Color {
                        this.finish(w);
                    } else {
                        this.start = Some(this.end);
                        this.dragging = true;
                    }
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, cx| {
                    this.end = this.point(e.position);
                    this.dragging = false;
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(
                |this, e: &KeyDownEvent, w, _| match e.keystroke.key.as_str() {
                    "escape" => {
                        this.reply.take();
                        w.remove_window();
                    }
                    "enter" => this.finish(w),
                    _ => {}
                },
            ));
        if let (Some(a), Some(b)) = (self.start, self.bounds.get()) {
            let rect = self.rect();
            root = root.child(
                div()
                    .absolute()
                    .left(b.size.width * a.0.min(self.end.0) as f32 / self.frame.width() as f32)
                    .top(b.size.height * a.1.min(self.end.1) as f32 / self.frame.height() as f32)
                    .w(b.size.width * rect.width as f32 / self.frame.width() as f32)
                    .h(b.size.height * rect.height as f32 / self.frame.height() as f32)
                    .border_2()
                    .border_color(rgb(ACCENT)),
            );
        }
        root.child(
            div()
                .absolute()
                .left_4()
                .top_4()
                .p_3()
                .rounded_lg()
                .bg(rgb(PANEL))
                .text_color(rgb(TEXT))
                .child(mono(self.text()))
                .child(muted(if self.mode == Mode::Color {
                    "Click: copy color · Esc: cancel"
                } else {
                    "Drag to measure/select · Enter: finish · Esc: cancel"
                })),
        )
    }
}

struct Pin {
    image: Arc<RenderImage>,
    focus: FocusHandle,
    zoom: f32,
}
pub fn pin(frame: Arc<Frame>, cx: &mut App) -> anyhow::Result<()> {
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                utilities::with_pool(|| {
                    let (w, h) = utilities::fit(frame.width(), frame.height(), 8192, 8192);
                    let frame = ::toolkit::effects::resize(
                        &frame,
                        w,
                        h,
                        ::toolkit::effects::ResizeFilter::Bilinear,
                    )?;
                    let mut bgra = frame.into_data();
                    for p in bgra.as_chunks_mut::<4>().0 {
                        p.swap(0, 2);
                    }
                    Ok::<_, anyhow::Error>((w, h, bgra))
                })
                .and_then(|r| r)
            })
            .await;
        match result {
            Ok((w, h, bgra)) => {
                if let Ok(Err(e)) = cx.update(|cx| open_pin(w, h, bgra, cx)) {
                    eprintln!("Pin to screen: {e:#}");
                }
            }
            Err(e) => eprintln!("Pin to screen: {e:#}"),
        }
    })
    .detach();
    Ok(())
}
fn open_pin(w: u32, h: u32, bgra: Vec<u8>, cx: &mut App) -> anyhow::Result<()> {
    let (display_w, display_h) = utilities::fit(w, h, 900, 650);
    let image = render_image(w, h, bgra).ok_or_else(|| anyhow::anyhow!("Invalid pinned image"))?;
    let handle = cx.open_window(
        WindowOptions {
            titlebar: None,
            kind: WindowKind::PopUp,
            is_resizable: true,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(80.), px(80.)),
                size(px(display_w as f32), px(display_h as f32 + 36.)),
            ))),
            ..Default::default()
        },
        |w, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(w);
                Pin {
                    image,
                    focus,
                    zoom: 1.,
                }
            })
        },
    )?;
    handle.update(cx, |_, window, cx| {
        crate::capture_overlay::native_place(
            window,
            (80, 80, display_w as i32, display_h as i32 + 36),
            true,
            cx,
        )
    })??;
    Ok(())
}
impl Render for Pin {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let image_size = self.image.size(0);
        let viewport = window.viewport_size();
        let fit = (f32::from(viewport.width) / image_size.width.0 as f32)
            .min((f32::from(viewport.height) - 36.).max(1.) / image_size.height.0 as f32)
            .min(1.);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, w, _| {
                if e.keystroke.key == "escape" {
                    w.remove_window();
                }
            }))
            .child(
                div()
                    .flex()
                    .h(px(36.))
                    .items_center()
                    .gap_2()
                    .px_2()
                    .child(
                        muted("Pinned image · drag to move")
                            .flex_1()
                            .on_mouse_down(MouseButton::Left, |_, w, _| w.start_window_move()),
                    )
                    .child(
                        button("pin-minus", "−").on_click(cx.listener(|this, _, _, cx| {
                            this.zoom = (this.zoom / 1.25).max(0.1);
                            cx.notify();
                        })),
                    )
                    .child(
                        button("pin-plus", "+").on_click(cx.listener(|this, _, _, cx| {
                            this.zoom = (this.zoom * 1.25).min(8.);
                            cx.notify();
                        })),
                    )
                    .child(button("pin-close", "×").on_click(|_, w, _| w.remove_window())),
            )
            .child(
                div()
                    .id("pin-image-scroll")
                    .flex_1()
                    .overflow_scroll()
                    .child(
                        img(self.image.clone())
                            .w(px(self.image.size(0).width.0 as f32 * self.zoom * fit))
                            .h(px(self.image.size(0).height.0 as f32 * self.zoom * fit)),
                    ),
            )
    }
}

struct Monitor {
    index: usize,
    focus: FocusHandle,
}
pub fn monitor(cx: &mut App) -> anyhow::Result<()> {
    let display = cx
        .primary_display()
        .ok_or_else(|| anyhow::anyhow!("No display"))?;
    cx.open_window(
        WindowOptions {
            titlebar: None,
            kind: WindowKind::PopUp,
            window_bounds: Some(WindowBounds::Fullscreen(display.bounds())),
            ..Default::default()
        },
        |w, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(w);
                Monitor { index: 0, focus }
            })
        },
    )?;
    Ok(())
}
impl Render for Monitor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = [
            0x000000, 0xffffff, 0xff0000, 0x00ff00, 0x0000ff, 0x808080, 0x202020,
        ];
        div()
            .size_full()
            .bg(rgb(colors[self.index]))
            .track_focus(&self.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.index = (this.index + 1) % 7;
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, w, cx| {
                if e.keystroke.key == "escape" {
                    w.remove_window();
                } else {
                    this.index = (this.index + 1) % 7;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .absolute()
                    .top_4()
                    .left_4()
                    .p_2()
                    .bg(rgba(0x111111cc))
                    .text_color(rgb(0xffffff))
                    .child("Monitor test · click / key: next color · Esc: close"),
            )
    }
}

/// Native synthetic input/paint only: never captures the desktop or writes the clipboard.
pub fn smoke(cx: &mut App) -> anyhow::Result<()> {
    for mode in [Mode::Color, Mode::Ruler, Mode::Pin] {
        let f = ::toolkit::effects::solid_frame(640, 360, [12, 34, 56, 255]);
        let mut bytes = f.data().to_vec();
        for p in bytes.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
        }
        let (tx, rx) = mpsc::channel();
        let handle = open(f, bytes, mode, tx, false, cx)?;
        let raw: AnyWindowHandle = handle.into();
        raw.update(cx, |root, w, cx| -> anyhow::Result<()> {
            let view = root.downcast::<Screen>().unwrap();
            w.draw(cx).clear();
            let b = view
                .read(cx)
                .bounds
                .get()
                .ok_or_else(|| anyhow::anyhow!("Screen tool not painted"))?;
            let from = b.origin + point(b.size.width * 0.2, b.size.height * 0.2);
            let to = b.origin + point(b.size.width * 0.7, b.size.height * 0.7);
            w.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: from,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            if mode != Mode::Color {
                w.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: to,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Default::default(),
                    }),
                    cx,
                );
                w.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position: to,
                        modifiers: Default::default(),
                        click_count: 1,
                    }),
                    cx,
                );
                w.draw(cx).clear();
                w.dispatch_event(
                    PlatformInput::KeyDown(KeyDownEvent {
                        keystroke: Keystroke::parse("enter")?,
                        is_held: false,
                    }),
                    cx,
                );
            }
            Ok(())
        })??;
        let result = rx.try_recv()??;
        if mode == Mode::Color {
            anyhow::ensure!(
                result.text.starts_with("#0C2238"),
                "Incorrect sampled color"
            );
        } else if mode == Mode::Pin {
            let frame = result
                .image
                .ok_or_else(|| anyhow::anyhow!("No selected pin image"))?;
            anyhow::ensure!(
                (319..=322).contains(&frame.width()) && (179..=182).contains(&frame.height()),
                "Wrong pin crop"
            );
        } else {
            anyhow::ensure!(
                result.text.contains("Distance:") && result.text.contains("Angle:"),
                "Missing ruler measurements"
            );
        }
    }
    println!("Screen tools smoke: color, ruler and pin-region native input passed");
    let image = render_image(240, 120, [56u8, 34, 12, 255].repeat(240 * 120)).unwrap();
    let pin = cx.open_window(
        WindowOptions {
            show: false,
            focus: false,
            ..Default::default()
        },
        |_, cx| {
            cx.new(|cx| Pin {
                image,
                focus: cx.focus_handle(),
                zoom: 1.,
            })
        },
    )?;
    let pin: AnyWindowHandle = pin.into();
    pin.update(cx, |_, w, cx| {
        w.draw(cx).clear();
        w.remove_window();
    })?;
    let monitor = cx.open_window(
        WindowOptions {
            show: false,
            focus: false,
            ..Default::default()
        },
        |_, cx| {
            cx.new(|cx| Monitor {
                index: 0,
                focus: cx.focus_handle(),
            })
        },
    )?;
    let monitor: AnyWindowHandle = monitor.into();
    monitor.update(cx, |root, w, cx| {
        let monitor = root.downcast::<Monitor>().unwrap();
        for i in 0..7 {
            monitor.update(cx, |v, cx| {
                v.index = i;
                cx.notify();
            });
            w.draw(cx).clear();
        }
        w.remove_window();
    })?;
    Ok(())
}
