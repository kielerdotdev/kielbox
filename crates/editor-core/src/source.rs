//! Worker-side source checks. A stamp is conservative metadata, not a content hash.
use crate::{MediaAsset, Project};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStamp {
    pub canonical_path: PathBuf,
    pub bytes: u64,
    pub modified: SystemTime,
}
impl SourceStamp {
    /// Filesystem I/O: call from a worker, never from rendering or input handlers.
    pub fn read(path: &Path) -> Result<Self> {
        (|| -> Result<Self> {
            let canonical_path = path.canonicalize()?;
            let metadata = std::fs::metadata(&canonical_path)?;
            ensure!(metadata.is_file(), "source is not a regular file");
            Ok(Self {
                canonical_path,
                bytes: metadata.len(),
                modified: metadata.modified()?,
            })
        })()
        .with_context(|| format!("Source unavailable: {}", path.display()))
    }
}

/// Legacy projects have no stored identity. Check their stored media description
/// against a fresh probe; this cannot prove that identical-looking bytes are original.
pub fn validate_probed_source(saved: &MediaAsset, current: &MediaAsset) -> Result<()> {
    ensure!(
        saved.width == current.width
            && saved.height == current.height
            && saved.duration_ns.abs_diff(current.duration_ns) <= 1_000_000
            && saved.streams.len() == current.streams.len()
            && saved.streams.iter().all(|s| current
                .streams
                .iter()
                .any(|c| c.index == s.index && c.kind == s.kind)),
        "Source changed; review the original before using saved edits: {}",
        saved.path.display()
    );
    Ok(())
}

