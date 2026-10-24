//! Native render fixtures backed exclusively by generated pixels and GPUI events.
use super::*;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[derive(Clone, Copy)]
enum Route {
    HoverRegion,
    Region,
    BlurDraft,
    PixelateDraft,
    Properties,
    Retry,
}

/// Open an isolated hidden overlay, exercise its real input handlers, and export
/// only that window's GPU render target. This never captures the user's desktop.
/// Routes: overlay-region, overlay-blur-draft, overlay-pixelate-draft,
/// overlay-properties (a committed, selected blur with its property controls).
pub fn capture_fixture(
    route: String,
    output: PathBuf,
    failed: Arc<AtomicBool>,
    requested: Size<Pixels>,
    cx: &mut App,
) -> anyhow::Result<()> {
    let scenario = match route.as_str() {
        "overlay-hover-region" => Route::HoverRegion,
        "overlay-region" | "overlay-region-tools" => Route::Region,
        "overlay-blur-draft" => Route::BlurDraft,
        "overlay-pixelate-draft" => Route::PixelateDraft,
        "overlay-properties" | "overlay-selected-properties" => Route::Properties,
        "overlay-retry" => Route::Retry,
        _ => anyhow::bail!("Unknown overlay fixture route: {route}"),
    };
    let width = f32::from(requested.width).round() as u32;
    let height = f32::from(requested.height).round() as u32;
    anyhow::ensure!(
        (800..=3840).contains(&width) && (600..=2160).contains(&height),
        "Overlay fixture dimensions must be between 800×600 and 3840×2160"
    );
    let mut frame = synthetic_frame(width, height)?;
    if matches!(scenario, Route::HoverRegion) { frame.origin = rcr_toolkit::types::Point { x: -640, y: -180 }; }
    let (reply, reply_rx) = mpsc::sync_channel(1);
    let handle = if matches!(scenario, Route::HoverRegion) {
        let targets = crate::region_targets::TargetSnapshot::from_rectangles(vec![
            Rect::new(-620, -160, width - 40, height - 40),
            Rect::new(-640 + (width / 4) as i32, -180 + (height / 5) as i32, width / 2, height / 3),
        ]);
        open_with_targets(frame, true, false, reply, CancelToken::new(), false, targets, cx)?
    } else { open(frame, true, false, reply, CancelToken::new(), false, cx)? };
    let window: AnyWindowHandle = handle.into();
    // Hidden popup bounds do not guarantee an initialized native render target.
    // Match the Studio fixture's explicit resize before drawing or dispatching input.
    window.update(cx, |_, window, _| window.resize(requested))?;
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
            let mut stage = 0;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                if matches!(scenario, Route::Retry)
                    && let Ok(Some(submission)) = reply_rx.try_recv() {
                        submission.completion.try_send(OverlayCompletion::Failed(format!(
                            "Couldn't copy image: the clipboard is busy. Your frozen selection and annotations remain open. Try Copy again, or choose Save. Synthetic preview only; no clipboard or file operation ran.\nDiagnostic: {}", "X".repeat(180))))?;
                }
                let ready = window.update(cx, |root, window, cx| {
                    if window.viewport_size() != requested {
                        window.resize(requested);
                        return Ok(false);
                    }
                    let view = root
                        .downcast::<CaptureOverlay>()
                        .expect("Overlay fixture root");
                    fixture_step(view, scenario, &mut stage, window, cx)
                })??;
                if ready {
                    break;
                }
                anyhow::ensure!(
                    started.elapsed() < Duration::from_secs(20),
                    "Overlay fixture did not become ready (stage {stage})"
                );
            }
            let completion = window.update(cx, |root, window, cx| {
                let view = root
                    .downcast::<CaptureOverlay>()
                    .expect("Overlay fixture root");
                anyhow::ensure!(
                    matches!(scenario, Route::Retry) || view.read(cx).error.is_none(),
                    "Overlay fixture error: {:?}",
                    view.read(cx).error
                );
                anyhow::ensure!(
                    window.viewport_size() == requested,
                    "Overlay native viewport does not match requested fixture size"
                );
                window.draw(cx).clear();
                window.request_render_snapshot(output.clone())
            })??;
            completion.await??;
            println!("Native overlay snapshot: {route} -> {}", output.display());
            Ok(())
        }
        .await;
        if let Err(error) = result {
            eprintln!("Overlay snapshot failed: {error:#}");
            failed.store(true, Ordering::Relaxed);
        }
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();
    Ok(())
}

fn synthetic_frame(width: u32, height: u32) -> anyhow::Result<Frame> {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let xf = x as f32 / width as f32;
            let yf = y as f32 / height as f32;
            let color = if yf < 0.08 {
                [18, 24, 32, 255]
            } else if xf < 0.12 {
                [24, 31, 42, 255]
            } else if (0.17..0.92).contains(&xf) && (0.15..0.77).contains(&yf) {
                if (0.22..0.82).contains(&xf) && (0.28..0.60).contains(&yf) {
                    if (x / 7 + y / 7) % 2 == 0 {
                        [48, 145, 125, 255]
                    } else {
                        [235, 197, 92, 255]
                    }
                } else if (0.20..0.86).contains(&xf) && (y / 12) % 3 == 0 {
                    [183, 196, 207, 255]
                } else {
                    [235, 240, 245, 255]
                }
            } else {
                [39, 49, 63, 255]
            };
            rgba.extend_from_slice(&color);
        }
    }
    Ok(Frame::from_rgba8(width, height, rgba)?)
}

