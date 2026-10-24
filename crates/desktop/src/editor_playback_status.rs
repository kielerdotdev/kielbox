//! Playback presentation only. Seeking and recording use independent pipelines.
use super::*;

const MAX_REASON_CHARS: usize = 1_024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Seeking,
    Cpu,
    Starting,
    Gpu,
    Fallback(String),
    Failed(String),
}
impl Mode {
    fn badge(&self) -> &'static str {
        match self {
            Self::Seeking => "Playback info",
            Self::Cpu | Self::Fallback(_) => "Playback · CPU",
            Self::Starting => "Playback · preparing",
            Self::Gpu => "Playback · GPU",
            Self::Failed(_) => "Playback · unavailable",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Presentation {
    badge: &'static str,
    summary: &'static str,
    reason: Option<String>,
    next_session: &'static str,
}

fn bounded_reason(reason: &str) -> String {
    let mut chars = reason.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t');
    let mut text: String = chars.by_ref().take(MAX_REASON_CHARS).collect();
    if chars.next().is_some() { text.push_str("\n[Additional details omitted]"); }
    text
}

fn presentation(mode: &Mode, enabled: bool, session_setting: Option<bool>, requested: bool) -> Presentation {
    let (badge, summary, reason) = match mode {
        Mode::Seeking => ("Playback info", "Dragging the timeline previews individual frames. GPU video preview applies when you press Play.", None),
        Mode::Cpu => ("Playback · CPU", "Video playback uses the CPU. This is a supported playback mode.",
            (session_setting == Some(true) && !requested).then(|| "GPU preview is unavailable for this window.".into())),
        Mode::Starting => ("Playback · preparing", "Preparing accelerated video playback. The current preview may remain visible while it loads.", None),
        Mode::Gpu => ("Playback · GPU", "Video playback uses your GPU.", None),
        Mode::Fallback(reason) => ("Playback · CPU", "Video playback is using the CPU instead of the GPU. See the reason below.", Some(bounded_reason(reason))),
        Mode::Failed(reason) => ("Playback · unavailable", "Playback could not continue. Review the reason below before reopening the clip.", Some(bounded_reason(reason))),
    };
    let next_session = match (enabled, session_setting) {
        (true, Some(false)) => "GPU preview is now on. To try it here, close this panel, move the playhead, then press Play.",
        (false, Some(true)) => "GPU preview is now off. To switch this clip to CPU playback, close this panel, move the playhead, then press Play.",
        (true, _) => "GPU preview is on. Clips and edits it cannot accelerate use the CPU automatically.",
        (false, _) => "GPU preview is off. Video playback uses the CPU.",
    };
    Presentation { badge, summary, reason, next_session }
}

pub(super) struct Details {
    pub open: bool,
    pub session_setting: Option<bool>,
    pub requested: bool,
    badge_focus: FocusHandle,
    close_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    scroll: ScrollHandle,
    snapshot: Option<(Mode, bool, Option<bool>, bool)>,
}

impl Details {
    pub fn new(cx: &mut Context<EditorView>) -> Self {
        Self { open: false, session_setting: None, requested: false,
            badge_focus: cx.focus_handle().tab_stop(true), close_focus: cx.focus_handle().tab_stop(true),
            previous_focus: None, scroll: ScrollHandle::new(), snapshot: None }
    }
    pub fn reset_session(&mut self) {
        self.session_setting = None;
        self.requested = false;
    }
    pub fn clear_snapshot(&mut self) {
        self.snapshot = None;
    }
}

impl EditorView {
    /// Native keyboard dispatch inside the existing owned-media editor fixture.
    /// No clipboard actions or presentation snapshots are used by this proof.
    pub(super) fn smoke_playback_details(
        view: &Entity<Self>, window: &mut Window, cx: &mut App,
        history: fn(&mut Editor) -> anyhow::Result<serde_json::Value>,
    ) -> anyhow::Result<()> {
        use anyhow::ensure;
        fn key(window: &mut Window, cx: &mut App, name: &str, control: bool, shift: bool) {
            window.draw(cx).clear();
            let keystroke = Keystroke { key: name.into(), key_char: None,
                modifiers: Modifiers { control, shift, ..Default::default() } };
            window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent {
                keystroke: keystroke.clone(), is_held: false,
            }), cx);
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
            window.draw(cx).clear();
        }
        let before = view.update(cx, |editor, _| history(&mut editor.model))?;
        let transport = |cx: &App| {
            let editor = view.read(cx);
            (editor.playing, editor.playhead, editor.selected, editor.preview.is_some(),
                editor.scrub.is_some(), editor.preview_due)
        };
        let original_transport = transport(cx);
        ensure!(!original_transport.0, "Playback details fixture must begin paused");
        ensure!(!view.read(cx).playback_details.open && !view.read(cx).export_details_open,
            "Playback details fixture must begin without a modal");
        let badge = view.read(cx).playback_details.badge_focus.clone();
        badge.focus(window);
        window.draw(cx).clear();
        window.focus_prev();
        ensure!(!badge.is_focused(window), "Playback badge has no previous Tab control");
        key(window, cx, "tab", false, false);
        ensure!(badge.is_focused(window), "Tab did not reach the painted playback badge");
        key(window, cx, "enter", false, false);
        ensure!(view.read(cx).playback_details.open
            && view.read(cx).playback_details.close_focus.is_focused(window),
            "Enter on the actual playback badge did not open and focus Close");
        for shift in [false, false, true, true] {
            key(window, cx, "tab", false, shift);
            ensure!(view.read(cx).playback_details.open
                && view.read(cx).playback_details.close_focus.is_focused(window),
                "Playback modal Tab/Shift+Tab escaped Close");
        }
        for (name, control, shift) in [
            ("z", true, false), ("z", true, true), ("s", false, false),
            ("delete", false, false), ("left", false, false), ("right", false, false),
            ("e", true, false), ("space", true, false),
        ] {
            key(window, cx, name, control, shift);
            ensure!(view.read(cx).playback_details.open,
                "Playback modal shortcut {name} unexpectedly closed the dialog");
            ensure!(transport(cx) == original_transport && view.read(cx).export.is_none(),
                "Playback modal shortcut {name} changed transport, selection or started export");
            ensure!(view.update(cx, |editor, _| history(&mut editor.model))? == before,
                "Playback modal shortcut {name} changed project or Undo/Redo branches");
        }
        // Space is an actual native click on Close; it must not toggle playback.
        key(window, cx, "space", false, false);
        ensure!(!view.read(cx).playback_details.open && badge.is_focused(window),
            "Space on Close did not close playback details and restore badge focus");
        ensure!(transport(cx) == original_transport, "Close leaked Space into playback");
        key(window, cx, "enter", false, false);
        ensure!(view.read(cx).playback_details.open,
            "Restored badge focus could not reopen details with Enter");
        key(window, cx, "escape", false, false);
        ensure!(!view.read(cx).playback_details.open && badge.is_focused(window),
            "Escape did not close playback details and restore badge focus");
        ensure!(transport(cx) == original_transport
            && view.update(cx, |editor, _| history(&mut editor.model))? == before,
            "Playback details changed transport, selection, project or Undo/Redo history");
        println!("Playback details keyboard smoke: actual badge Tab/Enter, modal Tab/Shift+Tab containment, blocked edit/seek/export shortcuts, native Space close and Escape focus restoration; transport, selection and Undo/Redo unchanged; no clipboard action");
        Ok(())
    }

