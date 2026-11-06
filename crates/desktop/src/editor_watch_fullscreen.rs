//! Explicit Clip show fullscreen control and an exclusively hidden-host proof.
use super::{AnyWindowHandle, App, Bounds, Context, Editor, EditorView, Entity, Pixels, Window};
use std::time::Duration;
#[path = "editor_native_fullscreen_proof.rs"]
pub(super) mod native;

gpui::actions!(rcr_watch, [ToggleViewerFullscreen]);

#[derive(Default)]
pub(super) struct State {
    pending: Option<bool>,
    window: Option<AnyWindowHandle>,
    // Only the guarded hidden input fixture sets this. Hidden GPUI Windows
    // hosts defer fullscreen until shown, so the proof must not queue it.
    suppress_platform_for_proof: bool,
}
#[derive(Debug, PartialEq, Eq)]
enum Exit { Windowed, Pending, Toggle }
#[derive(Debug, PartialEq, Eq)]
enum RestoreAction { Wait, Exit, Done }
struct Restore { entering: bool, exiting: bool }
impl Restore {
    fn observe(&mut self, fullscreen: bool) -> RestoreAction {
        if self.entering {
            if !fullscreen { return RestoreAction::Wait; }
            self.entering = false;
        }
        if self.exiting { return if fullscreen { RestoreAction::Wait } else { RestoreAction::Done }; }
        if !fullscreen { return RestoreAction::Done; }
        self.exiting = true;
        RestoreAction::Exit
    }
}
impl State {
    fn observe(&mut self, fullscreen: bool) {
        if self.pending == Some(fullscreen) { self.pending = None; }
        if !fullscreen && self.pending.is_none() { self.window = None; }
    }
    fn request_toggle(&mut self, fullscreen: bool) -> bool {
        self.observe(fullscreen);
        if self.pending.is_some() { return false; }
        self.pending = Some(!fullscreen);
        true
    }
    fn request_exit(&mut self, fullscreen: bool) -> Exit {
        self.observe(fullscreen);
        if self.pending.is_some() { return Exit::Pending; }
        if !fullscreen { return Exit::Windowed; }
        self.pending = Some(false);
        Exit::Toggle
    }
}
impl EditorView {
    pub(super) fn observe_watch_fullscreen(&mut self, window: &Window) {
        self.watch_fullscreen.observe(window.is_fullscreen());
    }
    pub(super) fn toggle_watch_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.clip_show || !self.watch_fullscreen.request_toggle(window.is_fullscreen()) { return; }
        self.watch_fullscreen.window = Some(window.window_handle());
        if !self.watch_fullscreen.suppress_platform_for_proof { window.toggle_fullscreen(); }
        cx.notify();
    }
    /// Escape consumes a pending transition; repeated keys cannot enqueue a
    /// second toggle that accidentally re-enters fullscreen.
    pub(super) fn exit_watch_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.watch_fullscreen.request_exit(window.is_fullscreen()) {
            Exit::Windowed => false,
            Exit::Pending => true,
            Exit::Toggle => {
                if !self.watch_fullscreen.suppress_platform_for_proof { window.toggle_fullscreen(); }
                cx.notify(); true
            }
        }
    }
    pub(super) fn prepare_watch_mode_exit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.observe_watch_fullscreen(window);
        if self.watch_fullscreen.pending == Some(true) { return false; }
        if window.is_fullscreen() && self.watch_fullscreen.pending.is_none() {
            self.watch_fullscreen.pending = Some(false);
            window.toggle_fullscreen();
            cx.notify();
        }
        true
    }
    /// A sidebar/tool switch can hide the viewer without its Library button.
    /// Restore only the window this watcher owns, after pending platform work.
    pub(super) fn restore_watch_fullscreen_on_hide(&mut self, cx: &mut Context<Self>) {
        let Some(window) = self.watch_fullscreen.window.take() else { return; };
        let mut restore = Restore { entering: self.watch_fullscreen.pending == Some(true), exiting: self.watch_fullscreen.pending == Some(false) };
        cx.spawn(async move |view, cx| {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                cx.background_executor().timer(Duration::from_millis(10)).await;
                let Ok(watching) = view.update(cx, |editor, _| editor.clip_show) else { return; };
                if watching {
                    let _ = view.update(cx, |editor, _| editor.watch_fullscreen.window = Some(window));
                    return;
                }
                let Ok(action) = window.update(cx, |_, window, _| {
                    let action = restore.observe(window.is_fullscreen());
                    if action == RestoreAction::Exit { window.toggle_fullscreen(); }
                    action
                }) else { return; };
                match action {
                    RestoreAction::Exit => {
                        let _ = view.update(cx, |editor, _| editor.watch_fullscreen.pending = Some(false));
                    }
                    RestoreAction::Done => {
                        let _ = view.update(cx, |editor, _| {
                            editor.watch_fullscreen.pending = None;
                            editor.watch_fullscreen.window = None;
                        });
                        return;
                    }
                    RestoreAction::Wait => {}
                }
                if std::time::Instant::now() >= deadline {
                    let _ = view.update(cx, |editor, cx| {
                        editor.watch_fullscreen.window = Some(window);
                        editor.error = Some("Fullscreen restoration did not finish. Reopen the viewer and retry leaving fullscreen.".into());
                        cx.notify();
                    });
                    return;
                }
            }
        }).detach();
    }
}

