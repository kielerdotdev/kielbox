//! Thread-owned Win32 global shortcuts; no hooks or input-thread work.
use std::{collections::HashMap, sync::mpsc, thread};
use windows::Win32::{
    Foundation::{LPARAM, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{
            HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
        },
        WindowsAndMessaging::{
            GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT,
        },
    },
};

pub const ACTIONS: [&str; 7] = [
    "Toggle instant replay",
    "Save replay",
    "Start / stop recording",
    "Capture region",
    "Capture entire screen",
    "Capture active window",
    "Record video / stop screencast",
];
#[derive(Clone, Debug)]
pub struct Status {
    pub registered: bool,
    pub detail: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Chord {
    modifiers: u32,
    key: u32,
}
fn parse(text: &str) -> Result<Option<Chord>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut modifiers = 0;
    let mut key = None;
    for part in text.split('+').map(|s| s.trim().to_ascii_lowercase()) {
        let modifier = match part.as_str() {
            "ctrl" | "control" => 2,
            "alt" => 1,
            "shift" => 4,
            "win" => 8,
            _ => 0,
        };
        if modifier != 0 {
            if modifiers & modifier != 0 {
                return Err("A modifier appears twice.".into());
            }
            modifiers |= modifier;
            continue;
        }
        let code = match part.as_str() {
            "printscreen" | "print_screen" | "snapshot" => 0x2c,
            "space" => 0x20,
            "insert" => 0x2d,
            "delete" => 0x2e,
            "home" => 0x24,
            "end" => 0x23,
            "pageup" | "page_up" => 0x21,
            "pagedown" | "page_down" => 0x22,
            "up" => 0x26,
            "down" => 0x28,
            "left" => 0x25,
            "right" => 0x27,
            "pause" => 0x13,
            _ if part.len() == 1 && part.as_bytes()[0].is_ascii_alphanumeric() => {
                u32::from(part.as_bytes()[0].to_ascii_uppercase())
            }
            _ if part.starts_with('f') => {
                let n = part[1..]
                    .parse::<u32>()
                    .map_err(|_| "Choose a letter, number, function key or PrintScreen.")?;
                if !(1..=24).contains(&n) {
                    return Err("Function keys must be F1–F24.".into());
                }
                0x70 + n - 1
            }
            _ => return Err("Choose a letter, number, function key or PrintScreen.".into()),
        };
        if key.replace(code).is_some() {
            return Err("Use one key with Ctrl, Alt, Shift or Win.".into());
        }
    }
    let key = key.ok_or("Press a key as well as a modifier.")?;
    if key == 0x7b {
        return Err("F12 is reserved by Windows. Choose another key.".into());
    }
    if modifiers == 0 && !((0x70..=0x87).contains(&key) || key == 0x2c) {
        return Err("Add Ctrl, Alt, Shift or Win so normal typing keeps working.".into());
    }
    if (modifiers == 8 && matches!(key, 0x4c | 0x44)) || (modifiers == 1 && key == 0x73) {
        return Err("This shortcut is reserved by Windows. Choose another combination.".into());
    }
    Ok(Some(Chord { modifiers, key }))
}
pub fn validate(bindings: &[String; 7]) -> Result<(), String> {
    let mut seen = HashMap::new();
    for (i, text) in bindings.iter().enumerate() {
        if let Some(chord) = parse(text).map_err(|e| format!("{}: {e}", ACTIONS[i]))?
            && let Some(previous) = seen.insert(chord, i)
        {
            return Err(format!(
                "{text} is already assigned to {}.",
                ACTIONS[previous]
            ));
        }
    }
    Ok(())
}
pub fn quick_binding_conflicts(bindings: &[String; 7]) -> bool {
    let Ok(Some(quick)) = parse(&bindings[6]) else { return false; };
    bindings[..6].iter().any(|binding| parse(binding).ok().flatten() == Some(quick))
}
pub fn from_keystroke(stroke: &gpui::Keystroke) -> Result<String, String> {
    let mut parts = Vec::new();
    if stroke.modifiers.control {
        parts.push("Ctrl".to_owned());
    }
    if stroke.modifiers.alt {
        parts.push("Alt".to_owned());
    }
    if stroke.modifiers.shift {
        parts.push("Shift".to_owned());
    }
    if stroke.modifiers.platform {
        parts.push("Win".to_owned());
    }
    let key = match stroke.key.as_str() {
        "print_screen" | "snapshot" | "printscreen" => "PrintScreen".to_owned(),
        other => other.to_ascii_uppercase(),
    };
    parts.push(key);
    let text = parts.join("+");
    parse(&text)?;
    Ok(text)
}
pub enum Event {
    ToggleQuickRecord,
    ToggleReplay,
    SaveReplay,
    ToggleRecord,
    Screenshot(rcr_toolkit::core::workflow::CaptureTarget),
    Notice(String),
}
pub struct Hotkeys {
    pub events: mpsc::Receiver<Event>,
    pub statuses: Vec<Status>,
    thread_id: u32,
    worker: Option<thread::JoinHandle<()>>,
}
impl Hotkeys {
    pub fn start_with(bindings: &[String; 7]) -> anyhow::Result<Self> {
        validate(bindings).map_err(anyhow::Error::msg)?;
        let parsed = bindings
            .iter()
            .map(|b| parse(b).expect("validated chord"))
            .collect::<Vec<_>>();
        let (tx, events) = mpsc::sync_channel(16);
        let (ready, initialized) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("rcr-hotkeys".into())
            .spawn(move || unsafe {
                // Create the message queue before publishing this thread's ID.
                let mut message = MSG::default();
                let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
                let mut registered = Vec::new();
                let mut statuses = Vec::new();
                for (i, chord) in parsed.into_iter().enumerate() {
                    let Some(chord) = chord else {
                        statuses.push(Status { registered: false, detail: "Disabled".into() });
                        continue;
                    };
                    let id = i as i32 + 1;
                    match RegisterHotKey(None, id, HOT_KEY_MODIFIERS(chord.modifiers) | MOD_NOREPEAT, chord.key) {
                        Ok(()) => {
                            registered.push(id);
                            statuses.push(Status { registered: true, detail: "Available globally".into() });
                        }
                        Err(error) => statuses.push(Status {
                            registered: false,
                            detail: if error.code().0 as u32 == 0x8007_0581 {
                                "In use by Windows or another app. Click to reassign."
                            } else {
                                "Windows could not register this shortcut. Click to reassign."
                            }.into(),
                        }),
                    }
                }
                let unavailable = statuses.iter().filter(|s| !s.registered && s.detail != "Disabled").count();
                if unavailable > 0 {
                    let _ = tx.try_send(Event::Notice(format!(
                        "{unavailable} global shortcuts unavailable. Reassign them in Settings → Global shortcuts. Capture buttons still work."
                    )));
                }
                if ready.send((GetCurrentThreadId(), statuses)).is_ok() {
                    loop {
                        if GetMessageW(&mut message, None, 0, 0).0 <= 0 { break; }
                        if message.message == WM_HOTKEY {
                            let event = match message.wParam.0 {
                                1 => Event::ToggleReplay,
                                2 => Event::SaveReplay,
                                3 => Event::ToggleRecord,
                                4 => Event::Screenshot(rcr_toolkit::core::workflow::CaptureTarget::Region),
                                5 => Event::Screenshot(rcr_toolkit::core::workflow::CaptureTarget::Fullscreen),
                                6 => Event::Screenshot(rcr_toolkit::core::workflow::CaptureTarget::Window),
                                7 => Event::ToggleQuickRecord,
                                _ => continue,
                            };
                            let _ = tx.try_send(event);
                        }
                    }
                }
                for id in registered { let _ = UnregisterHotKey(None, id); }
            })?;
        match initialized.recv() {
            Ok((thread_id, statuses)) => Ok(Self {
                events,
                statuses,
                thread_id,
                worker: Some(worker),
            }),
            Err(error) => {
                let _ = worker.join();
                Err(error.into())
            }
        }
    }
}
impl Drop for Hotkeys {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_disabled_are_valid() {
        let mut b = crate::preferences::default_hotkeys();
        assert!(validate(&b).is_ok());
        b[5].clear();
        assert!(validate(&b).is_ok());
    }
    #[test]
    fn duplicates_are_case_and_order_insensitive() {
        let mut b = crate::preferences::default_hotkeys();
        b[1] = "shift+CTRL+f9".into();
        assert!(validate(&b).unwrap_err().contains("already assigned"));
    }
    #[test]
    fn unsafe_or_ambiguous_shortcuts_are_rejected() {
        for value in [
            "A",
            "Ctrl",
            "Ctrl+Ctrl+A",
            "Ctrl+A+B",
            "F12",
            "Alt+F4",
            "Win+L",
            "Ctrl+???",
        ] {
            assert!(parse(value).is_err(), "{value}");
        }
    }
    #[test]
    fn gpui_keystrokes_round_trip_to_persisted_chords() {
        for (input, expected) in [
            ("ctrl-shift-f9", "Ctrl+Shift+F9"),
            ("alt-printscreen", "Alt+PrintScreen"),
            ("ctrl-a", "Ctrl+A"),
        ] {
            let stroke = gpui::Keystroke::parse(input).unwrap();
            assert_eq!(from_keystroke(&stroke).unwrap(), expected);
        }
    }
    #[test]
    fn supported_keys_map_to_windows_values() {
        assert_eq!(
            parse("Ctrl+Shift+F9").unwrap(),
            Some(Chord {
                modifiers: 6,
                key: 0x78
            })
        );
        assert_eq!(
            parse("Alt+PrintScreen").unwrap(),
            Some(Chord {
                modifiers: 1,
                key: 0x2c
            })
        );
        assert_eq!(
            parse("Ctrl+Page_Up").unwrap(),
            Some(Chord {
                modifiers: 2,
                key: 0x21
            })
        );
    }
}
