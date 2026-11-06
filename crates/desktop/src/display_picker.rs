//! Read-only display discovery, with one actual enumeration admitted globally.
use crate::{preferences::SavedDisplay, theme::*};
use gpui::{prelude::*, *};
use windows_capture::display::Info;
use std::sync::{Arc, OnceLock, mpsc, atomic::{AtomicBool, Ordering}};

fn clean(text: &str) -> String { text.chars().filter(|c| !c.is_control()).take(256).collect() }
struct Lease(Arc<AtomicBool>);
impl Drop for Lease { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }
struct Job { reply: async_channel::Sender<Result<Vec<Info>, String>>, cancelled: Arc<AtomicBool>, _lease: Lease }
struct Discovery { requests: mpsc::SyncSender<Job>, busy: Arc<AtomicBool> }
impl Discovery {
    fn start(enumerate: impl Fn() -> Result<Vec<Info>, String> + Send + 'static) -> Result<Self, String> {
        let (requests, jobs) = mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new().name("rcr-display-list".into()).spawn(move || {
            while let Ok(job) = jobs.recv() {
                let result = if job.cancelled.load(Ordering::Acquire) { Err("Display picker closed".into()) }
                    else { std::panic::catch_unwind(std::panic::AssertUnwindSafe(&enumerate))
                        .unwrap_or_else(|_| Err("Display discovery failed unexpectedly. Refresh to retry.".into())) };
                drop(job._lease);
                let _ = job.reply.try_send(result);
            }
        }).map_err(|error| format!("Could not start display discovery: {error}"))?;
        Ok(Self { requests, busy: Arc::new(AtomicBool::new(false)) })
    }
    fn request(&self, cancelled: Arc<AtomicBool>) -> Result<async_channel::Receiver<Result<Vec<Info>, String>>, String> {
        self.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Display discovery is still running. Wait, then Refresh.".to_owned())?;
        let (reply, receiver) = async_channel::bounded(1);
        self.requests.try_send(Job { reply, cancelled, _lease: Lease(self.busy.clone()) })
            .map_err(|_| "Display discovery worker stopped. Restart rcr.".to_owned())?;
        Ok(receiver)
    }
}
fn request(cancelled: Arc<AtomicBool>) -> Result<async_channel::Receiver<Result<Vec<Info>, String>>, String> {
    static WORKER: OnceLock<Result<Discovery, String>> = OnceLock::new();
    WORKER.get_or_init(|| Discovery::start(|| windows_capture::display::enumerate().map_err(|error| format!("{error:#}"))))
        .as_ref().map_err(Clone::clone)?.request(cancelled)
}
struct Row { key: String, title: String, detail: String, choice: Option<SavedDisplay>, unavailable: Option<String> }
#[derive(Default)]
struct Model { entries: Vec<Info>, retained: Option<SavedDisplay>, highlighted: String, checked: bool }
impl Model {
    fn new(saved: Option<SavedDisplay>) -> Self {
        let highlighted = saved.as_ref().map_or_else(|| "primary".into(), |saved| format!("device:{}", saved.id));
        Self { retained: saved, highlighted, ..Self::default() }
    }
    fn replace(&mut self, entries: Vec<Info>) {
        self.checked = true;
        let mut seen = std::collections::BTreeSet::new();
        // Discovery already enforces its topology bound. Truncating again here
        // could incorrectly label a connected saved display as unavailable.
        self.entries = entries.into_iter().filter(|entry| seen.insert((entry.device_path.clone(), entry.device_name.clone()))).collect();
    }
    fn rows(&self) -> Vec<Row> {
        let primary = self.entries.iter().find(|entry| entry.primary);
        let mut rows = vec![Row { key: "primary".into(), title: "Windows primary display".into(),
            detail: "Follow Windows primary when capture starts; keep that display until stopped.".into(),
            choice: None, unavailable: primary.and_then(|entry| entry.unsupported_reason.clone()) }];
        if let Some(saved) = &self.retained && !self.entries.iter().any(|entry| entry.device_path.as_deref() == Some(saved.id.as_str())) {
            rows.push(Row { key: format!("device:{}", saved.id), title: saved.display_label(),
                detail: if self.checked { "Saved display unavailable. Reconnect and Refresh, or explicitly choose another." }
                    else { "Saved display not verified. Refresh to check its availability." }.into(),
                choice: Some(saved.clone()), unavailable: Some(if self.checked {
                    "The saved display is unavailable; it has not been replaced with another display."
                } else { "The saved display has not been verified. Refresh before selecting." }.into()) });
        }
        for entry in &self.entries {
            let title = SavedDisplay { id: String::new(), label: entry.label.clone() }.display_label();
            rows.push(Row { key: entry.device_path.as_ref().map_or_else(|| format!("unknown:{}", entry.device_name), |id| format!("device:{id}")),
                detail: format!("{} × {} · {} · {}{}", entry.width, entry.height, clean(&entry.gpu_name), clean(&entry.device_name), if entry.primary { " · Primary" } else { "" }),
                choice: entry.device_path.as_ref().map(|id| SavedDisplay { id: id.clone(), label: title.clone() }), title,
                unavailable: entry.unsupported_reason.clone().or_else(|| entry.device_path.is_none().then(|| "Display identity unavailable. Refresh before selecting.".into())) });
        }
        rows
    }
    fn highlight(&mut self, index: usize) {
        if let Some(row) = self.rows().get(index) {
            self.highlighted = row.key.clone();
            if row.unavailable.is_none() { self.retained = row.choice.clone(); }
        }
    }
    fn choice(&self) -> Result<Option<SavedDisplay>, String> {
        let row = self.rows().into_iter().find(|row| row.key == self.highlighted).ok_or("Choose a display or Windows primary explicitly.")?;
        if let Some(reason) = row.unavailable { return Err(reason); }
        Ok(row.choice)
    }
    fn highlighted_index(&self) -> Option<usize> {
        self.rows().iter().position(|row| row.key == self.highlighted)
    }
}
pub enum PickerEvent { Selected(Option<SavedDisplay>), Closed }
pub struct DisplayPicker {
    model: Model, focus: FocusHandle, refresh_focus: FocusHandle, select_focus: FocusHandle, close_focus: FocusHandle,
    restore: FocusHandle, scroll: ScrollHandle, cancelled: Arc<AtomicBool>, generation: u64,
    loading: bool, closed: bool, initial_focus: bool, fixture: bool, error: Option<String>,
}
impl EventEmitter<PickerEvent> for DisplayPicker {}
impl Drop for DisplayPicker { fn drop(&mut self) { self.cancelled.store(true, Ordering::Release); } }
impl DisplayPicker {
    pub fn new(current: Option<SavedDisplay>, restore: FocusHandle, fixture: bool, cx: &mut Context<Self>) -> Self {
        let mut this = Self { model: Model::new(current), focus: cx.focus_handle().tab_stop(true), refresh_focus: cx.focus_handle().tab_stop(true),
            select_focus: cx.focus_handle().tab_stop(true), close_focus: cx.focus_handle().tab_stop(true), restore, scroll: ScrollHandle::new(),
            cancelled: Arc::new(AtomicBool::new(false)), generation: 0, loading: false, closed: false, initial_focus: true, fixture, error: None };
        if fixture { this.model.replace(fixture_displays()); } else { this.refresh(cx); }
        this
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.closed || self.loading { return; }
        if self.fixture { self.model.replace(fixture_displays()); self.error = None; cx.notify(); return; }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        match request(self.cancelled.clone()) {
            Ok(reply) => {
                self.loading = true; self.error = None;
                cx.spawn(async move |view, cx| {
                    let result = reply.recv().await.unwrap_or_else(|_| Err("Display discovery stopped before returning its list.".into()));
                    let _ = view.update(cx, |this, cx| {
                        if this.closed || this.generation != generation { return; }
                        this.loading = false;
                        match result { Ok(entries) => this.model.replace(entries), Err(error) => this.error = Some(error.chars().take(2000).collect()) }
                        cx.notify();
                    });
                }).detach();
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
    fn finish(&mut self, selected: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed { return; }
        if selected && self.loading { return; }
        let event = if selected { match self.model.choice() { Ok(choice) => PickerEvent::Selected(choice), Err(error) => { self.error = Some(error); cx.notify(); return; } } }
            else { PickerEvent::Closed };
        self.closed = true; self.cancelled.store(true, Ordering::Release);
        self.restore.focus(window); cx.emit(event);
    }
}
impl Render for DisplayPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.initial_focus { self.initial_focus = false; self.focus.focus(window); }
        let mut list = div().id("display-list").track_focus(&self.focus).track_scroll(&self.scroll).flex_1().min_h_0().overflow_y_scroll()
            .border_1().border_color(rgb(LINE)).focus(|s| s.border_color(rgb(ACCENT))).rounded_md().flex().flex_col();
        for (index, row) in self.model.rows().into_iter().enumerate() {
            list = list.child(div().id(("display-row", index)).p_3().min_w_0().flex_shrink_0().flex().flex_col().gap_1()
                .bg(rgb(if row.key == self.model.highlighted { 0x173025 } else { PANEL }))
                .child(div().min_w_0().whitespace_normal().child(row.title))
                .child(muted(row.detail).whitespace_normal())
                .when_some(row.unavailable, |s, reason| s.child(muted(clean(&reason)).text_color(rgb(0xf0b48b)).whitespace_normal()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.model.highlight(index);
                    if let Some(index) = this.model.highlighted_index() { this.scroll.scroll_to_item(index); }
                    this.focus.focus(window); this.error = None; cx.notify();
                })));
        }
        div().id("display-picker-modal").absolute().inset_0().occlude().bg(rgba(0x000000aa)).flex().items_center().justify_center().p_4()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if key == "escape" { this.finish(false, window, cx); }
                else if key == "tab" {
                    let stops = [&this.focus, &this.refresh_focus, &this.select_focus, &this.close_focus];
                    let current = stops.iter().position(|focus| focus.is_focused(window)).unwrap_or(0);
                    let next = if event.keystroke.modifiers.shift { (current + 3) % 4 } else { (current + 1) % 4 };
                    stops[next].focus(window);
                } else if this.focus.is_focused(window) && matches!(key, "up" | "down" | "home" | "end") {
                    let rows = this.model.rows(); let last = rows.len().saturating_sub(1);
                    let current = rows.iter().position(|row| row.key == this.model.highlighted).unwrap_or(0);
                    let next = match key { "up" => current.saturating_sub(1), "down" => (current + 1).min(last), "home" => 0, _ => last };
                    this.model.highlight(next);
                    if let Some(index) = this.model.highlighted_index() { this.scroll.scroll_to_item(index); }
                    cx.notify();
                } else if key == "enter" && this.focus.is_focused(window) { this.finish(true, window, cx); }
                else if matches!(key, "enter" | "space") { return; }
                window.prevent_default(); cx.stop_propagation();
            }))
            .child(div().w(px(560.)).max_w_full().h(px(500.)).max_h(relative(0.9)).p_4().rounded_lg().bg(rgb(PANEL))
                .border_1().border_color(rgb(LINE)).flex().flex_col().gap_3()
                .child(div().text_size(px(18.)).child("Capture display"))
                .child(muted("Choose a display. Capture stays on that display until stopped.")).child(list)
                .when(self.loading, |s| s.child(muted("Finding displays…")))
                .when(!self.loading && self.model.checked && self.model.entries.is_empty() && self.error.is_none(),
                    |s| s.child(muted("No physical displays found. Reconnect a monitor, then Refresh.")))
                .when_some(self.error.clone(), |s, error| s.child(muted(error).text_color(rgb(0xffb0b0)).whitespace_normal()))
                .child(muted("Unavailable selections are kept until you explicitly choose another. Stop capture before choosing another display.").text_size(px(11.)))
                .child(div().flex().justify_end().gap_2()
                    .child(button("refresh-displays", if self.loading { "Refreshing…" } else { "Refresh" }).track_focus(&self.refresh_focus).on_click(cx.listener(|this, _, _, cx| this.refresh(cx))))
                    .child(button("select-display", "Select").track_focus(&self.select_focus).opacity(if self.loading || self.model.choice().is_err() { 0.5 } else { 1. })
                        .on_click(cx.listener(|this, _, window, cx| this.finish(true, window, cx))))
                    .child(button("cancel-display", "Cancel").track_focus(&self.close_focus).on_click(cx.listener(|this, _, window, cx| this.finish(false, window, cx))))))
    }
}
fn fixture_displays() -> Vec<Info> {
    [true, false].into_iter().map(|primary| Info { device_path: Some(format!("fixture-display-{primary}")), label: if primary { "Fixture primary monitor" } else { "Fixture secondary monitor" }.into(),
        device_name: if primary { "Fixture DISPLAY1" } else { "Fixture DISPLAY2" }.into(), gpu_name: "Fixture NVIDIA adapter".into(), width: 2560, height: 1440, primary,
        unsupported_reason: (!primary).then(|| "Diagnostic fixture: HDR display is not supported for SDR capture.".into()) }).collect()
}

