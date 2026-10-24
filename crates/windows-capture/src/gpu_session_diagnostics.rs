//! Test-only, current-process context metadata. No window/session enumeration,
//! desktop switching, user identity queries, pixels, or input access.
use windows::{
    Win32::{
        Foundation::HANDLE,
        System::{
            RemoteDesktop::*,
            StationsAndDesktops::*,
            Threading::{GetCurrentProcessId, GetCurrentThreadId},
        },
    },
    core::PWSTR,
};

struct ObjectName {
    name: Option<String>,
    error: Option<String>,
    needed_bytes: u32,
}
impl ObjectName {
    fn failed(error: windows::core::Error) -> Self {
        Self {
            name: None,
            error: Some(format!("{error}")),
            needed_bytes: 0,
        }
    }
    fn query(handle: HANDLE) -> Self {
        // Fixed 512-byte cap: never allocate according to an OS-provided length.
        let mut buffer = [0u16; 256];
        let mut needed = 0;
        let result = unsafe {
            GetUserObjectInformationW(
                handle,
                UOI_NAME,
                Some(buffer.as_mut_ptr().cast()),
                std::mem::size_of_val(&buffer) as u32,
                Some(&mut needed),
            )
        };
        match result {
            Ok(()) => {
                let count = buffer
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(buffer.len());
                Self {
                    name: Some(String::from_utf16_lossy(&buffer[..count])),
                    error: None,
                    needed_bytes: needed,
                }
            }
            Err(error) => Self {
                name: None,
                error: Some(format!("{error}")),
                needed_bytes: needed,
            },
        }
    }
    fn json(&self) -> serde_json::Value {
        serde_json::json!({"name": self.name, "error": self.error, "needed_bytes": self.needed_bytes})
    }
}

struct InputDesktop(HDESK);
impl Drop for InputDesktop {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseDesktop(self.0);
        }
    }
}
struct SessionBuffer(*mut std::ffi::c_void);
impl Drop for SessionBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                WTSFreeMemory(self.0);
            }
        }
    }
}

fn session_state(session: u32) -> Result<i32, String> {
    let mut pointer = PWSTR::null();
    let mut bytes = 0;
    let result = unsafe {
        WTSQuerySessionInformationW(None, session, WTSConnectState, &mut pointer, &mut bytes)
    };
    let buffer = SessionBuffer(pointer.0.cast());
    result.map_err(|error| format!("{error}"))?;
    if buffer.0.is_null() || bytes < std::mem::size_of::<i32>() as u32 {
        return Err("WTS connection-state response was missing or too short".into());
    }
    // WTSConnectState returns one WTS_CONNECTSTATE_CLASS; read no string/user data.
    Ok(unsafe { buffer.0.cast::<i32>().read_unaligned() })
}

fn state_name(state: i32) -> &'static str {
    match state {
        0 => "active",
        1 => "connected",
        2 => "connect-query",
        3 => "shadow",
        4 => "disconnected",
        5 => "idle",
        6 => "listen",
        7 => "reset",
        8 => "down",
        9 => "init",
        _ => "unknown",
    }
}

fn context_hint(
    session: Option<i32>,
    station: Option<&str>,
    thread: Option<&str>,
    input: Option<&str>,
) -> Option<&'static str> {
    if matches!(session, Some(4 | 6 | 7 | 8)) {
        return Some(
            "The process session is inactive or disconnected. Run the proof from the signed-in interactive session and retry.",
        );
    }
    if station.is_some_and(|station| !station.eq_ignore_ascii_case("WinSta0")) {
        return Some(
            "The process uses a noninteractive window station. Launch the proof from the interactive desktop rather than a service window station.",
        );
    }
    if let (Some(thread), Some(input)) = (thread, input) {
        if !thread.eq_ignore_ascii_case(input) {
            return Some(
                "The capture thread desktop differs from the input desktop. Return to the normal interactive desktop and launch the proof there.",
            );
        }
    }
    None
}

pub(super) fn snapshot() -> serde_json::Value {
    let process = unsafe { GetCurrentProcessId() };
    let thread = unsafe { GetCurrentThreadId() };
    // Both returned handles are borrowed; do not CloseDesktop/CloseWindowStation.
    let station = match unsafe { GetProcessWindowStation() } {
        Ok(handle) => ObjectName::query(HANDLE(handle.0)),
        Err(error) => ObjectName::failed(error),
    };
    let desktop = match unsafe { GetThreadDesktop(thread) } {
        Ok(handle) => ObjectName::query(HANDLE(handle.0)),
        Err(error) => ObjectName::failed(error),
    };
    let input =
        match unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) } {
            Ok(handle) => {
                let owned = InputDesktop(handle);
                ObjectName::query(HANDLE(owned.0.0))
            }
            Err(error) => ObjectName::failed(error),
        };
    let mut session = 0;
    let session_result = unsafe { ProcessIdToSessionId(process, &mut session) };
    let connection = match &session_result {
        Ok(()) => session_state(session),
        Err(error) => Err(format!("{error}")),
    };
    let hint = context_hint(
        connection.as_ref().ok().copied(),
        station.name.as_deref(),
        desktop.name.as_deref(),
        input.name.as_deref(),
    );
    serde_json::json!({
        "process_id": process, "thread_id": thread,
        "session_id": session_result.as_ref().ok().map(|_| session),
        "session_state": connection.as_ref().ok(),
        "session_state_name": connection.as_ref().ok().map(|state| state_name(*state)),
        "session_query_error": connection.as_ref().err(),
        "window_station": station.json(), "thread_desktop": desktop.json(), "input_desktop": input.json(),
        "thread_matches_input": desktop.name.as_ref().zip(input.name.as_ref()).map(|(thread, input)| thread.eq_ignore_ascii_case(input)),
        "actionable_hint": hint,
        "limits": "Matching desktop names do not prove an active session; OpenInputDesktop may succeed for a disconnected session. No lock state is inferred from an access error.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_hints_require_a_concrete_inactive_or_mismatched_condition() {
        assert!(
            context_hint(Some(4), Some("WinSta0"), Some("Default"), Some("Default"))
                .unwrap()
                .contains("disconnected")
        );
        assert!(
            context_hint(Some(0), Some("Service-0x0-3e7$"), Some("Default"), None)
                .unwrap()
                .contains("noninteractive")
        );
        assert!(
            context_hint(Some(0), Some("WinSta0"), Some("Default"), Some("Winlogon"))
                .unwrap()
                .contains("differs")
        );
        assert!(context_hint(Some(0), Some("winsta0"), Some("Default"), Some("default")).is_none());
        assert!(context_hint(None, None, None, None).is_none());
    }
}
