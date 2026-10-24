//! A bounded recovery browser. Scans run only on opening or explicit Refresh;
//! restore workers revalidate journal membership and never overwrite originals.
use crate::{
    theme::*,
    trash_recovery::{self, RecoveryCatalog, RestoreOutcome, TrashEntry},
};
use gpui::{prelude::*, *};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const PAGE_SIZE: usize = 32;
pub enum RecoveryEvent {
    Closed,
    Restoring(bool),
    Restored(TrashEntry, RestoreOutcome),
}

/// Shared by every recovery panel opened in one Studio. A blocked OS call
/// keeps its admission until it actually returns, even after its view closes.
#[derive(Clone, Default)]
pub struct RecoveryAdmission {
    scan: Arc<AtomicBool>,
    folder: Arc<AtomicBool>,
}
struct AdmissionLease(Arc<AtomicBool>);
impl AdmissionLease {
    fn acquire(slot: &Arc<AtomicBool>) -> Option<Self> {
        slot.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(slot.clone()))
    }
}
impl Drop for AdmissionLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub struct TrashRecoveryPanel {
    root: PathBuf,
    catalog: RecoveryCatalog,
    selected: usize,
    page: usize,
    loading: bool,
    restoring: bool,
    closed: bool,
    cancel: Arc<AtomicBool>,
    error: Option<String>,
    notice: Option<String>,
    list_focus: FocusHandle,
    restore_focus: FocusHandle,
    refresh_focus: FocusHandle,
    folder_focus: FocusHandle,
    close_focus: FocusHandle,
    prev_focus: FocusHandle,
    next_focus: FocusHandle,
    return_focus: FocusHandle,
    request_focus: bool,
    scroll: ScrollHandle,
    fixture: bool,
    admission: RecoveryAdmission,
    opening_folder: bool,
    warnings_visible: bool,
    warnings_focus: FocusHandle,
    warnings_list_focus: FocusHandle,
    warning_scroll: ScrollHandle,
    warning_cursor: usize,
}
impl EventEmitter<RecoveryEvent> for TrashRecoveryPanel {}
impl Drop for TrashRecoveryPanel {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl TrashRecoveryPanel {
    pub fn new(
        root: PathBuf,
        return_focus: FocusHandle,
        admission: RecoveryAdmission,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut panel = Self::empty(root, return_focus, admission, cx);
        panel.refresh(cx);
        panel
    }
    fn empty(
        root: PathBuf,
        return_focus: FocusHandle,
        admission: RecoveryAdmission,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            root,
            catalog: RecoveryCatalog {
                entries: vec![],
                warnings: vec![],
                truncated: false,
            },
            selected: 0,
            page: 0,
            loading: false,
            restoring: false,
            closed: false,
            cancel: Arc::new(AtomicBool::new(false)),
            error: None,
            notice: None,
            list_focus: cx.focus_handle().tab_stop(true),
            restore_focus: cx.focus_handle().tab_stop(true),
            refresh_focus: cx.focus_handle().tab_stop(true),
            folder_focus: cx.focus_handle().tab_stop(true),
            close_focus: cx.focus_handle().tab_stop(true),
            prev_focus: cx.focus_handle().tab_stop(true),
            next_focus: cx.focus_handle().tab_stop(true),
            return_focus,
            request_focus: true,
            scroll: ScrollHandle::new(),
            fixture: false,
            admission,
            opening_folder: false,
            warnings_visible: false,
            warnings_focus: cx.focus_handle().tab_stop(true),
            warnings_list_focus: cx.focus_handle().tab_stop(true),
            warning_scroll: ScrollHandle::new(),
            warning_cursor: 0,
        }
    }
    pub fn ready(&self) -> bool {
        !self.loading && !self.restoring && !self.closed
    }
    pub fn entries(&self) -> &[TrashEntry] {
        &self.catalog.entries
    }
    pub fn list_focused(&self, window: &Window) -> bool {
        self.list_focus.is_focused(window)
    }
    pub fn owns_focus(&self, window: &Window) -> bool {
        [
            &self.list_focus,
            &self.restore_focus,
            &self.refresh_focus,
            &self.folder_focus,
            &self.close_focus,
            &self.prev_focus,
            &self.next_focus,
            &self.warnings_focus,
            &self.warnings_list_focus,
        ]
        .iter()
        .any(|focus| focus.is_focused(window))
    }
    pub fn snapshot(return_focus: FocusHandle, cx: &mut Context<Self>) -> Self {
        let root = PathBuf::from(r"C:\Captures");
        let mut panel = Self::empty(root.clone(), return_focus, RecoveryAdmission::default(), cx);
        panel.fixture = true;
        panel.catalog.warnings.push("Older recovery records in .trash\\legacy-session have no recognized journal. Files were kept; use Open folder to review them.".into());
        for (name, size, conflict) in [
            (r"recordings\Match highlight.mkv", 87_654_321, false),
            (r"screenshots\Settings before update.png", 456_789, true),
            (
                r"projects\遠征 — annotated screenshot.ssxe",
                9_765_432,
                false,
            ),
        ] {
            panel.catalog.entries.push(TrashEntry {
                original: root.join(name),
                trashed: root
                    .join(".trash")
                    .join("synthetic")
                    .join(format!("{}.bin", panel.catalog.entries.len())),
                batch: root.join(".trash").join("synthetic"),
                bytes: size,
                removed_at: None,
                conflict,
            });
        }
        panel
    }
    pub fn expand_warnings(&mut self, cx: &mut Context<Self>) {
        self.warnings_visible = true;
        cx.notify();
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.restoring || self.closed {
            return;
        }
        if self.fixture {
            self.notice = Some("Synthetic recovery preview.".into());
            cx.notify();
            return;
        }
        let Some(lease) = AdmissionLease::acquire(&self.admission.scan) else {
            self.error = Some(
                "A previous Trash scan is still reading storage. Refresh when it finishes.".into(),
            );
            cx.notify();
            return;
        };
        self.loading = true;
        self.error = None;
        let root = self.root.clone();
        let cancel = self.cancel.clone();
        let selected = self
            .catalog
            .entries
            .get(self.selected)
            .map(|entry| entry.trashed.clone());
        let worker = cx.background_executor().spawn(async move {
            let _lease = lease;
            trash_recovery::scan(&root, || cancel.load(Ordering::Acquire))
        });
        cx.spawn(async move |panel, cx| {
            let result = worker.await;
            let _ = panel.update(cx, |this, cx| {
                if this.closed {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(catalog) => {
                        this.catalog = catalog;
                        this.selected = selected
                            .and_then(|path| {
                                this.catalog
                                    .entries
                                    .iter()
                                    .position(|entry| entry.trashed == path)
                            })
                            .unwrap_or(0);
                        this.page = this.selected / PAGE_SIZE;
                    }
                    Err(error) => {
                        this.error = Some(format!("Could not read capture Trash: {error:#}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn can_restore(&self) -> bool {
        !self.loading
            && !self.restoring
            && self
                .catalog
                .entries
                .get(self.selected)
                .is_some_and(|entry| !entry.conflict)
    }
    fn restore_selected(&mut self, cx: &mut Context<Self>) {
        if !self.can_restore() || self.closed {
            return;
        }
        if self.fixture {
            self.notice = Some("Synthetic preview: original files stay untouched.".into());
            cx.notify();
            return;
        }
        let entry = self.catalog.entries[self.selected].clone();
        let root = self.root.clone();
        let cancel = self.cancel.clone();
        self.restoring = true;
        cx.emit(RecoveryEvent::Restoring(true));
        self.error = None;
        self.notice = Some("Restoring to the original location…".into());
        let worker = cx.background_executor().spawn(async move {
            let outcome =
                trash_recovery::restore_entry(&root, &entry, || cancel.load(Ordering::Acquire));
            (entry, outcome)
        });
        // Retain only this view until the admitted restore resolves. Closing the
        // modal still cancels promptly, but a move that already finished must
        // notify Studio even after the panel is no longer painted.
        let panel = cx.entity();
        cx.spawn(async move |_, cx| {
            let (entry, result) = worker.await;
            let _ = panel.update(cx, |this, cx| {
                this.restoring = false;
                cx.emit(RecoveryEvent::Restoring(false));
                match result {
                    Ok(outcome) => {
                        this.notice = Some(match &outcome {
                            RestoreOutcome::Restored => "Restored to the original location.".into(),
                            RestoreOutcome::NoLongerInTrash => {
                                "This file is no longer in Trash. The list was updated.".into()
                            }
                        });
                        this.catalog
                            .entries
                            .retain(|current| current.trashed != entry.trashed);
                        this.selected = this
                            .selected
                            .min(this.catalog.entries.len().saturating_sub(1));
                        this.page = this.selected / PAGE_SIZE;
                        cx.emit(RecoveryEvent::Restored(entry, outcome));
                    }
                    Err(error) if !this.closed => {
                        this.error = Some(format!("Could not restore the selected file: {error:#}"))
                    }
                    Err(_) => {}
                }
                let retained = cx.entity();
                cx.defer(move |_| drop(retained));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.cancel.store(true, Ordering::Release);
        self.return_focus.focus(window);
        cx.emit(RecoveryEvent::Closed);
    }
    fn open_folder(&mut self, cx: &mut Context<Self>) {
        if self.opening_folder || self.closed {
            return;
        }
        if self.fixture {
            self.notice = Some("Synthetic preview: no folder is opened.".into());
            cx.notify();
            return;
        }
        let Some(lease) = AdmissionLease::acquire(&self.admission.folder) else {
            self.error =
                Some("Opening a Trash folder is still pending. Try again when it finishes.".into());
            cx.notify();
            return;
        };
        self.opening_folder = true;
        let folder = self.root.join(".trash");
        let worker = cx.background_executor().spawn(async move {
            let _lease = lease;
            std::fs::create_dir_all(&folder)?;
            std::process::Command::new("explorer.exe")
                .arg(folder.canonicalize()?)
                .spawn()?;
            Ok::<_, anyhow::Error>(())
        });
        cx.spawn(async move |panel, cx| {
            let result = worker.await;
            let _ = panel.update(cx, |this, cx| {
                this.opening_folder = false;
                if let Err(error) = result {
                    this.error = Some(format!("Could not open Trash folder: {error:#}"));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn navigate(&mut self, key: &str) {
        let last = self.catalog.entries.len().saturating_sub(1);
        self.selected = match key {
            "up" => self.selected.saturating_sub(1),
            "down" => (self.selected + 1).min(last),
            "home" => 0,
            "end" => last,
            "pageup" => self.selected.saturating_sub(PAGE_SIZE),
            "pagedown" => (self.selected + PAGE_SIZE).min(last),
            _ => self.selected,
        };
        self.page = self.selected / PAGE_SIZE;
        self.scroll.scroll_to_item(self.selected % PAGE_SIZE);
    }
    pub fn set_reference_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(message);
        cx.notify();
    }
}

fn bytes(value: u64) -> String {
    if value >= 1_048_576 {
        format!("{:.1} MB", value as f64 / 1_048_576.)
    } else if value >= 1024 {
        format!("{:.0} KB", value as f64 / 1024.)
    } else {
        format!("{value} B")
    }
}
struct PathHint(String);
impl Render for PathHint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(680.))
            .p_2()
            .rounded_md()
            .bg(rgb(SOFT))
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .child(self.0.clone())
    }
}
impl Render for TrashRecoveryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.request_focus {
            self.request_focus = false;
            self.list_focus.focus(window);
        }
        let count = self.catalog.entries.len();
        let start = self.page * PAGE_SIZE;
        let end = (start + PAGE_SIZE).min(count);
        let previous = self.page > 0;
        let next = end < count;
        let can_restore = self.can_restore();
        let available_height = (window.viewport_size().height - px(32.))
            .max(px(200.))
            .min(px(580.));
        let mut list = div()
            .id("trash-recovery-list")
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
        for (i, entry) in self
            .catalog
            .entries
            .iter()
            .enumerate()
            .skip(start)
            .take(PAGE_SIZE)
        {
            let title = entry
                .original
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let path = entry.original.display().to_string();
            let full_path = path.clone();
            list = list.child(
                div()
                    .id(("trash-recovery-row", i))
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .tooltip(move |_, cx| cx.new(|_| PathHint(full_path.clone())).into())
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .bg(rgb(if self.selected == i { 0x173025 } else { PANEL }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(title))
                            .child(muted(path).truncate()),
                    )
                    .child(muted(bytes(entry.bytes)).w(px(70.)).flex_shrink_0())
                    .child(
                        div()
                            .w(px(130.))
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .text_color(rgb(if entry.conflict { 0xffb0b0 } else { MUTED }))
                            .child(if entry.conflict {
                                "Original path occupied"
                            } else {
                                "Ready to restore"
                            }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.selected = i;
                        this.list_focus.focus(window);
                        cx.notify();
                    })),
            );
        }
        if count == 0 {
            list = list.child(div().flex_1().p_6().flex().flex_col().items_center().justify_center().gap_2()
                .child(if self.loading { "Reading capture Trash…" } else { "No recoverable captures" })
                .child(muted("Files with recovery records appear here. Open folder to inspect older files.")));
        }
        let root_label = self.root.display().to_string();
        let mut warning_rows = div()
            .id("trash-recovery-warning-details")
            .track_focus(&self.warnings_list_focus)
            .min_h_0()
            .max_h(px(120.))
            .overflow_y_scroll()
            .track_scroll(&self.warning_scroll)
            .border_1()
            .border_color(rgb(LINE))
            .focus(|s| s.border_color(rgb(ACCENT)))
            .rounded_md()
            .flex()
            .flex_col();
        for warning in self.catalog.warnings.iter().take(64) {
            warning_rows = warning_rows.child(
                div()
                    .flex_shrink_0()
                    .p_2()
                    .child(muted(warning.clone()).text_size(px(11.))),
            );
        }
        div()
            .id("trash-recovery-modal")
            .absolute()
            .inset_0()
            .occlude()
            .bg(rgba(0x000000a8))
            .flex()
            .items_center()
            .justify_center()
            .px_4()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let modified = event.keystroke.modifiers.control
                    || event.keystroke.modifiers.platform
                    || event.keystroke.modifiers.alt;
                if key == "escape" {
                    this.close(window, cx);
                } else if key == "tab" && !modified {
                    let mut handles = vec![&this.list_focus];
                    if this.can_restore() {
                        handles.push(&this.restore_focus);
                    }
                    if !this.loading && !this.restoring {
                        handles.push(&this.refresh_focus);
                    }
                    if !this.opening_folder {
                        handles.push(&this.folder_focus);
                    }
                    if !this.catalog.warnings.is_empty() {
                        handles.push(&this.warnings_focus);
                    }
                    if this.warnings_visible && !this.catalog.warnings.is_empty() {
                        handles.push(&this.warnings_list_focus);
                    }
                    handles.push(&this.close_focus);
                    if this.page > 0 {
                        handles.push(&this.prev_focus);
                    }
                    if (this.page + 1) * PAGE_SIZE < this.catalog.entries.len() {
                        handles.push(&this.next_focus);
                    }
                    let current = handles
                        .iter()
                        .position(|h| h.is_focused(window))
                        .unwrap_or(0);
                    let offset = if event.keystroke.modifiers.shift {
                        handles.len() - 1
                    } else {
                        1
                    };
                    handles[(current + offset) % handles.len()].focus(window);
                } else if !modified
                    && this.warnings_list_focus.is_focused(window)
                    && matches!(key, "up" | "down" | "home" | "end" | "pageup" | "pagedown")
                {
                    let last = this.catalog.warnings.len().min(64).saturating_sub(1);
                    this.warning_cursor = match key {
                        "up" => this.warning_cursor.saturating_sub(1),
                        "down" => (this.warning_cursor + 1).min(last),
                        "home" => 0,
                        "end" => last,
                        "pageup" => this.warning_cursor.saturating_sub(3),
                        _ => (this.warning_cursor + 3).min(last),
                    };
                    this.warning_scroll.scroll_to_item(this.warning_cursor);
                    cx.notify();
                } else if !modified
                    && this.list_focus.is_focused(window)
                    && matches!(key, "up" | "down" | "home" | "end" | "pageup" | "pagedown")
                {
                    this.navigate(key);
                    cx.notify();
                } else if key == "enter" && !modified && this.list_focus.is_focused(window) {
                    this.restore_selected(cx);
                } else {
                    return;
                }
                window.prevent_default();
                cx.stop_propagation();
            }))
            .on_key_down(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .w(px(760.))
                    .max_w_full()
                    .h(available_height)
                    .p_5()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(LINE))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().text_size(px(20.)).child("Capture Trash"))
                            .child(div().flex_1())
                            .child(
                                button("close-trash-recovery", "Close · Esc")
                                    .track_focus(&self.close_focus)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.close(window, cx)),
                                    ),
                            ),
                    )
                    .child(muted("Files return to their original locations."))
                    .child(muted(root_label).truncate())
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .px_3()
                            .child(muted("Original file").flex_1())
                            .child(muted("Size").w(px(70.)))
                            .child(muted("Status").w(px(130.))),
                    )
                    .child(list)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(muted(if count == 0 {
                                "0 files".into()
                            } else {
                                format!("{}–{end} of {count}", start + 1)
                            }))
                            .child(div().flex_1())
                            .child(
                                button("trash-previous-page", "Previous")
                                    .track_focus(&self.prev_focus)
                                    .tab_stop(previous)
                                    .opacity(if previous { 1. } else { 0.35 })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.page > 0 {
                                            this.page -= 1;
                                            this.selected = this.page * PAGE_SIZE;
                                            this.scroll.scroll_to_item(0);
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                button("trash-next-page", "Next")
                                    .track_focus(&self.next_focus)
                                    .tab_stop(next)
                                    .opacity(if next { 1. } else { 0.35 })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if (this.page + 1) * PAGE_SIZE < this.catalog.entries.len()
                                        {
                                            this.page += 1;
                                            this.selected = this.page * PAGE_SIZE;
                                            this.scroll.scroll_to_item(0);
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                    .when(self.loading && count > 0, |s| {
                        s.child(muted("Refreshing recovery records…"))
                    })
                    .when(!self.catalog.warnings.is_empty(), |s| {
                        s.child(
                            button(
                                "trash-recovery-show-warnings",
                                format!(
                                    "{} warnings ({})",
                                    if self.warnings_visible {
                                        "Hide"
                                    } else {
                                        "Show"
                                    },
                                    self.catalog.warnings.len()
                                ),
                            )
                            .track_focus(&self.warnings_focus)
                            .justify_start()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.warnings_visible = !this.warnings_visible;
                                cx.notify();
                            })),
                        )
                    })
                    .when(
                        self.warnings_visible && !self.catalog.warnings.is_empty(),
                        |s| s.child(warning_rows),
                    )
                    .when_some(self.notice.clone(), |s, notice| s.child(muted(notice)))
                    .when_some(self.error.clone(), |s, error| {
                        s.child(
                            div()
                                .id("trash-recovery-error")
                                .min_h_0()
                                .max_h(px(80.))
                                .overflow_y_scroll()
                                .text_color(rgb(0xffb0b0))
                                .child(error),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                button("open-trash-recovery-folder", "Open folder")
                                    .track_focus(&self.folder_focus)
                                    .tab_stop(!self.opening_folder)
                                    .opacity(if self.opening_folder { 0.35 } else { 1. })
                                    .on_click(cx.listener(|this, _, _, cx| this.open_folder(cx))),
                            )
                            .child(
                                button("refresh-trash-recovery", "Refresh")
                                    .track_focus(&self.refresh_focus)
                                    .tab_stop(!self.loading && !self.restoring)
                                    .opacity(if self.loading || self.restoring {
                                        0.35
                                    } else {
                                        1.
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                            )
                            .child(
                                button(
                                    "restore-trash-recovery",
                                    if self.restoring {
                                        "Restoring…"
                                    } else {
                                        "Restore selected"
                                    },
                                )
                                .track_focus(&self.restore_focus)
                                .tab_stop(can_restore)
                                .opacity(if can_restore { 1. } else { 0.35 })
                                .bg(rgb(0x173025))
                                .on_click(cx.listener(|this, _, _, cx| this.restore_selected(cx))),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn recovery_admission_survives_closed_panels_and_releases_on_unwind() {
        let first = RecoveryAdmission::default();
        let reopened = first.clone();
        let lease = AdmissionLease::acquire(&first.scan).unwrap();
        drop(first);
        assert!(AdmissionLease::acquire(&reopened.scan).is_none());
        drop(lease);
        assert!(AdmissionLease::acquire(&reopened.scan).is_some());
        let slot = reopened.folder.clone();
        let _ = std::panic::catch_unwind(move || {
            let _lease = AdmissionLease::acquire(&slot).unwrap();
            panic!("fixture worker unwind");
        });
        assert!(AdmissionLease::acquire(&reopened.folder).is_some());
    }
}
