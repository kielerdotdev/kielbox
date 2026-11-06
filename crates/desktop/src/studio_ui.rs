use crate::{Studio, recordings, theme::*};
use gpui::{prelude::*, *};
use std::sync::atomic::Ordering;

/// The exact visible context that requested a shell action. A delayed result
/// must not become an error for another clip or another Library search/page.
#[derive(PartialEq, Eq)]
pub(crate) struct ShellNavigation {
    tab: usize,
    root: std::path::PathBuf,
    library: (usize, usize, bool, usize, String),
    editor: (u64, bool, Option<std::path::PathBuf>),
    saved_replay: Option<std::path::PathBuf>,
}

fn audio_error_details(errors: &[String]) -> Vec<String> {
    errors.iter().take(4).map(|error| {
        if error.trim().is_empty() { return "Audio source reported an unspecified error.".into(); }
        let mut characters = error.chars();
        let mut detail = characters.by_ref().take(2048).collect::<String>();
        if characters.next().is_some() { detail.push('…'); }
        detail
    }).collect()
}
fn audio_recovery_hint(active: bool, stopping: bool) -> &'static str {
    if !active {
        "Last capture reported audio problems. Reselect the source or device before starting again."
    } else if stopping {
        "Capture is finishing. Saved audio may be incomplete. Reselect the source or device once capture stops."
    } else {
        "Audio may be incomplete. Save any needed replay before stopping, then reselect the source or device."
    }
}
fn audio_error_scroll_y(key: &str, current: f32, viewport: f32) -> Option<f32> {
    let page = (viewport - 16.).max(32.);
    Some(match key {
        "up" => (current + 24.).min(0.), "down" => current - 24.,
        "pageup" => (current + page).min(0.), "pagedown" => current - page,
        "home" => 0., "end" => -1_000_000., _ => return None,
    })
}

fn persist_hotkeys<E>(
    preferences: &mut crate::preferences::Preferences,
    bindings: [String; 7],
    persist: impl FnOnce(&crate::preferences::Preferences) -> Result<(), E>,
) -> Result<(), E> {
    let previous = std::mem::replace(&mut preferences.hotkeys, bindings);
    if let Err(error) = persist(preferences) {
        preferences.hotkeys = previous;
        return Err(error);
    }
    Ok(())
}

