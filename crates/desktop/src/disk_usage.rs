//! Dense disk usage workspace; filesystem access stays on a cancellable worker.
use crate::{disk_treemap, theme::*};
use gpui::{prelude::*, *};
use ::toolkit::{
    core::workflow::CancelToken,
    disk_usage::{self, DiskUsage, ScanProgress},
};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::mpsc,
    time::Duration,
};
#[path = "disk_explore.rs"]
mod explore;

struct FileType {
    extension: String,
    bytes: u64,
    files: Vec<usize>,
}
struct Results {
    scan: std::sync::Arc<DiskUsage>,
    largest: Vec<usize>,
    types: Vec<FileType>,
}
impl Results {
    fn new(scan: DiskUsage) -> Self {
        let mut largest: Vec<_> = scan
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| !n.is_directory)
            .map(|(id, _)| id)
            .collect();
        largest.sort_unstable_by(|a, b| {
            scan.nodes[*b]
                .bytes
                .cmp(&scan.nodes[*a].bytes)
                .then(a.cmp(b))
        });
        let mut groups = BTreeMap::<String, (u64, Vec<usize>)>::new();
        for &id in &largest {
            let node = &scan.nodes[id];
            let entry = groups.entry(extension(&node.name)).or_default();
            entry.0 = entry.0.saturating_add(node.bytes);
            entry.1.push(id);
        }
        let mut types: Vec<_> = groups
            .into_iter()
            .map(|(extension, (bytes, files))| FileType {
                extension,
                bytes,
                files,
            })
            .collect();
        types.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then(a.extension.cmp(&b.extension)));
        Self {
            scan: std::sync::Arc::new(scan),
            largest,
            types,
        }
    }
}
struct Job {
    cancel: CancelToken,
    progress: mpsc::Receiver<ScanProgress>,
    done: mpsc::Receiver<std::io::Result<Results>>,
}
pub struct DiskUsageView {
    icons: Entity<crate::file_type_icons::FileTypeIcons>,
    results: Option<Results>,
    job: Option<Job>,
    progress: Option<ScanProgress>,
    status: String,
    folder: usize,
    selected: usize,
    expanded: HashSet<usize>,
    rows: Vec<(usize, usize)>,
    largest_mode: bool,
    explore_mode: bool,
    explore_boxes: bool,
    hovered: Option<usize>,
    map_mode: bool,
    file_map: bool,
    extension_filter: Option<String>,
    picker_open: bool,
    list_scroll: UniformListScrollHandle,
    explore_scroll: UniformListScrollHandle,
}
impl DiskUsageView {
    /// The icon worker is persistent and harmless while idle; only an actual
    /// scan or an outstanding user folder choice defers an update restart.
    pub fn update_blocker(&self) -> Option<&'static str> {
        if self.job.is_some() { Some("Wait for the disk scan to finish or stop it first.") }
        else if self.picker_open { Some("Close the disk folder picker first.") }
        else { None }
    }
    pub fn scan_finished(&self, cx: &App) -> bool {
        self.job.is_none() && self.icons.read(cx).ready()
    }
    pub fn show_files(&mut self, cx: &mut Context<Self>) {
        if !self.largest_mode || self.explore_mode {
            self.explore_mode = false;
            self.largest_mode = true;
            self.extension_filter = None;
            self.rebuild_rows();
            cx.notify();
        }
    }
    pub fn show_tree(&mut self, cx: &mut Context<Self>) {
        if self.explore_mode || self.largest_mode {
            self.explore_mode = false;
            self.largest_mode = false;
            self.extension_filter = None;
            self.rebuild_rows();
            cx.notify();
        }
    }
    pub fn show_explore(&mut self, boxes: bool, cx: &mut Context<Self>) {
        if !self.explore_mode || self.explore_boxes != boxes {
            self.explore_mode = true;
            self.explore_boxes = boxes;
            cx.notify();
        }
    }
    pub fn show_map(&mut self, cx: &mut Context<Self>) {
        if !self.map_mode {
            self.map_mode = true;
            cx.notify();
        }
    }
    pub fn new(cx: &mut Context<Self>) -> Self {
        let icons = cx.new(crate::file_type_icons::FileTypeIcons::new);
        cx.observe(&icons, |_, _, cx| cx.notify()).detach();
        Self {
            icons,
            results: None,
            job: None,
            progress: None,
            status: "Choose a drive or folder to analyze.".into(),
            folder: 0,
            selected: 0,
            expanded: HashSet::new(),
            rows: Vec::new(),
            largest_mode: false,
            explore_mode: true,
            explore_boxes: false,
            hovered: None,
            map_mode: true,
            file_map: false,
            extension_filter: None,
            picker_open: false,
            list_scroll: UniformListScrollHandle::new(),
            explore_scroll: UniformListScrollHandle::new(),
        }
    }
    fn choose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() || self.picker_open {
            return;
        }
        self.picker_open = true;
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a drive or folder to analyze".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = prompt.await;
            let _ = view.update(cx, |this, cx| {
                this.picker_open = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.start(path, cx);
                        }
                    }
                    Ok(Err(error)) => this.status = format!("Could not choose folder: {error}"),
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub fn start(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.job.is_some() {
            return;
        }
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let (ptx, progress) = mpsc::sync_channel(1);
        let (dtx, done) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("disk-usage-scan".into())
            .spawn(move || {
                let result = disk_usage::scan(path, &token, |progress| {
                    let _ = ptx.try_send(progress);
                })
                .map(Results::new);
                let _ = dtx.send(result);
            }) {
            Ok(_) => {
                self.results = None;
                self.rows.clear();
                self.expanded.clear();
                self.extension_filter = None;
                self.progress = None;
                self.folder = 0;
                self.selected = 0;
                self.hovered = None;
                self.explore_scroll = UniformListScrollHandle::new();
                self.status = "Scanning…".into();
                self.job = Some(Job {
                    cancel,
                    progress,
                    done,
                });
                cx.spawn(async move |view, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        if view
                            .update(cx, |this, cx| {
                                this.poll(cx);
                                this.job.is_some()
                            })
                            .ok()
                            != Some(true)
                        {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => self.status = format!("Could not start scan: {error}"),
        }
        cx.notify();
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let Some(job) = &self.job else {
            return;
        };
        if let Ok(progress) = job.progress.try_recv() {
            self.progress = Some(progress);
            cx.notify();
        }
        match job.done.try_recv() {
            Ok(result) => {
                self.job = None;
                match result {
                    Ok(results) => {
                        let root = &results.scan.nodes[0];
                        self.status = format!(
                            "{} in {:.1}s | {} | {} files | {} folders | {} skipped | {} errors",
                            if results.scan.cancelled {
                                "Stopped — partial results"
                            } else {
                                "Scan complete"
                            },
                            results.scan.elapsed.as_secs_f64(),
                            results.scan.method.label(),
                            root.files,
                            root.directories,
                            root.skipped,
                            root.errors
                        );
                        if let Some(reason) = &results.scan.fallback_reason {
                            self.status = format!("{reason} | {}", self.status);
                        }
                        self.results = Some(results);
                        self.expanded.insert(0);
                        self.rebuild_rows();
                    }
                    Err(error) => self.status = format!("Scan failed: {error}"),
                }
                cx.notify();
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                self.status = "Scan worker stopped unexpectedly. Choose a folder to retry.".into();
                cx.notify();
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let Some(results) = &self.results else {
            return;
        };
        if self.largest_mode {
            let ids = self
                .extension_filter
                .as_ref()
                .and_then(|filter| results.types.iter().find(|t| t.extension == *filter))
                .map(|t| &t.files)
                .unwrap_or(&results.largest);
            self.rows.extend(ids.iter().map(|id| (*id, 0)));
        } else {
            let mut stack = vec![(self.folder, 0)];
            while let Some((id, depth)) = stack.pop() {
                self.rows.push((id, depth));
                if self.expanded.contains(&id) {
                    stack.extend(
                        results.scan.nodes[id]
                            .children
                            .iter()
                            .rev()
                            .map(|id| (*id, depth + 1)),
                    );
                }
            }
        }
    }
    fn focus_folder(&mut self, id: usize, cx: &mut Context<Self>) {
        if self
            .results
            .as_ref()
            .is_none_or(|r| !r.scan.nodes[id].is_directory)
        {
            return;
        }
        self.folder = id;
        self.hovered = None;
        self.explore_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.selected = id;
        self.expanded.clear();
        self.expanded.insert(id);
        self.largest_mode = false;
        self.extension_filter = None;
        self.rebuild_rows();
        self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }
    fn type_icon(&self, key: crate::file_type_icons::Key, cx: &mut Context<Self>) -> AnyElement {
        let image = self.icons.update(cx, |icons, cx| icons.get(key, cx));
        match image {
            Some(image) => img(image)
                .size(px(16.))
                .flex_shrink_0()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => div().size(px(16.)).flex_shrink_0().into_any_element(),
        }
    }
    fn row(&self, index: usize, wide: bool, cx: &mut Context<Self>) -> AnyElement {
        let (id, depth) = self.rows[index];
        let scan = &self.results.as_ref().unwrap().scan;
        let node = &scan.nodes[id];
        let denominator = if self.largest_mode {
            0
        } else {
            node.parent.unwrap_or(id)
        };
        let share = node.bytes as f64 / scan.nodes[denominator].bytes.max(1) as f64;
        let is_dir = node.is_directory;
        let mut row = div()
            .id(("disk-row", id))
            .tab_index(0)
            .h(px(21.))
            .w_full()
            .flex()
            .items_center()
            .text_size(px(12.))
            .border_1()
            .border_color(rgba(0x00000000))
            .bg(rgb(if id == self.selected {
                0x164b69
            } else if index.is_multiple_of(2) {
                0x17191c
            } else {
                PANEL
            }))
            .focus(|s| s.border_color(rgb(ACCENT)))
            .hover(|s| s.bg(rgb(0x283139)))
            .cursor_pointer()
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "enter" {
                    this.selected = id;
                    if is_dir {
                        this.focus_folder(id, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.selected = id;
                if event.click_count() == 2 && is_dir {
                    this.focus_folder(id, cx);
                }
                cx.notify();
            }));
        row = row.child(
            div()
                .h_full()
                .flex()
                .items_center()
                .min_w_0()
                .gap_1()
                .when(!self.largest_mode, |s| s.flex_1())
                .when(self.largest_mode, |s| {
                    s.w(px(if wide { 250. } else { 170. })).flex_shrink_0()
                })
                .border_r_1()
                .border_color(rgb(LINE))
                .pl(px((depth.min(20) * 12) as f32 + 2.))
                .child(
                    div()
                        .id(("disk-expand", id))
                        .w(px(14.))
                        .flex_shrink_0()
                        .text_color(rgb(MUTED))
                        .child(if is_dir {
                            if self.expanded.contains(&id) {
                                "−"
                            } else {
                                "+"
                            }
                        } else {
                            ""
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if is_dir {
                                if !this.expanded.remove(&id) {
                                    this.expanded.insert(id);
                                }
                                this.rebuild_rows();
                                cx.notify();
                            }
                        })),
                )
                .child(self.type_icon(
                    if is_dir {
                        if self.expanded.contains(&id) {
                            crate::file_type_icons::Key::OpenFolder
                        } else {
                            crate::file_type_icons::Key::Folder
                        }
                    } else {
                        file_icon_key(&extension(&node.name))
                    },
                    cx,
                ))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(node.name.to_string_lossy().into_owned()),
                ),
        );
        if self.largest_mode {
            row = row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px_1()
                    .truncate()
                    .border_r_1()
                    .border_color(rgb(LINE))
                    .child(
                        scan.path(id)
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    ),
            );
        }
        row = row
            .child(
                div()
                    .w(px(80.))
                    .flex_shrink_0()
                    .h_full()
                    .relative()
                    .border_r_1()
                    .border_color(rgb(LINE))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(2.))
                            .bottom(px(2.))
                            .w(relative(share.min(1.) as f32))
                            .bg(rgb(0x354b46)),
                    )
                    .child(
                        div()
                            .relative()
                            .px_1()
                            .text_right()
                            .child(format!("{:.1}%", share * 100.)),
                    ),
            )
            .child(data_cell(format_bytes(node.bytes), 90.));
        if !self.largest_mode {
            row = row.child(data_cell(node.files.to_string(), 68.));
            if wide {
                row = row.child(data_cell(node.directories.to_string(), 68.));
            }
        }
        row.into_any_element()
    }
    fn treemap(&self, width: f32, height: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(results) = &self.results else {
            return div().into_any_element();
        };
        let scan = &results.scan;
        let layout = disk_treemap::nested_layout(self.folder, width, height, |id| {
            let node = &scan.nodes[id];
            (node.bytes, node.children.as_slice())
        });
        let mut map = div()
            .relative()
            .h(px(height))
            .flex_shrink_0()
            .w_full()
            .bg(rgb(0x090a0c))
            .overflow_hidden();
        for (index, tile) in layout.frames.iter().chain(layout.leaves.iter()).enumerate() {
            let frame = index < layout.frames.len();
            let id = tile.id;
            let node = &scan.nodes[id];
            let is_dir = node.is_directory;
            map = map.child(
                div()
                    .id(("disk-tile", index))
                    .absolute()
                    .left(relative(tile.x / width))
                    .top(px(tile.y))
                    .w(relative(tile.width / width))
                    .h(px(tile.height))
                    .border_1()
                    .border_color(rgb(if id == self.selected { TEXT } else { 0x282b30 }))
                    .bg(rgb(if frame {
                        0x25282e
                    } else {
                        node_color(&node.name, is_dir)
                    }))
                    .overflow_hidden()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.82))
                    .when(tile.width > 42. && tile.height > 12., |s| {
                        s.child(
                            div()
                                .px_1()
                                .text_size(px(10.))
                                .text_color(rgb(TEXT))
                                .child(
                                    div()
                                        .truncate()
                                        .child(node.name.to_string_lossy().into_owned()),
                                )
                                .when(!frame && !is_dir && tile.height > 32., |s| {
                                    s.child(div().truncate().child(format_bytes(node.bytes)))
                                }),
                        )
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.selected = id;
                        if (event.click_count() == 2 || this.explore_mode) && is_dir {
                            this.focus_folder(id, cx);
                        }
                        cx.notify();
                    })),
            );
        }
        map.into_any_element()
    }
}

