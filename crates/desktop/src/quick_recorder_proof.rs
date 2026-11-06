//! Opt-in native verification. Captures a generated, owned window only; no desktop/audio.
use crate::{
    quick_recorder::QuickRecorder,
    quick_recording::{Event, Options, Worker},
};
use anyhow::{Context as _, Result, ensure};
use gpui::{prelude::*, *};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use rcr_recording_tools::source::CaptureTarget;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct Pattern(u32);
impl Render for Pattern {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(if self.0.is_multiple_of(2) {
                0x1255aa
            } else {
                0xaa3311
            }))
            .text_color(rgb(0xffffff))
            .p_5()
            .child(format!("RCR owned recorder proof {}", self.0))
    }
}
pub fn run(output: PathBuf, failed: Arc<AtomicBool>, cx: &mut App) -> Result<()> {
    let output = if output.is_absolute() {
        output
    } else {
        std::env::current_dir()?.join(output)
    };
    std::fs::create_dir_all(&output)?;
    let source: AnyWindowHandle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(60.), px(60.)),
                    size(px(480.), px(270.)),
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("RCR synthetic recorder verification".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Pattern(0)),
        )?
        .into();
    let native = source.update(cx, |_, window, _| -> Result<String> {
        let RawWindowHandle::Win32(native) = HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .as_raw()
        else {
            anyhow::bail!("Expected Win32 window");
        };
        Ok(native.hwnd.get().to_string())
    })??;
    let recorder = cx.new(QuickRecorder::new);
    let preview = recorder.clone();
    let controls: AnyWindowHandle = cx
        .open_window(
            WindowOptions {
                show: false,
                focus: false,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(600.), px(570.)),
                ))),
                ..Default::default()
            },
            |_, _| preview,
        )?
        .into();
    controls.update(cx, |_, window, _| window.resize(size(px(600.), px(570.))))?;
    cx.spawn(async move |cx| {
        let result: Result<()> = async {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let snapshot = controls.update(cx, |_, window, cx| {
                window.draw(cx).clear();
                window.request_render_snapshot(output.join("quick-recorder.png"))
            })??;
            snapshot.await??;
            // Exercise opening/reopening the actual production controller as
            // well as the isolated render. No desktop selection is invoked.
            cx.update(|cx| recorder.update(cx, |recorder, cx| recorder.show_controls(cx)))?;
            cx.update(|cx| recorder.update(cx, |recorder, cx| recorder.show_controls(cx)))?;
            let worker = Worker::start(Options { target: CaptureTarget::Window(native), cursor: false,
                system_audio: false, microphone: false, fps: 30, folder: output.clone() })?;
            let started = Instant::now();
            let mut ready_at = None;
            let mut paused = false;
            let mut resumed = false;
            let mut stopped = false;
            let mut saved = None;
            let mut frames = 0;
            loop {
                ensure!(started.elapsed() < Duration::from_secs(25), "Native recorder proof timed out");
                cx.background_executor().timer(Duration::from_millis(33)).await;
                source.update(cx, |view, window, cx| {
                    view.downcast::<Pattern>().unwrap().update(cx, |view, cx| { view.0 += 1; cx.notify(); });
                    window.draw(cx).clear(); window.present_for_diagnostics()
                })??;
                for event in worker.events.try_iter() {
                    match event {
                        Event::Started => ready_at = Some(Instant::now()),
                        Event::Progress(stats, warnings) => { frames = stats.encoded; ensure!(warnings.is_empty(), "Unexpected warnings: {warnings:?}"); }
                        Event::Failed(error) => anyhow::bail!("{error}"),
                        Event::Saved(path, warnings) => { ensure!(warnings.is_empty(), "Unexpected warnings: {warnings:?}"); saved = Some(path); }
                        _ => {}
                    }
                }
                if let Some(ready) = ready_at {
                    let elapsed = ready.elapsed();
                    if !paused && elapsed > Duration::from_millis(650) { worker.pause(true); paused = true; }
                    if paused && !resumed && elapsed > Duration::from_millis(1000) { worker.pause(false); resumed = true; }
                    if !stopped && elapsed > Duration::from_millis(2100) { worker.stop(); stopped = true; }
                }
                if saved.is_some() && worker.finished() { break; }
            }
            ensure!(frames >= 20 && paused && resumed && stopped, "Did not exercise recording/pause/resume/stop");
            let saved = saved.context("No saved clip")?;
            drop(worker);
            println!("Quick recorder proof: owned WGC window, {frames} frames, pause/resume/stop, H.264 MP4 -> {}", saved.display());
            println!("Quick recorder UI snapshot: {}", output.join("quick-recorder.png").display());
            Ok(())
        }.await;
        if let Err(error) = result { eprintln!("Quick recorder proof failed: {error:#}"); failed.store(true, Ordering::Relaxed); }
        println!("Quick recorder proof: worker released; requesting exit");
        let _ = cx.update(|cx| cx.quit());
        println!("Quick recorder proof: exit requested");
    }).detach();
    Ok(())
}
