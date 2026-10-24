//! Owned-window, actual-source teardown proof. Invoked explicitly by the native fixture.
use super::*;
const RESOURCE_CYCLES: u32 = 4;

pub struct EditorResourceSmoke {
    baseline: core::ScrubMemorySnapshot,
    hidden: bool,
    cycles: u32,
    started: Instant,
    project: Option<serde_json::Value>,
    images: Vec<std::sync::Weak<RenderImage>>,
    warm_step: u32,
    warm_pending: Option<(u64, Instant)>,
    settle_started: Option<Instant>,
    released_at: Option<Instant>,
}
impl Default for EditorResourceSmoke {
    fn default() -> Self {
        Self {
            baseline: core::scrub_memory_snapshot(),
            hidden: false,
            cycles: 0,
            started: Instant::now(),
            project: None,
            images: Vec::new(),
            warm_step: 0,
            warm_pending: None,
            settle_started: None,
            released_at: None,
        }
    }
}
impl EditorResourceSmoke {
    pub fn hidden(&self) -> bool {
        self.hidden
    }
}
fn core_snapshot(value: core::ScrubMemorySnapshot) -> serde_json::Value {
    serde_json::json!({"active_workers":value.active_workers,"cached_bgra_bytes":value.cached_bgra_bytes,
        "cached_software_surface_bytes":value.cached_software_surface_bytes,"cached_gpu_surface_bytes":value.cached_gpu_surface_bytes})
}
fn atlas_snapshot(window: &Window) -> serde_json::Value {
    #[cfg(windows)]
    if let Some(atlas) = window.windows_atlas_diagnostics() {
        return serde_json::json!({"texture_creates":atlas.texture_creates,"texture_uploads":atlas.texture_uploads,
            "upload_bytes":atlas.upload_bytes,"tile_removals":atlas.tile_removals,"live_textures":atlas.live_textures,
            "peak_live_textures":atlas.peak_live_textures,"device_resets":atlas.device_resets,
            "empty_cache_reuses":atlas.empty_cache_reuses,"empty_cache_evictions":atlas.empty_cache_evictions,
            "cached_empty_textures":atlas.cached_empty_textures,"cached_empty_bytes":atlas.cached_empty_bytes});
    }
    let _ = window;
    serde_json::Value::Null
}
impl EditorView {
    pub fn retained_media_snapshot(&self) -> serde_json::Value {
        let mut ids = HashSet::new();
        let mut image_bytes = 0usize;
        for image in self
            .image
            .iter()
            .chain(self.retired.iter())
            .chain(self.thumbnails.values().flatten())
        {
            if ids.insert(image.id) {
                for index in 0..image.frame_count() {
                    image_bytes += image.as_bytes(index).map_or(0, <[u8]>::len);
                }
            }
        }
        let pending_bytes = self
            .next_frame
            .as_ref()
            .map_or(0, |frame| frame.frame.bgra.len());
        let waveform_bytes = self
            .waveforms
            .values()
            .map(|bins| bins.capacity() * std::mem::size_of::<WaveBin>())
            .sum::<usize>();
        serde_json::json!({"host_bgra_payload_bytes":image_bytes + pending_bytes,"host_waveform_capacity_bytes":waveform_bytes,
            "image_count":ids.len(),"pending_frame_bytes":pending_bytes,"pending_media_tasks":self.pending.len(),
            "scrub_session":self.scrub.is_some(),"playback_session":self.preview.is_some(),
            "preserved_edit_sessions":self.sessions.entries.len(),"preserved_project_assets":self.model.project.assets.len(),
            "scope":"Owned CPU image pixels and waveform capacity; excludes edit history, allocator, DLLs, driver and GPU allocations"})
    }
    pub fn smoke_resources(
        view: Entity<Self>,
        probe: &mut EditorResourceSmoke,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            probe.started.elapsed() < Duration::from_secs(45),
            "Editor resource teardown timed out: hidden={}, core={}, owned={}",
            probe.hidden,
            core_snapshot(core::scrub_memory_snapshot()),
            view.read(cx).retained_media_snapshot()
        );
        let current = core::scrub_memory_snapshot();
        if probe.hidden {
            let editor = view.read(cx);
            let snapshot = editor.retained_media_snapshot();
            anyhow::ensure!(
                snapshot["host_bgra_payload_bytes"] == 0
                    && snapshot["host_waveform_capacity_bytes"] == 0
                    && editor.scrub.is_none()
                    && editor.preview.is_none(),
                "Hidden editor retained derived media: {snapshot}"
            );
            if current != probe.baseline
                || probe.images.iter().any(|image| image.upgrade().is_some())
            {
                probe.released_at = None;
                return Ok(false);
            }
            let released_at = *probe.released_at.get_or_insert_with(Instant::now);
            if released_at.elapsed() < Duration::from_secs(2) {
                return Ok(false);
            }
            let project = serde_json::to_value(&editor.model.project)?;
            anyhow::ensure!(
                Some(project) == probe.project,
                "Hiding editor changed the project"
            );
            println!(
                "Editor resources released: {}",
                serde_json::json!({"cycle":probe.cycles + 1,"core":core_snapshot(current),
                "editor":snapshot,"process":crate::memory_usage::snapshot(),"atlas":atlas_snapshot(window),"cpu_image_weak_refs_released":true,
                "release_cooldown_ms":released_at.elapsed().as_secs_f64() * 1000.})
            );
            view.update(cx, |editor, cx| editor.show(cx));
            probe.hidden = false;
            probe.cycles += 1;
            probe.warm_step = 0;
            probe.warm_pending = None;
            probe.settle_started = None;
            probe.released_at = None;
            return Ok(false);
        }
        let editor = view.read(cx);
        if !editor.ready
            || editor.image.is_none()
            || editor.painted_seek.get().is_none()
            || current.active_workers <= probe.baseline.active_workers
            || current.cached_bgra_bytes <= probe.baseline.cached_bgra_bytes
        {
            return Ok(false);
        }
        anyhow::ensure!(
            editor.error.is_none(),
            "Resource fixture source playback failed: {:?}",
            editor.error
        );
        if probe.cycles < RESOURCE_CYCLES {
            let duration = editor.model.project.duration();
            anyhow::ensure!(
                duration >= 10,
                "Resource warm-up needs at least ten source frames"
            );
            if let Some((target, requested_at)) = probe.warm_pending {
                if !editor.painted_seek.get().is_some_and(|painted| {
                    painted.at >= requested_at
                        && painted.identity.frame == target
                        && painted.identity.seek.is_some_and(|metadata| {
                            metadata.requested_frame == target && metadata.exact
                        })
                }) {
                    return Ok(false);
                }
                probe.warm_pending = None;
                probe.warm_step += 1;
                if probe.warm_step == 8 {
                    probe.settle_started = Some(Instant::now());
                }
            }
            if probe.warm_step < 8 {
                let target = duration.saturating_mul(u64::from(probe.warm_step + 1)) / 10;
                probe.warm_pending = Some((target, Instant::now()));
                view.update(cx, |editor, cx| editor.seek(target, cx));
                return Ok(false);
            }
            let original_settled = editor.painted_seek.get().is_some_and(|painted| {
                painted.identity.frame == duration.saturating_mul(8) / 10
                    && painted
                        .identity
                        .seek
                        .is_some_and(|metadata| metadata.original && metadata.exact)
            });
            if !original_settled
                && probe
                    .settle_started
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
            {
                return Ok(false);
            }
        }
        let project = serde_json::to_value(&editor.model.project)?;
        let snapshot = editor.retained_media_snapshot();
        let images = editor
            .image
            .iter()
            .chain(editor.retired.iter())
            .chain(editor.thumbnails.values().flatten())
            .map(Arc::downgrade)
            .collect();
        if probe.project.is_none() {
            probe.project = Some(project);
        } else {
            anyhow::ensure!(
                Some(project) == probe.project,
                "Reopening editor changed the project"
            );
        }
        if probe.cycles == RESOURCE_CYCLES {
            return Ok(true);
        }
        println!(
            "Editor resources active: {}",
            serde_json::json!({"cycle":probe.cycles + 1,"core":core_snapshot(current),
            "editor":snapshot,"process":crate::memory_usage::snapshot(),"atlas":atlas_snapshot(window),"warm_distinct_seeks":probe.warm_step,
            "settled_original":view.read(cx).painted_seek.get().is_some_and(|painted| painted.identity.seek.is_some_and(|metadata| metadata.original))})
        );
        probe.images = images;
        view.update(cx, |editor, cx| editor.hide(cx));
        probe.hidden = true;
        probe.released_at = None;
        Ok(false)
    }
}
