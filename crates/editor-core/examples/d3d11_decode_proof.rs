//! Explicit AV1 D3D11VA proof: no capture, audio, UI, source writes or CPU video download.
#[cfg(windows)]
#[path = "../src/playback_gpu/convert.rs"]
#[allow(dead_code)] // This proof uses tracked publication, not the standalone untracked APIs.
mod convert;
#[cfg(windows)]
#[path = "../src/playback_gpu/decode.rs"]
#[allow(dead_code)] // Isolated decode proof does not use the optional transport APIs.
mod decode;
#[cfg(windows)]
#[path = "../src/ffmpeg_runtime.rs"]
#[allow(dead_code)]
mod ffmpeg_runtime;

#[cfg(windows)]
mod proof {
    use super::{
        convert::{
            ConvertConfig, Nv12Converter, PublicationDiagnostics, QueryDiagnostics, ReadTicket,
            Skip, TrackedPublish,
        },
        decode::{self, DecodeStopped, DecodedFrame, Decoder, FrameInfo, StopReason},
    };
    use anyhow::{ensure, Context, Result};
    use serde_json::{json, Value};
    use std::{
        collections::VecDeque,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };
    use windows::{
        core::{IUnknown, Interface, HRESULT},
        Win32::Graphics::{
            Direct3D11::*,
            Dxgi::{IDXGIKeyedMutex, IDXGIResource},
        },
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ConsumerMode {
        None,
        Same,
        Separate,
    }
    impl ConsumerMode {
        fn name(self) -> &'static str {
            match self {
                Self::None => "none",
                Self::Same => "same",
                Self::Separate => "separate",
            }
        }
    }

