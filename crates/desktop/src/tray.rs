//! Native tray commands feed the existing GPUI loop; capture stays on its worker.
use std::sync::mpsc;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};

pub struct Tray {
    icon: TrayIcon,
    status: MenuItem,
    replay: MenuItem,
    toggle_replay: MenuItem,
    record: MenuItem,
    region: MenuItem,
    fullscreen: MenuItem,
    quick_record: MenuItem,
    quick_label: String,
    quick_recording: bool,
    presentation: Presentation,
    pub events: mpsc::Receiver<String>,
}

/// Owned, inexpensive snapshot: callers never lend pipeline state to the tray.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrayState {
    pub active: bool,
    pub stopping: bool,
    pub recording: bool,
    pub replay_ready: bool,
    pub replay_seconds: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    Starting,
    Replay,
    Recording,
    Stopping,
}
impl TrayState {
    /// Starting describes worker intent until the first encoded frame arrives.
    pub(crate) fn mode(self) -> Mode {
        if !self.active { Mode::Off }
        else if self.stopping { Mode::Stopping }
        else if !self.replay_ready { Mode::Starting }
        else if self.recording { Mode::Recording }
        else { Mode::Replay }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Presentation {
    mode: Mode,
    status: String,
    replay: String,
    replay_enabled: bool,
    toggle_replay: String,
    toggle_enabled: bool,
    record: String,
    record_enabled: bool,
    region: String,
    fullscreen: String,
}

fn shortcut(label: &str, hotkey: &str) -> String {
    if hotkey.trim().is_empty() {
        label.into()
    } else {
        // Display the configured global shortcut without registering a second accelerator.
        format!("{label}\t{}", hotkey.trim())
    }
}

impl Presentation {
    fn with_screencast(mut self, recording: bool) -> Self {
        if recording {
            self.mode = Mode::Recording;
            self.status = format!("Screencast recording · {}", self.status);
        }
        self
    }
    fn new(state: TrayState, hotkeys: &[String; 7]) -> Self {
        let mode = state.mode();
        let ready = state.active && !state.stopping && state.replay_ready;
        let replay_label = if ready && state.replay_seconds > 0 {
            format!("Save last {}s", state.replay_seconds)
        } else if state.active && !state.stopping && !state.replay_ready {
            "Save replay (buffer starting…)".into()
        } else {
            "Save replay".into()
        };
        Self {
            mode,
            status: match mode {
                Mode::Off => "Instant replay is off",
                Mode::Starting if state.recording => "Starting recording…",
                Mode::Starting => "Starting instant replay…",
                Mode::Replay => "Instant replay is on",
                Mode::Recording => "Recording · instant replay is on",
                Mode::Stopping => "Finishing capture…",
            }
            .into(),
            replay: shortcut(&replay_label, &hotkeys[1]),
            replay_enabled: ready,
            toggle_replay: shortcut(
                if state.active {
                    "Disable instant replay"
                } else {
                    "Enable instant replay"
                },
                &hotkeys[0],
            ),
            toggle_enabled: !state.active || (!state.stopping && !state.recording),
            record: shortcut(
                if mode == Mode::Stopping {
                    "Finishing capture…"
                } else if mode == Mode::Starting && state.recording {
                    "Cancel starting recording"
                } else if mode == Mode::Recording {
                    "Stop recording"
                } else {
                    "Start recording"
                },
                &hotkeys[2],
            ),
            record_enabled: mode != Mode::Stopping,
            region: shortcut("Screenshot region", &hotkeys[3]),
            fullscreen: shortcut("Screenshot full screen", &hotkeys[4]),
        }
    }
}

impl Tray {
    pub fn new() -> anyhow::Result<Self> {
        let menu = Menu::new();
        let presentation = Presentation::new(
            TrayState::default(),
            &std::array::from_fn(|_| String::new()),
        );
        let status = MenuItem::new(&presentation.status, false, None);
        let replay = MenuItem::with_id("replay", &presentation.replay, false, None);
        let toggle_replay =
            MenuItem::with_id("toggle-replay", &presentation.toggle_replay, true, None);
        let record = MenuItem::with_id("record", &presentation.record, true, None);
        let region = MenuItem::with_id("region", &presentation.region, true, None);
        let fullscreen = MenuItem::with_id("fullscreen", &presentation.fullscreen, true, None);
        let quick_record = MenuItem::with_id("quick-record", "Record video…", true, None);
        menu.append(&MenuItem::with_id("open", "Open Kielbox", true, None))?;
        menu.append(&MenuItem::with_id("library", "Library", true, None))?;
        menu.append(&MenuItem::with_id("settings", "Settings", true, None))?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&status)?;
        menu.append(&toggle_replay)?;
        menu.append(&replay)?;
        menu.append(&record)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&region)?;
        menu.append(&fullscreen)?;
        menu.append(&quick_record)?;
        menu.append(&PredefinedMenuItem::separator())?;
        let tools = Submenu::new("Tools", true);
        for (id, label) in [
            ("screen-tool-color", "Screen color picker"),
            ("screen-tool-ruler", "Ruler"),
            ("screen-tool-pin", "Pin to screen"),
            ("screen-tool-ocr", "OCR region"),
            ("screen-tool-qr", "Read QR from screen"),
            ("screen-tool-monitor", "Monitor test"),
        ] {
            tools.append(&MenuItem::with_id(id, label, true, None))?;
        }
        tools.append(&PredefinedMenuItem::separator())?;
        tools.append(&MenuItem::with_id("utilities", "All utilities", true, None))?;
        for (i, tool) in ::toolkit::utilities::Tool::ALL.into_iter().enumerate() {
            tools.append(&MenuItem::with_id(
                format!("utility-{i}"),
                tool.name(),
                true,
                None,
            ))?;
        }
        menu.append(&tools)?;
        menu.append(&MenuItem::with_id("quit", "Quit Kielbox", true, None))?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(format!("Kielbox — {}", presentation.status))
            .with_icon(state_icon(presentation.mode)?)
            .build()?;
        let (tx, events) = mpsc::channel();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = tx.send(event.id.0);
        }));
        Ok(Self {
            icon,
            status,
            replay,
            toggle_replay,
            record,
            region,
            fullscreen,
            quick_record,
            quick_label: String::new(),
            quick_recording: false,
            presentation,
            events,
        })
    }

    /// Safe to call each UI poll: identical visible state performs no native work.
    pub fn update_quick_recorder(&mut self, recording: bool, busy: bool, hotkey: &str) {
        self.quick_recording = recording;
        let label = shortcut(if recording { "Stop video recording" } else if busy { "Show recording controls…" } else { "Record video…" }, hotkey);
        if self.quick_label != label { self.quick_record.set_text(&label); self.quick_label = label; }
    }
    pub fn update(&mut self, state: TrayState, hotkeys: &[String; 7]) -> anyhow::Result<()> {
        let next = Presentation::new(state, hotkeys).with_screencast(self.quick_recording);
        let previous = &self.presentation;
        if *previous == next {
            return Ok(());
        }
        if previous.mode != next.mode {
            self.icon.set_icon(Some(state_icon(next.mode)?))?;
        }
        if previous.status != next.status {
            self.icon
                .set_tooltip(Some(format!("Kielbox — {}", next.status)))?;
            self.status.set_text(&next.status);
        }
        if previous.replay != next.replay {
            self.replay.set_text(&next.replay);
        }
        if previous.replay_enabled != next.replay_enabled {
            self.replay.set_enabled(next.replay_enabled);
        }
        if previous.toggle_replay != next.toggle_replay {
            self.toggle_replay.set_text(&next.toggle_replay);
        }
        if previous.toggle_enabled != next.toggle_enabled {
            self.toggle_replay.set_enabled(next.toggle_enabled);
        }
        if previous.record != next.record {
            self.record.set_text(&next.record);
        }
        if previous.record_enabled != next.record_enabled {
            self.record.set_enabled(next.record_enabled);
        }
        if previous.region != next.region {
            self.region.set_text(&next.region);
        }
        if previous.fullscreen != next.fullscreen {
            self.fullscreen.set_text(&next.fullscreen);
        }
        self.presentation = next;
        Ok(())
    }
}

