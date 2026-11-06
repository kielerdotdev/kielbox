//! Local watch-session export receipt; rendering reads cached metadata only.
use super::{ACCENT, Bounds, Context, Div, EditorView, JobProgress, LINE, PANEL, Pixels,
    Preset, Project, Window, button, canvas, completed_export_source, div, label, px, rgb};
use gpui::prelude::*;
use std::{cell::RefCell, path::{Path, PathBuf}, rc::Rc, sync::{Arc, atomic::Ordering}};

#[derive(Default)]
pub(super) struct State {
    pub compatible: bool,
    output: Option<PathBuf>,
    bytes: Option<u64>,
    pub saved_compatible: bool,
    pub saved_bytes: Option<u64>,
    pub bounds: Rc<RefCell<[Option<Bounds<Pixels>>; 3]>>,
}
impl State {
    pub fn started(&mut self, output: PathBuf, compatible: bool) {
        self.compatible = compatible;
        self.output = Some(output);
        self.bytes = None;
        self.clear_receipt();
    }
    pub fn set_size(&mut self, path: &Path, bytes: Option<u64>) {
        if self.output.as_deref() == Some(path) { self.bytes = bytes; }
    }
    pub fn completed(&mut self, path: &Path) {
        self.saved_compatible = self.output.as_deref() == Some(path) && self.compatible;
        self.saved_bytes = if self.output.as_deref() == Some(path) { self.bytes } else { None };
        self.output = None;
        self.bytes = None;
    }
    pub fn clear_receipt(&mut self) { self.saved_bytes = None; self.saved_compatible = false; }
}

pub(super) fn compatible_project(project: &Project) -> (Project, u32) {
    let mut output = project.framed_export();
    (output.width, output.height) = Preset::FullHd.size(&output);
    (output, 8_000_000)
}
fn file_label(path: &Path, bytes: Option<u64>) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let size = match bytes {
        None => "size unavailable".into(),
        Some(bytes) if bytes < 1000 => format!("{bytes} B"),
        Some(bytes) if bytes < 1_000_000 => format!("{:.1} KB", bytes as f64 / 1000.),
        Some(bytes) if bytes < 1_000_000_000 => format!("{:.1} MB", bytes as f64 / 1_000_000.),
        Some(bytes) => format!("{:.2} GB", bytes as f64 / 1_000_000_000.),
    };
    format!("{name} · {size}")
}

impl EditorView {
    fn source_file_label(&self) -> String {
        let Some(path) = &self.recording else { return "Choose a clip to prepare a local MP4".into(); };
        let bytes = self.model.project.assets.iter().find(|asset| &asset.path == path
            || asset.source_stamp.as_ref().is_some_and(|stamp| &stamp.canonical_path == path))
            .and_then(|asset| asset.source_stamp.as_ref()).map(|stamp| stamp.bytes);
        file_label(path, bytes)
    }

    pub(super) fn watch_share_row(&self, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        *self.watch_share.bounds.borrow_mut() = [None; 3];
        let measure = |index: usize| {
            let bounds = self.watch_share.bounds.clone();
            canvas(move |area, _, _| bounds.borrow_mut()[index] = Some(area), |_, _, _, _| {}).absolute().inset_0()
        };
        let mut row = div().id("watch-share-row").relative().h(px(36.)).flex_shrink_0()
            .px_6().border_b_1().border_color(rgb(LINE)).bg(rgb(PANEL))
            .flex().items_center().gap_3().child(measure(0));
        if let Some(job) = &self.export {
            let percent = (job.microseconds.load(Ordering::Relaxed) as f64 / self.export_total_us.max(1.) * 100.).min(100.);
            let status = if job.cancel.load(Ordering::Relaxed) { "Cancelling…".into() }
                else if job.suppressing_audio.load(Ordering::Relaxed) { "Cleaning microphone…".into() }
                else { format!("Preparing {} · {percent:.0}%", if self.watch_share.compatible { "MP4" } else { "export" }) };
            row = row.child(div().relative().flex_1().min_w_0().truncate().child(label(format!("{status} · {}", completed_export_source(&self.export_source_title)))).child(measure(2)))
                .child(button("watch-cancel-export", "Cancel", "Cancel preparing this local file")
                    .relative().h(px(26.)).child(measure(1)).on_click(cx.listener(|this, _, _, cx| {
                        if let Some(job) = &this.export { job.cancel.store(true, Ordering::Relaxed); }
                        cx.notify();
                    })));
        } else if let Some(path) = &self.saved {
            let copy = path.clone();
            let reveal = path.clone();
            let source = self.saved_source_title.as_deref().unwrap_or("Previous clip");
            let ready = if self.watch_share.saved_compatible { "MP4 ready" } else { "Export ready" };
            row = row.child(div().relative().flex_1().min_w_0().truncate()
                .child(label(format!("{ready} · {} · {source}", file_label(path, self.watch_share.saved_bytes))))
                .child(measure(2)))
                .child(button("watch-copy-file", if self.copy_operation != 0 { "Copying…" } else if self.copied { "Copied" } else { "Copy file" }, "Copy this file to paste into a chat")
                    .relative().h(px(26.)).child(measure(1)).on_click(cx.listener(move |this, _, window, cx| {
                        #[cfg(windows)] this.copy_saved_file(copy.clone(), window, cx);
                        #[cfg(not(windows))] { let _ = (&copy, window); this.error = Some("File copying is unavailable on this platform.".into()); cx.notify(); }
                    })))
                .child(button("watch-show-file", "Show folder", "Show the prepared file in its folder").h(px(26.))
                    .on_click(cx.listener(move |this, _, _, cx| this.reveal_output(reveal.clone(), cx))))
                .child(button("watch-dismiss-file", "×", "Dismiss this prepared file receipt").h(px(26.)).px_2()
                    .on_click(cx.listener(|this, _, _, cx| { this.saved = None; this.watch_share.clear_receipt(); cx.notify(); })));
        } else {
            let failed = self.export_detail.is_some();
            let status = if self.export_cancelled { "Preparation cancelled".into() }
                else if failed { "Could not prepare the file".into() } else { self.source_file_label() };
            row = row.child(div().relative().flex_1().min_w_0().truncate().child(label(status)).child(measure(2)))
                .when(failed, |row| row.child(button("watch-export-details", "Details", "View export failure and recovery details").h(px(26.))
                    .on_click(cx.listener(|this, _, window, cx| this.show_export_details(window, cx)))))
                .child(button("watch-prepare-mp4", "Prepare MP4", "Make an H.264/AAC MP4 up to 1080p, then copy the file to paste into a chat")
                    .relative().h(px(26.)).bg(rgb(0x173025)).text_color(rgb(ACCENT))
                    .opacity(if self.ready && !self.loading { 1. } else { 0.35 })
                    .child(measure(1)).on_click(cx.listener(|this, _, _, cx| this.prepare_watch_mp4(cx))));
        }
        row
    }

