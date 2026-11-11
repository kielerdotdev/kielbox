//! Real worker/FFmpeg lifecycle tests using generated pixels and audio only.
use super::tests::probe;
use super::*;
use recording_tools::{
    audio::synth::{Signal, SyntheticAudio, SyntheticAudioConfig},
    error::SourceError,
    source::{
        SourceEvent, SourceInfo,
        synthetic::{SyntheticConfig, SyntheticSource, TimeMode},
    },
    time::Clock,
};

fn options(folder: &std::path::Path) -> Options {
    Options {
        target: CaptureTarget::Window("owned-generated-source".into()),
        cursor: false,
        system_audio: true,
        microphone: true,
        fps: 60,
        folder: folder.to_path_buf(),
    }
}
fn factory(time: TimeMode, frames: Option<u64>) -> SourceFactory {
    Box::new(move |options, fps| {
        let mut config = SyntheticConfig::new(320, 180, fps).time(time);
        config.max_frames = frames;
        let audio = [options.system_audio, options.microphone]
            .into_iter()
            .enumerate()
            .filter(|(_, enabled)| *enabled)
            .map(|(index, _)| {
                Box::new(SyntheticAudio::new(SyntheticAudioConfig::new(
                    Signal::Tone {
                        hz: if index == 0 { 440. } else { 880. },
                        amplitude: 0.2,
                        start: Duration::ZERO,
                    },
                    time,
                ))) as Box<dyn AudioSource>
            })
            .collect();
        Ok((Box::new(SyntheticSource::new(config)), audio))
    })
}
#[derive(Default)]
struct Results {
    stats: StatsSnapshot,
    saved: Vec<(PathBuf, Vec<String>)>,
    errors: Vec<String>,
    cancelled: usize,
}
impl Results {
    fn poll(&mut self, worker: &Worker) -> bool {
        let (events, retired) = worker.poll();
        for event in events {
            match event {
                Event::Progress(stats, _) => self.stats = stats,
                Event::Saved(path, warnings) => self.saved.push((path, warnings)),
                Event::Failed(error) => self.errors.push(error),
                Event::Cancelled => self.cancelled += 1,
                _ => {}
            }
        }
        retired
    }
    fn wait(&mut self, worker: &Worker, ready: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            let retired = self.poll(worker);
            assert!(self.errors.is_empty(), "{:?}", self.errors);
            if ready(self) {
                return;
            }
            assert!(!retired, "Worker ended before expected state");
            assert!(Instant::now() < deadline, "Worker state timed out");
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn finish(&mut self, worker: &Worker) {
        let deadline = Instant::now() + Duration::from_secs(25);
        while !self.poll(worker) {
            assert!(Instant::now() < deadline, "Worker did not retire");
            thread::sleep(Duration::from_millis(20));
        }
    }
}
fn decode_all(path: &std::path::Path) {
    let output = ffmpeg()
        .args(["-threads", "2", "-i"])
        .arg(path)
        .args(["-map", "0", "-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn failed_owned_aac_encoder_preserves_a_decodable_video_prefix() {
    use recording_tools::encode::{AudioParams, VideoParams};
    use recording_tools::source::synthetic::render;
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("audio-failed.mp4");
    let config = RecordConfig::new(&path);
    let fps = Fps::from_f64(5.).unwrap();
    let pattern = SyntheticConfig::new(160, 90, fps);
    let spec = OutputSpec {
        path: path.clone(),
        container: config.container,
        video: VideoParams {
            size: pattern.size,
            fps,
        },
        audio: Some(AudioParams {
            sample_rate: 48_000,
            channels: 2,
        }),
        video_settings: config.video,
        audio_settings: config.audio,
        gif: config.gif,
    };
    let mut encoder = ExternalEncoder::open(&spec).unwrap();
    for slot in 0..5 {
        encoder
            .write_video(slot, &Arc::new(EncoderInput::Rgba(render(&pattern, slot))))
            .unwrap();
        encoder.write_audio(&vec![0.; 9600 * 2]).unwrap();
    }
    let child = encoder.audio.as_ref().unwrap().child.clone();
    child.lock().unwrap().kill().unwrap();
    child.lock().unwrap().wait().unwrap();
    assert!(encoder.write_audio(&[0.; 512]).is_err());
    let summary = Box::new(encoder).finish(Duration::from_secs(1)).unwrap();
    assert!(!summary.has_audio);
    assert_eq!(summary.audio_samples, 0);
    let info = probe(&path);
    assert_eq!(info["streams"].as_array().unwrap().len(), 1);
    assert_eq!(info["streams"][0]["codec_name"], "h264");
    decode_all(&path);
    assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 1);
}

#[test]
fn requested_audio_lost_at_finalization_is_reported_in_recording_warnings() {
    struct VideoOnlyAtFinish(Option<ExternalEncoder>);
    impl Encoder for VideoOnlyAtFinish {
        fn description(&self) -> String {
            "injected final audio failure".into()
        }
        fn input_kind(&self) -> InputKind {
            InputKind::Rgba8
        }
        fn has_audio(&self) -> bool {
            true
        }
        fn write_video(
            &mut self,
            slot: u64,
            input: &Arc<EncoderInput>,
        ) -> recording_tools::error::Result<()> {
            self.0.as_mut().unwrap().write_video(slot, input)
        }
        fn write_audio(&mut self, _: &[f32]) -> recording_tools::error::Result<()> {
            Ok(())
        }
        fn bytes_written(&self) -> u64 {
            self.0.as_ref().unwrap().bytes_written()
        }
        fn finish(
            mut self: Box<Self>,
            end: Duration,
        ) -> recording_tools::error::Result<EncodeSummary> {
            Box::new(self.0.take().unwrap()).finish(end)
        }
        fn abort(self: Box<Self>) {}
    }
    let folder = tempfile::tempdir().unwrap();
    let config = RecordConfig::new(folder.path().join("reported-audio-loss.mp4"));
    let source = SyntheticSource::new(
        SyntheticConfig::new(160, 90, config.fps)
            .time(TimeMode::Virtual)
            .max_frames(10),
    );
    let audio = SyntheticAudio::new(SyntheticAudioConfig::new(
        Signal::Silence,
        TimeMode::Virtual,
    ));
    let session = RecordingSession::start_with(
        config,
        Box::new(source),
        vec![Box::new(audio)],
        Box::new(|spec| {
            let mut video_only = spec.clone();
            video_only.audio = None;
            Ok(
                Box::new(VideoOnlyAtFinish(Some(ExternalEncoder::open(&video_only)?)))
                    as Box<dyn Encoder>,
            )
        }),
    )
    .unwrap();
    assert!(session.wait_finished(Duration::from_secs(20)).is_some());
    let recording = session.stop().unwrap();
    assert!(!recording.has_audio);
    assert!(
        recording
            .warnings
            .iter()
            .any(|warning| warning.contains("saved video without sound"))
    );
    decode_all(&recording.path);
}

/// Explicit, CPU-only evaluation of the actual external encoder. No desktop,
/// microphone, GPU or realtime waits are involved. Keep ignored during normal
/// tests because this intentionally repeats complete FFmpeg lifecycles.
#[test]
#[ignore = "Generated-media stop latency evaluation; run explicitly with --nocapture"]
fn generated_stop_latency_eval() {
    use recording_tools::encode::{AudioParams, VideoParams};
    use recording_tools::source::synthetic::{read_counter, render};
    let durations = std::env::var("RCR_STOP_EVAL_SECONDS")
        .unwrap_or_else(|_| "1,10,60".into())
        .split(',')
        .map(|value| value.parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    let repeats = std::env::var("RCR_STOP_EVAL_REPEATS")
        .unwrap_or_else(|_| "2".into())
        .parse::<usize>()
        .unwrap();
    assert!(!durations.is_empty() && durations.iter().all(|seconds| (1..=600).contains(seconds)));
    assert!((1..=10).contains(&repeats));
    let fps = Fps::from_f64(5.).unwrap();
    let pattern = SyntheticConfig::new(160, 90, fps).time(TimeMode::Virtual);
    let mut rows = Vec::new();
    for repeat in 0..repeats {
        for &seconds in &durations {
            for audio_enabled in [false, true] {
                let folder = tempfile::tempdir().unwrap();
                let path = folder.path().join("actual.mp4");
                let config = RecordConfig::new(&path);
                let spec = OutputSpec {
                    path: path.clone(),
                    container: config.container,
                    video: VideoParams {
                        size: pattern.size,
                        fps,
                    },
                    audio: audio_enabled.then_some(AudioParams {
                        sample_rate: 48_000,
                        channels: 2,
                    }),
                    video_settings: config.video,
                    audio_settings: config.audio,
                    gif: config.gif,
                };
                let mut encoder = ExternalEncoder::open(&spec).unwrap();
                let staging = encoder.directory.path().to_owned();
                let mut legacy_pcm = audio_enabled
                    .then(|| std::fs::File::create(folder.path().join("legacy.f32")).unwrap());
                let frames = seconds * 5;
                let mut audio_frame = 0_u64;
                let feed_started = Instant::now();
                for slot in 0..frames {
                    let input = Arc::new(EncoderInput::Rgba(render(&pattern, slot)));
                    encoder.write_video(slot, &input).unwrap();
                    if audio_enabled {
                        let samples = (0..9600)
                            .flat_map(|_| {
                                let value = (std::f64::consts::TAU * 440. * audio_frame as f64
                                    / 48_000.)
                                    .sin() as f32
                                    * 0.2;
                                audio_frame += 1;
                                [value, value]
                            })
                            .collect::<Vec<_>>();
                        encoder.write_audio(&samples).unwrap();
                        let bytes = samples
                            .iter()
                            .flat_map(|sample| sample.to_le_bytes())
                            .collect::<Vec<_>>();
                        legacy_pcm.as_mut().unwrap().write_all(&bytes).unwrap();
                    }
                }
                let feed_ms = feed_started.elapsed().as_secs_f64() * 1000.;
                drop(legacy_pcm);
                assert!(encoder.audio_bytes.capacity() <= 32 * 1024);
                assert!(encoder.last_pixels.capacity() <= 160 * 90 * 4);
                assert!(
                    !staging.join("audio.f32").exists(),
                    "Raw PCM must not accumulate during recording"
                );
                let children = [&encoder.video, &encoder.audio]
                    .into_iter()
                    .filter_map(|process| process.as_ref().map(|process| process.child.clone()))
                    .collect::<Vec<_>>();
                assert_eq!(children.len(), if audio_enabled { 2 } else { 1 });
                let staging_bytes = encoder.bytes_written();
                let started = Instant::now();
                let summary = Box::new(encoder)
                    .finish(Duration::from_secs(seconds))
                    .unwrap();
                let stop_ms = started.elapsed().as_secs_f64() * 1000.;
                assert_eq!(summary.video_frames, frames);
                assert!(!staging.exists(), "Encoder staging leaked after completion");
                for child in children {
                    assert!(
                        child.lock().unwrap().try_wait().unwrap().is_some(),
                        "FFmpeg child was not reaped"
                    );
                }

                // Conservative baseline: only the old full-clip AAC + final mux
                // phase, excluding its video encoder drain and durability wait.
                let legacy_aac_mux_ms = audio_enabled.then(|| {
                    let mut command = ffmpeg();
                    command
                        .arg("-i")
                        .arg(&path)
                        .args(["-f", "f32le", "-ar", "48000", "-ac", "2", "-i"])
                        .arg(folder.path().join("legacy.f32"))
                        .args([
                            "-map",
                            "0:v:0",
                            "-map",
                            "1:a:0",
                            "-c:v",
                            "copy",
                            "-c:a",
                            "aac",
                            "-b:a",
                            "128k",
                            "-threads",
                            "1",
                            "-af",
                            "apad",
                            "-t",
                            &seconds.to_string(),
                            "-movflags",
                            "+faststart",
                        ])
                        .arg(folder.path().join("legacy.mp4"))
                        .stdin(Stdio::null());
                    let started = Instant::now();
                    Process::spawn(&mut command)
                        .unwrap()
                        .wait(Duration::from_secs(30))
                        .unwrap();
                    started.elapsed().as_secs_f64() * 1000.
                });
                let info = probe(&path);
                let streams = info["streams"].as_array().unwrap();
                let video = streams
                    .iter()
                    .find(|stream| stream["codec_type"] == "video")
                    .unwrap();
                assert_eq!(
                    video["nb_frames"].as_str().unwrap().parse::<u64>().unwrap(),
                    frames
                );
                assert_eq!(video["r_frame_rate"], "5/1");
                assert_eq!(video["codec_name"], "h264");
                assert_eq!(
                    streams
                        .iter()
                        .filter(|stream| stream["codec_name"] == "aac")
                        .count(),
                    usize::from(audio_enabled)
                );
                let duration = info["format"]["duration"]
                    .as_str()
                    .unwrap()
                    .parse::<f64>()
                    .unwrap();
                assert!(
                    (duration - seconds as f64).abs() < 0.05,
                    "Duration changed: {duration}"
                );
                let decoded = ffmpeg()
                    .args(["-threads", "1", "-i"])
                    .arg(&path)
                    .args([
                        "-map", "0:v:0", "-pix_fmt", "gray", "-f", "rawvideo", "pipe:1",
                    ])
                    .output()
                    .unwrap();
                assert!(decoded.status.success());
                assert_eq!(decoded.stdout.len(), frames as usize * 160 * 90);
                for (slot, pixels) in decoded
                    .stdout
                    .as_chunks::<{ 160 * 90 }>()
                    .0
                    .iter()
                    .enumerate()
                {
                    assert_eq!(
                        read_counter(160, 90, |x, y| pixels[y as usize * 160 + x as usize]),
                        slot as u16,
                        "Video frame lost or changed"
                    );
                }
                decode_all(&path);
                let row = serde_json::json!({"repeat": repeat, "seconds": seconds, "audio": audio_enabled,
                    "frames": frames, "feed_ms": feed_ms, "stop_ms": stop_ms,
                    "legacy_aac_mux_ms": legacy_aac_mux_ms, "staging_bytes_before_stop": staging_bytes,
                    "output_bytes": summary.bytes, "decoded": true, "frame_counters_verified": true,
                    "children_reaped": true});
                eprintln!("STOP_EVAL {row}");
                rows.push(row);
            }
        }
    }
    if let Some(path) = std::env::var_os("RCR_STOP_EVAL_OUTPUT") {
        fs::write(path, serde_json::to_vec_pretty(&serde_json::json!({
            "scope": "CPU-only generated media; excludes live capture/audio, user interaction, and hardware encoder behavior",
            "width": 160, "height": 90, "fps": 5, "rows": rows
        })).unwrap()).unwrap();
    }
}

#[test]
fn retirement_drains_every_terminal_event_that_arrives_after_the_first_poll() {
    let (tx, rx) = mpsc::sync_channel(16);
    let (events, retired) = collect_events(&rx, || {
        tx.send(Event::Finishing).unwrap();
        tx.send(Event::Saved(
            PathBuf::from("actual.mp4"),
            vec!["source warning".into()],
        ))
        .unwrap();
        true
    });
    assert!(retired);
    assert_eq!(events.len(), 2);
    assert!(
        matches!(&events[1], Event::Saved(path, warnings) if path == &PathBuf::from("actual.mp4") && warnings.len() == 1)
    );
}

#[test]
#[ignore = "Owned child entrypoint for the blocked-stdin watchdog test"]
fn blocked_stdin_child() {
    if std::env::var_os("RCR_OWNED_BLOCKED_STDIN").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }
    loop {
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_blocked_encoder_input_is_interrupted_by_the_write_watchdog() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "quick_recording::stress_tests::blocked_stdin_child",
            "--ignored",
            "--nocapture",
        ])
        .env("RCR_OWNED_BLOCKED_STDIN", "1")
        .stdin(Stdio::piped());
    let mut process = Process::spawn(&mut command).unwrap();
    let child = process.child.clone();
    let started = Instant::now();
    assert!(
        process
            .write_with_timeout(&vec![0; 4 * 1024 * 1024], Duration::from_millis(500))
            .is_err()
    );
    drop(process);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(child.lock().unwrap().try_wait().unwrap().is_some());
}

#[test]
fn startup_panic_is_reported_and_leaves_no_owned_staging() {
    let folder = tempfile::tempdir().unwrap();
    let worker = Worker::start_with_sources(
        options(folder.path()),
        Box::new(|_, _| panic!("owned injected startup failure")),
    )
    .unwrap();
    let mut result = Results::default();
    result.finish(&worker);
    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].contains("startup"));
    drop(worker);
    assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 0);
}

