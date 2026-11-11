//! Desktop settings composition; capability selection lives in feature_controls.
use super::*;

const SETTINGS_TABS: [&str; 6] = [
    "Capture",
    "Audio",
    "Shortcuts",
    "Storage",
    "App",
    "Features",
];
const SHORTCUTS: [&str; 7] = [
    "Turn replay on or off",
    "Save replay",
    "Start or stop recording",
    "Screenshot region",
    "Screenshot screen",
    "Screenshot window",
    "Quick screencast",
];

pub(super) fn view(s: &State) -> El {
    let mut nav = col()
        .key("settings-nav")
        .role("tablist")
        .label("Settings sections")
        .w(200.0)
        .gap(2.0)
        .child(theme::h1("Settings").px(10.0).pb(14.0));
    for (i, title) in SETTINGS_TABS.iter().enumerate() {
        let on = s.settings_tab == i;
        nav = nav.child(
            row()
                .key(format!("settings-tab-{i}"))
                .role("tab")
                .label(title.to_string())
                .checked(on)
                .focusable(true)
                .h(32.0)
                .px(10.0)
                .center_y()
                .radius(6.0)
                .fill(if on {
                    theme::c(theme::BG3)
                } else {
                    Color::TRANSPARENT
                })
                .hover(theme::white(0.05))
                .on_press(move |s: &mut State| s.settings_tab = i)
                .child(
                    text(title.to_string())
                        .font(rune::TextStyle::new(13.5, 440.0).lh(18.0))
                        .color(theme::c(theme::T1)),
                ),
        );
    }
    let section = match s.settings_tab {
        1 => settings_audio(s),
        2 => settings_shortcuts(s),
        3 => settings_storage(s),
        4 => settings_app(s),
        5 => feature_controls::settings(s),
        _ => settings_capture(s),
    };
    row()
        .grow(1.0)
        .min_h(0.0)
        .pt(22.0)
        .px(32.0)
        .gap(48.0)
        .items_start()
        .child(nav)
        .child(scroll(
            col()
                .grow(1.0)
                .min_w(0.0)
                .max_w(760.0)
                .pt(4.0)
                .pb(40.0)
                .gap(36.0)
                .child(section),
        ))
}

pub(super) fn settings_header(title: &str, accessory: Option<El>) -> El {
    let mut line = row()
        .w_full()
        .pb(6.0)
        .gap(12.0)
        .center_y()
        .child(theme::h2(title).grow(1.0));
    if let Some(a) = accessory {
        line = line.child(a);
    }
    line
}

