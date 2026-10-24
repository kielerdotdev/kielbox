//! Frozen-screen GPUI region picker; selection is always returned in desktop pixels.
use crate::{image_editor::frame_image, theme::*};
use gpui::{prelude::*, *};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use rcr_toolkit::{
    core::workflow::CancelToken,
    types::{Frame, Rect},
};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, mpsc},
    time::Duration,
};

pub struct RegionOverlay {
    image: Option<Arc<RenderImage>>,
    source: (i32, i32, u32, u32),
    focus: FocusHandle,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    start: Option<Point<Pixels>>,
    end: Option<Point<Pixels>>,
    reply: Option<mpsc::SyncSender<Option<Rect>>>,
    cancel: CancelToken,
}
pub fn open(
    frame: Frame,
    reply: mpsc::SyncSender<Option<Rect>>,
    cancel: CancelToken,
    cx: &mut App,
) -> anyhow::Result<()> {
    let scale = frame.scale_factor.max(0.5) as f32;
    let bounds = Bounds::new(
        point(
            px(frame.origin.x as f32 / scale),
            px(frame.origin.y as f32 / scale),
        ),
        size(
            px(frame.width() as f32 / scale),
            px(frame.height() as f32 / scale),
        ),
    );
    let physical = (
        frame.origin.x,
        frame.origin.y,
        i32::try_from(frame.width())?,
        i32::try_from(frame.height())?,
    );
    let overlay = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            kind: WindowKind::PopUp,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window);
                cx.spawn_in(window, async move |view, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                        if view
                            .update_in(cx, |this: &mut RegionOverlay, w, _| {
                                if this.cancel.is_cancelled() {
                                    w.remove_window();
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
                RegionOverlay {
                    source: (
                        frame.origin.x,
                        frame.origin.y,
                        frame.width(),
                        frame.height(),
                    ),
                    image: frame_image(frame),
                    focus,
                    bounds: Rc::new(Cell::new(None)),
                    start: None,
                    end: None,
                    reply: Some(reply),
                    cancel,
                }
            })
        },
    )?;
    let positioned = overlay.update(cx, |_, window, _| -> anyhow::Result<()> {
        let RawWindowHandle::Win32(handle) = window
            .window_handle()
            .map_err(|e| anyhow::anyhow!("Region picker handle: {e}"))?
            .as_raw()
        else {
            anyhow::bail!("Expected a Windows region-picker window");
        };
        use windows::Win32::{
            Foundation::HWND,
            UI::WindowsAndMessaging::{HWND_TOPMOST, SWP_SHOWWINDOW, SetWindowPos},
        };
        // A stitched frame has scale=1 even on mixed-DPI desktops. Place the owned popup
        // in physical desktop pixels, then GPUI's WM_SIZE/DPI handling sizes its canvas.
        unsafe {
            SetWindowPos(
                HWND(handle.hwnd.get() as *mut _),
                Some(HWND_TOPMOST),
                physical.0,
                physical.1,
                physical.2,
                physical.3,
                SWP_SHOWWINDOW,
            )?;
        }
        Ok(())
    })?;
    if let Err(error) = positioned {
        let _ = overlay.update(cx, |_, window, _| window.remove_window());
        return Err(error);
    }
    Ok(())
}
fn selected(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    source: (i32, i32, u32, u32),
) -> Option<Rect> {
    let x1 = (f32::from(start.x.min(end.x) - bounds.left()) / f32::from(bounds.size.width).max(1.))
        .clamp(0., 1.);
    let y1 = (f32::from(start.y.min(end.y) - bounds.top()) / f32::from(bounds.size.height).max(1.))
        .clamp(0., 1.);
    let x2 = (f32::from(start.x.max(end.x) - bounds.left()) / f32::from(bounds.size.width).max(1.))
        .clamp(0., 1.);
    let y2 = (f32::from(start.y.max(end.y) - bounds.top()) / f32::from(bounds.size.height).max(1.))
        .clamp(0., 1.);
    let left = (x1 * source.2 as f32).floor() as i32;
    let top = (y1 * source.3 as f32).floor() as i32;
    let width = ((x2 * source.2 as f32).ceil() as u32).saturating_sub(left as u32);
    let height = ((y2 * source.3 as f32).ceil() as u32).saturating_sub(top as u32);
    if width == 0 || height == 0 {
        return None;
    }
    Some(Rect::new(source.0 + left, source.1 + top, width, height))
}
impl Render for RegionOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let image = self.image.clone();
        let measure = self.bounds.clone();
        let selection = self.start.zip(self.end);
        let dimensions = selection
            .zip(self.bounds.get())
            .and_then(|((start, end), bounds)| selected(start, end, bounds, self.source))
            .map(|r| format!("{} × {} px · Release to capture", r.width, r.height));
        div()
            .id("screenshot-region-overlay")
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .cursor(CursorStyle::Crosshair)
            .bg(rgb(0x000000))
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, w, _| {
                if e.keystroke.key == "escape" {
                    if let Some(reply) = this.reply.take() {
                        let _ = reply.try_send(None);
                    }
                    w.remove_window();
                } else if matches!(e.keystroke.key.as_str(), "space" | "enter") {
                    let selection = this.start.zip(this.end).zip(this.bounds.get()).and_then(
                        |((start, end), bounds)| selected(start, end, bounds, this.source),
                    );
                    let rect = selection.unwrap_or_else(|| {
                        Rect::new(this.source.0, this.source.1, this.source.2, this.source.3)
                    });
                    if let Some(reply) = this.reply.take() {
                        let _ = reply.try_send(Some(rect));
                    }
                    w.remove_window();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, w, _| {
                    if let Some(reply) = this.reply.take() {
                        let _ = reply.try_send(None);
                    }
                    w.remove_window();
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.start = Some(e.position);
                    this.end = Some(e.position);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.start.is_some() {
                    this.end = Some(e.position);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, w, cx| {
                    let selection =
                        this.start
                            .zip(this.bounds.get())
                            .and_then(|(start, bounds)| {
                                selected(start, e.position, bounds, this.source)
                            });
                    if selection.is_some() {
                        if let Some(reply) = this.reply.take() {
                            let _ = reply.try_send(selection);
                        }
                        w.remove_window();
                    } else {
                        this.start = None;
                        this.end = None;
                        cx.notify();
                    }
                }),
            )
            .child(
                canvas(
                    move |area, _, _| measure.set(Some(area)),
                    move |area, _, window, _| {
                        if let Some(image) = image {
                            let _ = window.paint_image(area, Corners::default(), image, 0, false);
                        }
                        if let Some((start, end)) = selection {
                            let left = start.x.min(end.x).clamp(area.left(), area.right());
                            let right = start.x.max(end.x).clamp(area.left(), area.right());
                            let top = start.y.min(end.y).clamp(area.top(), area.bottom());
                            let bottom = start.y.max(end.y).clamp(area.top(), area.bottom());
                            for rect in [
                                Bounds::new(
                                    area.origin,
                                    size(area.size.width, (top - area.top()).max(px(0.))),
                                ),
                                Bounds::new(
                                    point(area.left(), bottom),
                                    size(area.size.width, (area.bottom() - bottom).max(px(0.))),
                                ),
                                Bounds::new(
                                    point(area.left(), top),
                                    size(
                                        (left - area.left()).max(px(0.)),
                                        (bottom - top).max(px(0.)),
                                    ),
                                ),
                                Bounds::new(
                                    point(right, top),
                                    size(
                                        (area.right() - right).max(px(0.)),
                                        (bottom - top).max(px(0.)),
                                    ),
                                ),
                            ] {
                                window.paint_quad(fill(rect, rgba(0x00000088)));
                            }
                            for rect in [
                                Bounds::new(point(left, top), size(right - left, px(1.))),
                                Bounds::new(point(left, bottom), size(right - left, px(1.))),
                                Bounds::new(point(left, top), size(px(1.), bottom - top)),
                                Bounds::new(point(right, top), size(px(1.), bottom - top)),
                            ] {
                                window.paint_quad(fill(rect, rgb(ACCENT)));
                            }
                        } else {
                            window.paint_quad(fill(area, rgba(0x00000044)));
                        }
                    },
                )
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top_4()
                    .left_4()
                    .px_4()
                    .py_2()
                    .rounded_lg()
                    .bg(rgba(0x131417dd))
                    .text_color(rgb(TEXT))
                    .font_family("Geist")
                    .child(dimensions.unwrap_or_else(|| {
                        "Drag to capture · Space: full desktop · Esc / right click: cancel".into()
                    })),
            )
    }
}
impl Drop for RegionOverlay {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn region_selection_scales_and_preserves_negative_monitor_origins() {
        let b = Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.)));
        assert_eq!(
            selected(
                point(px(960.), px(540.)),
                point(px(480.), px(270.)),
                b,
                (-3840, 0, 3840, 2160)
            ),
            Some(Rect::new(-2880, 540, 960, 540))
        );
        assert!(
            selected(
                point(px(10.), px(10.)),
                point(px(10.), px(50.)),
                b,
                (0, 0, 1920, 1080)
            )
            .is_none()
        );
    }
}
