//! Opt-in actual local desktop capture. NEVER enables a microphone.
//! Parent runner validates the resulting files independently after this exits.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    use media_core::AudioTrack;
    use windows_capture::pipeline::{
        self, CapturePhase, PipelineEvent, RecorderCommand, ReplaySave, RunOptions,
    };
    use std::{
        collections::BTreeMap,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        },
        thread,
        time::{Duration, Instant},
    };
    struct DisplayWake;
    impl Drop for DisplayWake {
        fn drop(&mut self) {
            use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS);
            }
        }
    }
    let previous_power = unsafe {
        use windows::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };
        SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED).0
    };
    ensure!(
        previous_power != 0,
        "Could not keep display/system awake for finite hardware test"
    );
    let _display = DisplayWake;
    thread::sleep(Duration::from_millis(500));
    let input_desktop = unsafe {
        use windows::Win32::System::StationsAndDesktops::{
            CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_CONTROL_FLAGS,
            DESKTOP_READOBJECTS, UOI_NAME,
        };
        match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) {
            Ok(desktop) => {
                let mut name = [0_u16; 256];
                let mut needed = 0;
                let read = GetUserObjectInformationW(
                    windows::Win32::Foundation::HANDLE(desktop.0),
                    UOI_NAME,
                    Some(name.as_mut_ptr().cast()),
                    (name.len() * 2) as u32,
                    Some(&mut needed),
                );
                let _ = CloseDesktop(desktop);
                serde_json::json!({"readable": true, "name": read.is_ok().then(|| String::from_utf16_lossy(&name[..name.iter().position(|value| *value == 0).unwrap_or(name.len())])),
                    "name_error": read.err().map(|error| error.to_string())})
            }
            Err(error) => serde_json::json!({"readable": false, "error": error.to_string()}),
        }
    };
    let mut seconds = 6_f64;
    let mut audio = false;
    let mut scenario = "continuous".to_owned();
    let mut animate_fixture = false;
    let mut output = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("Missing value for {arg}"))?;
        match arg.as_str() {
            "--seconds" => seconds = value.parse()?,
            "--audio" => {
                ensure!(
                    matches!(value.as_str(), "none" | "system"),
                    "Only none/system audio is permitted"
                );
                audio = value == "system";
            }
            "--scenario" => scenario = value,
            "--animate-fixture" => {
                ensure!(
                    matches!(value.as_str(), "true" | "false"),
                    "Fixture must be true/false"
                );
                animate_fixture = value == "true";
            }
            "--output" => output = Some(PathBuf::from(value)),
            _ => anyhow::bail!("Unknown option {arg}"),
        }
    }
    ensure!(
        seconds.is_finite() && (1. ..=120.).contains(&seconds),
        "Capture duration must be 1..120 seconds"
    );
    ensure!(
        matches!(
            scenario.as_str(),
            "continuous" | "replay-on-stop" | "record-and-replay" | "replay-save-near-stop"
        ),
        "Unknown scenario"
    );
    let output = output.context("--output <new-directory> is required")?;
    ensure!(
        !output.exists(),
        "Output directory already exists; choose a new case path"
    );
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    let mut fixture = if animate_fixture {
        Some(OwnedFixture::new()?)
    } else {
        None
    };
    let main_path = output.join("recording.mkv");
    let replay_path = output.join("replay.mkv");
    let continuous = scenario != "replay-on-stop";
    let (observer_tx, observer_rx) = mpsc::sync_channel(16);
    let (event_tx, event_rx) = mpsc::sync_channel(16);
    let (command_tx, command_rx) = mpsc::sync_channel(8);
    let options = RunOptions {
        seconds: None,
        output: main_path.clone(),
        preview: false,
        display: Some(windows_capture::display::Selection::Primary),
        audio: if audio {
            vec![AudioTrack::system()]
        } else {
            vec![]
        },
        replay_seconds: seconds.ceil() as u64,
        record_to_disk: continuous,
        save_on_stop: !continuous,
        lifecycle: Some(observer_tx),
        ..Default::default()
    };
    let requested_profile = serde_json::to_value(&options.video)?;
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let epoch = Instant::now();
    let worker = thread::Builder::new()
        .name("rcr-hardware-stop-eval".into())
        .spawn(move || {
            let result =
                pipeline::run_controlled(options, worker_stop, Some(event_tx), Some(command_rx));
            (Instant::now(), result)
        })?;
    let mut phases = BTreeMap::new();
    let mut capture_started = None;
    let mut stop_at = None;
    let mut replay_requested = false;
    let mut saved = BTreeMap::<PathBuf, f64>::new();
    let mut pending = Vec::<ReplaySave>::new();
    let mut notices = Vec::new();
    let mut errors = Vec::new();
    let mut terminal_stats = None;
    loop {
        if let Some(fixture) = fixture.as_mut() {
            fixture.tick()?;
        }
        for event in observer_rx.try_iter() {
            phases.insert(
                format!("{:?}", event.phase),
                event.at.duration_since(epoch).as_secs_f64() * 1000.,
            );
            if event.phase == CapturePhase::CaptureStarted {
                capture_started = Some(event.at);
            }
        }
        for event in event_rx.try_iter() {
            match event {
                PipelineEvent::Started(_) | PipelineEvent::Stats(_) => {}
                PipelineEvent::Finished(stats) => terminal_stats = Some(stats),
                PipelineEvent::Error(error) => errors.push(error),
                PipelineEvent::Notice(notice) => notices.push(notice),
                PipelineEvent::ReplaySaved(path) => {
                    saved.insert(path, epoch.elapsed().as_secs_f64() * 1000.);
                }
                PipelineEvent::ReplayPending(job) => pending.push(job),
            }
        }
        let mut index = 0;
        while index < pending.len() {
            if !pending[index].is_finished() {
                index += 1;
                continue;
            }
            match pending.swap_remove(index).complete() {
                Ok(path) => {
                    saved.insert(path, epoch.elapsed().as_secs_f64() * 1000.);
                }
                Err(error) => errors.push(format!("Offline replay: {error:#}")),
            }
        }
        if let Some(started) = capture_started {
            let save_time = if scenario == "replay-save-near-stop" {
                seconds - 0.005
            } else {
                seconds / 2.
            };
            if matches!(
                scenario.as_str(),
                "record-and-replay" | "replay-save-near-stop"
            ) && !replay_requested
                && started.elapsed().as_secs_f64() >= save_time
            {
                command_tx
                    .try_send(RecorderCommand::SaveReplay {
                        path: replay_path.clone(),
                        seconds: seconds.ceil() as u64,
                        suppress_microphone: false,
                    })
                    .map_err(|_| anyhow::anyhow!("Could not submit replay request"))?;
                replay_requested = true;
            }
            if stop_at.is_none() && started.elapsed().as_secs_f64() >= seconds {
                let requested = Instant::now();
                stop.store(true, Ordering::Release);
                stop_at = Some(requested);
            }
        }
        if worker.is_finished() && pending.is_empty() {
            break;
        }
        if epoch.elapsed() >= Duration::from_secs_f64(seconds + 60.) {
            stop.store(true, Ordering::Release);
            for job in &pending {
                job.cancel();
            }
            anyhow::bail!(
                "Hardware evaluation exceeded capture+60s safety budget; capture may be blocked"
            );
        }
        thread::sleep(Duration::from_millis(2));
    }
    // Drain lifecycle and terminal events published immediately before worker exit.
    for event in observer_rx.try_iter() {
        phases.insert(
            format!("{:?}", event.phase),
            event.at.duration_since(epoch).as_secs_f64() * 1000.,
        );
    }
    for event in event_rx.try_iter() {
        match event {
            PipelineEvent::Finished(stats) => terminal_stats = Some(stats),
            PipelineEvent::ReplaySaved(path) => {
                saved.insert(path, epoch.elapsed().as_secs_f64() * 1000.);
            }
            PipelineEvent::Notice(message) => notices.push(message),
            PipelineEvent::Error(error) => errors.push(error),
            PipelineEvent::ReplayPending(job) => {
                let path = job.complete()?;
                saved.insert(path, epoch.elapsed().as_secs_f64() * 1000.);
            }
            _ => {}
        }
    }
    let (worker_returned, result) = worker
        .join()
        .map_err(|_| anyhow::anyhow!("Capture worker panicked"))?;
    let retired = Instant::now();
    let pipeline = match result {
        Ok(stats) => Some(stats),
        Err(error) => {
            errors.push(format!("{error:#}"));
            None
        }
    };
    let stop_ms = stop_at.map(|stop| stop.duration_since(epoch).as_secs_f64() * 1000.);
    if continuous && phases.contains_key("RecordingPublished") {
        saved.insert(main_path, phases["RecordingPublished"]);
    }
    let phase_after_stop = phases
        .iter()
        .filter_map(|(phase, ms)| stop_ms.map(|stop| (phase.clone(), *ms - stop)))
        .collect::<BTreeMap<_, _>>();
    let saved_rows = saved.iter().map(|(path, ms)| serde_json::json!({
        "path": path, "published_ms": ms, "stop_to_published_ms": stop_ms.map(|stop| ms - stop),
    })).collect::<Vec<_>>();
    let fixture_diagnostics = fixture.as_ref().map(OwnedFixture::diagnostics);
    if animate_fixture && pipeline.as_ref().is_some_and(|stats| stats.captured <= 10) {
        errors.push("Owned animated fixture did not produce >10 observed desktop updates; active-content capture is unproven".into());
    }
    let report = serde_json::json!({
        "scope": "Actual DXGI/NVENC/WASAPI worker path; independently requested Stop, no Desktop UI dispatch or microphone",
        "seconds_requested": seconds, "scenario": scenario, "desktop_loopback": audio, "microphone": false,
        "display_keep_awake": true, "previous_execution_state": previous_power, "input_desktop": input_desktop,
        "owned_animated_fixture": animate_fixture,
        "fixture_diagnostics": fixture_diagnostics,
        "requested_video": requested_profile, "stop_requested_ms": stop_ms, "phases_ms": phases,
        "phase_after_stop_ms": phase_after_stop,
        "stop_to_worker_return_ms": stop_at.map(|stop| worker_returned.saturating_duration_since(stop).as_secs_f64() * 1000.),
        "stop_to_worker_join_ms": stop_at.map(|stop| retired.saturating_duration_since(stop).as_secs_f64() * 1000.),
        "files": saved_rows, "pipeline": pipeline, "terminal_event_seen": terminal_stats.is_some(),
        "errors": errors, "notices": notices,
    });
    std::fs::write(
        output.join("worker-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    ensure!(
        errors.is_empty(),
        "Hardware capture failed; inspect worker-report.json"
    );
    ensure!(
        stop_at.is_some() && terminal_stats.is_some(),
        "Hardware test did not observe Stop and terminal result"
    );
    ensure!(
        !saved.is_empty() && (!replay_requested || saved.contains_key(&replay_path)),
        "Requested output was not published"
    );
    Ok(())
}

