//! The workflow browser presents the actual execution order, including imported steps.
use super::*;
use rcr_toolkit::core::settings::{AfterCapture, AfterUpload, Workflow};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

pub(super) type LayoutProbe = Rc<RefCell<BTreeMap<usize, Bounds<Pixels>>>>;

fn measure(probe: LayoutProbe, key: usize) -> Div {
    div().absolute().size_full().child(
        canvas(
            move |bounds, _, _| {
                probe.borrow_mut().insert(key, bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full(),
    )
}

fn smoke_key(key: &str, shift: bool, window: &mut Window, cx: &mut App) {
    let keystroke = Keystroke {
        modifiers: Modifiers {
            shift,
            ..Default::default()
        },
        key: key.into(),
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
    window.draw(cx).clear();
}

/// Exercise actual painted hit targets without running captures or changing settings.
pub(super) fn smoke(
    view: Entity<Toolkit>,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    let probe = LayoutProbe::default();
    let original = view.read(cx).workflow_index;
    let original_focus = window.focused(cx);
    let count = view
        .read(cx)
        .snapshot
        .as_ref()
        .map_or(0, |s| s.settings.workflows.len());
    view.update(cx, |this, cx| {
        this.workflow_probe = Some(probe.clone());
        cx.notify();
    });
    window.draw(cx).clear();
    for i in 0..count {
        let bounds = probe
            .borrow()
            .get(&(10 + i))
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Workflow {i} not painted"))?;
        let position = bounds.center();
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Default::default(),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                first_mouse: false,
                modifiers: Default::default(),
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                modifiers: Default::default(),
            }),
            cx,
        );
        anyhow::ensure!(
            view.read(cx).workflow_index == i,
            "Workflow {i} selection hit target failed"
        );
        window.draw(cx).clear();
        let measurements = probe.borrow();
        let sidebar = measurements
            .get(&0)
            .ok_or_else(|| anyhow::anyhow!("Missing sidebar"))?;
        let detail = measurements
            .get(&1)
            .ok_or_else(|| anyhow::anyhow!("Missing workflow detail"))?;
        let run = measurements
            .get(&2)
            .ok_or_else(|| anyhow::anyhow!("Missing run button"))?;
        if window.viewport_size().width >= px(860.) {
            anyhow::ensure!(sidebar.right() <= detail.left(), "Workflow columns overlap");
        }
        anyhow::ensure!(
            run.size.width < px(200.) && run.size.width > px(80.),
            "Run control has incorrect width"
        );
        anyhow::ensure!(
            run.right() <= window.viewport_size().width && detail.size.width > px(300.),
            "Workflow detail exceeds viewport"
        );
    }
    // Walk native Tab focus; never activate unrelated run/capture controls.
    // Enter and Space must both reach the same production on_click selection.
    for i in 0..count {
        let focus = view
            .read(cx)
            .workflow_focus
            .get(&i)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Workflow {i} has no focus handle"))?;
        for activation in ["enter", "space"] {
            view.update(cx, |this, cx| {
                this.workflow_index = usize::MAX;
                cx.notify();
            });
            window.draw(cx).clear();
            let mut reached = false;
            for _ in 0..512 {
                smoke_key("tab", false, window, cx);
                if window.focused(cx).as_ref() == Some(&focus) {
                    reached = true;
                    break;
                }
            }
            anyhow::ensure!(reached, "Workflow {i} is unreachable by Tab");
            smoke_key("tab", true, window, cx);
            smoke_key("tab", false, window, cx);
            anyhow::ensure!(
                window.focused(cx).as_ref() == Some(&focus),
                "Workflow {i} reverse Tab traversal lost focus"
            );
            smoke_key(activation, false, window, cx);
            anyhow::ensure!(
                view.read(cx).workflow_index == i,
                "Workflow {i} {activation} activation did not select it"
            );
            anyhow::ensure!(
                window.focused(cx).as_ref() == Some(&focus),
                "Workflow {i} activation moved focus"
            );
        }
    }
    // A pending workflow save/capture must not expose active-looking edit targets.
    // Override only the busy presentation/admission predicate; no worker job runs.
    let (busy_index, before) = {
        let snapshot = view.read(cx).snapshot.as_ref().unwrap();
        let index = snapshot.settings.workflows.iter().position(|workflow| !workflow.input.is_recording() && !workflow.after_capture.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Workflow fixture has no editable actions"))?;
        (index, snapshot.settings.workflows.clone())
    };
    let original_override = view.read(cx).workflow_busy_override;
    probe.borrow_mut().clear();
    view.update(cx, |this, cx| {
        this.workflow_index = busy_index;
        this.workflow_busy_override = Some(true);
        cx.notify();
    });
    window.draw(cx).clear();
    let remove = probe.borrow().get(&1002).copied().ok_or_else(|| anyhow::anyhow!("Disabled workflow Remove control was not painted"))?;
    anyhow::ensure!(remove.bottom() <= window.viewport_size().height, "Disabled workflow control is outside fixture viewport");
    let position = remove.center();
    window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Default::default() }), cx);
    window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { position, button: MouseButton::Left, click_count: 1, first_mouse: false, modifiers: Default::default() }), cx);
    window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { position, button: MouseButton::Left, click_count: 1, modifiers: Default::default() }), cx);
    window.draw(cx).clear();
    anyhow::ensure!(view.read(cx).snapshot.as_ref().unwrap().settings.workflows == before, "Busy workflow edit changed saved settings");
    anyhow::ensure!(!view.read(cx).worker.as_ref().is_some_and(|worker| worker.busy()), "Disabled workflow edit queued a worker job");
    view.update(cx, |this, cx| {
        this.workflow_busy_override = original_override;
        this.workflow_probe = None;
        this.workflow_index = original;
        cx.notify();
    });
    if let Some(focus) = original_focus {
        focus.focus(window);
    }
    window.draw(cx).clear();
    println!(
        "Workflow smoke: {count} sidebar pointer/Tab/Enter/Space selections; reverse Tab, retained focus, separate columns/compact run controls, disabled busy Remove rejects native pointer without settings/job changes"
    );
    Ok(())
}

