//! Runtime-only application audio choices, bound to one selected process lifetime.
use crate::{text_input::TextInput, theme::*};
use gpui::{prelude::*, *};
use std::{
    cell::Cell,
    collections::BTreeSet,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_PROCESSES: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AudioChoice {
    #[default]
    Desktop,
    Process(ProcessEntry),
}
impl AudioChoice {
    pub fn label(&self) -> &str {
        match self {
            Self::Desktop => "Desktop audio",
            Self::Process(p) => &p.exe_name,
        }
    }
    fn same_instance(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Desktop, Self::Desktop) => true,
            (Self::Process(a), Self::Process(b)) => {
                a.pid == b.pid && a.created_at_hns == b.created_at_hns
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub created_at_hns: u64,
    pub exe_name: String,
    pub window_title: String,
}

#[derive(Default)]
struct ListModel {
    entries: Vec<ProcessEntry>,
    query: String,
    highlight: AudioChoice,
}
impl ListModel {
    fn replace(&mut self, mut entries: Vec<ProcessEntry>) {
        entries.sort_by_cached_key(|p| {
            (
                p.exe_name.to_lowercase(),
                p.window_title.to_lowercase(),
                p.pid,
            )
        });
        let mut seen = BTreeSet::new();
        entries.retain(|p| p.pid != 0 && seen.insert((p.pid, p.created_at_hns)));
        entries.truncate(MAX_PROCESSES);
        self.entries = entries;
        self.reconcile();
    }
    fn choices(&self) -> Vec<AudioChoice> {
        let mut out = vec![AudioChoice::Desktop];
        out.extend(
            self.entries
                .iter()
                .filter(|p| {
                    format!("{} {} {}", p.exe_name, p.window_title, p.pid)
                        .to_lowercase()
                        .contains(&self.query)
                })
                .cloned()
                .map(AudioChoice::Process),
        );
        out
    }
    fn reconcile(&mut self) {
        let choices = self.choices();
        self.highlight = choices
            .iter()
            .find(|p| {
                p.same_instance(&self.highlight)
                    && (self.query.is_empty() || matches!(p, AudioChoice::Process(_)))
            })
            .or_else(|| (!self.query.is_empty()).then(|| choices.get(1)).flatten())
            .cloned()
            .unwrap_or_default();
    }
    fn filter(&mut self, query: String) {
        self.query = query.trim().to_lowercase();
        self.reconcile();
    }
    fn navigate(&mut self, key: &str) {
        let choices = self.choices();
        let current = choices
            .iter()
            .position(|p| p.same_instance(&self.highlight))
            .unwrap_or(0);
        let last = choices.len() - 1; // Desktop always remains available.
        let next = match key {
            "up" => current.saturating_sub(1),
            "down" => (current + 1).min(last),
            "home" => 0,
            "end" => last,
            _ => current,
        };
        self.highlight = choices[next].clone();
    }
    fn accept(&self, from_search: bool) -> Result<AudioChoice, &'static str> {
        if from_search && !self.query.is_empty() {
            self.choices()
                .into_iter()
                .find(|p| matches!(p, AudioChoice::Process(_)) && p.same_instance(&self.highlight))
                .or_else(|| {
                    self.choices()
                        .into_iter()
                        .find(|p| matches!(p, AudioChoice::Process(_)))
                })
                .ok_or("No matching application. Clear search or select Desktop explicitly.")
        } else {
            Ok(self.highlight.clone())
        }
    }
}

pub enum PickerEvent {
    Selected(AudioChoice),
    Closed,
}
pub struct AudioSourcePicker {
    model: ListModel,
    search: Entity<TextInput>,
    list_focus: FocusHandle,
    refresh_focus: FocusHandle,
    close_focus: FocusHandle,
    restore_focus: FocusHandle,
    scroll: ScrollHandle,
    row_label_width: Rc<Cell<f32>>,
    row_text_readable: Rc<Cell<bool>>,
    cancelled: Arc<AtomicBool>,
    loading: bool,
    closed: bool,
    truncated: bool,
    error: Option<String>,
    request_focus: bool,
}
impl EventEmitter<PickerEvent> for AudioSourcePicker {}
impl Drop for AudioSourcePicker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
impl AudioSourcePicker {
    pub fn new(current: AudioChoice, restore_focus: FocusHandle, cx: &mut Context<Self>) -> Self {
        Self::create(current, restore_focus, true, cx)
    }
    fn create(
        current: AudioChoice,
        restore_focus: FocusHandle,
        scan: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            let mut input = TextInput::new(cx, "");
            input.set_placeholder("Search running applications…", cx);
            input
        });
        cx.observe(&search, |this, input, cx| {
            this.model.filter(input.read(cx).value());
            this.error = None;
            this.scroll.scroll_to_item(0);
            cx.notify();
        })
        .detach();
        let mut picker = Self {
            model: ListModel {
                highlight: current,
                ..Default::default()
            },
            search,
            list_focus: cx.focus_handle().tab_stop(true),
            refresh_focus: cx.focus_handle().tab_stop(true),
            close_focus: cx.focus_handle().tab_stop(true),
            restore_focus,
            scroll: ScrollHandle::new(),
            row_label_width: Rc::new(Cell::new(0.)),
            row_text_readable: Rc::new(Cell::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            loading: false,
            closed: false,
            truncated: false,
            error: None,
            request_focus: true,
        };
        if scan {
            picker.refresh(cx);
        }
        picker
    }
    pub(super) fn seeded(
        current: AudioChoice,
        restore: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut picker = Self::create(current, restore, false, cx);
        picker.model.replace(vec![ProcessEntry {
            pid: 424242,
            created_at_hns: 77,
            exe_name: "Fixture game.exe".into(),
            window_title: "Synthetic game window 🎮".into(),
        }]);
        picker
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.closed {
            return;
        }
        self.loading = true;
        self.error = None;
        let cancelled = self.cancelled.clone();
        let worker = cx
            .background_executor()
            .spawn(async move { enumerate(&cancelled) });
        cx.spawn(async move |this, cx| {
            let result = worker.await;
            let _ = this.update(cx, |this, cx| {
                if this.closed {
                    return;
                }
                this.loading = false;
                match result {
                    Ok((entries, truncated)) => {
                        this.model.replace(entries);
                        this.truncated = truncated;
                    }
                    Err(error) => {
                        this.error = Some(format!("Could not list applications: {error:#}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn finish(&mut self, choice: Option<AudioChoice>, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        if let Some(AudioChoice::Process(selected)) = &choice
            && !self
                .model
                .entries
                .iter()
                .any(|p| p.pid == selected.pid && p.created_at_hns == selected.created_at_hns)
        {
            self.error =
                Some("Application is no longer listed. Refresh and select it again.".into());
            cx.notify();
            return;
        }
        self.closed = true;
        self.cancelled.store(true, Ordering::Release);
        self.restore_focus.focus(window);
        cx.emit(match choice {
            Some(choice) => PickerEvent::Selected(choice),
            None => PickerEvent::Closed,
        });
    }
}

impl Render for AudioSourcePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.request_focus {
            self.request_focus = false;
            self.search.read(cx).focus_handle(cx).focus(window);
        }
        let choices = self.model.choices();
        let no_matches =
            choices.len() == 1 && !self.model.query.is_empty() && !self.model.entries.is_empty();
        let row = choices
            .iter()
            .position(|p| p.same_instance(&self.model.highlight))
            .unwrap_or(0);
        let mut list = div()
            .id("audio-source-list")
            .w_full()
            .min_w_0()
            .track_focus(&self.list_focus)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .border_1()
            .border_color(rgb(LINE))
            .focus(|s| s.border_color(rgb(ACCENT)))
            .rounded_md()
            .flex()
            .flex_col();
        for (i, choice) in choices.into_iter().enumerate() {
            let label_width = self.row_label_width.clone();
            let (title, detail) = match &choice {
                AudioChoice::Desktop => {
                    ("Desktop audio".to_string(), "All applications".to_string())
                }
                AudioChoice::Process(p) => (
                    p.window_title.clone(),
                    format!("{} · PID {}", p.exe_name, p.pid),
                ),
            };
            let readable = self.row_text_readable.clone();
            let expected_title = title.clone();
            let expected_detail = detail.clone();
            let title_text = StyledText::new(title);
            let title_layout = title_text.layout().clone();
            let detail_text = StyledText::new(detail);
            let detail_layout = detail_text.layout().clone();
            list = list.child(
                div()
                    .when(i == 0, |row| {
                        row.on_children_prepainted(move |bounds, _, _| {
                            label_width.set(bounds.first().map_or(0., |bounds| f32::from(bounds.size.width)));
                            readable.set(title_layout.text() == expected_title && detail_layout.text() == expected_detail);
                        })
                    })
                    .id(("audio-source-row", i))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_2()
                    .flex_shrink_0()
                    .bg(rgb(if row == i { 0x173025 } else { PANEL }))
                    .cursor_pointer()
                    .child(div().w_full().min_w_0().truncate().child(title_text))
                    .child(div().text_color(rgb(MUTED)).w_full().min_w_0().truncate().child(detail_text))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.finish(Some(choice.clone()), window, cx)
                    })),
            );
        }
        let max_height = (window.viewport_size().height - px(32.))
            .max(px(180.))
            .min(px(460.));
        div().id("audio-source-picker-modal").absolute().inset_0().occlude()
            .flex().items_center().justify_end().px_6().bg(rgba(0x00000066))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                this.finish(None, window, cx); cx.stop_propagation();
            }))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let modified = event.keystroke.modifiers.control || event.keystroke.modifiers.platform || event.keystroke.modifiers.alt;
                if key == "escape" {
                    this.finish(None, window, cx);
                } else if key == "tab" && !modified {
                    let search = this.search.read(cx).focus_handle(cx);
                    let handles = [&search, &this.list_focus, &this.refresh_focus, &this.close_focus];
                    let current = handles.iter().position(|h| h.is_focused(window)).unwrap_or(0);
                    handles[(current + if event.keystroke.modifiers.shift { 3 } else { 1 }) % 4].focus(window);
                } else if !modified && matches!(key, "up" | "down" | "home" | "end")
                    && (this.list_focus.is_focused(window) || (matches!(key, "up" | "down") && this.search.read(cx).focus_handle(cx).is_focused(window)))
                {
                    this.model.navigate(key);
                    this.list_focus.focus(window);
                    let index = this.model.choices().iter().position(|p| p.same_instance(&this.model.highlight)).unwrap_or(0);
                    this.scroll.scroll_to_item(index);
                    cx.notify();
                } else if key == "enter" && !modified && (this.list_focus.is_focused(window) || this.search.read(cx).focus_handle(cx).is_focused(window)) {
                    match this.model.accept(this.search.read(cx).focus_handle(cx).is_focused(window)) {
                        Ok(choice) => this.finish(Some(choice), window, cx),
                        Err(message) => { this.error = Some(message.into()); cx.notify(); }
                    }
                } else { return; }
                window.prevent_default(); cx.stop_propagation();
            }))
            .child(div().w(px(360.)).max_w_full().h(max_height).p_4().rounded_lg().bg(rgb(PANEL))
                .border_1().border_color(rgb(LINE)).flex().flex_col().gap_3()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(div().text_size(px(18.)).child("Audio source"))
                .child(self.search.clone())
                .child(list)
                .when(self.loading, |s| s.child(muted("Finding running applications…")))
                .when(!self.loading && self.model.entries.is_empty() && self.error.is_none(), |s| s.child(muted("No accessible application windows. Open the app, then Refresh.")))
                .when(!self.loading && no_matches, |s| s.child(muted("No applications match your search.")))
                .when_some(self.error.clone(), |s, error| s.child(div().text_color(rgb(0xffb0b0)).child(error)))
                .when(self.truncated, |s| s.child(muted("Application list limited. Close unused windows and Refresh.")))
                .child(muted("Includes child processes. Select the running app, not its launcher. Reselect if the app or rcr restarts.").text_size(px(11.)))
                .child(div().flex().justify_end().gap_2()
                    .child(button("refresh-audio-sources", if self.loading { "Refreshing…" } else { "Refresh" })
                        .track_focus(&self.refresh_focus).opacity(if self.loading { 0.45 } else { 1. })
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))))
                    .child(button("close-audio-sources", "Cancel").track_focus(&self.close_focus)
                        .on_click(cx.listener(|this, _, window, cx| this.finish(None, window, cx))))))
    }
}

