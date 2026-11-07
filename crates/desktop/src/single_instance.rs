//! One normal desktop owner per Windows user across sessions/elevation.
//! Hooks/diagnostics bypass this; a failure never falls back to session-local admission.
use sha2::{Digest, Sha256};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS,
            GetLastError, HANDLE, SetLastError,
        },
        Security::{
            GetLengthSid, GetTokenInformation, IsValidSid, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::Threading::{
            CreateMutexW, GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
        UI::WindowsAndMessaging::{
            FindWindowW, GetWindowThreadProcessId, SW_RESTORE, SetForegroundWindow, ShowWindow,
        },
    },
    core::{PCWSTR, PWSTR, w},
};

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub struct Guard {
    _handle: OwnedHandle,
}

fn user_mutex_name(sid: &[u8]) -> String {
    format!("Global\\KielboxDesktop-v1-{:x}", Sha256::digest(sid))
}

fn current_user_sid() -> Result<Vec<u8>, String> {
    let mut raw = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) }.map_err(|error| {
        format!("Could not identify the Windows user for Kielbox admission: {error}")
    })?;
    let token = OwnedHandle(raw);
    let mut required = 0;
    let probe = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut required) };
    if !matches!(probe, Err(ref error) if error.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult()) {
        return Err(format!(
            "Could not size the Windows user token for Kielbox admission: {probe:?}"
        ));
    }
    if required < std::mem::size_of::<TOKEN_USER>() as u32 || required > 64 * 1024 {
        return Err(
            "Windows returned an invalid user-token size; Kielbox admission was not established."
                .into(),
        );
    }
    // TOKEN_USER includes an aligned pointer into this same buffer. Allocate
    // pointer-aligned storage rather than casting an unaligned Vec<u8>.
    let mut buffer = vec![0usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
    let capacity = required;
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            capacity,
            &mut required,
        )
    }
    .map_err(|error| format!("Could not read the Windows user for Kielbox admission: {error}"))?;
    if required > capacity || required < std::mem::size_of::<TOKEN_USER>() as u32 {
        return Err(
            "Windows returned an incomplete user token; Kielbox admission was not established."
                .into(),
        );
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let start = buffer.as_ptr() as usize;
    let end = start + required as usize;
    let sid_start = user.User.Sid.0 as usize;
    if sid_start < start || sid_start.checked_add(8).is_none_or(|sid_end| sid_end > end) {
        return Err(
            "Windows returned an invalid user SID; Kielbox admission was not established.".into(),
        );
    }
    let length = unsafe { GetLengthSid(user.User.Sid) } as usize;
    if length < 8
        || sid_start
            .checked_add(length)
            .is_none_or(|sid_end| sid_end > end)
        || !unsafe { IsValidSid(user.User.Sid) }.as_bool()
    {
        return Err(
            "Windows returned an invalid user SID; Kielbox admission was not established.".into(),
        );
    }
    Ok(unsafe { std::slice::from_raw_parts(user.User.Sid.0.cast::<u8>(), length) }.to_vec())
}

impl Guard {
    /// `None` means another normal desktop process owns this product's mutex.
    /// Keep the returned guard alive until the application and its workers exit.
    pub fn acquire() -> Result<Option<Self>, String> {
        let name = user_mutex_name(&current_user_sid()?);
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        Self::acquire_named(PCWSTR(name.as_ptr()))
    }

    fn acquire_named(name: PCWSTR) -> Result<Option<Self>, String> {
        // No ownership is requested: the live named object is the admission
        // token, and closing the final handle retires it even after a crash.
        let handle = unsafe {
            SetLastError(ERROR_SUCCESS);
            CreateMutexW(None, false, name)
        }
        .map_err(|error| format!("Could not establish the Kielbox desktop instance: {error}"))?;
        let existing = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let guard = Self {
            _handle: OwnedHandle(handle),
        };
        if existing {
            drop(guard);
            Ok(None)
        } else {
            Ok(Some(guard))
        }
    }
}

/// A second launch may restore this product's existing main window. Validate
/// its executable before operating on it; a matching caption alone is not enough.
/// Foreground activation remains subject to the Windows foreground policy.
pub fn activate_existing() -> bool {
    let Ok(window) = (unsafe { FindWindowW(None, w!("Kielbox")) }) else {
        return false;
    };
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut pid));
    }
    if pid == 0 {
        return false;
    }
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return false;
    };
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    if queried.is_err() {
        return false;
    }
    use std::os::windows::ffi::OsStringExt;
    let image = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]));
    let Some(current) = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
    else {
        return false;
    };
    if image.canonicalize().ok().as_ref() != Some(&current) {
        return false;
    }
    unsafe {
        let _ = ShowWindow(window, SW_RESTORE);
        SetForegroundWindow(window).as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_name() -> Vec<u16> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!(
            "Global\\KielboxDesktop-test-{}-{unique}-{}\0",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
        .encode_utf16()
        .collect()
    }

    #[test]
    fn one_owner_retains_admission_until_its_handle_is_released() {
        let name = test_name();
        let primary = Guard::acquire_named(PCWSTR(name.as_ptr()))
            .unwrap()
            .expect("first owner");
        assert!(
            Guard::acquire_named(PCWSTR(name.as_ptr()))
                .unwrap()
                .is_none()
        );
        assert!(
            Guard::acquire_named(PCWSTR(name.as_ptr()))
                .unwrap()
                .is_none()
        );
        drop(primary);
        assert!(
            Guard::acquire_named(PCWSTR(name.as_ptr()))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn independent_owned_names_do_not_block_each_other() {
        let one = test_name();
        let two = test_name();
        let _first = Guard::acquire_named(PCWSTR(one.as_ptr()))
            .unwrap()
            .expect("first owner");
        let _second = Guard::acquire_named(PCWSTR(two.as_ptr()))
            .unwrap()
            .expect("independent owner");
    }

    #[test]
    fn global_admission_name_depends_on_the_user_sid_and_not_the_session() {
        let first = user_mutex_name(b"synthetic user one");
        assert!(first.starts_with("Global\\KielboxDesktop-v1-"));
        assert_eq!(first.len(), "Global\\KielboxDesktop-v1-".len() + 64);
        assert_eq!(first, user_mutex_name(b"synthetic user one"));
        assert_ne!(first, user_mutex_name(b"synthetic user two"));
    }

    #[test]
    fn windows_process_token_produces_a_stable_nonempty_user_sid() {
        let first = current_user_sid().unwrap();
        let second = current_user_sid().unwrap();
        assert!(!first.is_empty());
        assert_eq!(first, second);
    }
}