struct Hint(&'static str);
impl Render for Hint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(SOFT))
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .child(self.0)
    }
}

fn title(workflow: &Workflow) -> String {
    // Do not replace a name the user assigned to a built-in workflow.
    let (original, compact) = match workflow.id.as_str() {
        "capture-region" => ("Capture region, save, copy and upload", "Region capture"),
        "capture-region-edit" => ("Capture region, edit, save and upload", "Region + editor"),
        "capture-fullscreen" => ("Capture screen, save and copy", "Entire desktop"),
        "capture-window" => ("Capture active window, save and upload", "Active window"),
        "capture-monitor" => ("Capture active monitor, save and copy", "Current display"),
        "capture-last-region" => ("Repeat last region, save and upload", "Last region"),
        "record-screen" => ("Record screen and upload", "Screen recording"),
        "record-gif" => ("Record GIF and upload", "GIF recording"),
        "upload-files" => ("Upload files", "Upload files"),
        "upload-clipboard" => ("Upload clipboard contents", "Clipboard upload"),
        _ => return workflow.name.clone(),
    };
    if workflow.name == original {
        compact.into()
    } else {
        workflow.name.clone()
    }
}

fn source(input: InputKind) -> (&'static str, &'static str) {
    match input {
        InputKind::CaptureRegion => ("capture-select", "Select an area of your screen"),
        InputKind::CaptureFullscreen => ("monitor", "Capture your entire desktop"),
        InputKind::CaptureMonitor => ("monitor", "Capture the display under your cursor"),
        InputKind::CaptureWindow => ("grid", "Capture the active window"),
        InputKind::CaptureLastRegion => ("undo", "Capture your previous selection again"),
        InputKind::RecordScreen => ("film", "Record with the RCR capture engine"),
        InputKind::RecordGif => ("film", "Create a GIF from a recording in the editor"),
        InputKind::Files => ("folder", "Choose one or more files to share"),
        InputKind::Clipboard => ("copy", "Share an image, text or files from your clipboard"),
    }
}