/// All metadata for a process comes from one retained handle. A PID reused
/// between enumeration and recording cannot silently attach a new application.
fn enumerate(cancelled: &AtomicBool) -> anyhow::Result<(Vec<ProcessEntry>, bool)> {
    use windows::{
        Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*},
        core::{BOOL, PWSTR},
    };
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    struct Scan<'a> {
        entries: Vec<ProcessEntry>,
        seen: BTreeSet<u32>,
        cancelled: &'a AtomicBool,
        visited: usize,
        truncated: bool,
    }
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        let scan = unsafe { &mut *(data.0 as *mut Scan<'_>) };
        if scan.cancelled.load(Ordering::Acquire) {
            return BOOL(0);
        }
        scan.visited += 1;
        if scan.seen.len() >= MAX_PROCESSES || scan.visited > 2048 {
            scan.truncated = true;
            return BOOL(0);
        }
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            return BOOL(1);
        }
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if pid == 0 || pid == std::process::id() || scan.seen.contains(&pid) {
            return BOOL(1);
        }
        let mut title = [0u16; 257];
        let n = unsafe { GetWindowTextW(hwnd, &mut title) };
        if n <= 0 {
            return BOOL(1);
        }
        scan.seen.insert(pid);
        let Ok(raw) = (unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
        }) else {
            return BOOL(1);
        };
        let handle = Handle(raw);
        let mut path = [0u16; 32768];
        let mut length = path.len() as u32;
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        if unsafe {
            QueryFullProcessImageNameW(
                handle.0,
                PROCESS_NAME_WIN32,
                PWSTR(path.as_mut_ptr()),
                &mut length,
            )
        }
        .is_err()
            || unsafe {
                GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user)
            }
            .is_err()
            || unsafe { WaitForSingleObject(handle.0, 0) } != WAIT_TIMEOUT
        {
            return BOOL(1);
        }
        let mut current_pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut current_pid));
        }
        if current_pid != pid {
            return BOOL(1);
        }
        let path = String::from_utf16_lossy(&path[..length as usize]);
        let exe_name = path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&path)
            .chars()
            .take(256)
            .collect();
        scan.entries.push(ProcessEntry {
            pid,
            created_at_hns: u64::from(created.dwLowDateTime)
                | (u64::from(created.dwHighDateTime) << 32),
            exe_name,
            window_title: String::from_utf16_lossy(&title[..n as usize])
                .trim()
                .to_string(),
        });
        BOOL(1)
    }
    let mut scan = Scan {
        entries: Vec::new(),
        seen: BTreeSet::new(),
        cancelled,
        visited: 0,
        truncated: false,
    };
    let result = unsafe { EnumWindows(Some(visit), LPARAM((&mut scan as *mut Scan<'_>) as isize)) };
    if !scan.truncated && !cancelled.load(Ordering::Acquire) {
        result?;
    }
    Ok((scan.entries, scan.truncated))
}

/// Owned-window fixture: synthetic candidates, real GPUI focus/key dispatch,
/// and production Studio subscriptions. Never enumerates or activates audio.
#[derive(Default)]
pub struct PickerSmoke {
    stage: usize,
    original: Option<(AudioChoice, usize, bool, bool, bool)>,
}
pub fn smoke(
    studio: Entity<crate::Studio>,
    state: &mut PickerSmoke,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<bool> {
    use anyhow::ensure;
    fn key(window: &mut Window, cx: &mut App, key: &str) {
        window.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke {
                    key: key.into(),
                    modifiers: Default::default(),
                    key_char: None,
                },
                is_held: false,
            }),
            cx,
        );
        window.draw(cx).clear();
    }
    fn type_text(window: &mut Window, cx: &mut App, text: &str) {
        for ch in text.chars() {
            window.dispatch_keystroke(
                Keystroke {
                    key: ch.to_string(),
                    key_char: Some(ch.to_string()),
                    modifiers: Default::default(),
                },
                cx,
            );
        }
        window.draw(cx).clear();
    }
    window.draw(cx).clear();
    match state.stage {
        0 => {
            ensure!(
                !studio.read(cx).active,
                "Audio picker fixture requires an inactive Studio"
            );
            state.original = Some((
                studio.read(cx).audio_choice.clone(),
                studio.read(cx).tab,
                studio.read(cx).system_audio,
                studio.read(cx).microphone,
                studio.read(cx).audio_picker_fixture,
            ));
            studio.update(cx, |this, cx| {
                this.tab = 0;
                this.audio_picker_fixture = true;
                cx.notify();
            });
            window.draw(cx).clear();
            studio.read(cx).audio_source_focus.focus(window);
            let keystroke = Keystroke {
                key: "enter".into(),
                modifiers: Default::default(),
                key_char: None,
            };
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                }),
                cx,
            );
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
            ensure!(
                studio.read(cx).audio_picker.is_some(),
                "Native source control did not open picker"
            );
            state.stage = 1;
        }
        1 => {
            let picker = studio.read(cx).audio_picker.clone().expect("Seeded picker");
            ensure!(
                picker.read(cx).row_label_width.get() >= 200.,
                "Application title has no usable native layout width: {}",
                picker.read(cx).row_label_width.get()
            );
            ensure!(picker.read(cx).row_text_readable.get(),
                "Application row's actual shaped title/detail collapsed during measurement");
            ensure!(
                picker
                    .read(cx)
                    .search
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "Picker did not focus search"
            );
            type_text(window, cx, "unmatchedzz");
            state.stage = 8;
        }
        8 if studio
            .read(cx)
            .audio_picker
            .as_ref()
            .is_some_and(|p| p.read(cx).model.query == "unmatchedzz") =>
        {
            key(window, cx, "enter");
            let picker = studio
                .read(cx)
                .audio_picker
                .clone()
                .expect("No-match Enter must keep picker open");
            ensure!(
                !picker.read(cx).closed
                    && picker.read(cx).error.as_deref()
                        == Some(
                            "No matching application. Clear search or select Desktop explicitly."
                        ),
                "No-match search Enter silently selected Desktop"
            );
            window.dispatch_keystroke(Keystroke::parse("ctrl-a").expect("Ctrl+A key"), cx);
            type_text(window, cx, "game");
            state.stage = 9;
        }
        9 if studio
            .read(cx)
            .audio_picker
            .as_ref()
            .is_some_and(|p| p.read(cx).model.query == "game") =>
        {
            let picker = studio
                .read(cx)
                .audio_picker
                .clone()
                .expect("Matching search picker");
            ensure!(
                matches!(picker.read(cx).model.highlight, AudioChoice::Process(_)),
                "Search did not highlight its matching application"
            );
            key(window, cx, "enter");
            state.stage = 2;
        }
        2 if studio.read(cx).audio_picker.is_none() => {
            ensure!(
                matches!(&studio.read(cx).audio_choice, AudioChoice::Process(p) if p.pid == 424242 && p.created_at_hns == 77),
                "Picker did not commit the selected process instance"
            );
            ensure!(
                studio.read(cx).audio_source_focus.is_focused(window),
                "Selection did not restore source control focus"
            );
            studio.update(cx, |this, cx| {
                let picker = cx.new(|cx| {
                    AudioSourcePicker::seeded(
                        this.audio_choice.clone(),
                        this.audio_source_focus.clone(),
                        cx,
                    )
                });
                this.attach_audio_picker(picker, cx);
            });
            state.stage = 3;
        }
        3 => {
            key(window, cx, "escape");
            state.stage = 4;
        }
        4 if studio.read(cx).audio_picker.is_none() => {
            ensure!(
                studio.read(cx).audio_source_focus.is_focused(window),
                "Escape did not restore source control focus"
            );
            studio.update(cx, |this, cx| {
                // Simulate a session starting while an existing selection event
                // is queued. No pipeline or capture worker is started here.
                this.active = true;
                this.open_audio_picker(cx);
                assert!(
                    this.audio_picker.is_none(),
                    "Active capture opened an audio picker"
                );
                let picker = cx.new(|cx| {
                    AudioSourcePicker::seeded(
                        AudioChoice::Desktop,
                        this.audio_source_focus.clone(),
                        cx,
                    )
                });
                this.attach_audio_picker(picker, cx);
            });
            state.stage = 5;
        }
        5 => {
            key(window, cx, "enter");
            state.stage = 6;
        }
        6 if studio.read(cx).audio_picker.is_none() => {
            ensure!(
                matches!(&studio.read(cx).audio_choice, AudioChoice::Process(p) if p.pid == 424242),
                "Late picker result changed an active capture source"
            );
            let (choice, tab, system, mic, fixture) =
                state.original.take().expect("Fixture original state");
            ensure!(
                studio.read(cx).system_audio == system && studio.read(cx).microphone == mic,
                "Source selection changed independent audio toggles"
            );
            studio.update(cx, |this, cx| {
                this.active = false;
                this.audio_choice = choice;
                this.audio_picker_fixture = fixture;
                this.tab = tab;
                cx.notify();
            });
            state.stage = 7;
            println!(
                "Audio picker smoke: native typed search/Enter selection, no-match Enter guard, process fingerprint, Escape focus, active opening/late-event guards, independent microphone; no audio activated"
            );
        }
        _ => {}
    }
    Ok(state.stage == 7)
}

