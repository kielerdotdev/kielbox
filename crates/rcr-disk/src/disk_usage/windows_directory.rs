//! Directory entries from one Win32 enumeration, without per-entry metadata I/O.
use rcr_workflows::workflow::CancelToken;
use std::{
    ffi::OsString,
    io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, Prefix},
};
use windows::{
    Win32::{
        Foundation::{ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, HANDLE},
        Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, FIND_FIRST_EX_FLAGS, FIND_FIRST_EX_LARGE_FETCH, FindClose,
            FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
            WIN32_FIND_DATAW,
        },
    },
    core::{Error, HRESULT, PCWSTR},
};

pub(super) struct Entry {
    pub name: OsString,
    pub attributes: u32,
    pub bytes: u64,
    pub is_directory: bool,
}

struct Search(HANDLE);
impl Drop for Search {
    fn drop(&mut self) {
        let _ = unsafe { FindClose(self.0) };
    }
}

fn os_error(error: Error) -> io::Error {
    io::Error::from_raw_os_error((error.code().0 as u32 & 0xffff) as i32)
}

fn pattern(path: &Path) -> io::Result<Vec<u16>> {
    let path = std::path::absolute(path)?;
    // Win32 wildcards belong only to the appended search pattern. Device paths
    // are not filesystem directory roots and must not escape this conversion.
    if path.components().any(|part| {
        matches!(part, Component::Normal(name)
        if name.encode_wide().any(|c| c == b'*' as u16 || c == b'?' as u16))
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Directory path contains a wildcard",
        ));
    }
    let mut encoded: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .map(|c| if c == b'/' as u16 { b'\\' as u16 } else { c })
        .collect();
    if encoded.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Directory path contains a NUL",
        ));
    }
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Directory path has no Windows root",
        ));
    };
    encoded = match prefix.kind() {
        Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(_, _) => encoded,
        Prefix::Disk(_) => "\\\\?\\".encode_utf16().chain(encoded).collect(),
        Prefix::UNC(_, _) => "\\\\?\\UNC\\"
            .encode_utf16()
            .chain(encoded.into_iter().skip(2))
            .collect(),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unsupported directory root",
            ));
        }
    };
    if encoded.last() != Some(&(b'\\' as u16)) {
        encoded.push(b'\\' as u16);
    }
    encoded.extend([b'*' as u16, 0]);
    Ok(encoded)
}

/// Entries already delivered remain delivered if enumeration subsequently fails.
/// Reparse/offline attributes are returned unchanged for the caller's policy.
pub(super) fn read(
    path: &Path,
    cancel: &CancelToken,
    mut visit: impl FnMut(Entry),
) -> io::Result<()> {
    let cancelled = || {
        if cancel.is_cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Directory scan cancelled",
            ))
        } else {
            Ok(())
        }
    };
    cancelled()?;
    let pattern = pattern(path)?;
    let mut data = WIN32_FIND_DATAW::default();
    let first = |data: &mut WIN32_FIND_DATAW, flags| unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            (data as *mut WIN32_FIND_DATAW).cast(),
            FindExSearchNameMatch,
            None,
            flags,
        )
    };
    let handle = match first(&mut data, FIND_FIRST_EX_LARGE_FETCH) {
        Err(error) if error.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
            first(&mut data, FIND_FIRST_EX_FLAGS(0)).map_err(os_error)?
        }
        result => result.map_err(os_error)?,
    };
    let search = Search(handle);
    loop {
        cancelled()?;
        let end = data
            .cFileName
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(data.cFileName.len());
        let name = &data.cFileName[..end];
        if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
            visit(Entry {
                name: OsString::from_wide(name),
                attributes: data.dwFileAttributes,
                bytes: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
                is_directory: data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
            });
        }
        cancelled()?;
        match unsafe { FindNextFileW(search.0, &mut data) } {
            Ok(()) => {}
            Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => {
                return Ok(());
            }
            Err(error) => return Err(os_error(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enumeration_reports_names_sizes_directories_and_cancels_between_entries() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("数据.bin"), [1, 2, 3, 4, 5]).unwrap();
        std::fs::create_dir(root.path().join("empty")).unwrap();
        let mut entries = Vec::new();
        read(root.path(), &CancelToken::new(), |entry| {
            entries.push(entry)
        })
        .unwrap();
        assert_eq!(entries.len(), 2);
        let file = entries
            .iter()
            .find(|entry| entry.name == "数据.bin")
            .unwrap();
        assert_eq!(file.bytes, 5);
        assert!(!file.is_directory);
        let directory = entries.iter().find(|entry| entry.name == "empty").unwrap();
        assert!(directory.is_directory);
        assert_ne!(directory.attributes & FILE_ATTRIBUTE_DIRECTORY.0, 0);
        let mut empty_entries = 0;
        read(&root.path().join("empty"), &CancelToken::new(), |_| {
            empty_entries += 1
        })
        .unwrap();
        assert_eq!(empty_entries, 0);
        assert!(read(&root.path().join("missing"), &CancelToken::new(), |_| {}).is_err());
        assert!(read(&root.path().join("数据.bin"), &CancelToken::new(), |_| {}).is_err());
        let cancel = CancelToken::new();
        let mut delivered = 0;
        assert_eq!(
            read(root.path(), &cancel, |_| {
                delivered += 1;
                cancel.cancel();
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(
            delivered, 1,
            "Previously delivered entries must survive cancellation"
        );
        assert_eq!(
            read(root.path(), &cancel, |_| panic!(
                "Cancelled scan delivered an entry"
            ))
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
    }
    #[test]
    fn extended_search_patterns_preserve_unicode_and_unc_roots() {
        let decoded = |path: &str| String::from_utf16(&pattern(Path::new(path)).unwrap()).unwrap();
        assert_eq!(decoded("C:\\数据\\folder"), "\\\\?\\C:\\数据\\folder\\*\0");
        assert_eq!(decoded("C:\\"), "\\\\?\\C:\\*\0");
        assert_eq!(
            decoded("\\\\server\\share\\folder"),
            "\\\\?\\UNC\\server\\share\\folder\\*\0"
        );
        assert_eq!(decoded("\\\\?\\C:\\folder"), "\\\\?\\C:\\folder\\*\0");
        assert!(pattern(Path::new("C:\\bad*name")).is_err());
        assert!(pattern(Path::new("C:\\bad\0name")).is_err());
    }
}
