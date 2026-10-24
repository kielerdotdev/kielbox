//! Opt-in GPUI integration fixture. CLI must isolate CWD before constructing Studio.
use super::*;
use crate::preferences::Preferences;
use anyhow::{Context as _, ensure};
use std::collections::BTreeSet;

#[derive(Default)]
pub struct LibraryImportSmoke {
    stage: u8,
    preparing: Option<mpsc::Receiver<Result<Fixture, String>>>,
    worker: Option<mpsc::Receiver<Result<(), String>>>,
    fixture: Option<Fixture>,
}

impl LibraryImportSmoke {
    pub const WORKSPACE_PREFIX: &'static str = "library-import-smoke-";
    pub const MARKER_FILE: &'static str = ".rcr-library-import-smoke";
    pub const MARKER_CONTENT: &'static [u8] = b"rcr-library-import-smoke-v1\n";
}

#[derive(Clone)]
struct Fixture {
    workspace: PathBuf,
    original: PathBuf,
    original_hash: Vec<u8>,
    video: PathBuf,
    image: PathBuf,
    offline_image: PathBuf,
    image_hash: Vec<u8>,
    local: PathBuf,
    unsupported: PathBuf,
    missing: PathBuf,
    folder: PathBuf,
    sidecar: PathBuf,
}

const SIDECAR: &[u8] = b"{ preserved fixture edit sidecar\n";
const DISPLAY_NAME: &str = "Offline favorite · café 🎮";

fn digest(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut source = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().to_vec())
}

fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(())
}

fn prepare(workspace: PathBuf, original: PathBuf) -> anyhow::Result<Fixture> {
    ensure!(
        original.is_absolute(),
        "Resolve --open before entering the isolated fixture workspace"
    );
    ensure!(
        recordings::is_supported(&original) && !recordings::is_image(&original),
        "Library import smoke needs a real video source"
    );
    let original_hash = digest(&original)?;
    let external = workspace.join("external");
    std::fs::create_dir(&external)?;
    let extension = original
        .extension()
        .context("Video extension missing")?
        .to_string_lossy()
        .to_ascii_uppercase();
    let video = external.join(format!("Imported video.{extension}"));
    let mut copy = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&video)?;
    std::io::copy(&mut std::fs::File::open(&original)?, &mut copy)?;
    copy.flush()?;
    drop(copy);
    let video = video.canonicalize()?;
    ensure!(
        digest(&video)? == original_hash,
        "Owned video copy differs from source"
    );
    let image = external.join("Imported image.PNG");
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        64,
        36,
        image::Rgba([38, 141, 102, 255]),
    ))
    .write_to(&mut encoded, image::ImageFormat::Png)?;
    write_new(&image, encoded.get_ref())?;
    let image = image.canonicalize()?;
    let image_hash = digest(&image)?;
    let offline_image = external.join("offline-image-backup.PNG");
    let root = workspace.join("recordings");
    std::fs::create_dir_all(root.join("nested"))?;
    let local = root.join("nested/Local capture.PNG");
    write_new(&local, encoded.get_ref())?;
    let local = local.canonicalize()?;
    let unsupported = external.join("unsupported.txt");
    write_new(&unsupported, b"unsupported fixture")?;
    let missing = external.join("missing.mp4");
    let folder = external.join("folder.mp4");
    std::fs::create_dir(&folder)?;
    let sidecar = recordings::edit_path(&video);
    std::fs::create_dir_all(sidecar.parent().context("Missing fixture project folder")?)?;
    write_new(&sidecar, SIDECAR)?;
    Ok(Fixture {
        workspace,
        original,
        original_hash,
        video,
        image,
        offline_image,
        image_hash,
        local,
        unsupported,
        missing,
        folder,
        sidecar,
    })
}

fn persisted() -> anyhow::Result<Preferences> {
    Ok(serde_json::from_slice(&std::fs::read(
        "projects/preferences.json",
    )?)?)
}

