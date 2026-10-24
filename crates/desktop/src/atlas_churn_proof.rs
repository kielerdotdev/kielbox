//! Explicit owned-window atlas reuse diagnostic. No capture, user files or OS input.
use anyhow::{Context as _, Result, ensure};
use gpui::{prelude::*, *};
use std::{path::PathBuf, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};

struct AtlasView {
    stable: Vec<Arc<RenderImage>>,
    changing: Vec<Arc<RenderImage>>,
    retired: Vec<Arc<RenderImage>>,
}
fn tile(dimensions: (u32, u32), color: [u8; 4]) -> Arc<RenderImage> {
    let mut pixels = vec![0; dimensions.0 as usize * dimensions.1 as usize * 4];
    for pixel in pixels.as_chunks_mut::<4>().0 { *pixel = color; }
    crate::image_editor::render_image(dimensions.0, dimensions.1, pixels).expect("owned atlas image")
}
impl Render for AtlasView {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        for retired in self.retired.drain(..) { let _ = window.drop_image(retired); }
        let images = self.stable.iter().chain(&self.changing).cloned().collect::<Vec<_>>();
        let single = images.len() == 1;
        div().size_full().bg(rgb(0)).child(canvas(|_, _, _| {}, move |bounds, _, window, _| {
            for (index, image) in images.into_iter().enumerate() {
                let target = if single { bounds } else { Bounds::new(bounds.origin + point(
                    bounds.size.width * (index % 2) as f32 / 2.,
                    bounds.size.height * (index / 2) as f32 / 2.),
                    size(bounds.size.width / 2., bounds.size.height / 2.)) };
                let _ = window.paint_image(target, Corners::default(), image, 0, false);
            }
        }).size_full())
    }
}

fn preview_step(window: AnyWindowHandle, cx: &mut AsyncApp,
    dimensions: Option<(u32, u32)>, color: [u8; 4]) -> Result<WindowsAtlasDiagnostics> {
    window.update(cx, |root, window, cx| -> Result<_> {
        let view = root.downcast::<AtlasView>().map_err(|_| anyhow::anyhow!("Atlas policy root changed"))?;
        view.update(cx, |view, cx| -> Result<()> {
            ensure!(view.stable.is_empty(), "Cache policy requires an isolated preview");
            view.retired.extend(std::mem::take(&mut view.changing));
            if let Some(dimensions) = dimensions { view.changing.push(tile(dimensions, color)); }
            cx.notify(); Ok(())
        })?;
        window.draw(cx).clear(); window.present_for_diagnostics()?;
        window.windows_atlas_diagnostics().context("Atlas policy diagnostics unavailable")
    })?
}
async fn assert_preview_color(window: AnyWindowHandle, cx: &mut AsyncApp,
    path: PathBuf, expected: [u8; 4]) -> Result<()> {
    let completion = window.update(cx, |_, window, cx| {
        window.draw(cx).clear(); window.request_render_snapshot(path.clone())
    })??;
    completion.await??;
    let pixels = image::open(path)?.into_rgba8();
    for (x, y) in [(1, 1), (3, 1), (1, 3), (3, 3)] {
        ensure!(pixels.get_pixel(pixels.width() * x / 4, pixels.height() * y / 4).0 == expected,
            "Cached preview geometry replacement displayed stale pixels");
    }
    Ok(())
}
async fn cache_policy(window: AnyWindowHandle, cx: &mut AsyncApp, output: &std::path::Path) -> Result<serde_json::Value> {
    let nominal_bytes = 1280 * 1024 * 4;
    let parked = preview_step(window, cx, None, [0; 4])?;
    ensure!(parked.cached_empty_textures == 1 && parked.cached_empty_bytes == nominal_bytes
        && parked.live_textures == 1, "Last preview was not retained within its nominal budget: {parked:?}");
    assert_preview_color(window, cx, output.join("policy-empty.png"), [0, 0, 0, 255]).await?;
    let portrait = preview_step(window, cx, Some((720, 1280)), [60, 40, 220, 255])?;
    ensure!(portrait.texture_creates == parked.texture_creates + 1 && portrait.cached_empty_textures == 1
        && portrait.live_textures == 2, "Incompatible geometry unexpectedly reused an undersized texture");
    assert_preview_color(window, cx, output.join("policy-portrait.png"), [220, 40, 60, 255]).await?;
    let portrait_parked = preview_step(window, cx, None, [0; 4])?;
    ensure!(portrait_parked.empty_cache_evictions == parked.empty_cache_evictions + 1
        && portrait_parked.cached_empty_textures == 1 && portrait_parked.cached_empty_bytes == nominal_bytes
        && portrait_parked.live_textures == 1, "Geometry eviction retained multiple empty resources");
    // 18 MiB, with aligned dimensions: exceeds the 16 MiB empty-resource limit.
    let oversized = preview_step(window, cx, Some((2048, 2304)), [60, 200, 40, 255])?;
    ensure!(oversized.texture_creates == portrait_parked.texture_creates + 1
        && oversized.cached_empty_textures == 1 && oversized.live_textures == 2,
        "Oversized active source displaced the existing bounded cache");
    assert_preview_color(window, cx, output.join("policy-oversized.png"), [40, 200, 60, 255]).await?;
    let after_oversized = preview_step(window, cx, None, [0; 4])?;
    ensure!(after_oversized.cached_empty_textures == 1 && after_oversized.cached_empty_bytes == nominal_bytes
        && after_oversized.live_textures == 1
        && after_oversized.empty_cache_evictions == portrait_parked.empty_cache_evictions,
        "Oversized empty resource was cached or evicted an eligible resource");
    let reused = preview_step(window, cx, Some((720, 1280)), [200, 80, 20, 255])?;
    ensure!(reused.texture_creates == after_oversized.texture_creates
        && reused.empty_cache_reuses == after_oversized.empty_cache_reuses + 1
        && reused.cached_empty_textures == 0 && reused.live_textures == 1,
        "A rejected oversized source broke reuse of the previous geometry");
    assert_preview_color(window, cx, output.join("policy-reused-portrait.png"), [20, 80, 200, 255]).await?;
    preview_step(window, cx, None, [0; 4])?;
    preview_step(window, cx, Some((1280, 720)), [60, 200, 40, 255])?;
    let landscape = preview_step(window, cx, None, [0; 4])?;
    ensure!(landscape.cached_empty_textures == 1 && landscape.cached_empty_bytes == nominal_bytes
        && landscape.live_textures == 1 && landscape.empty_cache_evictions == parked.empty_cache_evictions + 2,
        "Return to landscape did not replace incompatible cached geometry");
    let final_stats = preview_step(window, cx, Some((1280, 720)), [200, 80, 20, 255])?;
    ensure!(final_stats.texture_creates == landscape.texture_creates
        && final_stats.empty_cache_reuses == landscape.empty_cache_reuses + 1
        && final_stats.cached_empty_textures == 0 && final_stats.live_textures == 1
        && final_stats.device_resets == parked.device_resets, "Final cached landscape reuse failed");
    assert_preview_color(window, cx, output.join("policy-final-landscape.png"), [20, 80, 200, 255]).await?;
    Ok(serde_json::json!({"nominal_empty_budget_per_kind":16*1024*1024,
        "nominal_retained_bytes":nominal_bytes,"oversized_source_dimensions":[2048,2304],
        "incompatible_geometry_evictions":2,"oversize_rejected_without_evicting_eligible":true,
        "reuse_after_oversize_and_geometry_switch":true,"owned_pixels_verified":true,
        "scope":"Polychrome resources only; no device-loss injection; nominal bytes are not driver VRAM accounting"}))
}

