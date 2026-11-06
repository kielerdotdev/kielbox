//! Short transport checks. Default is a silent device simulator, no audio output.
use anyhow::{ensure, Context, Result};
use editor_core::*;
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let path = PathBuf::from(args.get(1).context("recording required")?);
    let mut editor = Editor::default();
    editor.execute(Edit::Sequence(QuickEdit::Add(probe(&path)?)))?;
    if args.iter().any(|a| a == "--random") {
        let scrub = ScrubSession::new(editor.project.clone(), (1280, 720))?;
        let now = Instant::now();
        while !scrub.seek_proxy_ready() {
            ensure!(
                now.elapsed() < Duration::from_secs(95),
                "Seek proxy not ready"
            );
            thread::sleep(Duration::from_millis(20));
        }
        println!("Proxy prepared in {:.2}s", now.elapsed().as_secs_f64());
        let mut samples = Vec::new();
        // Coprime permutation: distant, previously unvisited points throughout
        // the recording, then a broad forward/backward timeline drag.
        let duration = editor.project.duration().saturating_sub(1);
        let targets = (0..61)
            .map(|i| duration * ((i * 23) % 61) / 61)
            .chain((0..120).map(|i| duration * i / 120))
            .chain((0..120).rev().map(|i| duration * i / 120));
        for at in targets {
            let now = Instant::now();
            scrub.request(at);
            loop {
                if let Some(result) = scrub.frame() {
                    if result.map_err(anyhow::Error::msg)?.frame == at {
                        break;
                    }
                }
                ensure!(
                    now.elapsed() < Duration::from_secs(3),
                    "Random seek timed out"
                );
                thread::sleep(Duration::from_millis(1));
            }
            samples.push(now.elapsed().as_secs_f64() * 1000.);
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "{}",
            serde_json::json!({"random_and_sweep_samples":samples.len(), "median_ms":samples[samples.len()/2], "p95_ms":samples[samples.len()*95/100], "max_ms":samples.last()})
        );
        ensure!(
            samples[samples.len() * 95 / 100] < 40.,
            "Random seeks remain stalled"
        );
        scrub.shutdown();
        return Ok(());
    }
    if args.iter().any(|a| a == "--cuts") {
        for at in (15..=105).step_by(15) {
            let id = pieces(&editor.project)
                .iter()
                .find(|c| c.start < at && c.end() > at)
                .context("Cut lies outside recording")?
                .id;
            editor.execute(Edit::Sequence(QuickEdit::Split { id, at }))?;
        }
    }
    if args.iter().any(|a| a == "--seek") {
        let scrub = ScrubSession::new(editor.project.clone(), (1280, 720))?;
        for fraction in [0., 0.25, 0.75, 0.5] {
            let at = (editor.project.duration().saturating_sub(1) as f64 * fraction) as u64;
            let now = Instant::now();
            let session = PlaybackSession::new(
                editor.project.clone(),
                at,
                (1280, 720),
                PlaybackOutput::Silent,
            )?;
            loop {
                check(&session)?;
                if let Some(frame) = session.frame() {
                    ensure!(frame.frame == at, "Wrong seek frame");
                    break;
                }
                ensure!(now.elapsed() < Duration::from_secs(20), "Seek timed out");
                thread::sleep(Duration::from_millis(1));
            }
            println!(
                "full transport seek {at}: {:.1} ms, {} processes",
                now.elapsed().as_secs_f64() * 1000.,
                session.decoder_processes()
            );
            session.shutdown();
            for label in ["scrub", "cached"] {
                scrub.request(at);
                let now = Instant::now();
                loop {
                    if let Some(result) = scrub.frame() {
                        let frame = result.map_err(anyhow::Error::msg)?;
                        if frame.frame == at {
                            break;
                        }
                    }
                    ensure!(now.elapsed() < Duration::from_secs(20), "Scrub timed out");
                    thread::sleep(Duration::from_millis(1));
                }
                println!(
                    "{label} seek {at}: {:.1} ms, native={}, hardware={}",
                    now.elapsed().as_secs_f64() * 1000.,
                    scrub.native_decoder(),
                    scrub.hardware_decoder()
                );
            }
        }
        scrub.shutdown();
        return Ok(());
    }
    if args.iter().any(|a| a == "--drag") {
        let scrub = ScrubSession::new(editor.project.clone(), (1280, 720))?;
        let middle = (editor.project.duration() / 2).max(30);
        let wait = |at| -> Result<()> {
            let now = Instant::now();
            loop {
                if let Some(result) = scrub.frame() {
                    if result.map_err(anyhow::Error::msg)?.frame == at {
                        return Ok(());
                    }
                }
                ensure!(
                    now.elapsed() < Duration::from_secs(15),
                    "Drag seek timed out"
                );
                thread::sleep(Duration::from_millis(1));
            }
        };
        scrub.request(middle);
        wait(middle)?;
        thread::sleep(Duration::from_millis(400));
        let mut samples = Vec::new();
        for offset in (0..24).rev().chain(0..45).chain((0..24).rev()) {
            let at = (middle - 24 + offset).min(editor.project.duration() - 1);
            let now = Instant::now();
            scrub.request(at);
            wait(at)?;
            samples.push(now.elapsed().as_secs_f64() * 1000.);
            if let Some(rest) = Duration::from_millis(16).checked_sub(now.elapsed()) {
                thread::sleep(rest);
            }
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "{}",
            serde_json::json!({"drag_frames":samples.len(),"median_ms":samples[samples.len()/2],"p95_ms":samples[samples.len()*95/100],"max_ms":samples.last(),"neighbor_surface_hits":scrub.surface_cache_hits(),"surface_cache_bytes":scrub.surface_cache_bytes(),"NVDEC":scrub.hardware_decoder()})
        );
        ensure!(
            scrub.surface_cache_bytes() <= 192 * 1024 * 1024,
            "Surface cache exceeds budget"
        );
        ensure!(
            samples[samples.len() * 95 / 100] < 40.,
            "Continuous dragging is still stalled"
        );
        scrub.shutdown();
        return Ok(());
    }
    if args.iter().any(|a| a == "--legacy") {
        let mut startup = Vec::new();
        for _ in 0..3 {
            let now = Instant::now();
            let job = PreviewJob::start_sized(editor.project.clone(), 60, true, (1280, 720))?;
            job.frames.recv_timeout(Duration::from_secs(20))?;
            startup.push(now.elapsed().as_secs_f64() * 1000.);
            let now = Instant::now();
            for _ in 0..30 {
                job.frames.recv_timeout(Duration::from_secs(10))?;
            }
            println!(
                "legacy cadence {:.1} FPS",
                30. / now.elapsed().as_secs_f64()
            );
            job.stop();
            while !job.done.load(std::sync::atomic::Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
        }
        println!("legacy start/restart ms: {startup:?}");
        return Ok(());
    }
    let native = args.iter().any(|a| a == "--native-muted");
    if native {
        for track in editor
            .project
            .tracks
            .iter_mut()
            .filter(|t| t.kind == TrackKind::Audio)
        {
            track.muted = true;
        }
    }
    let mode = if native {
        PlaybackOutput::Device
    } else {
        PlaybackOutput::Silent
    };
    if args.iter().any(|arg| arg == "--churn") {
        let now = Instant::now();
        for at in [0, 15, 30, 45] {
            let session = PlaybackSession::new(
                editor.project.clone(),
                at,
                (640, 360),
                PlaybackOutput::Silent,
            )?;
            session.play();
            thread::sleep(Duration::from_millis(5));
            session.shutdown();
        }
        println!(
            "Cancelled and reaped four in-flight seeks in {:.1} ms",
            now.elapsed().as_secs_f64() * 1000.
        );
        return Ok(());
    }
    let start = Instant::now();
    let bounds = if args.iter().any(|a| a == "--full-size") {
        (editor.project.width, editor.project.height)
    } else {
        (1280, 720)
    };
    let session = PlaybackSession::new(editor.project.clone(), 0, bounds, mode)?;
    while !session.ready() && start.elapsed() < Duration::from_secs(20) {
        check(&session)?;
        let _ = session.frame();
        thread::sleep(Duration::from_millis(2));
    }
    ensure!(session.ready(), "Player did not warm up");
    let warm_ms = start.elapsed().as_secs_f64() * 1000.;
    let mut intervals = Vec::new();
    let mut resumes = Vec::new();
    let mut presented = 0;
    for _ in 0..3 {
        session.play();
        let begin = Instant::now();
        let mut last = None;
        while begin.elapsed() < Duration::from_millis(550) {
            check(&session)?;
            if session.frame().is_some() {
                let now = Instant::now();
                if let Some(last) = last {
                    intervals.push(now.duration_since(last).as_secs_f64() * 1000.);
                }
                last = Some(now);
                presented += 1;
            }
            thread::sleep(Duration::from_millis(1));
        }
        session.pause();
        thread::sleep(Duration::from_millis(35));
        let paused = session.position();
        thread::sleep(Duration::from_millis(180));
        ensure!(
            session.position() == paused,
            "Audio/video clock advanced while paused"
        );
        let paused_processes = session.decoder_processes();
        session.set_audio(&editor.project);
        ensure!(
            session.decoder_processes() == paused_processes,
            "Pause or audio controls restarted decoders"
        );
        let now = Instant::now();
        session.play();
        while !session.frame().is_some_and(|frame| frame.frame > paused)
            && now.elapsed() < Duration::from_secs(1)
        {
            check(&session)?;
            thread::sleep(Duration::from_millis(1));
        }
        let latency = now.elapsed().as_secs_f64() * 1000.;
        resumes.push(latency);
        ensure!(latency < 200., "Warm resume still stalled: {latency:.1} ms");
    }
    session.pause();
    check(&session)?;
    intervals.sort_by(f64::total_cmp);
    let p95 = intervals[intervals.len() * 95 / 100];
    let max = intervals.last().copied().unwrap_or_default();
    ensure!(
        presented >= 85,
        "Playback failed to sustain near 60 FPS: {presented} frames in 1.65 s"
    );
    println!(
        "{}",
        serde_json::json!({"native_muted":native,"cold_ready_ms":warm_ms,"warm_resume_ms":resumes,"presented_frames_in_1_65s":presented,"interval_p95_ms":p95,"interval_max_ms":max,"decoder_processes":session.decoder_processes(),"decoded_frames":session.decoded_frames(),"dropped_frames":session.dropped_frames()})
    );
    let stop = Instant::now();
    session.shutdown();
    println!(
        "all workers and child processes reaped in {:.1} ms",
        stop.elapsed().as_secs_f64() * 1000.
    );
    Ok(())
}
fn check(session: &PlaybackSession) -> Result<()> {
    let errors: Vec<_> = session.errors.try_iter().collect();
    ensure!(errors.is_empty(), "Playback errors: {errors:?}");
    Ok(())
}
