//! Real GPUI key dispatch against an exclusively owned copy, never user edits.
use super::recovery_smoke::{Fixture, prepare, verify_and_cleanup};
use super::*;
use anyhow::{Context as _, ensure};
use std::path::Path;

#[derive(Default)]
pub struct EditorKeyboardSmoke {
    stage: u8,
    preparing: Option<mpsc::Receiver<Result<Fixture, String>>>,
    verifying: Option<mpsc::Receiver<Result<(), String>>>,
    fixture: Option<Fixture>,
    saved_project: Option<serde_json::Value>,
}

fn key(window: &mut Window, cx: &mut App, name: &str, control: bool, shift: bool) {
    // GPUI installs native click-on-key-release listeners for the painted focus.
    window.draw(cx).clear();
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
    // Native keyboard clicks are generated on release, not key down.
    window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
    window.draw(cx).clear();
}

fn tab_stops(window: &mut Window, cx: &mut App) -> anyhow::Result<Vec<FocusHandle>> {
    window.blur();
    window.focus_next();
    let first = window.focused(cx).context("Editor has no tab stops")?;
    let mut stops = vec![first.clone()];
    for _ in 0..128 {
        window.focus_next();
        let next = window.focused(cx).context("Tab traversal lost focus")?;
        if next == first {
            return Ok(stops);
        }
        ensure!(
            !stops.contains(&next),
            "Tab traversal cycled before returning to its start"
        );
        stops.push(next);
    }
    anyhow::bail!("Editor tab traversal exceeded 128 controls")
}

fn history_signature(editor: &mut Editor) -> anyhow::Result<serde_json::Value> {
    // Core history traversal has no autosave/preview side effects. Restore both
    // stacks before serializing, including the redo branch left by append Undo.
    let current = editor.project.clone();
    let mut undo = Vec::new();
    while editor.undo() {
        undo.push(editor.project.clone());
    }
    for _ in 0..undo.len() {
        assert!(editor.redo());
    }
    let mut redo = Vec::new();
    while editor.redo() {
        redo.push(editor.project.clone());
    }
    for _ in 0..redo.len() {
        assert!(editor.undo());
    }
    Ok(serde_json::to_value((
        current,
        editor.linked_edits,
        undo,
        redo,
    ))?)
}

