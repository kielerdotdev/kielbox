#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod diagnostics_io;
#[cfg(windows)]
mod hotkeys;
#[cfg(windows)]
mod installation;
#[cfg(windows)]
mod installer_smoke;
mod library_scan;
mod preferences;
mod preferences_io;
#[cfg(windows)]
mod quick_recording;
mod recordings;
mod rune_annotation;
#[cfg(windows)]
mod rune_capture;
mod rune_editor;
mod rune_fixtures;
#[cfg(windows)]
mod rune_gpu;
#[cfg(windows)]
static PENDING_UPDATE: std::sync::Mutex<Option<(updates::PreparedUpdate, std::path::PathBuf)>> =
    std::sync::Mutex::new(None);
mod rune_palette;
mod rune_shot;
mod rune_theme;
mod rune_thumbs;
mod rune_tools;
mod rune_ui;
#[cfg(windows)]
mod setup_diagnostics;
#[cfg(windows)]
mod single_instance;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod update_handoff;
#[cfg(windows)]
mod updates;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    if args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--smoke-installed-status" | "--smoke-installed-check" | "--smoke-installed-update"
        )
    }) {
        if let Err(e) = installer_smoke::run(&args) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| {
        a.starts_with("--smoke-")
            || a.starts_with("--eval-")
            || matches!(
                a.as_str(),
                "--ui-snapshot" | "--proof-seconds" | "--exit-after-proof"
            )
    }) {
        eprintln!(
            "This retired desktop proof is unsupported. Use scripts/verify-rune.ps1 for Rune validation."
        );
        std::process::exit(2);
    }
    if args.iter().any(|a| a == "--shot-fixture") {
        rune_shot::run_fixture();
        return;
    }
    let headless = rune::tooling::is_headless();
    #[cfg(windows)]
    let _instance = if headless || args.iter().any(|a| a == "--diagnose") {
        None
    } else {
        match single_instance::Guard::acquire() {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => {
                single_instance::activate_existing();
                return;
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    };
    #[cfg(windows)]
    if !headless {
        let startup = (|| -> Result<(), String> {
            let version = updates::installed_version()?;
            if let Some(notice) = update_handoff::check_startup(version.as_deref())? {
                eprintln!("{notice}");
            }
            installation::initialize(version.is_some(), &args)?;
            Ok(())
        })();
        if let Err(error) = startup {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
    if args.iter().any(|a| a == "--diagnose") {
        #[cfg(windows)]
        {
            let report = setup_diagnostics::collect(
                setup_diagnostics::Config::capture(recordings::root()),
                &toolkit::core::workflow::CancelToken::new(),
            );
            if args.iter().any(|a| a == "--human") {
                println!("{}", report.human_summary());
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("Serialize diagnostics")
                );
            }
            if report.has_blocking_issues() {
                std::process::exit(1);
            }
        }
        return;
    }
    rune::tooling::set_app_id("dev.kielbox.Kielbox");
    let spec = rune::AppSpec {
        setup: Some(rune_ui::setup),
        poll: Some(rune_ui::poll),
        on_key: Some(rune_ui::on_key),
        scenes: &[
            ("cpu-fixture", |s| {
                s.go(rune_ui::Page::Capture);
                #[cfg(windows)]
                if s.headless {
                    s.capture.cpu_fixture();
                }
            }),
            ("gpu-fixture", |s| {
                s.go(rune_ui::Page::Capture);
                #[cfg(windows)]
                if s.headless {
                    s.capture.gpu_fixture();
                }
            }),
            ("library", |s| s.go(rune_ui::Page::Library)),
            ("library-fixture", rune_fixtures::library),
            ("editor-fixture", rune_fixtures::editor),
            ("capture", |s| s.go(rune_ui::Page::Capture)),
            ("editor", |s| s.page = rune_ui::Page::Editor),
            ("disk", |s| s.go_disk()),
            ("tools", |s| s.go(rune_ui::Page::Tools)),
            ("settings", |s| s.go(rune_ui::Page::Settings)),
        ],
        min_size: Some((760.0, 540.0)),
        ..rune::AppSpec::new(
            "Kielbox",
            (1200.0, 800.0),
            rune_ui::State::new,
            rune_ui::view,
        )
    };
    let app = rune::App::new(spec)
        .titlebar(rune::Titlebar::new(rune_ui::TITLEBAR))
        .keep_running()
        .on_close(|s, tx| {
            if !s.preferences.close_to_tray || !s.has_tray() {
                s.request_quit(tx);
            }
        });
    let mut app = if headless {
        app.compositor(DesktopCompositor {
            logged: false,
            surface: None,
        })
    } else {
        app
    };
    let handle = app.handle();
    #[cfg(windows)]
    let shot = (!headless).then(|| rune_shot::install(&mut app));
    app.init(move || {
        let mut s = rune_ui::State::new();
        s.handle = Some(handle);
        #[cfg(windows)]
        {
            s.shot = shot;
        }
        s
    })
    .run();
    if let Err(error) = preferences::finish_pending() {
        eprintln!("Settings shutdown: {error}");
        return;
    }
    #[cfg(windows)]
    if let Some((update, root)) = PENDING_UPDATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        if let Err(error) = update.apply_after_shutdown(true, &root) {
            eprintln!("Could not apply update: {error}");
        }
    }
}

struct DesktopCompositor {
    logged: bool,
    surface: Option<rune::NativeSurface>,
}
impl rune::Compositor<rune_ui::State> for DesktopCompositor {
    fn render(&mut self, f: &mut rune::Frame<'_, rune_ui::State>) {
        if f.rt.state.headless && !self.logged {
            self.surface = f.rt.surfaces.first().map(|d| d.surface.clone());
            self.logged = true;
        }
        f.draw_ui();
    }
}
impl Drop for DesktopCompositor {
    fn drop(&mut self) {
        if let Some(surface) = &self.surface {
            eprintln!(
                "Kielbox native surface proof: {:?}; error={:?}",
                surface.counts(),
                surface.last_error()
            );
        }
    }
}