    pub struct Options {
        source: PathBuf,
        dlls: PathBuf,
        frames: usize,
        luid: Option<(u32, i32)>,
        convert: bool,
        consumer: ConsumerMode,
        consumer_copy: bool,
        producer_flush_after_release: bool,
        consumer_flush_after_release: bool,
        query: QueryDiagnostics,
    }
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Options> {
        let mut args = args.into_iter();
        let (mut source, mut dlls, mut frames, mut low, mut high, mut convert) =
            (None, PathBuf::from("C:/bin"), 120, None, None, false);
        let (
            mut consumer,
            mut consumer_copy,
            mut producer_flush_after_release,
            mut consumer_flush_after_release,
            mut query,
        ) = (
            ConsumerMode::Same,
            false,
            false,
            false,
            QueryDiagnostics::default(),
        );
        let mut explicit_query = false;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--input"=>{ensure!(source.is_none(),"Repeated --input");source=Some(PathBuf::from(args.next().context("Missing --input path")?));}
                "--dll-dir"=>dlls=PathBuf::from(args.next().context("Missing --dll-dir")?),
                "--frames"=>frames=args.next().context("Missing --frames")?.parse()?,
                "--adapter-low"=>low=Some(args.next().context("Missing adapter low LUID")?.parse::<u32>()?),
                "--adapter-high"=>high=Some(args.next().context("Missing adapter high LUID")?.parse::<i32>()?),
                "--convert"=>convert=true,
                "--consumer"=>consumer=match args.next().context("Missing --consumer mode")?.as_str(){"none"=>ConsumerMode::None,"same"=>ConsumerMode::Same,"separate"=>ConsumerMode::Separate,_=>anyhow::bail!("Consumer mode must be none, same or separate")},
                "--consumer-copy"=>consumer_copy=true,
                "--producer-flush-after-release"=>producer_flush_after_release=true,
                "--consumer-flush-after-release"=>consumer_flush_after_release=true,
                "--query-kick-after-polls"=>{query.kick_after_polls=args.next().context("Missing query kick count")?.parse()?;explicit_query=true;},
                "--query-bool-data"=>{query.read_bool=true;explicit_query=true;},
                "--query-status-only"=>{query.read_bool=false;explicit_query=true;},
                _=>anyhow::bail!("Unknown option {arg}; use --input <owned AV1 file> [--dll-dir <absolute folder>] [--frames 2..120] [--adapter-low N --adapter-high N] [--convert]"),
            }
        }
        ensure!(
            (2..=decode::MAX_FRAMES).contains(&frames),
            "--frames must be 2..=120 including cancellation prefix"
        );
        ensure!(
            query.kick_after_polls <= 1000,
            "Query kick count must be 0..=1000 (zero disables)"
        );
        ensure!(
            consumer != ConsumerMode::None || (!consumer_copy && !consumer_flush_after_release),
            "No-consumer control cannot copy or flush a consumer"
        );
        ensure!(
            convert
                || (consumer == ConsumerMode::Same
                    && !consumer_copy
                    && !producer_flush_after_release
                    && !consumer_flush_after_release
                    && !explicit_query),
            "Query/consumer diagnostics require --convert"
        );
        ensure!(dlls.is_absolute(), "--dll-dir must be absolute");
        ensure!(
            low.is_some() == high.is_some(),
            "Supply both adapter LUID halves"
        );
        Ok(Options {
            source: source.context("--input is required")?,
            dlls,
            frames,
            luid: low.zip(high),
            convert,
            consumer,
            consumer_copy,
            producer_flush_after_release,
            consumer_flush_after_release,
            query,
        })
    }

    struct Consumer {
        _device: ID3D11Device,
        context: ID3D11DeviceContext,
        source: ID3D11Texture2D,
        mutex: IDXGIKeyedMutex,
        private: Option<ID3D11Texture2D>,
        separate: bool,
    }
    impl Consumer {
        fn new(
            decoder: &Decoder,
            converter: &Nv12Converter,
            mode: ConsumerMode,
            copy: bool,
            adapter: &decode::AdapterInfo,
        ) -> Result<Self> {
            ensure!(
                mode != ConsumerMode::None,
                "No-consumer control has no consumer device"
            );
            let output = converter.output();
            let (device, context, source) = if mode == ConsumerMode::Separate {
                let (device, context, actual) =
                    decode::create_device(Some((adapter.luid_low, adapter.luid_high)))?;
                ensure!(
                    (actual.luid_low, actual.luid_high) == (adapter.luid_low, adapter.luid_high),
                    "Consumer adapter differs from producer"
                );
                ensure!(
                    device.cast::<IUnknown>()? != decoder.device.cast::<IUnknown>()?,
                    "Separate-device control reused producer device identity"
                );
                // Legacy shared handles are borrowed and must not be CloseHandle'd.
                let handle = unsafe { output.texture.cast::<IDXGIResource>()?.GetSharedHandle()? };
                let mut imported: Option<ID3D11Texture2D> = None;
                unsafe {
                    device.OpenSharedResource(handle, &mut imported)?;
                }
                (
                    device,
                    context,
                    imported.context("Missing imported shared output")?,
                )
            } else {
                (
                    decoder.device.clone(),
                    decoder.context.clone(),
                    output.texture,
                )
            };
            let mutex = source.cast::<IDXGIKeyedMutex>()?;
            let private = if copy {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe {
                    source.GetDesc(&mut desc);
                }
                desc.MiscFlags = 0;
                desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
                let mut texture = None;
                unsafe {
                    device.CreateTexture2D(&desc, None, Some(&mut texture))?;
                }
                Some(texture.context("Missing private consumer copy")?)
            } else {
                None
            };
            Ok(Self {
                _device: device,
                context,
                source,
                mutex,
                private,
                separate: mode == ConsumerMode::Separate,
            })
        }
        /// Single nonblocking attempt. Every successful acquire releases key 0.
        fn consume(&self, flush_after_release: bool) -> Result<HRESULT> {
            unsafe {
                let status =
                    (Interface::vtable(&self.mutex).AcquireSync)(self.mutex.as_raw(), 1, 0);
                if status.0 != 0 {
                    return Ok(status);
                }
                if let Some(private) = &self.private {
                    self.context.CopyResource(private, &self.source);
                    self.context.Flush(); // Copy variant matches the consumer update contract.
                }
                let released = self.mutex.ReleaseSync(0);
                if flush_after_release {
                    self.context.Flush();
                }
                released?;
                Ok(status)
            }
        }
    }

    fn milliseconds(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1000.
    }
    #[derive(Default)]
    struct Samples(Vec<f64>);
    impl Samples {
        fn push(&mut self, duration: Duration) {
            self.0.push(milliseconds(duration));
        }
        fn report(&self) -> Value {
            if self.0.is_empty() {
                return json!({"count":0,"sum_ms":0.,"median_ms":null,"p95_ms":null,"max_ms":null});
            }
            let mut sorted = self.0.clone();
            sorted.sort_by(f64::total_cmp);
            let n = sorted.len();
            let median = if n.is_multiple_of(2) {
                (sorted[n / 2 - 1] + sorted[n / 2]) / 2.
            } else {
                sorted[n / 2]
            };
            json!({"count":n,"sum_ms":sorted.iter().sum::<f64>(),"median_ms":median,
                "p95_ms":sorted[(n*95).div_ceil(100)-1],"max_ms":sorted[n-1]})
        }
    }
    #[derive(Default)]
    struct Measurements {
        decode_prefix: Samples,
        decode_reopened: Samples,
        publish: Samples,
        retirement: Samples,
        output_acquire: Samples,
        poll_calls: u64,
        poll_cpu_ms: f64,
        poll_max_ms: f64,
        publish_attempts: u64,
        output_acquire_retries: u64,
        backpressure_retries: u64,
        sleep_calls: u64,
        sleep_ms: f64,
        query: QueryDiagnostics,
    }
    impl Measurements {
        fn wait(&mut self) {
            let at = Instant::now();
            std::thread::sleep(Duration::from_millis(1));
            self.sleep_calls += 1;
            self.sleep_ms += milliseconds(at.elapsed());
        }
        fn report(&self) -> Value {
            json!({"decode_prefix":self.decode_prefix.report(),"decode_reopened":self.decode_reopened.report(),
                "publish_call_work_per_frame":self.publish.report(),"input_retirement_observed_latency":self.retirement.report(),
                "output_acquire_wall_per_frame":self.output_acquire.report(),
                "query_poll":{"calls":self.poll_calls,"sum_call_wall_ms":self.poll_cpu_ms,"max_call_wall_ms":self.poll_max_ms},
                "publish_attempts":self.publish_attempts,"output_acquire_retries":self.output_acquire_retries,
                "backpressure_retries":self.backpressure_retries,"sleep_calls":self.sleep_calls,"actual_sleep_ms":self.sleep_ms,
                "interpretation":"Host wall timings, not GPU timestamps. Retirement latency starts before publish and ends at an observed completed query, so includes intervening host work. Output acquire includes polling/sleep. These metrics overlap; do not sum them as total execution time."})
        }
    }
    fn retire(
        decoder: &Decoder,
        converter: &mut Nv12Converter,
        pending: &mut VecDeque<(ReadTicket, DecodedFrame, Instant)>,
        measurements: &mut Measurements,
    ) -> Result<()> {
        loop {
            let at = Instant::now();
            let result = decoder
                .with_context_lock(|| converter.poll_completed_diagnostic(measurements.query));
            let elapsed = milliseconds(at.elapsed());
            measurements.poll_calls += 1;
            measurements.poll_cpu_ms += elapsed;
            measurements.poll_max_ms = measurements.poll_max_ms.max(elapsed);
            let Some(ticket) = result? else {
                break;
            };
            let expected = pending
                .front()
                .context("Completed GPU read has no retained AVFrame")?;
            ensure!(
                expected.0 == ticket,
                "GPU completion returned a different frame ticket"
            );
            let (_, frame, submitted_at) = pending
                .pop_front()
                .expect("matching retained frame checked");
            measurements.retirement.push(submitted_at.elapsed());
            drop(frame); // Only S_OK-completed input reads release pool ownership.
        }
        Ok(())
    }
    fn drain(
        decoder: &Decoder,
        converter: &mut Nv12Converter,
        pending: &mut VecDeque<(ReadTicket, DecodedFrame, Instant)>,
        measurements: &mut Measurements,
    ) -> Result<()> {
        decoder.with_context_lock(|| converter.begin_shutdown());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !pending.is_empty() {
            retire(decoder, converter, pending, measurements)?;
            ensure!(
                Instant::now() < deadline,
                "GPU input completion did not finish within proof shutdown deadline"
            );
            if !pending.is_empty() {
                measurements.wait();
            }
        }
        ensure!(
            converter.pending_reads() == 0,
            "Converter kept a completion without input ownership"
        );
        Ok(())
    }
    fn same_geometry(a: &FrameInfo, b: &FrameInfo) -> bool {
        a.coded_size == b.coded_size && a.visible_size == b.visible_size && a.crop == b.crop
    }
    pub fn run(options: Options) -> Result<Value> {
        let started = Instant::now();
        let stage = Instant::now();
        let (device, context, adapter) = decode::create_device(options.luid)?;
        let device_ms = milliseconds(stage.elapsed());
        let mut measurements = Measurements {
            query: options.query,
            ..Default::default()
        };
        // The first short context deliberately cancels. No converter reads are
        // submitted here, so its frames can be released immediately.
        let cancelled_prefix = (options.frames / 2).clamp(1, 3);
        let cancel = Arc::new(AtomicBool::new(false));
        let stage = Instant::now();
        let mut decoder = Decoder::open(
            &options.source,
            &options.dlls,
            device.clone(),
            context.clone(),
            cancel.clone(),
            Duration::from_secs(15),
        )?;
        let open_prefix_ms = milliseconds(stage.elapsed());
        let open_prefix_breakdown = decoder.open_timings.clone();
        let source = decoder.source.clone();
        let stage = Instant::now();
        for _ in 0..cancelled_prefix {
            let at = Instant::now();
            let frame = decoder
                .next()?
                .context("Source ended during cancellation prefix")?;
            measurements.decode_prefix.push(at.elapsed());
            drop(frame);
        }
        cancel.store(true, Ordering::Release);
        let error = match decoder.next() {
            Err(error) => error,
            Ok(_) => anyhow::bail!("Cancelled decoder accepted another frame"),
        };
        ensure!(
            error
                .downcast_ref::<DecodeStopped>()
                .is_some_and(|e| e.0 == StopReason::Cancelled),
            "Cancellation did not report its typed reason: {error:#}"
        );
        let decode_prefix_ms = milliseconds(stage.elapsed());
        let stage = Instant::now();
        drop(decoder); // All codec/demux/hardware refs close before reopening.
        let close_prefix_ms = milliseconds(stage.elapsed());

        let cancel = Arc::new(AtomicBool::new(false));
        let stage = Instant::now();
        let mut decoder = Decoder::open(
            &source,
            &options.dlls,
            device,
            context,
            cancel.clone(),
            Duration::from_secs(15),
        )?;
        let reopen_ms = milliseconds(stage.elapsed());
        let reopen_breakdown = decoder.open_timings.clone();
        let mut converter_init_ms = 0.;
        let mut consumer_init_ms = 0.;
        let mut converter: Option<Nv12Converter> = None;
        let mut consumer: Option<Consumer> = None;
        let mut pending = VecDeque::new();
        let mut first: Option<FrameInfo> = None;
        let mut last = None;
        let mut converted = 0usize;
        let mut decoded = 0usize;
        let stage = Instant::now();
        let run = (|| -> Result<()> {
            for _ in cancelled_prefix..options.frames {
                if let Some(converter) = &mut converter {
                    retire(&decoder, converter, &mut pending, &mut measurements)?;
                }
                let at = Instant::now();
                let frame = decoder
                    .next()?
                    .context("Source ended before requested bounded proof frame count")?;
                measurements.decode_reopened.push(at.elapsed());
                if let Some(first) = &first {
                    ensure!(
                        same_geometry(first, &frame.info),
                        "Source geometry changed during proof"
                    );
                } else {
                    first = Some(frame.info.clone());
                }
                last = Some(frame.info.clone());
                decoded += 1;
                if !options.convert {
                    continue;
                }
                frame.info.require_bt709()?;
                if converter.is_none() {
                    let config = ConvertConfig {
                        coded_size: frame.info.coded_size,
                        source_rect: Some(frame.info.source_rect()),
                        output_size: frame.info.visible_size,
                    };
                    let at = Instant::now();
                    converter = Some(decoder.with_context_lock(|| {
                        Nv12Converter::new(&decoder.device, &decoder.context, config)
                    })?);
                    converter_init_ms += milliseconds(at.elapsed());
                    if options.consumer != ConsumerMode::None {
                        let at = Instant::now();
                        consumer = Some(decoder.with_context_lock(|| {
                            Consumer::new(
                                &decoder,
                                converter.as_ref().expect("converter initialized"),
                                options.consumer,
                                options.consumer_copy,
                                &adapter,
                            )
                        })?);
                        consumer_init_ms += milliseconds(at.elapsed());
                    }
                }
                let converter = converter.as_mut().context("Converter missing")?;
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut publish_work = Duration::ZERO;
                loop {
                    let publish_at = Instant::now();
                    let result = decoder.with_context_lock(|| {
                        converter.publish_tracked_diagnostic(
                            &frame.texture,
                            frame.info.array_slice,
                            || cancel.load(Ordering::Acquire),
                            PublicationDiagnostics {
                                flush_after_release: options.producer_flush_after_release,
                                consumer_handoff: options.consumer != ConsumerMode::None,
                            },
                        )
                    });
                    publish_work += publish_at.elapsed();
                    measurements.publish_attempts += 1;
                    match result? {
                        TrackedPublish::Submitted {
                            ticket,
                            publication,
                        } => {
                            pending.push_back((ticket, frame, publish_at));
                            measurements.publish.push(publish_work);
                            // Even a publication error cannot drop the frame
                            // before its already submitted GPU read completes.
                            publication?;
                            converted += 1;
                            if let Some(consumer) = &consumer {
                                let acquire_at = Instant::now();
                                loop {
                                    let status = decoder.with_context_lock(|| {
                                        consumer.consume(options.consumer_flush_after_release)
                                    })?;
                                    if status.0 == 0 {
                                        break;
                                    }
                                    ensure!(
                                        status.0 == 0x102,
                                        "Proof output consumption acquire failed: {status:?}"
                                    );
                                    measurements.output_acquire_retries += 1;
                                    retire(&decoder, converter, &mut pending, &mut measurements)?;
                                    ensure!(
                                        Instant::now() < deadline,
                                        "Proof output consumption exceeded deadline"
                                    );
                                    measurements.wait();
                                }
                                measurements.output_acquire.push(acquire_at.elapsed());
                            }
                            break;
                        }
                        TrackedPublish::Skipped(Skip::QueueFull | Skip::Busy) => {
                            measurements.backpressure_retries += 1;
                            retire(&decoder, converter, &mut pending, &mut measurements)?;
                            ensure!(
                                Instant::now() < deadline,
                                "Converter backpressure exceeded proof deadline"
                            );
                            measurements.wait();
                        }
                        TrackedPublish::Skipped(reason) => {
                            anyhow::bail!("Converter stopped without a read: {reason:?}")
                        }
                    }
                }
                ensure!(
                    pending.len() <= 3,
                    "GPU input retention exceeded three frames"
                );
            }
            Ok(())
        })();
        let reopened_loop_ms = milliseconds(stage.elapsed());
        let stage = Instant::now();
        let drain_at = Instant::now();
        let drained = match &mut converter {
            Some(converter) => drain(&decoder, converter, &mut pending, &mut measurements),
            None => Ok(()),
        };
        let cleanup_drain_ms = milliseconds(drain_at.elapsed());
        let converter_output=converter.as_ref().map(|converter|{
            let output=converter.output();let counters=converter.diagnostic_counters();
            json!({"width":output.width,"height":output.height,"cached_input_views":converter.cached_input_views(),
                "query_kicks":counters.query_kicks,"query_bool_reads":counters.query_bool_reads,
                "producer_post_release_flushes":counters.producer_post_release_flushes,"no_consumer_publications":counters.no_consumer_publications})
        });
        let consumer_open_shared_count = consumer
            .as_ref()
            .map_or(0, |consumer| usize::from(consumer.separate));
        // On fatal/hung completion, stop decoder pool reuse before releasing its
        // outstanding AVFrames. There is no detached cleanup/reaper queue.
        let close_at = Instant::now();
        drop(decoder);
        let cleanup_decoder_close_ms = milliseconds(close_at.elapsed());
        drop(pending);
        drop(converter);
        drop(consumer);
        let cleanup_ms = milliseconds(stage.elapsed());
        match (run, drained) {
            (Err(error), Err(cleanup)) => {
                return Err(error.context(format!("GPU cleanup also failed: {cleanup:#}")))
            }
            (Err(error), _) | (_, Err(error)) => return Err(error),
            _ => {}
        }
        ensure!(
            decoded + cancelled_prefix <= decode::MAX_FRAMES,
            "Proof exceeded total source frame budget"
        );
        let elapsed_ms = milliseconds(started.elapsed());
        let phase_sum_ms = device_ms
            + open_prefix_ms
            + decode_prefix_ms
            + close_prefix_ms
            + reopen_ms
            + reopened_loop_ms
            + cleanup_ms;
        Ok(
            json!({"proof":"isolated_ffmpeg7_d3d11va_av1","source":source,"adapter":adapter,
            "cancelled_prefix_frames":cancelled_prefix,"reopened_frames":decoded,"converted_frames":converted,
            "first_frame":first,"last_frame":last,"elapsed_ms":elapsed_ms,
            "phase_ms":{"device":device_ms,"open_prefix":open_prefix_ms,"decode_prefix":decode_prefix_ms,
                "close_prefix":close_prefix_ms,"reopen":reopen_ms,"reopened_loop":reopened_loop_ms,"cleanup":cleanup_ms,
                "sum_exclusive":phase_sum_ms,"unattributed":elapsed_ms-phase_sum_ms},
            "nested_phase_ms":{"converter_init":converter_init_ms,"consumer_init":consumer_init_ms,"cleanup_query_drain":cleanup_drain_ms,"cleanup_decoder_close":cleanup_decoder_close_ms},
            "open_prefix_breakdown_ms":open_prefix_breakdown,"reopen_breakdown_ms":reopen_breakdown,
            "frame_timings_ms":measurements.report(),"converter_output":converter_output,
            "diagnostic_variant":{"consumer":options.consumer.name(),"consumer_copy":options.consumer_copy,
                "producer_flush_after_release":options.producer_flush_after_release,"consumer_flush_after_release":options.consumer_flush_after_release,
                "query_kick_after_polls":options.query.kick_after_polls,"query_bool_data":options.query.read_bool,"consumer_open_shared_count":consumer_open_shared_count},
            "cpu_video_downloads":0,"cpu_color_conversions":0,"audio_or_capture":false,
            "source_writes":false,"reopen_after_cancellation":true,
            "color_policy":"Report decoded metadata unchanged; conversion requires explicit limited BT.709 and SAR 1:1",
            "output_pixel_readback":false,"production_playback_integration":false}),
        )
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn diagnostic_variants_require_conversion_and_valid_consumer_work() {
            let args = |rest: &[&str]| {
                ["--input", "owned.mkv"]
                    .into_iter()
                    .chain(rest.iter().copied())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            let baseline = parse(args(&["--convert"])).unwrap();
            assert_eq!(baseline.consumer, ConsumerMode::Same);
            assert!(!baseline.consumer_copy);
            assert!(!baseline.producer_flush_after_release);
            assert_eq!(baseline.query.kick_after_polls, 1);
            assert!(baseline.query.read_bool);
            let old_baseline = parse(args(&[
                "--convert",
                "--query-kick-after-polls",
                "0",
                "--query-status-only",
            ]))
            .unwrap();
            assert_eq!(old_baseline.query.kick_after_polls, 0);
            assert!(!old_baseline.query.read_bool);
            assert!(parse(args(&["--query-bool-data"])).is_err());
            assert!(parse(args(&["--convert", "--query-kick-after-polls", "1001"])).is_err());
            assert!(parse(args(&[
                "--convert",
                "--consumer",
                "none",
                "--consumer-copy"
            ]))
            .is_err());
            assert!(parse(args(&[
                "--convert",
                "--consumer",
                "none",
                "--consumer-flush-after-release"
            ]))
            .is_err());
            assert!(parse(args(&["--convert", "--consumer", "unknown"])).is_err());
            let separate = parse(args(&[
                "--convert",
                "--consumer",
                "separate",
                "--consumer-copy",
                "--consumer-flush-after-release",
                "--producer-flush-after-release",
                "--query-kick-after-polls",
                "100",
                "--query-bool-data",
            ]))
            .unwrap();
            assert_eq!(separate.consumer, ConsumerMode::Separate);
            assert!(separate.consumer_copy && separate.consumer_flush_after_release);
            assert!(separate.producer_flush_after_release && separate.query.read_bool);
            assert_eq!(separate.query.kick_after_polls, 100);
            assert!(parse(args(&["--convert", "--consumer", "none"])).is_ok());
        }
        #[test]
        fn aggregate_timings_use_exact_median_and_nearest_rank_p95() {
            let mut samples = Samples::default();
            assert_eq!(samples.report()["count"], 0);
            assert!(samples.report()["p95_ms"].is_null());
            for ms in 0..20 {
                samples.push(Duration::from_millis(ms));
            }
            let report = samples.report();
            assert_eq!(report["count"], 20);
            assert_eq!(report["sum_ms"], 190.);
            assert_eq!(report["median_ms"], 9.5);
            assert_eq!(report["p95_ms"], 18.);
            assert_eq!(report["max_ms"], 19.);
        }
        #[test]
        fn frame_budget_and_adapter_identity_are_explicit() {
            let args = |rest: &[&str]| {
                std::iter::once("--input".to_owned())
                    .chain(std::iter::once("owned.mkv".to_owned()))
                    .chain(rest.iter().map(|v| (*v).to_owned()))
                    .collect::<Vec<_>>()
            };
            assert!(parse(args(&["--frames", "121"])).is_err());
            assert!(parse(args(&["--frames", "1"])).is_err());
            assert!(parse(args(&["--adapter-low", "1"])).is_err());
            let options = parse(args(&[
                "--frames",
                "120",
                "--adapter-low",
                "1",
                "--adapter-high",
                "-2",
            ]))
            .unwrap();
            assert_eq!(options.luid, Some((1, -2)));
        }
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    let options = proof::parse(std::env::args().skip(1))?;
    let report = proof::run(options)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("This isolated proof requires Windows x86-64")
}