#[test]
fn stop_and_cancel_during_startup_hold_admission_until_the_source_returns() {
    for cancel in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        let (entered, ready) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let worker = Worker::start_with_sources(
            options(folder.path()),
            Box::new(move |_, fps| {
                entered.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok((
                    Box::new(SyntheticSource::new(
                        SyntheticConfig::new(320, 180, fps).max_frames(1),
                    )),
                    vec![],
                ))
            }),
        )
        .unwrap();
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        let before = Instant::now();
        if cancel {
            worker.cancel();
        } else {
            worker.stop();
        }
        assert!(before.elapsed() < Duration::from_millis(50));
        assert!(!worker.finished());
        release.send(()).unwrap();
        let mut result = Results::default();
        result.finish(&worker);
        assert_eq!(result.cancelled, 1);
        assert!(result.errors.is_empty());
        assert!(result.saved.is_empty());
        drop(worker);
        assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 0);
    }
}

#[test]
fn repeated_pause_resume_compacts_video_and_audio_time_in_the_published_file() {
    let folder = tempfile::tempdir().unwrap();
    let output = folder.path().join("captures with spaces 日本語");
    let worker =
        Worker::start_with_sources(options(&output), factory(TimeMode::Realtime, None)).unwrap();
    let mut result = Results::default();
    result.wait(&worker, |r| r.stats.encoded >= 24);
    // Capture runs in real time from the start, however slowly the encoder warms up, so
    // measure from what was already recorded rather than from the first encoded frames.
    let started = Instant::now();
    let recorded_before = result.stats.recorded.as_secs_f64();
    for _ in 0..3 {
        worker.pause(true);
        thread::sleep(Duration::from_millis(200));
        result.poll(&worker);
        let at = result.stats.recorded;
        thread::sleep(Duration::from_millis(300));
        result.poll(&worker);
        assert_eq!(result.stats.recorded, at);
        worker.pause(false);
        result.wait(&worker, |r| {
            r.stats.recorded > at + Duration::from_millis(180)
        });
    }
    for _ in 0..50 {
        worker.stop();
    }
    result.finish(&worker);
    drop(worker);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.saved.len(), 1);
    let path = &result.saved[0].0;
    let metadata = probe(path);
    let seconds: f64 = metadata["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(seconds > 0.8);
    assert!(
        // Three 0.5 s pauses: at least 0.5 s of them must be gone.
        seconds < recorded_before + started.elapsed().as_secs_f64() - 0.5,
        "Pause was not removed: {seconds}"
    );
    assert!(
        metadata["streams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|stream| stream["codec_name"] == "aac")
    );
    decode_all(path);
    assert_eq!(fs::read_dir(&output).unwrap().count(), 1);
}

struct LostSource {
    source: SyntheticSource,
    remaining: usize,
}
impl FrameSource for LostSource {
    fn name(&self) -> &'static str {
        "owned-lost-source"
    }
    fn start(&mut self, clock: Clock) -> std::result::Result<(), SourceError> {
        self.source.start(clock)
    }
    fn info(&self) -> SourceInfo {
        self.source.info()
    }
    fn next_frame(&mut self, timeout: Duration) -> std::result::Result<SourceEvent, SourceError> {
        if self.remaining == 0 {
            return Err(SourceError::Unavailable("injected display removal".into()));
        }
        self.remaining -= 1;
        self.source.next_frame(timeout)
    }
    fn stop(&mut self) {
        self.source.stop();
    }
}
#[test]
fn source_loss_saves_a_decodable_prefix_and_reports_the_real_failure() {
    let folder = tempfile::tempdir().unwrap();
    let worker = Worker::start_with_sources(
        options(folder.path()),
        Box::new(|_, fps| {
            Ok((
                Box::new(LostSource {
                    source: SyntheticSource::new(
                        SyntheticConfig::new(320, 180, fps).time(TimeMode::Virtual),
                    ),
                    remaining: 90,
                }),
                vec![],
            ))
        }),
    )
    .unwrap();
    let mut result = Results::default();
    result.finish(&worker);
    drop(worker);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.saved.len(), 1);
    assert!(
        result.saved[0]
            .1
            .iter()
            .any(|warning| warning.contains("injected display removal")),
        "Source failure was hidden: {:?}",
        result.saved[0].1
    );
    decode_all(&result.saved[0].0);
}

