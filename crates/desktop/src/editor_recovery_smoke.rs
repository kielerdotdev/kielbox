//! Opt-in native smoke fixture. Only a unique copy is ever opened or edited.
use super::*;
use anyhow::{Context as _, ensure};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
};

const DAMAGED_EDITS: &[u8] = b"{ deliberately damaged recovery smoke edits\n";

#[derive(Default)]
pub struct RecoverySmoke {
    stage: u8,
    preparing: Option<mpsc::Receiver<Result<Fixture, String>>>,
    verifying: Option<mpsc::Receiver<Result<(), String>>>,
    fixture: Option<Fixture>,
    reset_project: Option<serde_json::Value>,
    source_change: Option<mpsc::Receiver<Result<(), String>>>,
    source_snapshot: Option<Vec<u8>>,
    source_backups: Vec<PathBuf>,
    source_checked: bool,
}

#[derive(Clone)]
pub(super) struct Fixture {
    directory: PathBuf,
    pub(super) media: PathBuf,
    pub(super) sidecar: PathBuf,
    original: PathBuf,
    original_digest: Vec<u8>,
    original_sidecar: Option<Vec<u8>>,
    backup: Option<PathBuf>,
}

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

fn optional_bytes(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn remove_owned_file(path: &Path) -> anyhow::Result<()> {
    // A cancelled media task may still be releasing its Windows file handle.
    for attempt in 0..20 {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error) if attempt < 19 && matches!(error.raw_os_error(), Some(5 | 32 | 33)) => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!()
}

pub(super) fn prepare(source: PathBuf) -> anyhow::Result<Fixture> {
    let original = source.canonicalize()?;
    ensure!(original.is_file(), "Recovery fixture needs a video file");
    let original_digest = digest(&original)?;
    let original_sidecar = optional_bytes(&recordings::edit_path(&original))?;
    std::fs::create_dir_all(".downloads")?;
    let directory = tempfile::Builder::new()
        .prefix("recovery-smoke-")
        .tempdir_in(".downloads")?
        .keep();
    let media = directory
        .join("capture")
        .with_extension(original.extension().unwrap_or_default());
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&media)?;
    std::io::copy(&mut std::fs::File::open(&original)?, &mut output)?;
    output.flush()?;
    drop(output);
    let media = media.canonicalize()?;
    ensure!(
        digest(&media)? == original_digest,
        "Fixture copy differs from original"
    );
    let sidecar = recordings::edit_path(&media);
    std::fs::create_dir_all(sidecar.parent().context("Missing sidecar directory")?)?;
    let mut edits = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&sidecar)
        .context("Refusing to overwrite an existing edit sidecar")?;
    edits.write_all(DAMAGED_EDITS)?;
    edits.flush()?;
    Ok(Fixture {
        directory,
        media,
        sidecar,
        original,
        original_digest,
        original_sidecar,
        backup: None,
    })
}

pub(super) fn verify_and_cleanup(fixture: Fixture) -> anyhow::Result<()> {
    ensure!(
        digest(&fixture.original)? == fixture.original_digest,
        "User original changed during editor smoke"
    );
    ensure!(
        digest(&fixture.media)? == fixture.original_digest,
        "Fixture source changed while editing"
    );
    ensure!(
        optional_bytes(&recordings::edit_path(&fixture.original))? == fixture.original_sidecar,
        "User edit sidecar changed during editor smoke"
    );
    if let Some(backup) = &fixture.backup {
        ensure!(
            std::fs::read(backup)? == DAMAGED_EDITS,
            "Autosave overwrote the recovery copy"
        );
    }
    // Every removed path was created exclusively by this fixture, with create_new.
    remove_owned_file(&fixture.sidecar)?;
    if let Some(backup) = &fixture.backup {
        remove_owned_file(backup)?;
    }
    remove_owned_file(&fixture.media)?;
    std::fs::remove_dir(&fixture.directory)?;
    Ok(())
}

