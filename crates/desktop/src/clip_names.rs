//! Capture display names live in preferences, independently of media filenames.
use crate::{
    preferences::validate_display_name,
    text_input::{SelectAll, TextInput},
    theme::*,
};
use gpui::{prelude::*, *};
use std::path::PathBuf;

pub enum NameEvent {
    Saved(PathBuf, String),
    Cancelled,
}

pub struct ClipNameDialog {
    path: PathBuf,
    input: Entity<TextInput>,
    cancel_focus: FocusHandle,
    save_focus: FocusHandle,
    error: Option<&'static str>,
    request_focus: bool,
}

impl EventEmitter<NameEvent> for ClipNameDialog {}

impl ClipNameDialog {
    pub fn new(path: PathBuf, current_name: String, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(cx, current_name);
            input.set_placeholder("Capture name", cx);
            input
        });
        cx.observe(&input, |this, _, cx| {
            this.error = None;
            cx.notify();
        })
        .detach();
        Self {
            path,
            input,
            cancel_focus: cx.focus_handle().tab_stop(true),
            save_focus: cx.focus_handle().tab_stop(true),
            error: None,
            request_focus: true,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        match validate_display_name(&self.input.read(cx).value()) {
            Ok(name) => cx.emit(NameEvent::Saved(self.path.clone(), name)),
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }
}

impl Render for ClipNameDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.request_focus {
            self.request_focus = false;
            let focus = self.input.read(cx).focus_handle(cx);
            focus.focus(window);
            // Selection uses TextInput's public action, after its focus node is painted.
            window.on_next_frame(move |window, cx| focus.dispatch_action(&SelectAll, window, cx));
        }

        div()
            .id("capture-name-modal")
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000a8))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" {
                    let input = this.input.read(cx).focus_handle(cx);
                    let current = if this.cancel_focus.is_focused(window) {
                        1
                    } else if this.save_focus.is_focused(window) {
                        2
                    } else {
                        0
                    };
                    let next = (current
                        + if event.keystroke.modifiers.shift {
                            2
                        } else {
                            1
                        })
                        % 3;
                    [&input, &this.cancel_focus, &this.save_focus][next].focus(window);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "enter" => {
                        if this.cancel_focus.is_focused(window) {
                            cx.emit(NameEvent::Cancelled);
                        } else {
                            this.save(cx);
                        }
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                    "escape" => {
                        cx.emit(NameEvent::Cancelled);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .child(
                div()
                    .w(px(440.))
                    .max_w_full()
                    .mx_4()
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .rounded(px(16.))
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(PANEL))
                    .text_color(rgb(TEXT))
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Rename capture"),
                    )
                    .child(muted("Name only. Original file stays in place."))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(MUTED))
                                    .child("Capture name"),
                            )
                            .child(
                                div()
                                    .p_2()
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(rgb(ACCENT))
                                    .bg(rgb(0x17181b))
                                    .child(self.input.clone()),
                            )
                            .children(self.error.map(|error| {
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(0xf87171))
                                    .child(error)
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(muted("Enter to save · Esc to cancel"))
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        button("cancel-capture-name", "Cancel")
                                            .track_focus(&self.cancel_focus)
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(NameEvent::Cancelled)
                                            })),
                                    )
                                    .child(
                                        button_with_hover(
                                            "save-capture-name",
                                            "Save name",
                                            0x55dda3,
                                        )
                                        .track_focus(&self.save_focus)
                                        .bg(rgb(ACCENT))
                                        .text_color(rgb(BG))
                                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                                    ),
                            ),
                    ),
            )
    }
}