fn capture_step(step: AfterCapture) -> (&'static str, &'static str, &'static str) {
    match step {
        AfterCapture::OpenEditor => (
            "capture-pen",
            "Annotate image",
            "Open the editor before continuing",
        ),
        AfterCapture::CopyImageToClipboard => {
            ("copy", "Copy image", "Put the image on your clipboard")
        }
        AfterCapture::SaveToFile => (
            "download",
            "Save to folder",
            "Save using your file naming preferences",
        ),
        AfterCapture::SaveAsDialog => (
            "folder",
            "Choose save location",
            "Ask where to save each file",
        ),
        AfterCapture::PinToScreen => (
            "portrait",
            "Pin to screen",
            "Show the image above other windows",
        ),
        AfterCapture::Ocr => (
            "capture-text",
            "Recognize text",
            "Extract text from the image",
        ),
        AfterCapture::Upload => (
            "landscape",
            "Upload",
            "Share using the configured destination",
        ),
        AfterCapture::DeleteLocalFile => (
            "trash",
            "Delete local file",
            "Remove the saved file after a confirmed upload",
        ),
    }
}

fn upload_step(step: &AfterUpload) -> (&'static str, String) {
    match step {
        AfterUpload::CopyUrl => ("copy", "Copy link".into()),
        AfterUpload::CopyShortUrl => ("copy", "Copy short link".into()),
        AfterUpload::OpenUrl => ("landscape", "Open link in browser".into()),
        AfterUpload::ShortenUrl => ("split", "Shorten link".into()),
        AfterUpload::ShowQrCode => ("grid", "Show QR code".into()),
        AfterUpload::ShowNotification => ("check", "Show notification".into()),
        AfterUpload::RunCommand { program, .. } => ("play", format!("Run {program}")),
    }
}

#[derive(Clone, Copy)]
enum StepEdit {
    Add(AfterCapture),
    Remove(usize),
    Move(usize, usize),
    AddUpload(u8),
    RemoveUpload(usize),
    MoveUpload(usize, usize),
}

fn edit_steps(workflow: &mut Workflow, edit: StepEdit) {
    match edit {
        StepEdit::Add(step) if !workflow.after_capture.contains(&step) => {
            workflow.after_capture.push(step)
        }
        StepEdit::Remove(i) if i < workflow.after_capture.len() => {
            workflow.after_capture.remove(i);
        }
        StepEdit::Move(from, to)
            if from < workflow.after_capture.len() && to < workflow.after_capture.len() =>
        {
            let step = workflow.after_capture.remove(from);
            workflow.after_capture.insert(to, step);
        }
        StepEdit::AddUpload(kind) => {
            let step = match kind {
                0 => AfterUpload::CopyUrl,
                1 => AfterUpload::OpenUrl,
                _ => AfterUpload::ShowNotification,
            };
            if !workflow.after_upload.contains(&step) {
                workflow.after_upload.push(step);
            }
        }
        StepEdit::RemoveUpload(i) if i < workflow.after_upload.len() => {
            workflow.after_upload.remove(i);
        }
        StepEdit::MoveUpload(from, to)
            if from < workflow.after_upload.len() && to < workflow.after_upload.len() =>
        {
            let step = workflow.after_upload.remove(from);
            workflow.after_upload.insert(to, step);
        }
        _ => {}
    }
}

