use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{OnceLock, atomic::{AtomicBool, Ordering}},
};
#[path = "preferences_recovery.rs"]
mod recovery;

// Save and recovery must agree: never publish settings that startup will refuse
// to read. Count encoded bytes while streaming, including JSON escaping.
const MAX_PREFERENCES_BYTES: u64 = 8 * 1024 * 1024;

struct SettingsWriter<W> {
    inner: W,
    written: u64,
}
impl<W: Write> Write for SettingsWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > MAX_PREFERENCES_BYTES.saturating_sub(self.written) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                "Settings exceed the 8 MiB save limit; existing settings were preserved"));
        }
        let count = self.inner.write(bytes)?;
        self.written += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> { self.inner.flush() }
}

/// Opaque endpoint identity plus a cached display label for unavailable devices.
/// IDs must never be matched by friendly name or silently replaced at startup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAudioEndpoint {
    pub id: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDisplay { pub id: String, pub label: String }
impl SavedDisplay {
    pub fn display_label(&self) -> String {
        let label: String = self.label.chars().filter(|c| !c.is_control()).take(256).collect();
        if label.trim().is_empty() { "Selected display".into() } else { label }
    }
}

impl SavedAudioEndpoint {
    pub fn display_label(&self) -> String {
        let label: String = self
            .label
            .chars()
            .filter(|ch| !ch.is_control())
            .take(256)
            .collect();
        if label.trim().is_empty() {
            "Selected audio device".into()
        } else {
            label
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePriority {
    EditingSpeed,
    #[default]
    #[serde(other)]
    ImageQuality,
}
impl CapturePriority {
    pub fn label(self) -> &'static str {
        match self { Self::ImageQuality => "Image quality", Self::EditingSpeed => "Editing speed" }
    }
    pub fn keyframe_interval(self, fps: u32) -> u32 {
        match self {
            Self::ImageQuality => fps.saturating_mul(2).max(1),
            Self::EditingSpeed => fps.div_ceil(2).max(1),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    #[cfg(windows)]
    pub updates: crate::updates::Settings,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub capture_priority: CapturePriority,
    pub display: Option<SavedDisplay>,
    pub replay_seconds: u64,
    pub system_audio: bool,
    pub microphone: bool,
    pub system_audio_device: Option<SavedAudioEndpoint>,
    pub microphone_device: Option<SavedAudioEndpoint>,
    pub clean_microphone: bool,
    pub gpu_video_preview: bool,
    pub folder: PathBuf,
    pub favorites: BTreeSet<PathBuf>,
    #[serde(deserialize_with = "deserialize_imported_files")]
    pub imported_files: BTreeSet<PathBuf>,
    #[serde(deserialize_with = "deserialize_display_names")]
    pub display_names: BTreeMap<PathBuf, String>,
    pub list_view: bool,
    pub close_to_tray: bool,
    #[serde(deserialize_with = "deserialize_hotkeys")]
    pub hotkeys: [String; 7],
}
pub fn default_hotkeys() -> [String; 7] {
    [
        "Ctrl+Shift+F9",
        "Ctrl+Shift+F10",
        "Ctrl+Shift+F11",
        "Ctrl+PrintScreen",
        "Shift+PrintScreen",
        "Alt+PrintScreen",
        "Ctrl+Shift+PrintScreen",
    ]
    .map(str::to_owned)
}
fn deserialize_hotkeys<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<[String; 7], D::Error> {
    let saved = Vec::<String>::deserialize(deserializer)?;
    if !(6..=7).contains(&saved.len()) {
        return Err(serde::de::Error::custom("Expected six legacy shortcuts or seven shortcuts"));
    }
    let mut bindings = default_hotkeys();
    for (index, binding) in saved.into_iter().enumerate() { bindings[index] = binding; }
    // A previously customized shortcut may already own the new default.
    if bindings[..6].iter().any(|binding| binding.eq_ignore_ascii_case(&bindings[6])) { bindings[6].clear(); }
    #[cfg(windows)]
    if crate::hotkeys::quick_binding_conflicts(&bindings) { bindings[6].clear(); }
    Ok(bindings)
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            #[cfg(windows)]
            updates: crate::updates::Settings::default(),
            height: 1440,
            fps: 60,
            bitrate: 25_000_000,
            capture_priority: CapturePriority::ImageQuality,
            display: None,
            replay_seconds: 120,
            system_audio: true,
            microphone: false,
            system_audio_device: None,
            microphone_device: None,
            clean_microphone: false,
            gpu_video_preview: false,
            folder: PathBuf::from("recordings"),
            favorites: BTreeSet::new(),
            imported_files: BTreeSet::new(),
            display_names: BTreeMap::new(),
            list_view: false,
            close_to_tray: true,
            hotkeys: default_hotkeys(),
        }
    }
}
impl Preferences {
    /// Capture names are local preferences; source files and project sidecars stay untouched.
    pub fn display_name(&self, path: &Path) -> String {
        self.custom_display_name(path)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
    }

    pub fn custom_display_name(&self, path: &Path) -> Option<&str> {
        self.display_names
            .get(&lookup_key(path))
            .map(String::as_str)
    }

    pub fn set_display_name(&mut self, path: &Path, name: &str) -> Result<(), &'static str> {
        let name = validate_display_name(name)?;
        self.display_names.insert(capture_key(path), name);
        Ok(())
    }

    pub fn load() -> Self {
        let mut p = preference_load().preferences.clone();
        if ![1080, 1440, 2160].contains(&p.height) {
            p.height = 1440;
        }
        if ![30, 60, 120].contains(&p.fps) {
            p.fps = 60;
        }
        if ![15_000_000, 25_000_000, 50_000_000].contains(&p.bitrate) {
            p.bitrate = 25_000_000;
        }
        if ![30, 60, 120, 300].contains(&p.replay_seconds) {
            p.replay_seconds = 120;
        }
        if p.folder.as_os_str().is_empty() {
            p.folder = PathBuf::from("recordings");
        }
        // Seed the single owner from the already loaded/sanitized snapshot.
        // No later paint or Library layout click reopens preferences.json.
        let _ = preference_writer(&p);
        p
    }
    pub fn save(&self) -> anyhow::Result<()> {
        preference_writer(self)?.save_barrier(self).map_err(anyhow::Error::msg)
    }
    pub(crate) fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        atomic_write(path, |temp| {
            serde_json::to_writer_pretty(SettingsWriter { inner: temp, written: 0 }, self)?;
            Ok(())
        })
    }
}

