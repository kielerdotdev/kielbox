//! Local import keeps references to originals; validation runs off the UI thread.
use crate::{
    Studio,
    preferences::{MAX_IMPORTED_FILES, Preferences},
    recordings,
};
use gpui::{prelude::*, *};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};

pub const MAX_IMPORT_BATCH: usize = 64;

#[derive(Clone, Copy)]
struct ImportTicket {
    navigation: u64,
    request: u64,
    tab: usize,
    references: u64,
}
#[derive(Default)]
pub(super) struct ImportGuard {
    navigation: u64,
    request: u64,
    tab: Option<usize>,
    references: u64,
    active: usize,
}
impl ImportGuard {
    pub(super) fn observe_navigation(&mut self, tab: usize) {
        if self.tab != Some(tab) {
            self.tab = Some(tab);
            self.navigation = self.navigation.wrapping_add(1);
        }
    }
    pub(super) fn leave_media(&mut self) {
        self.navigation = self.navigation.wrapping_add(1);
    }
    pub(super) fn mutate_references(&mut self) -> u64 {
        self.references = self.references.wrapping_add(1);
        self.references
    }
    pub(super) fn is_idle(&self) -> bool {
        self.active == 0
    }
    fn complete(&mut self) {
        self.active = self.active.saturating_sub(1);
    }
    fn begin(&mut self, tab: usize) -> ImportTicket {
        self.observe_navigation(tab);
        self.request = self.request.wrapping_add(1);
        self.active += 1;
        ImportTicket {
            navigation: self.navigation,
            request: self.request,
            tab,
            references: self.mutate_references(),
        }
    }
    fn can_navigate(&self, ticket: ImportTicket, tab: usize) -> bool {
        ticket.tab == 1
            && tab == 1
            && ticket.navigation == self.navigation
            && ticket.request == self.request
    }
}

pub fn supported(path: &Path) -> bool {
    // GIF and SSXE follow the image editor path, matching Library classification.
    recordings::is_supported(path)
}

#[derive(Default)]
struct ImportBatch {
    added: Vec<PathBuf>,
    available: Vec<PathBuf>,
    open: Option<PathBuf>,
    existing: usize,
    unsupported: usize,
    unavailable: usize,
    over_batch_limit: usize,
    over_library_limit: usize,
    save_failed: usize,
    superseded: usize,
}

fn scan(paths: Vec<PathBuf>, mut known: BTreeSet<PathBuf>, capacity: usize) -> ImportBatch {
    let single = paths.len() == 1;
    let mut batch = ImportBatch {
        over_batch_limit: paths.len().saturating_sub(MAX_IMPORT_BATCH),
        ..Default::default()
    };
    for path in paths.into_iter().take(MAX_IMPORT_BATCH) {
        if !supported(&path) {
            batch.unsupported += 1;
            continue;
        }
        let Ok(path) = path.canonicalize() else {
            batch.unavailable += 1;
            continue;
        };
        if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            batch.unavailable += 1;
            continue;
        }
        if known.contains(&path) {
            batch.existing += 1;
        } else if batch.added.len() >= capacity {
            batch.over_library_limit += 1;
            continue;
        } else {
            known.insert(path.clone());
            batch.added.push(path.clone());
        }
        batch.available.push(path.clone());
        if single {
            batch.open = Some(path);
        }
    }
    batch
}

impl ImportBatch {
    fn message(&self) -> String {
        let mut parts = Vec::new();
        if self.save_failed > 0 {
            parts.push(format!(
                "Couldn’t save {} imported reference{}; no new references were kept. Check storage access, then import again",
                self.save_failed,
                if self.save_failed == 1 { "" } else { "s" },
            ));
        }
        if !self.added.is_empty() {
            parts.push(format!(
                "Added {} capture{}",
                self.added.len(),
                if self.added.len() == 1 { "" } else { "s" }
            ));
        }
        if self.existing > 0 {
            parts.push(format!("{} already in Library", self.existing));
        }
        if self.superseded > 0 {
            parts.push(format!(
                "Skipped {} file(s) removed by a newer Library action",
                self.superseded
            ));
        }
        if self.unsupported > 0 {
            parts.push(format!(
                "Skipped {} unsupported file{}",
                self.unsupported,
                if self.unsupported == 1 { "" } else { "s" }
            ));
        }
        if self.unavailable > 0 {
            parts.push(format!(
                "Skipped {} unavailable file{} or folder{}",
                self.unavailable,
                if self.unavailable == 1 { "" } else { "s" },
                if self.unavailable == 1 { "" } else { "s" }
            ));
        }
        if self.over_batch_limit > 0 {
            parts.push(format!(
                "{} beyond the {MAX_IMPORT_BATCH}-file batch limit",
                self.over_batch_limit
            ));
        }
        if self.over_library_limit > 0 {
            parts.push(format!(
                "{} beyond the {MAX_IMPORTED_FILES}-import Library limit",
                self.over_library_limit
            ));
        }
        if parts.is_empty() {
            "No files selected.".into()
        } else {
            format!("{}. Originals stay in place.", parts.join(" · "))
        }
    }
}

