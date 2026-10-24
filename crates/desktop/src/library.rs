use crate::{Studio, recordings, theme::*};
#[path = "library_import.rs"]
pub(crate) mod import;
#[path = "library_removal.rs"]
mod removal;
use gpui::{prelude::*, *};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, mpsc, atomic::{AtomicBool, AtomicU64, Ordering}},
    time::{SystemTime, UNIX_EPOCH},
};

// Recipe version covers dimensions, pixel layout and sampling time. Cache entries are
// disposable derivatives; originals and project files are never modified.
const THUMB_MAGIC: &[u8; 8] = b"RCRTHM02";
const THUMB_BUDGET: u64 = 32 * 1024 * 1024;
const THUMB_MAX_BYTES: usize = 30 + 512 + 320 * 180 * 4;
const IMAGE_THUMB_DECODE_BUDGET: u64 = 128 * 1024 * 1024;
const IMAGE_THUMB_MAX_AXIS: u32 = 32768;

fn image_thumb_layout_allowed(width: u32, height: u32, bytes_per_pixel: u8, total: u64) -> bool {
    width > 0 && height > 0 && width <= IMAGE_THUMB_MAX_AXIS && height <= IMAGE_THUMB_MAX_AXIS
        && bytes_per_pixel > 0
        && u64::from(width).checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(u64::from(bytes_per_pixel))) == Some(total)
        && total <= IMAGE_THUMB_DECODE_BUDGET
}

fn load_image_thumbnail(path: &Path) -> Option<(image::RgbaImage, String)> {
    use image::ImageDecoder;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(IMAGE_THUMB_MAX_AXIS);
    limits.max_image_height = Some(IMAGE_THUMB_MAX_AXIS);
    limits.max_alloc = Some(IMAGE_THUMB_DECODE_BUDGET);
    let mut reader = image::ImageReader::open(path).ok()?;
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    let total = decoder.total_bytes();
    // Check the actual decoder output before DynamicImage allocates its pixels.
    // Request the remaining workspace budget where the codec supports it. That
    // limit is best-effort (PNG cannot update its internal limit after opening);
    // the decoded output size check itself is strict.
    if !image_thumb_layout_allowed(width, height, decoder.color_type().bytes_per_pixel(), total) {
        return None;
    }
    limits.max_alloc = Some(IMAGE_THUMB_DECODE_BUDGET - total);
    decoder.set_limits(limits).ok()?;
    let image = image::DynamicImage::from_decoder(decoder).ok()?;
    Some((image.thumbnail(320, 180).to_rgba8(), format!("{width} × {height}")))
}
pub fn thumbnail_cache_directory() -> PathBuf {
    std::env::var_os("RCR_THUMBNAIL_CACHE_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rcr-library-thumbnails-v2"))
}
fn thumbnail_cache_path(source: &Path) -> Option<PathBuf> {
    let path = source.canonicalize().ok()?;
    let metadata = std::fs::metadata(&path).ok()?;
    let modified = metadata.modified().ok()?;
    let (before_epoch, stamp) = match modified.duration_since(UNIX_EPOCH) {
        Ok(stamp) => (0u8, stamp),
        Err(before) => (1u8, before.duration()),
    };
    let mut digest = Sha256::new();
    digest.update(THUMB_MAGIC);
    digest.update(path.as_os_str().as_encoded_bytes());
    digest.update([0]);
    digest.update(metadata.len().to_le_bytes());
    digest.update([before_epoch]);
    digest.update(stamp.as_secs().to_le_bytes());
    digest.update(stamp.subsec_nanos().to_le_bytes());
    if let Ok(created) = metadata.created() {
        let (before, stamp) = match created.duration_since(UNIX_EPOCH) {
            Ok(stamp) => (0u8, stamp),
            Err(before) => (1u8, before.duration()),
        };
        digest.update([1, before]);
        digest.update(stamp.as_secs().to_le_bytes());
        digest.update(stamp.subsec_nanos().to_le_bytes());
    } else {
        digest.update([0]);
    }
    Some(thumbnail_cache_directory().join(format!("{:x}.bgra", digest.finalize())))
}
fn decode_thumbnail(bytes: &[u8]) -> Option<(image::RgbaImage, f64, String)> {
    if !(30..=THUMB_MAX_BYTES).contains(&bytes.len()) || &bytes[..8] != THUMB_MAGIC {
        return None;
    }
    let width = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
    let duration = f64::from_le_bytes(bytes[16..24].try_into().ok()?);
    let detail_len = u16::from_le_bytes(bytes[24..26].try_into().ok()?) as usize;
    let pixels_len = u32::from_le_bytes(bytes[26..30].try_into().ok()?) as usize;
    if !(1..=320).contains(&width)
        || !(1..=180).contains(&height)
        || !duration.is_finite()
        || duration < 0.
        || detail_len > 512
        || pixels_len != width as usize * height as usize * 4
        || bytes.len() != 30 + detail_len + pixels_len
    {
        return None;
    }
    let detail = std::str::from_utf8(&bytes[30..30 + detail_len])
        .ok()?
        .to_owned();
    Some((
        image::RgbaImage::from_raw(width, height, bytes[30 + detail_len..].to_vec())?,
        duration,
        detail,
    ))
}
fn load_thumbnail(cache: &Path, source: PathBuf) -> Option<Thumbnail> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(cache)
        .ok()?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(THUMB_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let (buffer, duration, detail) = decode_thumbnail(&bytes)?;
    // The cache's own modification time records recency, independently of source mtime.
    let _ = file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()));
    Some(Thumbnail {
        path: source,
        image: Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))),
        duration,
        detail,
    })
}
fn cache_file_name(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "bgra")
        && path.file_stem().is_some_and(|s| {
            let name = s.to_string_lossy();
            name.len() == 64 && name.bytes().all(|c| c.is_ascii_hexdigit())
        })
}
fn prune_thumbnails(folder: &Path) {
    if std::fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !cache_file_name(&path) {
                return None;
            }
            if entry.file_type().ok()?.is_symlink() {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some((metadata.modified().ok(), metadata.len(), path))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut bytes: u64 = files.iter().map(|(_, bytes, _)| *bytes).sum();
    for (_, size, path) in files {
        if bytes <= THUMB_BUDGET {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            bytes = bytes.saturating_sub(size);
        }
    }
}
fn save_thumbnail(cache: &Path, buffer: &image::RgbaImage, duration: f64, detail: &str) {
    let Some(folder) = cache.parent() else {
        return;
    };
    if buffer.width() > 320 || buffer.height() > 180 || detail.len() > 512 {
        return;
    }
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(folder)?;
        let mut file = tempfile::NamedTempFile::new_in(folder)?;
        file.write_all(THUMB_MAGIC)?;
        file.write_all(&buffer.width().to_le_bytes())?;
        file.write_all(&buffer.height().to_le_bytes())?;
        file.write_all(&duration.to_le_bytes())?;
        file.write_all(&(detail.len() as u16).to_le_bytes())?;
        file.write_all(&(buffer.len() as u32).to_le_bytes())?;
        file.write_all(detail.as_bytes())?;
        file.write_all(buffer.as_raw())?;
        file.flush()?;
        file.persist(cache).map_err(|error| error.error)?;
        Ok(())
    };
    match write() {
        Ok(()) => prune_thumbnails(folder),
        Err(error) => {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                eprintln!(
                    "Library thumbnail cache unavailable at {}: {error}",
                    folder.display()
                );
            }
        }
    }
}

fn file_size(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.0} KB", bytes as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", bytes as f64 / 1e6),
        _ => format!("{:.2} GB", bytes as f64 / 1e9),
    }
}

#[derive(Default)]
struct FileInfo {
    available: bool,
    local: bool,
    title: String,
    bytes: u64,
    modified: Option<std::time::SystemTime>,
    created: Option<std::time::SystemTime>,
    search: String,
}
impl FileInfo {
    fn preserve_last_known(&mut self, old: &Self) {
        if !self.available {
            self.title.clone_from(&old.title);
            self.search.clone_from(&old.search);
            self.bytes = old.bytes;
            self.created = old.created;
            self.modified = old.modified;
        }
    }
}

#[derive(Default)]
struct FolderOpenAdmission(Arc<AtomicBool>);
struct FolderOpenLease(Arc<AtomicBool>);
impl FolderOpenAdmission {
    fn pending(&self) -> bool { self.0.load(Ordering::Acquire) }
    fn acquire(&self) -> Option<FolderOpenLease> {
        self.0.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok()
            .map(|_| FolderOpenLease(self.0.clone()))
    }
}
impl Drop for FolderOpenLease {
    fn drop(&mut self) { self.0.store(false, Ordering::Release); }
}
enum Task {
    Thumbnail(PathBuf),
    Catalog(Vec<PathBuf>),
}
struct WorkTask { revision: u64, task: Task }
struct WorkResult { revision: u64, item: Option<ResultItem> }

#[derive(Clone, Default)]
struct LibraryWork {
    stopped: Arc<AtomicBool>,
    revision: Arc<AtomicU64>,
}
impl LibraryWork {
    fn current(&self) -> u64 { self.revision.load(Ordering::Acquire) }
    fn cancelled(&self, revision: u64) -> bool {
        self.stopped.load(Ordering::Acquire) || self.current() != revision
    }
    fn advance(&self) { self.revision.fetch_add(1, Ordering::AcqRel); }
    fn stop(&self) { self.stopped.store(true, Ordering::Release); }
}

fn join_library_worker(worker: std::thread::JoinHandle<()>) -> std::io::Result<std::thread::JoinHandle<bool>> {
    std::thread::Builder::new().name("rcr-library-join".into())
        .spawn(move || {
            let joined = worker.join().is_ok();
            if !joined { eprintln!("Library thumbnail worker panicked"); }
            joined
        })
}
enum ResultItem {
    Thumbnail(Thumbnail),
    Catalog(Vec<PathBuf>, HashMap<PathBuf, FileInfo>),
}
struct Hint(SharedString);
impl Render for Hint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(SOFT))
            .text_sm()
            .child(self.0.clone())
    }
}
fn hint(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |_, cx| cx.new(|_| Hint(text.clone())).into()
}
struct Thumbnail {
    path: PathBuf,
    image: Option<Arc<RenderImage>>,
    duration: f64,
    detail: String,
}
struct KeyboardSmokeRestore {
    selection: HashSet<PathBuf>,
    search: String,
    view: (usize, bool, usize, usize, usize),
    view_key: String,
    names: std::collections::BTreeMap<PathBuf, String>,
    focus: Option<FocusHandle>,
    capture_focus: Option<FocusHandle>,
    range: SelectionRange,
    selection_probe: Option<SelectionProbe>,
    keyboard_return: Option<PathBuf>,
}
impl KeyboardSmokeRestore {
    fn restore(self, studio: &Entity<Studio>, window: &mut Window, cx: &mut App) {
        studio.update(cx, |this, cx| {
            this.name_dialog = None;
            this.name_restore = None;
            this.library.selected = self.selection;
            this.library.selection_range = self.range;
            this.library.selection_probe = self.selection_probe;
            this.library.keyboard_return = self.keyboard_return;
            (
                this.library.filter,
                this.library.favorites_only,
                this.library.sort,
                this.library.page,
                this.tab,
            ) = self.view;
            this.library.view_key = self.view_key;
            this.search
                .update(cx, |search, cx| search.set_value(self.search, cx));
            cx.notify();
        });
        window.draw(cx).clear();
        if let Some(focus) = self.focus {
            focus.focus(window);
        }
    }
}
type SelectionProbe = Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>;

