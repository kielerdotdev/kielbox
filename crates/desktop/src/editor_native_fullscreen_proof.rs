//! Opt-in owned-window native evaluation. This host is shown without activation;
//! no OS input, capture, clipboard operation, or other application's HWND is used.
//! Wire as a child of `watch_fullscreen` to inspect the controller's pending state.
use super::{App, Bounds, Editor, EditorView, Entity, Pixels, Window};
use anyhow::{Context as _, ensure};
use gpui::Subscription;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use windows::Win32::{
    Foundation::{ERROR_SUCCESS, GetLastError, HANDLE, HWND, RECT, SetLastError},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow},
    UI::WindowsAndMessaging::{GetForegroundWindow, GetPropW, GetWindowLongPtrW, GetWindowRect,
        GetWindowThreadProcessId, GWL_EXSTYLE, GWL_STYLE, HWND_BOTTOM, IsWindow,
        IsWindowVisible, RemovePropW, SetPropW, SetWindowLongPtrW, SetWindowPos, SWP_FRAMECHANGED,
        SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SWP_SHOWWINDOW, WS_CAPTION, WS_EX_NOACTIVATE, WS_MAXIMIZEBOX,
        WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME},
};

struct Host { hwnd: HWND, original_extended_style: isize, marker: Box<u8> }
impl Host {
    fn marker(&self) -> HANDLE { HANDLE((&*self.marker as *const u8) as *mut _) }
    fn show(window: &Window) -> anyhow::Result<Self> {
        let RawWindowHandle::Win32(raw) = HasWindowHandle::window_handle(window)
            .map_err(|e| anyhow::anyhow!("Native fullscreen host handle: {e}"))?.as_raw() else {
            anyhow::bail!("Native fullscreen proof requires a Windows host");
        };
        let hwnd = HWND(raw.hwnd.get() as *mut _);
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)); }
        ensure!(pid == std::process::id(), "Native fullscreen proof refuses another process's window");
        ensure!(!unsafe { IsWindowVisible(hwnd) }.as_bool(), "Native fullscreen fixture must own a never-shown host");
        ensure!(unsafe { GetForegroundWindow() } != hwnd, "Native fullscreen host was already foreground");
        let original_extended_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        let host = Self { hwnd, original_extended_style, marker: Box::new(0) };
        let rect = host.rect()?;
        let monitor = host.monitor()?;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let nudge = |start: i32, length: i32, low: i32, high: i32| {
            let last = (high - length).max(low);
            let next = (start + 1).clamp(low, last);
            if next == start && next > low { next - 1 } else { next }
        };
        let x = nudge(rect.left, width, monitor.left, monitor.right);
        let y = nudge(rect.top, height, monitor.top, monitor.bottom);
        ensure!(x != rect.left || y != rect.top, "Owned native proof host has no room for its initialization move");
        unsafe {
            SetPropW(hwnd, windows::core::w!("rcr-native-fullscreen-proof-owner"), Some(host.marker()))?;
            SetLastError(ERROR_SUCCESS);
            let previous = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, original_extended_style | WS_EX_NOACTIVATE.0 as isize);
            ensure!(previous != 0 || GetLastError() == ERROR_SUCCESS, "Could not protect native fullscreen host from activation");
            // No GPUI resize/activate call: resize lacks NOACTIVATE, and activate
            // can synthesize OS input. Only this owned HWND is positioned/shown.
            // Force a real WM_MOVE: a never-shown GPUI window still caches its
            // outer origin, whereas restored visible windows report client
            // origin. Record the baseline only after this move has settled.
            SetWindowPos(hwnd, Some(HWND_BOTTOM), x, y, width, height,
                SWP_SHOWWINDOW | SWP_NOACTIVATE | SWP_FRAMECHANGED)?;
        }
        Ok(host)
    }
    fn guard(&self, activation: bool) -> anyhow::Result<()> {
        ensure!(unsafe { IsWindow(Some(self.hwnd)) }.as_bool(), "Owned native fullscreen host disappeared");
        ensure!(unsafe { GetPropW(self.hwnd, windows::core::w!("rcr-native-fullscreen-proof-owner")) } == self.marker(),
            "Native fullscreen host ownership marker changed");
        ensure!(unsafe { IsWindowVisible(self.hwnd) }.as_bool(), "Native fullscreen host unexpectedly became hidden");
        ensure!(!activation && unsafe { GetForegroundWindow() } != self.hwnd,
            "Native fullscreen proof host was activated; evaluation stopped");
        ensure!(unsafe { GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) } & WS_EX_NOACTIVATE.0 as isize != 0,
            "Native fullscreen host lost its nonactivation style");
        Ok(())
    }
    fn rect(&self) -> anyhow::Result<RECT> {
        let mut rect = RECT::default(); unsafe { GetWindowRect(self.hwnd, &mut rect)?; } Ok(rect)
    }
    fn monitor(&self) -> anyhow::Result<RECT> {
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        ensure!(unsafe { GetMonitorInfoW(MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST), &mut info) }.as_bool(),
            "Could not measure the owned fullscreen host's monitor");
        Ok(info.rcMonitor)
    }
    fn style(&self) -> u32 { unsafe { GetWindowLongPtrW(self.hwnd, GWL_STYLE) as u32 } }
}
impl Drop for Host {
    fn drop(&mut self) {
        if !unsafe { IsWindow(Some(self.hwnd)) }.as_bool() { return; }
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(self.hwnd, Some(&mut pid)); }
        if pid != std::process::id() { return; }
        if unsafe { GetPropW(self.hwnd, windows::core::w!("rcr-native-fullscreen-proof-owner")) } != self.marker() { return; }
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, 0, 0, 0, 0,
                SWP_HIDEWINDOW | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOMOVE | SWP_NOSIZE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, self.original_extended_style);
            let _ = RemovePropW(self.hwnd, windows::core::w!("rcr-native-fullscreen-proof-owner"));
        }
    }
}
fn rect_equal(a: RECT, b: RECT) -> bool {
    [a.left - b.left, a.top - b.top, a.right - b.right, a.bottom - b.bottom].into_iter().all(|v| v.abs() <= 2)
}
fn bounds_equal(a: Bounds<Pixels>, b: Bounds<Pixels>) -> bool {
    [a.left() - b.left(), a.top() - b.top(), a.size.width - b.size.width, a.size.height - b.size.height]
        .into_iter().all(|v| f32::from(v).abs() <= 2.)
}

