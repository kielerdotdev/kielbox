//! Recording edits preserve timeline placement. Explicit reorder packs pieces;
//! each piece owns its video and all associated audio.
use crate::{Content, Edit, Editor, Id, MediaAsset, Project, TrackKind};
use anyhow::{ensure, Context, Result};

#[derive(Clone, Debug)]
pub enum QuickEdit {
    Add(MediaAsset),
    Split { id: Id, at: u64 },
    Delete(Id),
    TrimStart { id: Id, to: i64 },
    TrimEnd { id: Id, to: u64 },
    Speed { id: Id, value: f64 },
    Reorder { id: Id, index: usize },
}

pub fn pieces(project: &Project) -> Vec<&crate::Clip> {
    let mut clips: Vec<_> = project
        .clips
        .iter()
        .filter(|clip| {
            project
                .tracks
                .iter()
                .any(|t| t.id == clip.track && t.kind == TrackKind::Video)
        })
        .collect();
    clips.sort_by_key(|clip| clip.start);
    clips
}

/// Preserve useful source labels, without exposing stream indices to people.
pub fn audio_name(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.starts_with("application:") || lower.starts_with("application audio") {
        name.to_owned()
    } else if lower.contains("mic") {
        "Microphone".into()
    } else if lower.contains("system") || lower.contains("desktop") || lower.contains("game") {
        "Game".into()
    } else if lower.contains("discord") {
        "Discord".into()
    } else if lower.starts_with("audio") || lower == "soundhandler" || lower.is_empty() {
        "Recorded sound".into()
    } else {
        name.to_owned()
    }
}