fn keyboard_return_index(page: &[PathBuf], previous: Option<&Path>) -> Option<usize> {
    previous.and_then(|path| page.iter().position(|item| item == path))
        .or_else(|| (!page.is_empty()).then_some(0))
}

fn selection_measure(probe: SelectionProbe, key: usize) -> Div {
    div().absolute().size_full().child(canvas(move |bounds, _, _| {
        probe.borrow_mut().insert(key, bounds);
    }, |_, _, _, _| {}).absolute().size_full())
}

/// A range never crosses the currently displayed page or borrows an anchor from
/// a different ordering. Batch selections outside that page remain untouched.
#[derive(Clone, Default)]
struct SelectionRange {
    page: Vec<PathBuf>,
    anchor: Option<PathBuf>,
}
impl SelectionRange {
    fn sync(&mut self, page: &[PathBuf]) {
        if self.page != page {
            self.page = page.to_vec();
            self.anchor = None;
        }
    }
    fn apply(&mut self, selected: &mut HashSet<PathBuf>, path: &Path, extend: bool) {
        let Some(end) = self.page.iter().position(|item| item == path) else { return; };
        if extend {
            if let Some(start) = self.anchor.as_ref()
                .and_then(|anchor| self.page.iter().position(|item| item == anchor)) {
                selected.extend(self.page[start.min(end)..=start.max(end)].iter().cloned());
                return;
            }
            selected.insert(path.to_path_buf());
        } else if !selected.remove(path) {
            selected.insert(path.to_path_buf());
        }
        self.anchor = Some(path.to_path_buf());
    }
}
#[derive(Default)]
struct FileMembership {
    paths: Option<Vec<PathBuf>>,
}
impl FileMembership {
    fn reconcile<T>(&mut self, files: &[PathBuf], selected: &mut HashSet<PathBuf>,
        focus: &mut HashMap<PathBuf, T>) -> bool {
        if self.paths.as_deref() == Some(files) { return false; }
        let present: HashSet<_> = files.iter().collect();
        selected.retain(|path| present.contains(path));
        focus.retain(|path, _| present.contains(path));
        self.paths = Some(files.to_vec());
        true
    }
}

