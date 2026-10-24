//! Chart exploration keeps the data tables available as separate views.
use super::*;

impl DiskUsageView {
    pub(super) fn breadcrumbs(&self, cx: &mut Context<Self>) -> AnyElement {
        let scan = &self.results.as_ref().unwrap().scan;
        let mut ancestors = Vec::new();
        let mut current = Some(self.folder);
        while let Some(id) = current {
            ancestors.push(id);
            current = scan.nodes[id].parent;
        }
        ancestors.reverse();
        let mut crumbs = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .overflow_hidden();
        let count = ancestors.len();
        for (index, id) in ancestors.into_iter().enumerate() {
            if count > 6 && index > 0 && index < count - 5 {
                if index == 1 {
                    crumbs = crumbs.child(muted("…"));
                }
                continue;
            }
            if index > 0 {
                crumbs = crumbs.child(muted("›"));
            }
            let name = if id == 0 {
                scan.root_path
                    .file_name()
                    .unwrap_or(scan.root_path.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            } else {
                scan.nodes[id].name.to_string_lossy().into_owned()
            };
            crumbs = crumbs.child(
                dense_button(("disk-crumb", id), name)
                    .max_w(px(160.))
                    .overflow_hidden()
                    .bg(rgb(if id == self.folder { SOFT } else { BG }))
                    .on_click(cx.listener(move |this, _, _, cx| this.focus_folder(id, cx))),
            );
        }
        crumbs.into_any_element()
    }

    fn explore_select(&mut self, id: usize, cx: &mut Context<Self>) {
        self.selected = id;
        if self
            .results
            .as_ref()
            .is_some_and(|r| r.scan.nodes[id].is_directory)
        {
            self.focus_folder(id, cx);
        } else {
            cx.notify();
        }
    }

    fn explore_hover(&mut self, id: Option<usize>, cx: &mut Context<Self>) {
        if self.hovered != id {
            self.hovered = id;
            cx.notify();
        }
    }

    pub(super) fn explore(&self, viewport: Size<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let scan = &self.results.as_ref().unwrap().scan;
        let sidebar_width = if viewport.width >= px(1100.) {
            360.
        } else {
            300.
        };
        let chart_width = (f32::from(viewport.width) - sidebar_width - 22.).max(200.);
        let side = (f32::from(viewport.height) - 250.)
            .max(180.)
            .min(chart_width - 20.);
        let mut chooser = div()
            .h(px(30.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .px_2();
        for (boxes, label) in [(false, "Sunburst"), (true, "Treemap")] {
            chooser = chooser.child(
                dense_button(("disk-chart-mode", boxes as usize), label)
                    .bg(rgb(if boxes == self.explore_boxes {
                        0x293c34
                    } else {
                        SOFT
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.show_explore(boxes, cx))),
            );
        }
        chooser = chooser
            .child(div().flex_1())
            .child(muted("Logical size").text_size(px(11.)));
        let plot = if self.explore_boxes {
            self.treemap(chart_width, side, cx)
        } else {
            crate::disk_sunburst::render(
                scan.clone(),
                self.folder,
                self.selected,
                self.hovered,
                side,
                cx,
                Self::explore_select,
                Self::explore_hover,
            )
        };
        let chart = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(PANEL))
            .child(chooser)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .child(plot),
            )
            .child(
                muted(if self.explore_boxes {
                    "Click a folder to explore · Use the breadcrumb to go back"
                } else {
                    "Click a folder to explore · Click the center to go back"
                })
                .h(px(24.))
                .flex_shrink_0()
                .text_center()
                .text_size(px(11.)),
            );
        let inspect = self.hovered.unwrap_or(self.selected);
        let node = &scan.nodes[inspect];
        let mut sidebar = div()
            .id("disk-explore-sidebar")
            .w(px(sidebar_width))
            .flex_shrink_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(PANEL))
            .border_l_1()
            .border_color(rgb(LINE))
            .on_hover(cx.listener(|this, inside: &bool, _, cx| {
                if !inside {
                    this.explore_hover(None, cx);
                }
            }))
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        muted(if self.hovered.is_some() {
                            "UNDER POINTER"
                        } else {
                            "SELECTED"
                        })
                        .text_size(px(10.)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(self.type_icon(
                                if node.is_directory {
                                    crate::file_type_icons::Key::Folder
                                } else {
                                    file_icon_key(&extension(&node.name))
                                },
                                cx,
                            ))
                            .child(div().flex_1().min_w_0().truncate().child(if inspect == 0 {
                                scan.root_path
                                    .file_name()
                                    .unwrap_or(scan.root_path.as_os_str())
                                    .to_string_lossy()
                                    .into_owned()
                            } else {
                                node.name.to_string_lossy().into_owned()
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(24.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format_bytes(node.bytes)),
                    )
                    .child(
                        muted(format!(
                            "{} files · {} folders",
                            node.files, node.directories
                        ))
                        .text_size(px(11.)),
                    ),
            )
            .child(
                div()
                    .h(px(24.))
                    .flex_shrink_0()
                    .px_2()
                    .border_y_1()
                    .border_color(rgb(LINE))
                    .flex()
                    .items_center()
                    .child(muted("FOLDER CONTENTS").text_size(px(10.)).flex_1())
                    .child(muted("Largest first").text_size(px(10.))),
            );
        let count = scan.nodes[self.folder].children.len();
        sidebar = sidebar.child(
            uniform_list(
                "disk-explore-items",
                count,
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    let scan = &this.results.as_ref().unwrap().scan;
                    range
                        .map(|index| {
                            let id = scan.nodes[this.folder].children[index];
                            let node = &scan.nodes[id];
                            let share =
                                node.bytes as f64 / scan.nodes[this.folder].bytes.max(1) as f64;
                            let color = crate::disk_sunburst::branch_color(id);
                            div()
                                .id(("disk-explore-item", id))
                                .tab_index(0)
                                .h(px(26.))
                                .w_full()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .bg(rgb(if this.selected == id || this.hovered == Some(id) {
                                    0x1f3930
                                } else {
                                    PANEL
                                }))
                                .hover(|s| s.bg(rgb(SOFT)))
                                .cursor_pointer()
                                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                    this.explore_hover(Some(id), cx)
                                }))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.explore_select(id, cx)),
                                )
                                .on_key_down(cx.listener(
                                    move |this, event: &KeyDownEvent, _, cx| {
                                        if event.keystroke.key == "enter" {
                                            this.explore_select(id, cx);
                                            cx.stop_propagation();
                                        }
                                    },
                                ))
                                .child(div().w(px(3.)).h(px(16.)).flex_shrink_0().bg(rgb(color)))
                                .child(this.type_icon(
                                    if node.is_directory {
                                        crate::file_type_icons::Key::Folder
                                    } else {
                                        file_icon_key(&extension(&node.name))
                                    },
                                    cx,
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(node.name.to_string_lossy().into_owned()),
                                )
                                .child(
                                    div()
                                        .w(px(70.))
                                        .flex_shrink_0()
                                        .text_right()
                                        .child(format_bytes(node.bytes)),
                                )
                                .child(
                                    muted(format!("{:.0}%", share * 100.))
                                        .w(px(34.))
                                        .flex_shrink_0()
                                        .text_right(),
                                )
                                .into_any_element()
                        })
                        .collect()
                }),
            )
            .track_scroll(self.explore_scroll.clone())
            .flex_1()
            .min_h_0(),
        );
        if count == 0 {
            sidebar = sidebar.child(muted("This folder is empty.").p_3());
        }
        sidebar = sidebar.child(
            div()
                .h(px(26.))
                .flex_shrink_0()
                .px_2()
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(format!(
                    "{} items · {}",
                    count,
                    format_bytes(scan.nodes[self.folder].bytes)
                )),
        );
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(px(4.))
            .child(chart)
            .child(sidebar)
            .into_any_element()
    }
}