fn atomic_write(path: &Path, write: impl FnOnce(&mut tempfile::NamedTempFile) -> anyhow::Result<()>) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    write(&mut temp)?;
    temp.flush()?;
    temp.persist(path)?;
    Ok(())
}

static PREFERENCE_WRITER: OnceLock<Result<crate::preferences_io::PreferenceWriter, String>> = OnceLock::new();
static PREFERENCE_LOAD: OnceLock<recovery::LoadedPreferences> = OnceLock::new();

fn preference_load() -> &'static recovery::LoadedPreferences {
    PREFERENCE_LOAD.get_or_init(|| recovery::load(Path::new("projects/preferences.json")))
}

/// Startup consumes this after `Preferences::load`; later save failures retain
/// their existing barrier/layout feedback rather than replacing this notice.
pub fn take_startup_notice() -> Option<String> {
    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::AcqRel) { return None; }
    let mut notice = preference_load().notice.clone();
    if let Some(Err(error)) = PREFERENCE_WRITER.get() {
        let detail = format!("Settings cannot be saved: {error}. Restart RCR to retry.");
        notice = Some(notice.map_or_else(|| detail.clone(), |notice| format!("{notice}\n{detail}")));
    }
    notice
}

fn preference_writer(initial: &Preferences) -> anyhow::Result<&'static crate::preferences_io::PreferenceWriter> {
    PREFERENCE_WRITER.get_or_init(|| preference_load().start_writer(initial.clone()))
        .as_ref().map_err(|error| anyhow::anyhow!(error.clone()))
}