pub struct Library {
    images: HashMap<PathBuf, Thumbnail>,
    pending: HashSet<PathBuf>,
    order: VecDeque<PathBuf>,
    tasks: Option<mpsc::SyncSender<WorkTask>>,
    results: Option<mpsc::Receiver<WorkResult>>,
    worker: Option<std::thread::JoinHandle<()>>,
    work: LibraryWork,
    catalog: HashMap<PathBuf, FileInfo>,
    catalog_paths: Vec<PathBuf>,
    catalog_pending: bool,
    catalog_dirty: bool,
    file_membership: FileMembership,
    view_key: String,
    removed: removal::RemovalUndo,
    import_guard: import::ImportGuard,
    removal_pending: bool,
    removal_ref_updates: BTreeSet<PathBuf>,
    removal_revision: u64,
    removal_intents: HashMap<PathBuf, u64>,
    pub page: usize,
    pub filter: usize,
    pub favorites_only: bool,
    pub sort: usize,
    pub selected: HashSet<PathBuf>,
    selection_range: SelectionRange,
    selection_probe: Option<SelectionProbe>,
    card_focus: HashMap<PathBuf, FocusHandle>,
    card_scroll: ScrollHandle,
    keyboard_return: Option<PathBuf>,
    keyboard_smoke: Option<KeyboardSmokeRestore>,
    pub(crate) root_scan_error: Option<recordings::RootScanError>,
    scan_preview: Option<bool>,
    folder_open: FolderOpenAdmission,
}
impl Library {
    pub(super) fn observe_navigation(&mut self, tab: usize) {
        self.import_guard.observe_navigation(tab);
    }
    pub(super) fn invalidate_import_navigation(&mut self) {
        self.import_guard.leave_media();
    }
    /// Exercise the painted Library through GPUI's native input routing. No OS input,
    /// preference writes, renames, or capture-file changes are performed by this probe.
    pub fn smoke_keyboard(
        studio: Entity<Studio>,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<bool> {
        use anyhow::{Context, ensure};
        fn key(window: &mut Window, cx: &mut App, name: &str, control: bool, shift: bool) {
            let keystroke = Keystroke {
                key: name.into(),
                key_char: None,
                modifiers: Modifiers {
                    control,
                    shift,
                    ..Default::default()
                },
            };
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                }),
                cx,
            );
            // GPUI activates focused controls on key release, including Space/Enter.
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
            window.draw(cx).clear();
        }
        fn click(window: &mut Window, cx: &mut App, bounds: Bounds<Pixels>, control: bool, shift: bool) {
            let position = bounds.center();
            let modifiers = Modifiers { control, shift, ..Default::default() };
            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), cx);
            window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left,
                position, modifiers, click_count: 1, first_mouse: false }), cx);
            window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left,
                position, modifiers, click_count: 1 }), cx);
            window.draw(cx).clear();
        }
        window.draw(cx).clear();
        // Entity events are delivered after the outer App update ends. Escape emits
        // NameEvent::Cancelled, so verify its subscriber on the next probe tick.
        if let Some(restore) = studio.update(cx, |this, _| this.library.keyboard_smoke.take()) {
            let result = (|| -> anyhow::Result<()> {
                ensure!(
                    studio.read(cx).name_dialog.is_none(),
                    "Escape did not cancel rename"
                );
                ensure!(
                    restore
                        .capture_focus
                        .as_ref()
                        .is_some_and(|focus| focus.is_focused(window)),
                    "Cancelling rename did not restore capture focus"
                );
                ensure!(
                    studio.read(cx).preferences.display_names == restore.names,
                    "Cancelled rename changed display names"
                );
                Ok(())
            })();
            restore.restore(&studio, window, cx);
            result?;
            println!(
                "Library keyboard smoke: pointer Ctrl+click, forward/reverse Shift ranges, missing-anchor selection, arrows/Home/End focus, F6 search/capture return and filtered fallback, F5 query/selection retention, Space selection, page Ctrl+A, search isolation, Escape, F2, modal Tab/Shift+Tab containment and cancellation passed"
            );
            return Ok(true);
        }
        let state = studio.read(cx);
        if state.library_scan.is_pending()
            || state.library.catalog_pending
            || state.library.catalog_dirty
            || state.library.catalog_paths != state.files
        {
            return Ok(false);
        }
        ensure!(
            state.files.len() >= 2,
            "Library keyboard smoke requires at least two captures"
        );
        let mut restore = KeyboardSmokeRestore {
            selection: state.library.selected.clone(),
            search: state.search.read(cx).value(),
            view: (
                state.library.filter,
                state.library.favorites_only,
                state.library.sort,
                state.library.page,
                state.tab,
            ),
            view_key: state.library.view_key.clone(),
            names: state.preferences.display_names.clone(),
            focus: window.focused(cx),
            capture_focus: None,
            range: state.library.selection_range.clone(),
            selection_probe: state.library.selection_probe.clone(),
            keyboard_return: state.library.keyboard_return.clone(),
        };
        let selection_probe = SelectionProbe::default();
        studio.update(cx, |this, cx| {
            this.tab = 1;
            this.library.filter = 0;
            this.library.favorites_only = false;
            this.library.sort = 0;
            this.library.page = 0;
            this.library.selected.clear();
            this.library.selection_range.anchor = None;
            this.library.selection_probe = Some(selection_probe.clone());
            this.search
                .update(cx, |search, cx| search.set_value("", cx));
            cx.notify();
        });
        window.draw(cx).clear();
        let result = (|| -> anyhow::Result<FocusHandle> {
            let state = studio.read(cx);
            let mut paths = state.files.clone();
            paths.sort_by(|a, b| {
                let a_stamp = state.library.catalog.get(a).and_then(|i| i.modified);
                let b_stamp = state.library.catalog.get(b).and_then(|i| i.modified);
                b_stamp.cmp(&a_stamp).then_with(|| a.cmp(b))
            });
            paths.truncate(24);
            let focus = paths
                .iter()
                .map(|path| {
                    state
                        .library
                        .card_focus
                        .get(path)
                        .cloned()
                        .context("Library card has not been painted")
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let search = state.search.clone();
            let measured = |key| selection_probe.borrow().get(&key).copied()
                .with_context(|| format!("Library selection target {key} was not painted"));
            click(window, cx, measured(0)?, true, false);
            ensure!(studio.read(cx).library.selected == HashSet::from([paths[0].clone()]),
                "Ctrl+click did not select only the first capture");
            click(window, cx, measured(3)?, false, true);
            ensure!(studio.read(cx).library.selected == HashSet::from([paths[0].clone(), paths[1].clone()]),
                "Shift+checkbox did not add the inclusive forward range");
            click(window, cx, measured(2)?, true, false);
            ensure!(!studio.read(cx).library.selected.contains(&paths[1]), "Ctrl+click did not toggle a capture off");
            click(window, cx, measured(0)?, false, true);
            ensure!(studio.read(cx).library.selected == HashSet::from([paths[0].clone(), paths[1].clone()]),
                "Shift+card did not add the inclusive reverse range");
            ensure!(studio.read(cx).tab == 1, "A modified pointer click opened the editor");
            studio.update(cx, |this, cx| {
                this.library.selected.clear();
                this.library.selection_range.anchor = None;
                cx.notify();
            });
            window.draw(cx).clear();
            click(window, cx, measured(1)?, false, true);
            ensure!(studio.read(cx).library.selected == HashSet::from([paths[0].clone()]),
                "Shift+click without an anchor did not select the clicked capture");
            ensure!(studio.read(cx).tab == 1, "Missing-anchor Shift+click opened the editor");
            studio.update(cx, |this, cx| {
                this.library.selected.clear();
                this.library.selection_range.anchor = None;
                cx.notify();
            });
            window.draw(cx).clear();
            // The first tab stop is the app navigation, outside the Library subtree.
            window.blur();
            window.focus_next();
            window.draw(cx).clear();
            key(window, cx, "f", true, false);
            ensure!(
                search.read(cx).focus_handle(cx).is_focused(window),
                "Ctrl+F from app navigation did not focus Library search"
            );
            focus[0].focus(window);
            window.draw(cx).clear();
            key(window, cx, "right", false, false);
            ensure!(
                focus[1].is_focused(window),
                "Right did not focus the next capture"
            );
            key(window, cx, "left", false, false);
            ensure!(
                focus[0].is_focused(window),
                "Left did not focus the previous capture"
            );
            key(window, cx, "end", false, false);
            ensure!(
                focus.last().unwrap().is_focused(window),
                "End did not focus the last capture"
            );
            key(window, cx, "home", false, false);
            ensure!(
                focus[0].is_focused(window),
                "Home did not focus the first capture"
            );
            let columns = if studio.read(cx).preferences.list_view {
                1
            } else {
                ((f32::from(window.viewport_size().width) - 220.) / 280.)
                    .floor()
                    .max(1.) as usize
            };
            let next_row = columns.min(focus.len() - 1);
            key(window, cx, "down", false, false);
            ensure!(
                focus[next_row].is_focused(window),
                "Down did not move by one grid row"
            );
            key(window, cx, "up", false, false);
            ensure!(
                focus[0].is_focused(window),
                "Up did not return to the first row"
            );
            focus[1].focus(window);
            let selection_before_f6 = studio.read(cx).library.selected.clone();
            key(window, cx, "f6", false, false);
            ensure!(search.read(cx).focus_handle(cx).is_focused(window), "F6 did not reach Library search");
            key(window, cx, "f6", false, true);
            ensure!(focus[1].is_focused(window), "Shift+F6 did not return to the exact previous capture");
            ensure!(studio.read(cx).library.selected == selection_before_f6, "F6 changed capture selection");
            key(window, cx, "f6", false, false);
            studio.update(cx, |this, cx| {
                this.library.filter = kind(&paths[0]);
                this.library.selected.insert(paths[1].clone());
                cx.notify();
            });
            window.draw(cx).clear();
            key(window, cx, "f6", false, false);
            ensure!(focus[0].is_focused(window), "F6 retained an old return target after the filter changed");
            key(window, cx, "f6", false, false);
            let query = paths[0].file_name().context("Owned capture has no filename")?.to_string_lossy().into_owned();
            search.update(cx, |input, cx| input.set_value(query.clone(), cx));
            window.draw(cx).clear();
            let filtered_selection = studio.read(cx).library.selected.clone();
            key(window, cx, "f6", false, false);
            ensure!(focus[0].is_focused(window), "F6 did not fall back to the first search result");
            ensure!(search.read(cx).value() == query, "F6 changed the search query");
            ensure!(studio.read(cx).library.selected == filtered_selection, "F6 discarded a selection outside the results");
            key(window, cx, "f6", false, false);
            key(window, cx, "f5", false, false);
            ensure!(studio.read(cx).library_scan.is_pending(), "F5 did not request an asynchronous refresh");
            ensure!(search.read(cx).value() == query, "F5 cleared the search query");
            ensure!(studio.read(cx).library.selected == filtered_selection, "F5 changed capture selection");
            ensure!(search.read(cx).focus_handle(cx).is_focused(window), "F5 moved focus away from search");
            studio.update(cx, |this, cx| {
                this.library.filter = 0;
                this.library.selected.clear();
                this.search.update(cx, |input, cx| input.set_value("", cx));
                cx.notify();
            });
            window.draw(cx).clear();
            focus[0].focus(window);
            key(window, cx, "home", false, false);
            key(window, cx, "space", false, false);
            ensure!(
                studio.read(cx).tab == 1,
                "Space opened a capture instead of selecting it"
            );
            ensure!(
                studio.read(cx).library.selected.contains(&paths[0]),
                "Space did not select the capture"
            );
            key(window, cx, "a", true, false);
            ensure!(
                studio.read(cx).library.selected.len() == paths.len(),
                "Ctrl+A did not select the visible page"
            );
            let selected_page = studio.read(cx).library.selected.clone();
            key(window, cx, "f", true, false);
            ensure!(
                search.read(cx).focus_handle(cx).is_focused(window),
                "Ctrl+F did not focus search"
            );
            search.update(cx, |input, cx| input.set_value("keyboard-smoke", cx));
            window.draw(cx).clear();
            key(window, cx, "a", true, false);
            key(window, cx, "backspace", false, false);
            ensure!(
                search.read(cx).value().is_empty(),
                "Ctrl+A did not select search text"
            );
            ensure!(
                studio.read(cx).library.selected == selected_page,
                "Search Ctrl+A changed capture selection"
            );
            key(window, cx, "escape", false, false);
            ensure!(
                focus[0].is_focused(window),
                "Escape from search did not restore first-card focus"
            );
            key(window, cx, "escape", false, false);
            ensure!(
                studio.read(cx).library.selected.is_empty(),
                "Escape did not clear capture selection"
            );
            key(window, cx, "f2", false, false);
            ensure!(
                studio.read(cx).name_dialog.is_some(),
                "F2 did not open Rename capture"
            );
            let dialog_input = window.focused(cx).context("Rename input has no focus")?;
            ensure!(
                !focus.iter().any(|handle| handle.is_focused(window)),
                "Rename left a Library card focused"
            );
            key(window, cx, "f", true, false);
            ensure!(
                dialog_input.is_focused(window),
                "Ctrl+F escaped the rename modal"
            );
            key(window, cx, "tab", false, false);
            let cancel = window.focused(cx).context("Rename Cancel has no focus")?;
            ensure!(cancel != dialog_input, "Tab did not leave rename input");
            key(window, cx, "tab", false, false);
            let save = window.focused(cx).context("Rename Save has no focus")?;
            ensure!(
                save != cancel && save != dialog_input,
                "Rename tab order did not reach Save"
            );
            key(window, cx, "tab", false, false);
            ensure!(
                dialog_input.is_focused(window),
                "Tab escaped the rename modal"
            );
            key(window, cx, "tab", false, true);
            ensure!(
                save.is_focused(window),
                "Shift+Tab escaped the rename modal"
            );
            key(window, cx, "escape", false, false);
            Ok(focus[0].clone())
        })();
        match result {
            Ok(focus) => {
                restore.capture_focus = Some(focus);
                studio.update(cx, |this, _| this.library.keyboard_smoke = Some(restore));
                Ok(false)
            }
            Err(error) => {
                restore.restore(&studio, window, cx);
                Err(error)
            }
        }
    }

    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel::<WorkTask>(1);
        let (done, results) = mpsc::sync_channel(2);
        let work = LibraryWork::default();
        let worker_work = work.clone();
        let worker = std::thread::Builder::new()
            .name("rcr-library-thumbnails".into())
            .spawn(move || {
                while let Ok(WorkTask { revision, task }) = rx.recv() {
                    if worker_work.stopped.load(Ordering::Acquire) { break; }
                    let cancelled = || worker_work.cancelled(revision);
                    // Even discarded tasks acknowledge progress: a newer catalog
                    // can retry a previously full task slot without a repaint loop.
                    let complete = |item| done.send(WorkResult { revision, item: if cancelled() { None } else { item } });
                    if cancelled() {
                        if complete(None).is_err() { break; }
                        continue;
                    }
                    let path = match task {
                        Task::Thumbnail(path) => path,
                        Task::Catalog(paths) => {
                            let root = recordings::root();
                            let root = root.canonicalize().unwrap_or(root);
                            let catalog = paths
                                .iter()
                                .take_while(|_| !cancelled())
                                .map(|path| {
                                    let metadata = std::fs::metadata(path).ok();
                                    let available = metadata
                                        .as_ref()
                                        .is_some_and(|metadata| metadata.is_file());
                                    let title = if available {
                                        recordings::title(path)
                                    } else {
                                        path.file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                            .into_owned()
                                    };
                                    let search = format!(
                                        "{} {}",
                                        title,
                                        path.file_name().unwrap_or_default().to_string_lossy()
                                    )
                                    .to_lowercase();
                                    (
                                        path.clone(),
                                        FileInfo {
                                            available,
                                            local: removal::under_root(path, &root),
                                            title,
                                            search,
                                            bytes: metadata.as_ref().map_or(0, |m| m.len()),
                                            created: metadata
                                                .as_ref()
                                                .and_then(|m| m.created().ok()),
                                            modified: metadata.and_then(|m| m.modified().ok()),
                                        },
                                    )
                                })
                                .collect();
                            if complete(Some(ResultItem::Catalog(paths, catalog))).is_err() {
                                break;
                            }
                            continue;
                        }
                    };
                    let cache = thumbnail_cache_path(&path);
                    if cancelled() {
                        if complete(None).is_err() { break; }
                        continue;
                    }
                    if let Some(thumbnail) = cache
                        .as_ref()
                        .and_then(|cache| load_thumbnail(cache, path.clone()))
                    {
                        if complete(Some(ResultItem::Thumbnail(thumbnail))).is_err() {
                            break;
                        }
                        continue;
                    }
                    if cancelled() {
                        if complete(None).is_err() { break; }
                        continue;
                    }
                    if recordings::is_image(&path) {
                        let source = load_image_thumbnail(&path);
                        if cancelled() {
                            if complete(None).is_err() { break; }
                            continue;
                        }
                        let detail = source.as_ref().map_or_else(String::new, |(_, detail)| detail.clone());
                        let image = source
                            .map(|(image, _)| image)
                            .map(|mut buffer| {
                                for p in buffer.pixels_mut() {
                                    p.0.swap(0, 2);
                                }
                                if let Some(cache) = &cache {
                                    save_thumbnail(cache, &buffer, 0., &detail);
                                }
                                Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
                            });
                        if complete(Some(ResultItem::Thumbnail(Thumbnail {
                                path,
                                image,
                                duration: 0.,
                                detail,
                            })))
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    let asset = rcr_editor_core::probe_cancellable(&path, cancelled).ok();
                    let duration = asset.as_ref().map_or(0., |a| a.duration_ns as f64 / 1e9);
                    let detail = asset.as_ref().map_or_else(String::new, |a| {
                        let audio = a
                            .streams
                            .iter()
                            .filter(|s| s.kind == rcr_editor_core::TrackKind::Audio)
                            .count();
                        format!(
                            "{} × {} · {} audio track{}",
                            a.width,
                            a.height,
                            audio,
                            if audio == 1 { "" } else { "s" }
                        )
                    });
                    let image = asset
                        .and_then(|a| rcr_editor_core::thumbnail_cancellable(&a, 0.2, (320, 180), cancelled).ok())
                        .and_then(|f| image::RgbaImage::from_raw(f.width, f.height, f.bgra))
                        .map(|b| {
                            if let Some(cache) = &cache && !cancelled() {
                                save_thumbnail(cache, &b, duration, &detail);
                            }
                            Arc::new(RenderImage::new(vec![image::Frame::new(b)]))
                        });
                    if complete(Some(ResultItem::Thumbnail(Thumbnail {
                            path,
                            image,
                            duration,
                            detail,
                        })))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("thumbnail worker");
        Self {
            images: HashMap::new(),
            catalog: HashMap::new(),
            catalog_paths: vec![],
            catalog_pending: false,
            catalog_dirty: true,
            file_membership: FileMembership::default(),
            view_key: String::new(),
            pending: HashSet::new(),
            order: VecDeque::new(),
            tasks: Some(tx),
            results: Some(results),
            worker: Some(worker),
            work,
            removed: removal::RemovalUndo::default(),
            import_guard: Default::default(),
            removal_pending: false,
            removal_ref_updates: BTreeSet::new(),
            removal_revision: 0,
            removal_intents: HashMap::new(),
            page: 0,
            filter: 0,
            favorites_only: false,
            sort: 0,
            selected: HashSet::new(),
            selection_range: SelectionRange::default(),
            selection_probe: None,
            card_focus: HashMap::new(),
            card_scroll: ScrollHandle::new(),
            keyboard_return: None,
            keyboard_smoke: None,
            root_scan_error: None,
            scan_preview: None,
            folder_open: FolderOpenAdmission::default(),
        }
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let mut progressed = false;
        for result in self.results.iter().flat_map(|results| results.try_iter()) {
            progressed = true;
            if result.revision != self.work.current() { continue; }
            let Some(result) = result.item else { continue; };
            let t = match result {
                ResultItem::Thumbnail(t) => t,
                ResultItem::Catalog(paths, mut catalog) => {
                    for (path, incoming) in &mut catalog {
                        if let Some(old) = self.catalog.get(path) {
                            incoming.preserve_last_known(old);
                        }
                    }
                    // A refreshed file may reuse its path after replacement or recording
                    // completion. Invalidate the resident image as well as the disk key.
                    self.images.retain(|path, _| {
                        self.catalog
                            .get(path)
                            .zip(catalog.get(path))
                            .is_some_and(|(old, new)| {
                                old.available == new.available
                                    && old.bytes == new.bytes
                                    && old.modified == new.modified
                                    && old.created == new.created
                            })
                    });
                    self.order.retain(|path| self.images.contains_key(path));
                    self.catalog_paths = paths;
                    self.catalog = catalog;
                    self.catalog_pending = false;
                    changed = true;
                    continue;
                }
            };
            self.pending.remove(&t.path);
            self.order.push_back(t.path.clone());
            self.images.insert(t.path.clone(), t);
            // At most 64 BGRA thumbnails (14 MiB); GPUI handles are released on eviction.
            while self.images.len() > 64 {
                if let Some(path) = self.order.pop_front() {
                    self.images.remove(&path);
                }
            }
            changed = true;
        }
        if progressed && self.catalog_dirty && !self.catalog_pending
            && let Some(files) = self.file_membership.paths.clone() {
            self.request_catalog(&files);
        }
        changed
    }
    fn request(&mut self, path: &PathBuf) {
        if !self.images.contains_key(path)
            && !self.pending.contains(path)
            && self
                .tasks
                .as_ref()
                .is_some_and(|tx| tx.try_send(WorkTask { revision: self.work.current(), task: Task::Thumbnail(path.clone()) }).is_ok())
        {
            self.pending.insert(path.clone());
        }
    }
    pub fn invalidate_metadata(&mut self) {
        self.invalidate_work();
        self.images.retain(|_, thumb| thumb.image.is_some());
        self.order.retain(|path| self.images.contains_key(path));
    }
    pub fn cached_title(&self, path: &Path) -> String {
        self.catalog
            .get(path)
            .map(|info| info.title.clone())
            .unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
    }
    pub fn catalog_files(&mut self, files: &[PathBuf]) {
        // Called when the scanner applies a file list, rather than on each paint.
        // One hash index keeps pruning linear even after visiting many pages.
        if self.file_membership.reconcile(files, &mut self.selected, &mut self.card_focus) {
            if self.selection_range.anchor.as_ref().is_some_and(|anchor| !files.contains(anchor)) {
                self.selection_range.anchor = None;
            }
            self.invalidate_work();
        }
        self.request_catalog(files);
    }
    pub fn snapshot_previews_ready(&self, files: &[PathBuf]) -> bool {
        files
            .iter()
            .take(4)
            .all(|path| self.images.contains_key(path))
    }
    fn request_catalog(&mut self, files: &[PathBuf]) {
        if !self.catalog_pending
            && (self.catalog_dirty || self.catalog_paths != files)
            && self
                .tasks
                .as_ref()
                .is_some_and(|tx| tx.try_send(WorkTask { revision: self.work.current(), task: Task::Catalog(files.to_vec()) }).is_ok())
        {
            self.catalog_pending = true;
            self.catalog_dirty = false;
        }
    }
    fn invalidate_work(&mut self) {
        self.work.advance();
        self.pending.clear();
        self.catalog_pending = false;
        self.catalog_dirty = true;
    }
    pub fn thumbnail(&mut self, path: &PathBuf) -> Div {
        let mut el = div()
            .relative()
            .w_full()
            .h_full()
            .overflow_hidden()
            .bg(rgb(0x101114));
        if self.catalog.get(path).is_some_and(|info| !info.available) {
            return el.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(icon("folder"))
                    .child(muted("File unavailable")),
            );
        }
        self.request(path);
        if let Some(t) = self.images.get(path) {
            if t.image.is_none() {
                el = el.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(muted("Preview unavailable")),
                );
            }
            if let Some(image) = &t.image {
                el = el.child(img(image.clone()).size_full().object_fit(ObjectFit::Cover));
            }
            if t.duration > 0. {
                el = el.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .right_2()
                        .px_1()
                        .rounded_sm()
                        .bg(rgba(0x000000bb))
                        .child(
                            mono(format!(
                                "{}:{:02}",
                                t.duration as u64 / 60,
                                t.duration as u64 % 60
                            ))
                            .text_color(rgb(TEXT)),
                        ),
                );
            }
        } else {
            el = el.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(icon(if recordings::is_image(path) {
                        "landscape"
                    } else {
                        "film"
                    }))
                    .child(muted("Loading preview…")),
            );
        }
        el
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        self.work.stop();
        self.tasks.take();
        // A worker can be blocked publishing into its bounded result channel.
        // Disconnect both directions before transferring its join off the UI.
        self.results.take();
        if let Some(worker) = self.worker.take()
            && let Err(error) = join_library_worker(worker) {
            eprintln!("Library background join unavailable: {error}");
        }
    }
}
fn kind(p: &std::path::Path) -> usize {
    if recordings::is_image(p) {
        return 4;
    }
    if p.parent().is_some_and(|p| p.ends_with("exports")) {
        3
    } else if p
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("replay-"))
    {
        1
    } else {
        2
    }
}
impl Studio {
    fn open_library_folder(&mut self, cx: &mut Context<Self>) {
        let Some(lease) = self.library.folder_open.acquire() else { return; };
        let root = recordings::root();
        let navigation = self.shell_navigation(cx);
        match recordings::begin_shell_action(recordings::ShellAction::OpenFolder, root, cx, move |this, operation, _, result, cx| {
            let _lease = lease;
            if this.shell_operation != operation { return; }
            this.shell_operation = 0;
            if this.notice.as_deref() == Some("Opening folder…") {
                this.notice = if this.shell_navigation(cx) == navigation { result.err() } else { None };
            }
            cx.notify();
        }) {
            Ok(operation) => { self.shell_operation = operation; self.notice = Some("Opening folder…".into()); }
            Err(error) => self.notice = Some(error),
        }
        cx.notify();
    }
    /// Metadata-only visual fixture; no captures, preferences or imports are changed on disk.
    pub(crate) fn snapshot_library_scan(&mut self, failed: bool, cx: &mut Context<Self>) {
        self.library_scan.cancel();
        self.library.invalidate_work();
        self.library.catalog_dirty = false;
        self.library.catalog_pending = false;
        self.library.scan_preview = Some(failed);
        self.files = ["Replay · Last round.mp4", "Desktop recording.mp4"]
            .map(|name| PathBuf::from("owned-library-preview").join(name)).to_vec();
        for (index, path) in self.files.iter().enumerate() {
            self.library.catalog.insert(path.clone(), FileInfo {
                available: !failed, local: true,
                title: path.file_name().unwrap().to_string_lossy().into_owned(),
                bytes: 42_000_000 + index as u64 * 8_000_000,
                ..Default::default()
            });
            self.library.images.insert(path.clone(), Thumbnail { path: path.clone(), image: None,
                duration: 32. + index as f64 * 16., detail: "2560 × 1440 · 60 FPS · AV1".into() });
        }
        self.library.root_scan_error = failed.then(|| recordings::RootScanError {
            root: PathBuf::from("R:/Gameplay/Captures"), kind: std::io::ErrorKind::NotFound,
        });
        self.notice = None;
        cx.notify();
    }

