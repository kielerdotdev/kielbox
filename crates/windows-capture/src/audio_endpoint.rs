//! Read-only endpoint discovery. Call on a worker; no audio stream is activated.
use crate::audio::ComApartment;
use anyhow::{ensure, Context, Result};
use rcr_media_core::{
    validate_audio_endpoint_id, AudioSource, AudioTrack, MAX_AUDIO_ENDPOINT_ID_UNITS,
};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
};
use windows::{
    core::{Interface, PCWSTR, PWSTR},
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::*,
        System::{
            Com::{
                CoCreateInstance, CoTaskMemFree,
                StructuredStorage::{PropVariantClear, PROPVARIANT},
                CLSCTX_ALL, STGM_READ,
            },
            Variant::VT_LPWSTR,
        },
    },
};

pub const MAX_AUDIO_ENDPOINTS: usize = 128;
pub const MAX_AUDIO_ENDPOINT_NAME_CHARS: usize = 256;
static ENUMERATION_ACTIVE: AtomicBool = AtomicBool::new(false);
#[derive(Debug)]
pub struct AudioEndpointEnumerationBusy;
impl std::fmt::Display for AudioEndpointEnumerationBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Audio devices are already being refreshed. Try again shortly")
    }
}
impl std::error::Error for AudioEndpointEnumerationBusy {}
struct EnumerationPermit;
impl EnumerationPermit {
    fn acquire() -> Result<Self> {
        if ENUMERATION_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(AudioEndpointEnumerationBusy.into());
        }
        Ok(Self)
    }
}
impl Drop for EnumerationPermit {
    fn drop(&mut self) {
        ENUMERATION_ACTIVE.store(false, Ordering::Release);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioEndpointKind {
    Render,
    Capture,
}
impl AudioEndpointKind {
    fn flow(self) -> EDataFlow {
        match self {
            Self::Render => eRender,
            Self::Capture => eCapture,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Render => "desktop audio",
            Self::Capture => "microphone",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioEndpoint {
    pub id: String,
    pub name: String,
    pub kind: AudioEndpointKind,
    pub is_default: bool,
}
#[derive(Clone, Debug, Default)]
pub struct AudioEndpointList {
    pub endpoints: Vec<AudioEndpoint>,
    pub truncated: bool,
    /// Endpoints that disappeared or could not be read during enumeration.
    pub unavailable_entries: usize,
}
struct TaskString(PWSTR);
impl Drop for TaskString {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0 .0.cast()));
        }
    }
}
struct PropertyValue(PROPVARIANT);
impl Drop for PropertyValue {
    fn drop(&mut self) {
        unsafe {
            let _ = PropVariantClear(&mut self.0);
        }
    }
}

/// The pointer is an owned MMDevice/property-store null-terminated UTF-16 value.
unsafe fn bounded_wide(value: PCWSTR, limit: usize) -> Result<String> {
    ensure!(
        !value.0.is_null(),
        "Audio endpoint returned an empty string pointer"
    );
    for length in 0..=limit {
        if *value.0.add(length) == 0 {
            return String::from_utf16(std::slice::from_raw_parts(value.0, length))
                .context("Audio endpoint returned invalid UTF-16");
        }
    }
    anyhow::bail!("Audio endpoint string exceeds its bounded length")
}
fn endpoint_id(endpoint: &IMMDevice) -> Result<String> {
    let owned = TaskString(unsafe { endpoint.GetId()? });
    let id = unsafe { bounded_wide(PCWSTR(owned.0 .0), MAX_AUDIO_ENDPOINT_ID_UNITS) }?;
    validate_audio_endpoint_id(&id)?;
    Ok(id)
}
fn friendly_name(endpoint: &IMMDevice, kind: AudioEndpointKind) -> String {
    let read = (|| -> Result<String> {
        unsafe {
            let properties = endpoint.OpenPropertyStore(STGM_READ)?;
            let value = PropertyValue(properties.GetValue(&PKEY_Device_FriendlyName)?);
            ensure!(
                value.0.Anonymous.Anonymous.vt == VT_LPWSTR,
                "Endpoint friendly name is unavailable"
            );
            bounded_wide(
                PCWSTR(value.0.Anonymous.Anonymous.Anonymous.pwszVal.0),
                4096,
            )
        }
    })();
    let fallback = match kind {
        AudioEndpointKind::Render => "Audio output",
        AudioEndpointKind::Capture => "Audio input",
    };
    let name = read.unwrap_or_else(|_| fallback.into());
    let name: String = name
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(MAX_AUDIO_ENDPOINT_NAME_CHARS)
        .collect();
    if name.trim().is_empty() {
        fallback.into()
    } else {
        name
    }
}
fn validate_endpoint_policy(
    expected: AudioEndpointKind,
    actual: EDataFlow,
    active: bool,
) -> Result<()> {
    ensure!(
        active,
        "Selected {} device is unavailable. Reconnect it or choose another device",
        expected.label()
    );
    ensure!(
        actual == expected.flow(),
        "Selected {} device has the wrong audio direction. Choose another device",
        expected.label()
    );
    Ok(())
}
fn inspect(
    endpoint: &IMMDevice,
    kind: AudioEndpointKind,
    default_id: Option<&str>,
) -> Result<AudioEndpoint> {
    unsafe {
        validate_endpoint_policy(
            kind,
            endpoint.cast::<IMMEndpoint>()?.GetDataFlow()?,
            endpoint.GetState()?.0 & DEVICE_STATE_ACTIVE.0 != 0,
        )?;
    }
    let id = endpoint_id(endpoint)?;
    Ok(AudioEndpoint {
        is_default: default_id == Some(id.as_str()),
        id,
        name: friendly_name(endpoint, kind),
        kind,
    })
}

/// Plain-data discovery only: no Activate/Initialize/Start/GetBuffer calls.
/// Windows default is the existing eConsole role, resolved afresh on capture start.
pub fn enumerate_audio_endpoints(kind: AudioEndpointKind) -> Result<AudioEndpointList> {
    enumerate_audio_endpoints_cancellable(kind, || false)
}
pub fn enumerate_audio_endpoints_cancellable(
    kind: AudioEndpointKind,
    cancelled: impl Fn() -> bool,
) -> Result<AudioEndpointList> {
    ensure!(!cancelled(), "Audio endpoint enumeration cancelled");
    // Held until all COM objects and this MTA balance are actually dropped.
    // A cancelled UI cannot create unbounded blocked MMDevice workers.
    let _permit = EnumerationPermit::acquire()?;
    let _com = ComApartment::new().context("Initialize audio endpoint enumeration MTA")?;
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    ensure!(!cancelled(), "Audio endpoint enumeration cancelled");
    let collection = unsafe { enumerator.EnumAudioEndpoints(kind.flow(), DEVICE_STATE_ACTIVE)? };
    let count = unsafe { collection.GetCount()? } as usize;
    let mut result = AudioEndpointList {
        truncated: count > MAX_AUDIO_ENDPOINTS,
        ..Default::default()
    };
    ensure!(!cancelled(), "Audio endpoint enumeration cancelled");
    let default = unsafe { enumerator.GetDefaultAudioEndpoint(kind.flow(), eConsole) }.ok();
    let default_id = default
        .as_ref()
        .and_then(|endpoint| endpoint_id(endpoint).ok());
    let mut seen = BTreeSet::new();
    // Preserve the current default even if its index is beyond the bounded scan.
    if let Some(endpoint) = default {
        match inspect(&endpoint, kind, default_id.as_deref()) {
            Ok(entry) => {
                seen.insert(entry.id.clone());
                result.endpoints.push(entry);
            }
            Err(_) => result.unavailable_entries += 1,
        }
    }
    for index in 0..count.min(MAX_AUDIO_ENDPOINTS) {
        ensure!(!cancelled(), "Audio endpoint enumeration cancelled");
        if result.endpoints.len() >= MAX_AUDIO_ENDPOINTS {
            break;
        }
        let entry = (|| -> Result<_> {
            let endpoint = unsafe { collection.Item(index as u32)? };
            inspect(&endpoint, kind, default_id.as_deref())
        })();
        match entry {
            Ok(entry) if seen.insert(entry.id.clone()) => result.endpoints.push(entry),
            Ok(_) => {}
            Err(_) => result.unavailable_entries += 1,
        }
    }
    ensure!(!cancelled(), "Audio endpoint enumeration cancelled");
    result
        .endpoints
        .sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.id.clone()));
    Ok(result)
}

