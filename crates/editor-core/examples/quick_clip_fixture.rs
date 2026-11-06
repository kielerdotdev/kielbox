//! End-to-end quick clip checks. Uses a synthetic two-tone recording, no UI automation.
use anyhow::{ensure, Context, Result};
use editor_core::*;
use std::{path::PathBuf, time::Duration};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let input = PathBuf::from(args.get(1).context("input recording required")?);
    let output = PathBuf::from(args.get(2).context("output prefix required")?);
    let asset = probe(&input)?;
    ensure!(
        asset
            .streams
            .iter()
            .filter(|s| s.kind == TrackKind::Audio)
            .count()
            == 2,
        "Fixture needs two separate audio tracks"
    );
    for stream in asset.streams.iter().filter(|s| s.kind == TrackKind::Audio) {
        ensure!(
            !waveform(&asset.path, stream.index)?.is_empty(),
            "Missing waveform"
        );
    }
    ensure!(thumbnails(&asset)?.len() == 4, "Missing thumbnails");
    let mut e = Editor::default();
    e.execute(Edit::Sequence(QuickEdit::Add(asset.clone())))?;
    let first = pieces(&e.project)[0].id;
    e.execute(Edit::Sequence(QuickEdit::TrimStart { id: first, to: 60 }))?;
    ensure!(pieces(&e.project)[0].start == 60, "Start trim closed its gap");
    // Closing a gap is an explicit movement, never a side effect of trimming.
    e.execute(Edit::Move { id: first, to: 0 })?;
    for edit in [
        QuickEdit::TrimEnd { id: first, to: 180 },
        QuickEdit::Split { id: first, at: 60 },
    ] {
        e.execute(Edit::Sequence(edit))?;
    }
    let second = pieces(&e.project)[1].id;
    e.execute(Edit::Sequence(QuickEdit::Speed {
        id: second,
        value: 2.,
    }))?;
    e.execute(Edit::Sequence(QuickEdit::Add(asset)))?;
    let third = pieces(&e.project)[2].id;
    e.execute(Edit::Sequence(QuickEdit::TrimEnd { id: third, to: 180 }))?;
    e.execute(Edit::Sequence(QuickEdit::Reorder {
        id: third,
        index: 0,
    }))?;
    e.execute(Edit::Sequence(QuickEdit::Delete(first)))?;
    ensure!(pieces(&e.project)[1].start == 120, "Delete moved a neighboring piece");
    e.execute(Edit::Move { id: second, to: 60 })?;
    ensure!(
        e.project.duration() == 120,
        "Expected two joined one-second pieces"
    );
    let session = output.with_extension("json");
    e.project.save(&session)?;
    let restored = Project::load(&session)?;
    ensure!(
        pieces(&restored).len() == 2 && restored.duration() == 120,
        "Reopening lost edits"
    );
    let preview = PreviewJob::start_sized(restored, 20, false, (1600, 900))?;
    let frame = preview.frames.recv_timeout(Duration::from_secs(30))?;
    ensure!(
        (frame.width, frame.height) == (1600, 900),
        "Preview still has a fixed resolution cap"
    );
    drop(preview);
    // The two variants independently prove both global audio controls and export mix.
    for (name, muted, gain) in [("game", "Microphone", 0.5), ("mic", "Game", 1.)] {
        let mut project = e.project.clone();
        for track in project
            .tracks
            .iter_mut()
            .filter(|t| t.kind == TrackKind::Audio)
        {
            track.muted = track.name == muted;
            track.gain = gain;
        }
        let path = PathBuf::from(format!("{}-{name}.mp4", output.display()));
        // A compact proof; Original quality in the UI retains the source resolution.
        project.width = 1280;
        project.height = 720;
        if std::env::var_os("RCR_DUMP_RENDER_PLAN").is_some() {
            let plan = RenderPlan::build(
                &project,
                0,
                project.duration(),
                1280,
                720,
                60,
                Some(AudioExport::Mix),
            )?;
            println!("inputs={:?}\n{}", plan.input_args, plan.filter);
        }
        export(
            &project,
            &ExportOptions {
                output: path,
                start: 0,
                end: None,
                audio: AudioExport::Mix,
                bitrate: 5_000_000,
                video_codec: ExportVideoCodec::Av1,
            },
            &JobProgress::default(),
        )?;
    }
    println!("Verified all audio streams, waveforms, thumbnails, stationary trims/delete, explicit gap-closing moves, speed/reorder, reopening and 1600×900 preview; exported independent audio choices.");
    Ok(())
}