/// Owned-window keyboard proof; seeded lists only, never enumeration or capture.
pub fn smoke(view: Entity<DisplayPicker>, window: &mut Window, cx: &mut App) -> anyhow::Result<()> {
    anyhow::ensure!(view.read(cx).fixture, "Display proof requires explicit fixture data");
    fn key(window: &mut Window, cx: &mut App, text: &str) {
        let keystroke = Keystroke::parse(text).expect("Display fixture key");
        window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent { keystroke: keystroke.clone(), is_held: false }), cx);
        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
        window.draw(cx).clear();
    }
    anyhow::ensure!(view.read(cx).model.choice().is_err(), "Missing display silently fell back before selection");
    key(window, cx, "home");
    anyhow::ensure!(view.read(cx).model.choice().is_ok(), "Primary policy was not explicitly keyboard-selectable");
    key(window, cx, "end"); key(window, cx, "enter");
    anyhow::ensure!(!view.read(cx).closed && view.read(cx).error.is_some(), "Unsupported display was accepted");
    for _ in 0..4 { key(window, cx, "tab"); }
    anyhow::ensure!(view.read(cx).focus.is_focused(window), "Tab escaped display picker");
    key(window, cx, "shift-tab");
    anyhow::ensure!(view.read(cx).close_focus.is_focused(window), "Reverse Tab escaped display picker");
    key(window, cx, "escape");
    anyhow::ensure!(view.read(cx).closed, "Escape did not close display picker");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Discovery, Model, SavedDisplay, fixture_displays};
    use std::sync::{Arc, mpsc, atomic::{AtomicBool, Ordering}};
    #[test]
    fn saved_display_after_128_rows_remains_available() {
        let mut entries = Vec::new();
        for index in 0..130 {
            let mut entry = fixture_displays().remove(0);
            entry.device_path = Some(format!("device-{index}"));
            entry.device_name = format!("DISPLAY{index}");
            entry.primary = index == 0;
            entry.unsupported_reason = None;
            entries.push(entry);
        }
        let saved = SavedDisplay { id: "device-129".into(), label: "Last monitor".into() };
        let mut model = Model::new(Some(saved.clone()));
        model.replace(entries);
        assert_eq!(model.entries.len(), 130);
        assert_eq!(model.choice().unwrap().unwrap().id, saved.id);
    }
    #[test]
    fn explicit_missing_and_unsupported_displays_never_fall_back_to_primary() {
        let saved = SavedDisplay { id: "missing".into(), label: "Saved monitor".into() };
        let mut model = Model::new(Some(saved.clone())); model.replace(fixture_displays());
        assert!(model.choice().unwrap_err().contains("unavailable"));
        assert_eq!(model.retained.as_ref(), Some(&saved));
        model.highlight(0); assert_eq!(model.choice().unwrap(), None);
        model.highlight(2); assert!(model.choice().unwrap_err().contains("HDR"));
        model.highlight(1); let chosen = model.choice().unwrap().unwrap();
        model.replace(Vec::new()); assert!(model.choice().is_err()); assert_eq!(model.retained, Some(chosen));
    }
    #[test]
    fn blocked_discovery_retains_actual_admission_after_picker_closes() {
        let (entered, seen) = mpsc::sync_channel(1); let (release, gate) = mpsc::sync_channel(1);
        let discovery = Discovery::start(move || { entered.send(()).unwrap(); gate.recv().unwrap(); Ok(Vec::new()) }).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let reply = discovery.request(cancelled.clone()).unwrap(); seen.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        cancelled.store(true, Ordering::Release); drop(reply);
        assert!(discovery.request(Arc::new(AtomicBool::new(false))).is_err());
        release.send(()).unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while discovery.busy.load(Ordering::Acquire) { assert!(std::time::Instant::now() < until); std::thread::yield_now(); }
    }
    #[test]
    fn unsupported_primary_is_not_replaced_by_an_available_secondary() {
        let mut entries = fixture_displays();
        entries[0].unsupported_reason = Some("Primary monitor uses unsupported GPU".into());
        entries[1].unsupported_reason = None;
        let mut model = Model::new(None); model.replace(entries);
        assert!(model.choice().unwrap_err().contains("unsupported GPU"));
        model.highlight(2);
        assert_eq!(model.choice().unwrap().unwrap().id, "fixture-display-false");
    }
    #[test]
    fn selecting_available_display_after_missing_row_removal_reveals_selected_identity() {
        let saved = SavedDisplay { id: "missing".into(), label: "Disconnected monitor".into() };
        let mut model = Model::new(Some(saved)); model.replace(fixture_displays());
        assert_eq!(model.highlighted_index(), Some(1));
        assert_eq!(model.rows().len(), 4);
        // Down or pointer selection targets the primary physical display at old
        // index 2; adopting its identity removes the unrelated missing row.
        model.highlight(2);
        assert_eq!(model.rows().len(), 3);
        assert_eq!(model.highlighted_index(), Some(1));
        assert_eq!(model.choice().unwrap().unwrap().id, "fixture-display-true");
        let next = model.highlighted_index().unwrap() + 1;
        model.highlight(next);
        assert_eq!(model.highlighted_index(), Some(2));
        assert!(model.choice().unwrap_err().contains("HDR"));
    }
}
