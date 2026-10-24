//! Owned-media diagnostic: keep issuing pointer input while frames are decoding.
use super::*;
use anyhow::{Context as _, ensure};

#[derive(Default)]
pub struct ScrubPressureSmoke {
    pub cold: bool,
    began: Option<Instant>,
    proxy_ready_at_start: bool,
    proxy_ready_after_ms: Option<f64>,
    issued: Vec<(u64, u64, Instant)>,
    observed: Vec<(u64, Instant)>,
    observed_dimensions: Vec<(u32, u32)>,
    approximate_paints: usize,
    temporal_errors_frames: Vec<u64>,
    request_to_paint_ms: Vec<f64>,
    baseline: Option<Vec<u8>>,
    final_request: Option<(u64, u64, Instant)>,
}

impl EditorView {
    pub fn smoke_scrub_pressure(
        view: Entity<Self>,
        probe: &mut ScrubPressureSmoke,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<bool> {
        window.draw(cx).clear();
        let editor = view.read(cx);
        if probe.began.is_none()
            && !probe.cold
            && !editor
                .scrub
                .as_ref()
                .is_some_and(|scrub| scrub.seek_proxy_ready())
        {
            return Ok(false);
        }
        let duration = editor.model.project.duration();
        if let Some(began) = probe.began
            && probe.proxy_ready_after_ms.is_none()
            && editor
                .scrub
                .as_ref()
                .is_some_and(|scrub| scrub.seek_proxy_ready())
        {
            probe.proxy_ready_after_ms = Some(began.elapsed().as_secs_f64() * 1000.);
        }
        ensure!(
            duration >= 120,
            "Pressure proof needs at least two seconds of media"
        );
        if let Some(PaintedSeek { identity: CpuFrameIdentity { frame, seek: Some(metadata), .. }, at: painted }) = editor.painted_seek.get()
            && probe.began.is_some_and(|began| painted >= began)
            && probe
                .observed
                .last()
                .is_none_or(|(previous, _)| *previous != frame)
            && let Some((_, _, requested)) = probe
                .issued
                .iter()
                .rev()
                .find(|(serial, requested, issued)| *serial == metadata.request_serial
                    && *requested == metadata.requested_frame && painted >= *issued)
        {
            ensure!(editor.painted_frame.get().is_some_and(|(actual, _)| actual == frame),
                "Seek metadata was detached from the image actually painted");
            probe.observed.push((frame, painted));
            probe.observed_dimensions.push(editor.painted_seek.get().unwrap().identity.size);
            probe.approximate_paints += usize::from(!metadata.exact);
            probe.temporal_errors_frames.push(frame.abs_diff(metadata.requested_frame));
            probe
                .request_to_paint_ms
                .push(painted.duration_since(*requested).as_secs_f64() * 1000.);
        }
        if let Some((serial, target, requested)) = probe.final_request {
            let refinement = editor.seek_refinement.as_ref()
                .context("Final seek lost its original-source refinement state")?;
            ensure!(refinement.quick_serial == serial && refinement.target == target,
                "Source refinement became detached from the released quick request");
            let original = refinement.original;
            if original.is_some() {
                ensure!(editor.image.as_ref().is_some_and(|image| editor.cpu_image_frame.is_some_and(|identity|
                    identity.image_id == image.id && identity.frame == target
                    && identity.seek.is_some_and(|metadata| metadata.requested_frame == target && metadata.exact))),
                    "Original-source refinement cleared or replaced the retained quick image with unrelated pixels");
            }
            let settled = editor.painted_seek.get().filter(|paint| paint.identity.frame == target
                && paint.identity.size == refinement.size && paint.at >= requested
                && paint.identity.seek.is_some_and(|metadata| metadata.exact && metadata.original
                    && original.is_some_and(|(original_serial, _)| metadata.request_serial == original_serial)
                    && metadata.requested_frame == target));
            if settled.is_none() {
                ensure!(
                    requested.elapsed() < Duration::from_secs(3),
                    "Final pressure seek never painted an original-source refinement: requested bounds {:?}, expected {:?}, quick serial {}, original {:?}, current painted {:?}, latest core serial {:?}",
                    refinement.bounds, refinement.size, serial, original.map(|(serial, _)| serial),
                    editor.painted_seek.get().map(|paint| (paint.identity.frame, paint.identity.size, paint.identity.seek)),
                    editor.scrub.as_ref().and_then(|scrub| scrub.latest_request_serial())
                );
                return Ok(false);
            }
            let settled = settled.unwrap();
            let quick_painted = refinement.quick_painted.context("Original refinement has no actual quick-paint receipt")?;
            ensure!(quick_painted >= requested && settled.at >= quick_painted,
                "Original refinement preceded the released quick image");
            ensure!(
                serde_json::to_vec(&editor.model.project)? == *probe.baseline.as_ref().unwrap(),
                "Sustained ruler dragging modified the project"
            );
            ensure!(
                probe.issued.len() >= 40,
                "Too few real input events exercised sustained seeking"
            );
            let during = probe
                .observed
                .iter()
                .filter(|(_, painted)| *painted <= requested)
                .collect::<Vec<_>>();
            ensure!(
                during.len() >= 2,
                "Sustained input starved the displayed preview: {} changing frames",
                during.len()
            );
            let mut gaps = during
                .windows(2)
                .map(|pair| pair[1].1.duration_since(pair[0].1).as_secs_f64() * 1000.)
                .collect::<Vec<_>>();
            gaps.sort_by(f64::total_cmp);
            let max_gap = gaps.last().copied().unwrap_or_default();
            let first_gap = during[0]
                .1
                .duration_since(probe.began.unwrap())
                .as_secs_f64()
                * 1000.;
            let tail_gap = requested
                .saturating_duration_since(during.last().unwrap().1)
                .as_secs_f64()
                * 1000.;
            let final_ms = quick_painted.duration_since(requested).as_secs_f64() * 1000.;
            let refinement_ms = settled.at.duration_since(requested).as_secs_f64() * 1000.;
            let refinement_decode_ms = settled.at.duration_since(original.unwrap().1).as_secs_f64() * 1000.;
            let mut ages = probe.request_to_paint_ms.clone();
            ages.sort_by(f64::total_cmp);
            println!(
                "Sustained scrub pressure: {}",
                serde_json::json!({
                "issued_seek_targets":probe.issued.len(),"changing_frames_during_input":during.len(),
                "approximate_changing_paints":probe.approximate_paints,
                "requested_vs_actual_temporal_error_frames_max":probe.temporal_errors_frames.iter().max(),
                "start_mode":if probe.cold {"without_waiting_for_proxy"} else {"seek_proxy_ready"},
                "proxy_ready_at_start":probe.proxy_ready_at_start,"proxy_ready_after_ms":probe.proxy_ready_after_ms,
                "observation_gap_ms":{"median":gaps[(gaps.len()-1)/2],"p95":gaps[(gaps.len()-1)*95/100],"max":max_gap},
                "first_changed_paint_ms":first_gap,"last_paint_to_release_ms":tail_gap,
                "request_to_paint_ms":{"median":ages[(ages.len()-1)/2],"p95":ages[(ages.len()-1)*95/100],"max":ages[ages.len()-1]},
                "final_target":target,"final_target_painted_ms":final_ms,"project_unchanged":true,
                "original_refinement_painted_ms":refinement_ms,"original_refinement_request_to_paint_ms":refinement_decode_ms,
                "original_refinement_dimensions":[settled.identity.size.0,settled.identity.size.1],
                "expected_original_dimensions":[refinement.size.0,refinement.size.1],
                "viewport_fitted_bounds":[editor.preview_size.0,editor.preview_size.1],
                "drag_preview_dimensions":probe.observed_dimensions.iter().copied().collect::<std::collections::BTreeSet<_>>(),
                "original_refinement_source_provenance":true,"quick_image_retained_during_refinement":true,
                "scope":"Real synthetic GPUI pointer input; continuous two-second forward/reverse sweep; owned target paint, not monitor scanout"})
            );
            if !probe.cold && probe.proxy_ready_at_start {
                ensure!(probe.observed_dimensions.contains(&fast_seek_size(&editor.model.project, editor.preview_size)),
                    "Warm scrubbing never painted the intended high-quality preview dimensions");
            }
            ensure!(
                during.len() >= 10
                    && first_gap < 250.
                    && tail_gap < 250.
                    && max_gap < 250.
                    && ages[ages.len() - 1] < 250.
                    && final_ms < 100.,
                "Sustained seek response remains too slow"
            );
            return Ok(true);
        }
        let began = if let Some(began) = probe.began {
            began
        } else {
            probe.baseline = Some(serde_json::to_vec(&editor.model.project)?);
            probe.proxy_ready_at_start = editor
                .scrub
                .as_ref()
                .is_some_and(|scrub| scrub.seek_proxy_ready());
            let began = Instant::now();
            probe.began = Some(began);
            began
        };
        let elapsed = began.elapsed();
        let ending = elapsed >= Duration::from_secs(2);
        let phase = (elapsed.as_secs_f64() / 2.).min(1.);
        let triangle = if phase <= 0.5 {
            phase * 2.
        } else {
            (1. - phase) * 2.
        };
        // Stay away from either end and sweep most of the source in both directions.
        let target = if ending {
            if let Some(value) = std::env::var_os("RCR_PRESSURE_FINAL_FRAME") {
                let frame = value.to_str().context("Pressure final frame is not valid text")?
                    .parse::<u64>().context("Pressure final frame is not an unsigned integer")?;
                ensure!(frame < duration, "Pressure final frame is outside the source");
                frame
            } else { duration * 7 / 10 }
        } else {
            ((duration - 1) as f64 * (0.1 + triangle * 0.8)).round() as u64
        };
        if probe
            .issued
            .last()
            .is_some_and(|(_, previous, _)| *previous == target)
        {
            return Ok(false);
        }
        let bounds = editor
            .strip_bounds
            .get()
            .context("Pressure seek strip not painted")?;
        let ruler = editor
            .ruler_bounds
            .get()
            .context("Pressure seek ruler not painted")?;
        let viewport = editor.timeline_window();
        let fraction = (target.saturating_sub(viewport.start) as f64 + 0.25) / viewport.span as f64;
        let position = point(
            bounds.left() + bounds.size.width * fraction as f32,
            ruler.center().y,
        );
        let initial = probe.issued.is_empty();
        let active_before = window.is_window_active();
        let scrubbing_before = editor.scrubbing;
        let playhead_before = editor.playhead;
        let issued = Instant::now();
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: (!initial).then_some(MouseButton::Left),
                modifiers: Default::default(),
            }),
            cx,
        );
        if initial {
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    position,
                    button: MouseButton::Left,
                    click_count: 1,
                    first_mouse: false,
                    modifiers: Default::default(),
                }),
                cx,
            );
        }
        let actual = view.read(cx).playhead;
        ensure!(
            actual.abs_diff(target) <= 1 && view.read(cx).scrubbing,
            "Continuous pointer input did not control the playhead: {}",
            serde_json::json!({
                "actual_frame":actual,"target_frame":target,"previous_frame":playhead_before,
                "scrubbing":view.read(cx).scrubbing,"scrubbing_before":scrubbing_before,
                "window_active":window.is_window_active(),"window_active_before":active_before,
                "initial_pointer_down":initial,"input_index":probe.issued.len(),
                "elapsed_ms":elapsed.as_secs_f64()*1000.,
                "point":[f32::from(position.x),f32::from(position.y)],
                "ruler_before":[f32::from(ruler.left()),f32::from(ruler.top()),f32::from(ruler.size.width),f32::from(ruler.size.height)],
                "strip_before":[f32::from(bounds.left()),f32::from(bounds.top()),f32::from(bounds.size.width),f32::from(bounds.size.height)],
                "ruler_after":view.read(cx).ruler_bounds.get().map(|area| [f32::from(area.left()),f32::from(area.top()),f32::from(area.size.width),f32::from(area.size.height)]),
                "strip_after":view.read(cx).strip_bounds.get().map(|area| [f32::from(area.left()),f32::from(area.top()),f32::from(area.size.width),f32::from(area.size.height)]),
                "timeline_before":{"start":viewport.start,"span":viewport.span},
                "timeline_after":{"start":view.read(cx).timeline_window().start,"span":view.read(cx).timeline_window().span},
                "window_size":[f32::from(window.viewport_size().width),f32::from(window.viewport_size().height)],
                "editor_focused":view.read(cx).focus.is_focused(window),
                "modal_or_picker":view.read(cx).export_details_open || view.read(cx).playback_details.open || view.read(cx).picker || view.read(cx).preset_menu,
            })
        );
        let serial = view.read(cx).scrub.as_ref().and_then(|scrub| scrub.latest_request_serial())
            .context("Pointer input did not issue a seek request")?;
        probe.issued.push((serial, actual, issued));
        ensure!(
            probe.issued.len() <= 512,
            "Pressure fixture exceeded bounded input history"
        );
        if ending {
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    position,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: Default::default(),
                }),
                cx,
            );
            ensure!(
                !view.read(cx).scrubbing,
                "Final pointer release left scrubbing active"
            );
            let exact_serial = view.read(cx).scrub.as_ref().and_then(|scrub| scrub.latest_request_serial())
                .context("Release did not issue an exact seek request")?;
            probe.issued.push((exact_serial, actual, issued));
            probe.final_request = Some((exact_serial, actual, issued));
        }
        Ok(false)
    }
}