#[test]
fn one_failed_audio_source_does_not_discard_video_or_the_other_source() {
    let folder = tempfile::tempdir().unwrap();
    let worker = Worker::start_with_sources(
        options(folder.path()),
        Box::new(|_, fps| {
            let mut broken = SyntheticAudioConfig::new(Signal::Silence, TimeMode::Virtual);
            broken.fail_on_start = true;
            let good = SyntheticAudioConfig::new(
                Signal::Tone {
                    hz: 880.,
                    amplitude: 0.2,
                    start: Duration::ZERO,
                },
                TimeMode::Virtual,
            );
            Ok((
                Box::new(SyntheticSource::new(
                    SyntheticConfig::new(320, 180, fps)
                        .time(TimeMode::Virtual)
                        .max_frames(60),
                )),
                vec![
                    Box::new(SyntheticAudio::new(broken)),
                    Box::new(SyntheticAudio::new(good)),
                ],
            ))
        }),
    )
    .unwrap();
    let mut result = Results::default();
    result.finish(&worker);
    drop(worker);
    assert!(result.errors.is_empty());
    assert_eq!(result.saved.len(), 1);
    assert!(
        result.saved[0]
            .1
            .iter()
            .any(|warning| warning.contains("unavailable"))
    );
    let metadata = probe(&result.saved[0].0);
    assert!(
        metadata["streams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|stream| stream["codec_name"] == "aac")
    );
    decode_all(&result.saved[0].0);
}

#[test]
fn closing_an_observer_with_a_full_progress_queue_still_finalizes_its_clip() {
    let folder = tempfile::tempdir().unwrap();
    let worker =
        Worker::start_with_sources(options(folder.path()), factory(TimeMode::Realtime, None))
            .unwrap();
    thread::sleep(Duration::from_millis(2100));
    drop(worker);
    let files = fs::read_dir(folder.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].extension().unwrap(), "mp4");
    decode_all(&files[0]);
}

