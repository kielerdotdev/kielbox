use super::*;
use crate::theme::{button, muted, panel, toggle};

pub(super) struct PendingInstall {
    pub update: updates::PreparedUpdate,
    pub restart: bool,
    pub data_root: PathBuf,
}

pub(super) fn startup_error(message: &str) {
    use windows::{core::PCWSTR, Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK}};
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "Kielbox".encode_utf16().chain(Some(0)).collect();
    unsafe { let _ = MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONERROR); }
}

impl Studio {
    pub(super) fn seed_updates_preview(&mut self, stage: u8) {
        let release = updates::Release { version: "0.1.0-ci.101".into(), bytes: 64_000_000, notes: "Preview update".into() };
        let status = match stage {
            0 => updates::Status::NotInstalled, 2 => updates::Status::Checking,
            3 => updates::Status::Available(release), 4 => updates::Status::Downloading(release),
            5 => updates::Status::Ready(release),
            6 => updates::Status::Error("The update service is unavailable. Your current version is still ready to use. Try checking again later.".into()),
            _ => updates::Status::Idle,
        };
        self.updates = updates::Updates::presentation_fixture(status, Instant::now());
        self.welcome_visible = stage == 7;
        self.tab = if stage == 7 { 1 } else { 3 };
    }

    pub(super) fn snapshot_updates_visible(&mut self, cx: &mut Context<Self>) -> anyhow::Result<bool> {
        let Some(section) = self.updates_bounds.get() else { return Ok(false); };
        let viewport = self.settings_scroll.bounds();
        if viewport.size.height <= px(0.) { return Ok(false); }
        anyhow::ensure!(section.size.height <= viewport.size.height - px(32.), "Updates section exceeds viewport height");
        anyhow::ensure!(section.size.width <= viewport.size.width, "Updates section exceeds viewport width");
        if section.top() >= viewport.top() + px(16.) && section.bottom() <= viewport.bottom() - px(16.) { return Ok(true); }
        let mut offset = self.settings_scroll.offset();
        offset.y = (offset.y - (section.top() - viewport.top()) + px(24.))
            .clamp(-self.settings_scroll.max_offset().height, px(0.));
        self.settings_scroll.set_offset(offset);
        cx.notify();
        Ok(false)
    }

