//! The app page launches capture; selection lives on the desktop overlay.
use super::*;
impl QuickRecorder {
    fn mark(&self, element: Stateful<Div>, key: &'static str) -> Stateful<Div> {
        element.when_some(self.probe.clone(), |s, probe| {
            s.relative().child(menu_proof::measure(probe, key))
        })
    }
}
impl Render for QuickRecorder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .id("capture-launcher")
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap_4()
            .text_color(rgb(TEXT))
            .font_family("Geist")
            .text_size(px(13.))
            .when_some(self.probe.clone(), |s, probe| {
                s.relative().child(menu_proof::measure(probe, "launcher"))
            })
            .child(
                div()
                    .w_full()
                    .text_size(px(22.))
                    .child("Screenshot or record your screen"),
            )
            .child(muted("Choose an area, screen or window directly on your desktop.").w_full())
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        self.mark(
                            button_with_hover("new-capture", "New capture", 0x65dcaa)
                                .child(icon("camera").text_color(rgb(BG))),
                            "open-picker",
                        )
                        .h(px(40.))
                        .bg(rgb(ACCENT))
                        .text_color(rgb(BG))
                        .on_click(cx.listener(|this, _, _, cx| this.launch(false, cx))),
                    )
                    .child(
                        self.mark(
                            button("new-video", "Record video").child(icon("video-camera")),
                            "open-video-picker",
                        )
                        .h(px(40.))
                        .on_click(cx.listener(|this, _, _, cx| this.launch(true, cx))),
                    ),
            );
        if self.worker.is_some() || self.pending.is_some() {
            let seconds = self.stats.recorded.as_secs();
            let mut status = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .p_3()
                .rounded_lg()
                .bg(rgb(PANEL))
                .child(div().size(px(8.)).rounded_full().bg(rgb(if self.paused {
                    MUTED
                } else {
                    0xf45b69
                })))
                .child(div().flex_1().min_w_0().child(if self.finishing {
                    "Saving video…".into()
                } else if self.recording {
                    format!(
                        "{}  {:02}:{:02}",
                        if self.paused { "Paused" } else { "Recording" },
                        seconds / 60,
                        seconds % 60
                    )
                } else {
                    "Preparing recording…".into()
                }));
            if !self.finishing {
                if self.recording {
                    status = status
                        .child(
                            self.mark(
                                button(
                                    "capture-pause",
                                    if self.paused { "Resume" } else { "Pause" },
                                ),
                                "pause",
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.paused = !this.paused;
                                if let Some(worker) = &this.worker {
                                    worker.pause(this.paused);
                                }
                                cx.notify();
                            })),
                        )
                        .child(
                            self.mark(
                                button("capture-stop", "Stop & save").child(icon("stop")),
                                "stop",
                            )
                            .bg(rgb(0x8e2e3d))
                            .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                        );
                }
                status = status.child(
                    self.mark(button("capture-discard", "Discard"), "cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                );
            }
            root = root.child(status);
        }
        if self.snapshot.is_some() {
            root = root.child(muted("Preparing capture…"));
        }
        if let Some(path) = self.saved.clone() {
            root = root.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .child(icon("check").text_color(rgb(ACCENT)))
                    .child(div().flex_1().min_w_0().child("Video saved"))
                    .child(
                        button(
                            "capture-copy",
                            if self.copy_pending {
                                "Copying…"
                            } else {
                                "Copy file"
                            },
                        )
                        .child(icon("copy"))
                        .on_click(cx.listener(|this, _, window, cx| this.copy(window, cx))),
                    )
                    .child(button("capture-reveal", "Show file").on_click(cx.listener(
                        move |this, _, _, cx| {
                            if let Err(error) = recordings::begin_shell_action(
                                recordings::ShellAction::Reveal,
                                path.clone(),
                                cx,
                                |this, _, _, result, cx| {
                                    if let Err(error) = result {
                                        this.notice = error;
                                        cx.notify();
                                    }
                                },
                            ) {
                                this.notice = error;
                                cx.notify();
                            }
                        },
                    ))),
            );
        }
        if !self.notice.is_empty() {
            root = root.child(muted(self.notice.clone()).w_full().whitespace_normal());
        }
        for warning in self.warnings.iter().take(4) {
            root = root.child(muted(warning.clone()).w_full().whitespace_normal());
        }
        root
    }
}