#[cfg(windows)]
struct OwnedFixture {
    window: windows::Win32::Foundation::HWND,
    frame: u64,
    updated: std::time::Instant,
    _local: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(windows)]
impl OwnedFixture {
    fn new() -> anyhow::Result<Self> {
        use windows::{core::w, Win32::UI::WindowsAndMessaging::*};
        let window = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                w!("STATIC"),
                w!("RCR actual hardware recording evaluation"),
                WS_POPUP | WS_VISIBLE | WS_BORDER,
                24,
                24,
                300,
                64,
                None,
                None,
                None,
                None,
            )?
        };
        unsafe {
            let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
            SetWindowPos(
                window,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            )?;
        }
        Ok(Self {
            window,
            frame: 0,
            updated: std::time::Instant::now() - std::time::Duration::from_secs(1),
            _local: std::marker::PhantomData,
        })
    }
    fn tick(&mut self) -> anyhow::Result<()> {
        use windows::{core::PCWSTR, Win32::UI::WindowsAndMessaging::*};
        if self.updated.elapsed() >= std::time::Duration::from_millis(100) {
            self.frame += 1;
            let text = format!(
                "RCR actual local recording test\nOwned changing marker {}",
                self.frame
            )
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
            unsafe {
                SetWindowTextW(self.window, PCWSTR(text.as_ptr()))?;
                SetWindowPos(
                    self.window,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                )?;
            }
            self.updated = std::time::Instant::now();
        }
        let mut message = MSG::default();
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }
    fn diagnostics(&self) -> serde_json::Value {
        use windows::Win32::{Foundation::RECT, UI::WindowsAndMessaging::*};
        let mut rect = RECT::default();
        let read = unsafe { GetWindowRect(self.window, &mut rect) };
        serde_json::json!({"ticks": self.frame,
            "is_window_visible": unsafe { IsWindowVisible(self.window).as_bool() },
            "is_topmost": unsafe { GetWindowLongPtrW(self.window, GWL_EXSTYLE) } & WS_EX_TOPMOST.0 as isize != 0,
            "rect": [rect.left, rect.top, rect.right, rect.bottom],
            "rect_error": read.err().map(|error| error.to_string())})
    }
}

#[cfg(windows)]
impl Drop for OwnedFixture {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.window);
        }
    }
}

#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("Hardware stop evaluation requires Windows")
}