fn settings_capture(s: &State) -> El {
    let p = &s.preferences;
    let (status, _) = capture_status(s);
    let note = (status != Status::Off).then(|| {
        row()
            .gap(7.0)
            .center_y()
            .child(theme::dot(7.0, theme::c(theme::ACCENT)))
            .child(theme::small(
                "Capture running: changes apply the next time it starts",
            ))
    });
    let seg = |key: &str, name: &str, options: Vec<(String, bool)>, f: fn(&mut State, usize)| {
        theme::segmented(key, name, options, move |s, i| {
            f(s, i);
            s.save_preferences();
        })
    };
    let heights = [720, 1080, 1440, 2160];
    let rates = [30, 60, 120];
    let bitrates = [15_000_000, 25_000_000, 50_000_000];
    let lengths = [30, 60, 120, 300];
    // Average AV1 output size per hour at the chosen bitrate.
    let per_hour = p.bitrate as f64 * 3_600.0 / 8.0 / 1e9;
    col()
        .w_full()
        .child(settings_header("Capture", note))
        .child(theme::setting(
            "Resolution",
            None,
            seg(
                "height",
                "Resolution",
                heights
                    .iter()
                    .map(|h| (format!("{h}p"), p.height == *h))
                    .collect(),
                |s, i| s.preferences.height = [720, 1080, 1440, 2160][i],
            ),
            false,
        ))
        .child(theme::setting(
            "Frame rate",
            None,
            seg(
                "fps",
                "Frame rate",
                rates.iter().map(|r| (r.to_string(), p.fps == *r)).collect(),
                |s, i| s.preferences.fps = [30, 60, 120][i],
            ),
            false,
        ))
        .child(theme::setting(
            "Quality",
            Some(&format!("About {per_hour:.1} GB per hour")),
            seg(
                "bitrate",
                "Quality",
                bitrates
                    .iter()
                    .map(|b| (format!("{} Mb/s", b / 1_000_000), p.bitrate == *b))
                    .collect(),
                |s, i| s.preferences.bitrate = [15_000_000, 25_000_000, 50_000_000][i],
            ),
            false,
        ))
        .child(theme::setting(
            "Optimise for",
            None,
            seg(
                "priority",
                "Optimise for",
                [
                    crate::preferences::CapturePriority::ImageQuality,
                    crate::preferences::CapturePriority::EditingSpeed,
                ]
                .iter()
                .map(|c| (c.label().to_string(), p.capture_priority == *c))
                .collect(),
                |s, i| {
                    s.preferences.capture_priority = if i == 0 {
                        crate::preferences::CapturePriority::ImageQuality
                    } else {
                        crate::preferences::CapturePriority::EditingSpeed
                    }
                },
            ),
            false,
        ))
        .child(theme::setting(
            "Replay length",
            Some("How much the instant replay keeps"),
            seg(
                "replay-length",
                "Replay length",
                lengths
                    .iter()
                    .map(|l| {
                        (
                            if *l < 60 {
                                format!("{l} s")
                            } else {
                                format!("{} min", l / 60)
                            },
                            p.replay_seconds == *l,
                        )
                    })
                    .collect(),
                |s, i| s.preferences.replay_seconds = [30, 60, 120, 300][i],
            ),
            true,
        ))
}

fn pref_switch(
    key: &'static str,
    title: &str,
    on: bool,
    flip: fn(&mut crate::preferences::Preferences),
) -> El {
    theme::switch(key, title, on, move |s, _| {
        if matches!(key, "system-audio" | "microphone" | "clean-mic") && recorder_busy(s) {
            s.notify(
                NoticeKind::Error,
                "Stop capture before changing audio settings.",
            );
            return;
        }
        flip(&mut s.preferences);
        s.save_preferences();
    })
}

fn recorder_busy(s: &State) -> bool {
    #[cfg(windows)]
    {
        s.capture.status().0 != Status::Off
    }
    #[cfg(not(windows))]
    {
        let _ = s;
        false
    }
}
fn settings_audio(s: &State) -> El {
    let p = &s.preferences;
    col()
        .w_full()
        .child(settings_header("Audio", None))
        .child(theme::setting(
            "Desktop audio",
            Some("Saved as its own track"),
            pref_switch("system-audio", "Desktop audio", p.system_audio, |p| {
                p.system_audio = !p.system_audio
            })
            .disabled(recorder_busy(s)),
            false,
        ))
        .child(theme::setting(
            "Microphone",
            Some("Saved as its own track"),
            pref_switch("microphone", "Microphone", p.microphone, |p| {
                p.microphone = !p.microphone
            })
            .disabled(recorder_busy(s)),
            false,
        ))
        .child(theme::setting(
            "Clean up microphone",
            Some("Removes background noise from the microphone in saved replays"),
            pref_switch(
                "clean-mic",
                "Clean up microphone",
                p.clean_microphone,
                |p| p.clean_microphone = !p.clean_microphone,
            )
            .disabled(recorder_busy(s)),
            true,
        ))
}