#[cfg(test)]
mod tests {
    use super::{AudioChoice, ListModel, MAX_PROCESSES, ProcessEntry};
    fn entry(pid: u32, created: u64, name: &str) -> ProcessEntry {
        ProcessEntry {
            pid,
            created_at_hns: created,
            exe_name: name.into(),
            window_title: format!("Game {pid} 🎮"),
        }
    }
    #[test]
    fn refreshed_labels_keep_instance_and_pid_reuse_invalidates_it() {
        let mut model = ListModel::default();
        model.replace(vec![entry(7, 10, "game.exe")]);
        model.navigate("end");
        model.replace(vec![entry(7, 10, "renamed.exe")]);
        assert_eq!(
            model.highlight,
            AudioChoice::Process(entry(7, 10, "renamed.exe"))
        );
        model.replace(vec![entry(7, 11, "different.exe")]);
        assert_eq!(model.highlight, AudioChoice::Desktop);
    }
    #[test]
    fn unicode_search_dedup_limit_and_navigation_keep_desktop_available() {
        let mut model = ListModel::default();
        model.replace(vec![
            entry(1, 1, "遊戲.exe"),
            entry(1, 1, "duplicate.exe"),
            entry(2, 2, "music.exe"),
        ]);
        assert_eq!(model.entries.len(), 2);
        model.filter("🎮".into());
        assert_eq!(model.choices().len(), 3);
        model.filter("missing".into());
        model.navigate("end");
        assert_eq!(model.choices(), vec![AudioChoice::Desktop]);
        assert_eq!(model.highlight, AudioChoice::Desktop);
        model.filter(String::new());
        model.replace((1..=400).map(|i| entry(i, 1, "app.exe")).collect());
        assert_eq!(model.entries.len(), MAX_PROCESSES);
        model.navigate("end");
        model.navigate("down");
        assert!(matches!(model.highlight, AudioChoice::Process(_)));
        model.navigate("home");
        model.navigate("up");
        assert_eq!(model.highlight, AudioChoice::Desktop);
    }
    #[test]
    fn search_enter_prefers_matching_application_and_never_falls_back_to_desktop() {
        let mut model = ListModel::default();
        model.replace(vec![
            entry(7, 11, "game.exe"),
            entry(8, 12, "game-helper.exe"),
        ]);
        model.filter("game".into());
        assert!(matches!(model.accept(true), Ok(AudioChoice::Process(_))));
        model.navigate("end");
        let chosen = model.highlight.clone();
        model.filter("game".into());
        assert_eq!(model.accept(true), Ok(chosen));
        // Desktop remains explicitly selectable from the list, even with a
        // search present. Enter in the search field means accepting a match.
        model.navigate("home");
        assert_eq!(model.accept(false), Ok(AudioChoice::Desktop));
        assert!(matches!(model.accept(true), Ok(AudioChoice::Process(_))));
        model.filter("not-running".into());
        assert!(model.accept(true).is_err());
        assert_eq!(model.accept(false), Ok(AudioChoice::Desktop));
        model.filter(String::new());
        assert_eq!(model.accept(true), Ok(AudioChoice::Desktop));
    }
}