impl Render for DiskUsageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.icons.update(cx, |icons, _| icons.prepare(window));
        let busy = self.job.is_some();
        let show_map = if self.largest_mode {
            self.file_map
        } else {
            self.map_mode
        };
        let wide = window.viewport_size().width >= px(1100.);
        let mut tools = div()
            .h(px(28.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .bg(rgb(PANEL))
            .child(
                dense_button("disk-choose", "Choose drive / folder")
                    .when(!busy && !self.picker_open, |s| {
                        s.on_click(cx.listener(|this, _, window, cx| this.choose(window, cx)))
                    })
                    .when(busy || self.picker_open, |s| s.opacity(0.5)),
            );
        if busy {
            tools = tools.child(dense_button("disk-stop", "Stop").on_click(cx.listener(
                |this, _, _, cx| {
                    if let Some(job) = &this.job {
                        job.cancel.cancel();
                    }
                    this.status = "Stopping — keeping partial results…".into();
                    cx.notify();
                },
            )));
        } else if self.results.is_some() {
            tools = tools
                .child(dense_button("disk-rescan", "Rescan").on_click(cx.listener(
                    |this, _, _, cx| {
                        if let Some(result) = &this.results {
                            this.start(result.scan.root_path.clone(), cx);
                        }
                    },
                )))
                .child(dense_button("disk-clear", "Clear").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.results = None;
                        this.hovered = None;
                        this.icons.update(cx, |icons, cx| icons.clear(cx));
                        this.rows = Vec::new();
                        this.expanded = HashSet::new();
                        this.progress = None;
                        this.extension_filter = None;
                        this.list_scroll = UniformListScrollHandle::new();
                        this.explore_scroll = UniformListScrollHandle::new();
                        this.status = "Results cleared".into();
                        cx.notify();
                    },
                )));
        }
        if let Some(results) = &self.results {
            let root = &results.scan.nodes[0];
            tools = tools
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px_2()
                        .truncate()
                        .child(results.scan.root_path.display().to_string()),
                )
                .child(div().flex_shrink_0().child(format!(
                    "{} | {} files | {} folders{}",
                    format_bytes(root.bytes),
                    root.files,
                    root.directories,
                    if results.scan.cancelled {
                        " | Partial"
                    } else {
                        ""
                    }
                )));
        } else {
            tools = tools.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .truncate()
                    .child("Disk usage"),
            );
        }
        let mut body = div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .text_size(px(12.))
            .gap(px(2.))
            .child(tools);
        if let Some(results) = &self.results {
            let scan = &results.scan;
            let selected_path = scan.path(self.selected);
            let mut navigation = div()
                .h(px(25.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .child(dense_button("disk-up", "↑").when_some(
                    scan.nodes[self.folder].parent,
                    |s, parent| {
                        s.on_click(cx.listener(move |this, _, _, cx| this.focus_folder(parent, cx)))
                    },
                ))
                .child(
                    dense_button("disk-root", "Root")
                        .on_click(cx.listener(|this, _, _, cx| this.focus_folder(0, cx))),
                )
                .child(self.breadcrumbs(cx))
                .child(dense_button("disk-reveal", "Reveal").on_click({
                    let path = selected_path.clone();
                    move |_, _, cx| cx.reveal_path(&path)
                }))
                .child(dense_button("disk-copy", "Copy path").on_click({
                    let path = selected_path.clone();
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()))
                    }
                }));
            if !scan.errors.is_empty() {
                let errors = scan
                    .errors
                    .iter()
                    .map(|e| format!("{}: {}", e.path.display(), e.message))
                    .collect::<Vec<_>>()
                    .join("\n");
                navigation = navigation.child(dense_button("disk-errors", "Copy errors").on_click(
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(errors.clone()))
                    },
                ));
            }
            body = body.child(navigation);
            let mut tabs = div()
                .h(px(24.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1()
                .border_b_1()
                .border_color(rgb(LINE));
            tabs = tabs.child(
                dense_button("disk-explore-tab", "Explore")
                    .bg(rgb(if self.explore_mode { 0x30343a } else { PANEL }))
                    .on_click(
                        cx.listener(|this, _, _, cx| this.show_explore(this.explore_boxes, cx)),
                    ),
            );
            for (mode, name) in [(false, "Tree View"), (true, "File View")] {
                tabs = tabs.child(
                    dense_button(("disk-mode", mode as usize), name)
                        .bg(rgb(if mode == self.largest_mode && !self.explore_mode {
                            0x30343a
                        } else {
                            PANEL
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.largest_mode = mode;
                            this.explore_mode = false;
                            this.extension_filter = None;
                            this.rebuild_rows();
                            this.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
                            cx.notify();
                        })),
                );
            }
            tabs = tabs
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(self.extension_filter.clone().unwrap_or_default()),
                )
                .when(!self.explore_mode, |s| {
                    s.child(
                        dense_button(
                            "disk-map-toggle",
                            if show_map { "Treemap ✓" } else { "Treemap" },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.largest_mode {
                                this.file_map = !this.file_map;
                            } else {
                                this.map_mode = !this.map_mode;
                            }
                            cx.notify();
                        })),
                    )
                });
            body = body.child(tabs);
            if self.explore_mode {
                return body.child(self.explore(window.viewport_size(), cx)).child(
                    div()
                        .h(px(20.))
                        .flex_shrink_0()
                        .px_1()
                        .text_size(px(11.))
                        .text_color(rgb(MUTED))
                        .child(self.status.clone()),
                );
            }
            let mut headings = div()
                .h(px(22.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .bg(rgb(0x292c31))
                .child(
                    div()
                        .min_w_0()
                        .h_full()
                        .px_1()
                        .border_r_1()
                        .border_color(rgb(LINE))
                        .when(!self.largest_mode, |s| s.flex_1())
                        .when(self.largest_mode, |s| {
                            s.w(px(if wide { 250. } else { 170. })).flex_shrink_0()
                        })
                        .child(if self.largest_mode {
                            "File name"
                        } else {
                            "Folder / file"
                        }),
                );
            if self.largest_mode {
                headings = headings.child(div().flex_1().min_w_0().px_1().child("Path"));
            }
            headings = headings
                .child(data_cell(
                    if self.largest_mode {
                        "% of scan"
                    } else {
                        "% of parent"
                    },
                    80.,
                ))
                .child(data_cell("Size ▾", 90.));
            if !self.largest_mode {
                headings = headings.child(data_cell("Files", 68.));
                if wide {
                    headings = headings.child(data_cell("Folders", 68.));
                }
            }
            let table = div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(PANEL))
                .child(headings)
                .child(
                    uniform_list(
                        "disk-files",
                        self.rows.len(),
                        cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                            let wide = window.viewport_size().width >= px(1100.);
                            range.map(|index| this.row(index, wide, cx)).collect()
                        }),
                    )
                    .track_scroll(self.list_scroll.clone())
                    .flex_1()
                    .min_h_0(),
                );
            let total = scan.nodes[0].bytes.max(1) as f64;
            let types = div()
                .w(px(if wide { 340. } else { 280. }))
                .flex_shrink_0()
                .min_h_0()
                .flex()
                .flex_col()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(PANEL))
                .child(
                    div()
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .bg(rgb(0x292c31))
                        .child(div().flex_1().min_w_0().px_1().child("Extension"))
                        .child(data_cell("Percent", 58.))
                        .child(data_cell("Size", 84.))
                        .child(data_cell("Files", 62.)),
                )
                .child(
                    uniform_list(
                        "disk-types",
                        results.types.len(),
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            range
                                .map(|index| {
                                    let entry = &this.results.as_ref().unwrap().types[index];
                                    let extension = entry.extension.clone();
                                    div()
                                        .id(("disk-type", index))
                                        .w_full()
                                        .h(px(21.))
                                        .flex()
                                        .items_center()
                                        .text_size(px(12.))
                                        .bg(rgb(if index.is_multiple_of(2) {
                                            0x17191c
                                        } else {
                                            PANEL
                                        }))
                                        .hover(|s| s.bg(rgb(0x283139)))
                                        .cursor_pointer()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .px_1()
                                                .child(
                                                    div()
                                                        .size(px(9.))
                                                        .flex_shrink_0()
                                                        .bg(rgb(type_color(&extension))),
                                                )
                                                .child(
                                                    this.type_icon(file_icon_key(&extension), cx),
                                                )
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .truncate()
                                                        .child(extension.clone()),
                                                ),
                                        )
                                        .child(data_cell(
                                            format!("{:.1}%", entry.bytes as f64 / total * 100.),
                                            58.,
                                        ))
                                        .child(data_cell(format_bytes(entry.bytes), 84.))
                                        .child(data_cell(entry.files.len().to_string(), 62.))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.largest_mode = true;
                                            this.explore_mode = false;
                                            this.extension_filter = Some(extension.clone());
                                            this.rebuild_rows();
                                            this.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
                                            cx.notify();
                                        }))
                                        .into_any_element()
                                })
                                .collect()
                        }),
                    )
                    .flex_1()
                    .min_h_0(),
                );
            body = body.child(
                div()
                    .flex_1()
                    .min_h(px(80.))
                    .flex()
                    .gap(px(3.))
                    .child(table)
                    .when(!self.largest_mode, |s| s.child(types)),
            );
            if show_map {
                let width = (f32::from(window.viewport_size().width) - 18.).max(200.);
                let height = (f32::from(window.viewport_size().height) * 0.25).clamp(100., 260.);
                body = body.child(self.treemap(width, height, cx));
            }
            body = body.child(
                div()
                    .h(px(21.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .bg(rgb(PANEL))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(selected_path.display().to_string()),
                    )
                    .child(div().flex_shrink_0().child(format!(
                        "{} | {} files | {} folders",
                        format_bytes(scan.nodes[self.selected].bytes),
                        scan.nodes[self.selected].files,
                        scan.nodes[self.selected].directories
                    ))),
            );
        } else {
            let status = self
                .progress
                .as_ref()
                .map(|p| {
                    if p.total_records > 0 {
                        return format!(
                            "{} · {} · {} / {} records",
                            p.method.label(),
                            p.stage,
                            p.records,
                            p.total_records
                        );
                    }
                    format!(
                        "{} · {} · {} files · {} folders · {} errors",
                        p.method.label(),
                        format_bytes(p.bytes),
                        p.files,
                        p.directories,
                        p.errors
                    )
                })
                .unwrap_or_else(|| "Choose a drive or folder to analyze.".into());
            body = body.child(div().flex_1().p_2().child(status));
        }
        body.child(
            div()
                .h(px(20.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .px_1()
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(self.status.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .child("Logical size · links / offline files skipped"),
                ),
        )
    }
}
impl Drop for DiskUsageView {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.cancel();
        }
    }
}
fn dense_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    button(id, label)
        .h(px(23.))
        .px_2()
        .rounded(px(2.))
        .text_size(px(12.))
}
fn file_icon_key(extension: &str) -> crate::file_type_icons::Key {
    crate::file_type_icons::Key::File(if extension == "(no extension)" {
        String::new()
    } else {
        extension.to_owned()
    })
}
fn data_cell(text: impl Into<SharedString>, width: f32) -> Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .h_full()
        .px_1()
        .text_right()
        .border_r_1()
        .border_color(rgb(LINE))
        .child(text.into())
}
fn extension(name: &std::ffi::OsStr) -> String {
    std::path::Path::new(name)
        .extension()
        .filter(|ext| !ext.is_empty())
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .unwrap_or_else(|| "(no extension)".into())
}
fn node_color(name: &std::ffi::OsStr, directory: bool) -> u32 {
    if directory {
        0x317e67
    } else {
        type_color(&extension(name))
    }
}
fn type_color(extension: &str) -> u32 {
    const COLORS: [u32; 10] = [
        0x467bb5, 0x8b69b4, 0xc18b42, 0x429c9b, 0xb4647c, 0x638c45, 0x6575bd, 0xbb7151, 0x9a8849,
        0x4d9590,
    ];
    let hash = extension.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    });
    COLORS[hash % COLORS.len()]
}
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        if value < 1024. {
            break;
        }
        value /= 1024.;
        unit = next;
    }
    format!("{value:.1} {unit}")
}
