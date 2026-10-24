//! Read-only display discovery and exact physical-display selection.
//! Enumeration never creates D3D devices, duplication sessions, encoders or pixel buffers.
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{collections::BTreeMap, mem::size_of};
use windows::{
    Win32::{
        Devices::Display::*,
        Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
        Graphics::{
            Dxgi::{Common::*, *},
            Gdi::{GetMonitorInfoW, MONITORINFO},
        },
    },
    core::Interface,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Primary,
    DevicePath(String),
}
#[derive(Clone, Debug, Serialize)]
pub struct Info {
    pub device_path: Option<String>,
    pub label: String,
    pub device_name: String,
    pub gpu_name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub unsupported_reason: Option<String>,
}
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    pub info: Info,
    pub adapter_luid: (u32, i32),
    pub output_name: [u16; 32],
}
#[derive(Clone, Debug)]
struct Target {
    path: Option<String>,
    label: String,
}
fn utf16(text: &[u16]) -> String {
    String::from_utf16_lossy(&text[..text.iter().position(|c| *c == 0).unwrap_or(text.len())])
}
/// Opaque API identities must be valid, explicitly terminated UTF-16.
/// Display labels may be repaired for presentation; identities must never be repaired or normalized.
fn identity(text: &[u16]) -> Option<String> {
    let end = text.iter().position(|c| *c == 0)?;
    if end == 0 {
        return None;
    }
    String::from_utf16(&text[..end]).ok()
}
fn label(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect::<String>()
        .trim()
        .to_owned()
}
type ActiveTargets = BTreeMap<((u32, i32), String), Vec<Target>>;
fn active_targets() -> Result<ActiveTargets> {
    unsafe {
        // Display topology may change between sizing and querying. Bound retries and allocations.
        for _ in 0..4 {
            let (mut count, mut mode_count) = (0, 0);
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut count, &mut mode_count)
                .ok()
                .context("Could not read connected displays")?;
            ensure!(
                count <= 256 && mode_count <= 1024,
                "Display topology exceeded the discovery limit"
            );
            if count == 0 {
                return Ok(BTreeMap::new());
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); count as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            let result = QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            );
            if result == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            result.ok().context("Could not query active displays")?;
            ensure!(
                count as usize <= paths.len(),
                "Display topology changed during discovery"
            );
            paths.truncate(count as usize);
            let mut targets = BTreeMap::<_, Vec<Target>>::new();
            for path in paths {
                let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
                source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
                source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
                source.header.adapterId = path.sourceInfo.adapterId;
                source.header.id = path.sourceInfo.id;
                ensure!(
                    DisplayConfigGetDeviceInfo(&mut source.header) == ERROR_SUCCESS.0 as i32,
                    "Could not identify a connected display source; refresh the display list"
                );
                let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
                target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
                target.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
                target.header.adapterId = path.targetInfo.adapterId;
                target.header.id = path.targetInfo.id;
                let known =
                    DisplayConfigGetDeviceInfo(&mut target.header) == ERROR_SUCCESS.0 as i32;
                let device_path = known.then(|| identity(&target.monitorDevicePath)).flatten();
                let name = if known {
                    label(&utf16(&target.monitorFriendlyDeviceName))
                } else {
                    String::new()
                };
                targets
                    .entry((
                        (
                            path.sourceInfo.adapterId.LowPart,
                            path.sourceInfo.adapterId.HighPart,
                        ),
                        identity(&source.viewGdiDeviceName).context("A display source identity is invalid. Refresh or reconnect the display.")?,
                    ))
                    .or_default()
                    .push(Target {
                        path: device_path,
                        label: name,
                    });
            }
            return Ok(targets);
        }
    }
    bail!(
        "Displays kept changing during discovery. Wait for the display layout to settle, then refresh."
    )
}
fn unsupported_reason(
    target_count: usize,
    identified: bool,
    nvidia: bool,
    unrotated: bool,
    sdr: Option<bool>,
    area: (u32, u32),
) -> Option<&'static str> {
    if target_count > 1 {
        Some("Mirrored displays are not supported. Use an extended display.")
    } else if !identified {
        Some("This display could not be identified safely. Refresh or reconnect it.")
    } else if !nvidia {
        Some(
            "This display is connected to another GPU. NVIDIA-attached displays are supported first.",
        )
    } else if !unrotated {
        Some("Rotated displays are not supported. Choose a landscape display.")
    } else if sdr.is_none() {
        Some("The display color mode could not be verified. Refresh the display list.")
    } else if sdr == Some(false) {
        Some(
            "Unsupported display color mode. Choose standard SDR for this display or select another display.",
        )
    } else if area.0 == 0 || area.1 == 0 {
        Some("The display has no active capture area.")
    } else {
        None
    }
}
fn records() -> Result<Vec<Resolved>> {
    let targets = active_targets()?;
    let mut rows = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        for ai in 0..32 {
            let adapter = match factory.EnumAdapters1(ai) {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            let desc = adapter.GetDesc1()?;
            let luid = (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart);
            for oi in 0..32 {
                let output = match adapter.EnumOutputs(oi) {
                    Ok(o) => o,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(e.into()),
                };
                let od = output.GetDesc()?;
                if !od.AttachedToDesktop.as_bool() {
                    continue;
                }
                let device_name = identity(&od.DeviceName).context(
                    "A DXGI display identity is invalid. Refresh or reconnect the display.",
                )?;
                let matches = targets.get(&(luid, device_name.clone()));
                let target = matches
                    .filter(|entries| entries.len() == 1)
                    .and_then(|entries| entries.first());
                let path = target.and_then(|entry| entry.path.clone());
                let width =
                    (od.DesktopCoordinates.right - od.DesktopCoordinates.left).max(0) as u32;
                let height =
                    (od.DesktopCoordinates.bottom - od.DesktopCoordinates.top).max(0) as u32;
                let mut monitor = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                ensure!(
                    GetMonitorInfoW(od.Monitor, &mut monitor).as_bool(),
                    "Could not identify the primary display; refresh the display list"
                );
                let color = output
                    .cast::<IDXGIOutput6>()
                    .ok()
                    .and_then(|output| output.GetDesc1().ok())
                    .map(|d| d.ColorSpace);
                let reason = unsupported_reason(
                    matches.map_or(0, Vec::len),
                    path.is_some(),
                    desc.VendorId == 0x10de,
                    matches!(
                        od.Rotation,
                        DXGI_MODE_ROTATION_IDENTITY | DXGI_MODE_ROTATION_UNSPECIFIED
                    ),
                    color.map(|color| color == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709),
                    (width, height),
                )
                .map(str::to_owned);
                let friendly = target
                    .map(|t| t.label.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| device_name.clone());
                rows.push(Resolved {
                    info: Info {
                        device_path: path,
                        label: friendly,
                        device_name,
                        gpu_name: label(&utf16(&desc.Description)),
                        width,
                        height,
                        primary: monitor.dwFlags & 1 != 0,
                        unsupported_reason: reason,
                    },
                    adapter_luid: luid,
                    output_name: od.DeviceName,
                });
            }
            match adapter.EnumOutputs(32) {
                Ok(_) => bail!(
                    "Display discovery exceeded 32 outputs on one adapter. The list is incomplete; capture selection is unavailable."
                ),
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => {}
                Err(error) => return Err(error.into()),
            }
        }
        match factory.EnumAdapters1(32) {
            Ok(_) => bail!(
                "Display discovery exceeded 32 adapters. The list is incomplete; capture selection is unavailable."
            ),
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => {}
            Err(error) => return Err(error.into()),
        }
    }
    // A driver returning the same monitor path twice is ambiguous, even if names differ.
    let mut counts = BTreeMap::<String, usize>::new();
    for row in &rows {
        if let Some(path) = &row.info.device_path {
            *counts.entry(path.clone()).or_default() += 1;
        }
    }
    for row in &mut rows {
        if row
            .info
            .device_path
            .as_ref()
            .is_some_and(|path| counts[path] > 1)
        {
            row.info.unsupported_reason =
                Some("The display identity is ambiguous. Refresh or reconnect it.".into());
        }
    }
    rows.sort_by_key(|row| (!row.info.primary, row.info.device_name.clone()));
    Ok(rows)
}
/// Lists active outputs, including unsupported rows with a specific explanation.
pub fn enumerate() -> Result<Vec<Info>> {
    Ok(records()?.into_iter().map(|record| record.info).collect())
}
fn select<'a>(rows: &'a [Resolved], selection: &Selection) -> Result<&'a Resolved> {
    let matching = rows
        .iter()
        .filter(|row| match selection {
            Selection::Primary => row.info.primary,
            Selection::DevicePath(path) => {
                row.info.device_path.as_ref().is_some_and(|id| id == path)
            }
        })
        .collect::<Vec<_>>();
    ensure!(
        matching.len() == 1,
        if matching.is_empty() {
            "The selected display is unavailable. Reconnect it or choose another display."
        } else {
            "The selected display is ambiguous. Refresh and choose a display again."
        }
    );
    let selected = matching[0];
    if let Some(reason) = &selected.info.unsupported_reason {
        bail!("{reason}");
    }
    ensure!(
        selected.info.device_path.is_some(),
        "The selected display has no stable identity"
    );
    Ok(selected)
}
pub(crate) fn resolve(selection: &Selection) -> Result<Resolved> {
    Ok(select(&records()?, selection)?.clone())
}
fn verify_connection(expected: &Resolved, current: &Resolved) -> Result<()> {
    ensure!(
        expected
            .info
            .device_path
            .as_ref()
            .zip(current.info.device_path.as_ref())
            .is_some_and(|(expected, current)| expected == current),
        "The selected physical display changed. Stop capture and select it again."
    );
    ensure!(
        current.adapter_luid == expected.adapter_luid
            && current.output_name == expected.output_name,
        "The selected display connection changed. Stop capture and select it again."
    );
    ensure!(
        current.info.width == expected.info.width && current.info.height == expected.info.height,
        "The selected display size changed. Stop capture and select it again."
    );
    Ok(())
}
pub(crate) fn verify(expected: &Resolved) -> Result<()> {
    let path = expected
        .info
        .device_path
        .as_ref()
        .context("The selected display identity was lost")?;
    let current = resolve(&Selection::DevicePath(path.clone()))?;
    verify_connection(expected, &current)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row(path: &str, primary: bool) -> Resolved {
        Resolved {
            info: Info {
                device_path: Some(path.into()),
                label: "Identical friendly name".into(),
                device_name: path.into(),
                gpu_name: "NVIDIA".into(),
                width: 2560,
                height: 1440,
                primary,
                unsupported_reason: None,
            },
            adapter_luid: (1, 0),
            output_name: [0; 32],
        }
    }
    #[test]
    fn opaque_identity_requires_terminated_valid_utf16_and_preserves_exact_unicode() {
        assert_eq!(identity(&[0xd800, 0]), None);
        assert_eq!(identity(&[0xdc00, 0]), None);
        assert_eq!(identity(&[65, 66]), None);
        assert_eq!(identity(&[0, 65, 0]), None);
        assert_eq!(identity(&[]), None);
        let expected = "  Display-Å-😀  ";
        let mut encoded = expected.encode_utf16().collect::<Vec<_>>();
        encoded.extend([0, 0, 0]);
        assert_eq!(identity(&encoded).as_deref(), Some(expected));
        assert_eq!(identity(&[65, 0, 0xd800]).as_deref(), Some("A"));
    }
    #[test]
    fn identities_are_not_case_folded_or_trimmed_and_duplicates_are_rejected() {
        let rows = [row("Exact-Å", true), row("exact-Å", false)];
        assert!(select(&rows, &Selection::DevicePath(" Exact-Å".into())).is_err());
        assert_eq!(
            select(&rows, &Selection::DevicePath("Exact-Å".into()))
                .unwrap()
                .info
                .device_path
                .as_deref(),
            Some("Exact-Å")
        );
        assert!(
            select(
                &[row("Exact-Å", true), row("Exact-Å", false)],
                &Selection::DevicePath("Exact-Å".into())
            )
            .is_err()
        );
    }
    #[test]
    fn session_connection_cannot_switch_identity_gpu_output_or_geometry() {
        let expected = row("selected", true);
        let mut current = expected.clone();
        current.info.primary = false;
        assert!(verify_connection(&expected, &current).is_ok());
        current.info.device_path = Some("replacement".into());
        assert!(verify_connection(&expected, &current).is_err());
        current = expected.clone();
        current.adapter_luid = (2, 0);
        assert!(verify_connection(&expected, &current).is_err());
        current = expected.clone();
        current.output_name[0] = 1;
        assert!(verify_connection(&expected, &current).is_err());
        current = expected.clone();
        current.info.width = 1920;
        assert!(verify_connection(&expected, &current).is_err());
    }
    #[test]
    fn discovery_classifies_mirror_hdr_other_gpu_and_unknown_color_without_capture() {
        assert!(unsupported_reason(1, true, true, true, Some(true), (2560, 1440)).is_none());
        for (count, identified, nvidia, unrotated, sdr, reason) in [
            (2, true, true, true, Some(true), "Mirrored"),
            (1, false, true, true, Some(true), "identified"),
            (1, true, false, true, Some(true), "another GPU"),
            (1, true, true, false, Some(true), "Rotated"),
            (1, true, true, true, None, "verified"),
            (1, true, true, true, Some(false), "color mode"),
        ] {
            assert!(
                unsupported_reason(count, identified, nvidia, unrotated, sdr, (2560, 1440))
                    .unwrap()
                    .contains(reason)
            );
        }
    }
    #[test]
    fn exact_identity_survives_order_changes_and_duplicate_friendly_names() {
        let mut rows = vec![row("path-A", true), row("path-B", false)];
        rows.reverse();
        assert_eq!(
            select(&rows, &Selection::DevicePath("path-A".into()))
                .unwrap()
                .info
                .device_path
                .as_deref(),
            Some("path-A")
        );
    }
    #[test]
    fn missing_display_never_falls_back_to_primary() {
        assert!(
            select(
                &[row("other", true)],
                &Selection::DevicePath("missing".into())
            )
            .is_err()
        );
    }
    #[test]
    fn primary_unsupported_never_falls_back_to_nvidia_secondary() {
        let mut primary = row("intel", true);
        primary.info.unsupported_reason = Some("Not NVIDIA".into());
        assert!(select(&[primary, row("nvidia", false)], &Selection::Primary).is_err());
    }
    #[test]
    fn ambiguous_and_mirrored_displays_are_blocked() {
        let mut mirror = row("mirror", true);
        mirror.info.unsupported_reason = Some("Mirrored display".into());
        assert!(select(&[mirror], &Selection::Primary).is_err());
        assert!(
            select(
                &[row("same", true), row("same", false)],
                &Selection::DevicePath("same".into())
            )
            .is_err()
        );
    }
    #[test]
    fn unsupported_hdr_or_missing_stable_identity_is_blocked() {
        let mut hdr = row("hdr", true);
        hdr.info.unsupported_reason = Some("HDR unsupported".into());
        assert!(select(&[hdr], &Selection::Primary).is_err());
        let mut unknown = row("x", true);
        unknown.info.device_path = None;
        assert!(select(&[unknown], &Selection::Primary).is_err());
    }
}
