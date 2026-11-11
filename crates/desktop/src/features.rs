//! Desktop capability policy. No feature starts another feature implicitly.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Screenshots,
    ImageEditing,
    VideoEditing,
    StorageAnalysis,
    Utilities,
    Recording,
    Screencasts,
}
impl Feature {
    pub const ALL: [Self; 7] = [
        Self::Screenshots,
        Self::ImageEditing,
        Self::VideoEditing,
        Self::StorageAnalysis,
        Self::Utilities,
        Self::Recording,
        Self::Screencasts,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::Screenshots => "screenshots",
            Self::ImageEditing => "image_editing",
            Self::VideoEditing => "video_editing",
            Self::StorageAnalysis => "storage_analysis",
            Self::Utilities => "utilities",
            Self::Recording => "recording",
            Self::Screencasts => "screencasts",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Screenshots => "Screenshots",
            Self::ImageEditing => "Image editing",
            Self::VideoEditing => "Video editing",
            Self::StorageAnalysis => "Storage analysis",
            Self::Utilities => "Utilities",
            Self::Recording => "Recording and instant replay",
            Self::Screencasts => "Screencasts",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Screenshots => "Capture your screen and save screenshots.",
            Self::ImageEditing => "Open and annotate images, including imported files.",
            Self::VideoEditing => "Edit videos from Kielbox, ShadowPlay, or other recorders.",
            Self::StorageAnalysis => "Scan drives and folders to see what takes space.",
            Self::Utilities => "Use image, video, text, and screen utilities.",
            Self::Recording => {
                "Run the native recorder and replay buffer. Disable to keep using another recorder."
            }
            Self::Screencasts => "Record a screencast with the separate quick recorder.",
        }
    }
    pub fn shortcut(index: usize) -> Self {
        match index {
            0..=2 => Self::Recording,
            3..=5 => Self::Screenshots,
            6 => Self::Screencasts,
            _ => panic!("unknown shortcut index"),
        }
    }
}

/// An empty set keeps existing installations compatible. Unknown saved IDs survive
/// downgrades and upgrades, rather than making the whole preferences file unreadable.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    pub disabled: BTreeSet<String>,
}
impl Features {
    pub fn selected(&self, feature: Feature) -> bool {
        !self.disabled.contains(feature.id())
    }
    pub fn set(&mut self, feature: Feature, enabled: bool) {
        if enabled {
            self.disabled.remove(feature.id());
        } else {
            self.disabled.insert(feature.id().into());
        }
    }
    pub fn allows(&self, feature: Feature) -> bool {
        self.allows_with(feature, policy())
    }
    fn allows_with(&self, feature: Feature, policy: &Policy) -> bool {
        self.selected(feature) && !policy.blocks(feature)
    }
    pub fn bindings(&self, saved: &[String; 7]) -> [String; 7] {
        std::array::from_fn(|i| {
            if self.allows(Feature::shortcut(i)) {
                saved[i].clone()
            } else {
                String::new()
            }
        })
    }
}

/// Immutable for the process lifetime. Restart after changing the environment.
/// Invalid policy fails closed and is visible in Settings, never silently ignored.
#[derive(Default)]
pub struct Policy {
    blocked: BTreeSet<Feature>,
    pub error: Option<String>,
}
impl Policy {
    pub fn parse(value: &str) -> Self {
        let mut policy = Self::default();
        for id in value.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            if let Some(feature) = Feature::ALL.into_iter().find(|f| f.id() == id) {
                policy.blocked.insert(feature);
            } else {
                policy.error = Some(format!(
                    "Unknown feature '{id}' in KIELBOX_DISABLE_FEATURES. All optional features are blocked until this is corrected and Kielbox is restarted."
                ));
                policy.blocked.extend(Feature::ALL);
                break;
            }
        }
        policy
    }
    pub fn blocks(&self, feature: Feature) -> bool {
        self.blocked.contains(&feature)
    }
}
pub fn policy() -> &'static Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    POLICY.get_or_init(|| match std::env::var("KIELBOX_DISABLE_FEATURES") {
        Ok(value) => Policy::parse(&value),
        Err(std::env::VarError::NotPresent) => Policy::default(),
        Err(_) => Policy::parse("<invalid encoding>"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_policy_overrides_choices_without_mutating_them() {
        if let Ok(value) = std::env::var("KIELBOX_TEST_POLICY_CHILD") {
            let settings = Features::default();
            assert!(settings.selected(Feature::Recording));
            assert!(!settings.allows(Feature::Recording));
            assert_eq!(settings.allows(Feature::VideoEditing), value == "valid");
            assert_eq!(policy().error.is_some(), value != "valid");
            assert!(settings.disabled.is_empty());
            return;
        }
        for (value, kind) in [("recording,screencasts", "valid"), ("recroding", "invalid")] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "features::tests::environment_policy_overrides_choices_without_mutating_them",
                ])
                .env("KIELBOX_DISABLE_FEATURES", value)
                .env("KIELBOX_TEST_POLICY_CHILD", kind)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }

    #[test]
    fn combinations_and_policy_are_independent() {
        for mask in 0..(1 << Feature::ALL.len()) {
            let mut settings = Features::default();
            for (i, f) in Feature::ALL.into_iter().enumerate() {
                settings.set(f, mask & (1 << i) != 0);
            }
            for (i, f) in Feature::ALL.into_iter().enumerate() {
                assert_eq!(settings.selected(f), mask & (1 << i) != 0);
            }
            for killed in Feature::ALL {
                let policy = Policy::parse(killed.id());
                for f in Feature::ALL {
                    assert_eq!(
                        settings.allows_with(f, &policy),
                        settings.selected(f) && f != killed
                    );
                }
            }
            let decoded: Features =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(decoded.disabled, settings.disabled);
        }
        let unknown: Features =
            serde_json::from_str(r#"{"disabled":["future_tool","recording"]}"#).unwrap();
        assert!(
            serde_json::to_string(&unknown)
                .unwrap()
                .contains("future_tool")
        );
        assert!(!unknown.selected(Feature::Recording));
        let policy = Policy::parse("recording, screencasts,recording");
        assert!(policy.blocks(Feature::Recording));
        assert!(!policy.blocks(Feature::VideoEditing));
        assert!(Policy::parse("recroding").blocked.len() == Feature::ALL.len());
        let mut settings = Features::default();
        settings.set(Feature::Recording, false);
        settings.set(Feature::Screencasts, false);
        let bindings = settings.bindings(&std::array::from_fn(|i| i.to_string()));
        assert!(bindings[..3].iter().all(String::is_empty));
        assert!(bindings[6].is_empty());
        assert_eq!(&bindings[3..6], &["3", "4", "5"]);
    }
}
