//! Actual decoder correctness, latest-request and edited-timeline checks.
use anyhow::{ensure, Context, Result};
use editor_core::*;
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

fn receive(scrub: &ScrubSession, at: u64) -> Result<PreviewFrame> {
    let now = Instant::now();
    loop {
        if let Some(result) = scrub.frame() {
            let frame = result.map_err(anyhow::Error::msg)?;
            if frame.frame == at {
                return Ok(frame);
            }
        }
        ensure!(
            now.elapsed() < Duration::from_secs(15),
            "Seek timed out at {at}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
fn compare(scrub: &ScrubSession, project: &Project, at: u64) -> Result<()> {
    scrub.request(at);
    let actual = receive(scrub, at)?;
    let reference = PreviewJob::start_sized(project.clone(), at, false, (640, 360))?;
    let expected = reference.frames.recv_timeout(Duration::from_secs(15))?;
    reference.stop();
    while !reference.done.load(std::sync::atomic::Ordering::Acquire) {
        thread::sleep(Duration::from_millis(1));
    }
    ensure!(
        (actual.width, actual.height) == (expected.width, expected.height),
        "Seek dimensions differ"
    );
    let error = actual
        .bgra
        .iter()
        .zip(&expected.bgra)
        .map(|(a, b)| f64::from(a.abs_diff(*b)))
        .sum::<f64>()
        / actual.bgra.len() as f64;
    if error >= 5. {
        eprintln!(
            "Actual {:?}; reference {:?}; clips {:?}",
            &actual.bgra[..4],
            &expected.bgra[..4],
            pieces(project)
                .iter()
                .map(|c| (c.start, c.source_in_ns, c.speed))
                .collect::<Vec<_>>()
        );
    }
    ensure!(
        error < 5.,
        "Wrong seek frame/colors at {at}: mean byte error {error:.3}"
    );
    println!(
        "Frame {at}: mean BGRA error {error:.3}, native={}, NVDEC={}",
        scrub.native_decoder(),
        scrub.hardware_decoder()
    );
    Ok(())
}
fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).context("Recording required")?);
    let mut editor = Editor::default();
    editor.execute(Edit::Sequence(QuickEdit::Add(probe(&path)?)))?;
    let scrub = ScrubSession::new(editor.project.clone(), (640, 360))?;
    if std::env::args().any(|a| a == "--proxy") {
        let now = Instant::now();
        while !scrub.seek_proxy_ready() {
            ensure!(
                now.elapsed() < Duration::from_secs(95),
                "Proxy preparation failed"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    let end = editor.project.duration() - 1;
    for at in [0, end / 4, end * 3 / 4, end / 2] {
        compare(&scrub, &editor.project, at)?;
    }
    scrub.request(end);
    receive(&scrub, end)?;
    let now = Instant::now();
    let mut intermediate = 0;
    for i in 0..120 {
        scrub.request(end * i / 120);
        if let Some(result) = scrub.frame() {
            result.map_err(anyhow::Error::msg)?;
            intermediate += 1;
        }
        thread::sleep(Duration::from_millis(3));
    }
    scrub.request(end / 3);
    receive(&scrub, end / 3)?;
    ensure!(
        intermediate > 0,
        "Continuous scrubbing starved frame presentation"
    );
    println!("120 rapid requests coalesced; {intermediate} intermediate frames; final target reached in {:.1} ms",now.elapsed().as_secs_f64()*1000.);
    let id = pieces(&editor.project)[0].id;
    editor.execute(Edit::Sequence(QuickEdit::TrimStart {
        id,
        to: i64::from(editor.project.fps),
    }))?;
    scrub.replace_project(editor.project.clone(), (640, 360))?;
    // A start trim retains its leading gap; exercise both that gap and the
    // trimmed source boundary instead of assuming the clip moved to zero.
    compare(&scrub, &editor.project, 0)?;
    compare(&scrub, &editor.project, pieces(&editor.project)[0].start)?;
    editor.execute(Edit::Sequence(QuickEdit::Speed { id, value: 2. }))?;
    scrub.replace_project(editor.project.clone(), (640, 360))?;
    compare(&scrub, &editor.project, editor.project.duration() / 2)?;
    ensure!(editor.undo(), "Could not undo speed change");
    let cut = editor.project.duration() / 2;
    editor.execute(Edit::Sequence(QuickEdit::Split { id, at: cut }))?;
    let second = pieces(&editor.project)[1].id;
    editor.execute(Edit::Sequence(QuickEdit::Reorder {
        id: second,
        index: 0,
    }))?;
    scrub.replace_project(editor.project.clone(), (640, 360))?;
    compare(&scrub, &editor.project, 0)?;
    compare(&scrub, &editor.project, cut + 1)?;
    editor.execute(Edit::Transform {
        id: second,
        value: Transform {
            opacity: 0.5,
            ..Default::default()
        },
    })?;
    scrub.replace_project(editor.project.clone(), (640, 360))?;
    compare(&scrub, &editor.project, 0)?;
    let now = Instant::now();
    scrub.request(editor.project.duration() - 1);
    scrub.shutdown();
    println!(
        "Decoder cancelled and joined in {:.1} ms",
        now.elapsed().as_secs_f64() * 1000.
    );
    let preparing = ScrubSession::new(editor.project.clone(), (640, 360))?;
    thread::sleep(Duration::from_millis(100));
    let now = Instant::now();
    preparing.shutdown();
    ensure!(
        now.elapsed() < Duration::from_secs(3),
        "Proxy preparation did not cancel promptly"
    );
    println!(
        "Proxy preparation cancelled and reaped in {:.1} ms",
        now.elapsed().as_secs_f64() * 1000.
    );
    Ok(())
}