pub fn set_list_view(initial: &Preferences, list_view: bool) -> Result<u64, String> {
    preference_writer(initial).map_err(|error| error.to_string())?.set_list_view(list_view)
}

pub fn take_layout_ack(initial: &Preferences) -> Option<crate::preferences_io::LayoutAck> {
    preference_writer(initial).ok()?.take_layout_ack()
}

pub fn finish_pending() -> Result<(), String> {
    if let Some(Ok(writer)) = PREFERENCE_WRITER.get()
    {
        return writer.finish_pending(std::time::Duration::from_secs(2));
    }
    Ok(())
}

pub const MAX_DISPLAY_NAME_CHARS: usize = 128;
pub const MAX_IMPORTED_FILES: usize = 512;

fn deserialize_imported_files<'de, D>(deserializer: D) -> Result<BTreeSet<PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(BTreeSet::<PathBuf>::deserialize(deserializer)?
        .into_iter()
        .filter(|path| !path.as_os_str().is_empty())
        .take(MAX_IMPORTED_FILES)
        .collect())
}

pub fn validate_display_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a name for this capture.");
    }
    if name.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err("Keep the name to 128 characters or fewer.");
    }
    if name
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
    {
        return Err("Use a single line without control characters.");
    }
    Ok(name.to_owned())
}