    pub(super) fn update_blocker(&self, cx: &App) -> Option<&'static str> {
        if self.active || self.worker.is_some() || !self.pending_replays.is_empty() {
            return Some("Finish recording and replay saves before restarting.");
        }
        if self.layout_save_pending { return Some("Wait for settings to finish saving."); }
        if self.trash_restoring { return Some("Wait for file recovery to finish."); }
        if self.pending_native_dialogs != 0 { return Some("Finish or cancel the open file dialog before restarting."); }
        if self.shell_operation != 0 { return Some("Wait for the file operation to finish."); }
        if self.name_dialog.is_some() || self.display_picker.is_some() || self.audio_picker.is_some()
            || self.audio_device_picker.is_some() || self.hotkey_edit.is_some() {
            return Some("Finish or cancel the open dialog before restarting.");
        }
        if let Some(reason) = self.library.update_blocker() { return Some(reason); }
        if let Some(reason) = self.editor.read(cx).update_blocker() { return Some(reason); }
        self.toolkit.read(cx).update_blocker(cx)
    }

    pub(super) fn queue_install_on_exit(&mut self, restart: bool, cx: &mut Context<Self>) {
        if self.update_blocker(cx).is_some() { return; }
        if !restart && !self.updates.settings.install_on_quit { return; }
        if let Some(update) = self.updates.prepare_ready() {
            let data_root = self.installation_next_root.clone().or_else(|| INSTALLATION.get().map(|state| state.working_directory.clone()));
            if let Some(data_root) = data_root {
                *UPDATE_ON_EXIT.lock().unwrap_or_else(|error| error.into_inner()) = Some(PendingInstall { update, restart, data_root });
            }
        }
    }

    pub(super) fn quit_with_update(&mut self, restart: bool, cx: &mut Context<Self>) {
        if restart {
            if let Some(reason) = self.update_blocker(cx) {
                self.notice = Some(reason.into());
                cx.notify();
                return;
            }
            if !self.save_preferences() { cx.notify(); return; }
        }
        self.queue_install_on_exit(restart, cx);
        if restart && UPDATE_ON_EXIT.lock().unwrap_or_else(|error| error.into_inner()).is_none() {
            if let Some(path) = &self.installation_next_root {
                *RESTART_AFTER_EXIT.lock().unwrap_or_else(|error| error.into_inner()) = Some(path.clone());
            } else {
                self.notice = Some("Download the update before restarting.".into());
                cx.notify();
                return;
            }
        }
        cx.quit();
    }

    fn change_update_setting(&mut self, setting: usize, cx: &mut Context<Self>) {
        let previous = self.updates.settings.clone();
        match setting {
            0 => self.updates.settings.automatic_checks = !previous.automatic_checks,
            1 => self.updates.settings.automatic_downloads = !previous.automatic_downloads,
            _ => self.updates.settings.install_on_quit = !previous.install_on_quit,
        }
        self.preferences.updates = self.updates.settings.clone();
        if !self.save_preferences() {
            self.updates.settings = previous.clone();
            self.preferences.updates = previous;
        }
        cx.notify();
    }

    fn choose_existing_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(reason) = self.update_blocker(cx) { self.notice = Some(reason.into()); cx.notify(); return; }
        self.pending_native_dialogs += 1;
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: false, directories: true, multiple: false,
            prompt: Some("Choose your existing Kielbox or RCR data folder".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = selected.await;
            let _ = view.update(cx, |this, cx| {
                this.pending_native_dialogs = this.pending_native_dialogs.saturating_sub(1);
                cx.notify();
                if let Ok(Ok(Some(paths))) = result && let Some(path) = paths.into_iter().next() {
                    if let Some(reason) = this.update_blocker(cx) { this.notice = Some(reason.into()); cx.notify(); return; }
                    let selected = INSTALLATION.get().ok_or_else(|| "This build does not own an installed data folder.".to_owned())
                        .and_then(|state| state.remember_working_directory(&path));
                    match selected {
                        Ok(path) => {
                            this.installation_next_root = Some(path);
                            this.installation_restart_needed = true;
                            this.notice = Some("Data folder selected. Restart Kielbox to open it; existing files have not been moved.".into());
                        }
                        Err(error) => this.notice = Some(error),
                    }
                    cx.notify();
                }
            });
        }).detach();
    }

    pub(super) fn welcome_ui(&self, cx: &mut Context<Self>) -> Div {
        let mut welcome = panel().p_5().flex().flex_col().gap_3()
            .child(div().text_size(px(22.)).font_weight(FontWeight::MEDIUM).child("Welcome to Kielbox"))
            .child(muted("Your screenshots, clips, and disk tools are ready. Recording is off until you start it."))
            .child(muted("Used RCR before? Choose the folder containing your existing projects and recordings. Nothing will be moved."));
        if let Some(path) = &self.installation_next_root {
            welcome = welcome.child(muted(format!("Selected data folder: {}", path.display())));
        }
        let next = if self.installation_restart_needed {
            button("welcome-restart", "Restart to use selected folder").bg(rgb(ACCENT)).text_color(rgb(0x101112))
                .on_click(cx.listener(|this, _, _, cx| this.quit_with_update(true, cx)))
        } else {
            button("welcome-continue", "Open Kielbox").bg(rgb(ACCENT)).text_color(rgb(0x101112))
                .on_click(cx.listener(|this, _, _, cx| { this.welcome_visible = false; this.tab = 1; cx.notify(); }))
        };
        welcome.child(div().flex().flex_wrap().gap_2()
            .child(button("welcome-existing-data", "Use existing data").on_click(cx.listener(|this, _, window, cx| this.choose_existing_data(window, cx))))
            .child(next))
    }

    pub(super) fn updates_ui(&self, cx: &mut Context<Self>) -> Div {
        use updates::Status;
        let description = match self.updates.status() {
            Status::NotInstalled => "Use the installer to get built-in updates.".to_owned(),
            Status::Idle => "Updates are checked in the background.".to_owned(),
            Status::Checking => "Checking for updates…".to_owned(),
            Status::UpToDate => "Kielbox is up to date.".to_owned(),
            Status::NoReleases => "No update has been published for this channel yet.".to_owned(),
            Status::Available(release) => format!("Version {} is available · {:.1} MB", release.version, release.bytes as f64 / 1_000_000.),
            Status::Downloading(release) => format!("Downloading {} · {}%", release.version, self.updates.progress()),
            Status::Ready(release) => format!("Version {} is ready. Restart when you're finished working.", release.version),
            Status::Error(error) => format!("Could not update: {}{}", error.chars().take(240).collect::<String>(), if error.chars().count() > 240 { "…" } else { "" }),
        };
        let mut actions = div().flex().flex_wrap().gap_2();
        if self.updates.installed() {
            actions = actions.child(button("updates-release-notes", "Release notes")
                .on_click(|_, _, cx| cx.open_url(updates::DOWNLOAD_PAGE)));
            actions = actions.child(button("updates-check", "Check now")
                .when(!self.updates.can_check(), |button| button.opacity(0.5))
                .on_click(cx.listener(|this, _, _, cx| { this.updates.check_now(); cx.notify(); })));
            if self.updates.can_download() {
                actions = actions.child(button("updates-download", "Download update")
                    .on_click(cx.listener(|this, _, _, cx| { this.updates.download(); cx.notify(); })));
            }
            if matches!(self.updates.status(), Status::Ready(_)) {
                actions = actions.child(button("updates-restart", "Restart to update")
                    .on_click(cx.listener(|this, _, _, cx| this.quit_with_update(true, cx))));
            }
        } else {
            actions = actions.child(button("updates-get-installer", "Get Kielbox installer")
                .on_click(|_, _, cx| cx.open_url("https://github.com/kielerdotdev/kielbox/releases/download/preview/Kielbox-Setup.exe")));
        }
        let mut section = panel().p_4().flex().flex_col().gap_3()
            .child(div().font_weight(FontWeight::MEDIUM).child("Kielbox updates"))
            .child(muted(format!("Version {} · {}", self.updates.current_version(), self.updates.channel().label())))
            .child(muted(description)).child(actions);
        if let Some(checked) = self.updates.last_checked() {
            let minutes = checked.elapsed().unwrap_or_default().as_secs() / 60;
            section = section.child(muted(if minutes == 0 { "Last checked just now".to_owned() } else { format!("Last checked {minutes} minutes ago") }));
        }
        if self.updates.installed() {
            for (index, label, enabled) in [
                (0, "Check for updates automatically", self.updates.settings.automatic_checks),
                (1, "Download updates when idle", self.updates.settings.automatic_downloads),
                (2, "Install downloaded updates when I quit", self.updates.settings.install_on_quit),
            ] {
                section = section.child(div().flex().items_center().justify_between()
                    .child(muted(label)).child(toggle(("update-option", index), enabled)
                        .on_click(cx.listener(move |this, _, _, cx| this.change_update_setting(index, cx)))));
            }
        }
        if let Some(state) = INSTALLATION.get().filter(|state| state.managed) {
            section = section.child(muted(format!("Data folder: {}", self.installation_next_root.as_ref().unwrap_or(&state.working_directory).display())))
                .child(button("installation-data", "Choose existing data folder")
                    .on_click(cx.listener(|this, _, window, cx| this.choose_existing_data(window, cx))));
        }
        if self.installation_restart_needed {
            section = section.child(button("installation-restart", "Restart to use selected folder")
                .on_click(cx.listener(|this, _, _, cx| this.quit_with_update(true, cx))));
        }
        let bounds = self.updates_bounds.clone();
        section.relative().child(canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {}).absolute().size_full())
    }
}