fn diagnostic_keyboard(
    view: &Entity<EditorView>,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    let before = view.update(cx, |editor, _| history_signature(&mut editor.model))?;
    ensure!(
        before[2]
            .as_array()
            .is_some_and(|history| !history.is_empty())
            && before[3]
                .as_array()
                .is_some_and(|history| !history.is_empty()),
        "Diagnostic smoke needs real Undo and Redo branches"
    );
    let (playhead, selected) = {
        let editor = view.read(cx);
        ensure!(!editor.playing, "Diagnostic smoke must begin paused");
        (editor.playhead, editor.selected)
    };
    let previous = view.read(cx).focus.clone();
    previous.focus(window);
    view.update(cx, |editor, cx| {
        let error = anyhow::anyhow!(
            "Owned keyboard diagnostic · {}",
            "失敗🎞".repeat(MAX_EXPORT_DETAIL_CHARS)
        );
        let failure =
            ExportFailure::from(error.context("Synthetic export failure; no encoder started"));
        editor.export_detail = Some(failure.detail);
        editor.show_export_details(window, cx);
    });
    window.draw(cx).clear();
    ensure!(
        view.read(cx)
            .export_detail
            .as_deref()
            .is_some_and(|detail| detail.starts_with("Synthetic export failure")
                && detail.ends_with("[Additional export details omitted]")
                && detail.chars().count() < MAX_EXPORT_DETAIL_CHARS + 64),
        "Diagnostic fixture did not use bounded actual error text"
    );
    ensure!(
        view.read(cx).export_details_focus.is_focused(window),
        "Details did not focus Copy details"
    );
    for _ in 0..3 {
        key(window, cx, "tab", false, false);
        ensure!(
            view.read(cx).export_details_close_focus.is_focused(window),
            "Modal Tab escaped Copy → Close"
        );
        key(window, cx, "tab", false, false);
        ensure!(
            view.read(cx).export_details_focus.is_focused(window),
            "Modal Tab escaped Close → Copy"
        );
        key(window, cx, "tab", false, true);
        ensure!(
            view.read(cx).export_details_close_focus.is_focused(window),
            "Modal Shift+Tab escaped Copy → Close"
        );
        key(window, cx, "tab", false, true);
        ensure!(
            view.read(cx).export_details_focus.is_focused(window),
            "Modal Shift+Tab escaped Close → Copy"
        );
    }
    for (name, control, shift) in [
        ("z", true, false),
        ("z", true, true),
        ("s", false, false),
        ("delete", false, false),
    ] {
        key(window, cx, name, control, shift);
        ensure!(
            view.read(cx).export_details_open,
            "Editor shortcut unexpectedly closed diagnostic modal"
        );
        ensure!(
            !view.read(cx).playing && view.read(cx).export.is_none(),
            "Modal shortcut started playback or export"
        );
        let after = view.update(cx, |editor, _| history_signature(&mut editor.model))?;
        ensure!(
            after == before,
            "Modal shortcut {name} changed project or Undo/Redo history"
        );
    }
    // Space activates Close, never Copy: the fixture must not touch OS clipboard.
    key(window, cx, "tab", false, false);
    key(window, cx, "space", false, false);
    ensure!(
        !view.read(cx).export_details_open && previous.is_focused(window),
        "Space on Close failed to close and restore prior focus"
    );
    ensure!(
        !view.read(cx).playing && !view.read(cx).export_details_copied,
        "Space leaked into playback or clipboard action"
    );
    view.update(cx, |editor, cx| editor.show_export_details(window, cx));
    key(window, cx, "escape", false, false);
    ensure!(
        !view.read(cx).export_details_open && previous.is_focused(window),
        "Escape did not close details and restore previous focus"
    );
    // A retained incomplete file adds a real third action. Reach it through
    // keyboard navigation, but never invoke Explorer or the OS clipboard.
    let partial = view.read(cx).recording.as_ref().context("Missing owned fixture path")?
        .with_file_name(format!("keyboard-incomplete-proof-{}.partial", "long-path-".repeat(48)));
    view.update(cx, |editor, cx| {
        editor.export_partial = Some(partial.clone());
        editor.show_export_details(window, cx);
    });
    key(window, cx, "tab", false, false);
    ensure!(view.read(cx).export_details_close_focus.is_focused(window), "Partial modal Copy → Close failed");
    key(window, cx, "tab", false, false);
    ensure!(view.read(cx).export_details_reveal_focus.is_focused(window), "Partial modal Close → retained-file action failed");
    key(window, cx, "tab", false, false);
    ensure!(view.read(cx).export_details_focus.is_focused(window), "Partial modal retained-file action → Copy failed");
    key(window, cx, "tab", false, true);
    ensure!(view.read(cx).export_details_reveal_focus.is_focused(window), "Partial modal reverse Copy → retained-file action failed");
    key(window, cx, "tab", false, true);
    ensure!(view.read(cx).export_details_close_focus.is_focused(window), "Partial modal reverse retained-file action → Close failed");
    key(window, cx, "end", false, false);
    ensure!(view.read(cx).export_details_scroll.offset().y < px(0.), "Long export diagnostic body did not scroll to End");
    key(window, cx, "home", false, false);
    ensure!(view.read(cx).export_details_scroll.offset().y == px(0.), "Export diagnostic Home did not return to its beginning");
    // Pending actions are skipped by Tab and guarded even if programmatically
    // focused. These are synthetic receipt IDs, with no service submission.
    view.update(cx, |editor, cx| {
        editor.export_details_copy_operation = 501;
        editor.shell_operation = 601;
        editor.export_details_copy_error = Some(format!("Injected copy error {}", "detail ".repeat(160)));
        editor.export_details_reveal_error = Some(format!("Injected reveal error {}", "detail ".repeat(160)));
        cx.notify();
    });
    for shift in [false, true] {
        key(window, cx, "tab", false, shift);
        ensure!(view.read(cx).export_details_close_focus.is_focused(window), "Pending action remained in modal Tab cycle");
    }
    let close = view.read(cx).export_details_close_bounds.get().context("Export Close button was not measured")?;
    let viewport = window.viewport_size();
    ensure!(close.left() >= px(0.) && close.top() >= px(0.) && close.right() <= viewport.width && close.bottom() <= viewport.height,
        "Long partial path/diagnostics/errors pushed Close outside the viewport");
    view.read(cx).export_details_focus.focus(window);
    key(window, cx, "enter", false, false);
    ensure!(view.read(cx).export_details_copy_operation == 501 && !view.read(cx).export_details_copied,
        "Pending Copy dispatched another request or claimed success");
    view.read(cx).export_details_reveal_focus.focus(window);
    key(window, cx, "enter", false, false);
    ensure!(view.read(cx).shell_operation == 601, "Pending reveal dispatched another request");
    view.update(cx, |editor, _| -> anyhow::Result<()> {
        let session = editor.shell_identity();
        let detail = editor.export_detail.clone().context("Missing diagnostic text")?;
        editor.finish_export_details_copy(500, &session, &detail, Ok(()));
        ensure!(editor.export_details_copy_operation == 501 && !editor.export_details_copied, "Wrong copy receipt consumed current operation");
        let mut old_session = session.clone();
        old_session.0 = old_session.0.wrapping_add(1);
        editor.finish_export_details_copy(501, &old_session, &detail, Ok(()));
        ensure!(editor.export_details_copy_operation == 0 && !editor.export_details_copied, "Old-generation receipt claimed copy success");
        editor.export_details_copy_operation = 502;
        editor.export_details_copy_error = None;
        editor.finish_export_details_copy(502, &session, &detail, Err("Injected clipboard lock failure".into()));
        ensure!(!editor.export_details_copied && editor.export_details_copy_error.as_deref().is_some_and(|error| error.contains("Injected clipboard lock failure")),
            "Actual failed receipt did not expose retryable diagnostic-copy error");
        editor.export_details_copy_error = None;
        editor.export_details_copy_operation = 503;
        editor.export_detail = Some(format!("{detail}\nA newer diagnostic"));
        editor.finish_export_details_copy(503, &session, &detail, Ok(()));
        ensure!(!editor.export_details_copied, "Receipt for changed diagnostics claimed success");
        editor.export_detail = Some(detail.clone());
        editor.export_details_copy_operation = 504;
        let mut other_recording = session.clone();
        other_recording.2 = Some(partial.clone());
        editor.finish_export_details_copy(504, &other_recording, &detail, Ok(()));
        ensure!(!editor.export_details_copied, "Receipt for another recording claimed success");
        editor.export_details_copy_operation = 507;
        editor.visible = false;
        editor.finish_export_details_copy(507, &session, &detail, Ok(()));
        ensure!(!editor.export_details_copied, "Hidden editor receipt claimed success");
        editor.visible = true;
        editor.export_details_copy_operation = 505;
        editor.finish_export_details_copy(505, &session, &detail, Ok(()));
        ensure!(editor.export_details_copied && editor.export_details_copy_error.is_none(), "Matching successful receipt did not acknowledge copy");
        editor.export_details_copied = false;
        editor.export_details_copy_operation = 506;
        editor.export_details_close_focus.focus(window);
        Ok(())
    })?;
    key(window, cx, "escape", false, false);
    view.update(cx, |editor, _| {
        let session = editor.shell_identity();
        let detail = editor.export_detail.clone().unwrap();
        editor.finish_export_details_copy(506, &session, &detail, Ok(()));
        ensure!(!editor.export_details_copied && editor.export_details_copy_operation == 0,
            "Receipt arriving after dialog closure changed copy status");
        editor.shell_operation = 0;
        editor.export_partial = None;
        Ok::<_, anyhow::Error>(())
    })?;
    ensure!(previous.is_focused(window), "Partial diagnostic Escape did not restore focus");
    println!("Export details recovery smoke: retained-file action forward/reverse Tab, pending actions skipped and guarded, injected copy receipt success/failure plus operation/generation/source/text/closed-dialog guards; no Explorer or clipboard call");
    let after = view.update(cx, |editor, _| history_signature(&mut editor.model))?;
    ensure!(
        after == before && view.read(cx).playhead == playhead && view.read(cx).selected == selected,
        "Diagnostic keyboard interaction changed editor state or history"
    );
    view.update(cx, |editor, cx| {
        editor.clear_export_details();
        cx.notify();
    });
    Ok(())
}