    pub fn library_ui(&mut self, cx: &mut Context<Self>) -> Div {
        let scanning = self.library.scan_preview.map_or_else(|| self.library_scan.is_pending(), |failed| !failed);
        let root_error = self.library.root_scan_error.clone();
        // Catalog admission belongs to scanner application and worker-progress
        // polling. A repaint must not clone the catalog into a full task queue.
        let search = self.search.read(cx).value().trim().to_lowercase();
        let view_key = format!(
            "{}:{}:{}:{}",
            self.library.filter, self.library.favorites_only, self.library.sort, search
        );
        if self.library.view_key != view_key {
            self.library.selection_range.anchor = None;
            self.library.keyboard_return = None;
            self.library.page = 0;
            self.library.card_scroll.scroll_to_item(0);
            self.library.view_key = view_key;
        }
        let mut files = self
            .files
            .iter()
            .filter(|p| {
                (self.library.filter == 0 || kind(p) == self.library.filter)
                    && (!self.library.favorites_only || self.preferences.favorites.contains(*p))
                    && (search.is_empty()
                        || self
                            .preferences
                            .custom_display_name(p)
                            .is_some_and(|name| name.to_lowercase().contains(&search))
                        || self
                            .library
                            .catalog
                            .get(*p)
                            .is_some_and(|info| info.search.contains(&search))
                        || p.file_name()
                            .is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&search)))
            })
            .cloned()
            .collect::<Vec<_>>();
        match self.library.sort {
            0 | 1 => files.sort_by(|a, b| {
                let a_stamp = self.library.catalog.get(a).and_then(|i| i.modified);
                let b_stamp = self.library.catalog.get(b).and_then(|i| i.modified);
                let order = b_stamp.cmp(&a_stamp).then_with(|| a.cmp(b));
                if self.library.sort == 1 {
                    order.reverse()
                } else {
                    order
                }
            }),
            2 => files.sort_by_cached_key(|p| self.capture_title(p).to_lowercase()),
            3 => files.sort_by_key(|p| {
                std::cmp::Reverse(self.library.catalog.get(p).map_or(0, |i| i.bytes))
            }),
            _ => {}
        }
        let bytes: u64 = self
            .files
            .iter()
            .filter_map(|p| self.library.catalog.get(p))
            .map(|info| info.bytes)
            .sum();
        let count = files.len();
        let matching_bytes: u64 = files
            .iter()
            .filter_map(|p| self.library.catalog.get(p))
            .map(|i| i.bytes)
            .sum();
        let sidebar = div()
            .w(px(188.))
            .h_full()
            .flex_shrink_0()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                button("all-clips", "All captures")
                    .justify_start()
                    .child(icon("film"))
                    .bg(rgb(if !self.library.favorites_only {
                        SOFT
                    } else {
                        BG
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.library.favorites_only = false;
                        this.library.filter = 0;
                        cx.notify();
                    })),
            )
            .child(
                button("favorites", "Favorites")
                    .justify_start()
                    .child(icon("star"))
                    .bg(rgb(if self.library.favorites_only {
                        SOFT
                    } else {
                        BG
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.library.favorites_only = true;
                        cx.notify();
                    })),
            )
            .child(muted("ON THIS PC").mt_6().px_3())
            .child(
                div()
                    .flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(icon("folder"))
                    .child("Local captures"),
            )
            .child(div().flex_1())
            .child(
                panel()
                    .child(muted("Referenced media"))
                    .child(mono(format!(
                        "{} · {} files",
                        file_size(bytes),
                        self.files.len()
                    )))
                    .child(muted("Captures and imported originals")),
            );
        let mut filters = div().flex().flex_wrap().items_center().gap_2();
        for (i, name) in ["All", "Replays", "Recordings", "Exports", "Screenshots"]
            .into_iter()
            .enumerate()
        {
            let count = self
                .files
                .iter()
                .filter(|p| {
                    (!self.library.favorites_only || self.preferences.favorites.contains(*p))
                        && (i == 0 || kind(p) == i)
                })
                .count();
            filters = filters.child(
                button(("library-filter", i), format!("{name}  {count}"))
                    .h(px(28.))
                    .bg(rgb(if self.library.filter == i { SOFT } else { BG }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.library.filter = i;
                        cx.notify();
                    })),
            );
        }
        let mut sort_controls = div().flex().items_center().gap_1().child(muted("Sort"));
        for (i, label) in ["Newest", "Oldest", "Name", "Size"].into_iter().enumerate() {
            sort_controls = sort_controls.child(
                button(("library-sort", i), label)
                    .h(px(28.))
                    .px_2()
                    .bg(rgb(if self.library.sort == i { SOFT } else { BG }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.library.sort = i;
                        cx.notify();
                    })),
            );
        }
        let mut view_controls = div().flex().items_center().gap_2();
        if self.layout_save_pending || self.layout_save_error.is_some() {
            let text = if self.layout_save_pending { "Saving view…" } else { "View not saved" };
            let detail = self.layout_save_error.clone().unwrap_or_else(|| "The selected Library view is waiting for its settings write to finish.".into());
            view_controls = view_controls.child(div().id("library-view-save-status")
                .text_xs().text_color(rgb(if self.layout_save_pending { MUTED } else { 0xeab765 }))
                .tooltip(hint(detail)).child(text));
        }
        for (list, name) in [(false, "grid"), (true, "list")] {
            view_controls = view_controls.child(
                button(name, "")
                    .tooltip(hint(if list { "List view" } else { "Grid view" }))
                    .px_2()
                    .child(icon(name))
                    .bg(rgb(if self.preferences.list_view == list {
                        SOFT
                    } else {
                        BG
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_library_list_view(list, cx);
                    })),
            );
        }
        let mut cards = div()
            .id("library-cards")
            .track_scroll(&self.library.card_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_wrap()
            .content_start()
            .gap_4();
        if self.preferences.list_view {
            cards = cards.flex_col().flex_nowrap().gap_1();
        }
        if files.is_empty() {
            cards = cards.child(
                div()
                    .w_full()
                    .py_16()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(icon("film"))
                    .child(if scanning {
                        "Finding your captures…"
                    } else if root_error.is_some() && self.files.is_empty() {
                        "Your captures will appear when the folder reconnects"
                    } else if self.files.is_empty() {
                        "Your Library starts here"
                    } else {
                        "No matching captures"
                    })
                    .child(muted(if scanning {
                        "Your Library will appear here when the scan finishes."
                    } else if root_error.is_some() && self.files.is_empty() {
                        "Use the folder controls above to reconnect or create your capture folder."
                    } else if self.files.is_empty() {
                        "Record your screen, or drop videos and images here to import."
                    } else if self.library.favorites_only {
                        "Star a capture to keep your best moments here, or clear your filters."
                    } else {
                        "Try another search or clear your filters to see every capture."
                    }))
                    .when(!self.files.is_empty(), |el| {
                        el.child(button("library-reset-empty", "Clear filters").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.library.filter = 0;
                                this.library.favorites_only = false;
                                this.search
                                    .update(cx, |search, cx| search.set_value("", cx));
                                cx.notify();
                            }),
                        ))
                    }),
            );
        }
        self.library.page = self.library.page.min(count.saturating_sub(1) / 24);
        let page = self.library.page;
        let page_files = files
            .iter()
            .skip(page * 24)
            .take(24)
            .cloned()
            .collect::<Vec<_>>();
        let all_page_selected =
            !page_files.is_empty() && page_files.iter().all(|p| self.library.selected.contains(p));
        if self.library.selection_range.page != page_files {
            self.library.keyboard_return = None;
        }
        self.library.selection_range.sync(&page_files);
        let selected_elsewhere = self.library.selected.len().saturating_sub(
            page_files
                .iter()
                .filter(|path| self.library.selected.contains(*path))
                .count(),
        );
        let selection_page = page_files.clone();
        let page_focus = page_files
            .iter()
            .map(|path| {
                self.library
                    .card_focus
                    .entry(path.clone())
                    .or_insert_with(|| cx.focus_handle().tab_stop(true))
                    .clone()
            })
            .collect::<Vec<_>>();
        for (i, path) in files.into_iter().skip(page * 24).take(24).enumerate() {
            let open = path.clone();
            let fav = path.clone();
            let select = path.clone();
            let rename_action = path.clone();
            let reveal = path.clone();
            let favorite = self.preferences.favorites.contains(&path);
            let selected = self.library.selected.contains(&path);
            let thumb = self.library.thumbnail(&path);
            let actions = div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div().relative().child(button(("select", i), "")
                        .tooltip(hint(if selected {
                            "Deselect capture"
                        } else {
                            "Select for batch actions · Shift+click selects a range"
                        }))
                        .px_2()
                        .h(px(26.))
                        .bg(rgba(0x17181bcc))
                        .child(
                            div()
                                .size(px(16.))
                                .rounded_sm()
                                .border_1()
                                .border_color(rgb(if selected { ACCENT } else { MUTED }))
                                .when(selected, |s| {
                                    s.bg(rgb(ACCENT))
                                        .child(icon("check").size(px(14.)).text_color(rgb(BG)))
                                }),
                        )
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.library.selection_range.apply(&mut this.library.selected, &select, event.modifiers().shift);
                            cx.notify();
                        })))
                        .when_some(self.library.selection_probe.clone(), |el, probe| el.child(selection_measure(probe, i * 2 + 1))),
                )
                .child(div().flex_1())
                .child(
                    button(("rename", i), "")
                        .tooltip(hint("Rename capture · F2"))
                        .px_2()
                        .h(px(26.))
                        .bg(rgba(0x17181bcc))
                        .child(icon("capture-pen"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.rename_capture(rename_action.clone(), window, cx);
                        })),
                )
                .child(
                    button(("favorite", i), "")
                        .tooltip(hint(if favorite {
                            "Remove from favorites"
                        } else {
                            "Add to favorites"
                        }))
                        .px_2()
                        .h(px(26.))
                        .bg(rgba(0x17181bcc))
                        .child(icon("star").text_color(rgb(if favorite { ACCENT } else { MUTED })))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if !this.preferences.favorites.remove(&fav) {
                                this.preferences.favorites.insert(fav.clone());
                            }
                            this.save_preferences();
                            cx.notify();
                        })),
                );
            let title = self.capture_title(&path);
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let detail = self
                .library
                .images
                .get(&path)
                .map_or_else(String::new, |t| t.detail.clone());
            let capture_kind = ["", "Replay", "Recording", "Export", "Screenshot"][kind(&path)];
            let metadata = self.library.catalog.get(&path).map_or_else(
                || format!("{capture_kind} · Reading details…"),
                |info| {
                    if info.available {
                        format!("{capture_kind} · {}", file_size(info.bytes))
                    } else {
                        format!("{} last known · Unavailable", file_size(info.bytes))
                    }
                },
            );
            let navigation_focus = page_focus.clone();
            let rename = path.clone();
            let keyboard_select = path.clone();
            let mut card = div()
                .id(("library-card", i))
                .track_focus(&page_focus[i])
                .tab_index(0)
                .key_context("LibraryCard")
                .focus(|s| s.border_color(rgb(ACCENT)).bg(rgb(0x19241f)))
                .flex_shrink_0()
                .rounded(px(10.))
                .overflow_hidden()
                .bg(rgb(PANEL))
                .border_1()
                .border_color(rgb(if selected { ACCENT } else { LINE }))
                .cursor_pointer()
                .relative()
                .tooltip(hint(format!("{file_name} · Enter to edit · Space / Ctrl+click to select · Shift+click for a range · F6 switches to search")))
                .hover(move |s| s.border_color(rgb(if selected { ACCENT } else { 0x45464d })))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    let stroke = &event.keystroke;
                    if stroke.modifiers.modified() {
                        return;
                    }
                    let columns = if this.preferences.list_view {
                        1
                    } else {
                        ((f32::from(window.viewport_size().width) - 220.) / 280.)
                            .floor()
                            .max(1.) as usize
                    };
                    let next = match stroke.key.as_str() {
                        "left" => Some(i.saturating_sub(1)),
                        "right" => Some((i + 1).min(navigation_focus.len().saturating_sub(1))),
                        "up" => Some(i.saturating_sub(columns)),
                        "down" => Some((i + columns).min(navigation_focus.len().saturating_sub(1))),
                        "home" => Some(0),
                        "end" => Some(navigation_focus.len().saturating_sub(1)),
                        "f2" => {
                            this.rename_capture(rename.clone(), window, cx);
                            window.prevent_default();
                            cx.stop_propagation();
                            return;
                        }
                        _ => None,
                    };
                    if let Some(next) = next {
                        navigation_focus[next].focus(window);
                        this.library.card_scroll.scroll_to_item(next);
                        window.prevent_default();
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    if event.modifiers().shift || event.modifiers().control || matches!(
                        event,
                        ClickEvent::Keyboard(KeyboardClickEvent {
                            button: KeyboardButton::Space,
                            ..
                        })
                    ) {
                        this.library.selection_range.apply(&mut this.library.selected, &keyboard_select, event.modifiers().shift);
                        cx.notify();
                    } else {
                        this.open_recording(open.clone(), cx);
                    }
                }));
            card = card.when_some(self.library.selection_probe.clone(), |el, probe| el.child(selection_measure(probe, i * 2)));
            if self.preferences.list_view {
                card = card
                    .w_full()
                    .h(px(76.))
                    .flex()
                    .items_center()
                    .gap_4()
                    .px_3()
                    .child(
                        div()
                            .w(px(80.))
                            .h(px(45.))
                            .rounded_md()
                            .overflow_hidden()
                            .child(thumb),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(title))
                            .child(muted(metadata).truncate())
                            .child(muted(detail).truncate()),
                    )
                    .child(actions.w(px(116.)))
                    .child(
                        button(("reveal", i), "")
                            .tooltip(hint("Show original in folder"))
                            .child(icon("folder"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.open_shell_action(recordings::ShellAction::Reveal, reveal.clone(), cx);
                            })),
                    );
            } else {
                card = card
                    .w(px(264.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .relative()
                            .h(px(if self.library.catalog.get(&path).is_some_and(|info| !info.available) { 96. } else { 148. }))
                            .child(thumb)
                            .child(div().absolute().top_2().left_2().right_2().child(actions)),
                    )
                    .child(
                        div()
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().truncate().child(title))
                            .child(muted(metadata).truncate())
                            .child(muted(detail).truncate())
                            .child(
                                button(("reveal-grid", i), "Show in folder")
                                    .h(px(26.))
                                    .bg(rgb(PANEL))
                                    .justify_start()
                                    .child(icon("folder"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.open_shell_action(recordings::ShellAction::Reveal, path.clone(), cx);
                                    })),
                            ),
                    );
            }
            cards = cards.child(card);
        }
        let mut main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            // Keep a complete capture card visible when the batch toolbar appears.
            .when(!self.library.selected.is_empty(), |s| s.gap_3())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .child(
                                div()
                                    .text_size(px(22.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(if self.library.favorites_only {
                                        "Favorites"
                                    } else {
                                        "Library"
                                    }),
                            )
                            .child(muted(format!(
                                "{count} capture{} · {}{}",
                                if count == 1 { "" } else { "s" },
                                file_size(matching_bytes),
                                if count != self.files.len() {
                                    format!(" · {} in library", self.files.len())
                                } else {
                                    String::new()
                                }
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                button("import-library-media", "Import")
                                    .tooltip(hint("Open videos, images or .ssxe projects · originals stay in place"))
                                    .on_click(cx.listener(|this, _, window, cx| this.choose_library_import(window, cx))),
                            )
                            .child(
                                button("refresh-library", if scanning { "Refreshing…" } else { "Refresh" })
                                    .tooltip(hint("Refresh captures · F5 · Search, filters and selection stay in place"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.library.invalidate_metadata();
                                        this.refresh_files();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                button("open-library-trash", "Trash")
                                    .track_focus(&self.trash_focus)
                                    .tooltip(hint("Review removed captures and restore files from recovery records"))
                                    .on_click(cx.listener(|this, _, _, cx| this.open_library_trash(cx))),
                            )
                            .child(
                                button("open-library-folder", if self.library.folder_open.pending() { "Opening…" } else { "Open folder" })
                                    .child(icon("folder"))
                                    .on_click(cx.listener(|this, _, _, cx| this.open_library_folder(cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("library-search")
                            .w(px(300.))
                            .rounded_md()
                            .bg(rgb(0x17181b))
                            .tooltip(hint("Search capture names and filenames · Ctrl+F · F6 returns to captures"))
                            .child(self.search.clone()),
                    )
                    .child(div().flex_1())
                    .child(sort_controls),
            )
            .child(div().flex().flex_wrap().items_center().gap_2()
                .child(filters.flex_1().min_w_0())
                .child(view_controls.flex_shrink_0()))
            .when_some(root_error, |el, error| {
                el.child(panel().p_3().gap_1()
                    .child(div().flex().items_center().gap_3()
                        .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child("Capture folder unavailable"))
                        .child(button("retry-library-scan", if scanning { "Retrying…" } else { "Retry" })
                            .h(px(28.))
                            .on_click(cx.listener(|this, _, _, cx| { this.refresh_files(); cx.notify(); })))
                        .child(button("open-unavailable-library-folder", if self.library.folder_open.pending() { "Opening…" } else { "Open folder" })
                            .h(px(28.))
                            .on_click(cx.listener(|this, _, _, cx| this.open_library_folder(cx)))))
                    .child(muted(error.root.to_string_lossy().into_owned()).min_w_0().truncate())
                    .child(muted(if error.kind == std::io::ErrorKind::NotFound {
                        "Reconnect its drive or retry. A first recording creates a missing folder."
                    } else {
                        "Check the drive and folder permissions, then retry. Your existing Library is kept."
                    })))
            })
            .when(scanning && self.library.root_scan_error.is_none(), |el| {
                el.child(muted("Checking your capture folder… Your current captures remain available."))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(muted("Enter to edit · Space / Ctrl+click to select · Shift+click for range · F2 rename"))
                    .child(div().flex_1())
                    .when(count > 0, |el| {
                        el.child(
                            button(
                                "select-visible-captures",
                                if all_page_selected {
                                    "Deselect page"
                                } else {
                                    "Select page"
                                },
                            )
                            .h(px(26.))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    for path in &selection_page {
                                        if all_page_selected {
                                            this.library.selected.remove(path);
                                        } else {
                                            this.library.selected.insert(path.clone());
                                        }
                                    }
                                    cx.notify();
                                },
                            )),
                        )
                    }),
            )
            .child(cards);
        if count > 24 {
            main = main.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(muted(format!(
                        "{}–{} of {count}",
                        page * 24 + 1,
                        ((page + 1) * 24).min(count)
                    )))
                    .child(div().flex_1())
                    .child(
                        button("previous-page", "Previous")
                            .tab_stop(page > 0)
                            .opacity(if page > 0 { 1. } else { 0.35 })
                            .cursor(if page > 0 {
                                CursorStyle::PointingHand
                            } else {
                                CursorStyle::Arrow
                            })
                            .when(page > 0, |s| {
                                s.on_click(cx.listener(|this, _, _, cx| {
                                    this.library.page = this.library.page.saturating_sub(1);
                                    this.library.card_scroll.scroll_to_item(0);
                                    cx.notify();
                                }))
                            }),
                    )
                    .child(
                        button("next-page", "Next")
                            .tab_stop((page + 1) * 24 < count)
                            .opacity(if (page + 1) * 24 < count { 1. } else { 0.35 })
                            .cursor(if (page + 1) * 24 < count {
                                CursorStyle::PointingHand
                            } else {
                                CursorStyle::Arrow
                            })
                            .when((page + 1) * 24 < count, |s| {
                                s.on_click(cx.listener(move |this, _, _, cx| {
                                    if (this.library.page + 1) * 24 < count {
                                        this.library.page += 1;
                                        this.library.card_scroll.scroll_to_item(0);
                                    }
                                    cx.notify();
                                }))
                            }),
                    ),
            );
        }
        if self.library.removal_pending {
            main = main.child(muted("Working on Library files… You can keep using rcr."));
        }
        if !self.library.removed.is_empty() {
            main = main.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(muted(self.library.removed.description()).flex_1().min_w_0())
                    .child(
                        button("undo-remove", "Undo removal")
                            .tab_stop(!self.library.removal_pending)
                            .opacity(if self.library.removal_pending {
                                0.35
                            } else {
                                1.
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.undo_library_removal(cx);
                            })),
                    ),
            );
        }
        if !self.library.selected.is_empty() {
            main = main.child(
                panel()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .child(if selected_elsewhere == 0 {
                        format!("{} selected", self.library.selected.len())
                    } else {
                        format!("{} selected · {selected_elsewhere} outside this page", self.library.selected.len())
                    })
                    .child(div().flex_1())
                    .child(button("clear-selection", "Clear").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.library.selected.clear();
                            this.library.selection_range.anchor = None;
                            cx.notify();
                        },
                    )))
                    .child(
                        button(
                            "favorite-selection",
                            if self
                                .library
                                .selected
                                .iter()
                                .all(|p| self.preferences.favorites.contains(p))
                            {
                                "Unfavorite"
                            } else {
                                "Favorite"
                            },
                        )
                        .child(icon("star"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this
                                .library
                                .selected
                                .iter()
                                .all(|p| this.preferences.favorites.contains(p))
                            {
                                this.preferences
                                    .favorites
                                    .retain(|p| !this.library.selected.contains(p));
                            } else {
                                this.preferences
                                    .favorites
                                    .extend(this.library.selected.iter().cloned());
                            }
                            this.save_preferences();
                            cx.notify();
                        })),
                    )
                    .child(button("share-selection", "Share").on_click(cx.listener(
                        |this, _, _, cx| {
                            let paths = this.library.selected.iter().cloned().collect();
                            this.tab = 4;
                            this.toolkit
                                .update(cx, |toolkit, cx| toolkit.share_files(paths, cx));
                            cx.notify();
                        },
                    )))
                    .child(
                        button("show-selection", if self.library.selected.len() == 1 {
                            "Show in folder"
                        } else {
                            "Show first in folder"
                        }).on_click(cx.listener(
                            |this, _, _, cx| {
                                if let Some(path) = this.library.selected.iter().min().cloned() {
                                    this.open_shell_action(recordings::ShellAction::Reveal, path, cx);
                                }
                            },
                        )),
                    )
                    .child(
                        button("remove-selection", if self.library.selected.len() == 1 && self.library.selected.iter().all(|path| self.preferences.imported_files.contains(path) && self.library.catalog.get(path).is_some_and(|info| !info.local)) { "Remove from Library" } else { "Remove selected" })
                            .tab_stop(!self.library.removal_pending)
                            .opacity(if self.library.removal_pending { 0.35 } else { 1. })
                            .tooltip(hint("Captures in your capture folder move to Trash. Imported references outside it are removed from Library only; original files and edits stay in place. Undo restores either."))
                            .child(icon("trash"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.remove_selected_library(cx);
                            })),
                    ),
            );
        }
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .key_context("Library")
            .on_drop::<ExternalPaths>(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.import_library_media(paths.paths().to_vec(), cx);
            }))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(0x10241b)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let stroke = &event.keystroke;
                let search_focus = this.search.read(cx).focus_handle(cx);
                if stroke.key == "f6"
                    && !stroke.modifiers.control && !stroke.modifiers.alt
                    && !stroke.modifiers.platform && !stroke.modifiers.function
                {
                    if search_focus.is_focused(window) {
                        if let Some(index) = keyboard_return_index(&page_files, this.library.keyboard_return.as_deref()) {
                            page_focus[index].focus(window);
                            this.library.card_scroll.scroll_to_item(index);
                        }
                    } else {
                        if let Some(index) = page_focus.iter().position(|focus| focus.is_focused(window)) {
                            this.library.keyboard_return = Some(page_files[index].clone());
                        }
                        search_focus.focus(window);
                    }
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                } else if stroke.key == "f5" && !stroke.modifiers.modified() {
                    this.library.invalidate_metadata();
                    this.refresh_files();
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                } else if stroke.modifiers.control
                    && !stroke.modifiers.alt
                    && !stroke.modifiers.shift
                    && stroke.key == "f"
                {
                    search_focus.focus(window);
                    window.prevent_default();
                    cx.stop_propagation();
                } else if stroke.key == "escape" && !stroke.modifiers.modified() {
                    if search_focus.is_focused(window) {
                        this.search
                            .update(cx, |search, cx| search.set_value("", cx));
                        if let Some(first) = page_focus.first() {
                            first.focus(window);
                        }
                    } else {
                        this.library.selected.clear();
                        this.library.selection_range.anchor = None;
                    }
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                } else if !search_focus.is_focused(window)
                    && stroke.modifiers.control
                    && !stroke.modifiers.alt
                    && !stroke.modifiers.shift
                    && stroke.key == "a"
                {
                    this.library.selected.extend(page_files.iter().cloned());
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(sidebar)
            .child(main)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::time::{Duration, Instant};

    #[test]
    fn keyboard_pane_return_uses_exact_current_page_identity_and_handles_empty_results() {
        let page = ["first.mp4", "second.mp4"].map(PathBuf::from);
        assert_eq!(keyboard_return_index(&page, Some(&page[1])), Some(1));
        assert_eq!(keyboard_return_index(&page, Some(Path::new("SECOND.mp4"))), Some(0));
        assert_eq!(keyboard_return_index(&page, Some(Path::new("previous-page.mp4"))), Some(0));
        assert_eq!(keyboard_return_index(&page, None), Some(0));
        assert_eq!(keyboard_return_index(&[], Some(&page[0])), None);
    }

    #[test]
    fn unavailable_metadata_keeps_known_details_without_claiming_availability() {
        let old = FileInfo { available: true, local: true, title: "Winning round".into(),
            search: "winning round replay.mp4".into(), bytes: 42_000_000,
            created: Some(UNIX_EPOCH + Duration::from_secs(10)),
            modified: Some(UNIX_EPOCH + Duration::from_secs(20)) };
        let mut unavailable = FileInfo::default();
        unavailable.preserve_last_known(&old);
        assert!(!unavailable.available);
        assert!(!unavailable.local, "Fresh location classification must not be replaced");
        assert_eq!(unavailable.title, old.title);
        assert_eq!(unavailable.search, old.search);
        assert_eq!(unavailable.bytes, old.bytes);
        assert_eq!(unavailable.created, old.created);
        assert_eq!(unavailable.modified, old.modified);
        let mut replacement = FileInfo { available: true, title: "Replacement".into(),
            bytes: 9, modified: Some(UNIX_EPOCH + Duration::from_secs(30)), ..Default::default() };
        replacement.preserve_last_known(&unavailable);
        assert!(replacement.available);
        assert_eq!(replacement.title, "Replacement");
        assert_eq!(replacement.bytes, 9);
        assert_eq!(replacement.modified, Some(UNIX_EPOCH + Duration::from_secs(30)));
    }

    #[test]
    fn folder_admission_lasts_until_actual_worker_lease_retires() {
        let admission = FolderOpenAdmission::default();
        let lease = admission.acquire().unwrap();
        assert!(admission.pending());
        assert!(admission.acquire().is_none());
        drop(lease);
        assert!(!admission.pending());
        let lease = admission.acquire().unwrap();
        let state = admission.0.clone();
        drop(admission);
        assert!(state.load(Ordering::Acquire), "Dropping the view must not release a running job");
        drop(lease);
        assert!(!state.load(Ordering::Acquire));
    }

    #[test]
    fn range_selection_is_inclusive_additive_and_keeps_exact_path_anchor() {
        let page = ["a.mp4", "b.mp4", "B.mp4", "d.mp4"].map(PathBuf::from);
        let outside = PathBuf::from("outside-this-page.mp4");
        let mut selected = HashSet::from([outside.clone()]);
        let mut range = SelectionRange::default();
        range.sync(&page);
        range.apply(&mut selected, &page[1], false);
        range.apply(&mut selected, &page[3], true);
        assert_eq!(selected, HashSet::from([outside.clone(), page[1].clone(), page[2].clone(), page[3].clone()]));
        assert_eq!(range.anchor, Some(page[1].clone()));
        range.apply(&mut selected, &page[1], false);
        range.apply(&mut selected, &page[0], true);
        assert!(selected.contains(&outside));
        assert!(page.iter().all(|path| selected.contains(path)));
        range.sync(&[page[3].clone(), page[2].clone(), page[1].clone(), page[0].clone()]);
        assert!(range.anchor.is_none(), "Reordering must discard the old anchor");
        range.apply(&mut selected, &page[2], true);
        assert_eq!(range.anchor, Some(page[2].clone()));
        range.sync(&page[..2]);
        assert!(range.anchor.is_none(), "A removed or off-page anchor must not survive");
        let before = selected.clone();
        range.apply(&mut selected, &outside, true);
        assert_eq!(selected, before, "A stale callback must not select an absent path");
    }

    #[test]
    fn obsolete_work_cancels_and_disconnected_results_allow_background_join() {
        let work = LibraryWork::default();
        let revision = work.current();
        let worker_work = work.clone();
        let (results, receiver) = mpsc::sync_channel(1);
        let (entered, entry) = mpsc::channel();
        let (publishing, publication) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            results.send(()).unwrap(); // occupy the bounded result slot.
            entered.send(()).unwrap();
            while !worker_work.cancelled(revision) {
                std::thread::sleep(Duration::from_millis(1));
            }
            publishing.send(()).unwrap();
            assert!(results.send(()).is_err(), "result receiver remained connected during shutdown");
        });
        entry.recv_timeout(Duration::from_secs(2)).unwrap();
        work.advance();
        assert!(work.cancelled(revision));
        assert!(!work.cancelled(work.current()));
        publication.recv_timeout(Duration::from_secs(2)).unwrap();
        work.stop();
        assert!(work.cancelled(work.current()));
        drop(receiver); // same ordering as Library::drop, before join handoff.
        let background_join = join_library_worker(worker).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !background_join.is_finished() {
            assert!(Instant::now() < deadline, "disconnected worker did not complete background join");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(background_join.join().unwrap(), "owned blocked worker panicked");
    }

    #[test]
    fn obsolete_worker_results_cannot_replace_current_catalog_or_complete_new_requests() {
        let mut library = Library::new();
        let stale_revision = library.work.current();
        library.invalidate_work();
        library.catalog_dirty = false;
        library.catalog_pending = true;
        let path = PathBuf::from("current-request.mp4");
        library.pending.insert(path.clone());
        let (sender, receiver) = mpsc::sync_channel(2);
        library.results = Some(receiver);
        sender.send(WorkResult { revision: stale_revision, item: Some(ResultItem::Thumbnail(Thumbnail {
            path: path.clone(), image: None, duration: 0., detail: String::new(),
        })) }).unwrap();
        sender.send(WorkResult { revision: stale_revision,
            item: Some(ResultItem::Catalog(vec![path.clone()], HashMap::from([(path.clone(), FileInfo::default())]))) }).unwrap();
        assert!(!library.poll());
        assert!(library.pending.contains(&path));
        assert!(library.catalog_pending);
        assert!(library.images.is_empty() && library.catalog.is_empty() && library.catalog_paths.is_empty());
    }

    #[test]
    fn full_catalog_queue_retries_only_after_worker_progress_with_latest_paths() {
        let mut library = Library::new();
        let (tasks, receiver) = mpsc::sync_channel(1);
        let (progress, results) = mpsc::sync_channel(2);
        library.tasks = Some(tasks);
        library.results = Some(results);
        let stale_revision = library.work.current();
        assert!(library.tasks.as_ref().unwrap().try_send(WorkTask { revision: stale_revision,
            task: Task::Thumbnail(PathBuf::from("obsolete.png")) }).is_ok());
        let paths = vec![PathBuf::from("latest.mp4")];
        library.catalog_files(&paths);
        assert!(!library.catalog_pending && library.catalog_dirty);
        for _ in 0..20 { assert!(!library.poll()); }
        let old_task = receiver.try_recv().unwrap();
        assert_eq!(old_task.revision, stale_revision);
        for _ in 0..20 { assert!(!library.poll()); }
        assert!(matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)));
        // Cancelling obsolete work still acknowledges worker progress. This is
        // the sole retry trigger after the initial full-slot rejection.
        progress.send(WorkResult { revision: stale_revision, item: None }).unwrap();
        assert!(!library.poll());
        assert!(library.catalog_pending && !library.catalog_dirty);
        let admitted = receiver.try_recv().unwrap();
        assert_eq!(admitted.revision, library.work.current());
        match admitted.task {
            Task::Catalog(actual) => assert_eq!(actual, paths),
            Task::Thumbnail(_) => panic!("worker progress admitted another thumbnail instead of the latest catalog"),
        }
        for _ in 0..20 { assert!(!library.poll()); }
        assert!(matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn image_thumbnail_budget_preserves_8k_rgba_and_rejects_oversized_layouts() {
        assert!(image_thumb_layout_allowed(7680, 4320, 4, 132_710_400));
        assert!(image_thumb_layout_allowed(8192, 4096, 4, IMAGE_THUMB_DECODE_BUDGET));
        assert!(!image_thumb_layout_allowed(8192, 4097, 4, 134_250_496));
        assert!(!image_thumb_layout_allowed(32769, 1, 3, 98_307));
        assert!(!image_thumb_layout_allowed(8, 4, 4, 127));
        assert!(!image_thumb_layout_allowed(0, 4, 4, 0));
    }

    #[test]
    fn tiny_oversized_image_header_is_rejected_without_pixel_payload() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("large-header.bmp");
        // A 54-byte BMP header declares a 3GiB decoded image; no pixel data is
        // allocated or written by this fixture.
        let mut header = vec![0u8; 54];
        header[..2].copy_from_slice(b"BM");
        header[2..6].copy_from_slice(&54u32.to_le_bytes());
        header[10..14].copy_from_slice(&54u32.to_le_bytes());
        header[14..18].copy_from_slice(&40u32.to_le_bytes());
        header[18..22].copy_from_slice(&32768i32.to_le_bytes());
        header[22..26].copy_from_slice(&32768i32.to_le_bytes());
        header[26..28].copy_from_slice(&1u16.to_le_bytes());
        header[28..30].copy_from_slice(&24u16.to_le_bytes());
        std::fs::write(&source, &header).unwrap();
        assert!(load_image_thumbnail(&source).is_none());
        assert_eq!(std::fs::read(&source).unwrap(), header);
    }

    #[test]
    fn normal_image_thumbnail_keeps_pixels_and_does_not_modify_source() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("owned.png");
        image::RgbaImage::from_pixel(8, 4, image::Rgba([10, 20, 30, 255])).save(&source).unwrap();
        let original = std::fs::read(&source).unwrap();
        let (thumbnail, detail) = load_image_thumbnail(&source).unwrap();
        assert_eq!(thumbnail.dimensions(), (320, 160));
        assert_eq!(thumbnail.get_pixel(4, 2).0, [10, 20, 30, 255]);
        assert_eq!(detail, "8 × 4");
        assert_eq!(std::fs::read(&source).unwrap(), original);
    }

    #[test]
    #[ignore = "opt-in owned 8K RGBA PNG codec proof; allocates one 126.6MiB source before decoding"]
    fn owned_8k_rgba_png_thumbnail_obeys_decode_budget() {
        use image::{ImageEncoder, codecs::png::{CompressionType, FilterType, PngEncoder}};
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("owned-8k.png");
        let (width, height) = (7680u32, 4320u32);
        let decoded_bytes = u64::from(width) * u64::from(height) * 4;
        assert!(image_thumb_layout_allowed(width, height, 4, decoded_bytes));
        {
            let mut pixels = vec![0u8; decoded_bytes as usize];
            for pixel in pixels.as_chunks_mut::<4>().0 { pixel.copy_from_slice(&[33, 77, 121, 255]); }
            let file = std::fs::File::create(&source).unwrap();
            let writer = std::io::BufWriter::new(file);
            PngEncoder::new_with_quality(writer, CompressionType::Fast, FilterType::NoFilter)
                .write_image(&pixels, width, height, image::ExtendedColorType::Rgba8).unwrap();
        } // Release the source pixel Vec before production thumbnail decode.
        let before = std::fs::metadata(&source).unwrap();
        let started = Instant::now();
        let (thumbnail, detail) = load_image_thumbnail(&source).expect("actual 8K PNG thumbnail decode exceeded limits or failed");
        assert_eq!(thumbnail.dimensions(), (320, 180));
        assert_eq!(thumbnail.get_pixel(160, 90).0, [33, 77, 121, 255]);
        assert_eq!(detail, "7680 × 4320");
        let after = std::fs::metadata(&source).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        println!("{}", serde_json::json!({"proof":"owned_8k_png_library_thumbnail",
            "input":[width,height],"decoded_bytes":decoded_bytes,"decode_budget":IMAGE_THUMB_DECODE_BUDGET,
            "output":[thumbnail.width(),thumbnail.height()],"codec":"PNG RGBA8",
            "separate_source_and_decode_allocations":true,"source_unchanged":true,
            "elapsed_ms":started.elapsed().as_secs_f64()*1000.}));
    }

    #[test]
    fn changed_file_list_prunes_deleted_selection_and_preserves_exact_focus_identity() {
        let retained = PathBuf::from("Highlight.MP4");
        let deleted = PathBuf::from("Deleted.MP4");
        let case_alias = PathBuf::from("highlight.MP4");
        let mut membership = FileMembership::default();
        let mut selected = HashSet::from([retained.clone(), deleted.clone()]);
        let mut focus = HashMap::from([(retained.clone(), 7), (deleted.clone(), 9)]);
        assert!(membership.reconcile(&[retained.clone(), deleted.clone()], &mut selected, &mut focus));
        assert!(!membership.reconcile(&[retained.clone(), deleted.clone()], &mut selected, &mut focus));
        selected.insert(case_alias.clone());
        focus.insert(case_alias, 11);
        assert!(membership.reconcile(std::slice::from_ref(&retained), &mut selected, &mut focus));
        assert_eq!(selected, HashSet::from([retained.clone()]));
        assert_eq!(focus, HashMap::from([(retained, 7)]));
        assert!(membership.reconcile(&[], &mut selected, &mut focus));
        assert!(selected.is_empty() && focus.is_empty());
        assert!(!membership.reconcile(&[], &mut selected, &mut focus));
    }

    fn finish_catalog(library: &mut Library) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while library.catalog_pending {
            library.poll();
            assert!(Instant::now() < deadline, "metadata worker did not respond");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn offline_catalog_keeps_capture_identity_and_availability() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Imported highlight.MP4");
        std::fs::write(&source, b"source bytes").unwrap();
        let source = source.canonicalize().unwrap();
        let mut library = Library::new();
        library.catalog_files(std::slice::from_ref(&source));
        finish_catalog(&mut library);
        assert!(library.catalog[&source].available);
        let last_title = library.cached_title(&source);
        let last_modified = library.catalog[&source].modified;
        let last_bytes = library.catalog[&source].bytes;
        std::fs::remove_file(&source).unwrap();
        library.invalidate_metadata();
        library.catalog_files(std::slice::from_ref(&source));
        finish_catalog(&mut library);
        assert_eq!(library.catalog_paths, vec![source.clone()]);
        assert!(!library.catalog[&source].available);
        assert_eq!(library.cached_title(&source), last_title);
        assert_eq!(library.catalog[&source].modified, last_modified);
        assert_eq!(library.catalog[&source].bytes, last_bytes);
    }

    #[test]
    fn catalogue_refreshes_a_growing_file_without_changing_its_path() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("Screenshot.PNG");
        std::fs::write(&path, b"first").unwrap();
        let mut library = Library::new();
        library.request_catalog(std::slice::from_ref(&path));
        finish_catalog(&mut library);
        assert_eq!(library.catalog[&path].bytes, 5);
        assert!(library.catalog[&path].search.contains("screenshot.png"));

        std::fs::write(&path, b"finished recording").unwrap();
        library.invalidate_metadata();
        library.request_catalog(std::slice::from_ref(&path));
        finish_catalog(&mut library);
        assert_eq!(library.catalog[&path].bytes, 18);

        library.selected.insert(path);
        library.catalog_files(&[]);
        finish_catalog(&mut library);
        assert!(library.catalog.is_empty());
        assert!(library.selected.is_empty());
    }
    fn cache_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(THUMB_MAGIC);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&12.5f64.to_le_bytes());
        bytes.extend_from_slice(&5u16.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(b"1 x 1");
        bytes.extend_from_slice(&[30, 20, 10, 255]);
        bytes
    }

    #[test]
    fn thumbnail_cache_rejects_truncation_invalid_layout_and_nonfinite_duration() {
        let bytes = cache_bytes();
        let (image, duration, detail) = decode_thumbnail(&bytes).unwrap();
        assert_eq!(image.as_raw(), &[30, 20, 10, 255]);
        assert_eq!(duration, 12.5);
        assert_eq!(detail, "1 x 1");
        for end in 0..bytes.len() {
            assert!(
                decode_thumbnail(&bytes[..end]).is_none(),
                "accepted truncated entry at {end}"
            );
        }
        let mut corrupt = bytes.clone();
        corrupt[8..12].copy_from_slice(&321u32.to_le_bytes());
        assert!(decode_thumbnail(&corrupt).is_none());
        let mut corrupt = bytes.clone();
        corrupt[16..24].copy_from_slice(&f64::NAN.to_le_bytes());
        assert!(decode_thumbnail(&corrupt).is_none());
        let mut corrupt = bytes;
        corrupt[30] = 0xff;
        assert!(decode_thumbnail(&corrupt).is_none());
        assert!(decode_thumbnail(&vec![0; THUMB_MAX_BYTES + 1]).is_none());
    }

    #[test]
    fn cache_identity_invalidates_deleted_replaced_and_modified_sources() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("source.png");
        std::fs::write(&source, b"first").unwrap();
        let first = thumbnail_cache_path(&source).unwrap();
        std::fs::remove_file(&source).unwrap();
        assert!(thumbnail_cache_path(&source).is_none());
        std::fs::write(&source, b"replacement").unwrap();
        assert_ne!(first, thumbnail_cache_path(&source).unwrap());

        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        let stamp = UNIX_EPOCH + Duration::from_secs(1_720_000_000);
        file.set_times(
            std::fs::FileTimes::new().set_modified(stamp + Duration::from_nanos(100_000_100)),
        )
        .unwrap();
        let before = thumbnail_cache_path(&source).unwrap();
        file.set_times(
            std::fs::FileTimes::new().set_modified(stamp + Duration::from_nanos(100_000_200)),
        )
        .unwrap();
        assert_ne!(
            before,
            thumbnail_cache_path(&source).unwrap(),
            "full subsecond mtime must participate in the key"
        );
    }

    #[test]
    fn thumbnail_eviction_keeps_recent_entries_and_unrelated_files() {
        let folder = tempfile::tempdir().unwrap();
        let foreign = folder.path().join("keep.txt");
        std::fs::write(&foreign, b"not a thumbnail").unwrap();
        for i in 0..33u64 {
            let file = std::fs::File::create(folder.path().join(format!("{i:064x}.bgra"))).unwrap();
            file.set_len(1024 * 1024).unwrap();
            file.set_times(
                std::fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + Duration::from_secs(1_720_000_000 + i)),
            )
            .unwrap();
        }
        let directory = folder.path().join(format!("{:064x}.bgra", 100));
        std::fs::create_dir(&directory).unwrap();
        prune_thumbnails(folder.path());
        assert!(!folder.path().join(format!("{:064x}.bgra", 0)).exists());
        assert!(folder.path().join(format!("{:064x}.bgra", 32)).exists());
        assert_eq!(std::fs::read(&foreign).unwrap(), b"not a thumbnail");
        assert!(directory.is_dir());
        let retained = std::fs::read_dir(folder.path())
            .unwrap()
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum::<u64>();
        assert_eq!(retained, THUMB_BUDGET + b"not a thumbnail".len() as u64);
    }

    #[test]
    fn thumbnail_publication_round_trips_without_partial_files() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join(format!("{:064x}.bgra", 1));
        let buffer = image::RgbaImage::from_raw(1, 1, vec![30, 20, 10, 255]).unwrap();
        save_thumbnail(&path, &buffer, 3.5, "1 x 1");
        let (decoded, duration, _) = decode_thumbnail(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(decoded, buffer);
        assert_eq!(duration, 3.5);
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
    }
}

#[path = "library_import_smoke.rs"]
mod import_smoke;
pub use import_smoke::LibraryImportSmoke;
