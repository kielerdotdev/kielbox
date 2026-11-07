#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use gpui::{prelude::*, px, size, *};
#[cfg(windows)]
mod annotation_surface;
#[cfg(windows)]
mod atlas_churn_proof;
#[cfg(windows)]
mod audio_device_picker;
#[cfg(windows)]
mod display_picker;
#[cfg(windows)]
mod audio_source_picker;
#[cfg(windows)]
mod capture_overlay;
#[cfg(windows)]
mod clip_names;
mod editor;
#[cfg(any(windows, test))]
mod capture_retirement;
mod diagnostics_io;
#[cfg(windows)]
mod setup_diagnostics;
#[cfg(windows)]
mod installation;
#[cfg(windows)]
mod installation_ui;
#[cfg(windows)]
mod installer_smoke;
#[cfg(windows)]
mod single_instance;
#[cfg(windows)]
mod updates;
#[cfg(windows)]
mod update_handoff;
#[cfg(windows)]
mod hardware_stop_proof;
#[cfg(windows)]
mod disk_usage;
#[cfg(windows)]
mod shell_icons;
#[cfg(windows)]
mod file_type_icons;
#[cfg(any(windows, test))]
mod disk_treemap;
#[cfg(any(windows, test))]
mod disk_sunburst;
#[cfg(windows)]
mod gpu_surface_proof;
#[cfg(windows)]
mod hotkeys;
#[cfg(windows)]
mod quick_recording;
#[cfg(windows)]
mod quick_recorder;
#[cfg(windows)]
mod quick_recorder_proof;
#[cfg(windows)]
mod image_editor;
#[cfg(windows)]
mod library;
#[cfg(windows)]
mod library_scan;
#[cfg(windows)]
mod memory_usage;
mod preferences;
mod preferences_io;
mod recordings;
#[cfg(windows)]
mod region_targets;
#[cfg(windows)]
mod screen_tools;
#[cfg(windows)]
mod screenshot_overlay;
#[cfg(windows)]
mod studio_ui;
mod text_input;
mod theme;
mod timeline_view;
#[cfg(windows)]
mod toolkit;
#[cfg(windows)]
mod trash_recovery;
#[cfg(windows)]
mod trash_recovery_ui;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod ui_proof;
#[cfg(windows)]
mod utilities;
use media_core::{AudioTrack, PipelineStats};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use windows_capture::pipeline::{self, PipelineEvent, RecorderCommand, RunOptions};
#[cfg(windows)]
use std::time::Instant;

use theme::{ACCENT, LINE, MUTED, PANEL};

#[cfg(windows)]
static PROOF_FAILED: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
static INSTALLATION: std::sync::OnceLock<installation::State> = std::sync::OnceLock::new();
#[cfg(windows)]
static INSTALLATION_NOTICE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
#[cfg(windows)]
static UPDATE_ON_EXIT: std::sync::Mutex<Option<installation_ui::PendingInstall>> = std::sync::Mutex::new(None);
#[cfg(windows)]
static RESTART_AFTER_EXIT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg(windows)]
fn capture_proof_ready(stats: &PipelineStats, capture_failed: bool, preview_failed: bool, draws: u64) -> bool {
    !capture_failed && !preview_failed && stats.error.is_none()
        && stats.encoded > 0 && stats.preview_published > 0 && draws > 0
        && stats.raw_cpu_readbacks == 0 && stats.audio_errors.is_empty()
}

#[cfg(all(test, windows))]
mod capture_proof_tests {
    use super::{capture_proof_ready, PipelineStats};
    #[test]
    fn capture_proof_requires_actual_encoding_gpu_preview_and_clean_requested_audio() {
        let mut stats = PipelineStats { encoded: 360, preview_published: 360, ..Default::default() };
        assert!(capture_proof_ready(&stats, false, false, 360));
        assert!(!capture_proof_ready(&stats, true, false, 360));
        assert!(!capture_proof_ready(&stats, false, true, 360));
        assert!(!capture_proof_ready(&stats, false, false, 0));
        stats.encoded = 0;
        assert!(!capture_proof_ready(&stats, false, false, 360));
        stats.encoded = 360; stats.preview_published = 0;
        assert!(!capture_proof_ready(&stats, false, false, 360));
        stats.preview_published = 360; stats.raw_cpu_readbacks = 1;
        assert!(!capture_proof_ready(&stats, false, false, 360));
        stats.raw_cpu_readbacks = 0; stats.audio_errors.push("Requested microphone failed".into());
        assert!(!capture_proof_ready(&stats, false, false, 360));
        stats.audio_errors.clear(); stats.error = Some("Final output failed".into());
        assert!(!capture_proof_ready(&stats, false, false, 360));
    }
}

#[cfg(windows)]
fn capture_error_message(detail: String) -> String {
    if detail.contains("this GPU does not support AV1 NVENC encoding") {
        format!("The capture GPU cannot encode AV1 with NVENC. Use an NVIDIA GPU with AV1 NVENC support and a compatible NVIDIA driver.\n\n{detail}")
    } else { detail }
}

#[cfg(windows)]
fn consume_recording_restart(pending: &mut bool, allow_restart: bool) -> bool {
    std::mem::take(pending) && allow_restart
}

#[cfg(windows)]
fn replay_save_request(path: PathBuf, seconds: u64, microphone: bool, cleanup: bool) -> RecorderCommand {
    RecorderCommand::SaveReplay { path, seconds, suppress_microphone: microphone && cleanup }
}

#[cfg(windows)]
struct SetupInspection {
    cancel: ::toolkit::core::workflow::CancelToken,
    result: mpsc::Receiver<Result<setup_diagnostics::Report, String>>,
}
#[cfg(windows)]
impl Drop for SetupInspection {
    fn drop(&mut self) { self.cancel.cancel(); }
}

#[cfg(windows)]
struct Studio {
    updates: updates::Updates,
    welcome_visible: bool,
    installation_restart_needed: bool,
    installation_next_root: Option<PathBuf>,
    pending_native_dialogs: usize,
    setup_inspection: Option<SetupInspection>,
    setup_report: Option<setup_diagnostics::Report>,
    setup_error: Option<String>,
    settings_scroll: ScrollHandle,
    installation_bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    updates_bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    quick_recorder: Entity<quick_recorder::QuickRecorder>,
    editor_suspended: bool,
    shell_operation: u64,
    toolkit: Entity<toolkit::Toolkit>,
    toolkit_files: Vec<PathBuf>,
    tray: Option<tray::Tray>,
    preferences: preferences::Preferences,
    layout_save_revision: u64,
    layout_save_pending: bool,
    layout_save_error: Option<String>,
    library: library::Library,
    library_scan: library_scan::LibraryScanner,
    search: Entity<text_input::TextInput>,
    restart_recording: bool,
    session_profile: (u32, u32, u64),
    session_display: Option<preferences::SavedDisplay>,
    display_picker: Option<Entity<display_picker::DisplayPicker>>,
    display_focus: FocusHandle,
    receiver: Option<mpsc::Receiver<PipelineEvent>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    capture_terminal_seen: bool,
    pending_replays: Vec<pipeline::ReplaySave>,
    audio_error_focus: FocusHandle,
    audio_error_scroll: ScrollHandle,
    capture_options_scroll: ScrollHandle,
    surface: Option<WindowsGpuSurface>,
    preview_error: Option<String>,
    stats: PipelineStats,
    active: bool,
    error: Option<String>,
    proof_seconds: Option<f64>,
    exit_after_proof: bool,
    report_revision: u64,
    tab: usize,
    files: Vec<PathBuf>,
    commands: Option<mpsc::SyncSender<RecorderCommand>>,
    record_to_disk: bool,
    system_audio: bool,
    audio_choice: audio_source_picker::AudioChoice,
    audio_picker: Option<Entity<audio_source_picker::AudioSourcePicker>>,
    audio_device_picker: Option<Entity<audio_device_picker::AudioDevicePicker>>,
    system_device_focus: FocusHandle,
    microphone_device_focus: FocusHandle,
    audio_source_focus: FocusHandle,
    audio_picker_fixture: bool,
    microphone: bool,
    suppress_microphone: bool,
    notice: Option<String>,
    saved_replay: Option<PathBuf>,
    editor: Entity<editor::EditorView>,
    hotkeys: Option<hotkeys::Hotkeys>,
    hotkey_edit: Option<usize>,
    hotkey_focus: FocusHandle,
    hotkey_blur: Option<Subscription>,
    name_dialog: Option<Entity<clip_names::ClipNameDialog>>,
    name_restore: Option<FocusHandle>,
    trash_panel: Option<Entity<trash_recovery_ui::TrashRecoveryPanel>>,
    trash_focus: FocusHandle,
    trash_restoring: bool,
    trash_admission: trash_recovery_ui::RecoveryAdmission,
}

