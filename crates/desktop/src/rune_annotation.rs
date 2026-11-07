//! Existing annotation actor, with Rune pointer/key input and a bounded raster preview.
use crate::{
    rune_theme::{self as theme, Btn, c, icons},
    rune_ui::{JobResult, State as AppState, color},
};
use rune::{El, Key, KeyEvent, NativeSurface, TextEdit, col, el, row};
use std::path::PathBuf;
use toolkit::{
    annotation as a,
    image_editor::{Actor, Command, Event, Source},
    types::Frame,
};
pub struct Annotation {
    actor: Actor,
    surface: NativeSurface,
    raster: Vec<u8>,
    size: (u32, u32),
    viewport: (u32, u32),
    active: a::Tool,
    save_path: Option<PathBuf>,
    pub message: String,
    pub text: TextEdit,
    serial: u64,
    accepted: Option<Frame>,
}
impl Annotation {
    pub fn new(source: Source, wake: rune::Waker) -> anyhow::Result<Self> {
        let actor = Actor::new(source)?;
        actor.set_waker(move || wake());
        Ok(Self {
            actor,
            surface: NativeSurface::new(),
            raster: vec![],
            size: (0, 0),
            viewport: (0, 0),
            active: a::Tool::Select,
            save_path: None,
            message: String::new(),
            text: TextEdit::new(""),
            serial: 0,
            accepted: None,
        })
    }
    fn send(&mut self, command: Command) {
        if let Err(e) = self.actor.send(command) {
            self.message = format!("{e:#}");
        }
    }
    pub fn key(&mut self, event: &KeyEvent) -> bool {
        let mods = a::Modifiers {
            shift: event.mods.shift,
            ctrl: event.mods.cmd,
            alt: event.mods.alt,
        };
        match &event.key {
            Key::Command('z') => self.send(if event.mods.shift {
                Command::Redo
            } else {
                Command::Undo
            }),
            Key::Delete | Key::Backspace => self.send(Command::Delete),
            Key::Text(text) => self.send(Command::Text(text.clone())),
            _ => return false,
        }
        let _ = mods;
        true
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(preview) = self.actor.take_preview() {
            changed = true;
            self.active = preview.active_tool;
            let size = (preview.width, preview.height);
            if preview.reset || self.size != size {
                self.size = size;
                self.raster = vec![0; size.0 as usize * size.1 as usize * 4];
            }
            for tile in preview.tiles {
                let r = tile.rect;
                for y in 0..r.height as usize {
                    let source = y * r.width as usize * 4;
                    let destination = ((r.y as usize + y) * size.0 as usize + r.x as usize) * 4;
                    let len = r.width as usize * 4;
                    if source + len <= tile.bgra.len() && destination + len <= self.raster.len() {
                        self.raster[destination..destination + len]
                            .copy_from_slice(&tile.bgra[source..source + len]);
                    }
                }
            }
            self.serial += 1;
            if size.0 > 0 && size.1 > 0 {
                if let Err(e) = self.surface.push_bytes(
                    rune::native_surface::ByteFrame {
                        width: size.0,
                        height: size.1,
                        format: rune::native_surface::Format::Bgra,
                        color: Default::default(),
                        planes: [self.raster.clone(), vec![]],
                    },
                    self.serial,
                ) {
                    self.message = e;
                }
            }
        }
        for event in self.actor.events.try_iter() {
            changed = true;
            match event {
                Event::Accepted(frame) | Event::Copied(frame) => self.accepted = Some(frame),
                Event::Saved(path) => self.message = format!("Saved {}", path.display()),
                Event::Error(e) => self.message = e,
                _ => {}
            }
        }
        changed
    }
    pub fn take_save(&mut self) -> Option<(PathBuf, Frame)> {
        let frame = self.accepted.take()?;
        Some((self.save_path.take()?, frame))
    }
}
fn tool_icon(tool: a::Tool) -> &'static rune::Icon {
    use a::Tool::*;
    match tool {
        Select | Cursor => &icons::SELECT,
        Rectangle => &icons::RECT,
        Ellipse | CropEllipse => &icons::ELLIPSE,
        Line => &icons::LINE,
        Arrow => &icons::ARROW,
        Freehand | FreehandArrow | CropFreeform => &icons::FREEHAND,
        Text | Balloon => &icons::TEXT,
        Step => &icons::STEP,
        Magnify => &icons::SEARCH,
        Spotlight => &icons::EFFECTS,
        Blur | Pixelate => &icons::BLUR,
        Highlight | HighlightPen => &icons::HIGHLIGHT,
        Image => &icons::IMAGE,
        Sticker => &icons::STAR,
        Grid => &icons::THUMBNAILS,
        Eraser => &icons::TRASH,
        Crop => &icons::CROP,
        CutOut => &icons::SPLIT,
    }
}
fn tool_name(tool: a::Tool) -> String {
    // `HighlightPen` → `Highlight pen`.
    let raw = format!("{tool:?}");
    let mut name = String::new();
    for (i, ch) in raw.chars().enumerate() {
        if i > 0 && ch.is_uppercase() {
            name.push(' ');
            name.extend(ch.to_lowercase());
        } else {
            name.push(ch);
        }
    }
    name
}
fn send(command: fn() -> Command) -> impl Fn(&mut AppState, &mut rune::Tx) + 'static {
    move |s, _| {
        if let Some(a) = &mut s.editor.annotation {
            a.send(command());
        }
    }
}
pub fn view(s: &AppState) -> El {
    let Some(a) = &s.editor.annotation else {
        return col();
    };
    let title = s
        .editor
        .path
        .as_deref()
        .map(|p| crate::rune_ui::clip_title(s, p))
        .unwrap_or_else(|| "Image".into());
    let bar = row()
        .w_full()
        .px(24.0)
        .py(12.0)
        .gap(8.0)
        .center_y()
        .child(
            Btn::new("annotation-back", "Library")
                .ghost()
                .icon(&icons::BACK)
                .on(|s, _| s.go(crate::rune_ui::Page::Library))
                .build(),
        )
        .child(
            rune::text(title)
                .font(theme::H2)
                .color(c(theme::T1))
                .role("heading")
                .level(1)
                .single_line()
                .truncate()
                .min_w(0.0),
        )
        .child(theme::spacer())
        .child(
            Btn::icon_only("annotation-undo", "Undo", &icons::UNDO)
                .on(send(|| Command::Undo))
                .build(),
        )
        .child(
            Btn::icon_only("annotation-redo", "Redo", &icons::REDO)
                .on(send(|| Command::Redo))
                .build(),
        )
        .child(
            Btn::new("annotation-rotate", "Rotate")
                .ghost()
                .icon(&icons::ROTATE)
                .on(send(|| Command::Rotate))
                .build(),
        )
        .child(
            Btn::new("annotation-crop", "Apply crop")
                .ghost()
                .icon(&icons::CROP)
                .on(send(|| Command::ApplyCrop))
                .build(),
        )
        .child(
            Btn::new("annotation-project", "Save editable…")
                .on(|_, tx| {
                    tx.save_file(
                        rune::FileDialogOptions::default().filter("Annotation project", &["ssxe"]),
                        |s: &mut AppState, result, _| {
                            if let Ok(Some(path)) = result
                                && let Some(a) = &mut s.editor.annotation
                            {
                                a.send(Command::Project(path));
                            }
                        },
                    )
                })
                .build(),
        )
        .child(
            Btn::new("annotation-save", "Save image…")
                .primary()
                .on(|_, tx| {
                    tx.save_file(
                        rune::FileDialogOptions::default().filter("PNG", &["png"]),
                        |s: &mut AppState, result, _| match result {
                            Ok(Some(path)) => {
                                if let Some(a) = &mut s.editor.annotation {
                                    a.save_path = Some(path);
                                    a.send(Command::Accept);
                                }
                            }
                            Err(e) => s.notice = e.to_string(),
                            _ => {}
                        },
                    )
                })
                .build(),
        );
    let mut tools = row()
        .key("annotation-tools")
        .role("toolbar")
        .label("Annotation tools")
        .gap(2.0)
        .padding(5.0)
        .center_y();
    for tool in a::Tool::ALL {
        let on = a.active == tool;
        let mut b = Btn::icon_only(format!("annotation-{tool:?}"), tool_name(tool), tool_icon(tool))
            .checked(on)
            .on(move |s, _| {
                if let Some(a) = &mut s.editor.annotation {
                    a.send(Command::Tool(tool));
                }
            });
        if on {
            b = b.variant(theme::Variant::Neutral);
        }
        tools = tools.child(b.build());
    }
    let label = theme::field_shell(
        Some(theme::glyph(&icons::TEXT, 14.0, c(theme::T3))),
        theme::input("annotation-label", "Label text", &a.text, |s| {
            &mut s.editor.annotation.as_mut().unwrap().text
        })
        .on_input_change(|s: &mut AppState, _| {
            if let Some(a) = &mut s.editor.annotation {
                a.send(Command::Label(a.text.text.clone()));
            }
        }),
    )
    .w(240.0);
    let dock = row()
        .w_full()
        .px(24.0)
        .gap(12.0)
        .center_y()
        .child(
            theme::floating(tools.radius(9.0))
                .scroll_x()
                .min_w(0.0)
                .grow(1.0),
        )
        .child(label);
    let canvas = el()
        .key("annotation-canvas")
        .grow(1.0)
        .min_h(100.0)
        .w_full()
        .radius(8.0)
        .clip_rounded(8.0)
        .fill(color(0x121316))
        .native_surface(&a.surface, rune::SurfaceFit::Fit)
        .on_layout(|s: &mut AppState, r| {
            if let Some(a) = &mut s.editor.annotation {
                let size = (r[2].max(1.0) as u32, r[3].max(1.0) as u32);
                if a.viewport != size {
                    a.viewport = size;
                    a.send(Command::Viewport(size.0, size.1));
                }
            }
        })
        .on_drag(|s: &mut AppState, d, tx| {
            if let Some(a) = &mut s.editor.annotation {
                let p = a::PointF::new(d.x, d.y);
                let mods = tx.mods();
                let m = a::Modifiers {
                    ctrl: mods.cmd,
                    shift: mods.shift,
                    alt: mods.alt,
                };
                let command = match d.phase {
                    rune::Phase::Down => Command::Down(p, m),
                    rune::Phase::Move => Command::Move(p, m),
                    rune::Phase::Up => Command::Up(p, m),
                };
                a.send(command);
            }
        });
    let mut page = col()
        .grow(1.0)
        .min_h(0.0)
        .child(bar)
        .child(dock)
        .child(col().grow(1.0).min_h(0.0).px(24.0).pt(14.0).pb(18.0).child(canvas));
    if !a.message.is_empty() {
        page = page.child(theme::small(a.message.clone()).px(24.0).pb(14.0));
    }
    page
}
pub fn flush_save(s: &mut AppState) {
    if let Some((path, frame)) = s.editor.annotation.as_mut().and_then(Annotation::take_save) {
        s.job(move || {
            toolkit::utilities::save_image(&path, &frame)
                .map(|_| JobResult::Notice(format!("Saved {}", path.display())))
                .map_err(|e| format!("{e:#}"))
        });
    }
}