fn recovery_files(fixture: &Fixture) -> anyhow::Result<Vec<PathBuf>> {
    let folder = fixture
        .sidecar
        .parent()
        .context("Missing sidecar parent")?
        .join("recovery");
    if !folder.exists() {
        return Ok(Vec::new());
    }
    let prefix = format!(
        "{}-",
        fixture
            .sidecar
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
    );
    Ok(std::fs::read_dir(folder)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect())
}

impl EditorView {
    /// Call on each existing smoke tick, before the general --open branch.
    /// Preparation and final source verification run off the UI thread.
    pub fn smoke_recovery(
        view: Entity<Self>,
        probe: &mut RecoverySmoke,
        source: &Path,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<bool> {
        if probe.stage == 0 {
            if probe.preparing.is_none() {
                let source = source.to_owned();
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || {
                    let _ = tx.send(prepare(source).map_err(|error| format!("{error:#}")));
                });
                probe.preparing = Some(rx);
                return Ok(false);
            }
            let fixture = match probe.preparing.as_ref().unwrap().try_recv() {
                Ok(result) => result.map_err(anyhow::Error::msg)?,
                Err(mpsc::TryRecvError::Empty) => return Ok(false),
                Err(error) => return Err(error.into()),
            };
            ensure!(
                !view.read(cx).ready,
                "Recovery smoke must start without opening the user's original"
            );
            view.update(cx, |editor, cx| {
                editor.open_media(fixture.media.clone(), cx)
            });
            probe.fixture = Some(fixture);
            probe.stage = 1;
            return Ok(false);
        }

        let fixture = probe
            .fixture
            .as_mut()
            .context("Recovery fixture not prepared")?;
        window.draw(cx).clear();
        match probe.stage {
            1 => {
                let editor = view.read(cx);
                if !editor.edit_recovery {
                    ensure!(
                        editor.error.is_none(),
                        "Expected damaged-edit recovery, got {:?}",
                        editor.error
                    );
                    return Ok(false);
                }
                ensure!(
                    !editor.ready && !editor.loading && editor.error.is_some(),
                    "Damaged edits silently opened or left the editor loading"
                );
                ensure!(
                    std::fs::read(&fixture.sidecar)? == DAMAGED_EDITS,
                    "Opening damaged edits changed the sidecar before recovery consent"
                );
                ensure!(
                    recovery_files(fixture)?.is_empty(),
                    "Opening damaged edits created a recovery copy before consent"
                );
                view.update(cx, |editor, cx| editor.open_original(cx));
                probe.stage = 2;
            }
            2 => {
                ensure!(
                    view.read(cx).error.is_none(),
                    "Opening original failed: {:?}",
                    view.read(cx).error
                );
                if !view.read(cx).smoke_ready() {
                    return Ok(false);
                }
                let backups = recovery_files(fixture)?;
                ensure!(
                    backups.len() == 1,
                    "Open original must create exactly one recovery copy"
                );
                ensure!(
                    std::fs::read(&backups[0])? == DAMAGED_EDITS,
                    "Recovery copy did not preserve damaged bytes"
                );
                fixture.backup = Some(backups[0].clone());
                let original = view.read(cx).model.project.clone();
                ensure!(
                    !view.read(cx).edit_recovery,
                    "Recovery state did not clear after original decoded"
                );
                ensure!(
                    original
                        .assets
                        .iter()
                        .all(|asset| asset.path == fixture.media),
                    "Recovery referenced media outside the owned fixture"
                );
                let first = *core::pieces(&original)
                    .first()
                    .context("Recovered original has no video")?;
                ensure!(
                    first.duration >= 8,
                    "Recovery smoke requires at least eight source frames"
                );
                let trimmed_duration = first.duration * 3 / 4;
                view.update(cx, |editor, cx| {
                    editor.quick(
                        QuickEdit::TrimEnd {
                            id: first.id,
                            to: first.start + trimmed_duration,
                        },
                        cx,
                    )
                });
                let trimmed = serde_json::to_value(&view.read(cx).model.project)?;
                ensure!(
                    view.read(cx).model.project.duration() < original.duration(),
                    "Fixture trim did not shorten the original"
                );
                view.update(cx, |editor, cx| editor.reset_edits(cx));
                let reset = view.read(cx).model.project.clone();
                ensure!(
                    reset.duration() == original.duration(),
                    "Reset did not restore original duration"
                );
                ensure!(
                    core::pieces(&reset).len() == 1 && core::pieces(&reset)[0].source_in_ns == 0,
                    "Reset did not restore one complete source piece"
                );
                ensure!(
                    reset.clips.len() == original.clips.len()
                        && reset.clips.iter().all(|clip| clip.start == 0
                            && clip.source_in_ns == 0
                            && clip.duration == original.duration()),
                    "Reset did not restore all original audio/video tracks"
                );
                let reset = serde_json::to_value(&reset)?;
                view.update(cx, |editor, cx| editor.undo(cx));
                ensure!(
                    serde_json::to_value(&view.read(cx).model.project)? == trimmed,
                    "One Undo did not restore all edits before Reset"
                );
                view.update(cx, |editor, cx| editor.redo(cx));
                ensure!(
                    serde_json::to_value(&view.read(cx).model.project)? == reset,
                    "Redo did not restore the complete reset project"
                );
                probe.reset_project = Some(reset);
                probe.stage = 3;
            }
            3 => {
                ensure!(
                    view.read(cx).error.is_none(),
                    "Recovery/reset autosave failed: {:?}",
                    view.read(cx).error
                );
                let saved = match Project::load(&fixture.sidecar) {
                    Ok(project) => project,
                    Err(_) => return Ok(false),
                };
                if Some(serde_json::to_value(&saved)?) != probe.reset_project {
                    return Ok(false);
                }
                if !probe.source_checked {
                    view.update(cx, |editor, cx| editor.hide(cx));
                    probe.stage = 10;
                    return Ok(false);
                }
                // Freeze the fixture so shutdown cannot enqueue a late autosave after cleanup.
                let (preview, scrub) = view.update(cx, |editor, _| {
                    editor.pause();
                    let preview = editor.preview.take();
                    let scrub = editor.scrub.take();
                    editor.visible = false;
                    editor.ready = false;
                    editor.recording = None;
                    editor.generation.fetch_add(1, Ordering::Relaxed);
                    editor.pending.clear();
                    editor.pending_saves.remove(&fixture.sidecar);
                    editor.sessions.remove(&fixture.media);
                    (preview, scrub)
                });
                let (tx, rx) = mpsc::channel();
                ensure!(fixture.backup.is_some(), "Missing recovery backup");
                let fixture = fixture.clone();
                let extra_backups = probe.source_backups.clone();
                let source_snapshot = probe.source_snapshot.clone();
                thread::spawn(move || {
                    let result = (|| -> anyhow::Result<()> {
                        if let Some(preview) = preview {
                            preview.shutdown();
                        }
                        if let Some(scrub) = scrub {
                            scrub.shutdown();
                        }
                        for path in extra_backups {
                            ensure!(
                                Some(std::fs::read(&path)?) == source_snapshot,
                                "Source recovery backup changed"
                            );
                            remove_owned_file(&path)?;
                        }
                        verify_and_cleanup(fixture)
                    })();
                    let _ = tx.send(result.map_err(|error| format!("{error:#}")));
                });
                probe.verifying = Some(rx);
                probe.stage = 4;
            }
            4 => match probe
                .verifying
                .as_ref()
                .context("Missing verification worker")?
                .try_recv()
            {
                Ok(result) => {
                    result.map_err(anyhow::Error::msg)?;
                    println!(
                        "Recovery smoke passed: damaged edits preserved before consent, byte-exact backups, original decoded, Reset edits Undo/Redo exact, autosave durable, cached source-revision guard retains history/sidecar until explicit recovery, original media/sidecar unchanged; owned fixtures removed"
                    );
                    probe.stage = 5;
                    return Ok(true);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(error) => return Err(error.into()),
            },
            10 => {
                let editor = view.read(cx);
                if editor.pending_saves.contains_key(&fixture.sidecar)
                    || editor
                        .save_states
                        .get(&fixture.sidecar)
                        .is_some_and(|state| state.in_flight.is_some())
                {
                    return Ok(false);
                }
                probe.source_snapshot = Some(std::fs::read(&fixture.sidecar)?);
                let media = fixture.media.clone();
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || {
                    let result = (|| -> anyhow::Result<()> {
                        // Simulate a same-size source revision on the uniquely owned copy.
                        // Bytes stay valid, so explicit Open original can actually decode it.
                        let changed =
                            std::fs::metadata(&media)?.modified()? + Duration::from_secs(2);
                        for attempt in 0..20 {
                            let result = std::fs::OpenOptions::new()
                                .write(true)
                                .open(&media)
                                .and_then(|file| {
                                    file.set_times(std::fs::FileTimes::new().set_modified(changed))
                                });
                            match result {
                                Ok(()) => return Ok(()),
                                Err(error)
                                    if attempt < 19
                                        && matches!(error.raw_os_error(), Some(5 | 32 | 33)) =>
                                {
                                    thread::sleep(Duration::from_millis(100))
                                }
                                Err(error) => return Err(error.into()),
                            }
                        }
                        unreachable!()
                    })();
                    let _ = tx.send(result.map_err(|error| format!("{error:#}")));
                });
                probe.source_change = Some(rx);
                probe.stage = 11;
            }
            11 => match probe
                .source_change
                .as_ref()
                .context("Missing source revision worker")?
                .try_recv()
            {
                Ok(result) => {
                    result.map_err(anyhow::Error::msg)?;
                    view.update(cx, |editor, cx| {
                        editor.open_media(fixture.media.clone(), cx)
                    });
                    probe.stage = 12;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(error) => return Err(error.into()),
            },
            12 => {
                let editor = view.read(cx);
                if editor.loading {
                    return Ok(false);
                }
                ensure!(
                    editor.source_recovery && editor.edit_recovery && !editor.ready,
                    "Changed source silently reopened instead of preserving its previous edits: {:?}",
                    editor.error
                );
                ensure!(
                    Some(std::fs::read(&fixture.sidecar)?) == probe.source_snapshot,
                    "Source validation overwrote previous edits before recovery"
                );
                ensure!(
                    recovery_files(fixture)?.len() == 1,
                    "Source validation created a backup before explicit recovery"
                );
                ensure!(
                    editor.sessions.entries.iter().any(|(path, cached)| path
                        == &crate::preferences::lookup_key(&fixture.media)
                        && serde_json::to_value(&cached.project).ok() == probe.reset_project),
                    "Source validation discarded cached project history"
                );
                view.update(cx, |editor, cx| editor.open_original(cx));
                probe.stage = 13;
            }
            13 => {
                ensure!(
                    view.read(cx).error.is_none(),
                    "Explicit source recovery failed: {:?}",
                    view.read(cx).error
                );
                if !view.read(cx).smoke_ready() {
                    return Ok(false);
                }
                let backups = recovery_files(fixture)?;
                let extra = backups
                    .into_iter()
                    .filter(|path| Some(path) != fixture.backup.as_ref())
                    .collect::<Vec<_>>();
                ensure!(
                    !extra.is_empty() && extra.len() <= 2,
                    "Explicit recovery did not preserve disk and optional pending edits"
                );
                for backup in &extra {
                    ensure!(
                        Some(std::fs::read(backup)?) == probe.source_snapshot,
                        "Explicit recovery did not preserve the prior valid edits byte-exact"
                    );
                }
                ensure!(
                    !view.read(cx).source_recovery && !view.read(cx).edit_recovery,
                    "Source recovery state did not clear"
                );
                probe.source_backups = extra;
                probe.source_checked = true;
                probe.reset_project = Some(serde_json::to_value(&view.read(cx).model.project)?);
                probe.stage = 3;
            }
            _ => return Ok(true),
        }
        Ok(false)
    }
}