fn row(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl IntoElement,
) -> Div {
    div()
        .min_h(px(66.))
        .py_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .border_b_1()
        .border_color(rgba(0xffffff0d))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(title.into())
                .child(muted(description)),
        )
        .child(control)
}
impl Studio {
    pub(crate) fn open_display_picker(&mut self, cx: &mut Context<Self>) {
        if self.active || self.display_picker.is_some() || self.audio_picker.is_some() || self.audio_device_picker.is_some()
            || self.name_dialog.is_some() || self.trash_panel.is_some() { return; }
        let current = self.preferences.display.clone();
        let restore = self.display_focus.clone();
        let fixture = self.audio_picker_fixture;
        let picker = cx.new(|cx| crate::display_picker::DisplayPicker::new(current, restore, fixture, cx));
        cx.subscribe(&picker, |this, picker, event: &crate::display_picker::PickerEvent, cx| {
            if this.display_picker.as_ref().is_none_or(|current| current.entity_id() != picker.entity_id()) { return; }
            if !this.active && let crate::display_picker::PickerEvent::Selected(selected) = event {
                let previous = this.preferences.display.clone();
                this.preferences.display = selected.clone();
                if !this.audio_picker_fixture && !this.save_preferences() { this.preferences.display = previous; }
            }
            this.display_picker = None; cx.notify();
        }).detach();
        self.display_picker = Some(picker); cx.notify();
    }
    pub(crate) fn shell_navigation(&self, cx: &Context<Self>) -> ShellNavigation {
        ShellNavigation {
            tab: self.tab,
            root: recordings::root(),
            library: (self.library.page, self.library.filter, self.library.favorites_only,
                self.library.sort, self.search.read(cx).value().to_owned()),
            editor: self.editor.read(cx).shell_identity(),
            saved_replay: self.saved_replay.clone(),
        }
    }
    pub(crate) fn open_shell_action(&mut self, kind: recordings::ShellAction, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let navigation = self.shell_navigation(cx);
        match recordings::begin_shell_action(kind, path, cx, move |this, operation, path, result, cx| {
            if this.shell_operation != operation { return; }
            this.shell_operation = 0;
            let source_current = this.tab != 1 || this.files.iter().any(|source| source == path);
            if this.notice.as_deref() == Some("Opening folder…") {
                this.notice = if this.shell_navigation(cx) == navigation && source_current { result.err() } else { None };
            }
            cx.notify();
        }) {
            Ok(operation) => { self.shell_operation = operation; self.notice = Some("Opening folder…".into()); }
            Err(error) => self.notice = Some(error),
        }
        cx.notify();
    }
    fn restore_hotkeys(&mut self, cx: &mut Context<Self>) {
        self.hotkeys.take();
        match crate::hotkeys::Hotkeys::start_with(&self.preferences.hotkeys) {
            Ok(hotkeys) => self.hotkeys = Some(hotkeys),
            Err(error) => {
                self.notice = Some(format!("Could not register global shortcuts: {error}"))
            }
        }
        cx.notify();
    }
    pub(crate) fn cancel_hotkey_edit(&mut self, cx: &mut Context<Self>) {
        if self.hotkey_edit.take().is_some() {
            self.hotkey_blur.take();
            self.restore_hotkeys(cx);
        }
    }
    fn apply_hotkeys(&mut self, bindings: [String; 7], window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = crate::hotkeys::validate(&bindings) {
            self.notice = Some(error);
            cx.notify();
            return;
        }
        // Match save_preferences' current audio state without committing the
        // runtime shortcut registration before the atomic preference write.
        self.preferences.system_audio = self.system_audio;
        self.preferences.microphone = self.microphone;
        self.preferences.clean_microphone = self.suppress_microphone;
        let saved = persist_hotkeys(&mut self.preferences, bindings, crate::preferences::Preferences::save);
        let was_editing = self.hotkey_edit.take().is_some();
        self.hotkey_blur.take();
        self.notice = None;
        self.restore_hotkeys(cx);
        if let Err(error) = saved {
            let registration = self.notice.take();
            self.notice = Some(format!("Could not save shortcuts: {error}. Previous bindings were kept. Assign the shortcut again to retry.{}",
                registration.map_or_else(String::new, |detail| format!(" {detail}"))));
        }
        if was_editing { self.hotkey_focus.focus(window); }
        cx.notify();
    }
    fn shortcut_name(&self, index: usize) -> String {
        let text = &self.preferences.hotkeys[index];
        if text.is_empty() {
            "Disabled".into()
        } else {
            text.clone()
        }
    }
    fn hotkey_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.hotkey_edit else {
            return;
        };
        cx.stop_propagation();
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.cancel_hotkey_edit(cx);
            return;
        }
        if matches!(
            key,
            "control" | "ctrl" | "alt" | "shift" | "super" | "win" | "meta"
        ) {
            return;
        }
        let binding = if key == "backspace" {
            Ok(String::new())
        } else {
            crate::hotkeys::from_keystroke(&event.keystroke)
        };
        match binding {
            Ok(binding) => {
                let mut bindings = self.preferences.hotkeys.clone();
                bindings[index] = binding;
                self.apply_hotkeys(bindings, window, cx);
            }
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
            }
        }
    }
    fn audio_device_control(
        &self,
        kind: rcr_windows::audio_endpoint::AudioEndpointKind,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        use rcr_windows::audio_endpoint::AudioEndpointKind;
        let (id, selected, focus, default_label) = match kind {
            AudioEndpointKind::Render => (
                "desktop-audio-device",
                &self.preferences.system_audio_device,
                &self.system_device_focus,
                "Default output device",
            ),
            AudioEndpointKind::Capture => (
                "microphone-input-device",
                &self.preferences.microphone_device,
                &self.microphone_device_focus,
                "Default input device",
            ),
        };
        let label = selected
            .as_ref()
            .map_or_else(|| default_label.to_owned(), |device| device.display_label());
        div().flex().flex_col().min_w_0().w_full().child(
            button(id, "")
                .track_focus(focus)
                .tab_stop(!self.active)
                .h(px(if compact { 24. } else { 28. }))
                .px_0()
                .w_full()
                .min_w_0()
                .justify_start()
                .bg(rgb(PANEL))
                .text_size(px(12.))
                .text_color(rgb(MUTED))
                .opacity(if self.active { 0.45 } else { 1. })
                .child(div().flex().flex_1().min_w_0().truncate().child(label))
                .child(icon("chevron").flex_shrink_0())
                .on_click(
                    cx.listener(move |this, _, _, cx| this.open_audio_device_picker(kind, cx)),
                ),
        )
    }
    fn audio_controls(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let mut sources = div().flex().flex_col().min_w_0().w_full().gap_2();
        for (i, name, detail, enabled, icon_name) in [
            (
                0,
                "Desktop audio",
                "Game, chat and music",
                self.system_audio,
                "speaker",
            ),
            (
                1,
                "Microphone",
                "Default input device",
                self.microphone,
                "mic",
            ),
        ] {
            sources = sources.child(
                div()
                    .flex()
                    .min_w_0()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .py_2()
                    .when(compact, |s| s.py_1())
                    .child(icon(icon_name).flex_shrink_0().text_color(rgb(if enabled { ACCENT } else { MUTED })))
                    .child(if i == 0 {
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                button("capture-audio-source", "")
                                    .track_focus(&self.audio_source_focus)
                                    .tab_stop(!self.active)
                                    .h(px(if compact { 24. } else { 32. }))
                                    .px_0()
                                    .w_full()
                                    .min_w_0()
                                    .justify_start()
                                    .bg(rgb(PANEL))
                                    .opacity(if self.active { 0.45 } else { 1. })
                                    .child(
                                        div()
                                            .flex()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(self.audio_choice.label().to_string()),
                                    )
                                    .child(icon("chevron").flex_shrink_0())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.open_audio_picker(cx)),
                                    ),
                            )
                            .when(
                                matches!(
                                    self.audio_choice,
                                    crate::audio_source_picker::AudioChoice::Desktop
                                ),
                                |s| {
                                    s.child(self.audio_device_control(
                                        rcr_windows::audio_endpoint::AudioEndpointKind::Render,
                                        compact,
                                        cx,
                                    ))
                                },
                            )
                            .when(!compact, |s| {
                                s.child(muted(match &self.audio_choice {
                                    crate::audio_source_picker::AudioChoice::Desktop => detail,
                                    _ => "Includes child processes · reselect after relaunch",
                                }))
                            })
                    } else {
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(name)
                            .child(self.audio_device_control(
                                rcr_windows::audio_endpoint::AudioEndpointKind::Capture,
                                compact,
                                cx,
                            ))
                    })
                    .child(
                        toggle(("capture-audio", i as usize), enabled)
                            .tab_stop(!self.active)
                            .opacity(if self.active { 0.45 } else { 1. })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.active {
                                    if i == 0 {
                                        this.system_audio = !this.system_audio;
                                    } else {
                                        this.microphone = !this.microphone;
                                    }
                                    this.save_preferences();
                                    cx.notify();
                                }
                            })),
                    ),
            );
        }
        sources
    }
    fn capture_ui(&mut self, window: &Window, cx: &mut Context<Self>) -> Div {
        let compact = f32::from(window.viewport_size().height) < 800.;
        let capture_panel = || panel().when(compact, |s| s.p_3().gap_2());
        let mode = self.capture_state().mode();
        let recording = self.active && self.record_to_disk;
        let stopping = self.active && self.stop.load(Ordering::Relaxed);
        let ready = self.active && !stopping && self.stats.encoded > 0;
        let profile = if self.active {
            self.session_profile
        } else {
            (
                self.preferences.height,
                self.preferences.fps,
                self.preferences.replay_seconds,
            )
        };
        let selected_display = if self.active { &self.session_display } else { &self.preferences.display };
        let display_label = selected_display.as_ref().map_or_else(|| if self.active {
            "Primary display at capture start".to_owned()
        } else { "Windows primary display".to_owned() }, |display| if self.active {
            format!("Configured at start: {}", display.display_label())
        } else { display.display_label() });
        let buffered = if self.active {
            self.stats.replay_seconds
        } else {
            0.
        };
        let progress = (buffered / profile.2.max(1) as f64).clamp(0., 1.) as f32;
        let state = if stopping {
            "Finishing capture"
        } else if mode == crate::tray::Mode::Starting && recording {
            "Starting recording…"
        } else if mode == crate::tray::Mode::Starting {
            "Starting replay…"
        } else if recording {
            "Recording"
        } else if ready {
            "Replay ready"
        } else if self.active {
            "Starting capture"
        } else {
            "Capture is off"
        };
        let state_color = if matches!(mode, crate::tray::Mode::Starting | crate::tray::Mode::Stopping) {
            0xfbbf24
        } else if recording {
            0xf08b8b
        } else if self.active {
            ACCENT
        } else {
            MUTED
        };
        let preview_error = self
            .preview_error
            .clone()
            .or_else(|| self.surface.as_ref().and_then(WindowsGpuSurface::error));
        let preview: AnyElement = if let Some(error) = preview_error {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p_6()
                .gap_3()
                .child(icon("monitor").size(px(28.)))
                .child(
                    div()
                        .text_size(px(18.))
                        .font_weight(FontWeight::MEDIUM)
                        .child("Preview unavailable"),
                )
                .child(muted(if self.active {
                    "Capture continues. You can still save replay or stop recording."
                } else {
                    "The preview could not be displayed."
                }))
                .child(
                    div()
                        .max_w(px(480.))
                        .text_size(px(12.))
                        .text_color(rgb(MUTED))
                        .child(error),
                )
                .into_any_element()
        } else if let Some(surface) = &self.surface {
            gpui::surface(surface.clone())
                .size_full()
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .size(px(64.))
                        .rounded(px(18.))
                        .bg(rgb(PANEL))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon("monitor").size(px(28.))),
                )
                .child(
                    div()
                        .text_size(px(18.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(match mode {
                            crate::tray::Mode::Off => "Catch what happens next",
                            crate::tray::Mode::Starting if recording => "Starting recording…",
                            crate::tray::Mode::Starting => "Starting replay…",
                            crate::tray::Mode::Stopping => "Finishing capture…",
                            crate::tray::Mode::Recording => "Recording is active",
                            crate::tray::Mode::Replay => "Replay is active",
                        }),
                )
                .child(muted(match mode {
                    crate::tray::Mode::Off => "Enable replay to save a moment after it happens.",
                    crate::tray::Mode::Starting => "Waiting for capture and encoder output.",
                    crate::tray::Mode::Stopping => "Finishing accepted capture work.",
                    _ => "Capture is running without a preview surface.",
                }))
                .when(!self.active, |s| s.child(muted("Or start recording to keep the whole session.")))
                .into_any_element()
        };
        // Rendering runs at capture cadence. Do not stat every recording on every frame.
        let paths = self.files.iter().take(3).cloned().collect::<Vec<_>>();
        let mut recent = div().flex().gap_3();
        if paths.is_empty() {
            recent = recent.child(
                div()
                    .h(px(118.))
                    .w_full()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(muted("Your first saved capture will appear here.")),
            );
        }
        for (i, path) in paths.into_iter().enumerate() {
            let title = self.capture_title(&path);
            let thumb = self.library.thumbnail(&path);
            recent = recent.child(
                div()
                    .id(("recent", i))
                    .tab_index(0)
                    .focus(|s| s.border_color(rgb(ACCENT)))
                    .flex_1()
                    .min_w_0()
                    .rounded_lg()
                    .overflow_hidden()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(LINE))
                    .cursor_pointer()
                    .hover(|s| s.border_color(rgb(0x43534c)))
                    .child(div().h(px(88.)).child(thumb))
                    .child(div().p_2().text_size(px(12.)).truncate().child(title))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_recording(path.clone(), cx)),
                    ),
            );
        }
        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .child(button("capture-display", "").track_focus(&self.display_focus).w_full().min_w_0()
                .child(div().flex_1().min_w_0().truncate().child(display_label))
                .opacity(if self.active { 0.5 } else { 1. })
                .on_click(cx.listener(|this, _, _, cx| this.open_display_picker(cx))))
            .child(
                div()
                    .flex_1()
                    .min_h(px(180.))
                    .rounded(px(12.))
                    .bg(rgb(0x070809))
                    .border_1()
                    .border_color(rgb(LINE))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(42.))
                            .flex_shrink_0()
                            .px_4()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(rgb(LINE))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(icon("monitor"))
                                    .child("Desktop preview"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().size(px(6.)).rounded_full().bg(rgb(state_color)))
                                    .child(muted(state).text_color(rgb(state_color))),
                            ),
                    )
                    .child(div().flex_1().min_h_0().overflow_hidden().child(preview))
                    .child(
                        div()
                            .h(px(38.))
                            .flex_shrink_0()
                            .px_4()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_t_1()
                            .border_color(rgb(LINE))
                            .child(mono(format!(
                                "{}p  /  {} fps  /  AV1",
                                profile.0, profile.1
                            )))
                            .child(muted(if mode == crate::tray::Mode::Starting {
                                "Waiting for first frame"
                            } else if stopping {
                                "Finishing capture"
                            } else if recording {
                                "Saving full session"
                            } else if ready {
                                "Rolling replay buffer"
                            } else {
                                "Selected display"
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child("Recent captures")
                    .child(
                        button("capture-library", "View library")
                            .h(px(28.))
                            .bg(rgb(BG))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.tab = 1;
                                this.refresh_files();
                                cx.notify();
                            })),
                    ),
            )
            .child(recent);
        let replay_label = if stopping {
            "Finishing…"
        } else if !self.active {
            "Enable instant replay"
        } else if !ready {
            "Starting replay…"
        } else {
            "Save replay"
        };
        let right = div()
            .w(px(320.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                capture_panel()
                    .border_1()
                    .border_color(rgb(if ready { 0x294c3b } else { LINE }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Instant replay"),
                            )
                            .child(
                                muted(if stopping {
                                    "STOPPING"
                                } else if ready {
                                    "ON"
                                } else if self.active {
                                    "STARTING"
                                } else {
                                    "OFF"
                                })
                                .text_color(rgb(if matches!(mode, crate::tray::Mode::Starting | crate::tray::Mode::Stopping) {
                                    0xfbbf24
                                } else if self.active {
                                    ACCENT
                                } else {
                                    MUTED
                                })),
                            ),
                    )
                    .when(!compact, |s| s.child(muted(format!(
                        "Save up to the last {} seconds as a clip. Your rolling buffer stays ready.",
                        profile.2
                    ))))
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(32.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!("{:.0}s", buffered)),
                            )
                            .child(muted(format!("/ {}s ready", profile.2)).pb_1()),
                    )
                    .child(
                        div()
                            .h(px(4.))
                            .rounded_full()
                            .overflow_hidden()
                            .bg(rgb(SOFT))
                            .child(div().h_full().w(relative(progress)).bg(rgb(ACCENT))),
                    )
                    .child(
                        button("save-replay", replay_label)
                            .h(px(40.))
                            .w_full()
                            .bg(rgb(ACCENT))
                            .text_color(rgb(BG))
                            .opacity(if stopping || (self.active && !ready) {
                                0.5
                            } else {
                                1.
                            })
                            .tab_stop(!stopping && (!self.active || ready))
                            .child(
                                icon(if self.active { "download" } else { "play" })
                                    .text_color(rgb(BG)),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.active {
                                    if !this.stop.load(Ordering::Relaxed) && this.stats.encoded > 0
                                    {
                                        this.save_replay(cx);
                                    }
                                } else {
                                    this.record_to_disk = false;
                                    this.start(cx);
                                }
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(mono(self.shortcut_name(if self.active {1} else {0})))
                            .when(self.active && !recording && !stopping, |s| {
                                s.child(
                                    button("replay-switch", "Turn off")
                                        .h(px(26.))
                                        .px_2()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.stop.store(true, Ordering::Relaxed);
                                            cx.notify();
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                capture_panel()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Full recording"),
                            )
                            .child(icon("record").text_color(rgb(if mode == crate::tray::Mode::Starting && recording {
                                0xfbbf24
                            } else if mode == crate::tray::Mode::Recording {
                                0xf08b8b
                            } else {
                                MUTED
                            }))),
                    )
                    .when(!compact || self.active, |s| s.child(muted(if mode == crate::tray::Mode::Starting && recording {
                        "Waiting for the first encoded frame. You can cancel below."
                    } else if stopping {
                        "Finishing capture and saving accepted output."
                    } else if recording {
                        "Recording to disk. Replay remains available."
                    } else if self.active {
                        "Starting a recording restarts your replay buffer."
                    } else {
                        "Keep everything from start to stop."
                    })))
                    .when(mode == crate::tray::Mode::Recording, |s| {
                        s.child(
                            mono(format!(
                                "{:02}:{:02}  ·  {:.1} MB",
                                self.stats.elapsed_seconds as u64 / 60,
                                self.stats.elapsed_seconds as u64 % 60,
                                self.stats.compressed_bytes as f64 / 1e6
                            ))
                            .text_color(rgb(0xf08b8b)),
                        )
                    })
                    .child(
                        button(
                            "record-toggle",
                            if stopping {
                                "Finishing…"
                            } else if mode == crate::tray::Mode::Starting && recording {
                                "Cancel starting recording"
                            } else if recording {
                                "Stop and save recording"
                            } else {
                                "Start recording"
                            },
                        )
                        .h(px(36.))
                        .w_full()
                        .opacity(if stopping { 0.5 } else { 1. })
                        .tab_stop(!stopping)
                        .child(
                            icon(if recording { "stop" } else { "record" })
                                .text_color(rgb(if recording { 0xf08b8b } else { TEXT })),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.stop.load(Ordering::Relaxed) || !this.active {
                                this.toggle_record(cx);
                            }
                        })),
                    )
                    .child(mono(self.shortcut_name(2))),
            )
            .child(
                capture_panel()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child("Audio sources")
                            .child(muted(if !self.stats.audio_errors.is_empty() { "Check audio" } else { match u8::from(self.system_audio) + u8::from(self.microphone) {
                                0 => "No audio", 1 => "1 track", _ => "2 tracks"
                            }})),
                    )
                    .child(self.audio_controls(compact, cx))
                    .when(!compact || self.active, |s| s.child(muted(if self.active {
                        "Stop capture to change sources."
                    } else {
                        "Sources stay separate for editing."
                    })))
                    .when(self.microphone && self.suppress_microphone, |s| {
                        s.child(muted("Mic cleanup applies to the next saved replay. Original mic audio stays unchanged."))
                    })
                    .when(!self.stats.audio_errors.is_empty(), |s| {
                        s.child(div().flex().flex_col().gap_2().min_w_0()
                            .child(muted(audio_recovery_hint(self.active, stopping)).text_color(rgb(0xf0b48b)))
                            .child(div().id("capture-audio-errors").max_h(px(if compact { 80. } else { 120. }))
                                .track_focus(&self.audio_error_focus).track_scroll(&self.audio_error_scroll)
                                .border_1().border_color(rgb(if self.audio_error_focus.is_focused(window) { ACCENT } else { LINE }))
                                .overflow_y_scroll().p_2().rounded_md().bg(rgb(BG)).text_size(px(11.))
                                .flex().flex_col().gap_2()
                                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                                    this.audio_error_focus.focus(window); cx.notify();
                                }))
                                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                    if !this.audio_error_focus.is_focused(window) || event.keystroke.key == "tab" { return; }
                                    if event.keystroke.key == "escape" { window.focus_next(); }
                                    else if let Some(y) = audio_error_scroll_y(&event.keystroke.key,
                                        f32::from(this.audio_error_scroll.offset().y),
                                        f32::from(this.audio_error_scroll.bounds().size.height)) {
                                        let mut offset = this.audio_error_scroll.offset(); offset.y = px(y);
                                        this.audio_error_scroll.set_offset(offset);
                                    }
                                    // While the details own focus, local capture shortcuts
                                    // must not consume a user's diagnostic navigation.
                                    window.prevent_default(); cx.stop_propagation(); cx.notify();
                                }))
                                .children(audio_error_details(&self.stats.audio_errors).into_iter().map(|detail|
                                    div().min_w_0().flex_shrink_0().child(detail))))
                            .child(muted("Scroll details · Tab to focus · ↑/↓ / Page keys · Esc to leave").text_size(px(10.)))
                            .when(self.stats.audio_errors.len() > 4, |s| s.child(muted(format!(
                                "{} additional audio errors.", self.stats.audio_errors.len() - 4)))))
                    }),
            )
            .child(
                button("capture-settings", "Capture settings")
                    .w_full()
                    .bg(rgb(BG))
                    .child(icon("chevron"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tab = 3;
                        cx.notify();
                    })),
            )
            .when(!self.active, |s| s.child(muted("Windows · NVIDIA GPU with AV1 NVENC support · SDR desktop capture").text_size(px(11.))));
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .p_6()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(24.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Capture"),
                            )
                            .child(muted("Keep the moment. Get back to your game.")),
                    )
                    .child(
                        button("capture-screenshot", "Screenshot")
                            .child(icon("capture-rect"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.toolkit.update(cx, |toolkit, cx| {
                                    toolkit.capture(
                                        rcr_toolkit::core::workflow::CaptureTarget::Region,
                                        cx,
                                    )
                                }) {
                                    window.minimize_window();
                                }
                            })),
                    ),
            )
            .child(
                div().flex_1().min_h_0().flex().gap_5().child(left).child(
                    div()
                        .id("capture-options")
                        .h_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.capture_options_scroll)
                        .child(right),
                ),
            )
    }
    fn settings_ui(&self, cx: &mut Context<Self>) -> Div {
        let installation_bounds = self.installation_bounds.clone();
        let checking = self.setup_inspection.is_some();
        let cancelling = self.setup_inspection.as_ref().is_some_and(|check| check.cancel.is_cancelled());
        let mut installation = div().flex().flex_col().gap_2()
            .child(muted("CHECK INSTALLATION"))
            .child(muted("Inspect media tools and data locations. This does not start recording or change settings."))
            .child(div().flex().gap_2()
                .child(button("check-installation", if cancelling { "Cancelling…" } else if checking { "Checking…" } else { "Check installation" })
                    .tab_stop(!checking).opacity(if checking { 0.5 } else { 1. })
                    .on_click(cx.listener(|this, _, _, cx| this.check_installation(cx))))
                .when(checking && !cancelling, |row| row.child(button("cancel-installation-check", "Cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(check) = &this.setup_inspection { check.cancel.cancel(); }
                        cx.notify();
                    })))));
        if let Some(error) = &self.setup_error {
            installation = installation.child(div().child(error.clone()).text_color(rgb(0xf08b8b)));
        }
        if let Some(report) = &self.setup_report {
            for check in &report.checks {
                use crate::setup_diagnostics::Status;
                let (prefix, color) = match check.status {
                    Status::Ready => ("OK", ACCENT),
                    Status::Error => ("Needs attention", 0xf08b8b),
                    Status::Warning => ("Check", 0xfbbf24),
                    Status::NotVerified => ("Not tested", MUTED),
                    Status::Cancelled => ("Cancelled", MUTED),
                };
                installation = installation.child(div().flex().flex_col().gap_1()
                    .child(div().child(format!("{prefix}: {}", check.summary)).text_color(rgb(color)))
                    .when_some(check.action.clone(), |row, action| row.child(muted(action))));
            }
        }
        let installation = installation.relative().child(canvas(
            move |bounds, _, _| installation_bounds.set(Some(bounds)),
            |_, _, _, _| {},
        ).absolute().inset_0());
        let mut resolution = div().flex().gap_1().p_1().rounded_lg().bg(rgb(SOFT));
        for height in [1080, 1440, 2160] {
            resolution = resolution.child(
                button(("resolution", height as usize), format!("{height}p"))
                    .h(px(28.))
                    .bg(rgb(if self.preferences.height == height {
                        0x35363c
                    } else {
                        SOFT
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.preferences.height = height;
                        this.save_preferences();
                        cx.notify();
                    })),
            );
        }
        let mut fps = div().flex().gap_1().p_1().rounded_lg().bg(rgb(SOFT));
        for value in [30, 60, 120] {
            fps = fps.child(
                button(("fps", value as usize), format!("{value}"))
                    .h(px(28.))
                    .bg(rgb(if self.preferences.fps == value {
                        0x35363c
                    } else {
                        SOFT
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.preferences.fps = value;
                        this.save_preferences();
                        cx.notify();
                    })),
            );
        }
        let mut quality = div().flex().gap_1().p_1().rounded_lg().bg(rgb(SOFT));
        for (i, (name, value)) in [
            ("Balanced", 15_000_000),
            ("High", 25_000_000),
            ("Max", 50_000_000),
        ]
        .into_iter()
        .enumerate()
        {
            quality = quality.child(
                button(
                    ("quality", i),
                    format!("{name} · {} Mb/s", value / 1_000_000),
                )
                .h(px(28.))
                .bg(rgb(if self.preferences.bitrate == value {
                    0x35363c
                } else {
                    SOFT
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.preferences.bitrate = value;
                    this.save_preferences();
                    cx.notify();
                })),
            );
        }
        let mut priority = div().flex().gap_1().p_1().rounded_lg().bg(rgb(SOFT));
        for (index, value) in [crate::preferences::CapturePriority::ImageQuality,
            crate::preferences::CapturePriority::EditingSpeed].into_iter().enumerate() {
            priority = priority.child(button(("capture-priority", index), value.label()).h(px(28.))
                .bg(rgb(if self.preferences.capture_priority == value { 0x35363c } else { SOFT }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let previous = this.preferences.capture_priority;
                    this.preferences.capture_priority = value;
                    if !this.save_preferences() { this.preferences.capture_priority = previous; }
                    cx.notify();
                })));
        }
        let mut length = div().flex().gap_1().p_1().rounded_lg().bg(rgb(SOFT));
        for value in [30, 60, 120, 300] {
            length = length.child(
                button(("length", value as usize), format!("{value}s"))
                    .h(px(28.))
                    .bg(rgb(if self.preferences.replay_seconds == value {
                        0x35363c
                    } else {
                        SOFT
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.preferences.replay_seconds = value;
                        this.save_preferences();
                        cx.notify();
                    })),
            );
        }
        let recording = panel()
            .gap_0()
            .py_0()
            .child(row("Capture display",
                "Choose a monitor or follow Windows primary at session start. Stop capture before changing displays.",
                button("settings-display", "").w(px(250.)).min_w_0()
                    .child(div().flex_1().min_w_0().truncate().child(self.preferences.display.as_ref().map_or_else(|| "Windows primary display".into(), |saved| saved.display_label())))
                    .track_focus(&self.display_focus).opacity(if self.active { 0.5 } else { 1. })
                    .on_click(cx.listener(|this, _, _, cx| this.open_display_picker(cx)))))
            .child(row(
                "Resolution",
                "Output size of your recordings and replays",
                resolution,
            ))
            .child(row(
                "Frame rate",
                "Higher frame rates use more GPU and storage",
                fps,
            ))
            .child(row(
                "Quality",
                "Higher bitrate keeps more detail in fast motion",
                quality,
            ))
            .child(row(
                if self.active { "Next capture priority" } else { "Capture priority" },
                "Applies to the next capture. Editing speed can improve seeking in new clips but may reduce image detail at the same bitrate.",
                priority,
            ))
            .child(row(
                "Instant replay length",
                "Longer buffers use more memory; very busy scenes may shorten the available replay",
                length,
            ));
        let audio = panel()
            .gap_0()
            .py_0()
            .child(self.audio_controls(false, cx).py_3())
            .child(row(
                "Clean up replay microphone",
                "Optional DPDFNet for the next saved replay. You can change this during capture; original mic audio and full recordings stay unchanged.",
                toggle("settings-clean", self.suppress_microphone).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.suppress_microphone = !this.suppress_microphone;
                        this.save_preferences();
                        cx.notify();
                    },
                )),
            ));
        let playback = panel().gap_0().py_0().child(row(
            "Use GPU video preview",
            "Experimental AV1 acceleration with automatic CPU fallback. Applies to the next playback session.",
            toggle("gpu-video-preview", self.preferences.gpu_video_preview).on_click(cx.listener(
                |this, _, _, cx| {
                    let previous = this.preferences.gpu_video_preview;
                    this.preferences.gpu_video_preview = !previous;
                    if this.save_preferences() {
                        let enabled = this.preferences.gpu_video_preview;
                        this.editor.update(cx, |editor, cx| {
                            editor.set_gpu_preview_enabled(enabled, cx);
                        });
                    } else {
                        this.preferences.gpu_video_preview = previous;
                    }
                    cx.notify();
                },
            )),
        ));
        // Display the configured spelling. Resolving a drive or junction here
        // can block every Settings repaint and is unnecessary for this label.
        let folder = recordings::root().display().to_string();
        let saving = panel()
            .gap_0()
            .py_0()
            .child(row(
                "Save to",
                folder,
                button("change-folder", "Change")
                    .opacity(if self.active { 0.5 } else { 1. })
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.active {
                            return;
                        }
                        let selected = cx.prompt_for_paths(PathPromptOptions {
                            files: false,
                            directories: true,
                            multiple: false,
                            prompt: Some("Save recordings here".into()),
                        });
                        cx.spawn_in(window, async move |view, cx| {
                            if let Ok(Ok(Some(paths))) = selected.await
                                && let Some(path) = paths.into_iter().next()
                            {
                                let _ = view.update(cx, |this, cx| {
                                    // Capture may have started through a hotkey while
                                    // the native folder picker was still open.
                                    if this.active {
                                        this.notice = Some(
                                            "Stop capture before changing the save folder.".into(),
                                        );
                                        cx.notify();
                                        return;
                                    }
                                    let previous = this.preferences.folder.clone();
                                    this.preferences.folder = path.clone();
                                    if this.save_preferences() {
                                        recordings::set_root(path);
                                        this.refresh_files();
                                    } else {
                                        this.preferences.folder = previous;
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .detach();
                    })),
            ))
            .child(row(
                "Estimated video storage",
                "Based on selected bitrate; audio and container size are additional",
                mono(format!(
                    "{:.1} GB / hour",
                    self.preferences.bitrate as f64 * 3600. / 8e9
                )),
            ))
            .child(row(
                "Replay memory budget",
                "Includes bitrate headroom, enabled audio sources and the preceding keyframe",
                mono(rcr_media_core::replay_memory_budget(
                    &rcr_media_core::VideoConfig { bitrate: self.preferences.bitrate,
                        fps: self.preferences.fps,
                        keyframe_interval: self.preferences.capture_priority.keyframe_interval(self.preferences.fps),
                        ..Default::default() }, self.preferences.replay_seconds,
                    usize::from(self.system_audio) + usize::from(self.microphone))
                    .map_or_else(|error| error.to_string(), |bytes| format!("{:.0} MiB", bytes as f64 / 1_048_576.))),
            ));
        let mut shortcuts = panel().gap_0().py_0();
        for (index, title) in crate::hotkeys::ACTIONS.iter().enumerate() {
            let editing = self.hotkey_edit == Some(index);
            let status = self.hotkeys.as_ref().and_then(|h| h.statuses.get(index));
            let detail = if editing {
                "Press a shortcut. Escape cancels; Backspace disables.".to_owned()
            } else if self.hotkey_edit.is_some() {
                "Shortcuts are paused while assigning a key.".to_owned()
            } else {
                status
                    .map(|s| s.detail.clone())
                    .unwrap_or_else(|| "Click to assign a global shortcut".into())
            };
            let color = if editing {
                ACCENT
            } else if status.is_some_and(|s| !s.registered)
                && !self.preferences.hotkeys[index].is_empty()
            {
                0xf08b8b
            } else {
                TEXT
            };
            shortcuts = shortcuts.child(row(
                *title,
                detail,
                button(
                    ("bind-shortcut", index),
                    if editing {
                        "Press keys…".into()
                    } else {
                        self.shortcut_name(index)
                    },
                )
                .min_w(px(190.))
                .text_color(rgb(color))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.hotkey_blur.take();
                    this.hotkeys.take();
                    this.hotkey_edit = Some(index);
                    this.hotkey_focus.focus(window);
                    this.hotkey_blur = Some(cx.on_blur(
                        &this.hotkey_focus.clone(),
                        window,
                        |this, _, cx| this.cancel_hotkey_edit(cx),
                    ));
                    cx.notify();
                })),
            ));
        }
        shortcuts = shortcuts.child(
            div()
                .py_3()
                .flex()
                .justify_between()
                .items_center()
                .child(muted(
                    "Click a shortcut to change it. Changes save automatically.",
                ))
                .when(self.hotkey_edit.is_some(), |s| {
                    s.child(
                        button("cancel-shortcut", "Cancel binding")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_hotkey_edit(cx))),
                    )
                })
                .child(
                    button("restore-shortcuts", "Restore defaults").on_click(cx.listener(
                        |this, _, window, cx| {
                            this.apply_hotkeys(crate::preferences::default_hotkeys(), window, cx)
                        },
                    )),
                ),
        );
        div().flex_1().min_h_0().child(div().id("settings-scroll").track_scroll(&self.settings_scroll).track_focus(&self.hotkey_focus).on_key_down(cx.listener(Self::hotkey_key)).size_full().min_h_0().overflow_y_scroll().px_6().py_8().child(div().max_w(px(880.)).w_full().mx_auto().flex().flex_col().gap_6()
            .child(div().flex().items_center().justify_between()
                .child(div().text_size(px(24.)).font_weight(FontWeight::MEDIUM).child("Settings"))
                .child(button("recommended-capture", "Use 1440p60 defaults").on_click(cx.listener(|this,_,_,cx| {
                    let previous = (this.preferences.height, this.preferences.fps, this.preferences.bitrate,
                        this.preferences.replay_seconds, this.preferences.capture_priority);
                    this.preferences.height=1440;
                    this.preferences.fps=60;
                    this.preferences.bitrate=25_000_000;
                    this.preferences.replay_seconds=120;
                    this.preferences.capture_priority=crate::preferences::CapturePriority::ImageQuality;
                    if this.save_preferences() {
                        this.notice=Some(if this.active {"1440p60 defaults saved for the next capture session."} else {"1440p60 · AV1 · 25 Mb/s · 120-second replay"}.into());
                    } else {
                        (this.preferences.height, this.preferences.fps, this.preferences.bitrate,
                            this.preferences.replay_seconds, this.preferences.capture_priority) = previous;
                    }
                    cx.notify();
                }))))
            .child(muted(if self.active{"Recording settings apply next session. Stop capture to change audio sources or save folder."}else{"Make it yours. Settings save automatically."}))
            .child(div().flex().flex_col().gap_2().child(muted("VIDEO & REPLAY"))
                .child(muted("Requires Windows and an NVIDIA GPU with AV1 NVENC support. Capture currently supports SDR desktops."))
                .child(recording))
            .child(div().flex().flex_col().gap_2().child(muted("AUDIO · SEPARATE TRACKS")).child(audio))
            .child(div().flex().flex_col().gap_2().child(muted("EDITOR PLAYBACK")).child(playback))
            .child(div().flex().flex_col().gap_2().child(muted("STORAGE")).child(saving))
            .child(installation)
            .child(div().flex().flex_col().gap_2().child(muted("GLOBAL SHORTCUTS")).child(shortcuts))
            .child(row("Keep running in tray", "Closing the window keeps recording and global shortcuts available", toggle("close-to-tray",self.preferences.close_to_tray).on_click(cx.listener(|this,_,_,cx|{this.preferences.close_to_tray= !this.preferences.close_to_tray;this.save_preferences();cx.notify();}))))
            .child(button("screenshot-settings","Screenshot, workflow and sharing settings").on_click(cx.listener(|this,_,_,cx|{this.tab=4;this.toolkit.update(cx,|toolkit,cx|toolkit.preferences(cx));cx.notify();})))))
    }

    /// Explicit visual fixture only. Align the measured installation section,
    /// then require its entire report to be inside the painted scroll viewport.
    pub(crate) fn snapshot_installation_visible(&mut self, cx: &mut Context<Self>) -> anyhow::Result<bool> {
        let Some(section) = self.installation_bounds.get() else { return Ok(false); };
        let viewport = self.settings_scroll.bounds();
        if viewport.size.height <= px(0.) { return Ok(false); }
        anyhow::ensure!(section.size.height <= viewport.size.height - px(32.),
            "Installation fixture is too tall for the Settings viewport: section={section:?}, viewport={viewport:?}");
        anyhow::ensure!(section.left() >= viewport.left() && section.right() <= viewport.right(),
            "Installation fixture exceeds Settings viewport width: section={section:?}, viewport={viewport:?}");
        if section.top() >= viewport.top() + px(8.) && section.bottom() <= viewport.bottom() - px(8.) {
            return Ok(true);
        }
        let mut offset = self.settings_scroll.offset();
        offset.y = (offset.y + viewport.top() + px(16.) - section.top())
            .clamp(-self.settings_scroll.max_offset().height, px(0.));
        self.settings_scroll.set_offset(offset);
        cx.notify();
        Ok(false)
    }
}
impl Render for Studio {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.editor_suspended {
            return div().size_full().bg(rgb(BG)).into_any_element();
        }
        if self.tab != 2 && self.editor.read(cx).is_visible() {
            self.editor.update(cx, |editor, cx| editor.hide(cx));
        }
        self.library.observe_navigation(self.tab);
        if self.name_dialog.is_none()
            && let Some(focus) = self.name_restore.take()
        {
            focus.focus(window);
        }
        if self.tab == 2 {
            return div()
                .size_full()
                .bg(rgb(BG))
                .text_color(rgb(TEXT))
                .text_size(px(13.))
                .font_family("Geist")
                .child(self.editor.clone())
                .into_any_element();
        }
        let mut tabs = div().flex().gap_1().p_1().rounded(px(9.)).bg(rgb(0x141518));
        for (i, name) in [
            (0, "Capture"),
            (1, "Library"),
            (4, "Tools"),
            (3, "Settings"),
        ] {
            tabs = tabs.child(
                button(("nav", i), name)
                    .h(px(28.))
                    .bg(rgb(if self.tab == i { 0x26272b } else { 0x141518 }))
                    .text_color(rgb(if self.tab == i { TEXT } else { MUTED }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cancel_hotkey_edit(cx);
                                    this.tab = i;
                        this.refresh_files();
                        cx.notify();
                    })),
            );
        }
        let mode = self.capture_state().mode();
        let status = match mode {
            crate::tray::Mode::Off => "Replay is off",
            crate::tray::Mode::Starting if self.record_to_disk => "Starting recording…",
            crate::tray::Mode::Starting => "Starting replay…",
            crate::tray::Mode::Stopping => "Finishing capture…",
            crate::tray::Mode::Recording => "Recording",
            crate::tray::Mode::Replay => "Replay is on",
        };
        let header = div()
            .h(px(56.))
            .flex_shrink_0()
            .px_5()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(logo().w(px(180.)))
            .child(tabs)
            .child(
                div()
                    .w(px(180.))
                    .flex()
                    .justify_end()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(6.)).rounded_full().bg(rgb(match mode {
                        crate::tray::Mode::Starting | crate::tray::Mode::Stopping => 0xfbbf24,
                        crate::tray::Mode::Recording => 0xf08b8b,
                        crate::tray::Mode::Replay => ACCENT,
                        crate::tray::Mode::Off => MUTED,
                    })))
                    .child(muted(status)),
            );
        let content = match self.tab {
            4 => div().flex_1().min_h_0().child(self.toolkit.clone()),
            1 => self.library_ui(cx),
            3 => self.settings_ui(cx),
            _ => self.capture_ui(window, cx),
        };
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_size(px(13.))
            .font_family("Geist")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.tab == 1
                    && this.name_dialog.is_none()
                    && this.audio_picker.is_none()
                    && this.audio_device_picker.is_none()
                    && this.display_picker.is_none()
                    && this.trash_panel.is_none()
                    && event.keystroke.key == "f"
                    && event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.shift
                    && !event.keystroke.modifiers.alt
                    && !event.keystroke.modifiers.platform
                {
                    this.search.read(cx).focus_handle(cx).focus(window);
                    window.prevent_default();
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "tab"
                    && !event.keystroke.modifiers.control
                    && !event.keystroke.modifiers.alt
                    && !event.keystroke.modifiers.platform
                {
                    let was_audio_details = this.audio_error_focus.is_focused(window);
                    if event.keystroke.modifiers.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                    if this.tab == 0 && !was_audio_details && this.audio_error_focus.is_focused(window) {
                        this.capture_options_scroll.scroll_to_bottom();
                    }
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .child(content)
            .when_some(
                self.notice.clone().filter(|_| self.error.is_none()),
                |s, n| {
                    s.child(
                        div()
                            .absolute()
                            .bottom_5()
                            .right_6()
                            .max_w(px(
                                if self.notice.as_deref() == Some("Replay saved to your Library.") {
                                    640.
                                } else {
                                    480.
                                },
                            ))
                            .occlude()
                            .rounded_lg()
                            .bg(rgb(0x17181b))
                            .border_1()
                            .border_color(rgb(LINE))
                            .p_3()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(muted(n).flex_1().min_w_0())
                            .when_some(
                                self.saved_replay.clone().filter(|_| {
                                    self.notice.as_deref() == Some("Replay saved to your Library.")
                                }),
                                |s, path| {
                                    let reveal = path.clone();
                                    s.child(button("edit-saved-replay", "Edit").on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.notice = None;
                                            this.open_recording(path.clone(), cx);
                                        }),
                                    ))
                                    .child(
                                        button("reveal-saved-replay", "Show in folder")
                                            .on_click(cx.listener(move |this, _, _, cx| this.open_shell_action(recordings::ShellAction::Reveal, reveal.clone(), cx))),
                                    )
                                },
                            )
                            .child(
                                button("dismiss-notice", "")
                                    .px_1()
                                    .flex_shrink_0()
                                    .child(icon("close"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.notice = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                },
            )
            .when_some(self.error.clone(), |s, e| {
                s.child(
                    div()
                        .absolute()
                        .bottom_5()
                        .right_6()
                        .max_w(px(650.))
                        .occlude()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(0x3a2025))
                        .text_color(rgb(0xffb0b0))
                        .border_1()
                        .border_color(rgb(0x73404c))
                        .flex()
                        .items_start()
                        .gap_3()
                        .child(div().flex_1().min_w_0().child(e))
                        .child(
                            button("dismiss-capture-error", "")
                                .px_1()
                                .flex_shrink_0()
                                .child(icon("close"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.error = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .children(self.name_dialog.clone())
            .children(self.audio_picker.clone())
            .children(self.audio_device_picker.clone())
            .children(self.display_picker.clone())
            .children(self.trash_panel.clone())
            .into_any_element()
    }
}

#[cfg(test)]
mod capture_audio_health_tests {
    use super::{audio_error_details, audio_recovery_hint};
    #[test]
    fn keyboard_audio_details_navigation_uses_viewport_and_does_not_steal_tab() {
        use super::audio_error_scroll_y;
        assert_eq!(audio_error_scroll_y("down", 0., 80.), Some(-24.));
        assert_eq!(audio_error_scroll_y("up", -10., 80.), Some(0.));
        assert_eq!(audio_error_scroll_y("pagedown", -24., 80.), Some(-88.));
        assert_eq!(audio_error_scroll_y("pageup", -24., 80.), Some(0.));
        assert_eq!(audio_error_scroll_y("home", -900., 80.), Some(0.));
        assert!(audio_error_scroll_y("end", 0., 80.).unwrap() < -1000.);
        assert_eq!(audio_error_scroll_y("tab", 0., 80.), None);
        assert_eq!(audio_error_scroll_y("r", 0., 80.), None);
    }
    #[test]
    fn source_failures_remain_identifiable_after_stop_without_claiming_audio_was_recovered() {
        let errors = vec!["Microphone: selected USB input disconnected (0x88890004)".into(),
            "Desktop audio: loopback endpoint became unavailable".into()];
        let original = errors.clone();
        assert_eq!(audio_error_details(&errors), errors);
        assert_eq!(errors, original, "Rendering a warning changed diagnostic evidence");
        assert!(audio_recovery_hint(true, false).contains("before stopping"));
        assert!(audio_recovery_hint(true, true).contains("once capture stops"));
        assert!(audio_recovery_hint(false, false).contains("Last capture"));
        assert_eq!(audio_recovery_hint(false, true), audio_recovery_hint(false, false), "Stale stop flags hid the completed-session warning");
        assert!(audio_error_details(&[]).is_empty(), "Healthy next-session stats retained old warnings");
    }
    #[test]
    fn audio_error_rendering_bounds_long_unicode_and_error_count_without_touching_evidence() {
        let oversized = "🎙".repeat(5000);
        let mut errors = vec![oversized.clone(), "  ".into()];
        errors.extend((0..100).map(|index| format!("Source failure {index}")));
        let shown = audio_error_details(&errors);
        assert_eq!(shown.len(), 4);
        assert_eq!(shown[0].chars().count(), 2049);
        assert!(shown[0].ends_with('…'));
        assert!(shown[1].contains("unspecified"));
        assert_eq!(errors[0], oversized);
        assert_eq!(errors.len(), 102);
    }
}

#[cfg(test)]
mod hotkey_persistence_tests {
    use ::core::prelude::v1::test;
    use super::persist_hotkeys;
    use crate::preferences::{Preferences, SavedAudioEndpoint};

    #[test]
    fn failed_recording_switch_cannot_restart_a_later_manual_replay() {
        let mut pending_switch = true;
        assert!(!crate::consume_recording_restart(&mut pending_switch, false));
        assert!(!pending_switch, "Failed replay-to-recording switch retained its intent");
        // A later manually started replay has no pending recording switch.
        // Stopping that healthy replay must not unexpectedly start recording.
        assert!(!crate::consume_recording_restart(&mut pending_switch, false), "Manual start inherited an old switch");
        assert!(!crate::consume_recording_restart(&mut pending_switch, true));
        pending_switch = true;
        assert!(crate::consume_recording_restart(&mut pending_switch, true));
        assert!(!crate::consume_recording_restart(&mut pending_switch, true), "Healthy restart intent was consumed twice");
        pending_switch = true;
        assert!(!crate::consume_recording_restart(&mut pending_switch, false), "Manual start accepted stale restart intent");
        assert!(!crate::consume_recording_restart(&mut pending_switch, true));
    }

    #[test]
    fn live_microphone_cleanup_is_snapshotted_by_each_replay_request() {
        use rcr_windows::pipeline::RecorderCommand;
        let first = crate::replay_save_request("first.mkv".into(), 120, true, true);
        let next = crate::replay_save_request("next.mkv".into(), 120, true, false);
        assert!(matches!(first, RecorderCommand::SaveReplay { suppress_microphone: true, .. }),
            "Changing cleanup changed an already queued replay request");
        assert!(matches!(next, RecorderCommand::SaveReplay { suppress_microphone: false, .. }));
        assert!(matches!(crate::replay_save_request("no-mic.mkv".into(), 120, false, true),
            RecorderCommand::SaveReplay { suppress_microphone: false, .. }));
    }

    #[test]
    fn unsupported_av1_error_adds_actionable_context_without_losing_the_cause() {
        let cause = "initializing encoder: this GPU does not support AV1 NVENC encoding";
        let message = crate::capture_error_message(cause.into());
        assert!(message.starts_with("The capture GPU cannot encode AV1 with NVENC."));
        assert!(message.ends_with(cause));
        let other = "NVENC open session failed: device removed (0x887A0005)";
        assert_eq!(crate::capture_error_message(other.into()), other);
    }

    #[test]
    fn failed_shortcut_save_restores_bindings_and_preserves_other_preferences() {
        let mut preferences = Preferences { folder: "fixture-captures".into(), microphone: true,
            microphone_device: Some(SavedAudioEndpoint { id: "fixture-input".into(), label: "Mic".into() }),
            ..Default::default() };
        let before = serde_json::to_value(&preferences).unwrap();
        let mut proposed = preferences.hotkeys.clone();
        proposed[0] = "Ctrl+Alt+F8".into();
        let expected = proposed.clone();
        let result = persist_hotkeys(&mut preferences, proposed, |candidate| {
            assert_eq!(candidate.hotkeys, expected, "Persistence did not receive proposed bindings");
            Err("Synthetic disk-full failure")
        });
        assert_eq!(result, Err("Synthetic disk-full failure"));
        assert_eq!(serde_json::to_value(&preferences).unwrap(), before,
            "Failed persistence changed bindings or unrelated settings");
    }

    #[test]
    fn successful_shortcut_save_commits_only_bindings_after_persistence() {
        let mut preferences = Preferences { replay_seconds: 120, ..Default::default() };
        let mut expected = serde_json::to_value(&preferences).unwrap();
        let mut proposed = preferences.hotkeys.clone();
        proposed[2] = "Ctrl+Shift+F10".into();
        expected["hotkeys"] = serde_json::to_value(&proposed).unwrap();
        persist_hotkeys(&mut preferences, proposed, |candidate| {
            assert_eq!(candidate.replay_seconds, 120);
            assert_eq!(candidate.hotkeys[2], "Ctrl+Shift+F10");
            Ok::<_, ()>(())
        }).unwrap();
        assert_eq!(serde_json::to_value(&preferences).unwrap(), expected);
    }
}