pub fn run(output: PathBuf, require_reuse: bool, video_preview: bool, failed: Arc<AtomicBool>, cx: &mut App) -> Result<()> {
    let dimensions = if video_preview { (1280, 720) } else { (512, 512) };
    let cycles = if video_preview { 120 } else { 300 };
    let replaced = if video_preview { 1 } else { 2 };
    let source_bytes = u64::from(dimensions.0) * u64::from(dimensions.1) * 4;
    let warmup_uploads = if video_preview { 1 } else { 4 };
    let output = if output.is_absolute() { output } else { std::env::current_dir()?.join(output) };
    std::fs::create_dir_all(&output)?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    let output = output.join(format!("run-{stamp}-{}", std::process::id()));
    std::fs::create_dir(&output)?;
    std::fs::write(output.join("owned-proof.txt"), b"rcr-owned-atlas-churn-v1\nSynthetic BGRA images, own hidden window only.\n")?;
    let requested = size(px(512.), px(512.));
    let window: AnyWindowHandle = cx.open_window(WindowOptions {
        show: false, focus: false, titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), requested))),
        ..Default::default()
    }, |_, cx| cx.new(|_| AtlasView {
        stable: if video_preview { vec![] } else { vec![tile(dimensions, [160, 80, 32, 255]), tile(dimensions, [160, 80, 32, 255])] },
        changing: (0..replaced).map(|_| tile(dimensions, [60, 200, 40, 255])).collect(), retired: Vec::new(),
    }))?.into();
    window.update(cx, |_, window, _| window.resize(requested))?;
    cx.spawn(async move |cx| {
        let result: Result<()> = async {
            let ready = Instant::now();
            loop {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                if window.update(cx, |_, window, _| window.viewport_size())? == requested { break; }
                ensure!(ready.elapsed() < Duration::from_secs(5), "Owned atlas window resize timed out");
            }
            let initial = window.update(cx, |_, window, cx| -> Result<_> {
                window.draw(cx).clear(); window.present_for_diagnostics()?;
                window.windows_atlas_diagnostics().context("Windows atlas diagnostics unavailable")
            })??;
            ensure!(initial.texture_creates == 1 && initial.live_textures == 1
                && initial.texture_uploads == warmup_uploads
                && initial.upload_bytes == source_bytes * warmup_uploads,
                "Atlas warmup did not place the expected source images in one texture: {initial:?}");
            let mut timings = Vec::with_capacity(cycles);
            let started = Instant::now();
            for index in 1..=cycles {
                cx.background_executor().timer(Duration::from_millis(1)).await;
                ensure!(started.elapsed() < Duration::from_secs(15), "Owned atlas churn exceeded 15-second budget");
                let elapsed = window.update(cx, |root, window, cx| -> Result<_> {
                    let at = Instant::now();
                    let view = root.downcast::<AtlasView>().map_err(|_| anyhow::anyhow!("Atlas proof root changed"))?;
                    view.update(cx, |view, cx| {
                        let color = if index % 2 == 0 { [60, 40, 220, 255] } else { [60, 200, 40, 255] };
                        let next = (0..replaced).map(|_| tile(dimensions, color)).collect();
                        view.retired.extend(std::mem::replace(&mut view.changing, next));
                        cx.notify();
                    });
                    window.draw(cx).clear(); window.present_for_diagnostics()?;
                    Ok(at.elapsed().as_secs_f64() * 1000.)
                })??;
                timings.push(elapsed);
            }
            let timed_cycles_elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
            let final_stats = window.update(cx, |_, window, _| window.windows_atlas_diagnostics())?
                .context("Atlas diagnostics disappeared")?;
            ensure!(final_stats.device_resets == initial.device_resets, "Device reset invalidated atlas comparison");
            let creates = final_stats.texture_creates.saturating_sub(initial.texture_creates);
            let uploads = final_stats.texture_uploads.saturating_sub(initial.texture_uploads);
            let removals = final_stats.tile_removals.saturating_sub(initial.tile_removals);
            let upload_bytes = final_stats.upload_bytes.saturating_sub(initial.upload_bytes);
            let expected_updates = cycles as u64 * replaced;
            ensure!(uploads == expected_updates && removals == expected_updates, "Actual atlas path was not exercised: {uploads} uploads/{removals} removals");
            ensure!(upload_bytes == expected_updates * source_bytes, "Atlas upload byte count changed the comparison workload");
            ensure!(final_stats.peak_live_textures <= 4, "Atlas fixture exceeded its bounded live-texture budget");
            if require_reuse { ensure!(creates == 0, "Reclaimed dynamic bucket created {creates} additional textures"); }
            if require_reuse && video_preview {
                ensure!(final_stats.empty_cache_reuses.saturating_sub(initial.empty_cache_reuses) == cycles as u64
                    && final_stats.cached_empty_textures == 0,
                    "Timed preview replacements did not consume the bounded cache once each");
            }
            let path = output.join("final.png");
            let completion = window.update(cx, |_, window, cx| {
                window.draw(cx).clear(); window.request_render_snapshot(path.clone())
            })??;
            completion.await??;
            let pixels = image::open(&path)?.into_rgba8();
            let top_color = if video_preview { [220, 40, 60, 255] } else { [32, 80, 160, 255] };
            for (x, y, expected) in [(1, 1, top_color), (3, 1, top_color),
                (1, 3, [220, 40, 60, 255]), (3, 3, [220, 40, 60, 255])] {
                ensure!(pixels.get_pixel(pixels.width() * x / 4, pixels.height() * y / 4).0 == expected,
                    "Atlas reuse corrupted an owned stable or changed tile");
            }
            timings.sort_by(f64::total_cmp);
            let policy = if require_reuse && video_preview { Some(cache_policy(window, cx, &output).await?) } else { None };
            println!("Atlas churn proof passed: {}", serde_json::json!({
                "scenario":if video_preview {"single_video_preview"} else {"stable_and_changing_tiles"},
                "cycles":cycles,"source_dimensions":[dimensions.0,dimensions.1],"stable_tiles":if video_preview {0} else {2},"replaced_tiles_per_cycle":replaced,
                "texture_creates_after_warmup":creates,"uploads_after_warmup":uploads,"removals_after_warmup":removals,
                "warmup":format!("{initial:?}"),"upload_bytes_after_warmup":upload_bytes,
                "live_textures":final_stats.live_textures,"peak_live_textures":final_stats.peak_live_textures,
                "empty_cache_reuses_after_warmup":final_stats.empty_cache_reuses.saturating_sub(initial.empty_cache_reuses),
                "cache_policy":policy,
                "update_scene_present_ms":{"median":timings[(cycles-1)/2],"p95":timings[((cycles-1) as f64*0.95).round() as usize],"max":timings[cycles-1]},
                "require_reuse":require_reuse,"timed_cycles_elapsed_ms":timed_cycles_elapsed_ms,
                "elapsed_ms":started.elapsed().as_secs_f64()*1000.,
                "owned_final_pixels_verified":true,"scope":"Synthetic owned GPUI window; not user capture or monitor scanout",
                "output":output}));
            Ok(())
        }.await;
        if let Err(error) = result { failed.store(true, Ordering::Relaxed); eprintln!("Atlas churn proof failed: {error:#}"); }
        let _ = window.update(cx, |_, window, _| window.remove_window());
        let _ = cx.update(|cx| cx.quit());
    }).detach();
    Ok(())
}
