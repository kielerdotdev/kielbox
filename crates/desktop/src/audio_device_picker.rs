//! Read-only audio endpoint selection; enumeration never activates a stream.
use crate::{preferences::SavedAudioEndpoint, text_input::TextInput, theme::*};
use gpui::{prelude::*, *};
pub use windows_capture::audio_endpoint::AudioEndpointKind;
use windows_capture::audio_endpoint::{
    AudioEndpoint, AudioEndpointList, enumerate_audio_endpoints_cancellable,
};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
struct ListModel {
    entries: Vec<AudioEndpoint>,
    saved: Option<SavedAudioEndpoint>,
    highlight: Option<String>,
    default_explicit: bool,
    query: String,
}
struct Row {
    id: Option<String>,
    title: String,
    detail: &'static str,
    available: bool,
}
fn clean_name(name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|ch| !ch.is_control())
        .take(256)
        .collect();
    if name.trim().is_empty() {
        "Audio device".into()
    } else {
        name
    }
}
impl ListModel {
    fn replace(&mut self, kind: AudioEndpointKind, entries: Vec<AudioEndpoint>) {
        let mut seen = BTreeSet::new();
        self.entries = entries
            .into_iter()
            .filter(|entry| {
                entry.kind == kind && !entry.id.is_empty() && seen.insert(entry.id.clone())
            })
            .take(128)
            .map(|mut entry| {
                entry.name = clean_name(&entry.name);
                entry
            })
            .collect();
        self.entries
            .sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.id.clone()));
        self.reconcile();
    }
    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row {
            id: None,
            title: "Windows default".into(),
            detail: "Use the default device when capture starts",
            available: true,
        }];
        if let Some(saved) = &self.saved
            && !self.entries.iter().any(|entry| entry.id == saved.id)
        {
            rows.push(Row {
                id: Some(saved.id.clone()),
                title: saved.display_label(),
                detail: "Saved device unavailable · reconnect or choose another",
                available: false,
            });
        }
        let mut totals = BTreeMap::<String, usize>::new();
        for entry in &self.entries {
            *totals.entry(entry.name.to_lowercase()).or_default() += 1;
        }
        let mut ordinal = BTreeMap::<String, usize>::new();
        for entry in &self.entries {
            let name = entry.name.to_lowercase();
            let number = ordinal.entry(name.clone()).or_default();
            *number += 1;
            let title = if totals[&name] > 1 {
                format!("{} · Device {}", entry.name, number)
            } else {
                entry.name.clone()
            };
            if !title.to_lowercase().contains(&self.query) {
                continue;
            }
            rows.push(Row {
                id: Some(entry.id.clone()),
                title,
                detail: if entry.is_default {
                    "Current Windows default"
                } else {
                    "Available device"
                },
                available: true,
            });
        }
        rows
    }
    fn reconcile(&mut self) {
        let rows = self.rows();
        if !self.query.is_empty() {
            self.highlight = rows
                .iter()
                .find(|row| row.available && row.id.is_some() && row.id == self.highlight)
                .or_else(|| rows.iter().find(|row| row.available && row.id.is_some()))
                .and_then(|row| row.id.clone());
        } else if !rows.iter().any(|row| row.id == self.highlight) {
            self.highlight = None;
        }
    }
    fn filter(&mut self, query: String) {
        self.query = query.trim().to_lowercase();
        self.default_explicit = false;
        if self.query.is_empty() && self.highlight.is_none() {
            self.highlight = self.saved.as_ref().map(|saved| saved.id.clone());
        }
        self.reconcile();
    }
    fn navigate(&mut self, key: &str) {
        let rows = self.rows();
        let current = rows
            .iter()
            .position(|row| row.id == self.highlight)
            .unwrap_or(0);
        let last = rows.len() - 1;
        let next = match key {
            "up" => current.saturating_sub(1),
            "down" => (current + 1).min(last),
            "home" => 0,
            "end" => last,
            _ => current,
        };
        self.highlight = rows[next].id.clone();
        self.default_explicit = self.highlight.is_none();
    }
    fn accept(&self, from_search: bool) -> Result<Option<SavedAudioEndpoint>, &'static str> {
        let selected = if from_search && !self.query.is_empty() {
            self.rows()
                .into_iter()
                .find(|row| row.available && row.id.is_some() && row.id == self.highlight)
                .and_then(|row| row.id)
                .ok_or("No matching device. Clear search or select Windows default explicitly.")?
        } else {
            let Some(id) = &self.highlight else {
                if !self.default_explicit && (!self.query.is_empty() || self.saved.is_some()) {
                    return Err("Choose a device or explicitly select Windows default.");
                }
                return Ok(None);
            };
            id.clone()
        };
        self.entries.iter().find(|entry| entry.id == selected)
            .map(|entry| Some(SavedAudioEndpoint { id: entry.id.clone(), label: entry.name.clone() }))
            .ok_or("The saved device is unavailable. Reconnect it and Refresh, or explicitly choose another device.")
    }
}

