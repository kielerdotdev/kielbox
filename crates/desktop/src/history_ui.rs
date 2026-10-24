//! Full-database history search with bounded, replaceable worker requests.
use super::*;
use rcr_toolkit::worker::{HISTORY_PAGE_SIZE, HistoryPage, HistorySearch};
use std::{cell::RefCell, collections::{BTreeMap, BTreeSet}, rc::Rc, sync::Arc};

type Probe = Rc<RefCell<BTreeMap<usize, Bounds<Pixels>>>>;
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RowAction { CopyUrl, ShowFile, Edit }
#[derive(Default)]
pub(super) struct HistoryUi {
    pub(super) started: bool,
    pub(super) page_index: usize,
    dirty: bool,
    text: String,
    requested: Option<u64>,
    page: Option<HistoryPage>,
    loading: bool,
    error: Option<String>,
    debounce: Option<Task<()>>,
    fixture: Option<HistorySearch>,
    probe: Option<Probe>,
    previous_focus: Option<FocusHandle>,
    next_focus: Option<FocusHandle>,
    refresh_focus: Option<FocusHandle>,
    scroll: ScrollHandle,
    row_focus: BTreeMap<(u64, i64, RowAction), FocusHandle>,
    last_revealed_focus: Option<(u64, i64, RowAction)>,
}
fn normalized(text: &str) -> String { text.trim().chars().take(512).collect() }
impl HistoryUi {
    fn summary(&self) -> String {
        if self.loading { "Searching history…".into() }
        else if self.error.is_some() { "History unavailable".into() }
        else if let Some(page) = &self.page {
            if page.total == 0 { "0 captures".into() } else {
                format!("{}–{} of {} captures", page.page * HISTORY_PAGE_SIZE + 1,
                    page.page * HISTORY_PAGE_SIZE + page.entries.len(), page.total)
            }
        } else { "History".into() }
    }
}
impl Toolkit {
    pub(super) fn history_generation(&self) -> Option<u64> { self.history_ui.requested }
    pub(super) fn history_changed(&mut self, cx: &mut Context<Self>) {
        self.history_ui.dirty = true;
        if self.history_ui.started && self.pane == 3 {
            self.request_history_page(self.history_ui.page_index, cx);
        }
    }
    pub(super) fn schedule_history_search(&mut self, cx: &mut Context<Self>) {
        let text = normalized(&self.query.read(cx).value());
        if text == self.history_ui.text { cx.notify(); return; }
        self.history_ui.text = text;
        self.history_ui.page_index = 0;
        self.history_ui.requested = None;
        self.history_ui.row_focus.clear();
        self.history_ui.last_revealed_focus = None;
        self.history_ui.loading = true;
        self.history_ui.error = None;
        self.history_ui.debounce = Some(cx.spawn(async move |view, cx| {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let _ = view.update(cx, |this, cx| {
                if this.history_ui.requested.is_none() { this.request_history_page(0, cx); }
            });
        }));
        cx.notify();
    }
    pub(super) fn request_history_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.history_ui.started = true;
        self.history_ui.dirty = false;
        self.history_ui.loading = true;
        self.history_ui.row_focus.clear();
        self.history_ui.last_revealed_focus = None;
        self.history_ui.error = None;
        self.history_ui.text = normalized(&self.query.read(cx).value());
        let search = self.history_ui.fixture.as_ref().or_else(|| self.worker.as_ref().map(|worker| &worker.history_search));
        self.history_ui.requested = search.map(|search| search.request(&self.history_ui.text, page));
        if self.history_ui.requested.is_none() {
            self.history_ui.loading = false;
            self.history_ui.error = Some("History worker is unavailable. Restart rcr to reconnect.".into());
        }
        cx.notify();
    }
    pub(super) fn poll_history(&mut self, cx: &mut Context<Self>) {
        let search = self.history_ui.fixture.as_ref().or_else(|| self.worker.as_ref().map(|worker| &worker.history_search));
        let Some(result) = search.and_then(HistorySearch::take_result) else { return; };
        if self.history_ui.requested != Some(result.generation) { return; }
        self.history_ui.loading = false;
        match result.result {
            Ok(page) => {
                self.history_ui.scroll.scroll_to_top_of_item(0);
                self.history_ui.page_index = page.page;
                self.history_ui.page = Some(page);
                self.history_ui.error = None;
            }
            Err(error) => self.history_ui.error = Some(error),
        }
        cx.notify();
    }
    pub(super) fn history(&mut self, cx: &mut Context<Self>) -> Div {
        if !self.history_ui.started || self.history_ui.dirty { self.request_history_page(self.history_ui.page_index, cx); }
        let previous_focus = self.history_ui.previous_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
        let next_focus = self.history_ui.next_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
        let refresh_focus = self.history_ui.refresh_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
        if !self.history_ui.loading && self.history_ui.error.is_none() {
            let mut keys = BTreeSet::new();
            if let Some(page) = &self.history_ui.page {
                for entry in &page.entries {
                    if entry.upload_url.is_some() { keys.insert((page.generation, entry.id, RowAction::CopyUrl)); }
                    if let Some(path) = &entry.local_path {
                        keys.insert((page.generation, entry.id, RowAction::ShowFile));
                        if recordings::is_image(path) { keys.insert((page.generation, entry.id, RowAction::Edit)); }
                    }
                }
            }
            self.history_ui.row_focus.retain(|key, _| keys.contains(key));
            for key in keys { self.history_ui.row_focus.entry(key).or_insert_with(|| cx.focus_handle().tab_stop(true)); }
        } else { self.history_ui.row_focus.clear(); }
        let state = &self.history_ui;
        let ready = !state.loading && state.error.is_none();
        let previous = ready && state.page_index > 0;
        let next = ready && state.page.as_ref().is_some_and(|page| (page.page as u64 + 1) * (HISTORY_PAGE_SIZE as u64) < page.total);
        let summary = state.summary();
        let root = div().w_full().h_full().min_w_0().min_h_0().flex().flex_col().gap_2()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let modifiers = event.keystroke.modifiers;
                if modifiers.alt || modifiers.platform { return; }
                if !modifiers.control && event.keystroke.key == "tab" {
                    if modifiers.shift { window.focus_prev(); } else { window.focus_next(); }
                    this.reveal_history_focus(window, cx);
                } else if !event.is_held && !modifiers.shift && modifiers.control && event.keystroke.key == "f" {
                    this.query.read(cx).focus_handle(cx).focus(window);
                } else if !event.is_held && !modifiers.shift && !modifiers.control && event.keystroke.key == "f5" && !this.history_ui.loading {
                    if let Some(focus) = &this.history_ui.refresh_focus { focus.focus(window); }
                    this.request_history_page(this.history_ui.page_index, cx);
                } else { return; }
                window.prevent_default(); cx.stop_propagation();
            }))
            .child(div().text_size(px(22.)).child("Capture & upload history"))
            .child(muted("Search all saved history, including older captures and application names."))
            .child(div().w_full().min_w_0().child(self.query.clone()))
            .child(div().w_full().flex().flex_wrap().items_center().justify_between().gap_2()
                .child(muted(summary))
                .child(div().flex().gap_2()
                    .child(button("history-refresh", "Refresh").track_focus(&refresh_focus)
                        .opacity(if state.loading { 0.4 } else { 1. }).tab_stop(!state.loading)
                        .when_some(state.probe.clone(), |s, probe| s.child(measure(probe, 3)))
                        .when(!state.loading, |s| s.on_click(cx.listener(|this, _, _, cx| this.request_history_page(this.history_ui.page_index, cx)))))
                    .child(button("history-previous", "Previous").track_focus(&previous_focus)
                        .opacity(if previous { 1. } else { 0.4 }).tab_stop(previous)
                        .when_some(state.probe.clone(), |s, probe| s.child(measure(probe, 1)))
                        .when(previous, |s| s.on_click(cx.listener(|this, _, _, cx| this.request_history_page(this.history_ui.page_index.saturating_sub(1), cx)))))
                    .child(button("history-next", "Next").track_focus(&next_focus)
                        .opacity(if next { 1. } else { 0.4 }).tab_stop(next)
                        .when_some(state.probe.clone(), |s, probe| s.child(measure(probe, 2)))
                        .when(next, |s| s.on_click(cx.listener(|this, _, _, cx| this.request_history_page(this.history_ui.page_index + 1, cx)))))));
        if state.loading { return root.child(panel().child(muted("Searching…"))); }
        if let Some(error) = &state.error {
            return root.child(panel().child("History could not be loaded")
                .child(div().min_w_0().whitespace_normal().text_color(rgb(0xffb0b0)).child(error.clone()))
                .child(button("retry-history", "Retry").on_click(cx.listener(|this, _, _, cx| this.request_history_page(this.history_ui.page_index, cx)))));
        }
        let Some(page) = &state.page else { return root; };
        if page.entries.is_empty() {
            return root.child(panel().child(if state.text.is_empty() { "No captures yet" } else { "No captures match your search" })
                .child(muted(if state.text.is_empty() { "Saved screenshots and uploads will appear here." } else { "Try a filename, upload link, application or window title." }))
                .when(!state.text.is_empty(), |s| s.child(button("clear-history-search", "Clear search").on_click(cx.listener(|this, _, _, cx| {
                    this.query.update(cx, |input, cx| input.set_value(String::new(), cx));
                })))));
        }
        let mut rows = div().id("history-rows").w_full().flex_1().min_h_0().overflow_y_scroll()
            .track_scroll(&state.scroll)
            .flex().flex_col();
        for entry in &page.entries {
            let name = entry.local_path.as_ref().and_then(|path| path.file_name()).map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{:?}", entry.kind));
            let location = entry.upload_url.clone().or_else(|| entry.local_path.as_ref().map(|path| path.display().to_string()))
                .unwrap_or_else(|| "No local file or public link".into());
            let tooltip = location.clone();
            let mut actions = div().flex().flex_wrap().gap_2().flex_shrink_0();
            if let Some(url) = entry.upload_url.clone() {
                let focus = state.row_focus.get(&(page.generation, entry.id, RowAction::CopyUrl)).expect("Current history action focus");
                actions = actions.child(button(("copy-history-url", entry.id as usize), "Copy URL").track_focus(focus).h(px(28.)).px_2().text_size(px(12.)).on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone())); this.status = "Link copied".into(); cx.notify();
                })));
            }
            if let Some(path) = entry.local_path.clone() {
                let reveal = path.clone();
                let focus = state.row_focus.get(&(page.generation, entry.id, RowAction::ShowFile)).expect("Current history action focus");
                actions = actions.child(button(("reveal-history", entry.id as usize), if self.shell_operation != 0 { "Opening…" } else { "Show file" }).track_focus(focus).h(px(28.)).px_2().text_size(px(12.)).on_click(cx.listener(move |this, _, _, cx| this.show_file(reveal.clone(), cx))));
                if recordings::is_image(&path) {
                    let focus = state.row_focus.get(&(page.generation, entry.id, RowAction::Edit)).expect("Current history action focus");
                    actions = actions.child(button(("edit-history", entry.id as usize), "Edit").track_focus(focus).h(px(28.)).px_2().text_size(px(12.)).on_click(cx.listener(move |this, _, _, cx| this.open_image(path.clone(), cx))));
                }
            }
            rows = rows.child(div().w_full().min_w_0().h(px(56.)).px_3().py_2().border_b_1().border_color(rgb(LINE))
                .flex().flex_shrink_0().flex_row().items_center().gap_3()
                .when_some(state.probe.clone(), |s, probe| s.relative().child(measure(probe, 100 + entry.id as usize)))
                .child(icon(if entry.kind == rcr_toolkit::core::history::EntryKind::Video { "film" } else { "folder" }))
                .child(div().id(("history-label", entry.id as usize)).flex_1().min_w_0().flex().flex_col().gap_1().overflow_hidden()
                    .tooltip(move |_, cx| cx.new(|_| HistoryTip(tooltip.clone())).into())
                    .child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().child(name))
                    .child(muted(location).w_full().overflow_hidden().whitespace_nowrap()))
                .child(actions));
        }
        root.child(rows)
    }
    pub(super) fn reveal_history_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_ui.loading || self.history_ui.error.is_some() {
            self.history_ui.last_revealed_focus = None;
            return;
        }
        let focused = self.history_ui.row_focus.iter().find(|(_, focus)| focus.is_focused(window)).map(|(key, _)| *key);
        if focused == self.history_ui.last_revealed_focus { return; }
        self.history_ui.last_revealed_focus = focused;
        let Some((generation, id, _)) = focused else { return; };
        let Some(page) = &self.history_ui.page else { return; };
        if self.history_ui.requested != Some(generation) || page.generation != generation { return; }
        if let Some(index) = page.entries.iter().position(|entry| entry.id == id) {
            let Some(bounds) = self.history_ui.scroll.bounds_for_item(index) else { return; };
            let viewport = self.history_ui.scroll.bounds();
            let offset = self.history_ui.scroll.offset();
            if bounds.top() + offset.y >= viewport.top() - px(0.5) && bounds.bottom() + offset.y <= viewport.bottom() + px(0.5) { return; }
            // GPUI's FirstVisible strategy changes the offset only if this row
            // is partly or wholly outside the result viewport.
            self.history_ui.scroll.scroll_to_item(index);
            cx.notify();
        }
    }
}
fn measure(probe: Probe, key: usize) -> Div {
    div().absolute().size_full().child(canvas(move |bounds, _, _| { probe.borrow_mut().insert(key, bounds); }, |_, _, _, _| {}).absolute().size_full())
}
struct HistoryTip(String);
impl Render for HistoryTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().max_w(px(560.)).px_3().py_2().rounded_md().bg(rgb(SOFT)).text_size(px(12.)).whitespace_normal().child(self.0.clone())
    }
}