    fn playback_presentation(&self) -> Presentation {
        if let Some((mode, enabled, started, requested)) = &self.playback_details.snapshot {
            return presentation(mode, *enabled, *started, *requested);
        }
        #[cfg(windows)]
        let enabled = self.gpu_preview.enabled;
        #[cfg(not(windows))]
        let enabled = false;
        let mode = if self.preview.is_none() {
            Mode::Seeking
        } else {
            #[cfg(windows)]
            { match &self.gpu_preview.status {
                Some(core::GpuPlaybackStatus::Starting) => Mode::Starting,
                Some(core::GpuPlaybackStatus::Active | core::GpuPlaybackStatus::Finished)
                    if self.gpu_preview.surface.as_ref().is_some_and(|(_, surface)| surface.error().is_none() && surface.submitted_frame_id() > 0) => Mode::Gpu,
                Some(core::GpuPlaybackStatus::Active | core::GpuPlaybackStatus::Finished) => Mode::Starting,
                Some(core::GpuPlaybackStatus::Fallback(reason)) => Mode::Fallback(reason.clone()),
                Some(core::GpuPlaybackStatus::Failed(reason)) => Mode::Failed(reason.clone()),
                None if self.playback_details.requested => Mode::Starting,
                None => Mode::Cpu,
            } }
            #[cfg(not(windows))]
            { Mode::Cpu }
        };
        presentation(&mode, enabled, self.playback_details.session_setting, self.playback_details.requested)
    }