pub enum PickerEvent {
    Selected(Option<SavedAudioEndpoint>),
    Closed,
}
pub struct AudioDevicePicker {
    kind: AudioEndpointKind,
    model: ListModel,
    search: Entity<TextInput>,
    list_focus: FocusHandle,
    select_focus: FocusHandle,
    refresh_focus: FocusHandle,
    close_focus: FocusHandle,
    restore_focus: FocusHandle,
    scroll: ScrollHandle,
    row_label_width: Rc<Cell<f32>>,
    row_text_readable: Rc<Cell<bool>>,
    truncation_probe: Option<Rc<Cell<bool>>>,
    cancelled: Arc<AtomicBool>,
    loading: bool,
    closed: bool,
    truncated: bool,
    unavailable_entries: usize,
    error: Option<String>,
    request_focus: bool,
}
impl EventEmitter<PickerEvent> for AudioDevicePicker {}
impl Drop for AudioDevicePicker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
impl AudioDevicePicker {
    pub fn new(
        kind: AudioEndpointKind,
        current: Option<SavedAudioEndpoint>,
        restore_focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::create(kind, current, restore_focus, true, cx)
    }
    fn create(
        kind: AudioEndpointKind,
        current: Option<SavedAudioEndpoint>,
        restore_focus: FocusHandle,
        scan: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            let mut input = TextInput::new(cx, "");
            input.set_placeholder("Search audio devices…", cx);
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
            kind,
            model: ListModel {
                highlight: current.as_ref().map(|saved| saved.id.clone()),
                saved: current,
                ..Default::default()
            },
            search,
            list_focus: cx.focus_handle().tab_stop(true),
            select_focus: cx.focus_handle().tab_stop(true),
            refresh_focus: cx.focus_handle().tab_stop(true),
            close_focus: cx.focus_handle().tab_stop(true),
            restore_focus,
            scroll: ScrollHandle::new(),
            row_label_width: Rc::new(Cell::new(0.)),
            row_text_readable: Rc::new(Cell::new(false)),
            truncation_probe: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            loading: false,
            closed: false,
            truncated: false,
            unavailable_entries: 0,
            error: None,
            request_focus: true,
        };
        if scan {
            picker.refresh(cx);
        }
        picker
    }
    pub(super) fn seeded(
        kind: AudioEndpointKind,
        current: Option<SavedAudioEndpoint>,
        restore_focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut picker = Self::create(kind, current, restore_focus, false, cx);
        picker.truncation_probe = Some(Rc::new(Cell::new(false)));
        picker.model.replace(
            kind,
            vec![
                AudioEndpoint {
                    id: "fixture-device-a".into(),
                    name: "Fixture audio device".into(),
                    kind,
                    is_default: true,
                },
                AudioEndpoint {
                    id: "fixture-device-b".into(),
                    name: "Fixture audio device".into(),
                    kind,
                    is_default: false,
                },
            ],
        );
        picker
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.closed {
            return;
        }
        self.loading = true;
        self.error = None;
        self.model.entries.clear();
        let kind = self.kind;
        let cancelled = self.cancelled.clone();
        let worker = cx.background_executor().spawn(async move {
            enumerate_audio_endpoints_cancellable(kind, || cancelled.load(Ordering::Acquire))
        });
        cx.spawn(async move |this, cx| {
            let result = worker.await;
            let _ = this.update(cx, |this, cx| {
                if this.closed || this.cancelled.load(Ordering::Acquire) {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(AudioEndpointList {
                        endpoints,
                        truncated,
                        unavailable_entries,
                    }) => {
                        this.model.replace(this.kind, endpoints);
                        this.truncated = truncated;
                        this.unavailable_entries = unavailable_entries;
                    }
                    Err(error) => {
                        this.error = Some(
                            format!("Could not list audio devices: {error:#}")
                                .chars()
                                .take(2_000)
                                .collect(),
                        )
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn finish(
        &mut self,
        choice: Option<Option<SavedAudioEndpoint>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closed {
            return;
        }
        if let Some(Some(selected)) = &choice
            && (self.loading
                || !self
                    .model
                    .entries
                    .iter()
                    .any(|entry| entry.id == selected.id))
        {
            self.error =
                Some("This device is no longer available. Refresh and select it again.".into());
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
    fn select(&mut self, from_search: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.model.accept(from_search) {
            Ok(choice) => self.finish(Some(choice), window, cx),
            Err(message) => {
                self.error = Some(message.into());
                cx.notify();
            }
        }
    }
}

impl Render for AudioDevicePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.request_focus {
            self.request_focus = false;
            self.search.read(cx).focus_handle(cx).focus(window);
        }
        let rows = self.model.rows();
        let no_matches = !self.model.query.is_empty()
            && !rows.iter().any(|row| row.available && row.id.is_some());
        let selectable =
            self.model.accept(false).is_ok() && (!self.loading || self.model.highlight.is_none());
        let mut list = div()
            .id("audio-device-list")
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
        for (index, row) in rows.into_iter().enumerate() {
            let selected = row.id == self.model.highlight;
            let label_width = self.row_label_width.clone();
            let truncation_probe = self.truncation_probe.clone();
            let readable = self.row_text_readable.clone();
            let expected_title = row.title.clone();
            let detail = if self.loading && !row.available { "Checking saved device…" } else { row.detail };
            let title_text = StyledText::new(row.title);
            let title_layout = title_text.layout().clone();
            let detail_text = StyledText::new(detail);
            let detail_layout = detail_text.layout().clone();
            list = list.child(
                div()
                    .when(index == 0, |row| {
                        row.on_children_prepainted(move |bounds, window, cx| {
                            label_width.set(bounds.first().map_or(0., |bounds| f32::from(bounds.size.width)));
                            readable.set(title_layout.text() == expected_title && detail_layout.text() == detail);
                            if let Some(probe) = &truncation_probe && !probe.get() {
                                probe.set(truncation_remeasure_probe(window, cx));
                            }
                        })
                    })
                    .id(("audio-device-row", index))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_2()
                    .flex_shrink_0()
                    .bg(rgb(if selected { 0x173025 } else { PANEL }))
                    .when(!row.available, |s| s.opacity(0.65))
                    .cursor_pointer()
                    .child(div().w_full().min_w_0().truncate().child(title_text))
                    .child(
                        div().text_color(rgb(MUTED))
                        .w_full()
                        .min_w_0()
                        .truncate().child(detail_text),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.model.highlight = row.id.clone();
                        this.model.default_explicit = row.id.is_none();
                        this.list_focus.focus(window);
                        cx.notify();
                    })),
            );
        }
        let max_height = (window.viewport_size().height - px(32.))
            .max(px(180.))
            .min(px(500.));
        div().id("audio-device-picker-modal").absolute().inset_0().occlude().flex().items_center().justify_end().px_6().bg(rgba(0x00000066))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { this.finish(None, window, cx); cx.stop_propagation(); }))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let modified = event.keystroke.modifiers.control || event.keystroke.modifiers.platform || event.keystroke.modifiers.alt;
                let search = this.search.read(cx).focus_handle(cx);
                if key == "escape" { this.finish(None, window, cx); }
                else if key == "tab" && !modified {
                    let handles = [&search, &this.list_focus, &this.refresh_focus, &this.select_focus, &this.close_focus];
                    let current = handles.iter().position(|handle| handle.is_focused(window)).unwrap_or(0);
                    handles[(current + if event.keystroke.modifiers.shift { 4 } else { 1 }) % 5].focus(window);
                } else if !modified && matches!(key, "up" | "down" | "home" | "end")
                    && (this.list_focus.is_focused(window) || (matches!(key, "up" | "down") && search.is_focused(window)))
                {
                    this.model.navigate(key); this.list_focus.focus(window);
                    let row = this.model.rows().iter().position(|row| row.id == this.model.highlight).unwrap_or(0);
                    this.scroll.scroll_to_item(row); cx.notify();
                } else if key == "enter" && !modified && (this.list_focus.is_focused(window) || search.is_focused(window)) {
                    this.select(search.is_focused(window), window, cx);
                } else { return; }
                window.prevent_default(); cx.stop_propagation();
            }))
            .child(div().w(px(380.)).max_w_full().h(max_height).p_4().rounded_lg().bg(rgb(PANEL)).border_1().border_color(rgb(LINE))
                .flex().flex_col().gap_3().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(div().text_size(px(18.)).child(match self.kind { AudioEndpointKind::Render => "Desktop audio device", AudioEndpointKind::Capture => "Microphone device" }))
                .child(self.search.clone()).child(list)
                .when(self.loading, |s| s.child(muted("Finding audio devices…")))
                .when(!self.loading && no_matches, |s| s.child(muted("No devices match your search.")))
                .when(!self.loading && self.model.entries.is_empty() && self.error.is_none(), |s| s.child(muted("No available devices. Reconnect a device, then Refresh.")))
                .when(self.model.saved.as_ref().is_some_and(|saved| !self.model.entries.iter().any(|entry| entry.id == saved.id)) && !self.loading,
                    |s| s.child(muted("Your saved device remains selected until you explicitly choose another.")))
                .when_some(self.error.clone(), |s, error| s.child(div().id("audio-device-error").min_h_0().max_h(px(90.)).overflow_y_scroll()
                    .text_color(rgb(0xffb0b0)).child(div().min_w_0().whitespace_normal().child(error))))
                .when(self.truncated || self.unavailable_entries > 0, |s| s.child(muted(if self.truncated { "Device list limited. Reconnect your device and Refresh." } else { "Some devices could not be read. Refresh to try again." })))
                .child(muted(match self.kind { AudioEndpointKind::Render => "Chooses which output rcr records. It does not reroute applications or change Windows playback settings.",
                    AudioEndpointKind::Capture => "Chooses the microphone rcr records. It does not change the Windows default microphone." }).text_size(px(11.)))
                .child(div().flex().justify_end().gap_2()
                    .child(button("refresh-audio-devices", if self.loading { "Refreshing…" } else { "Refresh" }).track_focus(&self.refresh_focus)
                        .opacity(if self.loading { 0.45 } else { 1. }).on_click(cx.listener(|this, _, _, cx| this.refresh(cx))))
                    .child(button("select-audio-device", "Select").track_focus(&self.select_focus).opacity(if selectable { 1. } else { 0.45 })
                        .when(selectable, |s| s.on_click(cx.listener(|this, _, window, cx| this.select(false, window, cx)))))
                    .child(button("close-audio-devices", "Cancel").track_focus(&self.close_focus).on_click(cx.listener(|this, _, window, cx| this.finish(None, window, cx))))))
    }
}

