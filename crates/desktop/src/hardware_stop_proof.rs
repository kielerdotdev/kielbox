//! Explicit actual desktop recording proof, using the production Studio Stop
//! command and poll/retirement path in an invisible GPUI window. No OS input,
//! microphone, global shortcuts, tray registration, uploads or existing settings.
use crate::{audio_source_picker::AudioChoice, preferences::Preferences, recordings, Studio};
use anyhow::{ensure, Result};
use gpui::{prelude::*, *};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

struct OwnedPattern(u64);
fn pin_source(window: &Window) -> Result<()> {
    let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .as_raw()
    else {
        anyhow::bail!("Expected Win32 source window")
    };
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};
    unsafe {
        SetWindowPos(
            HWND(handle.hwnd.get() as *mut std::ffi::c_void),
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        )?;
    }
    Ok(())
}
impl Render for OwnedPattern {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_3()
            .bg(rgb(if self.0.is_multiple_of(2) {
                0x1255aa
            } else {
                0xaa3311
            }))
            .text_color(rgb(0xffffff))
            .child(format!("RCR actual capture test — owned marker {}", self.0))
    }
}

pub(crate) fn run(output: PathBuf, failed: Arc<AtomicBool>, cx: &mut App) -> Result<()> {
    let seconds = std::env::var("RCR_HARDWARE_STOP_SECONDS")
        .unwrap_or_else(|_| "6".into())
        .parse::<f64>()?;
    ensure!(
        seconds.is_finite() && (1. ..=120.).contains(&seconds),
        "Hardware proof duration must be 1..120 seconds"
    );
    let output = if output.is_absolute() {
        output
    } else {
        std::env::current_dir()?.join(output)
    };
    ensure!(
        !output.join("desktop-report.json").exists(),
        "Desktop proof report already exists"
    );
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    // This dedicated process owns the entire working directory. Studio cannot
    // read or overwrite the user's current settings, clips or project history.
    std::env::set_current_dir(&output)?;
    crate::theme::init(cx);
    crate::text_input::init(cx);
    crate::editor::init(cx);
    let source: AnyWindowHandle = cx
        .open_window(
            WindowOptions {
                show: true,
                focus: false,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(24.), px(24.)),
                    size(px(320.), px(100.)),
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("RCR owned recording test".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|_| OwnedPattern(0)),
        )?
        .into();
    source.update(cx, |_, window, _| pin_source(window))??;
    let folder = output.join("recordings");
    let studio = cx.new(|cx| {
        let mut studio = Studio::new(None, false, true, cx);
        studio.preferences = Preferences {
            folder: folder.clone(),
            ..Default::default()
        };
        studio.system_audio = false;
        studio.microphone = false;
        studio.suppress_microphone = false;
        studio.audio_choice = AudioChoice::Desktop;
        studio.record_to_disk = true;
        recordings::set_root(folder.clone());
        studio
    });
    let view = studio.clone();
    let host: AnyWindowHandle = cx
        .open_window(
            WindowOptions {
                show: false,
                focus: false,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1440.), px(900.)),
                ))),
                ..Default::default()
            },
            |_, _| view,
        )?
        .into();
    host.update(cx, |_, window, _| window.resize(size(px(1440.), px(900.))))?;
    cx.spawn(async move |cx| {
        let mut rows = Vec::<serde_json::Value>::new();
        let result: Result<()> = async {
            let _display = DisplayWake::new()?;
            cx.background_executor().timer(Duration::from_millis(500)).await;
            for audio in [false, true] {
                host.update(cx, |root, _, cx| {
                    let studio = root.downcast::<Studio>().unwrap();
                    studio.update(cx, |studio, cx| {
                        ensure!(!studio.active && studio.worker.is_none(), "Previous hardware worker has not retired");
                        studio.system_audio = audio;
                        studio.microphone = false;
                        studio.suppress_microphone = false;
                        studio.preferences.system_audio = audio;
                        studio.preferences.microphone = false;
                        studio.preferences.clean_microphone = false;
                        studio.toggle_record(cx);
                        ensure!(studio.active, "Desktop recording did not start: {:?}", studio.error);
                        Ok(())
                    })
                })??;
                let epoch = Instant::now();
                let mut capture_epoch = None;
                let mut stop_requested = None;
                let mut command_ms = None;
                let mut finishing_painted_ms = None;
                let mut idle_painted_ms = None;
                let mut file_observed_ms = None;
                let mut peak_preview_draws = 0_u64;
                let mut marker = 0_u64;
                let mut marker_at = Instant::now() - Duration::from_secs(1);
                loop {
                    ensure!(epoch.elapsed() < Duration::from_secs_f64(seconds + 60.), "Desktop Stop proof exceeded capture+60s budget");
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                    if marker_at.elapsed() >= Duration::from_millis(100) {
                        marker += 1;
                        source.update(cx, |root, window, cx| -> Result<()> {
                            pin_source(window)?;
                            root.downcast::<OwnedPattern>().unwrap().update(cx, |pattern, cx| { pattern.0 = marker; cx.notify(); });
                            window.refresh(); window.draw(cx).clear(); window.present_for_diagnostics()
                        })??;
                        marker_at = Instant::now();
                    }
                    let (active, worker, stats, error, preview_error, output, preview_draws, library_pending) = host.update(cx, |root, window, cx| -> Result<_> {
                        let studio = root.downcast::<Studio>().unwrap();
                        let snapshot = studio.read(cx);
                        let mut result = (snapshot.active, snapshot.worker.is_some(), snapshot.stats.clone(), snapshot.error.clone(),
                            snapshot.preview_error.clone().or_else(|| snapshot.surface.as_ref().and_then(|surface| surface.error())), PathBuf::from(&snapshot.stats.output),
                            snapshot.surface.as_ref().map_or(0, |surface| surface.draw_count()), snapshot.library_scan.is_pending());
                        window.refresh();
                        window.draw(cx).clear();
                        window.present_for_diagnostics()?;
                        result.6 = studio.read(cx).surface.as_ref().map_or(0, |surface| surface.draw_count());
                        Ok(result)
                    })??;
                    peak_preview_draws = peak_preview_draws.max(preview_draws);
                    ensure!(error.is_none(), "Desktop capture failed: {error:?}");
                    if capture_epoch.is_none() && stats.encoded > 0 {
                        capture_epoch = Some(Instant::now() - Duration::from_secs_f64(stats.elapsed_seconds));
                    }
                    if stop_requested.is_none() && capture_epoch.is_some_and(|started| started.elapsed().as_secs_f64() >= seconds) {
                        let requested = Instant::now();
                        let (dispatch, finishing) = host.update(cx, |root, window, cx| -> Result<_> {
                            let studio = root.downcast::<Studio>().unwrap();
                            let dispatched = Instant::now();
                            studio.update(cx, |studio, cx| studio.toggle_record(cx));
                            let command_ms = dispatched.elapsed().as_secs_f64() * 1000.;
                            let finishing = studio.read(cx).active && studio.read(cx).stop.load(Ordering::Acquire);
                            window.refresh(); window.draw(cx).clear();
                            window.present_for_diagnostics()?;
                            Ok((command_ms, finishing))
                        })??;
                        ensure!(finishing, "Production Stop command did not enter finishing state");
                        command_ms = Some(dispatch);
                        finishing_painted_ms = Some(requested.elapsed().as_secs_f64() * 1000.);
                        stop_requested = Some(requested);
                    }
                    if let Some(stopped) = stop_requested {
                        if output.is_file() && file_observed_ms.is_none() {
                            file_observed_ms = Some(stopped.elapsed().as_secs_f64() * 1000.);
                        }
                        if !active && !worker {
                            if idle_painted_ms.is_none() { idle_painted_ms = Some(stopped.elapsed().as_secs_f64() * 1000.); }
                            ensure!(output.is_file(), "Desktop reports idle before recording publication");
                            if library_pending { continue; }
                            let visible_in_library = host.update(cx, |root, _, cx| {
                                let studio = root.downcast::<Studio>().unwrap();
                                studio.read(cx).files.contains(&output)
                            })?;
                            ensure!(visible_in_library, "Saved recording is absent from refreshed Library");
                            let viewport = host.update(cx, |_, window, _| window.viewport_size())?;
                            rows.push(serde_json::json!({
                                "desktop_loopback": audio, "microphone": false, "seconds_requested": seconds,
                                "stop_command_dispatch_ms": command_ms, "stop_to_finishing_painted_ms": finishing_painted_ms,
                                "stop_to_ui_idle_painted_ms": idle_painted_ms, "stop_to_file_observed_ms": file_observed_ms,
                                "stop_to_library_ready_ms": stopped.elapsed().as_secs_f64() * 1000.,
                                "production_worker_retired": true, "saved_in_library": visible_in_library,
                                "fixture_ticks": marker, "active_source_updates_verified": stats.captured > 10,
                                "path": output, "preview_error": preview_error, "preview_draws": peak_preview_draws,
                                "viewport": [f32::from(viewport.width), f32::from(viewport.height)],
                                "pipeline": stats,
                            }));
                            ensure!(stats.captured > 10, "Owned topmost fixture did not produce >10 observed desktop updates");
                            ensure!(preview_error.is_none() && peak_preview_draws > 0,
                                "Desktop GPU preview was not drawn: count={peak_preview_draws}, error={preview_error:?}, viewport={viewport:?}");
                            break;
                        }
                    }
                }
            }
            Ok(())
        }.await;
        let report = serde_json::json!({
            "scope": "Actual Desktop Studio production recording/Stop command and 33ms poll/worker retirement; hidden GPUI draws, no synthesized OS click",
            "display_keep_awake": true,
            "owned_animated_fixture": true,
            "success": result.is_ok(), "error": result.as_ref().err().map(|error| format!("{error:#}")),
            "rows": rows,
        });
        let saved = std::fs::write(output.join("desktop-report.json"), serde_json::to_vec_pretty(&report).unwrap());
        if let Err(error) = &result { eprintln!("Desktop hardware Stop proof: {error:#}"); }
        if let Err(error) = &saved { eprintln!("Could not save desktop hardware report: {error}"); }
        if result.is_err() || saved.is_err() { failed.store(true, Ordering::Release); }
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        let _ = cx.update(|cx| cx.quit());
    }).detach();
    Ok(())
}

struct DisplayWake {
    owner: std::thread::ThreadId,
    _local: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl DisplayWake {
    fn new() -> Result<Self> {
        use windows::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };
        let previous = unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED)
        };
        ensure!(
            previous.0 != 0,
            "Could not keep display/system awake during hardware proof"
        );
        Ok(Self {
            owner: std::thread::current().id(),
            _local: std::marker::PhantomData,
        })
    }
}
impl Drop for DisplayWake {
    fn drop(&mut self) {
        debug_assert_eq!(self.owner, std::thread::current().id());
        use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}