fn shortcut_status(s: &State, i: usize) -> Option<String> {
    if s.hotkey_fields[i].text != s.preferences.hotkeys[i] {
        return Some("Not applied".into());
    }
    #[cfg(windows)]
    {
        return Some(
            s.hotkeys
                .as_ref()
                .and_then(|h| h.statuses.get(i))
                .map(|v| v.detail.clone())
                .unwrap_or_else(|| "Global shortcuts unavailable".into()),
        );
    }
    #[cfg(not(windows))]
    {
        Some("Global shortcuts require Windows".into())
    }
}
fn settings_shortcuts(s: &State) -> El {
    let restore = Btn::new("restore-hotkeys", "Restore defaults")
        .ghost()
        .on(|s, _| {
            for (field, binding) in s
                .hotkey_fields
                .iter_mut()
                .zip(crate::preferences::default_hotkeys())
            {
                *field = TextEdit::new(binding);
            }
        })
        .build();
    let mut section = col()
        .w_full()
        .child(settings_header("Shortcuts", Some(restore)));
    for (i, edit) in s.hotkey_fields.iter().enumerate() {
        if !s.allows(Feature::shortcut(i)) {
            continue;
        }
        section = section.child(theme::setting(
            SHORTCUTS.get(i).copied().unwrap_or("Shortcut"),
            shortcut_status(s, i).as_deref(),
            theme::field_shell(
                None,
                theme::input(
                    &format!("hotkey-{i}"),
                    SHORTCUTS.get(i).copied().unwrap_or("Shortcut"),
                    edit,
                    move |s| &mut s.hotkey_fields[i],
                )
                .placeholder("Not set", theme::c(theme::T3)),
            )
            .w(180.0),
            false,
        ));
    }
    section.child(
        row().pt(16.0).child(
            Btn::new("save-hotkeys", "Apply shortcuts")
                .primary()
                .on(|s, _| {
                    let bindings: [String; 7] =
                        std::array::from_fn(|i| s.hotkey_fields[i].text.clone());
                    #[cfg(windows)]
                    {
                        if let Err(e) =
                            crate::hotkeys::validate(&s.preferences.features.bindings(&bindings))
                        {
                            s.notice = e;
                            return;
                        }
                        s.hotkeys.take();
                        match crate::hotkeys::Hotkeys::start_with(
                            &s.preferences.features.bindings(&bindings),
                        ) {
                            Ok(keys) => {
                                let failures: Vec<_> = keys
                                    .statuses
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, status)| {
                                        !status.registered && status.detail != "Disabled"
                                    })
                                    .map(|(i, status)| {
                                        format!("{}: {}", SHORTCUTS[i], status.detail)
                                    })
                                    .collect();
                                if !failures.is_empty() {
                                    drop(keys);
                                    s.hotkeys = crate::hotkeys::Hotkeys::start_with(
                                        &s.preferences.features.bindings(&s.preferences.hotkeys),
                                    )
                                    .ok();
                                    s.notify(
                                        NoticeKind::Error,
                                        format!(
                                            "Shortcuts were not applied. {}",
                                            failures.join("; ")
                                        ),
                                    );
                                    return;
                                }
                                s.hotkeys = Some(keys);
                            }
                            Err(e) => {
                                s.hotkeys = crate::hotkeys::Hotkeys::start_with(
                                    &s.preferences.features.bindings(&s.preferences.hotkeys),
                                )
                                .ok();
                                s.notify(
                                    NoticeKind::Error,
                                    format!("Shortcuts were not changed: {e}"),
                                );
                                return;
                            }
                        }
                    }
                    s.preferences.hotkeys = bindings;
                    if s.save_preferences() {
                        s.notify(NoticeKind::Success, "Shortcuts applied.");
                    } else {
                        #[cfg(windows)]
                        {
                            s.hotkeys.take();
                            s.hotkeys = crate::hotkeys::Hotkeys::start_with(
                                &s.preferences.features.bindings(&s.preferences.hotkeys),
                            )
                            .ok();
                        }
                    }
                })
                .build(),
        ),
    )
}