/// Run inside prepaint so the same actual text layout is measured twice. The
/// two runs also catch truncation mutating its captured runs before remeasure.
fn truncation_remeasure_probe(window: &mut Window, cx: &mut App) -> bool {
    const TEXT: &str = "Audio endpoint label restored after narrow measurement";
    let style = window.text_style();
    let text = StyledText::new(TEXT).with_runs(vec![style.to_run(6), style.to_run(TEXT.len() - 6)]);
    let layout = text.layout().clone();
    let mut element = div().truncate().child(text).into_any_element();
    element.layout_as_root(size(AvailableSpace::Definite(px(1.)), AvailableSpace::MinContent), window, cx);
    let narrowed = layout.text() != TEXT && layout.text().contains('…');
    element.layout_as_root(size(AvailableSpace::Definite(px(600.)), AvailableSpace::MinContent), window, cx);
    narrowed && layout.text() == TEXT
}

/// Owned-window proof using Studio's production controls and subscriptions.
/// All candidates are seeded and preference persistence is disabled by Studio's
/// existing fixture flag. No endpoint enumeration or audio activation runs.
#[derive(Default)]
pub struct DevicePickerSmoke {
    stage: u8,
    original: Option<SmokeOriginal>,
    held_picker: Option<Entity<AudioDevicePicker>>,
}
struct SmokeOriginal {
    system: Option<SavedAudioEndpoint>,
    microphone: Option<SavedAudioEndpoint>,
    choice: crate::audio_source_picker::AudioChoice,
    tab: usize,
    system_enabled: bool,
    microphone_enabled: bool,
    fixture: bool,
    focus: Option<FocusHandle>,
}
pub fn smoke(
    studio: Entity<crate::Studio>,
    state: &mut DevicePickerSmoke,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<bool> {
    use anyhow::{Context, ensure};
    fn key(window: &mut Window, cx: &mut App, key: &str) {
        let keystroke = Keystroke::parse(key).expect("Fixture key");
        window.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
            }),
            cx,
        );
        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
        window.draw(cx).clear();
    }
    fn text(window: &mut Window, cx: &mut App, text: &str) {
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
                !studio.read(cx).active
                    && studio.read(cx).audio_picker.is_none()
                    && studio.read(cx).audio_device_picker.is_none(),
                "Device fixture requires inactive Studio without an audio modal"
            );
            let this = studio.read(cx);
            state.original = Some(SmokeOriginal {
                system: this.preferences.system_audio_device.clone(),
                microphone: this.preferences.microphone_device.clone(),
                choice: this.audio_choice.clone(),
                tab: this.tab,
                system_enabled: this.system_audio,
                microphone_enabled: this.microphone,
                fixture: this.audio_picker_fixture,
                focus: window.focused(cx),
            });
            studio.update(cx, |this, cx| {
                this.audio_picker_fixture = true;
                this.tab = 0;
                this.audio_choice = crate::audio_source_picker::AudioChoice::Desktop;
                this.preferences.system_audio_device = Some(SavedAudioEndpoint {
                    id: "fixture-missing".into(),
                    label: "Disconnected headset 🎧".into(),
                });
                cx.notify();
            });
            window.draw(cx).clear();
            studio.read(cx).system_device_focus.focus(window);
            key(window, cx, "enter");
            state.held_picker = studio.read(cx).audio_device_picker.clone();
            ensure!(
                state.held_picker.is_some(),
                "Native Capture device button did not open picker"
            );
            state.stage = 1;
        }
        1 => {
            let picker = state
                .held_picker
                .as_ref()
                .context("Missing device picker")?;
            ensure!(
                picker.read(cx).row_label_width.get() >= 200.,
                "Device title has no usable native layout width: {}",
                picker.read(cx).row_label_width.get()
            );
            ensure!(picker.read(cx).row_text_readable.get(),
                "Device row's actual shaped title/detail collapsed during measurement");
            ensure!(picker.read(cx).truncation_probe.as_ref().is_some_and(|probe| probe.get()),
                "Truncated text did not recover its full glyph text when remeasured wider");
            ensure!(
                picker.read(cx).kind == AudioEndpointKind::Render && !picker.read(cx).loading,
                "Capture button did not use seeded output endpoints"
            );
            ensure!(
                picker
                    .read(cx)
                    .search
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "Device search did not receive focus"
            );
            ensure!(
                picker
                    .read(cx)
                    .model
                    .rows()
                    .iter()
                    .any(|row| !row.available && row.title == "Disconnected headset 🎧"),
                "Unavailable saved endpoint disappeared"
            );
            ensure!(
                picker.read(cx).model.accept(false).is_err(),
                "Unavailable device enabled Select"
            );
            key(window, cx, "tab");
            ensure!(
                picker.read(cx).list_focus.is_focused(window),
                "Tab did not reach device list"
            );
            key(window, cx, "enter");
            ensure!(
                !picker.read(cx).is_closed() && picker.read(cx).error.is_some(),
                "Unavailable Enter silently replaced selection"
            );
            key(window, cx, "tab");
            ensure!(
                picker.read(cx).refresh_focus.is_focused(window),
                "Tab did not reach Refresh"
            );
            key(window, cx, "tab");
            ensure!(
                picker.read(cx).select_focus.is_focused(window),
                "Tab did not reach Select"
            );
            key(window, cx, "enter");
            ensure!(
                !picker.read(cx).is_closed(),
                "Disabled Select accepted unavailable endpoint"
            );
            key(window, cx, "tab");
            ensure!(
                picker.read(cx).close_focus.is_focused(window),
                "Tab did not reach Cancel"
            );
            key(window, cx, "tab");
            ensure!(
                picker
                    .read(cx)
                    .search
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "Tab escaped modal"
            );
            key(window, cx, "shift-tab");
            ensure!(
                picker.read(cx).close_focus.is_focused(window),
                "Shift+Tab escaped modal"
            );
            key(window, cx, "tab");
            text(window, cx, "unmatchedzz");
            state.stage = 2;
        }
        2 => {
            let picker = state
                .held_picker
                .as_ref()
                .context("Missing device picker")?;
            if picker.read(cx).model.query != "unmatchedzz" {
                return Ok(false);
            }
            key(window, cx, "enter");
            ensure!(
                !picker.read(cx).is_closed()
                    && picker.read(cx).error.is_some()
                    && picker.read(cx).model.accept(false).is_err(),
                "No-match search silently selected Windows default"
            );
            ensure!(
                studio
                    .read(cx)
                    .preferences
                    .system_audio_device
                    .as_ref()
                    .is_some_and(|saved| saved.id == "fixture-missing"),
                "Unavailable preference changed before explicit selection"
            );
            key(window, cx, "ctrl-a");
            text(window, cx, "fixture");
            state.stage = 3;
        }
        3 => {
            let picker = state
                .held_picker
                .as_ref()
                .context("Missing device picker")?;
            if picker.read(cx).model.query != "fixture" {
                return Ok(false);
            }
            let titles: Vec<_> = picker
                .read(cx)
                .model
                .rows()
                .into_iter()
                .filter(|row| row.available && row.id.is_some())
                .map(|row| row.title)
                .collect();
            ensure!(
                titles
                    == [
                        "Fixture audio device · Device 1",
                        "Fixture audio device · Device 2"
                    ],
                "Duplicate friendly names lack stable visible distinctions"
            );
            key(window, cx, "down");
            key(window, cx, "end");
            ensure!(
                picker.read(cx).list_focus.is_focused(window)
                    && picker.read(cx).model.highlight.as_deref() == Some("fixture-device-b"),
                "Native navigation did not select second exact endpoint ID"
            );
            key(window, cx, "tab");
            key(window, cx, "tab");
            ensure!(
                picker.read(cx).select_focus.is_focused(window),
                "Select is unreachable after list navigation"
            );
            key(window, cx, "enter");
            state.stage = 4;
        }
        4 => {
            if studio.read(cx).audio_device_picker.is_some() {
                return Ok(false);
            }
            ensure!(
                state
                    .held_picker
                    .as_ref()
                    .context("Missing picker")?
                    .read(cx)
                    .is_closed(),
                "Selected picker remained open"
            );
            ensure!(
                studio
                    .read(cx)
                    .preferences
                    .system_audio_device
                    .as_ref()
                    .is_some_and(|saved| saved.id == "fixture-device-b"),
                "Duplicate-name selection committed the wrong opaque ID"
            );
            ensure!(
                studio.read(cx).system_device_focus.is_focused(window),
                "Selection did not restore Capture device focus"
            );
            key(window, cx, "enter");
            state.held_picker = studio.read(cx).audio_device_picker.clone();
            ensure!(
                state.held_picker.is_some(),
                "Capture device could not reopen"
            );
            state.stage = 5;
        }
        5 => {
            key(window, cx, "tab");
            key(window, cx, "home");
            key(window, cx, "enter");
            state.stage = 6;
        }
        6 => {
            if studio.read(cx).audio_device_picker.is_some() {
                return Ok(false);
            }
            ensure!(
                studio.read(cx).preferences.system_audio_device.is_none(),
                "Explicit Windows default did not clear saved endpoint ID"
            );
            ensure!(
                studio.read(cx).system_device_focus.is_focused(window),
                "Default selection lost Capture focus"
            );
            studio.update(cx, |this, cx| {
                this.tab = 3;
                cx.notify();
            });
            window.draw(cx).clear();
            studio.read(cx).microphone_device_focus.focus(window);
            key(window, cx, "enter");
            state.held_picker = studio.read(cx).audio_device_picker.clone();
            ensure!(
                state.held_picker.is_some(),
                "Native Settings microphone device button did not open picker"
            );
            state.stage = 7;
        }
        7 => {
            ensure!(
                state
                    .held_picker
                    .as_ref()
                    .context("Missing input picker")?
                    .read(cx)
                    .kind
                    == AudioEndpointKind::Capture,
                "Settings microphone control opened output endpoints"
            );
            key(window, cx, "escape");
            state.stage = 8;
        }
        8 => {
            if studio.read(cx).audio_device_picker.is_some() {
                return Ok(false);
            }
            ensure!(
                state
                    .held_picker
                    .as_ref()
                    .context("Missing input picker")?
                    .read(cx)
                    .is_closed(),
                "Escape did not close device picker"
            );
            ensure!(
                studio.read(cx).microphone_device_focus.is_focused(window),
                "Escape did not restore Settings microphone focus"
            );
            ensure!(
                studio.read(cx).preferences.microphone_device
                    == state
                        .original
                        .as_ref()
                        .context("Missing original state")?
                        .microphone,
                "Cancelled picker changed independent microphone selection"
            );
            key(window, cx, "enter");
            state.held_picker = studio.read(cx).audio_device_picker.clone();
            ensure!(
                state.held_picker.is_some(),
                "Settings microphone picker did not reopen"
            );
            studio.update(cx, |this, cx| {
                this.active = true;
                cx.notify();
            });
            state.stage = 9;
        }
        9 => {
            key(window, cx, "tab");
            key(window, cx, "end");
            key(window, cx, "enter");
            state.stage = 10;
        }
        10 => {
            if studio.read(cx).audio_device_picker.is_some() {
                return Ok(false);
            }
            ensure!(
                studio.read(cx).preferences.microphone_device
                    == state
                        .original
                        .as_ref()
                        .context("Missing original state")?
                        .microphone,
                "Late selection changed an active capture's microphone"
            );
            studio.update(cx, |this, cx| {
                this.open_audio_device_picker(AudioEndpointKind::Capture, cx);
                ensure!(
                    this.audio_device_picker.is_none(),
                    "Active capture opened a device picker"
                );
                this.active = false;
                cx.notify();
                Ok::<_, anyhow::Error>(())
            })?;
            window.draw(cx).clear();
            studio.read(cx).system_device_focus.focus(window);
            key(window, cx, "enter");
            let current = studio
                .read(cx)
                .audio_device_picker
                .clone()
                .context("New Settings output picker did not open")?;
            // The old entity is deliberately retained: its queued event must
            // not close or overwrite the newly opened picker/selection.
            state
                .held_picker
                .as_ref()
                .context("Missing old picker")?
                .update(cx, |_, cx| {
                    cx.emit(PickerEvent::Selected(Some(SavedAudioEndpoint {
                        id: "stale-event".into(),
                        label: "Stale".into(),
                    })));
                });
            state.held_picker = Some(current);
            state.stage = 11;
        }
        11 => {
            let current = state
                .held_picker
                .as_ref()
                .context("Missing current picker")?;
            ensure!(
                studio
                    .read(cx)
                    .audio_device_picker
                    .as_ref()
                    .is_some_and(|picker| picker.entity_id() == current.entity_id())
                    && studio.read(cx).preferences.system_audio_device.is_none(),
                "Old picker event targeted the new modal"
            );
            key(window, cx, "escape");
            state.stage = 12;
        }
        12 => {
            if studio.read(cx).audio_device_picker.is_some() {
                return Ok(false);
            }
            let original = state
                .original
                .take()
                .context("Missing fixture original state")?;
            ensure!(
                studio.read(cx).system_audio == original.system_enabled
                    && studio.read(cx).microphone == original.microphone_enabled,
                "Device selection changed independent audio enable toggles"
            );
            studio.update(cx, |this, cx| {
                this.active = false;
                this.preferences.system_audio_device = original.system;
                this.preferences.microphone_device = original.microphone;
                this.audio_choice = original.choice;
                this.tab = original.tab;
                this.audio_picker_fixture = original.fixture;
                cx.notify();
            });
            state.held_picker = None;
            window.draw(cx).clear();
            if let Some(focus) = original.focus {
                focus.focus(window);
            }
            state.stage = 13;
            println!(
                "Audio device picker smoke: native Capture/Settings controls, unavailable/no-match guards, modal Tab/ShiftTab, duplicate-name exact ID, explicit Default, Escape focus, active/old-event guards; no enumeration, audio activation, or preference writes"
            );
        }
        _ => {}
    }
    Ok(state.stage == 13)
}

