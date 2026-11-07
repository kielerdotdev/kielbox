//! Ctrl K: one search across pages, capture actions, tools and clips.
use crate::{
    recordings,
    rune_theme::{self as theme, c, icons},
    rune_ui::{Page, State},
};
use rune::{El, Icon, Key, KeyEvent, TextEdit, Tx, col, el, row, text};
use std::path::PathBuf;
use toolkit::utilities::Tool;

pub struct Palette {
    pub query: TextEdit,
    pub selected: usize,
}

#[derive(Clone)]
enum Target {
    Page(Page),
    SaveReplay,
    ToggleReplay,
    ToggleRecord,
    Screenshot(toolkit::core::workflow::CaptureTarget),
    OpenFile,
    Tool(Tool),
    Clip(PathBuf),
}

struct Item {
    group: &'static str,
    icon: &'static Icon,
    title: String,
    hint: String,
    target: Target,
}

pub fn toggle(s: &mut State) {
    s.palette = match s.palette.take() {
        Some(_) => None,
        None => {
            let mut query = TextEdit::new("");
            query.focused = true;
            Some(Palette { query, selected: 0 })
        }
    };
}

fn items(s: &State) -> Vec<Item> {
    use toolkit::core::workflow::CaptureTarget;
    let query = s
        .palette
        .as_ref()
        .map(|p| p.query.text.trim().to_lowercase())
        .unwrap_or_default();
    let keys = &s.preferences.hotkeys;
    let mut all = vec![
        Item {
            group: "Actions",
            icon: &icons::DOWNLOAD,
            title: "Save replay".into(),
            hint: crate::rune_ui::keys(&keys[1]),
            target: Target::SaveReplay,
        },
        Item {
            group: "Actions",
            icon: &icons::POWER,
            title: if capture_active(s) {
                "Stop capture".into()
            } else {
                "Turn on instant replay".into()
            },
            hint: crate::rune_ui::keys(&keys[0]),
            target: Target::ToggleReplay,
        },
        Item {
            group: "Actions",
            icon: &icons::CAPTURE,
            title: "Start or stop recording".into(),
            hint: crate::rune_ui::keys(&keys[2]),
            target: Target::ToggleRecord,
        },
        Item {
            group: "Actions",
            icon: &icons::REGION,
            title: "Screenshot region".into(),
            hint: crate::rune_ui::keys(&keys[3]),
            target: Target::Screenshot(CaptureTarget::Region),
        },
        Item {
            group: "Actions",
            icon: &icons::DISPLAY,
            title: "Screenshot screen".into(),
            hint: crate::rune_ui::keys(&keys[4]),
            target: Target::Screenshot(CaptureTarget::Fullscreen),
        },
        Item {
            group: "Actions",
            icon: &icons::WINDOW,
            title: "Screenshot window".into(),
            hint: crate::rune_ui::keys(&keys[5]),
            target: Target::Screenshot(CaptureTarget::Window),
        },
        Item {
            group: "Actions",
            icon: &icons::FILE,
            title: "Open file…".into(),
            hint: "Ctrl O".into(),
            target: Target::OpenFile,
        },
    ];
    for (page, i, title) in [
        (Page::Library, &icons::LIBRARY, "Library"),
        (Page::Capture, &icons::CAPTURE, "Capture"),
        (Page::Tools, &icons::TOOLS, "Tools"),
        (Page::Disk, &icons::DISK, "Disk usage"),
        (Page::Settings, &icons::SETTINGS, "Settings"),
    ] {
        all.push(Item {
            group: "Go to",
            icon: i,
            title: title.into(),
            hint: String::new(),
            target: Target::Page(page),
        });
    }
    for tool in Tool::ALL {
        all.push(Item {
            group: "Tools",
            icon: crate::rune_tools::tool_icon(tool),
            title: crate::rune_tools::tool_title(tool).into(),
            hint: String::new(),
            target: Target::Tool(tool),
        });
    }
    let mut clips = 0;
    for path in &s.files {
        if clips == 8 {
            break;
        }
        let title = crate::rune_ui::clip_title(s, path);
        if !query.is_empty() && !title.to_lowercase().contains(&query) {
            continue;
        }
        clips += 1;
        all.push(Item {
            group: "Clips",
            icon: if recordings::is_image(path) {
                &icons::IMAGE
            } else {
                &icons::FILM
            },
            title,
            hint: String::new(),
            target: Target::Clip(path.clone()),
        });
    }
    if query.is_empty() {
        all.retain(|i| i.group != "Tools");
        return all;
    }
    all.into_iter()
        .filter(|i| i.title.to_lowercase().contains(&query) || i.group == "Clips")
        .collect()
}

#[cfg(windows)]
fn capture_active(s: &State) -> bool {
    s.capture.active
}
#[cfg(not(windows))]
fn capture_active(_: &State) -> bool {
    false
}