fn settings_storage(s: &State) -> El {
    let mut content = col()
        .w_full()
        .child(settings_header("Storage", None))
        .child(theme::setting(
            "Capture folder",
            Some(&s.preferences.folder.display().to_string()),
            Btn::new("choose-folder", "Choose…")
                .on(|_, tx| {
                    tx.open_folder(
                        rune::FileDialogOptions::default(),
                        |s: &mut State, result, _| match result {
                            Ok(Some(path)) => {
                                s.preferences.folder = path.clone();
                                if s.save_preferences() {
                                    recordings::set_root(path);
                                    s.refresh();
                                }
                            }
                            Err(e) => s.notice = e.to_string(),
                            _ => {}
                        },
                    )
                })
                .build(),
            false,
        ))
        .child(theme::setting(
            "Disk usage",
            Some("See what takes space on a drive or folder"),
            Btn::new("open-disk", "Open")
                .disabled(!s.allows(Feature::StorageAnalysis))
                .ghost()
                .on(|s, _| s.go_disk())
                .build(),
            true,
        ));
    #[cfg(windows)]
    if let Some(installation) = crate::installation::current() {
        content = content.child(theme::setting("Data workspace", Some(&installation.working_directory.display().to_string()),
            Btn::new("use-existing-data", "Use existing data…")
                .disabled(installation.configuration_path.is_none())
                .on(|_, tx| tx.open_folder(rune::FileDialogOptions::default(), |s: &mut State, result, _| {
                    match result {
                        Ok(Some(path)) => {
                            if let Some(installation) = crate::installation::current() {
                                match installation.remember_working_directory(&path) {
                                    Ok(path) => s.notify(NoticeKind::Success, format!("Data workspace selected: {}. Quit safely and reopen Kielbox to activate it. Existing files stay in place.", path.display())),
                                    Err(error) => s.notify(NoticeKind::Error, error),
                                }
                            }
                        }
                        Err(error) => s.notify(NoticeKind::Error, error.to_string()),
                        _ => {},
                    }
                })).build(), true));
    }
    content
}

fn settings_app(s: &State) -> El {
    let mut section = col()
        .w_full()
        .child(settings_header("App", None))
        .child(theme::setting(
            "Keep running in the tray when closed",
            Some("Shortcuts and instant replay keep working"),
            pref_switch(
                "tray-on-close",
                "Keep running in the tray when closed",
                s.preferences.close_to_tray,
                |p| p.close_to_tray = !p.close_to_tray,
            ),
            true,
        ));
    #[cfg(windows)]
    if let Some(updates) = &s.updates {
        let status = match updates.status() {
            crate::updates::Status::NotInstalled => {
                "Portable build: download new versions manually".to_string()
            }
            other => format!("{other:?}"),
        };
        section = section
            .child(el().h(36.0))
            .child(settings_header("Updates", None))
            .child(theme::setting(
                &format!("Kielbox {}", updates.current_version()),
                Some(&status),
                row()
                    .gap(8.0)
                    .child(
                        Btn::new("check-updates", "Check for updates")
                            .disabled(!updates.can_check())
                            .on(|s, _| {
                                if let Some(u) = &mut s.updates {
                                    u.check_now();
                                }
                            })
                            .build(),
                    )
                    .child(
                        Btn::new("download-update", "Download")
                            .disabled(!updates.can_download())
                            .on(|s, _| {
                                if let Some(u) = &mut s.updates {
                                    u.download();
                                }
                            })
                            .build(),
                    )
                    .child(
                        Btn::new("install-update", "Restart to update")
                            .primary()
                            .disabled(!updates.ready())
                            .on(|s, tx| {
                                if !s.prepare_shutdown() {
                                    s.notice =
                                        "Finish or Cancel capture, stop recording, and finish active jobs before updating.".into();
                                    return;
                                }
                                if let Some(update) =
                                    s.updates.as_ref().and_then(|u| u.prepare_ready())
                                {
                                    match std::env::current_dir() {
                                        Ok(root) => {
                                            *crate::PENDING_UPDATE
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner()) =
                                                Some((update, root));
                                            tx.quit();
                                        }
                                        Err(e) => s.notice = e.to_string(),
                                    }
                                }
                            })
                            .build(),
                    ),
                true,
            ));
    }
    section
}