#[cfg(test)]
mod tests {
    use super::{AudioEndpoint, AudioEndpointKind, ListModel, SavedAudioEndpoint};
    fn endpoint(id: &str, name: &str) -> AudioEndpoint {
        AudioEndpoint {
            id: id.into(),
            name: name.into(),
            kind: AudioEndpointKind::Render,
            is_default: false,
        }
    }
    #[test]
    fn missing_saved_device_is_preserved_and_requires_explicit_replacement() {
        let saved = SavedAudioEndpoint {
            id: "disconnected".into(),
            label: "Headset 🎧".into(),
        };
        let mut model = ListModel {
            saved: Some(saved.clone()),
            highlight: Some(saved.id.clone()),
            ..Default::default()
        };
        model.replace(
            AudioEndpointKind::Render,
            vec![endpoint("other", "Headset 🎧")],
        );
        assert!(
            model.accept(false).is_err(),
            "Same friendly name replaced an unavailable endpoint"
        );
        assert!(
            model
                .rows()
                .iter()
                .any(|row| row.title == "Headset 🎧" && !row.available)
        );
        model.navigate("home");
        assert_eq!(model.accept(false).unwrap(), None);
        model.highlight = Some(saved.id.clone());
        model.replace(
            AudioEndpointKind::Render,
            vec![endpoint("disconnected", "Headset 🎧")],
        );
        assert_eq!(model.accept(false).unwrap(), Some(saved));
    }
    #[test]
    fn duplicate_names_are_distinct_deterministic_and_select_by_identity() {
        let mut model = ListModel::default();
        model.replace(
            AudioEndpointKind::Render,
            vec![
                endpoint("z", "Speakers"),
                endpoint("a", "Speakers"),
                endpoint("a", "Duplicate ID"),
            ],
        );
        let names: Vec<_> = model
            .rows()
            .into_iter()
            .skip(1)
            .map(|row| row.title)
            .collect();
        assert_eq!(names, vec!["Speakers · Device 1", "Speakers · Device 2"]);
        model.navigate("end");
        assert_eq!(model.accept(false).unwrap().unwrap().id, "z");
        model.replace(
            AudioEndpointKind::Render,
            vec![endpoint("a", "Speakers"), endpoint("z", "Speakers")],
        );
        assert_eq!(model.accept(false).unwrap().unwrap().id, "z");
    }
    #[test]
    fn search_enter_never_silently_selects_default_and_wrong_kind_is_excluded() {
        let mut model = ListModel::default();
        let mut microphone = endpoint("mic", "Microphone");
        microphone.kind = AudioEndpointKind::Capture;
        model.replace(
            AudioEndpointKind::Render,
            vec![endpoint("headset", "Headset"), microphone],
        );
        model.filter("head".into());
        assert_eq!(model.accept(true).unwrap().unwrap().id, "headset");
        model.filter("missing".into());
        assert!(model.accept(true).is_err());
        assert!(
            model.accept(false).is_err(),
            "No-match search enabled implicit Default selection"
        );
        model.navigate("home");
        assert_eq!(model.accept(false).unwrap(), None);
        model.filter("microphone".into());
        assert!(model.accept(true).is_err());
    }
}