fn mouse(window: &mut Window, cx: &mut App, from: Point<Pixels>, to: Point<Pixels>, release: bool) {
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position: from,
            pressed_button: None,
            modifiers: Default::default(),
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position: from,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position: to,
            pressed_button: Some(MouseButton::Left),
            modifiers: Default::default(),
        }),
        cx,
    );
    if release {
        up(window, cx, to);
    }
}
fn up(window: &mut Window, cx: &mut App, position: Point<Pixels>) {
    window.dispatch_event(
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count: 1,
        }),
        cx,
    );
}
fn fixture_step(
    view: Entity<CaptureOverlay>,
    route: Route,
    stage: &mut usize,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<bool> {
    use anyhow::{Context, ensure};
    window.draw(cx).clear();
    if !view.read(cx).image.ready() {
        return Ok(false);
    }
    if matches!(route, Route::Retry) && *stage == 4 {
        return Ok(view.read(cx).pending.is_none() && view.read(cx).error.is_some());
    }
    ensure!(
        view.read(cx).error.is_none(),
        "Overlay rendering failed: {:?}",
        view.read(cx).error
    );
    let bounds = view
        .read(cx)
        .bounds
        .get()
        .context("Overlay canvas has not been painted")?;
    let at = |x: f32, y: f32| bounds.origin + point(bounds.size.width * x, bounds.size.height * y);
    if matches!(route, Route::HoverRegion) {
        let pointer = at(0.48, 0.37);
        let motion = |window: &mut Window, cx: &mut App, position| window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent {
            position, pressed_button: None, modifiers: Default::default() }), cx);
        motion(window, cx, pointer);
        let candidate = view.read(cx).hovered.context("Injected child target was not highlighted")?;
        ensure!(candidate == Rect::new(view.read(cx).selection.size.0 as i32 / 4, view.read(cx).selection.size.1 as i32 / 5,
            view.read(cx).selection.size.0 / 2, view.read(cx).selection.size.1 / 3), "Hover did not pick the smaller nested pane");
        ensure!(view.read(cx).selection.rect.is_none(), "Hover committed a selection");
        // Deliberately remove prior hover: a first click must hit-test its own position.
        view.update(cx, |this, _| { this.hovered = None; this.cursor_pixel = None; });
        window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { position: pointer, button: MouseButton::Left,
            modifiers: Default::default(), click_count: 1, first_mouse: false }), cx);
        up(window, cx, pointer);
        ensure!(view.read(cx).selection.rect == Some(candidate) && view.read(cx).pending.is_none() && view.read(cx).reply.is_some(),
            "Click did not select detected pane for annotation without completing capture");
        view.update(cx, |this, _| { this.selection.clear(); this.hovered = None; this.cursor_pixel = None; });
        motion(window, cx, pointer);
        mouse(window, cx, pointer, at(0.62, 0.46), true);
        ensure!(view.read(cx).selection.rect.is_some_and(|rect| rect != candidate), "Free drag did not override hovered pane");
        view.update(cx, |this, _| { this.selection.clear(); this.tool = Some(a::Tool::Blur); this.hovered = None; this.cursor_pixel = None; });
        motion(window, cx, pointer);
        ensure!(view.read(cx).hovered.is_none() && view.read(cx).cursor_pixel.is_none(), "Hover interfered with annotation mode");
        view.update(cx, |this, _| { this.tool = None; this.pending = Some((candidate, OverlayAction::Copy)); });
        motion(window, cx, pointer);
        ensure!(view.read(cx).hovered.is_none() && view.read(cx).selection.rect.is_none(), "Hover changed a pending capture");
        view.update(cx, |this, _| { this.pending = None; });
        motion(window, cx, pointer);
        window.draw(cx).clear();
        ensure!(view.read(cx).hovered == Some(candidate) && view.read(cx).cursor_pixel.is_some(), "Hover guide was not restored");
        return Ok(true);
    }
    if *stage == 0 {
        mouse(window, cx, at(0.14, 0.12), at(0.88, 0.70), true);
        ensure!(
            view.read(cx).selection.rect.is_some(),
            "Fixture region selection failed"
        );
        *stage = 1;
        return Ok(false);
    }
    if matches!(route, Route::Region) {
        return Ok(true);
    }
    if *stage == 1 {
        window.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke: Keystroke {
                    key: if matches!(route, Route::PixelateDraft) {
                        "p"
                    } else {
                        "b"
                    }
                    .into(),
                    key_char: None,
                    modifiers: Default::default(),
                },
                is_held: false,
            }),
            cx,
        );
        mouse(window, cx, at(0.29, 0.33), at(0.69, 0.54), false);
        ensure!(
            view.read(cx).draft_rect().is_some(),
            "Fixture effect has no immediate draft guide"
        );
        *stage = 2;
        return Ok(false);
    }
    if *stage == 2 {
        if !view
            .read(cx)
            .preview
            .as_ref()
            .is_some_and(|p| p.overlay.creation.is_some())
        {
            return Ok(false);
        }
        if !matches!(route, Route::Properties | Route::Retry) {
            return Ok(true);
        }
        up(window, cx, at(0.69, 0.54));
        *stage = 3;
        return Ok(false);
    }
    let ready = view.read(cx).preview.as_ref().is_some_and(|p| {
        p.objects == 1
            && p.properties
                .as_ref()
                .is_some_and(|p| p.tool == a::Tool::Blur && p.selected == 1)
    }) && view.read(cx).properties_bounds.get().is_some();
    if ready && matches!(route, Route::Retry) {
        view.update(cx, |this, cx| this.finish(OverlayAction::Copy, cx));
        *stage = 4;
        return Ok(false);
    }
    Ok(ready)
}
