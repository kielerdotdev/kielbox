//! Explicit native visual QA. Captures only this GPUI window's render target;
//! never reads the desktop or synthesizes OS input.
use crate::Studio;
use gpui::{AnyWindowHandle, App, Pixels, Size, WindowHandle};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn audio_error_fixture_active(screen: &str) -> Option<bool> {
    match screen {
        "capture-audio-error" => Some(true),
        "capture-audio-error-stopped" => Some(false),
        _ => None,
    }
}

#[derive(Clone, Copy, Default)]
pub struct FixtureOptions {
    pub watch_share_stage: Option<u8>,
    pub installation_preview: bool,
}

pub fn capture(
    window: WindowHandle<Studio>,
    screen: String,
    output: PathBuf,
    failed: Arc<AtomicBool>,
    size: Size<Pixels>,
    options: FixtureOptions,
    cx: &mut App,
) {
    let window: AnyWindowHandle = window.into();
    let _ = window.update(cx, |_, window, _| window.resize(size));
    cx.spawn(async move |cx| {
        let result: anyhow::Result<()> = async {
            let output = if output.is_absolute() {
                output
            } else {
                std::env::current_dir()?.join(output)
            };
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let started = Instant::now();
            let mut selection_seeded = false;
            let mut share_seeded = false;
            let mut setup_seeded = false;
            let mut fixture_error = None;
            println!(
                "Thumbnail cache: {}",
                crate::library::thumbnail_cache_directory().display()
            );
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let ready = window.update(cx, |root, window, cx| {
                    let studio = root.downcast::<Studio>().expect("Studio snapshot root");
                    if options.installation_preview && !setup_seeded {
                        studio.update(cx, |studio, cx| {
                            use crate::setup_diagnostics::{Check, Report, Status};
                            let checks = [
                                (Status::Ready, "Media tools are available", None),
                                (Status::NotVerified, "Actual recording hardware was not activated by this check", None),
                                (Status::Warning, "Development build depends on its source checkout", Some("Keep the checkout or use a release package.")),
                            ].into_iter().enumerate().map(|(index, (status, summary, action))| Check {
                                id: format!("fixture-{index}"), status, blocking: false,
                                summary: summary.into(), action: action.map(str::to_owned),
                                path: None, detail: None, elapsed_ms: 0,
                            }).collect();
                            studio.setup_report = Some(Report { schema_version: 1, app_version: "fixture".into(),
                                build_profile: "fixture".into(), platform: "Windows".into(), executable: None,
                                working_directory: "fixture-only".into(), checks, elapsed_ms: 0 });
                            cx.notify();
                        });
                        setup_seeded = true;
                    }
                    if ((screen.starts_with("library") && !screen.starts_with("library-trash"))
                        || screen.starts_with("capture"))
                        && studio.read(cx).library_scan.is_pending()
                    {
                        return false;
                    }
                    if screen == "library-selected" && !selection_seeded {
                        studio.update(cx, |this, cx| {
                            this.library
                                .selected
                                .extend(this.files.iter().take(3).cloned());
                            cx.notify();
                        });
                        selection_seeded = true;
                    }
                    if matches!(screen.as_str(), "library-layout-saving" | "library-layout-failed") && !selection_seeded {
                        studio.update(cx, |this, cx| {
                            // Presentation-only state: no layout submission or
                            // preference/file-service mutation belongs in this fixture.
                            this.layout_save_revision = 0;
                            this.layout_save_pending = screen == "library-layout-saving";
                            this.layout_save_error = (screen == "library-layout-failed").then(||
                                "Fixture: the settings write failed. Select List or Grid again to retry.".into());
                            cx.notify();
                        });
                        selection_seeded = true;
                    }
                    if screen == "capture-saved" && !selection_seeded {
                        studio.update(cx, |this, cx| {
                            this.notice = Some("Replay saved to your Library.".into());
                            this.saved_replay = this
                                .files
                                .iter()
                                .find(|path| !crate::recordings::is_image(path))
                                .cloned();
                            cx.notify();
                        });
                        selection_seeded = true;
                    }
                    if let Some(active) = audio_error_fixture_active(&screen)
                        && !selection_seeded {
                        studio.update(cx, |this, cx| {
                            // Explicit presentation fixture: no command, capture,
                            // audio-device activation, or preference writer runs.
                            this.active = active;
                            this.stop.store(false, Ordering::Relaxed);
                            this.record_to_disk = false;
                            this.system_audio = true;
                            this.microphone = true;
                            this.suppress_microphone = false;
                            this.session_profile = (1440, 60, 120);
                            this.stats.encoded = 720;
                            this.stats.replay_seconds = 12.;
                            this.stats.audio_errors = vec![
                                "Diagnostic fixture: microphone endpoint disconnected (0x88890004).".into(),
                                "Diagnostic fixture: desktop audio endpoint became unavailable.".into(),
                            ];
                            this.error = None;
                            this.notice = None;
                            cx.notify();
                        });
                        selection_seeded = true;
                    }
                    if screen.starts_with("tools") {
                        let toolkit = studio.read(cx).toolkit.clone();
                        if !toolkit.update(cx, |toolkit, cx| toolkit.snapshot_surface(&screen, cx))
                        {
                            return false;
                        }
                    }
                    // Bind any queued CPU frame before testing snapshot readiness.
                    // Hidden windows only advance this path when explicitly drawn.
                    window.draw(cx).clear();
                    if screen == "editor-clip-show" {
                        let editor = studio.read(cx).editor.clone();
                        if !editor.update(cx, |editor, cx| editor.seed_clip_show_snapshot(cx)) { return false; }
                        if !share_seeded && let Some(stage) = options.watch_share_stage {
                            if let Err(error) = editor.update(cx, |editor, cx| editor.seed_watch_share_status(stage, cx)) {
                                fixture_error = Some(error);
                                return true;
                            }
                            share_seeded = true;
                        }
                    }
                    window.draw(cx).clear();
                    if options.installation_preview {
                        match studio.update(cx, |studio, cx| studio.snapshot_installation_visible(cx)) {
                            Ok(true) => {},
                            Ok(false) => return false,
                            Err(error) => { fixture_error = Some(error); return true; }
                        }
                    }
                    if screen == "editor" {
                        studio.read(cx).editor.read(cx).smoke_ready()
                            && started.elapsed() > Duration::from_secs(5)
                    } else if screen.starts_with("library") && !screen.starts_with("library-trash")
                    {
                        let studio = studio.read(cx);
                        started.elapsed() > Duration::from_secs(3)
                            && (studio.library.snapshot_previews_ready(&studio.files)
                                || started.elapsed() > Duration::from_secs(12))
                    } else {
                        started.elapsed() > Duration::from_secs(3)
                    }
                })?;
                if let Some(error) = fixture_error.take() { return Err(error); }
                if ready {
                    break;
                }
                anyhow::ensure!(
                    started.elapsed() < Duration::from_secs(25),
                    "UI fixture did not become ready: {}", window.update(cx, |root, _, cx| {
                        root.downcast::<Studio>().expect("Snapshot root").read(cx).editor.read(cx).snapshot_status()
                    })?
                );
            }
            let completion = window.update(cx, |root, window, cx| {
                anyhow::ensure!(
                    window.viewport_size().width >= gpui::px(800.),
                    "Native snapshot viewport was not sized"
                );
                if screen == "settings-shortcuts" {
                    window.dispatch_event(
                        gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                            position: gpui::point(size.width / 2., size.height / 2.),
                            delta: gpui::ScrollDelta::Lines(gpui::point(0., -100.)),
                            ..Default::default()
                        }),
                        cx,
                    );
                }
                if audio_error_fixture_active(&screen).is_some() {
                    // Scroll only this window's capture-options element into
                    // its source-error state; no OS input is synthesized.
                    window.dispatch_event(gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                        position: gpui::point(size.width - gpui::px(150.), size.height - gpui::px(140.)),
                        delta: gpui::ScrollDelta::Lines(gpui::point(0., -100.)),
                        ..Default::default()
                    }), cx);
                }
                window.draw(cx).clear();
                if screen == "editor-clip-show" {
                    let studio = root.clone().downcast::<Studio>().expect("Clip show snapshot root");
                    studio.read(cx).editor.read(cx).verify_clip_show_snapshot(window)?;
                }
                if screen == "capture-display-picker" {
                    let studio = root.clone().downcast::<Studio>().expect("Studio display diagnostic root");
                    let before = studio.read(cx).preferences.display.clone();
                    let picker = studio.read(cx).display_picker.clone().ok_or_else(|| anyhow::anyhow!("Display fixture did not open"))?;
                    crate::display_picker::smoke(picker, window, cx)?;
                    anyhow::ensure!(studio.read(cx).preferences.display == before && studio.read(cx).worker.is_none(), "Display cancellation changed preferences or started capture");
                    studio.update(cx, |this, cx| this.open_display_picker(cx));
                    window.draw(cx).clear();
                    println!("Display picker keyboard: missing/unsupported retention, primary policy, Tab/ShiftTab and Escape passed; no activation");
                }
                if audio_error_fixture_active(&screen).is_some() {
                    let studio = root.downcast::<Studio>().expect("Studio audio diagnostic root");
                    let focus = studio.read(cx).audio_error_focus.clone();
                    focus.focus(window);
                    let key = |window: &mut gpui::Window, cx: &mut App, text: &str| {
                        let keystroke = gpui::Keystroke::parse(text).expect("Owned audio diagnostic key");
                        window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent { keystroke: keystroke.clone(), is_held: false }), cx);
                        window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
                        window.draw(cx).clear();
                    };
                    key(window, cx, "end");
                    let end = studio.read(cx).audio_error_scroll.offset().y;
                    key(window, cx, "home");
                    anyhow::ensure!(studio.read(cx).audio_error_scroll.offset().y == gpui::px(0.), "Home did not restore audio detail top");
                    key(window, cx, "pagedown");
                    if end < gpui::px(0.) {
                        anyhow::ensure!(studio.read(cx).audio_error_scroll.offset().y < gpui::px(0.), "PageDown failed to reveal overflowing audio details");
                    }
                    key(window, cx, "pageup");
                    key(window, cx, "down");
                    key(window, cx, "up");
                    key(window, cx, "escape");
                    anyhow::ensure!(!focus.is_focused(window), "Escape stranded focus in audio details");
                    anyhow::ensure!(studio.read(cx).worker.is_none() && studio.read(cx).commands.is_none(), "Audio diagnostic navigation started capture work");
                    focus.focus(window);
                    key(window, cx, "home");
                    anyhow::ensure!(focus.is_focused(window), "Audio details lost visible keyboard focus");
                    println!("Audio details keyboard: focused scrolling, Home/End/Page/arrow navigation and Escape passed; no capture worker");
                }
                let completion = window.request_render_snapshot(output.clone())?;
                Ok::<_, anyhow::Error>(completion)
            })??;
            completion.await??;
            println!("UI snapshot: {screen} -> {}", output.display());
            Ok(())
        }
        .await;
        if let Err(error) = result {
            eprintln!("UI snapshot failed: {error:#}");
            failed.store(true, Ordering::Relaxed);
        }
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(test)]
mod tests {
    #[test]
    fn audio_warning_fixtures_are_explicit_and_do_not_seed_normal_capture_or_settings_routes() {
        assert_eq!(super::audio_error_fixture_active("capture-audio-error"), Some(true));
        assert_eq!(super::audio_error_fixture_active("capture-audio-error-stopped"), Some(false));
        for route in ["capture", "capture-saved", "capture-recording", "capture-finishing", "settings-capture-active", "tools-history"] {
            assert_eq!(super::audio_error_fixture_active(route), None, "Ordinary route was changed by an audio diagnostic fixture");
        }
    }
}