pub(crate) fn apply(project: &mut Project, edit: QuickEdit) -> Result<()> {
    let mut order: Vec<_> = pieces(project).iter().map(|c| c.group).collect();
    let pack = matches!(&edit, QuickEdit::Reorder { .. });
    let mut editor = Editor::new(project.clone());
    match edit {
        QuickEdit::Add(mut asset) => {
            ensure!(
                asset.streams.iter().any(|s| s.kind == TrackKind::Video),
                "Choose a recording with video"
            );
            let mut video = false;
            let mut names = std::collections::HashSet::new();
            asset.streams.retain_mut(|s| {
                if s.kind == TrackKind::Video {
                    let keep = !video;
                    video = true;
                    keep
                } else {
                    let base = audio_name(&s.name);
                    s.name = base.clone();
                    let mut suffix = 2;
                    while !names.insert(s.name.clone()) {
                        s.name = format!("{base} ({suffix})");
                        suffix += 1;
                    }
                    true
                }
            });
            if order.is_empty() && project.assets.is_empty() {
                editor.project.width = asset.width;
                editor.project.height = asset.height;
            }
            editor.execute(Edit::Import {
                asset,
                at: editor.project.duration(),
            })?;
        }
        QuickEdit::Split { id, at } => {
            let group = project
                .clips
                .iter()
                .find(|c| c.id == id)
                .context("Select a piece")?
                .group;
            ensure!(order.contains(&group), "Select a video piece");
            editor.execute(Edit::Split { id, at })?;
        }
        QuickEdit::Delete(id) => {
            editor.execute(Edit::Delete { id, ripple: false })?;
        }
        QuickEdit::TrimStart { id, to } => {
            let to = u64::try_from(to).context("Trim precedes the timeline")?;
            let (previous_end, _) = neighbor_bounds(project, id)?;
            ensure!(to >= previous_end, "Trim overlaps the previous piece");
            editor.execute(Edit::TrimLeft { id, to })?;
        }
        QuickEdit::TrimEnd { id, to } => {
            let (_, next_start) = neighbor_bounds(project, id)?;
            ensure!(next_start.is_none_or(|next| to <= next), "Trim overlaps the next piece");
            editor.execute(Edit::TrimRight { id, to })?;
        }
        QuickEdit::Speed { id, value } => {
            let (_, next_start) = neighbor_bounds(project, id)?;
            editor.execute(Edit::Speed { id, value })?;
            let clip = editor.project.clips.iter().find(|clip| clip.id == id).context("Select a piece")?;
            ensure!(next_start.is_none_or(|next| clip.end() <= next), "Speed change overlaps the next piece");
        }
        QuickEdit::Reorder { id, index } => {
            let group = project
                .clips
                .iter()
                .find(|c| c.id == id)
                .context("Select a piece")?
                .group;
            order.retain(|g| *g != group);
            order.insert(index.min(order.len()), group);
        }
    }
    let mut cursor = 0;
    for group in order.into_iter().filter(|_| pack) {
        let duration = pieces(&editor.project)
            .iter()
            .find(|c| c.group == group)
            .context("Missing video piece")?
            .duration;
        for clip in editor.project.clips.iter_mut().filter(|c| c.group == group) {
            clip.start = cursor;
            clip.duration = duration;
        }
        cursor += duration;
    }
    // Keep the session bounded after deleting recordings and all their pieces.
    editor.project.assets.retain(|a| {
        editor
            .project
            .clips
            .iter()
            .any(|c| matches!(c.content, Content::Media { asset, .. } if asset == a.id))
    });
    *project = editor.project;
    Ok(())
}
fn neighbor_bounds(project: &Project, id: Id) -> Result<(u64, Option<u64>)> {
    let group = project.clips.iter().find(|clip| clip.id == id).context("Select a piece")?.group;
    let ordered = pieces(project);
    let index = ordered.iter().position(|clip| clip.group == group).context("Select a video piece")?;
    Ok((index.checked_sub(1).map_or(0, |index| ordered[index].end()),
        ordered.get(index + 1).map(|clip| clip.start)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MediaStream;
    #[test]
    fn explicit_application_labels_survive_recorder_aliases_and_import() {
        for name in ["Application: MicGameSystem.exe", "application: Discord", "Application audio (PID 42)"] {
            assert_eq!(audio_name(name), name);
        }
        assert_eq!(audio_name("System audio"), "Game");
        assert_eq!(audio_name("Microphone (DPDFNet)"), "Microphone");
        let name = "Application: MicGameSystem.exe";
        let mut media = asset();
        media.streams[1].name = name.into();
        let mut editor = Editor::default();
        editor.execute(Edit::Sequence(QuickEdit::Add(media))).unwrap();
        assert!(editor.project.tracks.iter().any(|track| track.kind == TrackKind::Audio && track.name == name));
        assert!(editor.project.assets[0].streams.iter().any(|stream| stream.name == name));
        assert!(editor.project.tracks.iter().any(|track| track.name == "Microphone"));
    }
    fn asset() -> MediaAsset {
        MediaAsset {
            id: 0,
            source_stamp: None,
            path: "fixture.mkv".into(),
            duration_ns: 10_000_000_000,
            width: 1920,
            height: 1080,
            streams: [
                (0, TrackKind::Video, "Video 0"),
                (1, TrackKind::Audio, "System audio"),
                (2, TrackKind::Audio, "Microphone"),
                (3, TrackKind::Audio, "Discord"),
            ]
            .into_iter()
            .map(|(index, kind, name)| MediaStream {
                index,
                kind,
                name: name.into(),
            })
            .collect(),
        }
    }
    fn assert_contiguous(p: &Project) {
        let mut cursor = 0;
        for piece in pieces(p) {
            assert_eq!(piece.start, cursor);
            for c in p.clips.iter().filter(|c| c.group == piece.group) {
                assert_eq!(
                    (c.start, c.duration, c.source_in_ns, c.speed),
                    (piece.start, piece.duration, piece.source_in_ns, piece.speed)
                );
            }
            cursor = piece.end();
        }
        p.validate().unwrap();
    }
    fn assert_linked(p: &Project) {
        for piece in pieces(p) {
            for clip in p.clips.iter().filter(|clip| clip.group == piece.group) {
                assert_eq!((clip.start, clip.duration, clip.source_in_ns, clip.speed),
                    (piece.start, piece.duration, piece.source_in_ns, piece.speed));
            }
        }
        p.validate().unwrap();
    }
    #[test]
    fn cuts_speed_reorder_and_delete_keep_all_audio_in_sync() {
        let mut e = Editor::default();
        e.execute(Edit::Sequence(QuickEdit::Add(asset()))).unwrap();
        assert_eq!((e.project.width, e.project.height), (1920, 1080));
        assert_eq!(
            e.project
                .tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Audio)
                .count(),
            3
        );
        let first = pieces(&e.project)[0].id;
        e.execute(Edit::Sequence(QuickEdit::TrimStart { id: first, to: 60 }))
            .unwrap();
        assert_eq!(pieces(&e.project)[0].source_in_ns, 1_000_000_000);
        e.execute(Edit::Sequence(QuickEdit::Split { id: first, at: 120 }))
            .unwrap();
        let right = pieces(&e.project)[1].id;
        e.execute(Edit::Sequence(QuickEdit::Speed {
            id: first,
            value: 2.,
        }))
        .unwrap();
        assert_linked(&e.project);
        e.execute(Edit::Sequence(QuickEdit::Reorder {
            id: right,
            index: 0,
        }))
        .unwrap();
        assert_eq!(pieces(&e.project)[0].id, right);
        assert_contiguous(&e.project);
        e.execute(Edit::Sequence(QuickEdit::Delete(first))).unwrap();
        assert_contiguous(&e.project);
        assert!(e.undo());
        assert_eq!(pieces(&e.project).len(), 2);
        assert_contiguous(&e.project);
        assert!(e.redo());
        assert_contiguous(&e.project);
    }
    #[test]
    fn append_joins_and_failed_trim_is_atomic() {
        let mut e = Editor::default();
        for _ in 0..2 {
            e.execute(Edit::Sequence(QuickEdit::Add(asset()))).unwrap();
        }
        assert_contiguous(&e.project);
        assert_eq!(e.project.duration(), 1200);
        let id = pieces(&e.project)[0].id;
        assert!(e
            .execute(Edit::Sequence(QuickEdit::TrimEnd { id, to: 900 }))
            .is_err());
        assert_eq!(e.project.duration(), 1200);
        assert_contiguous(&e.project);
    }
    #[test]
    fn first_edge_can_restore_source_and_global_audio_survives_join() {
        let mut e = Editor::default();
        e.execute(Edit::Sequence(QuickEdit::Add(asset()))).unwrap();
        let id = pieces(&e.project)[0].id;
        e.execute(Edit::Sequence(QuickEdit::TrimStart { id, to: 120 }))
            .unwrap();
        e.execute(Edit::Sequence(QuickEdit::TrimStart { id, to: 60 }))
            .unwrap();
        assert_eq!(pieces(&e.project)[0].source_in_ns, 1_000_000_000);
        assert_eq!(pieces(&e.project)[0].start, 60);
        assert_eq!(pieces(&e.project)[0].end(), 600);
        assert_linked(&e.project);
        let mic = e
            .project
            .tracks
            .iter()
            .find(|t| t.name == "Microphone")
            .unwrap()
            .id;
        e.execute(Edit::MuteTrack { id: mic }).unwrap();
        e.execute(Edit::TrackGain {
            id: mic,
            value: 0.7,
        })
        .unwrap();
        e.execute(Edit::Sequence(QuickEdit::Add(asset()))).unwrap();
        let mic_tracks: Vec<_> = e
            .project
            .tracks
            .iter()
            .filter(|t| t.name == "Microphone")
            .collect();
        assert_eq!(mic_tracks.len(), 1);
        assert!(mic_tracks[0].muted);
        assert_eq!(mic_tracks[0].gain, 0.7);
        assert_linked(&e.project);
        assert!(e
            .execute(Edit::Sequence(QuickEdit::TrimStart { id, to: i64::MIN }))
            .is_err());
    }
    #[test]
    fn trim_start_preserves_right_edge_neighbors_and_linked_audio_through_restore_and_history() {
        let mut editor = Editor::default();
        for _ in 0..2 { editor.execute(Edit::Sequence(QuickEdit::Add(asset()))).unwrap(); }
        let id = pieces(&editor.project)[0].id;
        let neighbor = pieces(&editor.project)[1].clone();
        for to in [120, 60, 90, 0, 180] {
            editor.execute(Edit::Sequence(QuickEdit::TrimStart { id, to })).unwrap();
            let first = pieces(&editor.project)[0];
            assert_eq!((first.start, first.end(), first.source_in_ns),
                (to as u64, 600, to as u64 * 1_000_000_000 / 60));
            assert_eq!(pieces(&editor.project)[1].start, neighbor.start);
            assert_eq!(pieces(&editor.project)[1].end(), neighbor.end());
            assert_linked(&editor.project);
        }
        let trimmed = serde_json::to_value(&editor.project).unwrap();
        assert!(editor.undo());
        assert_eq!(pieces(&editor.project)[0].start, 0);
        assert!(editor.redo());
        assert_eq!(serde_json::to_value(&editor.project).unwrap(), trimmed);
        for to in [-1, i64::MIN, 600, 9999] {
            assert!(editor.execute(Edit::Sequence(QuickEdit::TrimStart { id, to })).is_err());
            assert_eq!(serde_json::to_value(&editor.project).unwrap(), trimmed);
        }
    }
    #[test]
    fn gaps_survive_add_split_speed_and_delete_until_explicit_reorder() {
        let mut editor = Editor::default();
        editor.execute(Edit::Sequence(QuickEdit::Add(asset()))).unwrap();
        let first = pieces(&editor.project)[0].id;
        editor.execute(Edit::Sequence(QuickEdit::TrimStart { id: first, to: 120 })).unwrap();
        editor.execute(Edit::Sequence(QuickEdit::TrimEnd { id: first, to: 480 })).unwrap();
        editor.execute(Edit::Sequence(QuickEdit::Add(asset()))).unwrap();
        assert_eq!(pieces(&editor.project)[0].start, 120);
        assert_eq!(pieces(&editor.project)[1].start, 480);
        editor.execute(Edit::Sequence(QuickEdit::Split { id: first, at: 240 })).unwrap();
        let right = pieces(&editor.project)[1].id;
        assert_eq!(pieces(&editor.project).iter().map(|clip| clip.start).collect::<Vec<_>>(), [120, 240, 480]);
        editor.execute(Edit::Sequence(QuickEdit::Speed { id: first, value: 2. })).unwrap();
        assert_eq!(pieces(&editor.project)[0].end(), 180);
        assert_eq!(pieces(&editor.project)[1].start, 240);
        editor.execute(Edit::Sequence(QuickEdit::Delete(first))).unwrap();
        assert_eq!(pieces(&editor.project).iter().map(|clip| clip.start).collect::<Vec<_>>(), [240, 480]);
        assert_linked(&editor.project);
        editor.execute(Edit::Sequence(QuickEdit::Reorder { id: right, index: 0 })).unwrap();
        assert_contiguous(&editor.project);
    }
    #[test]
    fn quick_edge_extension_rejects_neighbor_overlap_and_source_overrun_atomically() {
        let mut editor = Editor::default();
        for _ in 0..2 { editor.execute(Edit::Sequence(QuickEdit::Add(asset()))).unwrap(); }
        let left = pieces(&editor.project)[0].id;
        let right = pieces(&editor.project)[1].id;
        editor.execute(Edit::Sequence(QuickEdit::TrimEnd { id: left, to: 480 })).unwrap();
        editor.execute(Edit::Sequence(QuickEdit::TrimStart { id: right, to: 720 })).unwrap();
        editor.execute(Edit::Sequence(QuickEdit::TrimStart { id: right, to: 600 })).unwrap();
        let before = serde_json::to_value(&editor.project).unwrap();
        for edit in [QuickEdit::TrimStart { id: right, to: 479 }, QuickEdit::TrimStart { id: right, to: 540 },
            QuickEdit::TrimEnd { id: left, to: 601 }, QuickEdit::TrimEnd { id: right, to: 1260 },
            QuickEdit::Speed { id: left, value: 0.5 }] {
            assert!(editor.execute(Edit::Sequence(edit)).is_err());
            assert_eq!(serde_json::to_value(&editor.project).unwrap(), before);
        }
        assert_linked(&editor.project);
    }
    #[test]
    fn unnamed_streams_are_distinct_and_video_only_has_no_fake_sound() {
        let mut source = asset();
        source.streams.retain(|s| s.kind == TrackKind::Video);
        let mut e = Editor::default();
        e.execute(Edit::Sequence(QuickEdit::Add(source))).unwrap();
        assert_eq!(e.project.tracks.len(), 1);
        let mut source = asset();
        for s in source
            .streams
            .iter_mut()
            .filter(|s| s.kind == TrackKind::Audio)
        {
            s.name = "Audio 1".into();
        }
        e.execute(Edit::Sequence(QuickEdit::Add(source))).unwrap();
        let names: Vec<_> = e
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["Recorded sound", "Recorded sound (2)", "Recorded sound (3)"]
        );
        assert_contiguous(&e.project);
    }
}