fn state_icon(mode: Mode) -> anyhow::Result<Icon> {
    let mut pixels = vec![0u8; 32 * 32 * 4];
    let color = match mode {
        Mode::Off => [156, 163, 175, 255],
        Mode::Starting | Mode::Stopping => [251, 191, 36, 255],
        Mode::Replay => [98, 219, 161, 255],
        Mode::Recording => [248, 83, 103, 255],
    };
    for y in 0i32..32 {
        for x in 0i32..32 {
            let distance = (x - 16).pow(2) + (y - 16).pow(2);
            let filled = match mode {
                Mode::Off => {
                    (64..=144).contains(&distance) || ((x - y).abs() < 2 && distance < 144)
                }
                Mode::Starting | Mode::Stopping => (10..22).contains(&x) && (10..22).contains(&y),
                Mode::Replay => {
                    (64..=144).contains(&distance)
                        || ((20..28).contains(&x) && (5..13).contains(&y))
                }
                Mode::Recording => distance <= 121,
            };
            if filled {
                let offset = ((y * 32 + x) * 4) as usize;
                pixels[offset..offset + 4].copy_from_slice(&color);
            }
        }
    }
    Ok(Icon::from_rgba(pixels, 32, 32)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(state: TrayState) -> Presentation {
        Presentation::new(state, &std::array::from_fn(|_| String::new()))
    }
    #[test]
    fn screencast_indicator_is_visible_without_changing_game_capture_actions() {
        for state in [TrayState::default(), TrayState { active: true, replay_ready: true, replay_seconds: 300, ..Default::default() }] {
            let ordinary = labels(state);
            let recording = labels(state).with_screencast(true);
            assert_eq!(recording.mode, Mode::Recording);
            assert!(recording.status.starts_with("Screencast recording"));
            assert_eq!(recording.replay_enabled, ordinary.replay_enabled);
            assert_eq!(recording.record, ordinary.record);
            assert_eq!(recording.toggle_enabled, ordinary.toggle_enabled);
            assert_eq!(labels(state).with_screencast(false), ordinary);
        }
    }

    #[test]
    fn inactive_stats_cannot_enable_capture_actions_or_change_labels() {
        let off = labels(TrayState::default());
        let stale = labels(TrayState {
            recording: true,
            stopping: true,
            replay_ready: true,
            replay_seconds: 120,
            ..TrayState::default()
        });
        assert_eq!(off, stale);
        assert!(!off.replay_enabled);
        assert!(off.toggle_enabled && off.record_enabled);
    }

    #[test]
    fn capture_transitions_show_truthful_labels_and_enablement() {
        let starting = labels(TrayState {
            active: true,
            ..TrayState::default()
        });
        assert_eq!(starting.mode, Mode::Starting);
        assert!(!starting.replay_enabled);
        let starting_recording = labels(TrayState { active: true, recording: true, ..Default::default() });
        assert_eq!(starting_recording.mode, Mode::Starting);
        assert_eq!(starting_recording.status, "Starting recording…");
        assert_eq!(starting_recording.record, "Cancel starting recording");
        assert!(starting_recording.record_enabled);
        assert!(!starting_recording.replay_enabled && !starting_recording.toggle_enabled);
        let replay = labels(TrayState {
            active: true,
            replay_ready: true,
            replay_seconds: 37,
            ..TrayState::default()
        });
        assert_eq!(replay.replay, "Save last 37s");
        assert!(replay.replay_enabled && replay.toggle_enabled);
        let recording = labels(TrayState {
            active: true,
            recording: true,
            replay_ready: true,
            ..TrayState::default()
        });
        assert_eq!(recording.record, "Stop recording");
        assert!(!recording.toggle_enabled);
        assert!(recording.replay_enabled);
        let stopping = labels(TrayState {
            active: true,
            stopping: true,
            recording: true,
            replay_ready: true,
            ..TrayState::default()
        });
        assert_eq!(stopping.mode, Mode::Stopping);
        assert!(!stopping.record_enabled && !stopping.toggle_enabled && !stopping.replay_enabled);
    }

    #[test]
    fn capture_phase_matrix_never_claims_recording_before_first_frame() {
        for active in [false, true] {
            for stopping in [false, true] {
                for recording in [false, true] {
                    for replay_ready in [false, true] {
                        let state = TrayState { active, stopping, recording, replay_ready, replay_seconds: 0 };
                        let shown = labels(state);
                        assert_eq!(shown.mode == Mode::Recording, active && !stopping && recording && replay_ready);
                        assert_eq!(shown.mode == Mode::Starting, active && !stopping && !replay_ready);
                        assert_eq!(shown.mode == Mode::Stopping, active && stopping);
                        assert_eq!(shown.replay_enabled, active && !stopping && replay_ready);
                        assert_eq!(shown.record_enabled, !active || !stopping);
                        if shown.mode == Mode::Starting && recording {
                            assert!(!shown.status.starts_with("Recording"));
                            assert_eq!(shown.record, "Cancel starting recording");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn shortcut_labels_follow_custom_and_unbound_preferences() {
        let hotkeys = ["Ctrl+Alt+F8", "", "Shift+F2", "Ctrl+F3", "", "", ""].map(str::to_owned);
        let shown = Presentation::new(TrayState::default(), &hotkeys);
        assert_eq!(shown.toggle_replay, "Enable instant replay\tCtrl+Alt+F8");
        assert_eq!(shown.replay, "Save replay");
        assert_eq!(shown.record, "Start recording\tShift+F2");
        assert_eq!(shown.region, "Screenshot region\tCtrl+F3");
        assert_eq!(shown.fullscreen, "Screenshot full screen");
    }
}
