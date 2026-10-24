//! Reversible local Trash and imported-reference removal share one Undo history.
use crate::{
    Studio,
    preferences::{self, MAX_IMPORTED_FILES, Preferences},
    recordings,
};
use gpui::{prelude::*, *};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Default)]
pub(super) struct RemovalUndo {
    moved: Vec<(PathBuf, PathBuf)>,
    imports_for_moved: BTreeSet<PathBuf>,
    references: BTreeSet<PathBuf>,
}

impl RemovalUndo {
    pub(super) fn is_empty(&self) -> bool {
        self.moved.is_empty() && self.references.is_empty()
    }
    pub(super) fn description(&self) -> String {
        let mut parts = Vec::new();
        if !self.moved.is_empty() {
            parts.push(format!(
                "{} capture{} in Trash",
                self.moved.len(),
                if self.moved.len() == 1 { "" } else { "s" }
            ));
        }
        if !self.references.is_empty() {
            parts.push(format!(
                "{} Library reference{} removed · originals stay in place",
                self.references.len(),
                if self.references.len() == 1 { "" } else { "s" }
            ));
        }
        format!("{} · Undo until rcr closes", parts.join(" · "))
    }
    fn forget_recovered(&mut self, original: &Path, trashed: &Path) -> bool {
        let before = self.moved.len();
        self.moved
            .retain(|(source, destination)| source != original || destination != trashed);
        if self.moved.len() == before || self.moved.iter().any(|(source, _)| source == original) {
            return false;
        }
        self.imports_for_moved.remove(original)
    }
}

pub(super) fn under_root(path: &Path, root: &Path) -> bool {
    let root = preferences::lookup_key(root);
    let path = preferences::lookup_key(path);
    path.starts_with(&root) && !path.starts_with(root.join(".trash"))
}

#[derive(Default)]
struct Outcome {
    completed: BTreeSet<PathBuf>,
    errors: Vec<String>,
}