impl EditorView {
    /// Invoke before the generic --open branch, with Studio showing editor tab 2.
    pub fn smoke_keyboard(
        view: Entity<Self>,
        probe: &mut EditorKeyboardSmoke,
        source: &Path,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<bool> {
        if probe.stage == 0 {
            if probe.preparing.is_none() {
                let source = source.to_owned();
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || {
                    let result = (|| -> anyhow::Result<Fixture> {
                        let fixture = prepare(source)?;
                        // prepare reserved this unique hash with create_new. Remove only
                        // its deliberate damaged placeholder so normal source loading runs.
                        std::fs::remove_file(&fixture.sidecar)?;
                        Ok(fixture)
                    })();
                    let _ = tx.send(result.map_err(|error| format!("{error:#}")));
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
                "Keyboard smoke must start before opening user media"
            );
            view.update(cx, |editor, cx| {
                editor.open_media(fixture.media.clone(), cx)
            });
            probe.fixture = Some(fixture);
            probe.stage = 1;
            return Ok(false);
        }
        window.draw(cx).clear();
        let fixture = probe.fixture.as_ref().context("Keyboard fixture missing")?;
        match probe.stage {
            1 => {
                ensure!(
                    view.read(cx).error.is_none(),
                    "Keyboard fixture failed to open: {:?}",
                    view.read(cx).error
                );
                if !view.read(cx).smoke_ready() {
                    return Ok(false);
                }
                let project = &view.read(cx).model.project;
                ensure!(
                    project.duration() >= 8,
                    "Keyboard smoke needs at least eight video frames"
                );
                ensure!(
                    project
                        .tracks
                        .iter()
                        .any(|track| track.kind == TrackKind::Audio),
                    "Keyboard smoke requires a video fixture with real audio"
                );
                let at = project.duration() / 2;
                view.update(cx, |editor, _| {
                    editor.playhead = at;
                    editor.focus.focus(window);
                });
                key(window, cx, "s", false, false);
                probe.stage = 2;
            }
            2 => {
                let project = view.read(cx).model.project.clone();
                let pieces = core::pieces(&project);
                ensure!(
                    pieces.len() == 2,
                    "Keyboard S did not split the source into two pieces"
                );
                let audio: Vec<_> = project
                    .tracks
                    .iter()
                    .filter(|track| {
                        track.kind == TrackKind::Audio
                            && project.clips.iter().any(|clip| clip.track == track.id)
                    })
                    .collect();
                ensure!(!audio.is_empty(), "Split lost source audio");
                view.update(cx, |editor, cx| {
                    editor.selected = Some(pieces[0].id);
                    cx.notify();
                });
                window.draw(cx).clear();
                let stops = tab_stops(window, cx)?;
                // Timeline ends with pieces, Add clip, then mute/volume pairs.
                ensure!(
                    stops.len() > audio.len() * 2 + 8,
                    "Editor tab controls are missing"
                );
                let add_index = stops.len() - audio.len() * 2 - 1;
                stops[add_index - 2].focus(window);
                key(window, cx, "tab", false, false);
                ensure!(
                    stops[add_index - 1].is_focused(window),
                    "Tab did not reach the later timeline piece"
                );
                key(window, cx, "enter", false, false);
                ensure!(
                    view.read(cx).selected == Some(pieces[1].id),
                    "Enter did not select the later piece"
                );
                ensure!(
                    view.read(cx).drag.is_none(),
                    "Keyboard piece selection began a pointer drag"
                );
                view.update(cx, |editor, cx| {
                    editor.selected = Some(pieces[0].id);
                    cx.notify();
                });
                key(window, cx, "space", false, false);
                ensure!(
                    view.read(cx).selected == Some(pieces[1].id),
                    "Space did not select the focused later piece"
                );
                ensure!(
                    !view.read(cx).playing,
                    "Space on a focused timeline piece also started playback"
                );

                let track = audio[0].id;
                let gain = |cx: &App| {
                    view.read(cx)
                        .model
                        .project
                        .tracks
                        .iter()
                        .find(|t| t.id == track)
                        .unwrap()
                        .gain
                };
                let baseline = gain(cx);
                ensure!(
                    (baseline - 1.).abs() < 1e-8,
                    "Fixture audio must begin at unity gain"
                );
                let before_volume = serde_json::to_value(&view.read(cx).model.project)?;
                let playhead = view.read(cx).playhead;
                stops[add_index + 2].focus(window);
                key(window, cx, "right", false, false);
                ensure!(
                    (gain(cx) - 1.05).abs() < 1e-8,
                    "Right did not increase audio by 5%"
                );
                key(window, cx, "left", false, true);
                ensure!(
                    (gain(cx) - 1.04).abs() < 1e-8,
                    "Shift+Left did not decrease audio by 1%"
                );
                key(window, cx, "home", false, false);
                ensure!(gain(cx) == 0., "Home did not set zero volume");
                key(window, cx, "end", false, false);
                ensure!(gain(cx) == 2., "End did not set maximum volume");
                ensure!(
                    view.read(cx).playhead == playhead,
                    "Volume arrows escaped into timeline seek shortcuts"
                );
                for expected in [0., 1.04, 1.05, baseline] {
                    key(window, cx, "z", true, false);
                    ensure!(
                        (gain(cx) - expected).abs() < 1e-8,
                        "Ctrl+Z did not undo one volume adjustment"
                    );
                }
                ensure!(
                    serde_json::to_value(&view.read(cx).model.project)? == before_volume,
                    "Volume Undo changed other timeline edits"
                );

                // Fresh smoke header: Library, Undo, Redo, Reset, Export, options.
                stops[5].focus(window);
                key(window, cx, "space", false, false);
                ensure!(
                    view.read(cx).preset_menu && view.read(cx).menu_focus.is_focused(window),
                    "Keyboard options did not focus the first preset"
                );
                ensure!(
                    !view.read(cx).playing,
                    "Space on a focused button also started playback"
                );
                key(window, cx, "escape", false, false);
                ensure!(
                    !view.read(cx).preset_menu && view.read(cx).focus.is_focused(window),
                    "Escape did not close presets and restore editor focus"
                );
                stops[5].focus(window);
                key(window, cx, "enter", false, false);
                key(window, cx, "tab", false, false);
                key(window, cx, "enter", false, false);
                ensure!(
                    view.read(cx).preset == Preset::FullHd && !view.read(cx).preset_menu,
                    "Tab/Enter did not select the second preset"
                );

                let named = "Keyboard custom name · café 🎮".to_owned();
                let unavailable = fixture
                    .media
                    .with_file_name("not-on-disk-cached-choice.mkv");
                let mut large = vec![(
                    fixture.media.with_file_name("image.GIF"),
                    "Image must be excluded".into(),
                )];
                large.extend((0..80).map(|i| {
                    (
                        fixture.media.with_file_name(format!("cached-{i}.MP4")),
                        format!("Cached name {i}"),
                    )
                }));
                view.update(cx, |editor, _| editor.set_library_choices(large));
                ensure!(
                    view.read(cx).library_choices.len() == MAX_LIBRARY_CHOICES,
                    "Recent video picker exceeded its 64-choice bound"
                );
                ensure!(
                    view.read(cx).library_choices[0].1 == "Cached name 0"
                        && view.read(cx).library_choices[MAX_LIBRARY_CHOICES - 1].1
                            == "Cached name 63",
                    "Bounded picker lost cached names or included an image"
                );
                view.update(cx, |editor, cx| {
                    editor.set_library_choices(vec![
                        (fixture.media.clone(), named.clone()),
                        (unavailable.clone(), "Cached unavailable capture".into()),
                    ]);
                    cx.notify();
                });
                window.draw(cx).clear();
                stops[add_index].focus(window);
                key(window, cx, "enter", false, false);
                ensure!(
                    view.read(cx).picker && view.read(cx).menu_focus.is_focused(window),
                    "Keyboard Add clip did not focus the first cached choice"
                );
                ensure!(
                    view.read(cx).library_choices
                        == vec![
                            (fixture.media.clone(), named),
                            (unavailable, "Cached unavailable capture".into())
                        ],
                    "Picker replaced supplied display names or filtered cached choices while painting"
                );
                key(window, cx, "tab", false, false);
                key(window, cx, "tab", false, true);
                ensure!(
                    view.read(cx).menu_focus.is_focused(window),
                    "Picker Shift+Tab did not return to its first choice"
                );
                key(window, cx, "escape", false, false);
                ensure!(
                    !view.read(cx).picker && view.read(cx).focus.is_focused(window),
                    "Escape did not close cached picker and restore editor focus"
                );
                ensure!(
                    serde_json::to_value(&view.read(cx).model.project)? == before_volume,
                    "Keyboard menus modified the edit project"
                );
                stops[add_index].focus(window);
                key(window, cx, "enter", false, false);
                key(window, cx, "enter", false, false);
                ensure!(
                    !view.read(cx).picker && view.read(cx).focus.is_focused(window),
                    "Accepting a cached choice did not close picker and restore editor focus"
                );
                probe.saved_project = Some(before_volume);
                probe.stage = 3;
            }
            3 => {
                ensure!(
                    view.read(cx).error.is_none(),
                    "Adding cached fixture choice failed: {:?}",
                    view.read(cx).error
                );
                let before: Project = serde_json::from_value(
                    probe
                        .saved_project
                        .clone()
                        .context("Missing pre-append project")?,
                )?;
                let project = &view.read(cx).model.project;
                if core::pieces(project).len() == 2 {
                    return Ok(false);
                }
                ensure!(
                    core::pieces(project).len() == 3 && project.duration() == before.duration() * 2,
                    "Accepting cached choice did not append one complete source recording"
                );
                ensure!(
                    project
                        .assets
                        .iter()
                        .all(|asset| asset.path == fixture.media),
                    "Keyboard append referenced user media instead of owned fixture"
                );
                ensure!(
                    !view.read(cx).picker && view.read(cx).focus.is_focused(window),
                    "Async append lost editor focus"
                );
                key(window, cx, "z", true, false);
                probe.stage = 4;
            }
            4 => {
                ensure!(
                    Some(serde_json::to_value(&view.read(cx).model.project)?)
                        == probe.saved_project,
                    "One keyboard Undo did not exactly restore project before append"
                );
                diagnostic_keyboard(&view, window, cx)?;
                Self::smoke_playback_details(&view, window, cx, history_signature)?;
                probe.stage = 7;
            }
            7 => {
                ensure!(
                    view.read(cx).error.is_none(),
                    "Keyboard fixture autosave failed: {:?}",
                    view.read(cx).error
                );
                let editor = view.read(cx);
                if editor.pending_saves.contains_key(&fixture.sidecar)
                    || !editor
                        .save_states
                        .get(&fixture.sidecar)
                        .is_some_and(|state| {
                            state.status == SaveStatus::Saved && state.in_flight.is_none()
                        })
                {
                    return Ok(false);
                }
                let saved = match Project::load(&fixture.sidecar) {
                    Ok(project) => project,
                    Err(_) => return Ok(false),
                };
                if Some(serde_json::to_value(&saved)?) != probe.saved_project {
                    return Ok(false);
                }
                let (preview, scrub) = view.update(cx, |editor, _| {
                    editor.pause();
                    let jobs = (editor.preview.take(), editor.scrub.take());
                    editor.visible = false;
                    editor.ready = false;
                    editor.recording = None;
                    editor.generation.fetch_add(1, Ordering::Relaxed);
                    editor.pending.clear();
                    editor.pending_saves.remove(&fixture.sidecar);
                    editor.sessions.remove(&fixture.media);
                    jobs
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
                    let _ =
                        tx.send(verify_and_cleanup(fixture).map_err(|error| format!("{error:#}")));
                });
                probe.verifying = Some(rx);
                probe.stage = 5;
            }
            5 => match probe
                .verifying
                .as_ref()
                .context("Missing verification worker")?
                .try_recv()
            {
                Ok(result) => {
                    result.map_err(anyhow::Error::msg)?;
                    probe.stage = 6;
                    println!(
                        "Editor keyboard smoke passed: split, Tab/Enter later-piece selection, coarse/fine/Home/End volume + Undo, preset activation and Escape, cached display-name picker focus/Escape/acceptance, async append + exact Undo, bounded export diagnostics Tab/Shift+Tab containment, Space/Escape focus restoration and blocked editor shortcuts with unchanged Undo/Redo; original media/edits unchanged, owned fixture removed"
                    );
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
