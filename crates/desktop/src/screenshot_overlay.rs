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
    selection: rcr_toolkit::selection::Selection,
    reply: Option<mpsc::SyncSender<Option<Rect>>>,
    cancel: CancelToken,
    recording: bool,
    probe: Option<crate::quick_recorder::menu_proof::Probe>,
}
pub fn open(
    frame: Frame,
    reply: mpsc::SyncSender<Option<Rect>>,
    cancel: CancelToken,
    cx: &mut App,
) -> anyhow::Result<()> {
    open_kind(frame, reply, cancel, false, cx)
}
fn open_kind(
    frame: Frame,
    reply: mpsc::SyncSender<Option<Rect>>,
    cancel: CancelToken,
    recording: bool,
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
                    selection: rcr_toolkit::selection::Selection::new(
                        frame.width(),
                        frame.height(),
                        false,
                    ),
                    image: frame_image(frame),
                    focus,
                    bounds: Rc::new(Cell::new(None)),
                    reply: Some(reply),
                    cancel,
                    recording,
                    probe: None,
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
#[cfg(test)]
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
impl RegionOverlay {
    fn point(&self, point: Point<Pixels>) -> Option<rcr_toolkit::selection::Point> {
        let bounds = self.bounds.get()?;
        Some(rcr_toolkit::selection::Point {
            x: ((f32::from(point.x - bounds.left()) / f32::from(bounds.size.width).max(1.))
                * self.source.2 as f32)
                .clamp(0., self.source.2 as f32),
            y: ((f32::from(point.y - bounds.top()) / f32::from(bounds.size.height).max(1.))
                * self.source.3 as f32)
                .clamp(0., self.source.3 as f32),
        })
    }
    fn confirm(&mut self, window: &mut Window) {
        let rect = self
            .selection
            .rect
            .map(|r| Rect::new(self.source.0 + r.x, self.source.1 + r.y, r.width, r.height));
        if rect.is_some() {
            if let Some(reply) = self.reply.take() {
                let _ = reply.try_send(rect);
            }
            window.remove_window();
        }
    }
    fn dismiss(&mut self, window: &mut Window) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(None);
        }
        window.remove_window();
    }
}
impl Render for RegionOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let image = self.image.clone();
        let measure = self.bounds.clone();
        let selection = self.selection.rect;
        let source = self.source;
        let mut root = div()
            .id("screenshot-region-overlay")
            .size_full()
            .relative()
            .font_family("Geist")
            .text_size(px(13.))
            .text_color(rgb(TEXT))
            .track_focus(&self.focus)
            .cursor(CursorStyle::Crosshair)
            .bg(rgb(0x000000))
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => this.dismiss(window),
                    "enter" | "space" => {
                        if !this.recording && this.selection.rect.is_none() {
                            this.selection.full();
                        }
                        this.confirm(window);
                    }
                    "left" => this.selection.nudge(-1, 0, e.keystroke.modifiers.shift),
                    "right" => this.selection.nudge(1, 0, e.keystroke.modifiers.shift),
                    "up" => this.selection.nudge(0, -1, e.keystroke.modifiers.shift),
                    "down" => this.selection.nudge(0, 1, e.keystroke.modifiers.shift),
                    _ => return,
                }
                cx.notify();
                window.prevent_default();
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, _| this.dismiss(window)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    if let Some(p) = this.point(e.position) {
                        let tolerance = this.bounds.get().map_or(8., |b| {
                            8. * this.source.2 as f32 / f32::from(b.size.width).max(1.)
                        });
                        this.selection.down(p, tolerance);
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
                cx.listener(|this, e: &MouseUpEvent, window, cx| {
                    if this.selection.dragging() {
                        if let Some(p) = this.point(e.position) {
                            this.selection.update(p, e.modifiers.shift);
                        }
                        this.selection.up();
                        if !this.recording {
                            this.confirm(window);
                        }
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
                        if let Some(rect) = selection {
                            let left =
                                area.left() + area.size.width * (rect.x as f32 / source.2 as f32);
                            let right =
                                left + area.size.width * (rect.width as f32 / source.2 as f32);
                            let top =
                                area.top() + area.size.height * (rect.y as f32 / source.3 as f32);
                            let bottom =
                                top + area.size.height * (rect.height as f32 / source.3 as f32);
                            for mask in [
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
                                    size((left - area.left()).max(px(0.)), bottom - top),
                                ),
                                Bounds::new(
                                    point(right, top),
                                    size((area.right() - right).max(px(0.)), bottom - top),
                                ),
                            ] {
                                window.paint_quad(fill(mask, rgba(0x00000088)));
                            }
                            for edge in [
                                Bounds::new(point(left, top), size(right - left, px(2.))),
                                Bounds::new(point(left, bottom), size(right - left, px(2.))),
                                Bounds::new(point(left, top), size(px(2.), bottom - top)),
                                Bounds::new(point(right, top), size(px(2.), bottom - top)),
                            ] {
                                window.paint_quad(fill(edge, rgb(TEXT)));
                            }
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
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(x - px(4.), y - px(4.)),
                                        size(px(8.), px(8.)),
                                    ),
                                    rgb(TEXT),
                                ));
                            }
                        } else {
                            window.paint_quad(fill(area, rgba(0x00000055)));
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
                    .bg(rgba(0x242529ee))
                    .text_color(rgb(TEXT))
                    .font_family("Geist")
                    .child(if let Some(rect) = selection {
                        format!(
                            "{} × {} · Drag to move · Handles to resize",
                            rect.width, rect.height
                        )
                    } else if self.recording {
                        "Drag an area to record · Esc to cancel".into()
                    } else {
                        "Drag to capture · Space: full desktop · Esc: cancel".into()
                    }),
            );
        if self.recording {
            root =
                root.child(
                    div()
                        .absolute()
                        .bottom(px(28.))
                        .left_0()
                        .w_full()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .id("record-area-bar")
                                .flex()
                                .items_center()
                                .gap_5()
                                .p_3()
                                .px_5()
                                .rounded(px(24.))
                                .bg(rgb(0x242529))
                                .border_1()
                                .border_color(rgb(0x45464f))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(button("cancel-record-area", "Cancel").on_click(
                                    cx.listener(|this, _, window, _| this.dismiss(window)),
                                ))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child("Record selection")
                                        .child(muted("Enter to start · Esc to cancel")),
                                )
                                .child(
                                    button("confirm-record-area", "")
                                        .when_some(self.probe.clone(), |s, probe| {
                                            s.relative().child(
                                                crate::quick_recorder::menu_proof::measure(
                                                    probe,
                                                    "area-confirm",
                                                ),
                                            )
                                        })
                                        .size(px(60.))
                                        .rounded_full()
                                        .border_2()
                                        .border_color(rgb(TEXT))
                                        .p_1()
                                        .opacity(if selection.is_some() { 1. } else { 0.4 })
                                        .child(div().size(px(46.)).rounded_full().bg(rgb(0xf45b69)))
                                        .on_click(
                                            cx.listener(|this, _, window, _| this.confirm(window)),
                                        ),
                                ),
                        ),
                );
        }
        root
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