fn capture_key(path: &Path) -> PathBuf {
    lookup_key(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

/// Render-time lookup must never stat media. Resolve relative paths against one
/// process base and normalize Windows canonical prefixes entirely in memory.
pub(crate) fn lookup_key(path: &Path) -> PathBuf {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    let absolute;
    let path = if path.is_absolute() {
        path
    } else {
        absolute = BASE
            .get_or_init(|| std::env::current_dir().unwrap_or_default())
            .join(path);
        &absolute
    };
    let mut key = PathBuf::new();
    for component in path.components() {
        match component {
            #[cfg(windows)]
            Component::Prefix(prefix) => {
                use std::{ffi::OsString, path::Prefix};
                match prefix.kind() {
                    Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                        key.push(format!("{}:", drive.to_ascii_uppercase() as char));
                    }
                    Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                        let mut ordinary = OsString::from("\\\\");
                        ordinary.push(server);
                        ordinary.push("\\");
                        ordinary.push(share);
                        key.push(ordinary);
                    }
                    _ => key.push(component.as_os_str()),
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(key.components().next_back(), Some(Component::Normal(_))) {
                    key.pop();
                } else if !key.has_root() {
                    key.push("..");
                }
            }
            _ => key.push(component.as_os_str()),
        }
    }
    key
}

fn deserialize_display_names<'de, D>(deserializer: D) -> Result<BTreeMap<PathBuf, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let names = BTreeMap::<PathBuf, String>::deserialize(deserializer)?;
    Ok(names
        .into_iter()
        .filter_map(|(path, name)| {
            if path.as_os_str().is_empty() {
                return None;
            }
            validate_display_name(&name)
                .ok()
                .map(|name| (capture_key(&path), name))
        })
        .collect())
}
#[cfg(test)]
mod tests {
    #[test]
    fn legacy_shortcuts_keep_customizations_when_quick_recorder_is_added() {
        let legacy = ["Ctrl+Alt+F1", "", "Ctrl+F3", "Ctrl+PrintScreen", "Shift+PrintScreen", "Alt+PrintScreen"];
        let loaded: super::Preferences = serde_json::from_value(serde_json::json!({ "hotkeys": legacy, "replay_seconds": 300 })).unwrap();
        assert_eq!(&loaded.hotkeys[..6], &legacy.map(str::to_owned));
        assert_eq!(loaded.hotkeys[6], "Ctrl+Shift+PrintScreen");
        assert_eq!(loaded.replay_seconds, 300);
        let restored: super::Preferences = serde_json::from_slice(&serde_json::to_vec(&loaded).unwrap()).unwrap();
        assert_eq!(restored.hotkeys, loaded.hotkeys);
    }
    #[cfg(windows)]
    #[test]
    fn migration_disables_new_binding_if_a_legacy_action_owns_it() {
        let legacy = ["Shift+Control+Snapshot", "", "Ctrl+F3", "Ctrl+PrintScreen", "Shift+PrintScreen", "Alt+PrintScreen"];
        let loaded: super::Preferences = serde_json::from_value(serde_json::json!({ "hotkeys": legacy })).unwrap();
        assert_eq!(loaded.hotkeys[0], legacy[0]);
        assert!(loaded.hotkeys[6].is_empty());
        assert!(crate::hotkeys::validate(&loaded.hotkeys).is_ok());
    }
    use super::*;
    #[test]
    fn saved_display_migrates_to_primary_policy_and_roundtrips_opaque_identity() {
        let legacy: Preferences = serde_json::from_str(r#"{"fps":60,"microphone":true}"#).unwrap();
        assert!(legacy.display.is_none());
        assert!(legacy.microphone);
        let saved = SavedDisplay { id: r"\\?\DISPLAY#OpaqueCaseSensitive#Port-A".into(), label: "Desk\nmonitor".into() };
        let preferences = Preferences { display: Some(saved.clone()), ..Preferences::default() };
        let restored: Preferences = serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert_eq!(restored.display, Some(saved.clone()));
        assert_eq!(saved.display_label(), "Deskmonitor");
        assert_eq!(SavedDisplay { id: "missing".into(), label: "\n\t".into() }.display_label(), "Selected display");
        assert_eq!(SavedDisplay { id: "missing".into(), label: "🖥".repeat(300) }.display_label().chars().count(), 256);
    }
    #[test]
    fn capture_priority_migration_keeps_existing_image_quality_and_unrelated_settings() {
        let legacy: Preferences = serde_json::from_str(r#"{"fps":120,"bitrate":50000000,"microphone":true,"folder":"existing-captures"}"#).unwrap();
        assert_eq!(legacy.capture_priority, CapturePriority::ImageQuality);
        assert_eq!(legacy.capture_priority.keyframe_interval(legacy.fps), 240);
        assert_eq!(legacy.bitrate, 50_000_000);
        assert!(legacy.microphone);
        assert_eq!(legacy.folder, PathBuf::from("existing-captures"));
        let future: Preferences = serde_json::from_str(r#"{"capture_priority":"future_mode","fps":30,"microphone":true}"#).unwrap();
        assert_eq!(future.capture_priority, CapturePriority::ImageQuality);
        assert_eq!(future.fps, 30);
        assert!(future.microphone, "Unknown future priority discarded unrelated preferences");
        let preferences = Preferences { capture_priority: CapturePriority::EditingSpeed, ..Preferences::default() };
        let json = serde_json::to_string(&preferences).unwrap();
        assert!(json.contains(r#""capture_priority":"editing_speed""#));
        let restored: Preferences = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.capture_priority, CapturePriority::EditingSpeed);
        assert_eq!(restored.capture_priority.keyframe_interval(restored.fps), 30);
        assert_eq!(Preferences::default().capture_priority, CapturePriority::ImageQuality);
    }
    #[test]
    fn capture_priority_intervals_preserve_two_seconds_or_round_half_second_up() {
        for (fps, quality, editing) in [(30, 60, 15), (60, 120, 30), (120, 240, 60), (59, 118, 30), (61, 122, 31), (1, 2, 1), (0, 1, 1)] {
            assert_eq!(CapturePriority::ImageQuality.keyframe_interval(fps), quality);
            assert_eq!(CapturePriority::EditingSpeed.keyframe_interval(fps), editing);
        }
        assert_eq!(CapturePriority::ImageQuality.keyframe_interval(u32::MAX), u32::MAX);
        assert_eq!(CapturePriority::EditingSpeed.keyframe_interval(u32::MAX), 2_147_483_648);
    }
    #[test]
    fn endpoint_choices_default_to_windows_and_preserve_exact_distinct_ids() {
        let old: Preferences = serde_json::from_str("{}").unwrap();
        assert!(old.system_audio_device.is_none() && old.microphone_device.is_none());
        let configured = Preferences {
            system_audio_device: Some(SavedAudioEndpoint {
                id: "{render-opaque-A}".into(),
                label: "USB Audio".into(),
            }),
            microphone_device: Some(SavedAudioEndpoint {
                id: "{capture-opaque-B}".into(),
                label: "USB Audio".into(),
            }),
            ..Default::default()
        };
        let restored: Preferences =
            serde_json::from_slice(&serde_json::to_vec(&configured).unwrap()).unwrap();
        assert_eq!(restored.system_audio_device, configured.system_audio_device);
        assert_eq!(restored.microphone_device, configured.microphone_device);
        assert_ne!(
            restored.system_audio_device.as_ref().unwrap().id,
            restored.microphone_device.as_ref().unwrap().id
        );
        assert_eq!(
            SavedAudioEndpoint {
                id: "exact".into(),
                label: "\n\t".into()
            }
            .display_label(),
            "Selected audio device"
        );
        assert_eq!(
            SavedAudioEndpoint {
                id: "exact".into(),
                label: "🎤".repeat(300)
            }
            .display_label()
            .chars()
            .count(),
            256
        );
    }
    #[test]
    fn old_preferences_keep_familiar_shortcuts() {
        let p: Preferences = serde_json::from_str(r#"{"height":1080}"#).unwrap();
        assert_eq!(p.hotkeys, default_hotkeys());
        assert!(p.display_names.is_empty());
        assert!(p.imported_files.is_empty());
        assert!(!p.gpu_video_preview);
        let mut custom = p;
        custom.hotkeys[0] = "Ctrl+Alt+F8".into();
        custom.hotkeys[5] = String::new();
        custom.gpu_video_preview = true;
        let restored: Preferences =
            serde_json::from_slice(&serde_json::to_vec(&custom).unwrap()).unwrap();
        assert_eq!(restored.hotkeys, custom.hotkeys);
        assert!(restored.gpu_video_preview);
    }

    #[test]
    fn imported_references_persist_with_names_and_favorites() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("external.MP4");
        std::fs::write(&media, b"untouched source").unwrap();
        let source = media.canonicalize().unwrap();
        let mut p = Preferences::default();
        p.imported_files.insert(source.clone());
        p.favorites.insert(source.clone());
        p.set_display_name(&source, "Imported highlight").unwrap();
        p.save_to(&dir.path().join("preferences.json")).unwrap();
        let restored: Preferences =
            serde_json::from_slice(&std::fs::read(dir.path().join("preferences.json")).unwrap())
                .unwrap();
        assert!(restored.imported_files.contains(&source));
        assert!(restored.favorites.contains(&source));
        assert_eq!(
            restored.custom_display_name(&source),
            Some("Imported highlight")
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"untouched source");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        std::fs::remove_file(&source).unwrap();
        let offline: Preferences =
            serde_json::from_slice(&std::fs::read(dir.path().join("preferences.json")).unwrap())
                .unwrap();
        assert!(offline.imported_files.contains(&source));
        assert!(offline.favorites.contains(&source));
        assert_eq!(
            offline.custom_display_name(&source),
            Some("Imported highlight")
        );
    }

    #[test]
    fn display_names_validate_unicode_and_loaded_preferences() {
        assert_eq!(
            validate_display_name("  Café 🎮  "),
            Ok("Café 🎮".to_owned())
        );
        assert!(validate_display_name(" \t ").is_err());
        assert!(validate_display_name(&"🎮".repeat(128)).is_ok());
        assert!(validate_display_name(&"🎮".repeat(129)).is_err());
        assert!(validate_display_name("hidden\nline").is_err());
        let loaded: Preferences = serde_json::from_value(serde_json::json!({
            "display_names": {"capture.mkv": "  Winning round  ", "empty.png": "  ", "bad.png": "bad\u{2028}line"}
        })).unwrap();
        assert_eq!(loaded.display_names.len(), 1);
        assert_eq!(
            loaded.display_name(Path::new("capture.mkv")),
            "Winning round"
        );
        assert_eq!(loaded.display_name(Path::new("other.png")), "other.png");
    }

    #[test]
    fn display_names_persist_without_changing_originals_or_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("capture.mkv");
        let sidecar = dir.path().join("capture.rcr.json");
        std::fs::write(&source, b"original media").unwrap();
        std::fs::write(&sidecar, b"original project").unwrap();
        let settings = dir.path().join("preferences.json");
        let mut p = Preferences::default();
        p.set_display_name(&source, "  Match highlight  ").unwrap();
        assert!(p.set_display_name(&source, "  ").is_err());
        p.save_to(&settings).unwrap();
        let loaded: Preferences =
            serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(loaded.display_name(&source), "Match highlight");
        assert_eq!(
            loaded.display_names.keys().next(),
            Some(&capture_key(&source))
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"original media");
        assert_eq!(std::fs::read(&sidecar).unwrap(), b"original project");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn names_remain_available_without_the_original_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("capture.png");
        std::fs::write(&source, b"original").unwrap();
        let mut p = Preferences::default();
        p.set_display_name(&source, "Keep this name").unwrap();
        std::fs::rename(&source, dir.path().join("moved.png")).unwrap();
        assert_eq!(p.custom_display_name(&source), Some("Keep this name"));
        let restored: Preferences =
            serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
        assert_eq!(
            restored.custom_display_name(&source),
            Some("Keep this name")
        );
    }

    #[cfg(windows)]
    #[test]
    fn canonical_windows_prefixes_match_ordinary_paths_without_disk_access() {
        assert_eq!(
            lookup_key(Path::new(r"\\?\C:\Clips\capture.png")),
            lookup_key(Path::new(r"c:\Clips\capture.png"))
        );
        assert_eq!(
            lookup_key(Path::new(r"\\?\UNC\server\share\capture.png")),
            lookup_key(Path::new(r"\\server\share\capture.png"))
        );
        let p: Preferences = serde_json::from_value(serde_json::json!({
            "display_names": {r"\\?\C:\missing-captures\clip.png": "Offline capture"}
        }))
        .unwrap();
        assert_eq!(
            p.custom_display_name(Path::new(r"C:\missing-captures\clip.png")),
            Some("Offline capture")
        );
    }
    #[test]
    fn preferences_replace_atomically_and_preserve_favorites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut p = Preferences::default();
        p.save_to(&path).unwrap();
        p.replay_seconds = 300;
        p.favorites.insert(PathBuf::from("test.mkv"));
        p.save_to(&path).unwrap();
        let loaded: Preferences = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded.replay_seconds, 300);
        assert!(loaded.favorites.contains(Path::new("test.mkv")));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_partial_write_keeps_previous_json_and_removes_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        let previous = Preferences { microphone: true, ..Default::default() };
        previous.save_to(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let result = atomic_write(&path, |temp| {
            temp.write_all(b"{\"hotkeys\": [\"partial")?;
            anyhow::bail!("Synthetic serialization/write failure")
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(serde_json::from_slice::<Preferences>(&bytes).unwrap().microphone);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn oversized_encoded_preferences_keep_previous_file_and_allow_smaller_retry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let previous = Preferences { microphone: true, ..Default::default() };
        previous.save_to(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut candidate = previous.clone();
        // Each control character needs six JSON bytes. Limit the serialized
        // stream, not just source string length or the number of saved entries.
        candidate.hotkeys[0] = "\u{0001}".repeat((MAX_PREFERENCES_BYTES / 6 + 1) as usize);
        let error = candidate.save_to(&path).unwrap_err();
        assert!(error.to_string().contains("8 MiB save limit"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1, "Failed save leaked a temporary file");
        let loaded = recovery::load(&path);
        assert!(loaded.notice.is_none() && loaded.preferences.microphone);
        let smaller = Preferences { fps: 30, ..previous };
        smaller.save_to(&path).unwrap();
        assert_eq!(recovery::load(&path).preferences.fps, 30);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