#[test]
fn overlapping_recorders_keep_cancel_and_stop_ownership_separate() {
    let folder = tempfile::tempdir().unwrap();
    let a = Worker::start_with_sources(
        options(&folder.path().join("a")),
        factory(TimeMode::Realtime, None),
    )
    .unwrap();
    let b = Worker::start_with_sources(
        options(&folder.path().join("b")),
        factory(TimeMode::Realtime, None),
    )
    .unwrap();
    let mut first = Results::default();
    let mut second = Results::default();
    first.wait(&a, |r| r.stats.encoded >= 18);
    second.wait(&b, |r| r.stats.encoded >= 18);
    b.cancel();
    second.finish(&b);
    drop(b);
    let before = first.stats.encoded;
    first.wait(&a, |r| r.stats.encoded > before + 12);
    a.stop();
    first.finish(&a);
    drop(a);
    assert_eq!(second.cancelled, 1);
    assert!(second.saved.is_empty());
    assert_eq!(fs::read_dir(folder.path().join("b")).unwrap().count(), 0);
    assert_eq!(first.saved.len(), 1);
    decode_all(&first.saved[0].0);
}

#[test]
#[ignore = "Opt-in AV1 NVENC proof; generated frames only, no screen/audio capture"]
fn five_minute_av1_replay_muxes_and_decodes_while_an_independent_screencast_runs() {
    use media_core::{AvReplayBuffer, MediaPacket, VideoConfig, replay_memory_budget};
    let folder = tempfile::tempdir().unwrap();
    let worker = Worker::start_with_sources(
        options(&folder.path().join("quick")),
        factory(TimeMode::Realtime, None),
    )
    .unwrap();
    let fixture = folder.path().join("generated.ivf");
    let encoded = ffmpeg()
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=30",
            "-t",
            "2",
            "-an",
            "-c:v",
            "av1_nvenc",
            "-preset",
            "p3",
            "-b:v",
            "1000000",
            "-g",
            "60",
            "-bf",
            "0",
            "-f",
            "ivf",
        ])
        .arg(&fixture)
        .output()
        .unwrap();
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    let bytes = fs::read(fixture).unwrap();
    assert_eq!(&bytes[..4], b"DKIF");
    let mut offset = 32;
    let mut gop = Vec::<Arc<[u8]>>::new();
    while offset + 12 <= bytes.len() {
        let length = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 12;
        gop.push(bytes[offset..offset + length].into());
        offset += length;
    }
    assert_eq!(gop.len(), 60);
    let config = VideoConfig {
        width: 320,
        height: 180,
        fps: 30,
        bitrate: 1_000_000,
        keyframe_interval: 60,
    };
    let budget = replay_memory_budget(&config, 300, 0).unwrap();
    let mut ring = AvReplayBuffer::new(budget, 300).unwrap();
    let mut result = Results::default();
    for frame in 0..360_u64 * 30 {
        let start = frame * 1_000_000_000 / 30;
        ring.push(MediaPacket {
            track: 1,
            pts_ns: start,
            duration_ns: (frame + 1) * 1_000_000_000 / 30 - start,
            keyframe: frame.is_multiple_of(60),
            data: gop[frame as usize % 60].clone(),
        });
        if frame.is_multiple_of(60) {
            result.poll(&worker);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(ring.bytes() <= budget);
    }
    let retained = (ring.bytes(), ring.duration_ns());
    let packets = ring.snapshot(300);
    assert_eq!(packets.len(), 9000);
    let replay = folder.path().join("five-minutes.mkv");
    media_io::save_snapshot(&replay, &config, &[], &packets).unwrap();
    worker.stop();
    result.finish(&worker);
    drop(worker);
    assert!(result.errors.is_empty());
    assert_eq!(result.saved.len(), 1);
    assert_eq!((ring.bytes(), ring.duration_ns()), retained);
    let info = probe(&replay);
    assert_eq!(info["streams"][0]["codec_name"], "av1");
    let duration: f64 = info["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((duration - 300.).abs() < 0.05, "{duration}");
    decode_all(&replay);
    decode_all(&result.saved[0].0);
    eprintln!(
        "Generated AV1 replay: 300 seconds / 9,000 frames independently decoded; concurrent screencast saved; retained ring unchanged"
    );
}