fn settled(studio: &Studio) -> bool {
    !studio.library.removal_pending
        && !studio.library_scan.is_pending()
        && !studio.library.catalog_pending
        && !studio.library.catalog_dirty
        && studio.library.catalog_paths == studio.files
}

fn key(window: &mut Window, cx: &mut App, name: &str) {
    window.draw(cx).clear();
    let keystroke = Keystroke {
        key: name.into(),
        key_char: None,
        modifiers: Default::default(),
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

fn start_worker(
    probe: &mut LibraryImportSmoke,
    job: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(job().map_err(|error| format!("{error:#}")));
    });
    probe.worker = Some(rx);
}

fn worker_ready(probe: &mut LibraryImportSmoke) -> anyhow::Result<bool> {
    match probe
        .worker
        .as_ref()
        .context("Missing fixture worker")?
        .try_recv()
    {
        Ok(result) => {
            result.map_err(anyhow::Error::msg)?;
            probe.worker = None;
            Ok(true)
        }
        Err(mpsc::TryRecvError::Empty) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn verify_sources(fixture: &Fixture) -> anyhow::Result<()> {
    ensure!(
        digest(&fixture.original)? == fixture.original_hash,
        "Original user media changed during Library fixture"
    );
    ensure!(
        digest(&fixture.video)? == fixture.original_hash,
        "Removing a Library reference changed its source video"
    );
    ensure!(
        std::fs::read(&fixture.sidecar)? == SIDECAR,
        "Removing a Library reference changed edit sidecar bytes"
    );
    ensure!(
        digest(&fixture.offline_image)? == fixture.image_hash,
        "Offline image backup changed"
    );
    ensure!(
        !fixture.image.exists(),
        "Offline image was recreated while removing/undoing its reference"
    );
    ensure!(
        digest(&fixture.local)? == fixture.image_hash,
        "Unselected local capture changed"
    );
    Ok(())
}

impl Library {
    /// Calls production asynchronous import, scan, removal and Undo paths. No OS
    /// picker or desktop input is used; all media mutations target fixture copies.
    pub fn smoke_import(
        studio: Entity<Studio>,
        probe: &mut LibraryImportSmoke,
        source_absolute: &Path,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<bool> {
        if probe.stage == 0 {
            if probe.preparing.is_none() {
                let workspace = std::env::current_dir()?;
                ensure!(
                    workspace.file_name().is_some_and(|name| name
                        .to_string_lossy()
                        .starts_with(LibraryImportSmoke::WORKSPACE_PREFIX)),
                    "Refusing Library fixture outside an isolated temporary workspace"
                );
                ensure!(
                    std::fs::read(workspace.join(LibraryImportSmoke::MARKER_FILE))?
                        == LibraryImportSmoke::MARKER_CONTENT,
                    "Library fixture marker is missing or invalid"
                );
                ensure!(
                    source_absolute.is_absolute(),
                    "Library fixture --open must be resolved before changing CWD"
                );
                ensure!(
                    studio.read(cx).preferences.imported_files.is_empty()
                        && studio.read(cx).preferences.favorites.is_empty()
                        && studio.read(cx).preferences.display_names.is_empty(),
                    "Library fixture must construct Studio after entering a fresh workspace"
                );
                let source = source_absolute.to_owned();
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ =
                        tx.send(prepare(workspace, source).map_err(|error| format!("{error:#}")));
                });
                probe.preparing = Some(rx);
                return Ok(false);
            }
            let fixture = match probe.preparing.as_ref().unwrap().try_recv() {
                Ok(result) => result.map_err(anyhow::Error::msg)?,
                Err(mpsc::TryRecvError::Empty) => return Ok(false),
                Err(error) => return Err(error.into()),
            };
            studio.update(cx, |this, cx| {
                this.tab = 1;
                this.import_library_media(
                    vec![
                        fixture.video.clone(),
                        fixture.image.clone(),
                        fixture.video.clone(),
                        fixture.unsupported.clone(),
                        fixture.missing.clone(),
                        fixture.folder.clone(),
                    ],
                    cx,
                );
            });
            probe.fixture = Some(fixture);
            probe.stage = 1;
            return Ok(false);
        }

        window.draw(cx).clear();
        let fixture = probe
            .fixture
            .as_ref()
            .context("Library fixture is not prepared")?
            .clone();
        ensure!(
            std::env::current_dir()? == fixture.workspace,
            "CWD changed while fixture workers were active"
        );
        match probe.stage {
            1 => {
                let state = studio.read(cx);
                if !state.preferences.imported_files.contains(&fixture.video)
                    || !state.preferences.imported_files.contains(&fixture.image)
                {
                    ensure!(
                        state
                            .notice
                            .as_deref()
                            .is_none_or(|notice| notice == "Adding files to Library…"),
                        "Import failed: {:?}",
                        state.notice
                    );
                    return Ok(false);
                }
                if !settled(state) {
                    return Ok(false);
                }
                ensure!(
                    state.preferences.imported_files.len() == 2,
                    "Import duplicate handling created extra references"
                );
                ensure!(
                    state.files.contains(&fixture.local)
                        && state.files.contains(&fixture.video)
                        && state.files.contains(&fixture.image),
                    "Background scan lost local or imported media"
                );
                ensure!(
                    state.library.card_focus.contains_key(&fixture.video)
                        && state.library.card_focus.contains_key(&fixture.image),
                    "Imported native cards were not painted"
                );
                if ![&fixture.video, &fixture.image].iter().all(|path| {
                    state
                        .library
                        .images
                        .get(*path)
                        .is_some_and(|thumbnail| thumbnail.image.is_some())
                }) {
                    return Ok(false);
                }
                let notice = state.notice.as_deref().unwrap_or_default();
                ensure!(
                    notice.contains("Added 2 captures")
                        && notice.contains("1 already")
                        && notice.contains("Skipped 1 unsupported")
                        && notice.contains("Skipped 2 unavailable"),
                    "Import count/skip feedback is incorrect: {notice}"
                );
                let saved = persisted()?;
                ensure!(
                    saved.imported_files
                        == BTreeSet::from([fixture.video.clone(), fixture.image.clone()]),
                    "Imported references were not persisted"
                );
                studio.update(cx, |this, cx| -> anyhow::Result<()> {
                    this.preferences
                        .set_display_name(&fixture.image, DISPLAY_NAME)
                        .map_err(anyhow::Error::msg)?;
                    this.preferences.favorites.insert(fixture.image.clone());
                    this.preferences.save()?;
                    cx.notify();
                    Ok(())
                })?;
                let moving = fixture.clone();
                start_worker(probe, move || {
                    ensure!(
                        !moving.offline_image.exists(),
                        "Fixture backup already exists"
                    );
                    std::fs::rename(&moving.image, &moving.offline_image)?;
                    Ok(())
                });
                probe.stage = 2;
            }
            2 => {
                if !worker_ready(probe)? {
                    return Ok(false);
                }
                studio.update(cx, |this, cx| {
                    this.refresh_files();
                    cx.notify();
                });
                probe.stage = 3;
            }
            3 => {
                let state = studio.read(cx);
                if !settled(state) {
                    return Ok(false);
                }
                ensure!(
                    state.files.contains(&fixture.image)
                        && state.preferences.imported_files.contains(&fixture.image),
                    "Offline import disappeared from Library"
                );
                ensure!(
                    !state.library.catalog[&fixture.image].available
                        && state.library.card_focus.contains_key(&fixture.image),
                    "Offline native card did not report unavailable"
                );
                ensure!(
                    state.capture_title(&fixture.image) == DISPLAY_NAME
                        && state.preferences.favorites.contains(&fixture.image),
                    "Offline capture lost display name or favorite"
                );
                let video_focus = state.library.card_focus[&fixture.video].clone();
                let image_focus = state.library.card_focus[&fixture.image].clone();
                video_focus.focus(window);
                key(window, cx, "space");
                image_focus.focus(window);
                key(window, cx, "space");
                ensure!(
                    studio.read(cx).library.selected
                        == HashSet::from([fixture.video.clone(), fixture.image.clone()]),
                    "Native card Space selection did not select the two imported references"
                );
                studio.update(cx, |this, cx| {
                    // Queue an old snapshot immediately before removal supersedes it.
                    this.refresh_files();
                    this.remove_selected_library(cx);
                });
                probe.stage = 4;
            }
            4 => {
                let state = studio.read(cx);
                if !settled(state) {
                    return Ok(false);
                }
                ensure!(
                    !state.files.contains(&fixture.video)
                        && !state.files.contains(&fixture.image)
                        && state.files.contains(&fixture.local),
                    "Stale scan resurrected removed references or lost the local capture"
                );
                ensure!(
                    state.preferences.imported_files.is_empty()
                        && !state.library.removed.is_empty(),
                    "Reference removal did not retain Undo history"
                );
                let saved = persisted()?;
                ensure!(
                    saved.imported_files.is_empty(),
                    "Reference removal was not persisted"
                );
                ensure!(
                    saved.custom_display_name(&fixture.image) == Some(DISPLAY_NAME)
                        && saved.favorites.contains(&fixture.image),
                    "Removal changed persistent names/favorites"
                );
                let verifying = fixture.clone();
                start_worker(probe, move || verify_sources(&verifying));
                probe.stage = 5;
            }
            5 => {
                if !worker_ready(probe)? {
                    return Ok(false);
                }
                studio.update(cx, |this, cx| {
                    this.undo_library_removal(cx);
                    // This deliberately obsolete request must never replace the
                    // following production snapshot containing restored references.
                    this.library_scan.request(crate::library_scan::ScanRequest {
                        root: recordings::root(),
                        toolkit_files: this.toolkit_files.clone(),
                        imported_files: BTreeSet::new(),
                    });
                    this.refresh_files();
                    cx.notify();
                });
                probe.stage = 6;
            }
            6 => {
                let state = studio.read(cx);
                if !settled(state) {
                    return Ok(false);
                }
                if !state
                    .library
                    .images
                    .get(&fixture.video)
                    .is_some_and(|thumbnail| thumbnail.image.is_some())
                {
                    return Ok(false);
                }
                ensure!(
                    state.files.contains(&fixture.video)
                        && state.files.contains(&fixture.image)
                        && state.files.contains(&fixture.local),
                    "Latest scan failed to restore imported and local cards"
                );
                ensure!(
                    state.library.removed.is_empty(),
                    "Successful Undo retained stale removal history"
                );
                ensure!(
                    state.library.card_focus.contains_key(&fixture.video)
                        && state.library.card_focus.contains_key(&fixture.image),
                    "Undo did not repaint imported native cards"
                );
                ensure!(
                    !state.library.catalog[&fixture.image].available,
                    "Undo recreated an offline source instead of restoring its reference"
                );
                let saved = persisted()?;
                ensure!(
                    saved.imported_files
                        == BTreeSet::from([fixture.video.clone(), fixture.image.clone()]),
                    "Undo did not persist restored references"
                );
                ensure!(
                    saved.custom_display_name(&fixture.image) == Some(DISPLAY_NAME)
                        && saved.favorites.contains(&fixture.image),
                    "Undo lost persistent display name/favorite"
                );
                let verifying = fixture.clone();
                start_worker(probe, move || verify_sources(&verifying));
                probe.stage = 7;
            }
            7 => {
                if !worker_ready(probe)? {
                    return Ok(false);
                }
                let owned = fixture.clone();
                start_worker(probe, move || {
                    let root = owned.workspace.join("recordings");
                    let source = root.join("nested/Restart recovery.PNG");
                    write_new(&source, &std::fs::read(&owned.local)?)?;
                    let moved = recordings::trash(std::slice::from_ref(&source), &root)?;
                    ensure!(
                        moved.len() == 1,
                        "Recovery fixture did not create a durable Trash record"
                    );
                    Ok(())
                });
                probe.stage = 8;
            }
            8 => {
                if !worker_ready(probe)? {
                    return Ok(false);
                }
                studio.update(cx, |this, cx| {
                    // Model a restarted application: recovery must come from the
                    // journal rather than this session's in-memory Undo stack.
                    this.library.removed = removal::RemovalUndo::default();
                    this.tab = 1;
                    cx.notify();
                });
                window.draw(cx).clear();
                studio.read(cx).trash_focus.focus(window);
                key(window, cx, "enter");
                ensure!(
                    studio.read(cx).trash_panel.is_some(),
                    "Native Trash button did not open recovery browser"
                );
                probe.stage = 9;
            }
            9 => {
                let panel = studio
                    .read(cx)
                    .trash_panel
                    .clone()
                    .context("Missing recovery browser")?;
                if !panel.read(cx).ready() {
                    return Ok(false);
                }
                ensure!(
                    panel.read(cx).entries().len() == 1
                        && panel.read(cx).entries()[0]
                            .original
                            .file_name()
                            .is_some_and(|name| name == "Restart recovery.PNG"),
                    "Recovery browser did not discover the journal after Undo memory was cleared"
                );
                ensure!(
                    panel.read(cx).list_focused(window),
                    "Recovery list did not receive native focus"
                );
                for _ in 0..5 {
                    key(window, cx, "tab");
                    ensure!(
                        panel.read(cx).owns_focus(window),
                        "Recovery Tab escaped its modal"
                    );
                }
                ensure!(
                    panel.read(cx).list_focused(window),
                    "Recovery Tab order did not cycle back to list"
                );
                key(window, cx, "enter");
                probe.stage = 10;
            }
            10 => {
                let panel = studio
                    .read(cx)
                    .trash_panel
                    .clone()
                    .context("Missing recovery browser during restore")?;
                if !panel.read(cx).ready()
                    || studio.read(cx).trash_restoring
                    || !panel.read(cx).entries().is_empty()
                {
                    return Ok(false);
                }
                ensure!(
                    studio.read(cx).library.removed.is_empty(),
                    "Durable recovery created stale in-memory Undo pairs"
                );
                ensure!(
                    studio.read(cx).tab == 1,
                    "Recovery navigated away from Library"
                );
                key(window, cx, "escape");
                probe.stage = 11;
            }
            11 => {
                if studio.read(cx).trash_panel.is_some() {
                    return Ok(false);
                }
                ensure!(
                    studio.read(cx).trash_focus.is_focused(window),
                    "Closing recovery did not restore Trash button focus"
                );
                let verifying = fixture.clone();
                start_worker(probe, move || {
                    verify_sources(&verifying)?;
                    ensure!(
                        digest(
                            &verifying
                                .workspace
                                .join("recordings/nested/Restart recovery.PNG")
                        )? == verifying.image_hash,
                        "Journal recovery did not restore the exact owned source bytes"
                    );
                    let saved = persisted()?;
                    ensure!(
                        saved.imported_files
                            == BTreeSet::from([verifying.video.clone(), verifying.image.clone()]),
                        "Durable recovery changed unrelated imported references"
                    );
                    ensure!(
                        saved.custom_display_name(&verifying.image) == Some(DISPLAY_NAME)
                            && saved.favorites.contains(&verifying.image),
                        "Durable recovery lost unrelated names/favorites"
                    );
                    Ok(())
                });
                probe.stage = 12;
            }
            12 => {
                if !worker_ready(probe)? {
                    return Ok(false);
                }
                println!(
                    "Library import smoke passed: background import/dedup, previews and native selection, offline named favorite, async reference removal/Undo, journal recovery after Undo reset, native modal Tab/Enter/Escape focus, exact restored bytes and preserved unrelated preferences; original media/sidecar unchanged. Isolated evidence: {}",
                    fixture.workspace.display()
                );
                probe.stage = 13;
                return Ok(true);
            }
            _ => return Ok(true),
        }
        Ok(false)
    }
}