    pub(super) fn verify_watch_share_layout(&self, window: &Window) -> anyhow::Result<()> {
        let bounds = self.watch_share.bounds.borrow();
        let row = bounds[0].ok_or_else(|| anyhow::anyhow!("Watch share row was not painted"))?;
        let action = bounds[1].ok_or_else(|| anyhow::anyhow!("Watch share action was not painted"))?;
        let status = bounds[2].ok_or_else(|| anyhow::anyhow!("Watch file/status metadata was not painted"))?;
        anyhow::ensure!(row.left() >= px(0.) && row.right() <= window.viewport_size().width
            && row.top() >= px(56.) && row.bottom() <= px(92.), "Watch share row overflow: {row:?}");
        anyhow::ensure!(status.size.width > px(20.) && status.left() >= row.left()
            && status.right() <= action.left() && action.right() <= row.right(), "Watch share text/action overlap: {status:?}, {action:?}");
        Ok(())
    }

    /// Generated status only: never starts an export, filesystem query or clipboard action.
    pub(crate) fn seed_watch_share_status(&mut self, stage: u8, cx: &mut Context<Self>) -> anyhow::Result<()> {
        anyhow::ensure!(self.export_worker.is_none(), "Do not replace a real export worker with a fixture");
        let before = serde_json::to_value(&self.model.project)?;
        self.clip_show = true;
        self.export = None;
        self.saved = None;
        self.export_partial = None;
        self.export_detail = None;
        self.export_cancelled = false;
        self.watch_share.started("fixture-only/share-ready.mp4".into(), true);
        match stage {
            0 => {},
            1 => {
                let progress = Arc::new(JobProgress::default());
                progress.microseconds.store(500_000, Ordering::Relaxed);
                self.export_total_us = 1_000_000.;
                self.export = Some(progress);
                self.export_source_title = "Earlier clip 🎮".into();
            }
            2 => {
                let path = PathBuf::from("fixture-only/share-ready.mp4");
                self.watch_share.set_size(&path, Some(12_345_678));
                self.watch_share.completed(&path);
                self.saved = Some(path);
                self.saved_source_title = Some("Earlier clip 🎮".into());
            }
            3 => self.export_detail = Some("Synthetic preparation failure. No encoder was started.".into()),
            _ => anyhow::bail!("Unknown watch share fixture stage"),
        }
        anyhow::ensure!(serde_json::to_value(&self.model.project)? == before, "Share UI fixture changed project edits");
        cx.notify();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Project, State, compatible_project, file_label};
    use std::path::Path;

    #[test]
    fn compatible_export_uses_a_bounded_clone_without_changing_project_edits() {
        for (width, height, expected) in [(2560, 1440, (1920, 1080)), (1080, 1920, (1080, 1920)), (640, 360, (640, 360))] {
            let source = Project { width, height, ..Project::default() };
            let before = serde_json::to_value(&source).unwrap();
            let (output, bitrate) = compatible_project(&source);
            assert_eq!((output.width, output.height), expected);
            assert_eq!(bitrate, 8_000_000);
            assert_eq!(serde_json::to_value(&source).unwrap(), before);
        }
    }
    #[test]
    fn compatible_receipt_owns_its_size_and_ignores_unrelated_late_metadata() {
        let mut state = State::default();
        state.started("share-one.mp4".into(), true);
        state.set_size(Path::new("different.mp4"), Some(999));
        state.set_size(Path::new("share-one.mp4"), Some(1234));
        state.completed(Path::new("share-one.mp4"));
        assert_eq!(state.saved_bytes, Some(1234));
        assert!(state.saved_compatible);
        state.set_size(Path::new("share-one.mp4"), Some(999));
        assert_eq!(state.saved_bytes, Some(1234));
        state.clear_receipt();
        assert_eq!(state.saved_bytes, None);
        assert!(!state.saved_compatible);
        assert!(file_label(Path::new("share-one.mp4"), Some(12_345_678)).contains("12.3 MB"));
    }
}