#[cfg(windows)]
impl Studio {
    fn new(
        proof_seconds: Option<f64>,
        exit_after_proof: bool,
        fixture: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                if view.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let preferences = preferences::Preferences::load();
        let startup_notice = match (preferences::take_startup_notice(), INSTALLATION_NOTICE.get()) {
            (Some(notice), Some(installation)) => Some(format!("{notice}\n{installation}")),
            (notice, installation) => notice.or_else(|| installation.cloned()),
        };
        recordings::set_root(preferences.folder.clone());
        let search = cx.new(|cx| text_input::TextInput::new(cx, ""));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        let toolkit = cx.new(toolkit::Toolkit::new);
        let quick_recorder = toolkit.read(cx).quick_recorder();
        cx.subscribe(&quick_recorder, |this, _, _: &quick_recorder::Saved, cx| {
            this.refresh_files();
            cx.notify();
        }).detach();
        cx.subscribe(&toolkit, |this, _, event: &toolkit::ToolkitEvent, cx| {
            match event {
                toolkit::ToolkitEvent::Activate => {
                    this.tab = 4;
                    this.editor.update(cx, |editor, cx| editor.hide(cx));
                }
                toolkit::ToolkitEvent::FilesChanged(paths) => {
                    this.toolkit_files = paths.clone();
                    this.refresh_files();
                }
                toolkit::ToolkitEvent::Recorder => this.quick_recorder.update(cx, |recorder, cx| recorder.show(cx)),
            }
            cx.notify();
        })
        .detach();
        let editor = cx.new(editor::EditorView::new);
        editor.update(cx, |editor, cx| {
            editor.set_gpu_preview_enabled(preferences.gpu_video_preview, cx);
        });
        cx.subscribe(&editor, |this, _, event: &editor::ShareClip, cx| {
            this.tab = 4;
            this.editor.update(cx, |editor, cx| editor.hide(cx));
            this.toolkit.update(cx, |toolkit, cx| {
                toolkit.share_files(vec![event.0.clone()], cx)
            });
            cx.notify();
        })
        .detach();
        cx.subscribe(&editor, |this, _, _: &editor::BackToLibrary, cx| {
            this.tab = 1;
            this.refresh_files();
            cx.notify();
        })
        .detach();
        let mut this = Self {
            updates: if fixture {
                updates::Updates::presentation_fixture(updates::Status::NotInstalled, Instant::now())
            } else { updates::Updates::new(Instant::now()) },
            welcome_visible: !fixture && INSTALLATION.get().is_some_and(|state| state.first_run),
            installation_restart_needed: false,
            installation_next_root: None,
            pending_native_dialogs: 0,
            setup_inspection: None,
            setup_report: None,
            setup_error: None,
            settings_scroll: ScrollHandle::new(),
            installation_bounds: Default::default(),
            updates_bounds: Default::default(),
            quick_recorder,
            editor_suspended: false,
            shell_operation: 0,
            toolkit,
            toolkit_files: vec![],
            tray: None,
            system_audio: preferences.system_audio,
            audio_choice: Default::default(),
            audio_picker: None,
            audio_device_picker: None,
            display_picker: None,
            display_focus: cx.focus_handle().tab_stop(true),
            session_display: preferences.display.clone(),
            system_device_focus: cx.focus_handle().tab_stop(true),
            microphone_device_focus: cx.focus_handle().tab_stop(true),
            audio_source_focus: cx.focus_handle().tab_stop(true),
            audio_picker_fixture: fixture,
            microphone: preferences.microphone,
            suppress_microphone: preferences.clean_microphone,
            session_profile: (
                preferences.height,
                preferences.fps,
                preferences.replay_seconds,
            ),
            preferences,
            layout_save_revision: 0,
            layout_save_pending: false,
            layout_save_error: None,
            library: library::Library::new(),
            library_scan: library_scan::LibraryScanner::new(),
            search,
            restart_recording: false,
            receiver: None,
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            capture_terminal_seen: false,
            pending_replays: Vec::new(),
            audio_error_focus: cx.focus_handle().tab_stop(true),
            audio_error_scroll: ScrollHandle::new(),
            capture_options_scroll: ScrollHandle::new(),
            surface: None,
            preview_error: None,
            stats: Default::default(),
            active: false,
            error: None,
            proof_seconds,
            exit_after_proof,
            report_revision: 0,
            tab: 0,
            files: Vec::new(),
            commands: None,
            record_to_disk: true,
            notice: startup_notice,
            saved_replay: None,
            editor,
            hotkeys: None,
            hotkey_edit: None,
            hotkey_focus: cx.focus_handle(),
            hotkey_blur: None,
            name_dialog: None,
            name_restore: None,
            trash_panel: None,
            trash_focus: cx.focus_handle().tab_stop(true),
            trash_restoring: false,
            trash_admission: Default::default(),
        };
        this.updates.settings = this.preferences.updates.clone();
        if !fixture {
            match hotkeys::Hotkeys::start_with(&this.preferences.hotkeys) {
                Ok(hotkeys) => this.hotkeys = Some(hotkeys),
                Err(error) => {
                    let message = format!("Global shortcuts unavailable: {error:#}");
                    this.notice = Some(match this.notice.take() {
                        Some(previous) => format!("{previous}\n{message}"),
                        None => message,
                    });
                }
            }
        }
        this.refresh_files();
        if proof_seconds.is_some() {
            this.start(cx);
        }
        this
    }
    fn open_recording(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.library.invalidate_import_navigation();
        let title = self.capture_title(&path);
        if recordings::is_image(&path) {
            self.tab = 4;
            self.toolkit
                .update(cx, |toolkit, cx| toolkit.open_image(path, cx));
            cx.notify();
            return;
        }
        self.tab = 2;
        self.sync_editor_choices(cx);
        self.editor.update(cx, |editor, cx| {
            editor.open_media(path, cx);
            editor.set_title(title);
        });
        cx.notify();
    }
    fn sync_editor_choices(&mut self, cx: &mut Context<Self>) {
        if self.tab != 2 {
            return;
        }
        let visible = self.visible_library_files(cx);
        let choices = visible
            .iter()
            .filter(|path| recordings::is_supported(path) && !recordings::is_image(path))
            .map(|path| (path.clone(), self.capture_title(path)))
            .collect();
        self.editor.update(cx, |editor, cx| {
            if editor.set_library_choices(choices) {
                cx.notify();
            }
        });
    }
    fn capture_title(&self, path: &std::path::Path) -> String {
        self.preferences
            .custom_display_name(path)
            .map(str::to_owned)
            .unwrap_or_else(|| self.library.cached_title(path))
    }
    fn rename_capture(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.preferences.display_name(&path);
        let dialog = cx.new(|cx| clip_names::ClipNameDialog::new(path, title, cx));
        cx.subscribe(&dialog, |this, _, event: &clip_names::NameEvent, cx| {
            if let clip_names::NameEvent::Saved(path, name) = event {
                if let Err(error) = this.preferences.set_display_name(path, name) {
                    this.notice = Some(error.into());
                } else {
                    this.save_preferences();
                    this.sync_editor_choices(cx);
                }
            }
            this.name_dialog = None;
            cx.notify();
        })
        .detach();
        self.name_restore = window.focused(cx);
        self.name_dialog = Some(dialog);
        cx.notify();
    }
    fn refresh_files(&mut self) {
        self.library_scan.request(library_scan::ScanRequest {
            root: recordings::root(),
            toolkit_files: self.toolkit_files.clone(),
            imported_files: self.preferences.imported_files.clone(),
        });
    }
    fn open_audio_picker(&mut self, cx: &mut Context<Self>) {
        if self.active
            || self.audio_picker.is_some()
            || self.audio_device_picker.is_some()
            || self.display_picker.is_some()
            || self.name_dialog.is_some()
        {
            return;
        }
        let picker = cx.new(|cx| {
            if self.audio_picker_fixture {
                audio_source_picker::AudioSourcePicker::seeded(
                    self.audio_choice.clone(),
                    self.audio_source_focus.clone(),
                    cx,
                )
            } else {
                audio_source_picker::AudioSourcePicker::new(
                    self.audio_choice.clone(),
                    self.audio_source_focus.clone(),
                    cx,
                )
            }
        });
        self.attach_audio_picker(picker, cx);
    }
    fn attach_audio_picker(
        &mut self,
        picker: Entity<audio_source_picker::AudioSourcePicker>,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe(
            &picker,
            |this, _, event: &audio_source_picker::PickerEvent, cx| {
                // Capture can start through a global shortcut while a picker worker
                // completes. A late selection never changes an active session.
                if !this.active
                    && let audio_source_picker::PickerEvent::Selected(choice) = event
                {
                    this.audio_choice = choice.clone();
                }
                this.audio_picker = None;
                cx.notify();
            },
        )
        .detach();
        self.audio_picker = Some(picker);
        cx.notify();
    }
    fn open_audio_device_picker(
        &mut self,
        kind: windows_capture::audio_endpoint::AudioEndpointKind,
        cx: &mut Context<Self>,
    ) {
        use windows_capture::audio_endpoint::AudioEndpointKind;
        if self.active
            || self.audio_picker.is_some()
            || self.audio_device_picker.is_some()
            || self.display_picker.is_some()
            || self.name_dialog.is_some()
            || self.trash_panel.is_some()
        {
            return;
        }
        let (current, restore) = match kind {
            AudioEndpointKind::Render => (
                self.preferences.system_audio_device.clone(),
                self.system_device_focus.clone(),
            ),
            AudioEndpointKind::Capture => (
                self.preferences.microphone_device.clone(),
                self.microphone_device_focus.clone(),
            ),
        };
        let picker = cx.new(|cx| {
            if self.audio_picker_fixture {
                audio_device_picker::AudioDevicePicker::seeded(kind, current, restore, cx)
            } else {
                audio_device_picker::AudioDevicePicker::new(kind, current, restore, cx)
            }
        });
        cx.subscribe(
            &picker,
            move |this, picker, event: &audio_device_picker::PickerEvent, cx| {
                if this
                    .audio_device_picker
                    .as_ref()
                    .is_none_or(|current| current.entity_id() != picker.entity_id())
                {
                    return;
                }
                if !this.active
                    && let audio_device_picker::PickerEvent::Selected(selected) = event
                {
                    match kind {
                        AudioEndpointKind::Render => {
                            this.preferences.system_audio_device = selected.clone()
                        }
                        AudioEndpointKind::Capture => {
                            this.preferences.microphone_device = selected.clone()
                        }
                    }
                    if !this.audio_picker_fixture {
                        this.save_preferences();
                    }
                }
                this.audio_device_picker = None;
                cx.notify();
            },
        )
        .detach();
        self.audio_device_picker = Some(picker);
        cx.notify();
    }
    fn start(&mut self, cx: &mut Context<Self>) {
        // Even an early Finished payload cannot release this session's actual
        // NVENC/D3D ownership. Poll alone retires the held worker after teardown.
        if self.active || self.worker.is_some() {
            return;
        }
        // An older report must not alter a newer start, including a failed one.
        self.report_revision = self.report_revision.wrapping_add(1);
        // Every start begins a new session. Automatic restart intent was already
        // consumed by the terminal-event handler; manual starts discard old intent.
        consume_recording_restart(&mut self.restart_recording, false);
        self.audio_picker = None;
        self.audio_device_picker = None;
        self.display_picker = None;
        self.capture_terminal_seen = false;
        self.stop = Arc::new(AtomicBool::new(false));
        self.surface = None;
        self.preview_error = None;
        self.error = None;
        self.stats = Default::default();
        let (tx, rx) = mpsc::sync_channel(8);
        let (command_tx, command_rx) = mpsc::sync_channel(8);
        self.commands = Some(command_tx);
        let stop = self.stop.clone();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut audio = Vec::new();
        if self.system_audio {
            let track = match &self.audio_choice {
                audio_source_picker::AudioChoice::Desktop => {
                    let mut track = AudioTrack::system();
                    track.endpoint_id = self
                        .preferences
                        .system_audio_device
                        .as_ref()
                        .map(|device| device.id.clone());
                    track.validate_endpoint_selection().map(|_| track)
                }
                audio_source_picker::AudioChoice::Process(process) => {
                    AudioTrack::process_instance(process.pid, process.created_at_hns).map(
                        |mut track| {
                            track.name = format!("Application: {}", process.exe_name);
                            track
                        },
                    )
                }
            };
            match track {
                Ok(track) => audio.push(track),
                Err(error) => {
                    self.commands = None;
                    self.error = Some(format!(
                        "Audio source unavailable: {error}. Reselect the audio source or device."
                    ));
                    cx.notify();
                    if self.exit_after_proof { cx.quit(); }
                    return;
                }
            }
        }
        if self.microphone {
            let mut track = AudioTrack::microphone();
            track.endpoint_id = self
                .preferences
                .microphone_device
                .as_ref()
                .map(|device| device.id.clone());
            if let Err(error) = track.validate_endpoint_selection() {
                self.commands = None;
                self.error = Some(format!(
                    "Microphone unavailable: {error}. Reselect the input device."
                ));
                cx.notify();
                if self.exit_after_proof { cx.quit(); }
                return;
            }
            audio.push(track);
        }
        let options = RunOptions {
            display: Some(self.preferences.display.as_ref().map_or(windows_capture::display::Selection::Primary,
                |saved| windows_capture::display::Selection::DevicePath(saved.id.clone()))),
            seconds: self.proof_seconds,
            preview: true,
            audio,
            record_to_disk: self.record_to_disk,
            suppress_microphone: self.suppress_microphone && self.microphone,
            output: recordings::root().join(format!("rcr-{stamp}.mkv")),
            replay_seconds: self.preferences.replay_seconds,
            video: media_core::VideoConfig {
                width: self.preferences.height * 16 / 9,
                height: self.preferences.height,
                fps: self.preferences.fps,
                bitrate: self.preferences.bitrate,
                keyframe_interval: self.preferences.capture_priority.keyframe_interval(self.preferences.fps),
            },
            ..Default::default()
        };
        self.session_profile = (
            options.video.height,
            options.video.fps,
            options.replay_seconds,
        );
        self.session_display = self.preferences.display.clone();
        self.stats.output = options.output.display().to_string();
        let worker = std::thread::Builder::new()
                .name("capture".into())
                .spawn(move || {
                    if let Err(error) =
                        pipeline::run_controlled(options, stop, Some(tx.clone()), Some(command_rx))
                    {
                        let _ = tx.send(PipelineEvent::Error(format!("{error:#}")));
                    }
                });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                self.commands = None;
                self.receiver = None;
                self.error = Some(format!("Could not start the capture worker: {error}"));
                cx.notify();
                if self.exit_after_proof { cx.quit(); }
                return;
            }
        };
        self.worker = Some(worker);
        self.receiver = Some(rx);
        self.active = true;
        cx.notify();
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let idle = self.update_blocker(cx).is_none();
        if self.updates.poll(Instant::now(), idle) { cx.notify(); }
        if self.tab != 3 && let Some(check) = &self.setup_inspection { check.cancel.cancel(); }
        let setup_result = self.setup_inspection.as_ref().and_then(|check| match check.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("Installation check stopped before returning a result. Try again.".into())),
        });
        if let Some(result) = setup_result {
            self.setup_inspection = None;
            match result {
                Ok(report) => { self.setup_report = Some(report); self.setup_error = None; }
                Err(error) => self.setup_error = Some(error),
            }
            cx.notify();
        }
        if let Some(ack) = preferences::take_layout_ack(&self.preferences)
            && ack.revision == self.layout_save_revision {
            self.layout_save_pending = false;
            if let Some(previous) = self.layout_save_error.take()
                && self.notice.as_ref() == Some(&previous) {
                self.notice = None;
            }
            if let Err(error) = ack.result {
                let message = format!("Library view changed, but could not be saved: {error}. Select List or Grid again to retry.");
                self.layout_save_error = Some(message.clone());
                if self.notice.is_none() { self.notice = Some(message); }
            }
            cx.notify();
        }
        if let Some(outcome) = self.library_scan.poll() {
            self.library.root_scan_error = outcome.apply(&mut self.files);
            self.library.invalidate_metadata();
            self.library.catalog_files(&self.files);
            self.sync_editor_choices(cx);
            cx.notify();
        }
        if self.library.poll() {
            self.sync_editor_choices(cx);
            cx.notify();
        }
        let tray_events = self
            .tray
            .as_ref()
            .map(|t| t.events.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for command in tray_events {
            match command.as_str() {
                "quick-record" => self.quick_recorder.update(cx, |recorder, cx| recorder.toggle(cx)),
                command if command.starts_with("screen-tool-") => {
                    self.tab = 4;
                    let mode = match command {
                        "screen-tool-color" => Some(screen_tools::Mode::Color),
                        "screen-tool-ruler" => Some(screen_tools::Mode::Ruler),
                        "screen-tool-pin" => Some(screen_tools::Mode::Pin),
                        "screen-tool-ocr" => Some(screen_tools::Mode::Ocr),
                        "screen-tool-qr" => Some(screen_tools::Mode::Qr),
                        _ => None,
                    };
                    if let Some(mode) = mode {
                        let toolkit = self.toolkit.clone();
                        if let Some(window) = cx
                            .windows()
                            .into_iter()
                            .find(|w| w.downcast::<Studio>().is_some())
                        {
                            let _ = window.update(cx, |_, window, cx| {
                                toolkit.update(cx, |t, cx| t.screen_utility(mode, window, cx))
                            });
                        }
                    } else if let Err(e) = screen_tools::monitor(cx) {
                        self.notice = Some(e.to_string());
                    }
                    cx.notify();
                }
                command if command == "utilities" || command.starts_with("utility-") => {
                    let tool = command
                        .strip_prefix("utility-")
                        .and_then(|s| s.parse::<usize>().ok())
                        .and_then(|i| ::toolkit::utilities::Tool::ALL.get(i).copied());
                    self.tab = 4;
                    self.toolkit.update(cx, |this, cx| this.utility(tool, cx));
                    if let Some(window) = cx
                        .windows()
                        .into_iter()
                        .find(|w| w.downcast::<Studio>().is_some())
                    {
                        let _ = window.update(cx, |_, w, _| w.activate_window());
                    }
                    cx.notify();
                }
                "open" | "library" | "settings" => {
                    if command == "library" {
                        self.cancel_hotkey_edit(cx);
                        self.tab = 1;
                    } else if command == "settings" {
                        self.tab = 3;
                    }
                    if let Some(window) = cx
                        .windows()
                        .into_iter()
                        .find(|w| w.downcast::<Studio>().is_some())
                    {
                        let _ = window.update(cx, |_, window, _| window.activate_window());
                    }
                    cx.notify();
                }
                "region" => self.quick_recorder.update(cx, |r, cx| r.launch(false, cx)),
                "fullscreen" => {
                    let target = if command == "region" {
                        ::toolkit::core::workflow::CaptureTarget::Region
                    } else {
                        ::toolkit::core::workflow::CaptureTarget::Fullscreen
                    };
                    if self.toolkit.update(cx, |t, cx| t.capture(target, cx))
                        && let Some(window) = cx
                            .windows()
                            .into_iter()
                            .find(|w| w.downcast::<Studio>().is_some())
                    {
                        let _ = window.update(cx, |_, window, _| window.minimize_window());
                    }
                }
                "replay" => {
                    if self.active {
                        self.save_replay(cx);
                    } else {
                        self.notice = Some("Arm the replay buffer before saving a replay.".into());
                        cx.notify();
                    }
                }
                "record" => self.toggle_record(cx),
                "toggle-replay" => self.toggle_replay(cx),
                "quit" => self.quit_with_update(false, cx),
                _ => {}
            }
        }
        let shortcuts = self
            .hotkeys
            .as_ref()
            .map(|h| h.events.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for event in shortcuts {
            match event {
                hotkeys::Event::ToggleReplay => self.toggle_replay(cx),
                hotkeys::Event::SaveReplay => {
                    if self.active {
                        self.save_replay(cx);
                    } else {
                        self.notice = Some("Arm the replay buffer before saving a replay.".into());
                    }
                }
                hotkeys::Event::ToggleRecord => self.toggle_record(cx),
                hotkeys::Event::ToggleQuickRecord => self.quick_recorder.update(cx, |recorder, cx| recorder.toggle(cx)),
                hotkeys::Event::Screenshot(::toolkit::core::workflow::CaptureTarget::Region) => self.quick_recorder.update(cx, |recorder, cx| recorder.launch(false, cx)),
                hotkeys::Event::Screenshot(target) => {
                    let started = self
                        .toolkit
                        .update(cx, |toolkit, cx| toolkit.capture(target, cx));
                    if started
                        && let Some(window) = cx
                            .windows()
                            .into_iter()
                            .find(|w| w.downcast::<Studio>().is_some())
                    {
                        let _ = window.update(cx, |_, window, _| window.minimize_window());
                    }
                }
                hotkeys::Event::Notice(message) => self.notice = Some(message),
            }
            cx.notify();
        }
        let batch = capture_retirement::poll(&mut self.worker, self.receiver.as_ref());
        let mut replay_outcomes = Vec::new();
        let mut replay_saved = false;
        for event in batch.events {
            match event {
                PipelineEvent::Started(surface) => match WindowsGpuSurface::new(surface.texture) {
                    Ok(surface) => self.surface = Some(surface),
                    Err(error) => {
                        self.preview_error = Some(error.to_string());
                    }
                },
                PipelineEvent::Stats(stats) => self.stats = stats,
                PipelineEvent::Finished(stats) => {
                    self.stats = stats;
                    self.capture_terminal_seen = true;
                    self.stop.store(true, Ordering::Relaxed);
                }
                PipelineEvent::Error(error) => {
                    self.error = Some(capture_error_message(error));
                    if self.exit_after_proof { PROOF_FAILED.store(true, Ordering::Relaxed); }
                    self.capture_terminal_seen = true;
                    self.stop.store(true, Ordering::Relaxed);
                }
                PipelineEvent::ReplaySaved(path) => {
                    self.saved_replay = Some(path);
                    replay_outcomes.push("Replay saved to your Library.".to_owned());
                    replay_saved = true;
                }
                PipelineEvent::ReplayPending(job) => {
                    self.pending_replays.push(job);
                    replay_outcomes.push("Capture stopped. Your replay is still saving in the background.".to_owned());
                }
                PipelineEvent::Notice(message) => replay_outcomes.push(message),
            }
            cx.notify();
        }
        let mut index = 0;
        while index < self.pending_replays.len() {
            if !self.pending_replays[index].is_finished() { index += 1; continue; }
            match self.pending_replays.swap_remove(index).complete() {
                Ok(path) => {
                    self.saved_replay = Some(path);
                    replay_outcomes.push("Replay saved to your Library.".to_owned());
                    replay_saved = true;
                }
                Err(error) => replay_outcomes.push(format!("Replay save failed: {error:#}")),
            }
        }
        if !replay_outcomes.is_empty() {
            // Two offline jobs may finish together. Keep every outcome so a
            // later success cannot overwrite a failure before it is displayed.
            self.notice = Some(replay_outcomes.join("\n"));
            if replay_saved { self.refresh_files(); }
            cx.notify();
        }
        if self.active && self.tab == 0 && self.surface.is_some() {
            cx.notify();
        }
        if let Some(joined) = batch.retired {
            if let Err(error) = capture_retirement::outcome(&mut self.capture_terminal_seen, joined) {
                self.error = Some(match self.error.take() {
                    Some(previous) => format!("{previous}\n\n{error}"),
                    None => error,
                });
                if self.exit_after_proof { PROOF_FAILED.store(true, Ordering::Relaxed); }
            }
            self.active = false;
            self.receiver.take();
            self.commands.take();
            cx.notify();
            let restart_recording = consume_recording_restart(&mut self.restart_recording, self.error.is_none() && !self.exit_after_proof);
            self.refresh_files();
            let draws = self.surface.as_ref().map_or(0, |s| s.draw_count());
            let preview_error = self.preview_error.clone().or_else(|| self.surface.as_ref().and_then(WindowsGpuSurface::error));
            let proof_ok = capture_proof_ready(&self.stats, self.error.is_some(), preview_error.is_some(), draws);
            let report = serde_json::json!({ "pipeline": self.stats, "gpu_preview_draws": draws,
                "error": self.error,
                "preview_error": preview_error,
                "proof_ok": self.exit_after_proof.then_some(proof_ok),
                "preview_path": "shared D3D11 BGRA -> keyed GPU copy -> private GPUI SRV" });
            let path = PathBuf::from(&self.stats.output).with_extension("preview.json");
            self.report_revision = self.report_revision.wrapping_add(1);
            let revision = self.report_revision;
            let should_exit = self.exit_after_proof;
            let receipt = diagnostics_io::write_report(path, serde_json::to_vec_pretty(&report).unwrap());
            cx.spawn(async move |view, cx| {
                let result = match receipt {
                    Ok(receipt) => match receipt.recv().with_timeout(Duration::from_secs(10), cx.background_executor()).await {
                        Ok(result) => result.unwrap_or_else(|_| Err("Diagnostic writer stopped before acknowledging the report".into())),
                        Err(_) => Err("The diagnostic write has not completed within 10 seconds; its worker still owns the pending write".into()),
                    },
                    Err(error) => Err(error),
                };
                if let Err(error) = &result { eprintln!("preview report: {error}"); }
                let _ = view.update(cx, |this, cx| {
                    if revision != this.report_revision { return; }
                    if let Err(error) = result {
                        if !this.active { this.notice = Some(format!("Capture finished; diagnostic report: {error}")); cx.notify(); }
                        if should_exit { PROOF_FAILED.store(true, Ordering::Relaxed); }
                    } else if should_exit && proof_ok {
                        PROOF_FAILED.store(false, Ordering::Relaxed);
                    }
                    if should_exit { cx.quit(); }
                });
            }).detach();
            println!("{report}");
            if restart_recording {
                self.record_to_disk = true;
                self.start(cx);
            }
        }
        let state = self.capture_state();
        let quick = self.quick_recorder.read(cx);
        if let Some(tray) = &mut self.tray {
            tray.update_quick_recorder(quick.recording(), quick.busy(), &self.preferences.hotkeys[6]);
        }
        if let Some(tray) = &mut self.tray
            && let Err(error) = tray.update(state, &self.preferences.hotkeys) {
            eprintln!("tray state: {error:#}");
        }
    }
    fn check_installation(&mut self, cx: &mut Context<Self>) {
        if self.setup_inspection.is_some() { return; }
        let config = setup_diagnostics::Config::capture(recordings::root());
        let cancel = ::toolkit::core::workflow::CancelToken::new();
        let worker_cancel = cancel.clone();
        let (sender, result) = mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new().name("rcr-installation-check".into()).spawn(move || {
            let report = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
                setup_diagnostics::collect(config, &worker_cancel)))
                .map_err(|_| "Installation check failed unexpectedly. Try again.".to_owned());
            let _ = sender.try_send(report);
        });
        self.setup_report = None;
        self.setup_error = None;
        match spawned {
            Ok(_) => self.setup_inspection = Some(SetupInspection { cancel, result }),
            Err(error) => self.setup_error = Some(format!("Could not start installation check: {error}")),
        }
        cx.notify();
    }
    fn save_preferences(&mut self) -> bool {
        self.preferences.system_audio = self.system_audio;
        self.preferences.microphone = self.microphone;
        self.preferences.clean_microphone = self.suppress_microphone;
        if let Err(error) = self.preferences.save() {
            self.notice = Some(format!("Could not save settings: {error}"));
            return false;
        }
        true
    }
    fn set_library_list_view(&mut self, list_view: bool, cx: &mut Context<Self>) {
        if self.preferences.list_view == list_view && self.layout_save_error.is_none() { return; }
        let queued = preferences::set_list_view(&self.preferences, list_view);
        // Layout is immediately useful even when settings storage is unavailable.
        self.preferences.list_view = list_view;
        match queued {
            Ok(revision) => {
                self.layout_save_revision = revision;
                self.layout_save_pending = true;
            }
            Err(error) => {
                self.layout_save_revision = 0;
                self.layout_save_pending = false;
                let message = format!("Could not save Library view: {error}. Restart RCR to retry saving.");
                self.layout_save_error = Some(message.clone());
                self.notice = Some(message);
            }
        }
        cx.notify();
    }
    fn capture_state(&self) -> tray::TrayState {
        tray::TrayState {
            active: self.active,
            stopping: self.stop.load(Ordering::Relaxed),
            recording: self.record_to_disk,
            replay_ready: self.stats.encoded > 0,
            replay_seconds: self.stats.replay_seconds.max(0.0).floor() as u64,
        }
    }
    fn toggle_record(&mut self, cx: &mut Context<Self>) {
        if self.active && self.stop.load(Ordering::Relaxed) {
            self.notice = Some("Finishing the current capture. Try again once it stops.".into());
            cx.notify();
            return;
        }
        if self.active {
            self.restart_recording = !self.record_to_disk;
            if self.restart_recording {
                self.notice = Some("Starting a recording; the replay buffer will restart.".into());
            }
            self.stop.store(true, Ordering::Relaxed);
        } else {
            self.record_to_disk = true;
            self.start(cx);
        }
        cx.notify();
    }
    fn save_replay(&mut self, cx: &mut Context<Self>) {
        if self.pending_replays.len() >= 2 {
            self.notice = Some("Two replays are still saving. Wait for one to finish before saving another.".into());
            cx.notify();
            return;
        }
        if !self.active || self.stop.load(Ordering::Relaxed) || self.stats.encoded == 0 {
            self.notice = Some(
                if !self.active {
                    "Turn on instant replay before saving a replay."
                } else if self.stop.load(Ordering::Relaxed) {
                    "Capture is finishing. Save a replay before stopping next time."
                } else {
                    "Replay is starting. Wait for the first frames before saving."
                }
                .into(),
            );
            cx.notify();
            return;
        }
        if let Some(commands) = &self.commands {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            match commands.try_send(replay_save_request(
                recordings::root().join(format!("replay-{stamp}.mkv")),
                self.session_profile.2,
                self.microphone,
                self.suppress_microphone,
            )) {
                Ok(()) => self.notice = Some("Saving replay in the background…".into()),
                Err(error) => self.notice = Some(format!("Replay request failed: {error}")),
            }
            cx.notify();
        }
    }
    fn toggle_replay(&mut self, cx: &mut Context<Self>) {
        if self.active && self.record_to_disk {
            self.notice =
                Some("Replay stays on during a full recording. Stop the recording first.".into());
        } else if self.active && self.stop.load(Ordering::Relaxed) {
            self.notice = Some("Finishing replay. Wait before starting another capture.".into());
        } else if self.active {
            self.stop.store(true, Ordering::Relaxed);
        } else {
            self.record_to_disk = false;
            self.start(cx);
        }
        cx.notify();
    }
}