impl Toolkit {
    fn workflow_busy(&self) -> bool {
        self.workflow_busy_override.unwrap_or_else(|| self.worker.as_ref().is_some_and(|worker| worker.busy()))
    }
    fn edit_workflow(&mut self, id: &str, edit: StepEdit, cx: &mut Context<Self>) {
        if self.workflow_busy() {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let mut settings = snapshot.settings.clone();
        let Some(workflow) = settings.workflows.iter_mut().find(|w| w.id == id) else {
            return;
        };
        edit_steps(workflow, edit);
        self.save(settings, cx);
    }

    pub(super) fn workflows(&mut self, narrow: bool, cx: &mut Context<Self>) -> Div {
        let heading = div().flex().items_center().justify_between().gap_3().child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(22.))
                        .font_weight(FontWeight::MEDIUM)
                        .child("Workflows"),
                )
                .child(muted(
                    "Choose a workflow, arrange its actions, then capture or share.",
                )),
        );
        let mut root = div()
            .w_full()
            .max_w(px(1180.))
            .mx_auto()
            .flex()
            .flex_col()
            .gap_5()
            .child(heading);
        let Some(snapshot) = &self.snapshot else {
            return root.child(muted("Loading workflows…"));
        };
        let selected = self
            .workflow_index
            .min(snapshot.settings.workflows.len().saturating_sub(1));
        let mut browser = div()
            .flex()
            .gap_6()
            .items_start()
            .when(narrow, |s| s.flex_col());
        let mut sidebar = div()
            .relative()
            .when_some(self.workflow_probe.clone(), |s, p| s.child(measure(p, 0)))
            .flex()
            .flex_col()
            .gap_4()
            .w(px(240.))
            .flex_shrink_0()
            .when(narrow, |s| s.w_full());
        for (group, label) in [(0, "SCREENSHOTS"), (1, "RECORDING"), (2, "SHARING")] {
            let mut items = div()
                .flex()
                .flex_col()
                .gap_1()
                .when(narrow, |s| s.flex_row().flex_wrap());
            let mut count = 0;
            for (i, workflow) in snapshot.settings.workflows.iter().enumerate() {
                let category = if workflow.input.is_recording() {
                    1
                } else if workflow.input.is_still_capture() {
                    0
                } else {
                    2
                };
                if category != group {
                    continue;
                }
                count += 1;
                let active = selected == i;
                let focus = self
                    .workflow_focus
                    .entry(i)
                    .or_insert_with(|| cx.focus_handle().tab_stop(true))
                    .clone();
                let (glyph, _) = source(workflow.input);
                items = items.child(
                    div()
                        .id(("workflow-choice", i))
                        .track_focus(&focus)
                        .tab_index(0)
                        .relative()
                        .when_some(self.workflow_probe.clone(), |s, p| {
                            s.child(measure(p, 10 + i))
                        })
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(if active { 0x285140 } else { BG }))
                        .focus(|s| s.border_color(rgb(ACCENT)).bg(rgb(SOFT)))
                        .bg(rgb(if active { 0x14271f } else { BG }))
                        .text_color(rgb(if active { TEXT } else { MUTED }))
                        .text_size(px(13.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(SOFT)).text_color(rgb(TEXT)))
                        .child(icon(glyph).text_color(rgb(if active { ACCENT } else { MUTED })))
                        .child(div().flex_1().min_w_0().child(title(workflow)))
                        .when(active, |s| {
                            s.child(div().size(px(5.)).rounded_full().bg(rgb(ACCENT)))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.workflow_index = i;
                            cx.notify();
                        })),
                );
            }
            if count > 0 {
                sidebar = sidebar.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            muted(label)
                                .px_3()
                                .text_size(px(10.))
                                .font_weight(FontWeight::SEMIBOLD),
                        )
                        .child(items),
                );
            }
        }
        browser = browser.child(sidebar);
        let Some(workflow) = snapshot.settings.workflows.get(selected) else {
            return root.child(browser.child(muted(
                "No workflows configured. Import your settings from Destinations.",
            )));
        };
        let (glyph, description) = source(workflow.input);
        let id = workflow.id.clone();
        let recording = workflow.input.is_recording();
        let files = workflow.input == InputKind::Files;
        let busy = self.workflow_busy();
        let mut detail = div()
            .relative()
            .when_some(self.workflow_probe.clone(), |s, p| s.child(measure(p, 1)))
            .flex_1()
            .min_w_0()
            .when(narrow, |s| s.w_full())
            .flex()
            .flex_col()
            .gap_5()
            .when(busy, |s| s.child(muted(if self.status == "Saving settings…" {
                "Saving workflow…"
            } else {
                "Finish or cancel the current operation to change workflow actions."
            })));
        detail = detail.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .pb_5()
                .border_b_1()
                .border_color(rgb(LINE))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(20.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(title(workflow)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(icon(glyph))
                                .child(muted(description)),
                        ),
                )
                .child(
                    button(
                        "run-workflow",
                        if busy {
                            "Working…"
                        } else if recording {
                            "Open recorder"
                        } else if files {
                            "Choose files"
                        } else {
                            "Run workflow"
                        },
                    )
                    .relative()
                    .when_some(self.workflow_probe.clone(), |s, p| s.child(measure(p, 2)))
                    .flex_shrink_0()
                    .gap_2()
                    .bg(rgb(if busy { SOFT } else { ACCENT }))
                    .text_color(rgb(if busy { MUTED } else { BG }))
                    .tab_stop(!busy)
                    .child(icon("play").text_color(rgb(if busy { MUTED } else { BG })))
                    .when(!busy, |s| s.on_click(cx.listener(move |this, _, window, cx| {
                        if this.worker.as_ref().is_some_and(|w| w.busy()) {
                            return;
                        }
                        if recording {
                            cx.emit(ToolkitEvent::Recorder);
                        } else if files {
                            this.choose_files(3, window, cx);
                        } else {
                            let capture = this
                                .snapshot
                                .as_ref()
                                .and_then(|s| s.settings.find_workflow(&id))
                                .is_some_and(|w| w.input.is_still_capture());
                            if this.submit(Action::Workflow(id.clone()), cx) && capture {
                                window.minimize_window();
                            }
                        }
                    }))),
                ),
        );
        if recording {
            detail = detail.child(panel().child("Continue in Capture")
                .child(muted("Set up recording, replay and separate audio tracks in Capture. Trim and export from the video editor.")));
        } else {
            let mut steps = div().flex().flex_col().gap_2().child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .pb_1()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Actions"),
                    )
                    .child(muted("Runs from top to bottom")),
            );
            for (i, step) in workflow.after_capture.iter().copied().enumerate() {
                let (glyph, label, help) = capture_step(step);
                let mut controls = div().flex().items_center().gap_1().flex_shrink_0();
                for (control, (key, text, edit, visible)) in [
                    (
                        "step-up",
                        "↑",
                        StepEdit::Move(i, i.saturating_sub(1)),
                        i > 0,
                    ),
                    (
                        "step-down",
                        "↓",
                        StepEdit::Move(i, i + 1),
                        i + 1 < workflow.after_capture.len(),
                    ),
                    ("step-remove", "×", StepEdit::Remove(i), true),
                ].into_iter().enumerate() {
                    if !visible {
                        continue;
                    }
                    let id = workflow.id.clone();
                    let tip = match key {
                        "step-up" => "Move earlier",
                        "step-down" => "Move later",
                        _ => "Remove action",
                    };
                    controls = controls.child(
                        button((key, i), text)
                            .relative()
                            .when_some(self.workflow_probe.clone(), |s, p| s.child(measure(p, 1000 + i * 10 + control)))
                            .w(px(28.))
                            .px_0()
                            .bg(rgb(PANEL))
                            .opacity(if busy { 0.4 } else { 1. })
                            .tab_stop(!busy)
                            .tooltip(move |_, cx| cx.new(|_| Hint(tip)).into())
                            .when(!busy, |s| s.on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_workflow(&id, edit, cx);
                            }))),
                    );
                }
                steps = steps.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_3()
                        .rounded(px(10.))
                        .bg(rgb(PANEL))
                        .child(muted((i + 1).to_string()).w(px(16.)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(32.))
                                .rounded(px(8.))
                                .bg(rgb(SOFT))
                                .child(icon(glyph)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().text_size(px(13.)).child(label))
                                .child(muted(help)),
                        )
                        .child(controls),
                );
            }
            if workflow.after_capture.is_empty() {
                steps = steps.child(muted("No actions yet. Add one below to get started."));
            }
            let mut add = div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .pt_2()
                .child(muted("Add action"));
            let mut available = false;
            for (i, step) in [
                AfterCapture::OpenEditor,
                AfterCapture::SaveToFile,
                AfterCapture::SaveAsDialog,
                AfterCapture::CopyImageToClipboard,
                AfterCapture::Upload,
            ]
            .into_iter()
            .enumerate()
            {
                if workflow.after_capture.contains(&step) {
                    continue;
                }
                available = true;
                let id = workflow.id.clone();
                add = add.child(
                    button(("add-workflow-step", i), capture_step(step).1)
                        .h(px(28.))
                        .text_size(px(12.))
                        .opacity(if busy { 0.4 } else { 1. })
                        .tab_stop(!busy)
                        .child(icon("plus").size(px(12.)))
                        .when(!busy, |s| s.on_click(cx.listener(move |this, _, _, cx| {
                            this.edit_workflow(&id, StepEdit::Add(step), cx)
                        }))),
                );
            }
            if available {
                steps = steps.child(add);
            }
            detail = detail.child(steps);
            if workflow.uploads() {
                let mut after = div().flex().flex_col().gap_2().child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .child("After upload"),
                        )
                        .child(muted("After a successful upload")),
                );
                for (i, step) in workflow.after_upload.iter().enumerate() {
                    let (glyph, label) = upload_step(step);
                    let mut controls = div().flex().items_center().gap_1().flex_shrink_0();
                    for (key, text, edit, visible, tip) in [
                        (
                            "upload-step-up",
                            "↑",
                            StepEdit::MoveUpload(i, i.saturating_sub(1)),
                            i > 0,
                            "Move earlier",
                        ),
                        (
                            "upload-step-down",
                            "↓",
                            StepEdit::MoveUpload(i, i + 1),
                            i + 1 < workflow.after_upload.len(),
                            "Move later",
                        ),
                        (
                            "upload-step-remove",
                            "×",
                            StepEdit::RemoveUpload(i),
                            true,
                            "Remove follow-up action",
                        ),
                    ] {
                        if !visible {
                            continue;
                        }
                        let id = workflow.id.clone();
                        controls = controls.child(
                            button((key, i), text)
                                .w(px(28.))
                                .px_0()
                                .bg(rgb(PANEL))
                                .opacity(if busy { 0.4 } else { 1. })
                                .tab_stop(!busy)
                                .tooltip(move |_, cx| cx.new(|_| Hint(tip)).into())
                                .when(!busy, |s| s.on_click(cx.listener(move |this, _, _, cx| {
                                    this.edit_workflow(&id, edit, cx)
                                }))),
                        );
                    }
                    after = after.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .py_2()
                            .rounded(px(8.))
                            .bg(rgb(PANEL))
                            .child(muted((i + 1).to_string()).w(px(16.)))
                            .child(icon(glyph))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .truncate()
                                    .child(label),
                            )
                            .child(controls),
                    );
                }
                if workflow.after_upload.is_empty() {
                    after = after.child(muted(
                        "No follow-up actions. Add one to copy the link or open it automatically.",
                    ));
                }
                let mut add = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .pt_2()
                    .child(muted("Add follow-up"));
                let mut available = false;
                for (i, step) in [
                    AfterUpload::CopyUrl,
                    AfterUpload::OpenUrl,
                    AfterUpload::ShowNotification,
                ]
                .into_iter()
                .enumerate()
                {
                    if workflow.after_upload.contains(&step) {
                        continue;
                    }
                    available = true;
                    let id = workflow.id.clone();
                    add = add.child(
                        button(("add-upload-step", i), upload_step(&step).1)
                            .h(px(28.))
                            .text_size(px(12.))
                            .opacity(if busy { 0.4 } else { 1. })
                            .tab_stop(!busy)
                            .child(icon("plus").size(px(12.)))
                            .when(!busy, |s| s.on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_workflow(&id, StepEdit::AddUpload(i as u8), cx)
                            }))),
                    );
                }
                if available {
                    after = after.child(add);
                }
                detail = detail.child(after);
            }
            let destination = if workflow.input.is_still_capture() {
                workflow.destination.image.as_ref().or(snapshot
                    .settings
                    .destinations
                    .image
                    .as_ref())
            } else if workflow.input == InputKind::Files {
                workflow
                    .destination
                    .file
                    .as_ref()
                    .or(snapshot.settings.destinations.file.as_ref())
            } else {
                None
            };
            let mut output = div()
                .flex()
                .flex_col()
                .gap_3()
                .pt_4()
                .border_t_1()
                .border_color(rgb(LINE));
            if workflow.after_capture.contains(&AfterCapture::SaveToFile) {
                output = output.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(icon("folder"))
                        .child(
                            div().flex_1().min_w_0().child(
                                muted(
                                    snapshot
                                        .settings
                                        .general
                                        .resolve_save_dir()
                                        .display()
                                        .to_string(),
                                )
                                .truncate(),
                            ),
                        )
                        .child(
                            button("workflow-save-preferences", "Change folder")
                                .h(px(28.))
                                .text_size(px(12.))
                                .on_click(cx.listener(|this, _, _, cx| this.preferences(cx))),
                        ),
                );
            }
            if workflow.uploads() {
                let label = match destination {
                    Some(name) => format!("Destination: {name}"),
                    None if workflow.input == InputKind::Clipboard => {
                        "Uses the destination for the clipboard content type".into()
                    }
                    None => "Choose an upload destination to share files".into(),
                };
                output = output.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(icon("landscape"))
                        .child(div().flex_1().min_w_0().child(muted(label).truncate()))
                        .child(
                            button("workflow-destinations", "Destinations")
                                .h(px(28.))
                                .text_size(px(12.))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.pane = 2;
                                    cx.notify();
                                })),
                        ),
                );
            }
            detail = detail.child(output);
        }
        browser = browser.child(detail);
        root = root.child(browser);
        root
    }
}