/// Validate without changing projects, sidecars, or undo history. Returns stamps
/// in asset order so a caller may explicitly adopt a verified legacy baseline.
/// Stamped sources require only metadata; legacy sources are probed once per path.
pub fn validate_sources_cancellable(
    project: &Project,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<SourceStamp>> {
    validate_with(project, &cancelled, |path| {
        crate::probe_cancellable(path, &cancelled)
    })
}
fn validate_with(
    project: &Project,
    cancelled: &impl Fn() -> bool,
    mut probe: impl FnMut(&Path) -> Result<MediaAsset>,
) -> Result<Vec<SourceStamp>> {
    ensure!(!cancelled(), "Source validation cancelled");
    let mut sources: HashMap<PathBuf, (SourceStamp, Option<MediaAsset>)> = HashMap::new();
    let mut aliases: HashMap<&Path, PathBuf> = HashMap::new();
    let mut stamps = Vec::with_capacity(project.assets.len());
    for asset in &project.assets {
        ensure!(!cancelled(), "Source validation cancelled");
        // Most asset paths are canonical already. Aliases share the probe cache too.
        let key = match aliases.get(asset.path.as_path()) {
            Some(key) => key.clone(),
            None => {
                let key = asset.path.canonicalize()
                    .with_context(|| format!("Source unavailable: {}", asset.path.display()))?;
                aliases.insert(asset.path.as_path(), key.clone());
                key
            }
        };
        let (stamp, probed) = match sources.entry(key.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert((SourceStamp::read(&key)?, None))
            }
        };
        if let Some(saved) = &asset.source_stamp {
            ensure!(
                saved == stamp,
                "Source changed; review the original before using saved edits: {}",
                asset.path.display()
            );
        } else {
            if probed.is_none() {
                let current = probe(&key)?;
                ensure!(
                    current.source_stamp.as_ref() == Some(stamp),
                    "Source changed while validating: {}",
                    asset.path.display()
                );
                *probed = Some(current);
            }
            validate_probed_source(asset, probed.as_ref().unwrap())?;
        }
        stamps.push(stamp.clone());
    }
    ensure!(!cancelled(), "Source validation cancelled");
    // Catch replacements during a multi-asset check, including legacy probing.
    for (path, (stamp, _)) in sources {
        ensure!(!cancelled(), "Source validation cancelled");
        ensure!(
            SourceStamp::read(&path)? == stamp,
            "Source changed while validating: {}",
            path.display()
        );
    }
    ensure!(!cancelled(), "Source validation cancelled");
    Ok(stamps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MediaStream, TrackKind};
    fn asset(path: PathBuf) -> MediaAsset {
        MediaAsset {
            id: 1,
            path,
            source_stamp: None,
            duration_ns: 3_000_000_000,
            width: 1280,
            height: 720,
            streams: vec![
                MediaStream {
                    index: 0,
                    kind: TrackKind::Video,
                    name: "Video".into(),
                },
                MediaStream {
                    index: 1,
                    kind: TrackKind::Audio,
                    name: "Audio".into(),
                },
            ],
        }
    }
    #[test]
    fn legacy_json_defaults_and_exact_stamp_round_trip() {
        let original = asset("legacy.mkv".into());
        let json = serde_json::to_value(&original).unwrap();
        assert!(json.get("source_stamp").is_none());
        let restored: MediaAsset = serde_json::from_value(json).unwrap();
        assert!(restored.source_stamp.is_none());
        let stamp = SourceStamp {
            canonical_path: "C:/source.mkv".into(),
            bytes: 42,
            modified: SystemTime::UNIX_EPOCH + std::time::Duration::new(123, 987654321),
        };
        assert_eq!(
            serde_json::from_slice::<SourceStamp>(&serde_json::to_vec(&stamp).unwrap()).unwrap(),
            stamp
        );
    }
    #[test]
    fn legacy_metadata_rejects_shorter_geometry_or_stream_changes() {
        let saved = asset("legacy.mkv".into());
        let mut current = saved.clone();
        current.duration_ns += 1_000_000;
        current.streams.reverse();
        validate_probed_source(&saved, &current).unwrap();
        current.duration_ns = saved.duration_ns - 1_000_001;
        assert!(validate_probed_source(&saved, &current).is_err());
        current = saved.clone();
        current.width += 1;
        assert!(validate_probed_source(&saved, &current).is_err());
        current = saved.clone();
        current.streams[1].index = 2;
        assert!(validate_probed_source(&saved, &current).is_err());
        current = saved.clone();
        current.streams[1].kind = TrackKind::Video;
        assert!(validate_probed_source(&saved, &current).is_err());
        current = saved.clone();
        current.streams.pop();
        assert!(validate_probed_source(&saved, &current).is_err());
    }
    #[test]
    fn stamped_source_never_probes_and_replacement_or_offline_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.mkv");
        std::fs::write(&path, b"original").unwrap();
        let mut source = asset(path.clone());
        source.source_stamp = Some(SourceStamp::read(&path).unwrap());
        let mut project = Project::default();
        project.assets.push(source);
        validate_with(&project, &|| false, |_| {
            panic!("unchanged stamped source must not probe")
        })
        .unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(
            project.assets[0].source_stamp.as_ref().unwrap().modified
                + std::time::Duration::from_secs(2),
        ))
        .unwrap();
        assert!(
            validate_with(&project, &|| false, |_| panic!(
                "mtime-only change must not probe"
            ))
            .is_err()
        );
        std::fs::write(&path, b"replacement-longer").unwrap();
        assert!(
            validate_with(&project, &|| false, |_| panic!(
                "changed stamp must not probe"
            ))
            .is_err()
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            validate_with(&project, &|| false, |_| panic!(
                "offline source must not probe"
            ))
            .is_err()
        );
    }
    #[test]
    fn legacy_sources_probe_once_without_mutating_project_and_detect_races() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.mkv");
        std::fs::write(&path, b"original").unwrap();
        let source = asset(path.clone());
        let project = Project {
            assets: vec![source.clone(), source.clone()],
            ..Project::default()
        };
        let before = serde_json::to_value(&project).unwrap();
        let mut count = 0;
        let stamps = validate_with(&project, &|| false, |_| {
            count += 1;
            let mut current = source.clone();
            current.source_stamp = Some(SourceStamp::read(&path)?);
            Ok(current)
        })
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(stamps.len(), 2);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert!(
            validate_with(&project, &|| false, |_| {
                let mut current = source.clone();
                current.source_stamp = Some(SourceStamp::read(&path)?);
                std::fs::write(&path, b"changed during probe")?;
                Ok(current)
            })
            .is_err()
        );
        assert!(
            validate_with(&project, &|| true, |_| panic!(
                "cancelled check must not probe"
            ))
            .is_err()
        );
    }
    #[test]
    fn rejected_source_and_cancelled_export_create_no_output_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.mkv");
        std::fs::write(&path, b"original").unwrap();
        let mut source = asset(path.clone());
        source.source_stamp = Some(SourceStamp::read(&path).unwrap());
        let mut project = Project::default();
        project.assets.push(source);
        std::fs::write(&path, b"replacement-longer").unwrap();
        let output = dir.path().join("never-created/result.mp4");
        let options = crate::ExportOptions {
            output: output.clone(),
            start: 0,
            end: None,
            audio: crate::AudioExport::Mix,
            bitrate: 10_000_000,
        };
        let progress = crate::JobProgress::default();
        assert!(crate::export(&project, &options, &progress).is_err());
        assert!(!output.parent().unwrap().exists());
        progress
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(crate::export(&Project::default(), &options, &progress).is_err());
        assert!(!output.parent().unwrap().exists());
    }
    #[test]
    fn editor_checks_archived_history_and_preserves_it_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.mkv");
        std::fs::write(&path, b"original").unwrap();
        let mut source = asset(path.clone());
        source.source_stamp = Some(SourceStamp::read(&path).unwrap());
        let project = Project {
            assets: vec![source],
            next_id: 2,
            ..Project::default()
        };
        let before = serde_json::to_value(&project).unwrap();
        let mut editor = crate::Editor::new(project);
        editor
            .execute(crate::Edit::Reframe {
                vertical: true,
                center: 0.25,
            })
            .unwrap();
        std::fs::write(&path, b"replacement-longer").unwrap();
        // Even if a caller had updated the active state, archived history must
        // never silently restore old metadata against the replacement bytes.
        editor.project.assets[0].source_stamp = Some(SourceStamp::read(&path).unwrap());
        let active = serde_json::to_value(&editor.project).unwrap();
        assert!(editor.validate_sources_cancellable(|| false).is_err());
        assert_eq!(serde_json::to_value(&editor.project).unwrap(), active);
        assert!(editor.undo());
        assert_eq!(serde_json::to_value(&editor.project).unwrap(), before);
        assert!(editor.redo());
        assert_eq!(serde_json::to_value(&editor.project).unwrap(), active);
    }
}
