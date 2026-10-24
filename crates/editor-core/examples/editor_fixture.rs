use anyhow::{Context, Result};
use rcr_editor_core::*;
use std::{path::PathBuf, sync::atomic::Ordering};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--monitor-only") {
        return monitor(Project::load(std::path::Path::new(
            args.get(2).context("project path required")?,
        ))?);
    }
    let input = PathBuf::from(args.get(1).context("input media required")?);
    let output = PathBuf::from(args.get(2).context("output .mp4 required")?);
    let mut editor = Editor::default();
    editor.execute(Edit::Import {
        asset: probe(&input)?,
        at: 0,
    })?;
    if args.iter().any(|a| a == "--suppress-mic") {
        let id = editor
            .project
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Audio && t.name.to_lowercase().contains("mic"))
            .context("fixture microphone track missing")?
            .id;
        editor.execute(Edit::SuppressNoise { id })?;
    }
    if args.iter().any(|a| a == "--fast") {
        editor.project.width = 960;
        editor.project.height = 540;
    }
    let video = editor
        .project
        .clips
        .iter()
        .find(|c| {
            editor
                .project
                .tracks
                .iter()
                .any(|t| t.id == c.track && t.kind == TrackKind::Video)
        })
        .unwrap()
        .id;
    editor.execute(Edit::TrimRight { id: video, to: 120 })?;
    editor.execute(Edit::Fade {
        id: video,
        fade_in: 12,
        fade_out: 12,
    })?;
    editor.execute(Edit::Transform {
        id: video,
        value: Transform {
            crop_bottom: 0.05,
            ..Default::default()
        },
    })?;
    let transform = editor
        .project
        .clips
        .iter()
        .find(|c| c.id == video)
        .unwrap()
        .transform;
    editor.execute(Edit::Keyframe {
        id: video,
        at: 90,
        value: Transform {
            x: 0.05,
            opacity: 0.8,
            ..transform
        },
    })?;
    editor.execute(Edit::Split { id: video, at: 60 })?;
    editor.execute(Edit::AddTitle {
        text: "RCR editor · separate audio".into(),
        at: 30,
        duration: 60,
    })?;
    let project = output.with_extension("rcr.json");
    editor.project.save(&project)?;
    let options = ExportOptions {
        output: output.clone(),
        start: if args.iter().any(|a| a == "--range-test") {
            30
        } else {
            0
        },
        end: if args.iter().any(|a| a == "--range-test") {
            Some(90)
        } else {
            None
        },
        audio: AudioExport::MixAndSeparate,
        bitrate: 12_000_000,
    };
    let progress = JobProgress::default();
    let path = export(&editor.project, &options, &progress)?;
    println!(
        "export={} duration_us={}",
        path.display(),
        progress.microseconds.load(Ordering::Relaxed)
    );
    let preview = PreviewJob::start(editor.project.clone(), 45, false)?;
    let frame = preview
        .frames
        .recv_timeout(std::time::Duration::from_secs(30))
        .context("preview frame timeout")?;
    anyhow::ensure!(
        frame.bgra.len() == frame.width as usize * frame.height as usize * 4,
        "bad preview size"
    );
    println!(
        "preview={}x{} frame={} bytes={}",
        frame.width,
        frame.height,
        frame.frame,
        frame.bgra.len()
    );
    preview.stop();
    let playback = PreviewJob::start(editor.project.clone(), 0, true)?;
    let first = playback
        .frames
        .recv_timeout(std::time::Duration::from_secs(20))?;
    let second = playback
        .frames
        .recv_timeout(std::time::Duration::from_secs(10))?;
    anyhow::ensure!(second.frame > first.frame, "playback did not advance");
    // Cancel with a full frame queue: stop must still reap the decoder.
    std::thread::sleep(std::time::Duration::from_millis(100));
    playback.stop();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !playback.done.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    anyhow::ensure!(
        playback.done.load(Ordering::Acquire),
        "preview cancellation stalled with a full queue"
    );
    println!("playback advanced; cancellation reaped decoder");
    if args.iter().any(|a| a == "--monitor-test") {
        monitor(editor.project)?;
    }
    Ok(())
}

fn monitor(project: Project) -> Result<()> {
    let audio = AudioPreviewJob::start(project, 0)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !audio.done.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let errors = audio.errors.try_iter().collect::<Vec<_>>();
    anyhow::ensure!(errors.is_empty(), "audio monitor errors: {errors:?}");
    anyhow::ensure!(
        audio.done.load(Ordering::Acquire),
        "audio monitor did not finish"
    );
    println!("audio monitor graph completed and reaped both subprocesses");
    Ok(())
}