#[derive(Default)]
pub(super) struct Proof { complete: bool }
fn protected_host(window: &Window) -> anyhow::Result<()> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::{GetForegroundWindow, IsWindowVisible}};
    let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("Fullscreen proof handle unavailable: {error}"))?.as_raw() else {
        anyhow::bail!("Fullscreen proof requires an owned Windows host");
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    anyhow::ensure!(!unsafe { IsWindowVisible(hwnd) }.as_bool(), "Fullscreen proof refuses a visible host");
    anyhow::ensure!(unsafe { GetForegroundWindow() } != hwnd, "Hidden fullscreen host became foreground");
    Ok(())
}
fn restored(a: Bounds<Pixels>, b: Bounds<Pixels>) -> bool {
    [a.left() - b.left(), a.top() - b.top(), a.size.width - b.size.width, a.size.height - b.size.height]
        .into_iter().all(|delta| f32::from(delta).abs() <= 2.)
}
impl Proof {
    pub(super) fn step(
        &mut self,
        view: &Entity<EditorView>,
        window: &mut Window,
        cx: &mut App,
        key: fn(&mut Window, &mut App, &str, bool, bool),
        history: fn(&mut Editor) -> anyhow::Result<serde_json::Value>,
    ) -> anyhow::Result<bool> {
        if self.complete { return Ok(true); }
        protected_host(window)?;
        anyhow::ensure!(!window.is_fullscreen(), "Hidden fullscreen controller proof must start windowed");
        let bounds = window.bounds();
        let before = view.update(cx, |editor, _| history(&mut editor.model))?;
        let volume = (view.read(cx).clip_show_volume, view.read(cx).clip_show_muted);
        view.update(cx, |editor, cx| {
            editor.pause();
            editor.watch_fullscreen.suppress_platform_for_proof = true;
            editor.clip_show = true;
            editor.focus.focus(window);
            cx.notify();
        });
        let result = (|| -> anyhow::Result<()> {
            // Paint the new key context. Dispatch uses production actions and
            // controller guards; only the platform operation is suppressed.
            window.draw(cx).clear();
            key(window, cx, "f11", false, false);
            anyhow::ensure!(view.read(cx).watch_fullscreen.pending == Some(true),
                "F11 did not dispatch the Clip show fullscreen request");
            key(window, cx, "f11", false, false);
            key(window, cx, "escape", false, false);
            key(window, cx, "escape", false, false);
            anyhow::ensure!(view.read(cx).watch_fullscreen.pending == Some(true) && view.read(cx).clip_show,
                "Repeated F11/Escape bypassed the pending-transition guard");
            anyhow::ensure!(!window.is_fullscreen() && restored(window.bounds(), bounds),
                "Synthetic controller proof changed native window geometry");
            anyhow::ensure!(view.update(cx, |editor, _| history(&mut editor.model))? == before,
                "Fullscreen request changed project or Undo/Redo");
            anyhow::ensure!((view.read(cx).clip_show_volume, view.read(cx).clip_show_muted) == volume,
                "Fullscreen request changed monitor volume or mute");
            protected_host(window)?;
            Ok(())
        })();
        // Always clear synthetic state, including a failed assertion. No hidden
        // native fullscreen placement was queued and no activation is needed.
        view.update(cx, |editor, cx| {
            editor.watch_fullscreen.suppress_platform_for_proof = false;
            editor.watch_fullscreen.pending = None;
            editor.watch_fullscreen.window = None;
            editor.clip_show = false;
            editor.focus.focus(window);
            cx.notify();
        });
        window.draw(cx).clear();
        result?;
        protected_host(window)?;
        println!("Hidden fullscreen controller proof passed: actual F11 dispatch, repeated-request/Escape pending guard, unchanged native bounds/project/history/volume; platform fullscreen suppressed. Visible native fullscreen geometry/restoration NOT VERIFIED: GPUI defers hidden fullscreen until show.");
        self.complete = true;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::{Exit, Restore, RestoreAction, State};

    #[test]
    fn hide_during_entry_waits_for_both_acknowledgments_without_duplicate_toggle() {
        let mut restore = Restore { entering: true, exiting: false };
        assert_eq!(restore.observe(false), RestoreAction::Wait);
        assert_eq!(restore.observe(false), RestoreAction::Wait);
        assert_eq!(restore.observe(true), RestoreAction::Exit);
        assert_eq!(restore.observe(true), RestoreAction::Wait);
        assert_eq!(restore.observe(false), RestoreAction::Done);
        let mut exiting = Restore { entering: false, exiting: true };
        assert_eq!(exiting.observe(true), RestoreAction::Wait);
        assert_eq!(exiting.observe(false), RestoreAction::Done);
    }

    #[test]
    fn fullscreen_controller_waits_for_ack_and_consumes_pending_escape() {
        let mut state = State::default();
        assert!(state.request_toggle(false));
        assert_eq!(state.pending, Some(true));
        assert!(!state.request_toggle(false));
        assert_eq!(state.request_exit(false), Exit::Pending);
        assert_eq!(state.pending, Some(true));
        // Simulate a platform observation, without claiming a native transition.
        state.observe(true);
        assert_eq!(state.pending, None);
        assert_eq!(state.request_exit(true), Exit::Toggle);
        assert_eq!(state.pending, Some(false));
        assert_eq!(state.request_exit(true), Exit::Pending);
        assert!(!state.request_toggle(true));
        state.observe(false);
        assert_eq!(state.pending, None);
        assert_eq!(state.request_exit(false), Exit::Windowed);
    }

    #[test]
    fn fullscreen_controller_f11_requests_exit_and_wrong_observation_keeps_pending() {
        let mut state = State::default();
        assert!(state.request_toggle(true));
        assert_eq!(state.pending, Some(false));
        state.observe(true);
        assert_eq!(state.pending, Some(false));
        assert_eq!(state.request_exit(true), Exit::Pending);
        state.observe(false);
        assert!(state.request_toggle(false));
        state.observe(false);
        assert_eq!(state.pending, Some(true));
    }
}
