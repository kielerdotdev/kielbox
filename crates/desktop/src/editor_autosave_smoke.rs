//! Opt-in native autosave failure/retry proof; only a unique owned sidecar is obstructed.
use super::*;
use anyhow::{Context as _, ensure};
use std::path::Path;

const MEDIA_ERROR: &str = "Unrelated media error fixture";
const CAPTURE_NAME: &str = "Autosave retry fixture";
type Prepared = Result<(recovery_smoke::Fixture, Project), String>;

#[derive(Default)]
pub struct AutosaveSmoke {
    stage: u8,
    preparing: Option<mpsc::Receiver<Prepared>>,
    verifying: Option<mpsc::Receiver<Result<(), String>>>,
    fixture: Option<recovery_smoke::Fixture>,
    expected: Option<serde_json::Value>,
    failed_revision: Option<u64>,
}

fn prepare(source: PathBuf) -> anyhow::Result<(recovery_smoke::Fixture, Project)> {
    let fixture = recovery_smoke::prepare(source)?;
    // prepare() exclusively created this copied capture and its damaged sidecar.
    // Replace only that owned sidecar with an empty directory to make atomic save fail.
    std::fs::remove_file(&fixture.sidecar)?;
    std::fs::create_dir(&fixture.sidecar)?;
    let mut editor = Editor::default();
    editor.execute(Edit::Sequence(QuickEdit::Add(core::probe(&fixture.media)?)))?;
    Ok((fixture, editor.project))
}

impl EditorView {
    /// Call in an isolated smoke launch before the normal --open branch.
    /// Uses the production debounce, save worker, revision ACKs and retry action.
    pub fn smoke_autosave(
        view: Entity<Self>,
        probe: &mut AutosaveSmoke,
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
            let (fixture, project) = match probe.preparing.as_ref().unwrap().try_recv() {
                Ok(result) => result.map_err(anyhow::Error::msg)?,
                Err(mpsc::TryRecvError::Empty) => return Ok(false),
                Err(error) => return Err(error.into()),
            };
            ensure!(
                !view.read(cx).ready,
                "Autosave smoke must not open the user's original"
            );
            ensure!(
                fixture.sidecar.is_dir(),
                "Owned save obstruction was not created"
            );
            probe.expected = Some(serde_json::to_value(&project)?);
            view.update(cx, |editor, cx| {
                editor.open_media(fixture.media.clone(), cx);
                // The background fixture already probed its owned source. Avoid the
                // general open task interpreting the deliberate directory as saved edits.
                editor.pending.clear();
                editor.sidecar = Some(fixture.sidecar.clone());
                editor.set_title(CAPTURE_NAME.into());
                editor.loaded(project);
                editor.error = Some(MEDIA_ERROR.into());
            });
            ensure!(
                view.read(cx).autosave_status() == SaveStatus::Saving,
                "Unacknowledged initial save was presented as saved"
            );
            probe.fixture = Some(fixture);
            probe.stage = 1;
            return Ok(false);
        }

        window.draw(cx).clear();
        let fixture = probe.fixture.as_ref().context("Missing autosave fixture")?;
        match probe.stage {
            1 => {
                let editor = view.read(cx);
                ensure!(
                    editor.error.as_deref() == Some(MEDIA_ERROR),
                    "Autosave failure clobbered an unrelated media error"
                );
                if editor.autosave_status() != SaveStatus::Failed {
                    ensure!(
                        editor.autosave_status() == SaveStatus::Saving,
                        "Failed save was presented as saved before acknowledgement"
                    );
                    return Ok(false);
                }
                ensure!(
                    editor.pending_saves.contains_key(&fixture.sidecar),
                    "Failed save discarded the latest project"
                );
                ensure!(
                    editor
                        .autosave_error()
                        .is_some_and(|message| message.contains(CAPTURE_NAME)),
                    "Save failure did not identify the affected capture"
                );
                probe.failed_revision = Some(
                    editor
                        .save_states
                        .get(&fixture.sidecar)
                        .context("Missing failed save revision")?
                        .revision,
                );
                ensure!(
                    fixture.sidecar.is_dir(),
                    "Failed save replaced the obstruction"
                );
                std::fs::remove_dir(&fixture.sidecar)?;
                view.update(cx, |editor, cx| editor.retry_saves(cx));
                let editor = view.read(cx);
                ensure!(
                    editor.autosave_status() == SaveStatus::Saving,
                    "Retry was presented as saved before its ACK"
                );
                ensure!(
                    editor.error.as_deref() == Some(MEDIA_ERROR),
                    "Retry cleared an unrelated media error"
                );
                ensure!(
                    editor
                        .save_states
                        .get(&fixture.sidecar)
                        .is_some_and(|state| Some(state.revision) > probe.failed_revision),
                    "Retry did not create a new save revision"
                );
                probe.stage = 2;
            }
            2 => {
                let editor = view.read(cx);
                if editor.autosave_status() != SaveStatus::Saved {
                    ensure!(
                        editor.autosave_status() != SaveStatus::Failed,
                        "Retry failed: {:?}",
                        editor.autosave_error()
                    );
                    return Ok(false);
                }
                ensure!(
                    !editor.pending_saves.contains_key(&fixture.sidecar),
                    "Acknowledged save remained pending"
                );
                ensure!(
                    editor.autosave_error().is_none(),
                    "Successful retry retained a failure notice"
                );
                ensure!(
                    editor.error.as_deref() == Some(MEDIA_ERROR),
                    "Successful save cleared an unrelated media error"
                );
                let saved = Project::load(&fixture.sidecar)?;
                ensure!(
                    Some(serde_json::to_value(saved)?) == probe.expected,
                    "Acknowledged sidecar does not contain the latest project"
                );
                let (preview, scrub) = view.update(cx, |editor, _| {
                    editor.pause();
                    let preview = editor.preview.take();
                    let scrub = editor.scrub.take();
                    editor.visible = false;
                    editor.ready = false;
                    editor.recording = None;
                    editor.error = None;
                    editor.generation.fetch_add(1, Ordering::Relaxed);
                    editor.pending.clear();
                    editor.pending_saves.remove(&fixture.sidecar);
                    editor.save_states.remove(&fixture.sidecar);
                    editor.sessions.remove(&fixture.media);
                    (preview, scrub)
                });
                let fixture = fixture.clone();
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || {
                    if let Some(preview) = preview {
                        preview.shutdown();
                    }
                    if let Some(scrub) = scrub {
                        scrub.shutdown();
                    }
                    let _ = tx.send(
                        recovery_smoke::verify_and_cleanup(fixture)
                            .map_err(|error| format!("{error:#}")),
                    );
                });
                probe.verifying = Some(rx);
                probe.stage = 3;
            }
            3 => match probe
                .verifying
                .as_ref()
                .context("Missing autosave verification worker")?
                .try_recv()
            {
                Ok(result) => {
                    result.map_err(anyhow::Error::msg)?;
                    println!(
                        "Autosave smoke passed: owned sidecar directory forced real worker failure, pending project retained, capture named, unrelated media error preserved, Retry showed Saving before ACK, latest project durably Saved, original media/sidecar unchanged; owned fixtures removed"
                    );
                    probe.stage = 4;
                    return Ok(true);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(error) => return Err(error.into()),
            },
            _ => return Ok(true),
        }
        Ok(false)
    }
}
