//! Window inspection and reversible border removal for explicitly selected windows.
use anyhow::{Result, ensure};
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Info {
    pub handle: u64,
    pub process: u32,
    pub title: String,
    pub class: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
#[cfg(windows)]
mod native {
    use super::*;
    use windows::Win32::{
        Foundation::{GetLastError, HWND, LPARAM, RECT, SetLastError, WIN32_ERROR},
        UI::WindowsAndMessaging::*,
    };
    use windows::core::BOOL;
    fn info(hwnd: HWND) -> Option<Info> {
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() {
                return None;
            }
            let mut title = [0u16; 1024];
            let n = GetWindowTextW(hwnd, &mut title);
            let mut class = [0u16; 256];
            let cn = GetClassNameW(hwnd, &mut class);
            let mut process = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut process));
            let mut r = RECT::default();
            GetWindowRect(hwnd, &mut r).ok()?;
            Some(Info {
                handle: hwnd.0 as usize as u64,
                process,
                title: String::from_utf16_lossy(&title[..n.max(0) as usize]),
                class: String::from_utf16_lossy(&class[..cn.max(0) as usize]),
                x: r.left,
                y: r.top,
                width: r.right - r.left,
                height: r.bottom - r.top,
            })
        }
    }
    pub fn list() -> Result<Vec<Info>> {
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            let entries = unsafe { &mut *(data.0 as *mut Vec<Info>) };
            if unsafe { IsWindowVisible(hwnd).as_bool() }
                && let Some(i) = info(hwnd)
                && !i.title.is_empty()
            {
                entries.push(i);
            }
            BOOL(1)
        }
        let mut entries: Vec<Info> = vec![];
        unsafe {
            EnumWindows(Some(each), LPARAM(&mut entries as *mut Vec<Info> as isize))?;
        }
        entries.sort_by(|a, b| a.title.cmp(&b.title));
        Ok(entries)
    }
    pub struct Borderless {
        info: Info,
        style: isize,
        restored: bool,
    }
    fn style(hwnd: HWND, value: isize) -> Result<()> {
        unsafe {
            SetLastError(WIN32_ERROR(0));
            let previous = SetWindowLongPtrW(hwnd, GWL_STYLE, value);
            if previous == 0 && GetLastError().0 != 0 {
                return Err(windows::core::Error::from_win32().into());
            }
        }
        Ok(())
    }
    impl Borderless {
        pub fn apply(selected: &Info) -> Result<Self> {
            let hwnd = HWND(selected.handle as usize as *mut _);
            let live = info(hwnd).ok_or_else(|| anyhow::anyhow!("Window has closed"))?;
            ensure!(
                live.process == selected.process && live.class == selected.class,
                "Selected window changed; refresh the window list"
            );
            let previous = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
            let mut saved = Self {
                info: live,
                style: previous,
                restored: false,
            };
            let remove = (WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU)
                .0 as isize;
            style(hwnd, previous & !remove)?;
            if let Err(e) = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE
                        | SWP_NOSIZE
                        | SWP_NOZORDER
                        | SWP_NOACTIVATE
                        | SWP_FRAMECHANGED
                        | SWP_ASYNCWINDOWPOS,
                )
            } {
                let _ = saved.restore();
                return Err(e.into());
            }
            Ok(saved)
        }
        pub fn handle(&self) -> u64 {
            self.info.handle
        }
        pub fn restore(&mut self) -> Result<()> {
            if self.restored {
                return Ok(());
            }
            let hwnd = HWND(self.info.handle as usize as *mut _);
            if let Some(live) = info(hwnd) {
                ensure!(
                    live.process == self.info.process && live.class == self.info.class,
                    "Window identity changed; leaving the new window untouched"
                );
                style(hwnd, self.style)?;
                unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        self.info.x,
                        self.info.y,
                        self.info.width,
                        self.info.height,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_ASYNCWINDOWPOS,
                    )?;
                }
            }
            self.restored = true;
            Ok(())
        }
    }
    impl Drop for Borderless {
        fn drop(&mut self) {
            let _ = self.restore();
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn border_removal_restores_style_and_rejects_changed_identity() {
            unsafe {
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("rcr synthetic border test"),
                    WS_OVERLAPPEDWINDOW,
                    10,
                    10,
                    300,
                    200,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
                let selected = info(hwnd).unwrap();
                let before = GetWindowLongPtrW(hwnd, GWL_STYLE);
                let mut wrong = selected.clone();
                wrong.process = wrong.process.wrapping_add(1);
                assert!(Borderless::apply(&wrong).is_err());
                assert_eq!(GetWindowLongPtrW(hwnd, GWL_STYLE), before);
                {
                    let mut saved = Borderless::apply(&selected).unwrap();
                    assert_eq!(
                        GetWindowLongPtrW(hwnd, GWL_STYLE) & WS_CAPTION.0 as isize,
                        0
                    );
                    saved.restore().unwrap();
                    assert_eq!(GetWindowLongPtrW(hwnd, GWL_STYLE), before);
                }
                DestroyWindow(hwnd).unwrap();
            }
        }
    }
}
#[cfg(windows)]
pub use native::{Borderless, list};
#[cfg(not(windows))]
pub fn list() -> Result<Vec<Info>> {
    anyhow::bail!("Window inspection is currently supported on Windows")
}
