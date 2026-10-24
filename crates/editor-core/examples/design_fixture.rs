//! Verify the design's library thumbnail and portrait export without UI automation.
use anyhow::{ensure, Context, Result};
use rcr_editor_core::*;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let source = PathBuf::from(args.get(1).context("Input recording required")?);
    let output = PathBuf::from(args.get(2).context("Output MP4 required")?);
    let asset = probe(&source)?;
    let thumb = thumbnail(&asset, 0.2, (320, 180))?;
    ensure!(
        thumb.bgra.len() == 320 * 180 * 4,
        "Library thumbnail is incomplete"
    );
    let mut editor = Editor::default();
    editor.execute(Edit::Sequence(QuickEdit::Add(asset)))?;
    let first = pieces(&editor.project)[0].id;
    editor.execute(Edit::Sequence(QuickEdit::TrimEnd { id: first, to: 60 }))?;
    editor.execute(Edit::Reframe {
        vertical: true,
        center: 0.75,
    })?;
    let before = serde_json::to_value(&editor.project)?;
    let session = output.with_extension("json");
    editor.project.save(&session)?;
    let restored = Project::load(&session)?;
    ensure!(
        restored.reframe == editor.project.reframe,
        "Reframe was lost on reopen"
    );
    let mut framed = restored.framed_export();
    framed.width = 720;
    framed.height = 1280;
    export(
        &framed,
        &ExportOptions {
            output: output.clone(),
            start: 0,
            end: None,
            audio: AudioExport::Mix,
            bitrate: 5_000_000,
        },
        &JobProgress::default(),
    )?;
    ensure!(
        serde_json::to_value(&editor.project)? == before,
        "Export modified the original project"
    );
    let exported = probe(&output)?;
    ensure!(
        (exported.width, exported.height) == (720, 1280),
        "Portrait dimensions were not exported"
    );
    ensure!(
        exported.duration_ns.abs_diff(1_000_000_000) < 100_000_000,
        "Unexpected export duration"
    );
    ensure!(
        exported.streams.iter().any(|s| s.kind == TrackKind::Audio),
        "Export lost the audio mix"
    );
    println!("Verified 320×180 BGRA thumbnail, saved framing, 720×1280 AV1 export with audio, and unchanged original edits.");
    Ok(())
}