#[cfg(windows)]
impl Drop for Studio {
    fn drop(&mut self) {
        if let Some(check) = &self.setup_inspection { check.cancel.cancel(); }
        self.library_scan.cancel();
        self.stop.store(true, Ordering::Relaxed);
        // Disconnect before joining: a worker's terminal event must not block UI shutdown.
        self.receiver.take();
        self.commands.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // Signal every offline job before joining any, so cancellation runs in
        // parallel and no accepted save outlives application shutdown.
        for job in &self.pending_replays { job.cancel(); }
        self.pending_replays.clear();
    }
}

fn main() {
    #[cfg(windows)]
    let startup_args: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    let normal_launch = !startup_args.iter().any(|arg| {
        arg.starts_with("--smoke-") || arg.starts_with("--eval-")
            || arg.starts_with("--diagnose") || arg == "--ui-snapshot"
            || arg == "--proof-seconds" || arg == "--exit-after-proof"
    });
    #[cfg(windows)]
    let _instance = if normal_launch {
        match single_instance::Guard::acquire() {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => {
                if !single_instance::activate_existing() {
                    installation_ui::startup_error("Kielbox is already running. Open it from the system tray, or quit the other copy before starting this one.");
                }
                return;
            }
            Err(error) => { installation_ui::startup_error(&error); std::process::exit(1); }
        }
    } else { None };
    #[cfg(windows)]
    if normal_launch {
        let installed_version = match updates::installed_version() {
            Ok(version) => version,
            Err(error) => { installation_ui::startup_error(&error); std::process::exit(1); }
        };
        match update_handoff::check_startup(installed_version.as_deref()) {
            Ok(Some(notice)) => { let _ = INSTALLATION_NOTICE.set(notice); }
            Ok(None) => {},
            Err(error) => { installation_ui::startup_error(&error); std::process::exit(1); }
        }
        match installation::initialize(installed_version.is_some(), &startup_args) {
            Ok(state) => { let _ = INSTALLATION.set(state); }
            Err(error) => { installation_ui::startup_error(&error); std::process::exit(1); }
        }
    }
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        let _ = std::fs::create_dir_all("projects");
        if let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("projects/crash.log")
        {
            let _ = writeln!(
                log,
                "\n{:?}\n{info}\n{}",
                SystemTime::now(),
                std::backtrace::Backtrace::force_capture()
            );
        }
        previous_hook(info);
    }));
    #[cfg(windows)]
    {
        let args: Vec<_> = std::env::args().collect();
        if args.iter().any(|arg| matches!(arg.as_str(), "--smoke-installed-status" | "--smoke-installed-update" | "--smoke-installed-check")) {
            if let Err(error) = installer_smoke::run(&args) {
                eprintln!("Installed-app proof failed: {error}");
                std::process::exit(1);
            }
            return;
        }
        if args.iter().any(|arg| arg == "--diagnose") {
            let directory = args.windows(2).find(|pair| pair[0] == "--data-folder")
                .map(|pair| PathBuf::from(&pair[1])).unwrap_or_else(|| PathBuf::from("recordings"));
            let report = setup_diagnostics::collect(setup_diagnostics::Config::capture(directory),
                &::toolkit::core::workflow::CancelToken::new());
            use std::io::Write as _;
            let bytes = if args.iter().any(|arg| arg == "--human") {
                Some(report.human_summary().into_bytes())
            } else { serde_json::to_vec_pretty(&report).ok() };
            let printed = bytes.is_some_and(|mut bytes| {
                bytes.push(b'\n');
                std::io::stdout().lock().write_all(&bytes).is_ok()
            });
            if !printed || report.has_blocking_issues() { std::process::exit(1); }
            return;
        }
        if args.iter().any(|arg| arg == "--eval-hardware-stop") {
            let output = args.windows(2).find(|pair| pair[0] == "--proof-output")
                .map(|pair| PathBuf::from(&pair[1]))
                .unwrap_or_else(|| PathBuf::from(format!(".downloads/hardware-stop-{}",
                    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos())));
            let failed = Arc::new(AtomicBool::new(false));
            let result = failed.clone();
            Application::new().with_assets(theme::Assets).run(move |cx| {
                if let Err(error) = hardware_stop_proof::run(output.clone(), result.clone(), cx) {
                    eprintln!("Desktop hardware Stop proof initialization: {error:#}");
                    result.store(true, Ordering::Release);
                    cx.quit();
                }
            });
            if failed.load(Ordering::Acquire) { std::process::exit(1); }
            return;
        }
        if args.iter().any(|arg| arg == "--smoke-capture-menu") {
            let original = std::env::current_dir().expect("working directory");
            let output = args.windows(2).find(|pair| pair[0] == "--proof-output").map(|pair| PathBuf::from(&pair[1]))
                .unwrap_or_else(|| original.join(".downloads/capture-menu-proof"));
            let output = if output.is_absolute() { output } else { original.join(output) };
            let profile = tempfile::Builder::new().prefix("rcr-menu-profile-").tempdir().expect("isolated profile");
            std::env::set_current_dir(profile.path()).expect("isolate profile");
            let failed = Arc::new(AtomicBool::new(false)); let result = failed.clone();
            Application::new().with_assets(theme::Assets).run(move |cx| {
                theme::init(cx); text_input::init(cx); editor::init(cx);
                if let Err(error) = quick_recorder::menu_proof::run(output.clone(), result.clone(), cx) {
                    eprintln!("Capture menu proof: {error:#}"); result.store(true, Ordering::Relaxed); cx.quit();
                }
            });
            let _ = std::env::set_current_dir(original);
            if failed.load(Ordering::Relaxed) { std::process::exit(1); }
            return;
        }
        if args.iter().any(|arg| arg == "--smoke-quick-recorder") {
            let output = args.windows(2).find(|pair| pair[0] == "--proof-output").map(|pair| PathBuf::from(&pair[1]))
                .unwrap_or_else(|| PathBuf::from(format!(".downloads/quick-recorder-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos())));
            let failed = Arc::new(AtomicBool::new(false));
            let result = failed.clone();
            Application::new().with_assets(theme::Assets).run(move |cx| {
                theme::init(cx);
                if let Err(error) = quick_recorder_proof::run(output.clone(), result.clone(), cx) {
                    eprintln!("Quick recorder proof: {error:#}"); result.store(true, Ordering::Relaxed); cx.quit();
                }
            });
            if failed.load(Ordering::Relaxed) { std::process::exit(1); }
            return;
        }
        if args.iter().any(|arg| arg == "--smoke-atlas-churn") {
            let require_reuse = args.iter().any(|arg| arg == "--require-atlas-reuse");
            let video_preview = args.iter().any(|arg| arg == "--atlas-video-preview");
            let failed = Arc::new(AtomicBool::new(false));
            let result = failed.clone();
            Application::new().with_assets(theme::Assets).run(move |cx| {
                if let Err(error) = atlas_churn_proof::run(PathBuf::from(".downloads/research/atlas-churn"), require_reuse, video_preview, result.clone(), cx) {
                    eprintln!("Atlas churn proof initialization failed: {error:#}");
                    result.store(true, Ordering::Relaxed); cx.quit();
                }
            });
            if failed.load(Ordering::Relaxed) { std::process::exit(1); }
            return;
        }
        if args.iter().any(|arg| arg == "--smoke-gpu-surface") {
            let output = args
                .windows(2)
                .find(|arg| arg[0] == "--surface-proof-output")
                .map(|arg| PathBuf::from(&arg[1]))
                .unwrap_or_else(|| PathBuf::from(".downloads/research/gpu-surface-proof"));
            let failed = Arc::new(AtomicBool::new(false));
            let result = failed.clone();
            Application::new()
                .with_assets(theme::Assets)
                .run(move |cx| {
                    if let Err(error) = gpu_surface_proof::run(output, result.clone(), cx) {
                        eprintln!("GPU surface proof initialization failed: {error:#}");
                        result.store(true, Ordering::Relaxed);
                        cx.quit();
                    }
                });
            if failed.load(Ordering::Relaxed) {
                std::process::exit(1);
            }
            return;
        }
        let proof_seconds = args
            .windows(2)
            .find(|a| a[0] == "--proof-seconds")
            .and_then(|a| a[1].parse::<f64>().ok());
        let exit_after_proof = args.iter().any(|a| a == "--exit-after-proof");
        // Early closure or a missing receipt must never count as proof success.
        PROOF_FAILED.store(exit_after_proof, Ordering::Relaxed);
        let open = args
            .windows(2)
            .find(|a| a[0] == "--open")
            .map(|a| PathBuf::from(&a[1]));
        let smoke_toolkit = args.iter().any(|a| a == "--smoke-toolkit");
        let analyze_folder = args.windows(2).find(|a| a[0] == "--analyze-folder")
            .map(|a| PathBuf::from(&a[1]));
        let smoke_history = args.iter().any(|a| a == "--smoke-history");
        let smoke_audio_picker = args.iter().any(|a| a == "--smoke-audio-picker");
        let smoke_audio_devices = args.iter().any(|a| a == "--smoke-audio-devices");
        let smoke_library_selection = args.iter().any(|a| a == "--smoke-library-selection");
        let smoke_library = smoke_library_selection || args.iter().any(|a| a == "--smoke-library");
        let smoke_library_import = args.iter().any(|a| a == "--smoke-library-import");
        let smoke_recovery = args.iter().any(|a| a == "--smoke-recovery");
        let smoke_editor_native_fullscreen = args.iter().any(|a| a == "--smoke-editor-native-fullscreen");
        let smoke_editor_keyboard = smoke_editor_native_fullscreen || args.iter().any(|a| a == "--smoke-editor-keyboard");
        let eval_clip_show = args.iter().any(|a| a == "--eval-clip-show");
        let smoke_editor_resources = args.iter().any(|a| a == "--smoke-editor-resources");
        let smoke_gpu_eof = args.iter().any(|a| a == "--smoke-gpu-playback-eof");
        let smoke_gpu_automatic = args.iter().any(|a| a == "--smoke-gpu-playback-automatic");
        let smoke_gpu_playback = smoke_gpu_eof || smoke_gpu_automatic || args.iter().any(|a| a == "--smoke-gpu-playback");
        let smoke_autosave = args.iter().any(|a| a == "--smoke-autosave");
        let snapshot = args
            .windows(2)
            .find(|a| a[0] == "--ui-snapshot")
            .map(|a| a[1].clone());
        let snapshot_output = args
            .windows(2)
            .find(|a| a[0] == "--snapshot-output")
            .map(|a| PathBuf::from(&a[1]));
        let watch_share_stage = args.windows(2).find(|pair| pair[0] == "--watch-share-state")
            .and_then(|pair| pair[1].parse::<u8>().ok()).filter(|stage| *stage <= 3);
        if args.iter().any(|arg| arg == "--watch-share-state")
            && (watch_share_stage.is_none() || snapshot.as_deref() != Some("editor-clip-show")) {
            eprintln!("--watch-share-state requires 0..3 and --ui-snapshot editor-clip-show");
            std::process::exit(1);
        }
        let installation_preview = args.iter().any(|arg| arg == "--installation-preview");
        if installation_preview && snapshot.as_deref() != Some("settings") {
            eprintln!("--installation-preview requires --ui-snapshot settings");
            std::process::exit(1);
        }
        let updates_preview = args.windows(2).find(|pair| pair[0] == "--updates-preview").map(|pair| match pair[1].as_str() {
            "not-installed" => 0, "idle" => 1, "checking" => 2, "available" => 3,
            "downloading" => 4, "ready" => 5, "error" => 6, "welcome" => 7,
            _ => 255,
        });
        if updates_preview == Some(255) || (updates_preview.is_some() && snapshot.is_none()) {
            eprintln!("--updates-preview requires --ui-snapshot and a valid presentation state");
            std::process::exit(1);
        }
        let fixture_options = ui_proof::FixtureOptions { watch_share_stage, installation_preview, updates_preview };
        if let Some(route) = snapshot.as_deref() {
            if !matches!(
                route,
                "capture"
                    | "capture-saved"
                    | "capture-starting-recording"
                    | "capture-recording"
                    | "capture-finishing"
                    | "capture-audio-error"
                    | "capture-audio-error-stopped"
                    | "capture-display-picker"
                    | "settings-capture-active"
                    | "capture-audio-picker"
                    | "capture-microphone-devices"
                    | "settings-audio-devices"
                    | "capture-preview-unavailable"
                    | "library"
                    | "library-selected"
                    | "library-refreshing"
                    | "library-folder-unavailable"
                    | "library-layout-saving"
                    | "library-layout-failed"
                    | "library-trash"
                    | "library-trash-warnings"
                    | "editor"
                    | "editor-clip-show"
                    | "editor-source-recovery"
                    | "editor-source-recovery-dismissed"
                    | "editor-export-failed"
                    | "editor-playback-fallback"
                    | "editor-playback-setting-pending"
                    | "settings"
                    | "settings-shortcuts"
                    | "tools"
                    | "tools-workflows"
                    | "tools-destinations"
                    | "tools-history"
                    | "tools-history-fixture"
                    | "tools-preferences"
                    | "tools-utilities"
                    | "tools-disk-usage"
                    | "tools-disk-usage-map"
                    | "tools-disk-usage-files"
                    | "tools-disk-usage-explore"
                    | "tools-disk-usage-explore-boxes"
                    | "tools-image-retry"
                    | "tools-image-blur"
                    | "tools-image-pixelate"
                    | "tools-image-magnifier"
                    | "overlay-region"
                    | "overlay-region-tools"
                    | "overlay-hover-region"
                    | "overlay-blur-draft"
                    | "overlay-pixelate-draft"
                    | "overlay-properties"
                    | "overlay-selected-properties"
                    | "overlay-retry"
            ) {
                eprintln!("Unknown UI snapshot route: {route}");
                std::process::exit(2);
            }
            if matches!(route, "editor" | "editor-clip-show") && open.is_none() {
                eprintln!("Editor snapshots require --open <recording>");
                std::process::exit(2);
            }
        }
        let snapshot_size = args
            .windows(2)
            .find(|a| a[0] == "--snapshot-size")
            .and_then(|a| a[1].split_once('x'))
            .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
            .filter(|(w, h)| (900..=3840).contains(w) && (600..=2160).contains(h))
            .unwrap_or((1440, 900));
        let smoke_trim = args.iter().any(|a| a == "--smoke-trim");
        let smoke_zoom = args.iter().any(|a| a == "--smoke-zoom");
        let smoke_random = args.iter().any(|a| a == "--smoke-random");
        let smoke_pressure_cold = args.iter().any(|a| a == "--smoke-scrub-pressure-cold");
        let smoke_pressure = smoke_pressure_cold || args.iter().any(|a| a == "--smoke-scrub-pressure");
        let smoke_scrub = smoke_random || smoke_pressure || args.iter().any(|a| a == "--smoke-scrub");
        let smoke = snapshot.is_some()
            || smoke_toolkit
            || smoke_history
            || smoke_audio_picker
            || smoke_audio_devices
            || smoke_library
            || smoke_library_import
            || smoke_recovery
            || smoke_editor_keyboard
            || eval_clip_show
            || smoke_editor_resources
            || smoke_gpu_playback
            || smoke_autosave
            || smoke_trim
            || smoke_zoom
            || smoke_scrub
            || args.iter().any(|a| a == "--smoke-open");
        if smoke
            && snapshot.is_none()
            && !smoke_toolkit
            && !smoke_history
            && !smoke_audio_picker
            && !smoke_audio_devices
            && !smoke_library
            && open.is_none()
        {
            eprintln!("Editor smoke fixtures require --open <recording>");
            std::process::exit(2);
        }
        if smoke_library_import && open.is_none() {
            eprintln!("--smoke-library-import requires --open <recording>");
            std::process::exit(2);
        }
        let smoke_recording = if smoke_library_import || smoke_gpu_playback || eval_clip_show {
            Some(
                open.as_ref()
                    .expect("owned fixture source")
                    .canonicalize()
                    .expect("owned fixture source must be available"),
            )
        } else {
            open.clone()
        };
        let fixture_kind = if eval_clip_show {
            Some("clip-show-eval")
        } else if smoke_library_import {
            Some("library-import")
        } else if smoke_gpu_playback {
            Some("gpu-playback")
        } else if smoke_audio_devices {
            Some("audio-devices")
        } else if smoke_history {
            Some("history")
        } else if smoke_library_selection {
            Some("library-selection")
        } else {
            None
        };
        let owned_fixture = if let Some(kind) = fixture_kind {
            let base = std::env::current_dir()
                .expect("fixture working directory")
                .join(".downloads");
            std::fs::create_dir_all(&base).expect("create fixture directory");
            let guard = tempfile::Builder::new()
                .prefix(&format!("{kind}-smoke-"))
                .tempdir_in(base)
                .expect("create isolated native fixture");
            std::fs::write(
                guard.path().join(format!(".rcr-{kind}-smoke")),
                format!("rcr-{kind}-smoke-v1\n"),
            )
            .expect("mark owned native fixture");
            std::env::set_current_dir(guard.path()).expect("enter isolated native fixture");
            if smoke_library_selection {
                std::fs::create_dir_all("recordings").expect("create owned selection fixtures");
                for (name, color) in [
                    ("Selection A.png", [30, 90, 160, 255]),
                    ("Selection B.png", [70, 160, 100, 255]),
                ] {
                    image::RgbaImage::from_pixel(64, 36, image::Rgba(color))
                        .save(std::path::Path::new("recordings").join(name))
                        .expect("save owned synthetic selection image");
                }
            }
            // Startup only: no App, worker or preference reader exists yet.
            unsafe {
                std::env::set_var(
                    "RCR_THUMBNAIL_CACHE_DIR",
                    guard.path().join("cache/thumbnails"),
                );
                std::env::set_var("RCR_SEEK_CACHE_DIR", guard.path().join("cache/seek"));
            }
            // Keep this process isolated through exit, including detached worker cleanup.
            // The caller may remove this owned artifact after the process has exited.
            Some(guard.keep())
        } else {
            None
        };
        let smoke_failed = Arc::new(AtomicBool::new(false));
        let smoke_result = smoke_failed.clone();
        let mut editor_resource_probe = editor::EditorResourceSmoke::default();
        if smoke_editor_resources { println!("Editor resources startup: {}", memory_usage::snapshot()); }
        Application::new()
            .with_assets(theme::Assets)
            .run(move |cx: &mut App| {
                theme::init(cx);
                text_input::init(cx);
                editor::init(cx);
                cx.on_window_closed(|cx| {
                    if cx.windows().is_empty() {
                        cx.quit();
                    }
                })
                .detach();
                let bounds = Bounds::centered(
                    None,
                    size(px(snapshot_size.0 as f32), px(snapshot_size.1 as f32)),
                    cx,
                );
                let window = cx
                    .open_window(
                        WindowOptions {
                            show: !smoke,
                            focus: !smoke,
                            window_bounds: Some(WindowBounds::Windowed(bounds)),
                            window_min_size: Some(size(
                                px(if snapshot.is_some() || eval_clip_show { 900. } else { 1100. }),
                                px(if snapshot.is_some() || eval_clip_show { 600. } else { 680. }),
                            )),
                            titlebar: Some(TitlebarOptions {
                                title: Some("Kielbox".into()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        |window, cx| {
                            cx.new(|cx| {
                                let mut studio =
                                    Studio::new(proof_seconds, exit_after_proof, smoke, cx);
                                if smoke_gpu_automatic {
                                    // Exercise the same configuration as the Settings
                                    // control without writing fixture preferences.
                                    studio.preferences.gpu_video_preview = true;
                                    studio.editor.update(cx, |editor, cx| {
                                        editor.set_gpu_preview_enabled(true, cx);
                                    });
                                }
                                if let Some(screen) = &snapshot {
                                    studio.tab = match screen.as_str() {
                                        "capture" | "capture-saved" | "capture-audio-picker" | "capture-microphone-devices"
                                        | "capture-audio-error" | "capture-audio-error-stopped" | "capture-display-picker" => 0,
                                        "settings-audio-devices" | "settings-capture-active" => 3,
                                        "library"
                                        | "library-selected"
                                        | "library-refreshing"
                                        | "library-folder-unavailable"
                                        | "library-layout-saving"
                                        | "library-layout-failed"
                                        | "library-trash"
                                        | "library-trash-warnings" => 1,
                                        "editor"
                                        | "editor-clip-show"
                                        | "editor-source-recovery"
                                        | "editor-source-recovery-dismissed"
                                        | "editor-export-failed"
                                        | "editor-playback-fallback"
                                        | "editor-playback-setting-pending" => 2,
                                        "settings" | "settings-shortcuts" => 3,
                                        "tools" => 4,
                                        _ => 0,
                                    };
                                    if screen.starts_with("tools-") {
                                        studio.tab = 4;
                                    }
                                    if screen == "capture-audio-picker" {
                                        studio.audio_picker_fixture = true;
                                        studio.open_audio_picker(cx);
                                    }
                                    if matches!(screen.as_str(), "capture-microphone-devices" | "settings-audio-devices") {
                                        studio.preferences.microphone_device = Some(preferences::SavedAudioEndpoint {
                                            id: "offline-owned-preview-microphone".into(),
                                            label: "Studio microphone (disconnected)".into(),
                                        });
                                        studio.open_audio_device_picker(windows_capture::audio_endpoint::AudioEndpointKind::Capture, cx);
                                    }
                                    if screen == "capture-preview-unavailable" {
                                        studio.preview_error = Some("The capture and interface use different GPUs. Shared-texture preview requires the same GPU.".into());
                                        studio.active = true;
                                        studio.record_to_disk = false;
                                    }
                                    if screen == "capture-display-picker" {
                                        studio.preferences.display = Some(preferences::SavedDisplay {
                                            id: "fixture-missing-display".into(), label: "Fixture disconnected monitor".into(),
                                        });
                                        studio.open_display_picker(cx);
                                    }
                                    if matches!(screen.as_str(), "capture-starting-recording" | "capture-recording" | "capture-finishing" | "settings-capture-active") {
                                        // Presentation-only fixture: no capture worker, encoder,
                                        // endpoint activation, settings write or output file.
                                        studio.active = true;
                                        studio.record_to_disk = true;
                                        studio.microphone = true;
                                        studio.suppress_microphone = true;
                                        if screen != "capture-starting-recording" {
                                            studio.stats.encoded = 120;
                                            studio.stats.elapsed_seconds = 2.;
                                            studio.stats.replay_seconds = 2.;
                                            studio.stats.compressed_bytes = 1_000_000;
                                        }
                                        if screen == "capture-finishing" { studio.stop.store(true, Ordering::Relaxed); }
                                    }
                                    if screen.starts_with("library-trash") {
                                        studio.snapshot_library_trash(
                                            screen.ends_with("-warnings"),
                                            cx,
                                        );
                                    }
                                    if matches!(screen.as_str(), "library-refreshing" | "library-folder-unavailable") {
                                        studio.snapshot_library_scan(screen == "library-folder-unavailable", cx);
                                    }
                                    if screen == "editor-export-failed" {
                                        studio.editor.update(cx, |editor, cx| {
                                            editor.seed_export_failure_preview(window, cx)
                                        });
                                    }
                                    if matches!(screen.as_str(), "editor-playback-fallback" | "editor-playback-setting-pending") {
                                        studio.editor.update(cx, |editor, cx| {
                                            editor.snapshot_playback_details(screen == "editor-playback-fallback", window, cx)
                                        });
                                    }
                                    if matches!(screen.as_str(), "editor" | "editor-clip-show")
                                        && let Some(path) = &open
                                    {
                                        if screen == "editor-clip-show" {
                                            studio.watch_recording(path.clone(), cx);
                                        } else {
                                            studio.open_recording(path.clone(), cx);
                                        }
                                    }
                                    if screen.starts_with("editor-source-recovery") {
                                        studio.editor.update(cx, |editor, cx| {
                                            editor.snapshot_source_recovery(cx)
                                        });
                                        if screen.ends_with("-dismissed") {
                                            studio.editor.update(cx, |editor, cx| {
                                                editor.dismiss_snapshot_notice(cx)
                                            });
                                        }
                                    }
                                } else if smoke {
                                    studio.tab = 1;
                                } else if let Some(path) = open {
                                    studio.open_recording(path, cx);
                                }
                                if let Some(path) = &analyze_folder {
                                    studio.tab = 4;
                                    studio.toolkit.update(cx, |toolkit, cx| toolkit.analyze_folder(path.clone(), cx));
                                }
                                studio
                            })
                        },
                    )
                    .expect("create GPUI window");
                if !smoke && proof_seconds.is_none() {
                    let _ = window.update(cx, |studio, window, cx| {
                        cx.observe_window_activation(window, |studio, window, cx| {
                            if studio.editor_suspended && window.is_window_active() {
                                studio.editor_suspended = false;
                                if studio.tab == 2 { studio.editor.update(cx, |editor, cx| editor.show(cx)); }
                                cx.notify();
                            }
                        }).detach();
                        match tray::Tray::new() {
                            Ok(tray) => studio.tray = Some(tray),
                            Err(e) => studio.notice = Some(format!("Tray unavailable: {e}")),
                        };
                        let root = cx.entity().downgrade();
                        window.on_window_should_close(cx, move |window, cx| {
                            let keep = root.upgrade().is_some_and(|studio| {
                                let studio = studio.read(cx);
                                studio.preferences.close_to_tray && studio.tray.is_some()
                            });
                            if keep {
                                if let Some(studio) = root.upgrade() {
                                    studio.update(cx, |studio, cx| {
                                        studio.editor_suspended = true;
                                        studio.editor.update(cx, |editor, cx| editor.hide(cx));
                                        cx.notify();
                                    });
                                }
                                window.minimize_window();
                                false
                            } else {
                                if let Some(studio) = root.upgrade() {
                                    studio.update(cx, |studio, cx| studio.queue_install_on_exit(false, cx));
                                }
                                true
                            }
                        });
                    });
                }
                if let Some(screen) = snapshot {
                    let output = snapshot_output.unwrap_or_else(|| {
                        PathBuf::from(format!(".downloads/research/rcr-{screen}.png"))
                    });
                    let requested = size(px(snapshot_size.0 as f32), px(snapshot_size.1 as f32));
                    if screen.starts_with("overlay-") {
                        if let Err(error) = capture_overlay::capture_fixture(
                            screen,
                            output,
                            smoke_result.clone(),
                            requested,
                            cx,
                        ) {
                            eprintln!("Overlay snapshot failed: {error:#}");
                            smoke_result.store(true, Ordering::Relaxed);
                            cx.quit();
                        }
                    } else {
                        ui_proof::capture(window, screen, output, smoke_result, requested, fixture_options, cx);
                    }
                } else if smoke {
                    let window: AnyWindowHandle = window.into();
                    // Pressure tests must exercise their requested viewport;
                    // the other fixtures retain their established dimensions.
                    let pressure_size = if smoke_pressure || smoke_editor_resources || eval_clip_show { snapshot_size } else { (1440, 900) };
                    let _ = window.update(cx, |_, window, _| {
                        window.resize(size(px(pressure_size.0 as f32), px(pressure_size.1 as f32)));
                    });
                    cx.spawn(async move |cx| {
                        let start = std::time::Instant::now();
                        let mut ready = false;
                        let mut toolkit_stage = 0;
                        let mut audio_picker_probe = audio_source_picker::PickerSmoke::default();
                        let mut audio_device_probe = audio_device_picker::DevicePickerSmoke::default();
                        let mut library_import_probe = library::LibraryImportSmoke::default();
                        let mut recovery_probe = editor::RecoverySmoke::default();
                        let mut editor_keyboard_probe = if smoke_editor_native_fullscreen {
                            editor::EditorKeyboardSmoke::native_fullscreen()
                        } else { editor::EditorKeyboardSmoke::default() };
                        let mut clip_show_probe = editor::ClipShowEvaluation::default();
                        let mut gpu_playback_probe = editor::GpuPlaybackSmoke::default();
                        gpu_playback_probe.until_eof = smoke_gpu_eof;
                        let mut autosave_probe = editor::AutosaveSmoke::default();
                        let mut resource_source_opened = false;
                        let mut scrub_probe = editor::ScrubSmoke::default();
                        let mut pressure_probe = editor::ScrubPressureSmoke::default();
                        pressure_probe.cold = smoke_pressure_cold;
                        let mut history_probe = toolkit::HistorySmoke::default();
                        scrub_probe.random = smoke_random;
                        for attempt in 0..if eval_clip_show {
                            4000
                        } else if smoke_random {
                            6000
                        } else if smoke_pressure {
                            25000
                        } else if smoke_library_import {
                            3000
                        } else {
                            750
                        } {
                            if eval_clip_show && start.elapsed() >= Duration::from_secs(60) {
                                eprintln!("Clip show evaluation exceeded its 60-second budget");
                                smoke_result.store(true, Ordering::Relaxed);
                                break;
                            }
                            if smoke_pressure && start.elapsed() >= Duration::from_secs(22) {
                                eprintln!("Sustained scrub pressure exceeded its 22-second wall-clock budget");
                                smoke_result.store(true, Ordering::Relaxed);
                                break;
                            }
                            let requested_timer = Duration::from_millis(if smoke_scrub || smoke_gpu_eof { 1 } else { 16 });
                            let timer_started = Instant::now();
                            let timer_completed = if smoke_gpu_eof {
                                // Observe the timer on its background continuation,
                                // before foreground dispatch adds its own delay.
                                let executor = cx.background_executor().clone();
                                cx.background_executor().spawn(async move {
                                    executor.timer(requested_timer).await;
                                    Instant::now()
                                }).await
                            } else {
                                cx.background_executor().timer(requested_timer).await;
                                Instant::now()
                            };
                            ready = window
                                .update(cx, |root, window, cx| {
                                    let foreground_entered = Instant::now();
                                    let studio = root.downcast::<Studio>().expect("Studio root");
                                    if eval_clip_show {
                                        studio.update(cx, |this, _| this.tab = if clip_show_probe.hidden() { 0 } else { 2 });
                                        window.draw(cx).clear();
                                        if let Err(error) = window.present_for_diagnostics() {
                                            eprintln!("Clip show evaluation presentation failed: {error:#}");
                                            smoke_result.store(true, Ordering::Relaxed);
                                            return false;
                                        }
                                        let editor = studio.read(cx).editor.clone();
                                        let evaluated = editor::EditorView::evaluate_clip_show(
                                            editor, &mut clip_show_probe,
                                            smoke_recording.as_deref().expect("evaluation source"), window, cx,
                                        );
                                        studio.update(cx, |this, cx| {
                                            this.tab = if clip_show_probe.hidden() { 0 } else { 2 };
                                            cx.notify();
                                        });
                                        return match evaluated {
                                            Ok(ready) => ready,
                                            Err(error) => {
                                                eprintln!("Clip show evaluation failed: {error:#}");
                                                smoke_result.store(true, Ordering::Relaxed);
                                                false
                                            }
                                        };
                                    }
                                    if smoke_editor_resources {
                                        if !resource_source_opened {
                                            studio.update(cx, |this, _| this.tab = 0);
                                            window.draw(cx).clear();
                                            if let Err(error) = window.present_for_diagnostics() {
                                                eprintln!("Editor resource presentation failed: {error:#}");
                                                smoke_result.store(true, Ordering::Relaxed);
                                                return false;
                                            }
                                            println!("Editor resources idle before open: {}", memory_usage::snapshot());
                                            studio.update(cx, |this, cx| this.open_recording(smoke_recording.clone().expect("resource fixture source"), cx));
                                            resource_source_opened = true;
                                        }
                                        studio.update(cx, |this, _| this.tab = if editor_resource_probe.hidden() { 0 } else { 2 });
                                        window.draw(cx).clear();
                                        // Hidden fixtures must submit their scene just like a visible
                                        // window; draw alone leaves atlas uploads queued in D3D.
                                        if let Err(error) = window.present_for_diagnostics() {
                                            eprintln!("Editor resource presentation failed: {error:#}");
                                            smoke_result.store(true, Ordering::Relaxed);
                                            return false;
                                        }
                                        let editor = studio.read(cx).editor.clone();
                                        let result = editor::EditorView::smoke_resources(editor, &mut editor_resource_probe, window, cx);
                                        studio.update(cx, |this, cx| {
                                            this.tab = if editor_resource_probe.hidden() { 0 } else { 2 };
                                            cx.notify();
                                        });
                                        return match result {
                                            Ok(ready) => ready,
                                            Err(error) => { eprintln!("Editor resource smoke failed: {error:#}"); smoke_result.store(true, Ordering::Relaxed); false }
                                        };
                                    }
                                    if smoke_history {
                                        let toolkit = studio.read(cx).toolkit.clone();
                                        studio.update(cx, |this, _| this.tab = 4);
                                        return match toolkit::Toolkit::smoke_history(toolkit, &mut history_probe, window, cx) {
                                            Ok(ready) => ready,
                                            Err(error) => {
                                                eprintln!("History smoke failed: {error:#}");
                                                smoke_result.store(true, Ordering::Relaxed);
                                                false
                                            }
                                        };
                                    }
                                    if smoke_audio_devices {
                                        return match audio_device_picker::smoke(studio, &mut audio_device_probe, window, cx) {
                                            Ok(ready) => ready,
                                            Err(error) => {
                                                eprintln!("Audio device picker smoke failed: {error:#}");
                                                smoke_result.store(true, Ordering::Relaxed);
                                                false
                                            }
                                        };
                                    }
                                    if smoke_audio_picker {
                                        return match audio_source_picker::smoke(
                                            studio,
                                            &mut audio_picker_probe,
                                            window,
                                            cx,
                                        ) {
                                            Ok(ready) => ready,
                                            Err(error) => {
                                                eprintln!("Audio picker smoke failed: {error:#}");
                                                smoke_result.store(true, Ordering::Relaxed);
                                                false
                                            }
                                        };
                                    }
                                    if smoke_library_import {
                                        return match library::Library::smoke_import(
                                            studio,
                                            &mut library_import_probe,
                                            smoke_recording.as_deref().expect("import source"),
                                            window,
                                            cx,
                                        ) {
                                            Ok(ready) => ready,
                                            Err(error) => {
                                                eprintln!("Library import smoke failed: {error:#}");
                                                smoke_result.store(true, Ordering::Relaxed);
                                                false
                                            }
                                        };
                                    }
                                    if smoke_library {
                                        return match library::Library::smoke_keyboard(
                                            studio, window, cx,
                                        ) {
                                            Ok(ready) => ready,
                                            Err(error) => {
                                                eprintln!(
                                                    "Library keyboard smoke failed: {error:#}"
                                                );
                                                smoke_result.store(true, Ordering::Relaxed);
                                                false
                                            }
                                        };
                                    }
                                    if smoke_toolkit {
                                        studio.update(cx, |studio, cx| {
                                            if studio.tab != 4 {
                                                studio.tab = 4;
                                                cx.notify();
                                            }
                                        });
                                        let toolkit = studio.read(cx).toolkit.clone();
                                        return match toolkit::Toolkit::smoke_step(
                                            toolkit,
                                            &mut toolkit_stage,
                                            window,
                                            cx,
                                        ) {
                                            Ok(ready) => ready,
                                            Err(e) => {
                                                eprintln!("Toolkit smoke failed: {e:#}");
                                                smoke_result.store(true, Ordering::Relaxed);
                                                false
                                            }
                                        };
                                    }
                                    if smoke_recovery || smoke_editor_keyboard || smoke_autosave {
                                        studio.update(cx, |studio, cx| {
                                            if studio.tab != 2 {
                                                studio.tab = 2;
                                                cx.notify();
                                            }
                                        });
                                        let editor = studio.read(cx).editor.clone();
                                        if smoke_autosave {
                                            return match editor::EditorView::smoke_autosave(
                                                editor,
                                                &mut autosave_probe,
                                                smoke_recording
                                                    .as_deref()
                                                    .expect("autosave source"),
                                                window,
                                                cx,
                                            ) {
                                                Ok(ready) => ready,
                                                Err(error) => {
                                                    eprintln!("Autosave smoke failed: {error:#}");
                                                    smoke_result.store(true, Ordering::Relaxed);
                                                    false
                                                }
                                            };
                                        }
                                        if smoke_editor_keyboard {
                                            return match editor::EditorView::smoke_keyboard(
                                                editor,
                                                &mut editor_keyboard_probe,
                                                smoke_recording
                                                    .as_deref()
                                                    .expect("keyboard source"),
                                                window,
                                                cx,
                                            ) {
                                                Ok(ready) => ready,
                                                Err(error) => {
                                                    eprintln!(
                                                        "Editor keyboard smoke failed: {error:#}"
                                                    );
                                                    smoke_result.store(true, Ordering::Relaxed);
                                                    false
                                                }
                                            };
                                        }
                                        return match editor::EditorView::smoke_recovery(
                                            editor,
                                            &mut recovery_probe,
                                            smoke_recording.as_deref().expect("recovery source"),
                                            window,
                                            cx,
                                        ) {
                                            Ok(ready) => ready,
                                            Err(error) => {
                                                eprintln!("Recovery smoke failed: {error:#}");
                                                smoke_result.store(true, Ordering::Relaxed);
                                                false
                                            }
                                        };
                                    }
                                    if attempt == 0 {
                                        studio.update(cx, |studio, cx| {
                                            studio.open_recording(
                                                smoke_recording.clone().expect("smoke recording"),
                                                cx,
                                            )
                                        });
                                    }
                                    let scene_started = Instant::now();
                                    window.draw(cx).clear();
                                    let scene_finished = Instant::now();
                                    let editor = studio.read(cx).editor.clone();
                                    if smoke_gpu_playback {
                                        let present_started = Instant::now();
                                        if let Err(error) = window.present_for_diagnostics() {
                                            eprintln!("GPU playback native presentation failed: {error:#}");
                                            smoke_result.store(true, Ordering::Relaxed);
                                            return false;
                                        }
                                        gpu_playback_probe.record_tick(editor::GpuPlaybackTickTiming {
                                            requested_timer,
                                            timer_wait: timer_completed.duration_since(timer_started),
                                            foreground_dispatch: foreground_entered.duration_since(timer_completed),
                                            scene_construction: scene_finished.duration_since(scene_started),
                                            native_present: present_started.elapsed(),
                                        });
                                        return match editor::EditorView::smoke_gpu_playback(
                                            editor,
                                            &mut gpu_playback_probe,
                                            window,
                                            cx,
                                        ) {
                                            Ok(ready) => ready,
                                            Err(error) => {
                                                eprintln!("GPU playback smoke failed: {error:#}");
                                                smoke_result.store(true, Ordering::Relaxed);
                                                false
                                            }
                                        };
                                    }
                                    let ready = editor.read(cx).smoke_ready();
                                    if ready && smoke_zoom {
                                        if let Err(error) =
                                            editor::EditorView::smoke_zoom(editor, window, cx)
                                        {
                                            eprintln!("Zoom smoke failed: {error:#}");
                                            smoke_result.store(true, Ordering::Relaxed);
                                            return false;
                                        }
                                        return true;
                                    }
                                    if ready && smoke_scrub {
                                        if smoke_pressure {
                                            return match editor::EditorView::smoke_scrub_pressure(editor, &mut pressure_probe, window, cx) {
                                                Ok(ready) => ready,
                                                Err(error) => { eprintln!("Sustained scrub pressure failed: {error:#}"); smoke_result.store(true, Ordering::Relaxed); false }
                                            };
                                        }
                                        return match editor::EditorView::smoke_scrub(
                                            editor,
                                            &mut scrub_probe,
                                            window,
                                            cx,
                                        ) {
                                            Ok(ready) => ready,
                                            Err(e) => {
                                                eprintln!("Drag render smoke failed: {e:#}");
                                                smoke_result.store(true, Ordering::Relaxed);
                                                false
                                            }
                                        };
                                    }
                                    if ready && smoke_trim {
                                        if let Err(error) =
                                            editor::EditorView::smoke_trim(editor, window, cx)
                                        {
                                            eprintln!("Trim smoke failed: {error:#}");
                                            smoke_result.store(true, Ordering::Relaxed);
                                            cx.quit();
                                            return false;
                                        }
                                        println!("Trim smoke passed");
                                    }
                                    ready
                                })
                                .unwrap_or_else(|error| {
                                    eprintln!("Native editor fixture window update failed: {error:#}");
                                    smoke_result.store(true, Ordering::Relaxed);
                                    false
                                });
                            if ready || smoke_result.load(Ordering::Relaxed) {
                                break;
                            }
                        }
                        if smoke_toolkit {
                            println!("Toolkit smoke: ready={ready}, stages={toolkit_stage}");
                        }
                        println!(
                            "Editor open smoke: ready={ready}, elapsed_ms={:.0}",
                            start.elapsed().as_secs_f64() * 1000.
                        );
                        smoke_result.store(!ready, Ordering::Relaxed);
                        let _ = cx.update(|cx| {
                            cx.quit();
                        });
                    })
                    .detach();
                } else {
                    cx.activate(true);
                }
            });
        // The UI has exited. Give already admitted layout writes a finite grace
        // period without delaying input/close or retrying failed disk operations.
        let preferences_flushed = preferences::finish_pending();
        if let Err(error) = &preferences_flushed { eprintln!("Preferences shutdown: {error}"); }
        if let Some(pending) = UPDATE_ON_EXIT.lock().unwrap_or_else(|error| error.into_inner()).take() {
            match &preferences_flushed {
                Ok(()) => {
                    if let Err(error) = pending.update.apply_after_shutdown(pending.restart, &pending.data_root) {
                        installation_ui::startup_error(&format!("Kielbox could not install its downloaded update. Your current installation and data are still available.\n\n{error}"));
                    }
                }
                Err(error) => installation_ui::startup_error(&format!("The update was postponed because settings have not finished saving. Reopen Kielbox to retry.\n\n{error}")),
            }
        }
        if let Some(data_root) = RESTART_AFTER_EXIT.lock().unwrap_or_else(|error| error.into_inner()).take()
            && preferences_flushed.is_ok() {
            drop(_instance);
            let restart = std::env::current_exe().and_then(|exe| std::process::Command::new(exe)
                .arg("--working-directory").arg(data_root).spawn());
            if let Err(error) = restart { installation_ui::startup_error(&format!("Could not restart Kielbox: {error}")); }
        }
        if let Some(directory) = owned_fixture {
            if smoke_library_import {
                println!("Owned Library fixture directory: {}", directory.display());
            } else {
                println!("Owned native fixture directory: {}", directory.display());
            }
        }
        if smoke_failed.load(Ordering::Relaxed) || (exit_after_proof && PROOF_FAILED.load(Ordering::Relaxed)) {
            std::process::exit(1);
        }
    }
    #[cfg(not(windows))]
    eprintln!("Linux UI backend and capture integration are pending.");
}