fn track_kind(track: &AudioTrack) -> Result<AudioEndpointKind> {
    track.validate_endpoint_selection()?;
    match track.source {
        AudioSource::System => Ok(AudioEndpointKind::Render),
        AudioSource::Microphone => Ok(AudioEndpointKind::Capture),
        AudioSource::Process { .. } => {
            anyhow::bail!("Application audio does not use a hardware endpoint")
        }
    }
}
/// Worker-local COM interface. Explicit IDs never fall back to Windows default.
pub(crate) fn resolve_endpoint(
    enumerator: &IMMDeviceEnumerator,
    track: &AudioTrack,
) -> Result<IMMDevice> {
    let kind = track_kind(track)?;
    let endpoint = unsafe {
        if let Some(id) = &track.endpoint_id {
            let id: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
            enumerator.GetDevice(PCWSTR(id.as_ptr())).with_context(|| {
                format!(
                    "Selected {} device is unavailable. Reconnect it or choose another device",
                    kind.label()
                )
            })?
        } else {
            enumerator
                .GetDefaultAudioEndpoint(kind.flow(), eConsole)
                .with_context(|| {
                    format!("Windows default {} device is unavailable", kind.label())
                })?
        }
    };
    unsafe {
        validate_endpoint_policy(
            kind,
            endpoint.cast::<IMMEndpoint>()?.GetDataFlow()?,
            endpoint.GetState()?.0 & DEVICE_STATE_ACTIVE.0 != 0,
        )?;
    }
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "opt-in read-only native MMDevice resolution; never activates audio"]
    fn native_explicit_endpoint_resolution_is_read_only() -> Result<()> {
        // Discovery acquires/releases its own admission and MTA first. Never
        // hold our verification permit across a nested enumeration call.
        let lists = [
            (
                AudioEndpointKind::Render,
                enumerate_audio_endpoints(AudioEndpointKind::Render)?,
            ),
            (
                AudioEndpointKind::Capture,
                enumerate_audio_endpoints(AudioEndpointKind::Capture)?,
            ),
        ];
        let _permit = EnumerationPermit::acquire()?;
        let _com = ComApartment::new()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let mut summaries = Vec::new();
        let mut resolved_total = 0usize;
        for (kind, list) in lists {
            let mut resolved = 0usize;
            let mut mismatches = 0usize;
            for entry in list.endpoints.iter().take(2) {
                let track = match kind {
                    AudioEndpointKind::Render => AudioTrack::system(),
                    AudioEndpointKind::Capture => AudioTrack::microphone(),
                }
                .with_endpoint(entry.id.clone())?;
                let endpoint = resolve_endpoint(&enumerator, &track)
                    .context("Read-only listed endpoint resolution failed")?;
                let actual = inspect(&endpoint, kind, None)?;
                ensure!(
                    actual.id == entry.id && actual.kind == entry.kind,
                    "Listed endpoint resolved to a different identity or direction"
                );
                resolved += 1;
                let wrong = match kind {
                    AudioEndpointKind::Render => AudioTrack::microphone(),
                    AudioEndpointKind::Capture => AudioTrack::system(),
                }
                .with_endpoint(entry.id.clone())?;
                ensure!(
                    resolve_endpoint(&enumerator, &wrong).is_err(),
                    "Opposite-direction endpoint was accepted"
                );
                mismatches += 1;
            }
            // Synthetic test-only endpoint-shaped ID; never reconstruct or
            // normalize a real endpoint ID. A new UUID avoids name/ID collisions.
            let guid = unsafe { windows::Win32::System::Com::CoCreateGuid()? };
            let missing = format!(
                "{{0.0.{}.00000000}}.{{{guid:?}}}",
                if kind == AudioEndpointKind::Render {
                    0
                } else {
                    1
                }
            );
            validate_audio_endpoint_id(&missing)?;
            ensure!(
                !list.endpoints.iter().any(|entry| entry.id == missing),
                "Synthetic missing ID unexpectedly exists"
            );
            let track = match kind {
                AudioEndpointKind::Render => AudioTrack::system(),
                AudioEndpointKind::Capture => AudioTrack::microphone(),
            }
            .with_endpoint(missing)?;
            ensure!(
                resolve_endpoint(&enumerator, &track).is_err(),
                "Missing explicit endpoint silently fell back to another device"
            );

            let default_track = match kind {
                AudioEndpointKind::Render => AudioTrack::system(),
                AudioEndpointKind::Capture => AudioTrack::microphone(),
            };
            let default_available =
                match unsafe { enumerator.GetDefaultAudioEndpoint(kind.flow(), eConsole) } {
                    Ok(expected) => {
                        let actual = resolve_endpoint(&enumerator, &default_track)
                            .context("Read-only Windows default resolution failed")?;
                        ensure!(
                            endpoint_id(&actual)? == endpoint_id(&expected)?,
                            "None did not resolve the current eConsole default"
                        );
                        inspect(&actual, kind, None)?;
                        true
                    }
                    Err(_) => {
                        ensure!(
                            resolve_endpoint(&enumerator, &default_track).is_err(),
                            "Unavailable Windows default unexpectedly resolved"
                        );
                        false
                    }
                };
            resolved_total += resolved;
            summaries.push(serde_json::json!({"kind":if kind==AudioEndpointKind::Render{"render"}else{"capture"},"enumerated":list.endpoints.len(),"explicit_resolved":resolved,"opposite_direction_rejected":mismatches,"synthetic_missing_rejected":1,"default_resolved":usize::from(default_available),"default_unavailable":usize::from(!default_available),"flow_without_listed_devices":usize::from(list.endpoints.is_empty()),"truncated":list.truncated,"unavailable_entries":list.unavailable_entries}));
        }
        println!(
            "{}",
            serde_json::json!({"proof":"read_only_explicit_endpoint_resolution","result":if resolved_total>0{"verified"}else{"unavailable_no_active_endpoints"},"explicit_resolved_total":resolved_total,"flows":summaries,"streams_activated":0,"captured_samples":0,"opaque_ids_logged":false})
        );
        ensure!(resolved_total>0,"Read-only endpoint verification unavailable: no active listed endpoint could be resolved");
        Ok(())
    }
    #[test]
    fn enumeration_admission_remains_exclusive_until_worker_scope_retires() {
        let permit = EnumerationPermit::acquire().unwrap();
        assert!(EnumerationPermit::acquire()
            .err()
            .unwrap()
            .is::<AudioEndpointEnumerationBusy>());
        drop(permit);
        drop(EnumerationPermit::acquire().unwrap());
    }
    #[test]
    fn endpoint_direction_and_activity_are_checked_before_capture() {
        assert!(validate_endpoint_policy(AudioEndpointKind::Render, eRender, true).is_ok());
        assert!(validate_endpoint_policy(AudioEndpointKind::Capture, eCapture, true).is_ok());
        assert!(validate_endpoint_policy(AudioEndpointKind::Render, eCapture, true).is_err());
        assert!(validate_endpoint_policy(AudioEndpointKind::Capture, eRender, true).is_err());
        assert!(validate_endpoint_policy(AudioEndpointKind::Capture, eCapture, false).is_err());
        assert_eq!(
            track_kind(&AudioTrack::microphone()).unwrap(),
            AudioEndpointKind::Capture
        );
        assert_eq!(
            track_kind(&AudioTrack::system().with_endpoint("Exact-ID").unwrap()).unwrap(),
            AudioEndpointKind::Render
        );
        assert!(track_kind(&AudioTrack::process(42).unwrap()).is_err());
    }
    #[test]
    fn bounded_utf16_preserves_identity_and_rejects_malformed_or_long_values() {
        let wide: Vec<u16> = "  设备-😀-MiXeD  ".encode_utf16().chain(Some(0)).collect();
        assert_eq!(
            unsafe { bounded_wide(PCWSTR(wide.as_ptr()), 64) }.unwrap(),
            "  设备-😀-MiXeD  "
        );
        assert!(unsafe { bounded_wide(PCWSTR(wide.as_ptr()), 2) }.is_err());
        let invalid = [0xd800, 0];
        assert!(unsafe { bounded_wide(PCWSTR(invalid.as_ptr()), 8) }.is_err());
    }
}
