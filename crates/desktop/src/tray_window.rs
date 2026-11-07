//! Keep capture alive while removing the main window from the taskbar.
use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{SW_HIDE, SW_SHOW, ShowWindowAsync},
};

fn handle(window: &Window) -> anyhow::Result<HWND> {
    match HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .as_raw()
    {
        RawWindowHandle::Win32(handle) => Ok(HWND(handle.hwnd.get() as *mut _)),
        _ => anyhow::bail!("Expected a Windows window"),
    }
}

pub fn hide(window: &Window) -> anyhow::Result<()> {
    unsafe {
        ShowWindowAsync(handle(window)?, SW_HIDE).ok()?;
    }
    Ok(())
}

pub fn open(window: &mut Window) {
    if let Ok(handle) = handle(window) {
        unsafe {
            let _ = ShowWindowAsync(handle, SW_SHOW);
        }
    }
    window.activate_window();
}