fn run(s: &mut State, tx: &mut Tx, target: Target) {
    s.palette = None;
    match target {
        Target::Page(page) => s.go(page),
        Target::OpenFile => s.browse(tx),
        Target::Tool(tool) => crate::rune_tools::open_tool(s, tool),
        Target::Clip(path) => s.open(path),
        #[cfg(windows)]
        Target::SaveReplay => s.capture.save_replay(&s.preferences, &mut s.notice),
        #[cfg(windows)]
        Target::ToggleReplay => {
            s.capture
                .toggle(false, &s.preferences, s.wake.clone(), &mut s.notice)
        }
        #[cfg(windows)]
        Target::ToggleRecord => {
            s.capture
                .toggle(true, &s.preferences, s.wake.clone(), &mut s.notice)
        }
        #[cfg(windows)]
        Target::Screenshot(target) => crate::rune_tools::screenshot(s, target),
        #[cfg(not(windows))]
        Target::SaveReplay
        | Target::ToggleReplay
        | Target::ToggleRecord
        | Target::Screenshot(_) => s.notice = "Capture is available on Windows.".into(),
    }
}

fn activate(s: &mut State, tx: &mut Tx) {
    let list = items(s);
    let selected = s.palette.as_ref().map_or(0, |p| p.selected);
    if let Some(item) = list.into_iter().nth(selected) {
        run(s, tx, item.target);
    }
}

fn step(s: &mut State, by: isize) {
    let count = items(s).len();
    if let Some(p) = &mut s.palette
        && count > 0
    {
        p.selected = (p.selected as isize + by).rem_euclid(count as isize) as usize;
    }
}

/// Keys while the palette is open but its field isn't focused.
pub fn key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
    match event.key {
        Key::Escape => s.palette = None,
        Key::Up => step(s, -1),
        Key::Down => step(s, 1),
        Key::Enter => activate(s, tx),
        _ => return false,
    }
    true
}

pub fn view(s: &State) -> Option<El> {
    let p = s.palette.as_ref()?;
    let list = items(s);
    let selected = p.selected.min(list.len().saturating_sub(1));
    let query = theme::input(
        "palette-query",
        "Search clips, tools, actions and settings",
        &p.query,
        |s| &mut s.palette.as_mut().expect("palette open").query,
    )
    .on_input_change(|s: &mut State, _| {
        if let Some(p) = &mut s.palette {
            p.selected = 0;
        }
    })
    .on_input_key(|s: &mut State, event, _| match event.key {
        Key::Up => {
            step(s, -1);
            true
        }
        Key::Down => {
            step(s, 1);
            true
        }
        Key::Escape => {
            s.palette = None;
            true
        }
        _ => false,
    })
    .on_submit(|s: &mut State, tx| activate(s, tx));
    let search = theme::field_shell(
        Some(theme::glyph(&icons::SEARCH, 16.0, c(theme::T3))),
        query,
    )
    .h(44.0);
    let mut results = col().w_full().gap(1.0).padding(6.0);
    let mut group = "";
    for (i, item) in list.into_iter().enumerate() {
        if item.group != group {
            group = item.group;
            results = results.child(
                theme::section_label(group)
                    .px(10.0)
                    .pt(if i == 0 { 4.0 } else { 10.0 })
                    .pb(4.0),
            );
        }
        let on = i == selected;
        let target = item.target.clone();
        let mut line = row()
            .key(format!("palette-{i}"))
            .role("option")
            .label(item.title.clone())
            .checked(on)
            .focusable(true)
            .h(36.0)
            .px(10.0)
            .gap(10.0)
            .center_y()
            .radius(6.0)
            .hover(theme::white(0.05))
            .on_press_tx(move |s: &mut State, tx| run(s, tx, target.clone()))
            .child(theme::glyph(
                item.icon,
                16.0,
                c(if on { theme::T1 } else { theme::T3 }),
            ))
            .child(
                text(item.title)
                    .font(theme::BODY)
                    .color(c(theme::T1))
                    .single_line()
                    .truncate()
                    .grow(1.0)
                    .min_w(0.0),
            );
        if !item.hint.is_empty() {
            line = line.child(theme::kbd(item.hint));
        }
        if on {
            line = line.fill(theme::white(0.08));
        }
        results = results.child(line);
    }
    if selected == 0 && results_empty(s) {
        results = results.child(theme::small("Nothing matches.").px(10.0).py(8.0));
    }
    let panel = theme::floating(
        col()
            .key("palette")
            .role("dialog")
            .label("Search")
            .w(600.0)
            .max_h(520.0)
            .modal(true, |s: &mut State| s.palette = None),
    )
    .child(row().padding(8.0).child(search.grow(1.0)))
    .child(theme::hr())
    .child(
        results
            .scroll(rune::ScrollSpec {
                stick_bottom: false,
            })
            .min_h(0.0)
            .grow(1.0),
    );
    Some(
        el().key("palette-scrim")
            .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .fill(theme::black(0.45))
            .on_press(|s: &mut State| s.palette = None)
            .child(
                row()
                    .w_full()
                    .pt(72.0)
                    .justify_center()
                    .child(panel.on_press(|_: &mut State| {})),
            ),
    )
}

fn results_empty(s: &State) -> bool {
    items(s).is_empty()
}