#[cfg(test)]
mod tests {
    use super::{AfterCapture, AfterUpload, StepEdit, Workflow, edit_steps, title};
    use rcr_toolkit::core::settings::builtin_workflows;

    #[test]
    fn compact_titles_preserve_custom_names() {
        let mut workflows = builtin_workflows();
        assert_eq!(title(&workflows[0]), "Region capture");
        workflows[0].name = "My upload preset".into();
        assert_eq!(title(&workflows[0]), "My upload preset");
    }

    #[test]
    fn editing_preserves_imported_steps_and_their_order() {
        let mut workflow = Workflow {
            after_capture: vec![
                AfterCapture::Ocr,
                AfterCapture::SaveAsDialog,
                AfterCapture::Upload,
                AfterCapture::DeleteLocalFile,
            ],
            ..Default::default()
        };
        edit_steps(&mut workflow, StepEdit::Move(1, 0));
        assert_eq!(
            workflow.after_capture,
            vec![
                AfterCapture::SaveAsDialog,
                AfterCapture::Ocr,
                AfterCapture::Upload,
                AfterCapture::DeleteLocalFile
            ]
        );
        edit_steps(&mut workflow, StepEdit::Remove(2));
        edit_steps(
            &mut workflow,
            StepEdit::Add(AfterCapture::CopyImageToClipboard),
        );
        edit_steps(
            &mut workflow,
            StepEdit::Add(AfterCapture::CopyImageToClipboard),
        );
        assert_eq!(
            workflow.after_capture,
            vec![
                AfterCapture::SaveAsDialog,
                AfterCapture::Ocr,
                AfterCapture::DeleteLocalFile,
                AfterCapture::CopyImageToClipboard
            ]
        );
        let before = workflow.clone();
        edit_steps(&mut workflow, StepEdit::Remove(50));
        edit_steps(&mut workflow, StepEdit::Move(0, 50));
        assert_eq!(workflow, before);
    }
    #[test]
    fn follow_up_editing_preserves_custom_commands_and_link_order() {
        let custom = AfterUpload::RunCommand {
            program: "my-tool.exe".into(),
            args: vec!["{url}".into()],
        };
        let mut workflow = Workflow {
            after_upload: vec![
                AfterUpload::ShortenUrl,
                AfterUpload::CopyUrl,
                custom.clone(),
            ],
            ..Default::default()
        };
        edit_steps(&mut workflow, StepEdit::AddUpload(1));
        edit_steps(&mut workflow, StepEdit::AddUpload(1));
        edit_steps(&mut workflow, StepEdit::MoveUpload(3, 2));
        assert_eq!(
            workflow.after_upload,
            vec![
                AfterUpload::ShortenUrl,
                AfterUpload::CopyUrl,
                AfterUpload::OpenUrl,
                custom
            ]
        );
        edit_steps(&mut workflow, StepEdit::RemoveUpload(1));
        assert_eq!(workflow.after_upload.len(), 3);
        let before = workflow.clone();
        edit_steps(&mut workflow, StepEdit::MoveUpload(0, 30));
        edit_steps(&mut workflow, StepEdit::RemoveUpload(30));
        assert_eq!(workflow, before);
    }
}