#[derive(Default)]
pub(in crate::editor) struct Proof {
    stage: u8,
    deadline: Option<Instant>,
    stage_started: Option<Instant>,
    host: Option<Host>,
    activation: Arc<AtomicBool>,
    observer: Option<Subscription>,
    original_rect: Option<RECT>,
    original_bounds: Option<Bounds<Pixels>>,
    original_style: u32,
    original_extended_style: isize,
    history: Option<serde_json::Value>,
    volume: Option<(f32, bool)>,
    measurements: Vec<serde_json::Value>,
}
impl Proof {
    fn next(&mut self, stage: u8) {
        self.stage = stage; self.stage_started = Some(Instant::now());
        self.deadline = Some(Instant::now() + Duration::from_secs(5));
    }
    fn measured(&mut self, name: &str) {
        self.measurements.push(serde_json::json!({"transition":name,
            "elapsed_ms":self.stage_started.map(|t| t.elapsed().as_millis()).unwrap_or(0)}));
    }
    fn windowed(&self, window: &Window) -> anyhow::Result<bool> {
        let host = self.host.as_ref().context("Native proof host missing")?;
        Ok(!window.is_fullscreen() && rect_equal(host.rect()?, self.original_rect.unwrap())
            && bounds_equal(window.bounds(), self.original_bounds.unwrap()) && host.style() == self.original_style
            && unsafe { GetWindowLongPtrW(host.hwnd, GWL_EXSTYLE) } == self.original_extended_style)
    }
    fn fullscreen(&self, window: &Window) -> anyhow::Result<bool> {
        let host = self.host.as_ref().context("Native proof host missing")?;
        let expected = self.original_style & !(WS_THICKFRAME | WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX | WS_CAPTION).0;
        Ok(window.is_fullscreen() && rect_equal(host.rect()?, host.monitor()?) && host.style() == expected)
    }
    fn diagnostic(&self, view: &Entity<EditorView>, window: &Window, cx: &App) -> serde_json::Value {
        let rect = |r: RECT| [r.left, r.top, r.right, r.bottom];
        let bounds = |b: Bounds<Pixels>| [f32::from(b.left()), f32::from(b.top()), f32::from(b.size.width), f32::from(b.size.height)];
        let host = self.host.as_ref();
        let expected_fullscreen = matches!(self.stage, 2 | 4 | 6);
        let expected_style = if expected_fullscreen {
            self.original_style & !(WS_THICKFRAME | WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX | WS_CAPTION).0
        } else { self.original_style };
        let editor = view.read(cx);
        serde_json::json!({"stage":self.stage,
            "fullscreen_actual":window.is_fullscreen(),"fullscreen_expected":expected_fullscreen,
            "rect_actual":host.and_then(|h| h.rect().ok()).map(rect),
            "rect_expected":if expected_fullscreen {host.and_then(|h|h.monitor().ok()).map(rect)}else{self.original_rect.map(rect)},
            "logical_bounds_actual":bounds(window.bounds()),"logical_bounds_expected":self.original_bounds.map(bounds),
            "style_actual":host.map(|h|format!("{:#010x}",h.style())),"style_expected":format!("{expected_style:#010x}"),
            "extended_style_actual":host.map(|h|format!("{:#010x}",unsafe{GetWindowLongPtrW(h.hwnd,GWL_EXSTYLE)})),
            "extended_style_expected":format!("{:#010x}",self.original_extended_style),
            "pending":editor.watch_fullscreen.pending,"clip_show":editor.clip_show,"editor_visible":editor.visible,
            "activation_observed":self.activation.load(Ordering::Acquire)})
    }
    fn cleanup(&mut self, view: &Entity<EditorView>, cx: &mut App) {
        self.host.take(); // Hide only this owned host; never restore other focus.
        self.observer.take();
        view.update(cx, |editor, cx| {
            editor.watch_fullscreen = Default::default();
            editor.clip_show = false; editor.visible = true; cx.notify();
        });
    }
    pub(in crate::editor) fn step(
        &mut self, view: &Entity<EditorView>, window: &mut Window, cx: &mut App,
        key: fn(&mut Window, &mut App, &str, bool, bool),
        history: fn(&mut Editor) -> anyhow::Result<serde_json::Value>,
    ) -> anyhow::Result<bool> {
        let result = self.advance(view, window, cx, key, history);
        if result.is_err() { self.cleanup(view, cx); }
        result
    }
    fn advance(
        &mut self, view: &Entity<EditorView>, window: &mut Window, cx: &mut App,
        key: fn(&mut Window, &mut App, &str, bool, bool),
        history: fn(&mut Editor) -> anyhow::Result<serde_json::Value>,
    ) -> anyhow::Result<bool> {
        if self.stage == 10 { return Ok(true); }
        if self.stage == 0 {
            ensure!(matches!(view.read(cx).preview_output, editor_core::PlaybackOutput::Silent),
                "Native fullscreen proof requires Silent playback");
            ensure!(!window.is_fullscreen(), "Native fullscreen proof must start windowed");
            self.history = Some(view.update(cx, |editor, _| history(&mut editor.model))?);
            self.volume = Some((view.read(cx).clip_show_volume, view.read(cx).clip_show_muted));
            let activated = self.activation.clone();
            self.observer = Some(view.update(cx, |_, cx| cx.observe_window_activation(window, move |_, window, _| {
                if window.is_window_active() { activated.store(true, Ordering::Release); }
            })));
            self.host = Some(Host::show(window)?);
            self.next(1); return Ok(false);
        }
        self.host.as_ref().context("Native proof host missing")?.guard(self.activation.load(Ordering::Acquire))?;
        ensure!(self.deadline.is_some_and(|d| Instant::now() < d),
            "Native fullscreen transition timed out: {}", self.diagnostic(view, window, cx));
        match self.stage {
            1 => {
                if self.stage_started.unwrap().elapsed() < Duration::from_millis(80) { return Ok(false); }
                let host = self.host.as_ref().unwrap();
                self.original_rect = Some(host.rect()?); self.original_bounds = Some(window.bounds());
                self.original_style = host.style();
                self.original_extended_style = unsafe { GetWindowLongPtrW(host.hwnd, GWL_EXSTYLE) };
                view.update(cx, |editor, cx| { editor.pause(); editor.clip_show = true; editor.focus.focus(window); cx.notify(); });
                window.draw(cx).clear(); key(window, cx, "f11", false, false); self.next(2);
            }
            2 => {
                if !self.fullscreen(window)? { return Ok(false); }
                self.measured("F11 enter");
                key(window, cx, "escape", false, false);
                ensure!(view.read(cx).watch_fullscreen.pending == Some(false) || !window.is_fullscreen(),
                    "Escape did not dispatch fullscreen exit: {}", self.diagnostic(view, window, cx));
                key(window, cx, "escape", false, false);
                ensure!(view.read(cx).clip_show, "Rapid Escape ended watching before fullscreen exit acknowledged"); self.next(3);
            }
            3 => {
                if !self.windowed(window)? { return Ok(false); }
                self.measured("Escape exit and restore"); key(window, cx, "f11", false, false); self.next(4);
            }
            4 => {
                if !self.fullscreen(window)? { return Ok(false); }
                self.measured("F11 second enter"); key(window, cx, "f11", false, false); self.next(5);
            }
            5 => {
                if !self.windowed(window)? { return Ok(false); }
                self.measured("F11 exit and restore");
                key(window, cx, "escape", false, false);
                ensure!(!view.read(cx).clip_show, "Windowed Escape did not return to editing");
                view.update(cx, |editor, cx| { editor.clip_show = true; editor.focus.focus(window); cx.notify(); });
                window.draw(cx).clear(); key(window, cx, "f11", false, false); self.next(6);
            }
            6 => {
                if !self.fullscreen(window)? { return Ok(false); }
                self.measured("Enter before sidebar hide"); view.update(cx, |editor, cx| editor.hide(cx)); self.next(7);
            }
            7 => {
                if !self.windowed(window)? { return Ok(false); }
                ensure!(view.read(cx).watch_fullscreen.pending.is_none(), "Hide restoration left a pending fullscreen controller");
                self.measured("Sidebar hide restoration");
                view.update(cx, |editor, cx| { editor.visible = true; editor.clip_show = true; editor.focus.focus(window); cx.notify(); });
                window.draw(cx).clear();
                // Same App update: hide before the async platform enter runs.
                key(window, cx, "f11", false, false);
                view.update(cx, |editor, cx| editor.hide(cx)); self.next(8);
            }
            8 => {
                if self.stage_started.unwrap().elapsed() < Duration::from_millis(300) || !self.windowed(window)? { return Ok(false); }
                ensure!(view.read(cx).watch_fullscreen.pending.is_none(), "Hide during entry stranded a pending fullscreen controller");
                ensure!(!view.read(cx).clip_show, "Hide during entry retained watch mode");
                ensure!(view.read(cx).error.is_none(), "Native restoration failed: {:?}", view.read(cx).error);
                self.measured("Immediate hide during pending entry"); self.next(9);
            }
            9 => {
                if !self.windowed(window)? { return Ok(false); }
                ensure!(Some(view.update(cx, |editor, _| history(&mut editor.model))?) == self.history,
                    "Native fullscreen changed project or Undo/Redo");
                ensure!(Some((view.read(cx).clip_show_volume, view.read(cx).clip_show_muted)) == self.volume,
                    "Native fullscreen changed playback volume/mute");
                self.host.as_ref().unwrap().guard(self.activation.load(Ordering::Acquire))?;
                println!("Native fullscreen proof JSON: {}", serde_json::json!({"passed":true,"activation_observed":false,
                    "os_input_sent":false,"platform_toggle_suppressed":false,"transitions":self.measurements}));
                self.cleanup(view, cx); self.stage = 10; return Ok(true);
            }
            _ => anyhow::bail!("Unexpected native fullscreen stage"),
        }
        Ok(false)
    }
}
