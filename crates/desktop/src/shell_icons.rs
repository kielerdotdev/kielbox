//! Windows file-type icons, queried by synthetic extension rather than real path.
//! Call on a worker thread: shell association providers may block.
use std::{
    collections::VecDeque,
    ffi::c_void,
    mem::size_of,
    sync::{Mutex, OnceLock},
};
use windows::{
    Win32::{
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
            DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, HBITMAP, HDC, HGDIOBJ, SelectObject,
        },
        Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL},
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::{
            Shell::{
                SHFILEINFOW, SHGFI_ICON, SHGFI_OPENICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES,
                SHGetFileInfoW,
            },
            WindowsAndMessaging::{
                DI_NORMAL, DestroyIcon, DrawIconEx, GetSystemMetrics, HICON, SM_CXSMICON,
                SM_CYSMICON,
            },
        },
    },
    core::PCWSTR,
};

#[derive(Clone)]
pub struct Icon {
    /// Top-down, straight-alpha RGBA pixels.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, PartialEq, Eq)]
struct Key {
    extension: Option<String>,
    open_folder: bool,
}

type Cache = VecDeque<(Key, Option<Icon>)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

/// `None` selects a folder; `Some("")` a generic file; `Some(".zip")`
/// a registered file type. Results, including failures, use a bounded cache.
pub fn load(extension: Option<&str>, open_folder: bool) -> Option<Icon> {
    let key = Key {
        extension: match extension {
            None => None,
            Some(extension) => {
                // A caller must never turn this synthetic lookup into a path or
                // URI. Keep arbitrary extension input out of shell path parsing.
                if extension.len() > 128
                    || extension
                        .chars()
                        .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
                {
                    return None;
                }
                let extension = extension.to_ascii_lowercase();
                Some(if extension.is_empty() || extension.starts_with('.') {
                    extension
                } else {
                    format!(".{extension}")
                })
            }
        },
        open_folder: extension.is_none() && open_folder,
    };
    let cache = CACHE.get_or_init(|| Mutex::new(VecDeque::new()));
    if let Some((_, icon)) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(entry, _)| entry == &key)
    {
        return icon.clone();
    }
    // An incompatible calling apartment is a thread-local condition, not a
    // missing file association. Do not poison the process cache on that failure.
    let _apartment = Apartment::new()?;
    let icon = query(&key);
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    // Another worker may have populated the same key while the shell was busy.
    if let Some((_, cached)) = cache.iter().find(|(entry, _)| entry == &key) {
        return cached.clone();
    }
    if cache.len() == 256 {
        cache.pop_front();
    }
    cache.push_back((key, icon.clone()));
    icon
}

