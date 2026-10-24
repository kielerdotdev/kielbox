//! Owned local-file transport proof. No capture, preferences, input writes or GPU readback.
//! --source-change-proof appends only to an exclusively created workspace copy.
#[cfg(windows)]
mod proof {
    use anyhow::{ensure, Context, Result};
    use rcr_editor_core::{
        Edit, Editor, GpuPlaybackConfig, GpuPlaybackOutput, GpuPlaybackStatus, PlaybackOutput,
        PlaybackSession, Project, SourceStamp, TrackKind,
    };
    use serde_json::json;
    use std::{
        fs::{File, OpenOptions},
        io::{Read, Write},
        path::{Path, PathBuf},
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };
    use windows::{
        core::Interface,
        Win32::{
            Foundation::HMODULE,
            Graphics::{
                Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0},
                Direct3D11::*,
                Dxgi::{
                    CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1, IDXGIKeyedMutex, IDXGIResource,
                },
            },
        },
    };

    struct Consumer {
        context: ID3D11DeviceContext,
        source: ID3D11Texture2D,
        private: ID3D11Texture2D,
        mutex: IDXGIKeyedMutex,
        counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
        last_id: u64,
        copies: u64,
    }
    impl Consumer {
        fn new(output: GpuPlaybackOutput, luid: (u32, i32)) -> Result<Self> {
            unsafe {
                let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
                let mut selected = None;
                for index in 0..32 {
                    let Ok(adapter) = factory.EnumAdapters1(index) else {
                        break;
                    };
                    let desc = adapter.GetDesc1()?;
                    if (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart) == luid {
                        selected = Some(adapter.cast::<IDXGIAdapter>()?);
                        break;
                    }
                }
                let adapter = selected.context("Requested consumer adapter unavailable")?;
                let (mut device, mut context) = (None, None);
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                    Some(&[D3D_FEATURE_LEVEL_11_0]),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )?;
                let device = device.context("Missing consumer device")?;
                let handle = output.texture.cast::<IDXGIResource>()?.GetSharedHandle()?;
                // Legacy shared handle is borrowed, never CloseHandle'd.
                let mut source: Option<ID3D11Texture2D> = None;
                device.OpenSharedResource(handle, &mut source)?;
                let source = source.context("Missing consumer import")?;
                let mutex = source.cast::<IDXGIKeyedMutex>()?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                source.GetDesc(&mut desc);
                desc.MiscFlags = 0;
                desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
                let mut private = None;
                device.CreateTexture2D(&desc, None, Some(&mut private))?;
                Ok(Self {
                    context: context.context("Missing consumer context")?,
                    source,
                    private: private.context("Missing private GPU copy")?,
                    mutex,
                    counter: output.published_frame,
                    last_id: 0,
                    copies: 0,
                })
            }
        }
        fn consume(&mut self, session: &PlaybackSession) -> Result<()> {
            unsafe {
                let status =
                    (Interface::vtable(&self.mutex).AcquireSync)(self.mutex.as_raw(), 1, 0);
                if status.0 == 0x102 {
                    return Ok(());
                }
                ensure!(status.0 == 0, "Consumer acquire failed: {status:?}");
                let id = self.counter.load(Ordering::Acquire);
                self.context.CopyResource(&self.private, &self.source);
                self.context.Flush();
                self.mutex.ReleaseSync(0)?;
                ensure!(
                    id > self.last_id,
                    "Published frame ID did not increase: {id} after {}",
                    self.last_id
                );
                ensure!(
                    id <= session.position() + 1,
                    "Published future frame {id} at transport position {}",
                    session.position()
                );
                self.last_id = id;
                self.copies += 1;
            }
            Ok(())
        }
    }
    fn tick(
        session: &PlaybackSession,
        consumer: &mut Option<Consumer>,
        luid: (u32, i32),
    ) -> Result<()> {
        if let Some(error) = session.errors.try_iter().next() {
            anyhow::bail!("Transport error: {error}");
        }
        ensure!(
            !matches!(
                session.gpu_status(),
                Some(GpuPlaybackStatus::Fallback(_) | GpuPlaybackStatus::Failed(_))
            ),
            "Unexpected GPU status: {:?}",
            session.gpu_status()
        );
        if consumer.is_none() {
            if let Some(output) = session.gpu_output() {
                *consumer = Some(Consumer::new(output, luid)?);
            }
        }
        if let Some(consumer) = consumer {
            consumer.consume(session)?;
        }
        let stats = session.gpu_stats().context("Missing GPU stats")?;
        ensure!(
            stats.max_future_frames <= 3 && stats.max_inflight_reads <= 3,
            "GPU retention exceeded budget"
        );
        ensure!(
            stats.last_published_id == 0
                || stats.last_published_id <= stats.last_publish_position + 1,
            "Worker published ahead of audio clock"
        );
        std::thread::sleep(Duration::from_millis(1));
        Ok(())
    }
    fn until(
        session: &PlaybackSession,
        consumer: &mut Option<Consumer>,
        luid: (u32, i32),
        predicate: impl Fn() -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !predicate() {
            ensure!(
                Instant::now() < deadline,
                "Transport proof stage timed out: {:?}",
                session.gpu_status()
            );
            tick(session, consumer, luid)?;
        }
        Ok(())
    }
    fn fixture_project(source: &Path) -> Result<Project> {
        let asset = rcr_editor_core::probe(source)?;
        ensure!(
            asset
                .streams
                .iter()
                .any(|stream| stream.kind == TrackKind::Audio),
            "Fixture needs an audio stream to exercise the existing audio clock"
        );
        let mut editor = Editor::default();
        editor.execute(Edit::Sequence(rcr_editor_core::QuickEdit::Add(asset)))?;
        let mut project = editor.project;
        // Even an audible tone fixture stays silent in optional WASAPI mode.
        for track in &mut project.tracks {
            if track.kind == TrackKind::Audio {
                track.muted = true;
            }
        }
        ensure!(
            project.duration() >= 90,
            "Fixture needs at least 90 project frames"
        );
        Ok(project)
    }
    fn source_change_proof(
        source: &Path,
        config: GpuPlaybackConfig,
        mode: PlaybackOutput,
    ) -> Result<()> {
        const MAX_COPY: u64 = 32 * 1024 * 1024;
        let original = SourceStamp::read(source)?;
        ensure!(
            original.bytes > 0 && original.bytes <= MAX_COPY,
            "Source-change fixture copy must be within 1byte..=32MiB"
        );
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let downloads = workspace.join(".downloads");
        std::fs::create_dir_all(&downloads)?;
        let downloads = downloads.canonicalize()?;
        ensure!(
            downloads.starts_with(&workspace),
            "Fixture directory escapes the workspace"
        );
        let owned = tempfile::Builder::new()
            .prefix("gpu-source-change-")
            .tempdir_in(downloads)?;
        let path = owned.path().join("owned-source.mkv");
        {
            let input = File::open(source)?;
            let mut copy = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            let count = std::io::copy(&mut input.take(MAX_COPY + 1), &mut copy)?;
            ensure!(
                count == original.bytes && count <= MAX_COPY,
                "Source changed or exceeded the bounded copy size"
            );
            copy.sync_all()?;
        }
        ensure!(
            SourceStamp::read(source)? == original,
            "Original input changed during fixture preparation"
        );
        let before = SourceStamp::read(&path)?;
        let mut project = fixture_project(&path)?;
        // This guard test needs an exact zero-subprocess invariant. Audio timing
        // is covered by the normal mode; remove only this copy's in-memory audio
        // clips/tracks so an audio FFmpeg child cannot obscure a forbidden fallback.
        project
            .tracks
            .retain(|track| track.kind == TrackKind::Video);
        project
            .clips
            .retain(|clip| project.tracks.iter().any(|track| track.id == clip.track));
        let luid = config.adapter_luid;
        let session = PlaybackSession::new_with_gpu(
            project.clone(),
            0,
            (project.width, project.height),
            mode,
            config,
        )?;
        let mut consumer = None;
        // Always join before removing our temporary copy, including assertion errors.
        let result = (|| -> Result<serde_json::Value> {
            until(&session, &mut consumer, luid, || session.ready())?;
            tick(&session, &mut consumer, luid)?;
            ensure!(
                consumer.as_ref().is_some_and(|c| c.last_id == 1) && session.position() == 0,
                "Source-change test must hold the first frame paused"
            );
            let publications = session.gpu_stats().context("Missing GPU stats")?.published;
            {
                let mut changed = OpenOptions::new().append(true).open(&path)?;
                changed.write_all(&[0])?;
                changed.sync_all()?;
            }
            let after = SourceStamp::read(&path)?;
            ensure!(
                after.bytes == before.bytes + 1 && after != before,
                "Owned-copy mutation did not change its source stamp"
            );
            let disable_at = Instant::now();
            session.disable_gpu();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut terminal = None;
            loop {
                ensure!(
                    Instant::now() < deadline,
                    "Changed-source terminal error timed out: {:?}",
                    session.gpu_status()
                );
                ensure!(
                    session.frame().is_none(),
                    "Changed source produced a CPU fallback frame"
                );
                ensure!(
                    session.decoder_processes() == 0,
                    "Changed source started a CPU video decoder"
                );
                ensure!(
                    !matches!(session.gpu_status(), Some(GpuPlaybackStatus::Fallback(_))),
                    "Changed source was routed into CPU fallback"
                );
                if terminal.is_none() {
                    terminal = session.errors.try_iter().next();
                }
                if terminal.is_some()
                    && matches!(session.gpu_status(), Some(GpuPlaybackStatus::Failed(_)))
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let error = terminal.context("Missing terminal error")?;
            ensure!(
                error.to_lowercase().contains("source") && error.to_lowercase().contains("changed"),
                "Terminal error is not source-specific: {error}"
            );
            let stats = session.gpu_stats().unwrap();
            ensure!(
                stats.published == publications
                    && session.position() == 0
                    && session.frame().is_none()
                    && session.decoder_processes() == 0,
                "Playback continued after the source changed"
            );
            Ok(
                json!({"proof":"gpu_source_change_blocks_fallback","original_input":source,"mutated_owned_copy":path,"owned_copy_bytes_before":before.bytes,"owned_copy_bytes_after":after.bytes,"terminal_error":error,"terminal_status":format!("{:?}",session.gpu_status()),"disable_to_terminal_ms":disable_at.elapsed().as_secs_f64()*1000.,"held_publications":publications,"cpu_fallback_frames":0,"cpu_video_decoder_processes":0,"audio_output":"not opened; video-only guard test","input_writes":false,"owned_copy_appended_bytes":1,"gpu_video_readbacks":0}),
            )
        })();
        drop(consumer);
        let close = Instant::now();
        session.shutdown();
        let shutdown_ms = close.elapsed().as_secs_f64() * 1000.;
        let original_unchanged = SourceStamp::read(source)? == original;
        let cleanup = owned.close();
        ensure!(
            original_unchanged,
            "Original input changed during source-change proof"
        );
        cleanup.context(
            "Could not remove exclusively owned source-change fixture directory after shutdown",
        )?;
        let mut report = result?;
        report["original_input_unchanged"] = json!(true);
        report["owned_fixture_removed"] = json!(true);
        report["shutdown_ms"] = json!(shutdown_ms);
        println!("{}", serde_json::to_string_pretty(&report)?);
        Ok(())
    }
    fn until_eof_proof(
        source: &Path,
        config: GpuPlaybackConfig,
        mode: PlaybackOutput,
    ) -> Result<()> {
        let started = Instant::now();
        let original = SourceStamp::read(source)?;
        let project = fixture_project(source)?;
        let duration = project.duration();
        let seconds = project.seconds(duration);
        ensure!(
            (300..=900).contains(&duration)
                && seconds <= 15.
                && project
                    .assets
                    .iter()
                    .all(|asset| asset.duration_ns <= 15_000_000_000),
            "EOF fixture must contain 300..=900 project frames and last at most15seconds"
        );
        let budget = Duration::from_secs_f64(seconds + 5.);
        let deadline = started + budget;
        let luid = config.adapter_luid;
        let session = PlaybackSession::new_with_gpu(
            project.clone(),
            0,
            (project.width, project.height),
            mode,
            config,
        )?;
        let mut consumer = None;
        // Always join on an assertion/timeout, not merely on successful EOF.
        let result = (|| -> Result<serde_json::Value> {
            while !session.ready() {
                ensure!(
                    Instant::now() < deadline,
                    "EOF startup exceeded fixture-duration+5sec budget: {:?}",
                    session.gpu_status()
                );
                ensure!(
                    session.frame().is_none(),
                    "EOF startup used a CPU video frame"
                );
                tick(&session, &mut consumer, luid)?;
            }
            tick(&session, &mut consumer, luid)?;
            ensure!(
                consumer.as_ref().is_some_and(|c| c.last_id == 1),
                "EOF proof did not begin with paused frame0"
            );
            session.play();
            let mut previous_position = 0;
            while !session.finished() {
                ensure!(Instant::now()<deadline,"Transport did not reach EOF within fixture-duration+5sec budget: position{} of{duration}, status{:?}",session.position(),session.gpu_status());
                ensure!(
                    session.frame().is_none(),
                    "EOF playback produced a CPU fallback frame"
                );
                tick(&session, &mut consumer, luid)?;
                let position = session.position();
                ensure!(
                    position >= previous_position && position <= duration,
                    "EOF audio clock regressed or exceeded project duration"
                );
                previous_position = position;
            }
            // finished() uses the end boundary (duration), while the final image
            // is frame duration-1 with its nonzero identity equal to duration.
            // Consume a still-pending final key1 publication before asserting it.
            tick(&session, &mut consumer, luid)?;
            let stats = session.gpu_stats().context("Missing EOF GPU stats")?;
            let consumed = consumer.as_ref().context("Missing EOF consumer")?;
            ensure!(
                session.position() == duration
                    && matches!(session.gpu_status(), Some(GpuPlaybackStatus::Finished)),
                "EOF transport stopped before its exact end boundary"
            );
            ensure!(
                (240..=900).contains(&stats.decoded),
                "Transport did not exceed the120-frame decoder-proof lifetime: {}decoded",
                stats.decoded
            );
            ensure!(
                stats.last_published_id == duration && consumed.last_id == duration,
                "Final EOF frame was missing: published{}, consumed{}, expectedID{duration}",
                stats.last_published_id,
                consumed.last_id
            );
            ensure!(
                (18..=32).contains(&stats.pool_size)
                    && stats.max_future_frames <= 3
                    && stats.max_inflight_reads <= 3,
                "EOF GPU pool/retention budget violated"
            );
            let output = session
                .gpu_output()
                .context("Final GPU image disappeared at EOF")?;
            let final_texture = output.texture.as_raw();
            let final_copies = consumed.copies;
            session.pause();
            let hold_until = Instant::now() + Duration::from_millis(100);
            while Instant::now() < hold_until {
                ensure!(
                    Instant::now() < deadline,
                    "EOF image hold exceeded fixture-duration+5sec budget"
                );
                ensure!(session.frame().is_none(), "EOF hold produced a CPU frame");
                tick(&session, &mut consumer, luid)?;
                let held = session.gpu_stats().unwrap();
                let held_output = session.gpu_output().context("Held GPU image disappeared")?;
                ensure!(
                    session.finished()
                        && session.position() == duration
                        && held.decoded == stats.decoded
                        && held.published == stats.published
                        && held.last_published_id == duration,
                    "EOF pause changed clock/decoder/publication"
                );
                ensure!(
                    held_output.texture.as_raw() == final_texture
                        && std::sync::Arc::ptr_eq(
                            &held_output.published_frame,
                            &output.published_frame
                        )
                        && held_output.published_frame.load(Ordering::Acquire) == duration,
                    "EOF held image identity changed"
                );
                ensure!(
                    consumer
                        .as_ref()
                        .is_some_and(|c| c.last_id == duration && c.copies == final_copies),
                    "EOF consumer copied a replacement for its final private image"
                );
            }
            Ok(
                json!({"proof":"gpu_transport_until_eof","source":source,"fixture_frames":duration,"fixture_seconds":seconds,"duration_plus_startup_budget_ms":budget.as_secs_f64()*1000.,"until_eof_and_hold_ms":started.elapsed().as_secs_f64()*1000.,"decoded_frames":stats.decoded,"published_frames":stats.published,"consumer_copies":final_copies,"final_project_position":session.position(),"final_image_frame":duration-1,"final_published_id":stats.last_published_id,"final_consumed_id":consumer.as_ref().unwrap().last_id,"held_final_image_identity_100ms":true,"max_future_frames":stats.max_future_frames,"max_inflight_reads":stats.max_inflight_reads,"gpu_busy_retries":stats.busy_skips,"gpu_busy_release_notifications":stats.busy_release_notifications,"gpu_queue_full_retries":stats.queue_full_skips,"gpu_busy_wait_ms":stats.busy_wait_ns as f64/1e6,"gpu_queue_full_wait_ms":stats.queue_full_wait_ns as f64/1e6,"gpu_clock_wait_ms":stats.clock_wait_ns as f64/1e6,"hardware_pool_size":stats.pool_size,"audio_output":if matches!(mode,PlaybackOutput::Device){"muted WASAPI"}else{"silent existing audio clock"},"all_audio_tracks_muted":true,"cpu_fallback_frames":0,"gpu_video_readbacks":0,"input_writes":false,"desktop_integration":false}),
            )
        })();
        drop(consumer);
        let close = Instant::now();
        session.shutdown();
        let shutdown_ms = close.elapsed().as_secs_f64() * 1000.;
        ensure!(
            SourceStamp::read(source)? == original,
            "EOF proof input changed"
        );
        let mut report = result?;
        report["shutdown_ms"] = json!(shutdown_ms);
        report["original_input_unchanged"] = json!(true);
        println!("{}", serde_json::to_string_pretty(&report)?);
        Ok(())
    }
    pub fn run() -> Result<()> {
        let mut args = std::env::args().skip(1);
        let (mut input, mut low, mut high, mut dlls, mut device) =
            (None, None, None, PathBuf::from("C:/bin"), false);
        let mut source_change = false;
        let mut until_eof = false;
        while let Some(arg) = args.next() {
            match arg.as_str(){
            "--input"=>input=Some(PathBuf::from(args.next().context("Missing input")?)),
            "--dll-dir"=>dlls=PathBuf::from(args.next().context("Missing DLL directory")?),
            "--adapter-low"=>low=Some(args.next().context("Missing adapter low")?.parse::<u32>()?),
            "--adapter-high"=>high=Some(args.next().context("Missing adapter high")?.parse::<i32>()?),
            "--wasapi"=>device=true,
            "--source-change-proof"=>source_change=true,
            "--until-eof"=>until_eof=true,
            _=>anyhow::bail!("Use --input <owned AV1 fixture> --adapter-low N --adapter-high N [--dll-dir C:/bin] [--wasapi] [--source-change-proof | --until-eof]"),
        }
            ensure!(
                !(source_change && until_eof),
                "--source-change-proof and --until-eof are separate fixture modes"
            );
        }
        let source = input.context("--input required")?.canonicalize()?;
        let luid = (
            low.context("--adapter-low required")?,
            high.context("--adapter-high required")?,
        );
        let stamp = SourceStamp::read(&source)?;
        let config = GpuPlaybackConfig {
            adapter_luid: luid,
            dll_directory: dlls,
            wake: None,
        };
        let mode = if device {
            PlaybackOutput::Device
        } else {
            PlaybackOutput::Silent
        };
        if source_change {
            return source_change_proof(&source, config, mode);
        }
        if until_eof {
            return until_eof_proof(&source, config, mode);
        }
        let project = fixture_project(&source)?;
        let size = (project.width, project.height);
        let session =
            PlaybackSession::new_with_gpu(project.clone(), 0, size, mode, config.clone())?;
        let mut consumer = None;
        until(&session, &mut consumer, luid, || session.ready())?;
        // Ready can precede our first consumer copy by one iteration.
        tick(&session, &mut consumer, luid)?;
        ensure!(
            consumer.as_ref().is_some_and(|c| c.last_id == 1),
            "Initial paused frame should have ID1"
        );
        session.play();
        until(&session, &mut consumer, luid, || session.position() >= 12)?;
        session.pause();
        let settle = Instant::now() + Duration::from_millis(50);
        while Instant::now() < settle {
            tick(&session, &mut consumer, luid)?;
        }
        let paused = session.position();
        let held = session.gpu_stats().unwrap();
        let hold = Instant::now() + Duration::from_millis(100);
        while Instant::now() < hold {
            tick(&session, &mut consumer, luid)?;
        }
        let after = session.gpu_stats().unwrap();
        ensure!(
            session.position() == paused
                && held.decoded == after.decoded
                && held.published == after.published,
            "Pause advanced clock/decoder/publication"
        );
        session.play();
        until(&session, &mut consumer, luid, || {
            session.position() >= paused + 6
        })?;
        let first_stats = session.gpu_stats().unwrap();
        ensure!(
            (18..=32).contains(&first_stats.pool_size),
            "Unexpected hardware pool size"
        );
        let first_copies = consumer.as_ref().unwrap().copies;
        drop(consumer);
        session.shutdown();

        // New session seeks the same source at a nonzero source-in plus playhead.
        // The original project is unchanged and there are no project/sidecar writes.
        let mut trimmed = project.clone();
        for clip in &mut trimmed.clips {
            ensure!(
                clip.duration > 30,
                "Every fixture stream must exceed the 500ms source-in trim"
            );
            clip.source_in_ns += 500_000_000;
            clip.duration -= 30;
        }
        let session = PlaybackSession::new_with_gpu(trimmed, 6, size, mode, config)?;
        let mut consumer = None;
        until(&session, &mut consumer, luid, || session.ready())?;
        tick(&session, &mut consumer, luid)?;
        ensure!(
            consumer.as_ref().is_some_and(|c| c.last_id == 7),
            "Trimmed seek did not publish requested project frame6"
        );
        let seek = session.gpu_stats().unwrap();
        ensure!(
            seek.seek_target_ns == 600_000_000
                && seek.first_published_id == 7
                && seek
                    .first_source_ns
                    .is_some_and(|ns| ns.abs_diff(600_000_000) <= 1_000_000),
            "Trimmed seek source PTS did not match its source-in/playhead target"
        );
        session.play();
        until(&session, &mut consumer, luid, || session.position() >= 14)?;
        let before_fallback = session.position();
        session.disable_gpu();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut cpu_frame = None;
        while cpu_frame.is_none() {
            ensure!(
                Instant::now() < deadline,
                "CPU fallback did not produce a frame: {:?}",
                session.gpu_status()
            );
            if let Some(error) = session.errors.try_iter().next() {
                anyhow::bail!("Fallback error: {error}");
            }
            if let Some(consumer) = &mut consumer {
                consumer.consume(&session)?;
            }
            cpu_frame = session.frame();
            std::thread::sleep(Duration::from_millis(1));
        }
        ensure!(
            session.position() >= before_fallback
                && cpu_frame.as_ref().unwrap().frame >= before_fallback,
            "CPU fallback reset audio/video timeline"
        );
        ensure!(
            session.gpu_output().is_none()
                && matches!(session.gpu_status(), Some(GpuPlaybackStatus::Fallback(_))),
            "Fallback did not replace GPU output with CPU frame"
        );
        let second_stats = session.gpu_stats().unwrap();
        drop(consumer);
        let close = Instant::now();
        session.shutdown();
        let shutdown_ms = close.elapsed().as_secs_f64() * 1000.;
        ensure!(SourceStamp::read(&source)? == stamp, "Proof source changed");
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"proof":"opt_in_gpu_audio_clock_transport","source":source,"adapter_luid":[luid.0,luid.1],"audio_output":if device{"muted WASAPI"}else{"silent existing audio clock"},"all_audio_tracks_muted":true,"pause_held":true,"resume":true,"trim_source_in_ns":500_000_000u64,"seek_project_frame":6,"seek_actual_source_ns":seek.first_source_ns,"seek_preroll_frames":seek.preroll_frames,"fallback_preserved_clock":true,"first_consumer_copies":first_copies,"first_retention":{"future":first_stats.max_future_frames,"reads":first_stats.max_inflight_reads,"pool":first_stats.pool_size},"second_retention":{"future":second_stats.max_future_frames,"reads":second_stats.max_inflight_reads,"pool":second_stats.pool_size},"shutdown_ms":shutdown_ms,"gpu_video_readbacks":0,"source_writes":false,"desktop_integration":false})
            )?
        );
        Ok(())
    }
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    proof::run()
}
#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("Windows D3D11 transport proof only")
}