fn remove(
    paths: &[PathBuf],
    root: &Path,
    preferences: &mut Preferences,
    history: &mut RemovalUndo,
    persist: impl Fn(&Preferences) -> anyhow::Result<()>,
) -> Outcome {
    let original_imports = preferences.imported_files.clone();
    let root = root.canonicalize().unwrap_or_else(|_| root.to_owned());
    let mut removed = RemovalUndo::default();
    let mut outcome = Outcome::default();
    for path in paths.iter().collect::<BTreeSet<_>>() {
        let identity = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        if under_root(&identity, &root) {
            match recordings::trash(std::slice::from_ref(path), &root) {
                Ok(moved) => {
                    if preferences.imported_files.remove(path) {
                        removed.imports_for_moved.insert(path.clone());
                    }
                    removed.moved.extend(moved);
                    outcome.completed.insert(path.clone());
                }
                Err(error) => outcome.errors.push(format!(
                    "Could not move {} to Trash: {error}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )),
            }
        } else if preferences.imported_files.remove(path) {
            removed.references.insert(path.clone());
            outcome.completed.insert(path.clone());
        } else {
            outcome.errors.push(format!(
                "{} is outside your capture folder and has no imported Library reference",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    if preferences.imported_files != original_imports
        && let Err(error) = persist(preferences)
    {
        // Reference changes are transactional. Roll back local moves as far as
        // possible; keep any failed rollback in Undo rather than lose its source.
        preferences.imported_files = original_imports;
        removed.references.clear();
        if let Err(restore_error) = recordings::restore(&mut removed.moved) {
            outcome.errors.push(format!(
                "Some captures remain in Trash; Undo can restore them: {restore_error}"
            ));
        }
        removed
            .imports_for_moved
            .retain(|path| removed.moved.iter().any(|(source, _)| source == path));
        outcome.completed = removed
            .moved
            .iter()
            .map(|(source, _)| source.clone())
            .collect();
        outcome.errors.push(format!(
            "Library references were kept because settings could not be saved: {error}"
        ));
    }
    history.moved.extend(removed.moved);
    history.imports_for_moved.extend(removed.imports_for_moved);
    history.references.extend(removed.references);
    outcome
}

fn restore(
    preferences: &mut Preferences,
    history: &mut RemovalUndo,
    persist: impl Fn(&Preferences) -> anyhow::Result<()>,
) -> Outcome {
    let mut outcome = Outcome::default();
    let before = history.moved.clone();
    if let Err(error) = recordings::restore(&mut history.moved) {
        outcome
            .errors
            .push(format!("Some captures could not be restored: {error}"));
    }
    for (source, _) in &before[history.moved.len()..] {
        outcome.completed.insert(source.clone());
        if history.imports_for_moved.remove(source) {
            history.references.insert(source.clone());
        }
    }
    let original_imports = preferences.imported_files.clone();
    let mut restored = BTreeSet::new();
    for path in &history.references {
        if preferences.imported_files.contains(path) {
            restored.insert(path.clone());
        } else if preferences.imported_files.len() < MAX_IMPORTED_FILES {
            preferences.imported_files.insert(path.clone());
            restored.insert(path.clone());
        } else if !outcome
            .errors
            .iter()
            .any(|error| error.contains("import limit"))
        {
            outcome.errors.push(
                "Library import limit reached. Remove another reference, then retry Undo.".into(),
            );
        }
    }
    if preferences.imported_files != original_imports
        && let Err(error) = persist(preferences)
    {
        preferences.imported_files = original_imports;
        restored.clear();
        outcome.errors.push(format!(
            "Could not save restored Library references; Undo is still available: {error}"
        ));
    }
    for path in restored {
        history.references.remove(&path);
        outcome.completed.insert(path);
    }
    outcome
}

impl Studio {
    pub(crate) fn remove_selected_library(&mut self, cx: &mut Context<Self>) {
        self.begin_library_removal(false, cx);
    }
    pub(crate) fn undo_library_removal(&mut self, cx: &mut Context<Self>) {
        self.begin_library_removal(true, cx);
    }
    fn begin_library_removal(&mut self, undo: bool, cx: &mut Context<Self>) {
        if self.library.removal_pending
            || self.trash_panel.is_some()
            || self.trash_restoring
            || (undo && self.library.removed.is_empty())
        {
            return;
        }
        let paths = self
            .library
            .selected
            .iter()
            .filter(|path| self.files.contains(path))
            .cloned()
            .collect::<Vec<_>>();
        if !undo && paths.is_empty() {
            return;
        }
        self.library.import_guard.leave_media();
        self.library.removal_revision = self.library.import_guard.mutate_references();
        if !undo {
            for path in &paths {
                self.library
                    .removal_intents
                    .insert(path.clone(), self.library.removal_revision);
            }
        }
        let root = recordings::root();
        let mut preferences = self.preferences.clone();
        let before_refs = preferences.imported_files.clone();
        let mut history = std::mem::take(&mut self.library.removed);
        let before_history = history.clone();
        let focus = cx
            .windows()
            .into_iter()
            .find(|w| w.downcast::<Studio>().is_some())
            .and_then(|w| {
                w.update(cx, |_, window, cx| window.focused(cx))
                    .ok()
                    .flatten()
            });
        self.library.removal_pending = true;
        self.library.removal_ref_updates.clear();
        self.notice = Some(
            if undo {
                "Restoring Library captures…"
            } else {
                "Removing captures… Originals in the capture folder stay in its .trash folder."
            }
            .into(),
        );
        let worker = cx.background_executor().spawn(async move {
            // Source filesystem work may block on an offline drive. Only one
            // operation is admitted, and this task never holds a UI entity.
            let outcome = if undo {
                restore(&mut preferences, &mut history, |_| Ok(()))
            } else {
                remove(&paths, &root, &mut preferences, &mut history, |_| Ok(()))
            };
            Work {
                undo,
                before_refs,
                after_refs: preferences.imported_files,
                history,
                before_history,
                outcome,
            }
        });
        cx.spawn(async move |view, cx| {
            let work = worker.await;
            let _ = view.update(cx, |this, cx| {
                this.reconcile_library_removal(work, focus, cx)
            });
        })
        .detach();
        cx.notify();
    }
    fn reconcile_library_removal(
        &mut self,
        mut work: Work,
        focus: Option<FocusHandle>,
        cx: &mut Context<Self>,
    ) {
        let current_refs = self.preferences.imported_files.clone();
        let blocked = merge_reference_delta(
            &work.before_refs,
            &work.after_refs,
            &mut self.preferences.imported_files,
            &self.library.removal_ref_updates,
        );
        work.history.references.extend(blocked.iter().cloned());
        if !blocked.is_empty() {
            work.outcome.errors.push(
                "Library import limit reached. Remove another reference, then retry Undo.".into(),
            );
        }
        // Preserve any import completed after the operation started, including
        // an explicit re-import of the same reference while disk work ran.
        for path in &self.library.removal_ref_updates {
            work.history.references.remove(path);
            if !work.history.moved.iter().any(|(source, _)| source == path) {
                work.outcome.completed.remove(path);
            }
        }
        // Existing short local atomic preference persistence remains on GPUI;
        // all source-drive inspection/moves/rollback are on the bounded worker.
        let saved = if current_refs == self.preferences.imported_files {
            Ok(())
        } else {
            self.preferences.save()
        };
        if let Err(error) = saved {
            self.preferences.imported_files = current_refs;
            work.outcome.errors.push(format!(
                "Library references were kept because settings could not be saved: {error}"
            ));
            if work.undo {
                work.history
                    .references
                    .extend(work.after_refs.difference(&work.before_refs).cloned());
            } else {
                let mut rollback = work
                    .history
                    .moved
                    .split_off(work.before_history.moved.len());
                let imports = work.history.imports_for_moved;
                self.library.removed = work.before_history;
                let worker = cx.background_executor().spawn(async move {
                    let error = recordings::restore(&mut rollback)
                        .err()
                        .map(|error| format!("{error:#}"));
                    (rollback, error)
                });
                cx.spawn(async move |view, cx| {
                    let (remaining, error) = worker.await;
                    let _ = view.update(cx, |this, cx| {
                        let mut result = work.outcome;
                        result.completed =
                            remaining.iter().map(|(source, _)| source.clone()).collect();
                        if let Some(error) = error {
                            result.errors.push(format!(
                                "Some captures remain in Trash; Undo can restore them: {error}"
                            ));
                        }
                        this.library.removed.imports_for_moved.extend(
                            imports
                                .into_iter()
                                .filter(|path| result.completed.contains(path)),
                        );
                        this.library.removed.moved.extend(remaining);
                        this.complete_library_removal(result, false, focus, cx);
                    });
                })
                .detach();
                cx.notify();
                return;
            }
        }
        self.library.removed = work.history;
        self.complete_library_removal(work.outcome, work.undo, focus, cx);
    }
    fn complete_library_removal(
        &mut self,
        result: Outcome,
        undo: bool,
        focus: Option<FocusHandle>,
        cx: &mut Context<Self>,
    ) {
        self.library.removal_pending = false;
        self.library.removal_ref_updates.clear();
        if self.library.import_guard.is_idle() {
            self.library.removal_intents.clear();
        }
        self.library
            .selected
            .retain(|path| !result.completed.contains(path));
        if !undo {
            self.files.retain(|path| {
                !result.completed.contains(path) || self.preferences.imported_files.contains(path)
            });
        }
        self.sync_editor_choices(cx);
        self.refresh_files();
        self.notice = Some(if result.errors.is_empty() {
            format!(
                "{} {} capture{}. {}",
                if undo { "Restored" } else { "Removed" },
                result.completed.len(),
                if result.completed.len() == 1 { "" } else { "s" },
                if undo {
                    ""
                } else {
                    "Undo is available until rcr closes."
                }
            )
        } else {
            result.errors.join(" · ")
        });
        if self.tab == 1 {
            let search = self.search.read(cx).focus_handle(cx);
            let studio = cx.weak_entity();
            if let Some(window) = cx
                .windows()
                .into_iter()
                .find(|w| w.downcast::<Studio>().is_some())
            {
                let _ = window.update(cx, |_, window, _| {
                    window.on_next_frame(move |window, cx| {
                        if !studio
                            .upgrade()
                            .is_some_and(|studio| studio.read(cx).tab == 1)
                        {
                            return;
                        }
                        if focus.as_ref().is_some_and(|focus| focus.is_focused(window))
                            || window.focused(cx).is_none()
                        {
                            search.focus(window);
                        }
                    })
                });
            }
        }
        cx.notify();
    }
    pub(crate) fn open_library_trash(&mut self, cx: &mut Context<Self>) {
        if self.trash_panel.is_some()
            || self.trash_restoring
            || self.library.removal_pending
            || self.name_dialog.is_some()
            || self.audio_picker.is_some()
        {
            return;
        }
        let root = recordings::root();
        let panel = cx.new(|cx| {
            crate::trash_recovery_ui::TrashRecoveryPanel::new(
                root,
                self.trash_focus.clone(),
                self.trash_admission.clone(),
                cx,
            )
        });
        self.attach_trash_panel(panel, cx);
    }
    pub(crate) fn snapshot_library_trash(&mut self, warnings: bool, cx: &mut Context<Self>) {
        let panel = cx.new(|cx| {
            let mut panel = crate::trash_recovery_ui::TrashRecoveryPanel::snapshot(
                self.trash_focus.clone(),
                cx,
            );
            if warnings {
                panel.expand_warnings(cx);
            }
            panel
        });
        self.attach_trash_panel(panel, cx);
    }
    fn attach_trash_panel(
        &mut self,
        panel: Entity<crate::trash_recovery_ui::TrashRecoveryPanel>,
        cx: &mut Context<Self>,
    ) {
        use crate::{trash_recovery::RestoreOutcome, trash_recovery_ui::RecoveryEvent};
        cx.subscribe(&panel, |this, panel, event: &RecoveryEvent, cx| {
            match event {
                RecoveryEvent::Closed => this.trash_panel = None,
                RecoveryEvent::Restoring(busy) => this.trash_restoring = *busy,
                RecoveryEvent::Restored(entry, outcome) => {
                    let imported = this.library.removed.forget_recovered(&entry.original, &entry.trashed);
                    if imported && *outcome == RestoreOutcome::Restored && !this.preferences.imported_files.contains(&entry.original) {
                        let error = if this.preferences.imported_files.len() >= MAX_IMPORTED_FILES {
                            Some("File restored. Library import limit reached; remove another reference, then retry Undo.".into())
                        } else {
                            this.preferences.imported_files.insert(entry.original.clone());
                            this.preferences.save().err().map(|error| {
                                this.preferences.imported_files.remove(&entry.original);
                                format!("File restored, but its Library reference could not be saved: {error}. Undo can retry the reference.")
                            })
                        };
                        if let Some(error) = error {
                            this.library.removed.references.insert(entry.original.clone());
                            panel.update(cx, |panel, cx| panel.set_reference_error(error, cx));
                        }
                    }
                    // This scans the current Library root; the modal's captured
                    // root and the user's current preferences never replace it.
                    this.refresh_files();
                }
            }
            cx.notify();
        }).detach();
        self.trash_panel = Some(panel);
        cx.notify();
    }
}

struct Work {
    undo: bool,
    before_refs: BTreeSet<PathBuf>,
    after_refs: BTreeSet<PathBuf>,
    history: RemovalUndo,
    before_history: RemovalUndo,
    outcome: Outcome,
}

/// Apply a worker's precise reference delta to the latest preferences. Never
/// replace favorites, display names, settings, or unrelated/newer references.
fn merge_reference_delta(
    before: &BTreeSet<PathBuf>,
    after: &BTreeSet<PathBuf>,
    current: &mut BTreeSet<PathBuf>,
    newer: &BTreeSet<PathBuf>,
) -> BTreeSet<PathBuf> {
    for path in before.difference(after) {
        if !newer.contains(path) {
            current.remove(path);
        }
    }
    let mut blocked = BTreeSet::new();
    for path in after.difference(before) {
        if !current.contains(path) {
            if current.len() < MAX_IMPORTED_FILES {
                current.insert(path.clone());
            } else {
                blocked.insert(path.clone());
            }
        }
    }
    blocked
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn worker_reference_delta_preserves_later_imports_reimports_and_other_preferences() {
        let removed = PathBuf::from("removed.png");
        let reimported = PathBuf::from("reimported.png");
        let later = PathBuf::from("later.png");
        let before = BTreeSet::from([removed.clone(), reimported.clone()]);
        let mut preferences = Preferences {
            imported_files: BTreeSet::from([removed.clone(), reimported.clone(), later.clone()]),
            ..Default::default()
        };
        preferences.favorites.insert(later.clone());
        preferences
            .display_names
            .insert(later.clone(), "Newer name".into());
        preferences.bitrate = 50_000_000;
        let blocked = merge_reference_delta(
            &before,
            &BTreeSet::new(),
            &mut preferences.imported_files,
            &BTreeSet::from([reimported.clone()]),
        );
        assert!(blocked.is_empty());
        assert_eq!(
            preferences.imported_files,
            BTreeSet::from([reimported, later.clone()])
        );
        assert!(preferences.favorites.contains(&later));
        assert_eq!(preferences.display_names[&later], "Newer name");
        assert_eq!(preferences.bitrate, 50_000_000);
    }

    #[test]
    fn worker_undo_delta_never_evicts_newer_imports_to_make_room() {
        let mut current = (0..MAX_IMPORTED_FILES)
            .map(|i| PathBuf::from(format!("new-{i}.png")))
            .collect::<BTreeSet<_>>();
        let expected = current.clone();
        let old = PathBuf::from("restore-old.png");
        let blocked = merge_reference_delta(
            &BTreeSet::new(),
            &BTreeSet::from([old.clone()]),
            &mut current,
            &BTreeSet::new(),
        );
        assert_eq!(current, expected);
        assert_eq!(blocked, BTreeSet::from([old]));
    }

    #[test]
    fn external_and_offline_references_remove_and_undo_without_touching_sources() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("recordings");
        std::fs::create_dir(&root).unwrap();
        let external = directory.path().join("external.MP4");
        std::fs::write(&external, b"original media").unwrap();
        let external = external.canonicalize().unwrap();
        let sidecar = external.with_extension("ssxe");
        std::fs::write(&sidecar, b"original sidecar").unwrap();
        let offline = directory.path().join("offline.png");
        let mut preferences = Preferences::default();
        preferences
            .imported_files
            .extend([external.clone(), offline.clone()]);
        preferences.favorites.insert(external.clone());
        preferences
            .set_display_name(&external, "Named import")
            .unwrap();
        let mut history = RemovalUndo::default();
        let result = remove(
            &[external.clone(), offline.clone()],
            &root,
            &mut preferences,
            &mut history,
            |_| Ok(()),
        );
        assert_eq!(result.completed.len(), 2);
        assert!(result.errors.is_empty());
        assert!(preferences.imported_files.is_empty());
        let persisted: Preferences =
            serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert!(persisted.imported_files.is_empty());
        assert!(persisted.favorites.contains(&external));
        assert_eq!(
            persisted.custom_display_name(&external),
            Some("Named import")
        );
        assert_eq!(std::fs::read(&external).unwrap(), b"original media");
        assert_eq!(std::fs::read(&sidecar).unwrap(), b"original sidecar");
        let result = restore(&mut preferences, &mut history, |_| Ok(()));
        assert_eq!(result.completed.len(), 2);
        assert!(history.is_empty());
        assert!(preferences.imported_files.contains(&external));
        assert!(preferences.imported_files.contains(&offline));
        let persisted: Preferences =
            serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert!(
            persisted.imported_files.contains(&external)
                && persisted.imported_files.contains(&offline)
        );
    }

    #[test]
    fn imported_local_capture_uses_trash_and_partial_undo_keeps_pending_work() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("recordings");
        std::fs::create_dir(&root).unwrap();
        let first = root.join("first.mp4");
        let second = root.join("second.mp4");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let first = first.canonicalize().unwrap();
        let second = second.canonicalize().unwrap();
        let mut preferences = Preferences::default();
        preferences
            .imported_files
            .extend([first.clone(), second.clone()]);
        let mut history = RemovalUndo::default();
        assert_eq!(
            remove(
                &[first.clone(), second.clone()],
                &root,
                &mut preferences,
                &mut history,
                |_| Ok(())
            )
            .completed
            .len(),
            2
        );
        assert!(!first.exists() && !second.exists());
        assert!(preferences.imported_files.is_empty());
        // Last move restores first; the earlier move is blocked by a new source.
        let blocked = history.moved[0].0.clone();
        std::fs::write(&blocked, b"new capture").unwrap();
        let result = restore(&mut preferences, &mut history, |_| Ok(()));
        assert_eq!(result.completed.len(), 1);
        assert_eq!(history.moved.len(), 1);
        assert!(!result.errors.is_empty());
        assert!(!preferences.imported_files.contains(&blocked));
        assert_eq!(std::fs::read(&blocked).unwrap(), b"new capture");
        std::fs::remove_file(&blocked).unwrap();
        assert!(
            restore(&mut preferences, &mut history, |_| Ok(()))
                .errors
                .is_empty()
        );
        assert!(history.is_empty());
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
        assert_eq!(std::fs::read(&second).unwrap(), b"second");
        assert!(
            preferences.imported_files.contains(&first)
                && preferences.imported_files.contains(&second)
        );
    }

    #[test]
    fn failed_reference_save_keeps_reference_and_failed_undo_can_retry() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("recordings");
        std::fs::create_dir(&root).unwrap();
        let offline = directory.path().join("offline.png");
        let mut preferences = Preferences::default();
        preferences.imported_files.insert(offline.clone());
        let mut history = RemovalUndo::default();
        let result = remove(
            std::slice::from_ref(&offline),
            &root,
            &mut preferences,
            &mut history,
            |_| anyhow::bail!("disk full"),
        );
        assert!(result.completed.is_empty());
        assert!(preferences.imported_files.contains(&offline));
        assert!(history.is_empty());
        remove(
            std::slice::from_ref(&offline),
            &root,
            &mut preferences,
            &mut history,
            |_| Ok(()),
        );
        assert!(
            !restore(&mut preferences, &mut history, |_| anyhow::bail!(
                "disk full"
            ))
            .errors
            .is_empty()
        );
        assert!(!history.is_empty());
        assert!(!preferences.imported_files.contains(&offline));
        assert!(
            restore(&mut preferences, &mut history, |_| Ok(()))
                .errors
                .is_empty()
        );
        assert!(history.is_empty());
        assert!(preferences.imported_files.contains(&offline));
    }

    #[test]
    fn mixed_removal_keeps_failed_local_reference_while_other_removals_are_reversible() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("recordings");
        std::fs::create_dir(&root).unwrap();
        let local = root.join("local.mp4");
        let external = directory.path().join("external.mp4");
        std::fs::write(&local, b"local").unwrap();
        std::fs::write(&external, b"external").unwrap();
        let local = local.canonicalize().unwrap();
        let external = external.canonicalize().unwrap();
        let missing_local = root.canonicalize().unwrap().join("offline-local.mp4");
        let mut preferences = Preferences::default();
        preferences
            .imported_files
            .extend([local.clone(), external.clone(), missing_local.clone()]);
        let mut history = RemovalUndo::default();
        let result = remove(
            &[local.clone(), external.clone(), missing_local.clone()],
            &root,
            &mut preferences,
            &mut history,
            |_| Ok(()),
        );
        assert_eq!(
            result.completed,
            BTreeSet::from([local.clone(), external.clone()])
        );
        assert_eq!(result.errors.len(), 1);
        assert!(preferences.imported_files.contains(&missing_local));
        assert!(!local.exists());
        assert_eq!(std::fs::read(&external).unwrap(), b"external");
        assert!(
            restore(&mut preferences, &mut history, |_| Ok(()))
                .errors
                .is_empty()
        );
        assert!(history.is_empty());
        assert_eq!(std::fs::read(&local).unwrap(), b"local");
        assert!(
            preferences.imported_files.contains(&local)
                && preferences.imported_files.contains(&external)
                && preferences.imported_files.contains(&missing_local)
        );
    }
}