/// Resolve user intent by request order, not by filesystem completion order.
fn reconcile_removal_intents(
    batch: &mut ImportBatch,
    revision: u64,
    intents: &HashMap<PathBuf, u64>,
) {
    let excluded = batch
        .available
        .iter()
        .filter(|path| {
            intents
                .get(*path)
                .is_some_and(|removed| *removed > revision)
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    for path in &excluded {
        if !batch.added.contains(path) {
            batch.existing = batch.existing.saturating_sub(1);
        }
    }
    batch.superseded += excluded.len();
    batch.available.retain(|path| !excluded.contains(path));
    batch.added.retain(|path| !excluded.contains(path));
    if batch
        .open
        .as_ref()
        .is_some_and(|path| excluded.contains(path))
    {
        batch.open = None;
    }
    for path in &batch.available {
        if intents.get(path).is_some_and(|removed| revision > *removed)
            && !batch.added.contains(path)
        {
            // The scan may have seen a card that the earlier Remove had not yet
            // moved. The later explicit Import still means keeping a reference.
            batch.added.push(path.clone());
            batch.existing = batch.existing.saturating_sub(1);
        }
    }
}

/// Commit against current UI state, which may have changed while scanning.
/// A failed preference write rolls back only this batch's newly inserted references.
fn commit_references(
    batch: &mut ImportBatch,
    preferences: &mut Preferences,
    files: &[PathBuf],
    persist: impl FnOnce(&Preferences) -> anyhow::Result<()>,
) -> Result<(), String> {
    let mut inserted = Vec::new();
    for path in batch.added.drain(..) {
        if preferences.imported_files.contains(&path) || files.contains(&path) {
            batch.existing += 1;
        } else if preferences.imported_files.len() >= MAX_IMPORTED_FILES {
            batch.over_library_limit += 1;
        } else {
            preferences.imported_files.insert(path.clone());
            inserted.push(path);
        }
    }
    if !inserted.is_empty()
        && let Err(error) = persist(preferences)
    {
        batch.save_failed = inserted.len();
        for path in inserted {
            preferences.imported_files.remove(&path);
        }
        // A failed import stays in Library so its error remains visible.
        batch.open = None;
        return Err(format!("{error:#}"));
    }
    batch.added = inserted;
    Ok(())
}

impl Studio {
    pub(crate) fn choose_library_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import videos, images or .ssxe projects".into()),
        });
        cx.spawn_in(window, async move |view, cx| match selected.await {
            Ok(Ok(Some(paths))) => {
                let _ = view.update(cx, |this, cx| this.import_library_media(paths, cx));
            }
            Ok(Ok(None)) => {}
            _ => {
                let _ = view.update(cx, |this, cx| {
                    this.notice = Some(
                        "Couldn’t open the file picker. You can also drop files into Library."
                            .into(),
                    );
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn import_library_media(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let known = self
            .files
            .iter()
            .chain(self.preferences.imported_files.iter())
            .cloned()
            .collect();
        let capacity = MAX_IMPORTED_FILES.saturating_sub(self.preferences.imported_files.len());
        let ticket = self.library.import_guard.begin(self.tab);
        self.notice = Some("Adding files to Library…".into());
        cx.notify();
        cx.spawn(async move |view, cx| {
            let batch = cx
                .background_executor()
                .spawn(async move { scan(paths, known, capacity) })
                .await;
            let _ = view.update(cx, |this, cx| {
                let mut batch = batch;
                reconcile_removal_intents(
                    &mut batch,
                    ticket.references,
                    &this.library.removal_intents,
                );
                let known_files = this
                    .files
                    .iter()
                    .filter(|path| {
                        !this
                            .library
                            .removal_intents
                            .get(*path)
                            .is_some_and(|removed| ticket.references > *removed)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let saved = commit_references(
                    &mut batch,
                    &mut this.preferences,
                    &known_files,
                    Preferences::save,
                );
                let available = batch
                    .available
                    .iter()
                    .filter(|path| {
                        this.preferences.imported_files.contains(*path)
                            || this.files.contains(*path)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                this.files.extend(available);
                if this.library.removal_pending && ticket.references > this.library.removal_revision
                {
                    this.library.removal_ref_updates.extend(
                        batch
                            .available
                            .iter()
                            .filter(|p| this.preferences.imported_files.contains(*p))
                            .cloned(),
                    );
                }
                this.library.import_guard.complete();
                if !this.library.removal_pending && this.library.import_guard.is_idle() {
                    this.library.removal_intents.clear();
                }
                this.files.sort();
                this.files.dedup();
                // Supersede any root scan captured before these references existed.
                this.refresh_files();
                this.notice = Some(match saved {
                    Ok(()) => batch.message(),
                    Err(detail) => format!("{} Save error: {detail}", batch.message()),
                });
                if this.library.import_guard.can_navigate(ticket, this.tab) {
                    if let Some(path) = batch.open.filter(|path| this.files.contains(path)) {
                        this.open_recording(path, cx);
                    } else {
                        if !batch.added.is_empty() || batch.existing > 0 {
                            this.library.filter = 0;
                            this.library.favorites_only = false;
                            this.library.page = 0;
                            this.search.update(cx, |input, cx| input.set_value("", cx));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn stale_import_commits_cannot_navigate_after_newer_import_or_leaving_and_returning() {
        let mut guard = ImportGuard::default();
        let old = guard.begin(1);
        assert!(guard.can_navigate(old, 1));
        let latest = guard.begin(1);
        assert!(!guard.can_navigate(old, 1));
        assert!(guard.can_navigate(latest, 1));
        guard.observe_navigation(3);
        guard.observe_navigation(1);
        assert!(!guard.can_navigate(latest, 1));
        let current = guard.begin(1);
        guard.leave_media();
        assert!(!guard.can_navigate(current, 1));
        let background = guard.begin(0);
        assert!(!guard.can_navigate(background, 0));
    }

    #[test]
    fn newer_remove_beats_older_import_but_later_reimport_keeps_a_reference() {
        let path = PathBuf::from("capture.png");
        let intents = HashMap::from([(path.clone(), 2)]);
        let mut older = ImportBatch {
            existing: 1,
            available: vec![path.clone()],
            open: Some(path.clone()),
            ..Default::default()
        };
        reconcile_removal_intents(&mut older, 1, &intents);
        assert!(older.available.is_empty() && older.added.is_empty() && older.open.is_none());
        assert_eq!(older.existing, 0);
        assert_eq!(older.superseded, 1);
        let mut later = ImportBatch {
            existing: 1,
            available: vec![path.clone()],
            open: Some(path.clone()),
            ..Default::default()
        };
        reconcile_removal_intents(&mut later, 3, &intents);
        assert_eq!(later.added, vec![path.clone()]);
        assert_eq!(later.available, vec![path]);
        assert_eq!(later.existing, 0);
        assert_eq!(later.superseded, 0);
    }

    #[test]
    fn failed_import_commit_rolls_back_only_new_references_and_can_be_retried() {
        let concurrent = PathBuf::from("concurrent.mp4");
        let previous = PathBuf::from("previous.mp4");
        let fresh = PathBuf::from("fresh.mp4");
        let mut preferences = Preferences::default();
        preferences
            .imported_files
            .extend([previous, concurrent.clone()]);
        preferences.favorites.insert(fresh.clone());
        preferences
            .display_names
            .insert(fresh.clone(), "Keep this name".into());
        let original = preferences.imported_files.clone();
        let names = preferences.display_names.clone();
        let favorites = preferences.favorites.clone();
        let mut batch = ImportBatch {
            added: vec![concurrent.clone(), fresh.clone()],
            available: vec![concurrent.clone(), fresh.clone()],
            open: Some(fresh.clone()),
            ..Default::default()
        };
        let result = commit_references(&mut batch, &mut preferences, &[], |pending| {
            assert!(pending.imported_files.contains(&concurrent));
            assert!(pending.imported_files.contains(&fresh));
            anyhow::bail!("fixture permission denied")
        });
        assert!(result.unwrap_err().contains("permission denied"));
        assert_eq!(preferences.imported_files, original);
        assert_eq!(preferences.display_names, names);
        assert_eq!(preferences.favorites, favorites);
        assert_eq!(batch.existing, 1);
        assert_eq!(batch.save_failed, 1);
        assert!(batch.added.is_empty() && batch.open.is_none());
        assert!(
            batch
                .message()
                .contains("Couldn’t save 1 imported reference")
        );
        assert!(!batch.message().contains("Added"));

        let mut retry = ImportBatch {
            added: vec![fresh.clone()],
            ..Default::default()
        };
        commit_references(&mut retry, &mut preferences, &[], |pending| {
            assert!(pending.imported_files.contains(&fresh));
            Ok(())
        })
        .unwrap();
        assert_eq!(retry.added, vec![fresh.clone()]);
        assert!(preferences.imported_files.contains(&fresh));
        assert!(original.is_subset(&preferences.imported_files));
    }

    #[test]
    fn import_commit_keeps_concurrently_discovered_files_without_rewriting_preferences() {
        let discovered = PathBuf::from("recordings/new.mp4");
        let mut preferences = Preferences::default();
        let mut batch = ImportBatch {
            added: vec![discovered.clone()],
            ..Default::default()
        };
        commit_references(
            &mut batch,
            &mut preferences,
            std::slice::from_ref(&discovered),
            |_| {
                panic!(
                    "An already-discovered local capture does not require a new import reference"
                )
            },
        )
        .unwrap();
        assert_eq!(batch.existing, 1);
        assert!(batch.added.is_empty());
        assert!(preferences.imported_files.is_empty());
    }

    #[test]
    fn imports_are_case_insensitive_and_keep_image_projects_out_of_video() {
        for name in [
            "recording.MP4",
            "clip.MOV",
            "clip.mKv",
            "picture.JpEg",
            "animation.GIF",
            "edits.SSXE",
        ] {
            assert!(supported(Path::new(name)));
        }
        for name in ["music.mp3", "settings.sxcu", "script.exe", "project.json"] {
            assert!(!supported(Path::new(name)));
        }
        assert!(recordings::is_image(Path::new("animation.GIF")));
        assert!(recordings::is_image(Path::new("edits.SSXE")));
    }

    #[test]
    fn import_scan_deduplicates_and_preserves_original_files() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("clip.MP4");
        std::fs::write(&source, b"original media bytes").unwrap();
        let folder = directory.path().join("folder.mp4");
        std::fs::create_dir(&folder).unwrap();
        let batch = scan(
            vec![
                source.clone(),
                source.clone(),
                directory.path().join("missing.png"),
                folder,
                directory.path().join("unsupported.txt"),
            ],
            BTreeSet::new(),
            512,
        );
        assert_eq!(batch.added, vec![source.canonicalize().unwrap()]);
        assert_eq!(batch.existing, 1);
        assert_eq!(batch.unavailable, 2);
        assert_eq!(batch.unsupported, 1);
        assert!(batch.open.is_none());
        assert_eq!(std::fs::read(&source).unwrap(), b"original media bytes");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn import_scan_bounds_work_and_opens_single_existing_capture() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("clip.png");
        std::fs::write(&source, b"source").unwrap();
        let canonical = source.canonicalize().unwrap();
        let batch = scan(vec![source.clone()], BTreeSet::from([canonical.clone()]), 0);
        assert_eq!(batch.open, Some(canonical));
        assert_eq!(batch.existing, 1);
        let batch = scan(vec![source; MAX_IMPORT_BATCH + 3], BTreeSet::new(), 0);
        assert_eq!(batch.over_batch_limit, 3);
        assert_eq!(batch.over_library_limit, MAX_IMPORT_BATCH);
    }
}