struct Apartment;
impl Apartment {
    fn new() -> Option<Self> {
        // Both S_OK and S_FALSE acquire one COM initialization count. A thread
        // already in an MTA cannot become an STA; do not uninitialize it.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .ok()?;
        Some(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct OwnedIcon(HICON);
impl Drop for OwnedIcon {
    fn drop(&mut self) {
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pixels: *mut c_void,
    width: i32,
    height: i32,
}
impl Surface {
    fn new(width: i32, height: i32) -> Option<Self> {
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            return None;
        }
        let mut surface = Self {
            dc,
            bitmap: HBITMAP::default(),
            previous: HGDIOBJ::default(),
            pixels: std::ptr::null_mut(),
            width,
            height,
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        surface.bitmap = unsafe {
            CreateDIBSection(
                Some(dc),
                &info,
                DIB_RGB_COLORS,
                &mut surface.pixels,
                None,
                0,
            )
        }
        .ok()?;
        if surface.pixels.is_null() {
            return None;
        }
        let previous = unsafe { SelectObject(dc, HGDIOBJ(surface.bitmap.0)) };
        if previous.is_invalid() || previous.0 as isize == -1 {
            return None;
        }
        surface.previous = previous;
        Some(surface)
    }

    fn render(&mut self, icon: HICON, background: u8) -> Option<Vec<u8>> {
        let length = self.width as usize * self.height as usize * 4;
        // The DIB owns exactly width*height packed 32-bit pixels. Flush GDI
        // before touching shared DIB memory and after drawing into it.
        if !unsafe { GdiFlush() }.as_bool() {
            return None;
        }
        unsafe { std::slice::from_raw_parts_mut(self.pixels.cast::<u8>(), length) }
            .fill(background);
        unsafe {
            DrawIconEx(
                self.dc,
                0,
                0,
                icon,
                self.width,
                self.height,
                0,
                None,
                DI_NORMAL,
            )
        }
        .ok()?;
        if !unsafe { GdiFlush() }.as_bool() {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(self.pixels.cast::<u8>(), length) }.to_vec())
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_invalid() {
                SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            }
            let _ = DeleteDC(self.dc);
        }
    }
}

fn query(key: &Key) -> Option<Icon> {
    let synthetic = match &key.extension {
        Some(extension) => format!("rcr-file-type{extension}"),
        None => "rcr-folder-type".to_owned(),
    };
    let name: Vec<u16> = synthetic.encode_utf16().chain(Some(0)).collect();
    let mut info = SHFILEINFOW::default();
    let mut flags = SHGFI_USEFILEATTRIBUTES | SHGFI_ICON | SHGFI_SMALLICON;
    if key.open_folder {
        flags |= SHGFI_OPENICON;
    }
    let result = unsafe {
        SHGetFileInfoW(
            PCWSTR(name.as_ptr()),
            if key.extension.is_none() {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                FILE_ATTRIBUTE_NORMAL
            },
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    // Guard any returned icon even if the function reported a failure.
    let icon = (!info.hIcon.is_invalid()).then(|| OwnedIcon(info.hIcon));
    if result == 0 {
        return None;
    }
    let icon = icon?;
    let width = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(1, 256);
    let height = unsafe { GetSystemMetrics(SM_CYSMICON) }.clamp(1, 256);
    let mut surface = Surface::new(width, height)?;
    let black = surface.render(icon.0, 0)?;
    let white = surface.render(icon.0, 255)?;
    Some(Icon {
        rgba: composite_to_rgba(&black, &white),
        width: width as u32,
        height: height as u32,
    })
}

/// Render against two backgrounds so both modern alpha icons and legacy AND
/// masks become transparent pixels. Ignore GDI's unreliable destination alpha.
fn composite_to_rgba(black: &[u8], white: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(black.len());
    for (black, white) in black
        .as_chunks::<4>()
        .0
        .iter()
        .zip(white.as_chunks::<4>().0)
    {
        let background = (0..3)
            .map(|c| white[c].saturating_sub(black[c]))
            .max()
            .unwrap();
        let alpha = 255 - background;
        for channel in [2, 1, 0] {
            rgba.push(if alpha == 0 {
                0
            } else {
                ((u32::from(black[channel]) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                    .min(255) as u8
            });
        }
        rgba.push(alpha);
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn converts_bgra_and_recovers_opaque_masked_and_antialiased_pixels() {
        let black = [30, 20, 10, 0, 0, 0, 0, 255, 32, 64, 96, 0];
        let white = [30, 20, 10, 255, 255, 255, 255, 0, 159, 191, 223, 255];
        assert_eq!(
            composite_to_rgba(&black, &white),
            [10, 20, 30, 255, 0, 0, 0, 0, 191, 128, 64, 128]
        );
    }

    #[test]
    #[ignore = "Queries the native Windows shell; run explicitly on an interactive Windows host"]
    fn native_folder_and_file_type_icons_have_visible_pixels() {
        for (extension, open) in [
            (None, false),
            (None, true),
            (Some(""), false),
            (Some(".zip"), false),
        ] {
            let icon = load(extension, open).expect("Windows shell should provide a fallback icon");
            assert_eq!(
                icon.rgba.len(),
                icon.width as usize * icon.height as usize * 4
            );
            assert!(
                icon.rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 0)
            );
        }
    }
}
