//! Verify that settled refinement bypasses the fast seek proxy at the same target.
use anyhow::{ensure, Context, Result};
use editor_core::*;
use std::{path::PathBuf, sync::atomic::Ordering, thread, time::{Duration, Instant}};

fn receive(session: &ScrubSession, serial: u64) -> Result<ScrubFrame> {
    let started = Instant::now();
    loop {
        if let Some(result) = session.frame_with_metadata() {
            let frame = result.map_err(anyhow::Error::msg)?;
            if frame.metadata.request_serial == serial {
                return Ok(frame);
            }
        }
        ensure!(started.elapsed() < Duration::from_secs(15), "Seek serial {serial} timed out");
        thread::sleep(Duration::from_millis(1));
    }
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let path = PathBuf::from(args.get(1).context("Recording required")?);
    let at = args.get(2).filter(|arg| !arg.starts_with("--"))
        .map(|arg| arg.parse::<u64>()).transpose()?.unwrap_or(1884);
    let mut editor = Editor::default();
    editor.execute(Edit::Sequence(QuickEdit::Add(probe(&path)?)))?;
    let effects = args.iter().any(|arg| arg == "--effects");
    if effects {
        let id = pieces(&editor.project)[0].id;
        editor.execute(Edit::Transform { id, value: Transform { opacity: 0.73, ..Default::default() } })?;
    }
    ensure!(at < editor.project.duration(), "Requested frame exceeds recording duration");
    let session = ScrubSession::new(editor.project.clone(), (1280, 720))?;
    if args.iter().any(|arg| arg == "--proxy") {
        let started = Instant::now();
        while !session.seek_proxy_ready() {
            ensure!(started.elapsed() < Duration::from_secs(15), "Proxy readiness timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }
    let started = Instant::now();
    let fast_serial = session.request_exact(at);
    let fast = receive(&session, fast_serial)?;
    ensure!(fast.frame.frame == at && fast.metadata.exact && !fast.metadata.original,
        "Fast result identity or source-quality metadata is wrong");
    println!("Fast: {}x{}, {:.2} ms, proxy_ready={}", fast.frame.width,
        fast.frame.height, started.elapsed().as_secs_f64() * 1000., session.seek_proxy_ready());

    let bounds = if effects { (1742, 981) } else { (2560, 1440) };
    let size = preview_size(&editor.project, bounds);
    let started = Instant::now();
    let original_serial = session.request_original(at, bounds);
    ensure!(original_serial > fast_serial, "Same-frame refinement reused the fast request serial");
    let original = receive(&session, original_serial)?;
    ensure!(original.metadata.original && original.metadata.exact && original.frame.frame == at,
        "Refinement returned proxy quality or the wrong target");
    ensure!((original.frame.width, original.frame.height) == size,
        "Refinement did not honor original preview geometry");
    println!("Original: {}x{}, {:.2} ms, native={}, hardware={}", original.frame.width,
        original.frame.height, started.elapsed().as_secs_f64() * 1000.,
        session.native_decoder(), session.hardware_decoder());

    let reference = PreviewJob::start_sized(editor.project.clone(), at, false, bounds)?;
    let expected = reference.frames.recv_timeout(Duration::from_secs(15));
    reference.stop();
    let stopped = Instant::now();
    while !reference.done.load(Ordering::Acquire) {
        ensure!(stopped.elapsed() < Duration::from_secs(15), "Reference shutdown timed out");
        thread::sleep(Duration::from_millis(1));
    }
    let expected = expected?;
    ensure!((expected.width, expected.height) == size && expected.bgra.len() == original.frame.bgra.len(),
        "Reference and refinement dimensions differ");
    let error = original.frame.bgra.iter().zip(&expected.bgra)
        .map(|(a, b)| f64::from(a.abs_diff(*b))).sum::<f64>() / expected.bgra.len() as f64;
    ensure!(error < 5., "Original-source mean BGRA error {error:.3} exceeds tolerance");
    println!("Same-target original-source mean BGRA error {error:.3}; serial {fast_serial} -> {original_serial}");
    session.shutdown();
    Ok(())
}