    fn show_playback_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.playback_details.previous_focus = window.focused(cx);
        self.playback_details.open = true;
        self.playback_details.scroll.set_offset(point(px(0.), px(0.)));
        self.picker = false;
        self.preset_menu = false;
        self.playback_details.close_focus.focus(window);
        cx.notify();
    }
    fn close_playback_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.playback_details.open = false;
        if let Some(focus) = self.playback_details.previous_focus.take() { focus.focus(window); }
        else { self.focus.focus(window); }
        cx.notify();
    }
    pub(super) fn playback_status_badge(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // The frequently painted badge never clones or formats diagnostic text.
        let text = if let Some((mode, ..)) = &self.playback_details.snapshot {
            mode.badge()
        } else if self.preview.is_none() {
            Mode::Seeking.badge()
        } else {
            #[cfg(windows)]
            { match &self.gpu_preview.status {
                Some(core::GpuPlaybackStatus::Starting) => Mode::Starting.badge(),
                Some(core::GpuPlaybackStatus::Active | core::GpuPlaybackStatus::Finished)
                    if self.gpu_preview.surface.as_ref().is_some_and(|(_, surface)| surface.error().is_none() && surface.submitted_frame_id() > 0) => Mode::Gpu.badge(),
                Some(core::GpuPlaybackStatus::Active | core::GpuPlaybackStatus::Finished) => Mode::Starting.badge(),
                Some(core::GpuPlaybackStatus::Failed(_)) => "Playback · unavailable",
                None if self.playback_details.requested => Mode::Starting.badge(),
                _ => Mode::Cpu.badge(),
            } }
            #[cfg(not(windows))]
            { Mode::Cpu.badge() }
        };
        button("playback-status", text, "View playback mode and acceleration settings")
            .track_focus(&self.playback_details.badge_focus)
            .bg(rgb(theme::BG)).text_color(rgb(MUTED))
            .on_click(cx.listener(|this, _, window, cx| this.show_playback_details(window, cx)))
    }
    pub(super) fn playback_details_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.playback_presentation();
        div().id("playback-details-modal").absolute().inset_0().occlude()
            .bg(rgba(0x000000a8)).flex().items_center().justify_center().px_4()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                match key {
                    "escape" => this.close_playback_details(window, cx),
                    "tab" => this.playback_details.close_focus.focus(window),
                    "up" | "down" | "pageup" | "pagedown" | "home" | "end" => {
                        let mut offset = this.playback_details.scroll.offset();
                        offset.y = match key {
                            "up" => offset.y + px(32.), "down" => offset.y - px(32.),
                            "pageup" => offset.y + px(160.), "pagedown" => offset.y - px(160.),
                            "home" => px(0.), _ => px(-1_000_000.),
                        };
                        this.playback_details.scroll.set_offset(offset);
                        cx.notify();
                    }
                    "enter" | "space" => return,
                    _ => {}
                }
                window.prevent_default(); cx.stop_propagation();
            }))
            .on_key_down(|_, _, cx| cx.stop_propagation())
            .child(div().w(px(560.)).max_w_full().p_5().rounded_lg().bg(rgb(PANEL))
                .border_1().border_color(rgb(LINE)).shadow_lg().flex().flex_col().gap_3()
                .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child("Playback details"))
                .child(label(state.badge))
                .child(div().text_sm().child(state.summary))
                .when_some(state.reason, |s, reason| s.child(
                    div().id("playback-reason").max_h(px(144.)).overflow_y_scroll()
                        .track_scroll(&self.playback_details.scroll).p_3().rounded_md().bg(rgb(theme::BG))
                        .text_size(px(12.)).children(reason.lines().map(|line|
                            div().min_w_0().whitespace_normal().child(if line.is_empty() { " " } else { line }.to_owned())))))
                .child(div().text_sm().text_color(rgb(MUTED)).child(state.next_session))
                .child(div().text_size(px(12.)).text_color(rgb(MUTED))
                    .child("Change GPU video preview in Settings → Editor playback. This setting does not change recording, export quality or original files."))
                .child(div().flex().justify_end().child(
                    button("close-playback-details", "Close · Esc", "Close playback details")
                        .track_focus(&self.playback_details.close_focus)
                        .on_click(cx.listener(|this, _, window, cx| this.close_playback_details(window, cx))))))
    }

    /// Presentation-only fixture. It starts no playback, decoder or capture.
    pub fn snapshot_playback_details(&mut self, fallback: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = true;
        self.playback_details.snapshot = Some(if fallback {
            (Mode::Fallback("This edit contains multiple video pieces. CPU video playback is used for this composition.".into()), true, Some(true), true)
        } else { (Mode::Cpu, true, Some(false), false) });
        self.show_playback_details(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, presentation, MAX_REASON_CHARS};
    #[test]
    fn cpu_selection_is_supported_and_distinct_from_unavailable_gpu() {
        let selected = presentation(&Mode::Cpu, false, Some(false), false);
        assert_eq!(selected.badge, "Playback · CPU");
        assert!(selected.reason.is_none());
        assert!(selected.summary.contains("supported"));
        let missing = presentation(&Mode::Cpu, true, Some(true), false);
        assert!(missing.reason.unwrap().contains("unavailable for this window"));
    }
    #[test]
    fn settings_changes_describe_next_session_without_relabeling_current_cpu_or_gpu() {
        let cpu = presentation(&Mode::Cpu, true, Some(false), false);
        assert_eq!(cpu.badge, "Playback · CPU");
        assert!(cpu.next_session.contains("GPU preview is now on"));
        assert!(cpu.next_session.contains("move the playhead, then press Play"));
        let gpu = presentation(&Mode::Gpu, false, Some(true), true);
        assert_eq!(gpu.badge, "Playback · GPU");
        assert!(gpu.next_session.contains("GPU preview is now off"));
        assert!(gpu.next_session.contains("move the playhead, then press Play"));
    }
    #[test]
    fn fallback_and_terminal_failure_preserve_bounded_unicode_reasons() {
        for mode in [Mode::Fallback("GPU playback composition uses CPU fallback".into()),
            Mode::Fallback("FFmpeg runtime was not found".into()),
            Mode::Fallback("GPU presentation disabled; using CPU video".into())] {
            let state = presentation(&mode, true, Some(true), true);
            assert_eq!(state.badge, "Playback · CPU");
            assert!(state.reason.is_some());
            assert!(state.summary.contains("using the CPU instead of the GPU"));
        }
        let state = presentation(&Mode::Failed(format!("{}\0", "🌱".repeat(2_000))), true, Some(true), true);
        assert_eq!(state.badge, "Playback · unavailable");
        let reason = state.reason.unwrap();
        assert_eq!(reason.chars().take(MAX_REASON_CHARS).collect::<String>(), "🌱".repeat(MAX_REASON_CHARS));
        assert!(reason.ends_with("[Additional details omitted]"));
        assert!(!reason.contains('\0'));
    }
    #[test]
    fn seeking_never_claims_gpu_playback_and_preparing_never_claims_active() {
        let seek = presentation(&Mode::Seeking, true, None, false);
        assert_eq!(seek.badge, "Playback info");
        assert!(seek.summary.contains("Dragging the timeline previews individual frames"));
        assert!(seek.reason.is_none());
        assert_eq!(presentation(&Mode::Starting, true, Some(true), true).badge, "Playback · preparing");
    }
}