#[derive(Default)]
pub struct HistorySmoke {
    stage: u8,
    original: Option<Box<HistoryUi>>,
    query: String,
    pane: usize,
    focus: Option<FocusHandle>,
    status: String,
    catalog: Vec<i64>,
    database: Option<Arc<rcr_toolkit::core::history::History>>,
}

fn fixture_database() -> anyhow::Result<Arc<rcr_toolkit::core::history::History>> {
    use rcr_toolkit::core::history::{EntryKind, History, NewEntry};
    let history = Arc::new(History::open_in_memory()?);
    for i in 0..257 {
        let mut entry = NewEntry::new(EntryKind::Image);
        entry.created_at = i;
        entry.local_path = Some(PathBuf::from(if i == 0 { "oldest-distinctive.png".into() } else { format!("Capture-{i:03}.png") }));
        entry.process_name = Some("Owned fixture application".into());
        history.insert(&entry)?;
    }
    Ok(history)
}
fn fixture_search() -> anyhow::Result<HistorySearch> { HistorySearch::new(fixture_database()?) }
fn key(window: &mut Window, cx: &mut App, key: &str) {
    let keystroke = Keystroke::parse(key).expect("Fixture key");
    window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent { keystroke: keystroke.clone(), is_held: false }), cx);
    window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
    window.draw(cx).clear();
}
fn text(window: &mut Window, cx: &mut App, text: &str) {
    for ch in text.chars() {
        window.dispatch_keystroke(Keystroke { key: ch.to_string(), key_char: Some(ch.to_string()), modifiers: Default::default() }, cx);
    }
    window.draw(cx).clear();
}
fn ensure_row_visible(view: &Entity<Toolkit>, id: i64, window: &Window, cx: &App) -> anyhow::Result<()> {
    let state = &view.read(cx).history_ui;
    let bounds = state.probe.as_ref().unwrap().borrow().get(&(100 + id as usize)).copied()
        .ok_or_else(|| anyhow::anyhow!("Focused history row was not painted"))?;
    let viewport = state.scroll.bounds();
    anyhow::ensure!(bounds.top() >= viewport.top() - px(0.5) && bounds.bottom() <= viewport.bottom() + px(0.5)
        && bounds.right() <= window.viewport_size().width, "Keyboard-focused history row is outside the scrolling viewport");
    Ok(())
}
impl Toolkit {
    /// In-memory visual fixture: no local file, settings, clipboard or upload action.
    pub(super) fn seed_history_fixture(&mut self, cx: &mut Context<Self>) -> bool {
        if self.history_ui.fixture.is_none() {
            match fixture_search() {
                Ok(search) => self.history_ui = HistoryUi { fixture: Some(search), ..Default::default() },
                Err(error) => { eprintln!("History fixture setup failed: {error:#}"); return false; }
            }
            self.query.update(cx, |input, cx| input.set_value(String::new(), cx));
            self.pane = 3;
            self.request_history_page(0, cx);
        }
        !self.history_ui.loading && self.history_ui.page.is_some()
    }
    pub fn smoke_history(view: Entity<Self>, state: &mut HistorySmoke, window: &mut Window, cx: &mut App) -> anyhow::Result<bool> {
        use anyhow::ensure;
        if view.read(cx).snapshot.is_none() { return Ok(false); }
        view.update(cx, |this, cx| this.poll_history(cx));
        window.draw(cx).clear();
        if state.stage == 0 {
            ensure!(!view.read(cx).worker.as_ref().is_some_and(|worker| worker.busy()), "History fixture requires an idle workflow worker");
            state.query = view.read(cx).query.read(cx).value();
            state.pane = view.read(cx).pane;
            state.focus = window.focused(cx);
            state.status = view.read(cx).status.clone();
            state.catalog = view.read(cx).snapshot.as_ref().unwrap().history.iter().map(|entry| entry.id).collect();
            let database = fixture_database()?;
            let search = HistorySearch::new(database.clone())?;
            state.database = Some(database);
            view.update(cx, |this, cx| {
                state.original = Some(Box::new(std::mem::replace(&mut this.history_ui,
                    HistoryUi { fixture: Some(search), probe: Some(Probe::default()), ..Default::default() })));
                this.query.update(cx, |input, cx| input.set_value(String::new(), cx));
                this.pane = 3;
                this.request_history_page(0, cx);
            });
            state.stage = 1;
            return Ok(false);
        }
        if view.read(cx).history_ui.loading { return Ok(false); }
        if let Some(error) = &view.read(cx).history_ui.error { anyhow::bail!("History fixture query failed: {error}"); }
        match state.stage {
            1 => {
                let page = view.read(cx).history_ui.page.as_ref().unwrap();
                ensure!((page.total, page.page, page.entries.len()) == (257, 0, 50), "Initial full-history page is incorrect");
                let bounds = view.read(cx).history_ui.probe.as_ref().unwrap().borrow();
                let row = bounds.get(&357).ok_or_else(|| anyhow::anyhow!("First history row was not painted"))?;
                ensure!(row.size.height >= px(48.) && row.size.height <= px(56.), "History rows are not compact");
                for key in [1, 2, 3] {
                    let button = bounds.get(&key).ok_or_else(|| anyhow::anyhow!("History paging control not painted"))?;
                    ensure!(button.size.width >= px(40.) && button.right() <= window.viewport_size().width && button.bottom() <= window.viewport_size().height,
                        "History paging control exceeds the viewport");
                }
                drop(bounds);
                let toolbar = view.read(cx).history_ui.probe.as_ref().unwrap().borrow().get(&2).copied().unwrap();
                view.read(cx).history_ui.scroll.scroll_to_item(49);
                window.draw(cx).clear();
                ensure!(view.read(cx).history_ui.scroll.offset().y < px(-100.), "History fixture did not scroll the result rows");
                ensure!(view.read(cx).history_ui.probe.as_ref().unwrap().borrow().get(&2).copied() == Some(toolbar),
                    "History paging controls scrolled away with rows");
                view.read(cx).history_ui.next_focus.as_ref().unwrap().focus(window);
                key(window, cx, "enter");
                state.stage = 2;
            }
            2 => {
                ensure!(view.read(cx).history_ui.page_index == 1, "Keyboard Next did not reach older entries");
                view.read(cx).history_ui.previous_focus.as_ref().unwrap().focus(window);
                key(window, cx, "space");
                state.stage = 3;
            }
            3 => {
                ensure!(view.read(cx).history_ui.page_index == 0, "Keyboard Previous did not return to first page");
                let page = view.read(cx).history_ui.page.as_ref().unwrap();
                let generation = page.generation;
                let first = page.entries.first().unwrap().id;
                let last = page.entries.last().unwrap().id;
                ensure!(view.read(cx).history_ui.row_focus.len() == 100, "History row focus map exceeds current page actions");
                view.read(cx).history_ui.next_focus.as_ref().unwrap().focus(window);
                key(window, cx, "tab");
                let first_show = view.read(cx).history_ui.row_focus.get(&(generation, first, RowAction::ShowFile)).unwrap().clone();
                ensure!(first_show.is_focused(window), "Tab did not enter the first history row");
                let offset = view.read(cx).history_ui.scroll.offset();
                key(window, cx, "tab");
                ensure!(view.read(cx).history_ui.scroll.offset() == offset, "Tab between visible row actions caused a gratuitous scroll jump");
                for _ in 0..98 { key(window, cx, "tab"); }
                let last_edit = view.read(cx).history_ui.row_focus.get(&(generation, last, RowAction::Edit)).unwrap().clone();
                ensure!(last_edit.is_focused(window), "Tab did not reach the last history row action");
                ensure_row_visible(&view, last, window, cx)?;
                let offset = view.read(cx).history_ui.scroll.offset();
                key(window, cx, "shift-tab");
                ensure!(view.read(cx).history_ui.scroll.offset() == offset, "Shift+Tab within a visible row caused a scroll jump");
                // Start focus above the viewport, then use actual reverse Tab to
                // reveal its previous action without invoking any OS action.
                view.read(cx).history_ui.row_focus.get(&(generation, first, RowAction::Edit)).unwrap().focus(window);
                key(window, cx, "shift-tab");
                ensure!(first_show.is_focused(window), "Shift+Tab did not return to the first row action");
                ensure_row_visible(&view, first, window, cx)?;
                view.read(cx).history_ui.row_focus.get(&(generation, last, RowAction::ShowFile)).unwrap().focus(window);
                key(window, cx, "tab");
                ensure!(last_edit.is_focused(window), "Tab failed from an offscreen row");
                ensure_row_visible(&view, last, window, cx)?;
                // Focus may enter from a sibling footer rather than dispatching
                // a key through History. The render hook must reveal it too.
                view.read(cx).history_ui.row_focus.get(&(generation, first, RowAction::ShowFile)).unwrap().focus(window);
                window.draw(cx).clear();
                ensure_row_visible(&view, first, window, cx)?;
                let position = view.read(cx).history_ui.scroll.bounds().center();
                window.dispatch_event(PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position, delta: ScrollDelta::Lines(point(0., -100.)), ..Default::default()
                }), cx);
                window.draw(cx).clear();
                let manual_offset = view.read(cx).history_ui.scroll.offset();
                ensure!(manual_offset.y < px(-100.) && first_show.is_focused(window), "Manual wheel scroll did not move away from the focused row");
                view.update(cx, |_, cx| cx.notify());
                window.draw(cx).clear();
                ensure!(view.read(cx).history_ui.scroll.offset() == manual_offset, "History render snapped manual scrolling back to unchanged row focus");
                last_edit.focus(window);
                window.draw(cx).clear();
                ensure_row_visible(&view, last, window, cx)?;
                key(window, cx, "ctrl-f");
                ensure!(view.read(cx).query.read(cx).focus_handle(cx).is_focused(window), "History Ctrl+F did not focus search");
                text(window, cx, "oldest");
                state.stage = 4;
            }
            4 => {
                let page = view.read(cx).history_ui.page.as_ref().unwrap();
                ensure!(page.text == "oldest" && page.total == 1 && page.entries[0].id == 1,
                    "Typing did not find the capture older than the first 100 records");
                ensure!(view.read(cx).history_ui.row_focus.len() == 2 && view.read(cx).history_ui.row_focus.keys().all(|(generation, id, _)|
                    *generation == page.generation && *id == 1), "New query retained stale page/action focus identities");
                key(window, cx, "ctrl-a"); text(window, cx, "missing-query");
                key(window, cx, "ctrl-a"); text(window, cx, "oldest");
                state.stage = 5;
            }
            5 => {
                let page = view.read(cx).history_ui.page.as_ref().unwrap();
                ensure!(page.text == "oldest" && page.total == 1, "Superseded search replaced latest results");
                ensure!(view.read(cx).status == state.status, "Searching changed workflow completion status");
                ensure!(!view.read(cx).worker.as_ref().is_some_and(|worker| worker.busy()), "Searching consumed workflow admission");
                ensure!(view.read(cx).snapshot.as_ref().unwrap().history.iter().map(|entry| entry.id).eq(state.catalog.iter().copied()),
                    "Filtered history replaced the Library catalog feed");
                // Simulate a capture completing while another Tools pane is open.
                view.update(cx, |this, cx| { this.pane = 0; cx.notify(); });
                let mut entry = rcr_toolkit::core::history::NewEntry::new(rcr_toolkit::core::history::EntryKind::Image);
                entry.local_path = Some(PathBuf::from("oldest-new-capture.png")); entry.created_at = 300;
                state.database.as_ref().unwrap().insert(&entry)?;
                view.update(cx, |this, cx| { this.history_changed(cx); this.pane = 3; cx.notify(); });
                window.draw(cx).clear();
                state.stage = 6;
            }
            6 => {
                ensure!(view.read(cx).history_ui.page.as_ref().unwrap().total == 2, "Returning to History missed a capture saved on another pane");
                let mut entry = rcr_toolkit::core::history::NewEntry::new(rcr_toolkit::core::history::EntryKind::Image);
                entry.local_path = Some(PathBuf::from("oldest-external-capture.png")); entry.created_at = 301;
                state.database.as_ref().unwrap().insert(&entry)?;
                key(window, cx, "f5");
                state.stage = 7;
            }
            7 => {
                ensure!(view.read(cx).history_ui.page.as_ref().unwrap().total == 3, "History F5 missed an external database change");
                view.update(cx, |this, cx| {
                    this.history_ui.error = Some("Owned history query failure fixture".into());
                    cx.notify();
                });
                window.draw(cx).clear();
                ensure!(view.read(cx).history_ui.summary() == "History unavailable" && view.read(cx).history_ui.row_focus.is_empty(),
                    "History failure retained stale counts or actionable row focus");
                view.update(cx, |this, cx| {
                    this.query.update(cx, |input, cx| input.set_value(state.query.clone(), cx));
                    this.history_ui = *state.original.take().unwrap();
                    this.pane = state.pane;
                    cx.notify();
                });
                if let Some(focus) = &state.focus { focus.focus(window); }
                window.draw(cx).clear();
                state.database = None;
                state.stage = 8;
                println!("History smoke: 257 initial in-memory entries; compact/fixed controls, 100 row-action Tab stops with visible forward/reverse focus, no visible-row jumps or manual-wheel snapback, Ctrl+F from last row, typed oldest/latest-query identities, pane-return/F5 freshness; unchanged workflow admission/status and Library catalog; no file/clipboard/network actions");
                return Ok(true);
            }
            _ => return Ok(true),
        }
        Ok(false)
    }
}
