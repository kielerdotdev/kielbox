//! Desktop composition and lifecycle for independently selected features.
use super::settings::settings_header;
use super::*;

impl State {
    pub fn allows(&self, feature: Feature) -> bool {
        self.preferences.features.allows(feature)
    }
    pub fn require(&mut self, feature: Feature) -> bool {
        if self.allows(feature) {
            return true;
        }
        self.notify(
            NoticeKind::Error,
            format!("{} is disabled. See Settings > Features.", feature.label()),
        );
        false
    }
    pub fn page_available(&self, page: Page) -> bool {
        match page {
            Page::Capture => {
                self.allows(Feature::Recording)
                    || self.allows(Feature::Screencasts)
                    || self.allows(Feature::Screenshots)
            }
            Page::Editor => {
                self.allows(Feature::ImageEditing) || self.allows(Feature::VideoEditing)
            }
            Page::Disk => self.allows(Feature::StorageAnalysis),
            Page::Tools => self.allows(Feature::Utilities) || self.allows(Feature::StorageAnalysis),
            _ => true,
        }
    }
    fn feature_busy(&self, feature: Feature) -> bool {
        if self.jobs.iter().any(|job| job.feature == Some(feature)) {
            return true;
        }
        #[cfg(windows)]
        if self.capture.feature_change_blocked(feature) {
            return true;
        }
        match feature {
            Feature::ImageEditing | Feature::VideoEditing => {
                self.editor.owns_feature(feature) && self.editor.feature_change_blocked()
            }
            // The overlay is shared by screenshots and screen-reading utilities.
            Feature::Screenshots => crate::rune_shot::shutdown_blocked(),
            Feature::Utilities => self.tools.busy() || crate::rune_shot::shutdown_blocked(),
            _ => false,
        }
    }
    pub fn set_feature(&mut self, feature: Feature, enabled: bool) {
        if crate::features::policy().blocks(feature) {
            self.notify(
                NoticeKind::Error,
                "This feature is blocked by the process kill switch.",
            );
            return;
        }
        if self.preferences.features.selected(feature) == enabled {
            return;
        }
        if !enabled && self.feature_busy(feature) {
            self.notify(
                NoticeKind::Error,
                format!(
                    "Finish or cancel active {} work before disabling it.",
                    feature.label().to_lowercase()
                ),
            );
            return;
        }
        self.preferences.features.set(feature, enabled);
        if !self.save_preferences() {
            return;
        }
        let mut editor_closed = false;
        if !enabled {
            match feature {
                Feature::ImageEditing | Feature::VideoEditing => {
                    let affected = self.editor.path.as_ref().is_some_and(|p| {
                        recordings::is_image(p) == (feature == Feature::ImageEditing)
                    });
                    if affected {
                        editor_closed = self.editor.close();
                    }
                }
                Feature::Utilities => self.tools.close(),
                Feature::StorageAnalysis => self.disk.close(),
                _ => {}
            }
        }
        if !self.page_available(self.page) || (self.page == Page::Editor && editor_closed) {
            self.go(Page::Library);
        }
        self.palette = None;
        #[cfg(windows)]
        if !self.headless
            && matches!(
                feature,
                Feature::Recording
                    | Feature::Screenshots
                    | Feature::Screencasts
                    | Feature::Utilities
            )
        {
            if feature != Feature::Utilities {
                self.hotkeys.take();
                match crate::hotkeys::Hotkeys::start_with(
                    &self
                        .preferences
                        .features
                        .bindings(&self.preferences.hotkeys),
                ) {
                    Ok(keys) => self.hotkeys = Some(keys),
                    Err(error) => self.notify(
                        NoticeKind::Error,
                        format!("Could not update shortcuts: {error}"),
                    ),
                }
            }
            self.tray.take();
            match crate::tray::Tray::new(&self.preferences.features) {
                Ok(tray) => self.tray = Some(tray),
                Err(error) => {
                    self.notify(NoticeKind::Error, format!("Could not update tray: {error}"))
                }
            }
        }
    }
}

pub(super) fn settings(s: &State) -> El {
    let mut section = col().w_full()
        .child(settings_header("Features", None))
        .child(theme::small("Choose the tools you use. Changes apply immediately. Your files and saved tool settings are kept.").pb(16.0));
    if let Some(error) = &crate::features::policy().error {
        section = section.child(theme::small(error.clone()).pb(16.0));
    }
    for feature in Feature::ALL {
        let blocked = crate::features::policy().blocks(feature);
        let control = if blocked {
            theme::small("Blocked by kill switch")
        } else {
            theme::switch(
                &format!("feature-{}", feature.id()),
                feature.label(),
                s.allows(feature),
                move |s, _| s.set_feature(feature, !s.preferences.features.selected(feature)),
            )
        };
        section = section.child(theme::setting(
            feature.label(),
            Some(feature.description()),
            control,
            false,
        ));
    }
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_owned_jobs_block_disabling_a_feature() {
        let mut app = State::with_headless(true);
        let (_sender, receiver) = mpsc::sync_channel(1);
        app.jobs.push(FileJob {
            feature: Some(Feature::Screenshots),
            result: receiver,
            worker: None,
        });
        app.set_feature(Feature::StorageAnalysis, false);
        assert!(!app.allows(Feature::StorageAnalysis));
        app.set_feature(Feature::Screenshots, false);
        assert!(app.allows(Feature::Screenshots));
        app.jobs.clear();
        app.set_feature(Feature::Screenshots, false);
        assert!(!app.allows(Feature::Screenshots));
    }

    #[test]
    fn disabled_features_cannot_dispatch_background_jobs() {
        let mut app = State::with_headless(true);
        app.set_feature(Feature::Utilities, false);
        app.feature_job(Feature::Utilities, || panic!("disabled job ran"));
        assert!(app.jobs.is_empty());
        assert!(app.notice.contains("disabled"));
    }
}
