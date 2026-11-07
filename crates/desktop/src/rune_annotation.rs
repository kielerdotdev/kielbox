//! Existing annotation actor, with Rune pointer/key input and a bounded raster preview.
use crate::rune_ui::{JobResult, State as AppState, button, color, field, heading, label};
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
pub fn view(s: &AppState) -> El {
    let Some(a) = &s.editor.annotation else {
        return col();
    };
    let mut tools = row().gap(4.0);
    for tool in a::Tool::ALL {
        tools = tools.child(
            button(
                format!("annotation-{tool:?}"),
                format!("{tool:?}"),
                move |s, _| {
                    if let Some(a) = &mut s.editor.annotation {
                        a.send(Command::Tool(tool));
                    }
                },
            )
            .checked(a.active == tool),
        );
    }
    let actions = row()
        .gap(6.0)
        .child(button("annotation-undo", "Undo", |s, _| {
            if let Some(a) = &mut s.editor.annotation {
                a.send(Command::Undo);
            }
        }))
        .child(button("annotation-redo", "Redo", |s, _| {
            if let Some(a) = &mut s.editor.annotation {
                a.send(Command::Redo);
            }
        }))
        .child(button("annotation-crop", "Apply crop", |s, _| {
            if let Some(a) = &mut s.editor.annotation {
                a.send(Command::ApplyCrop);
            }
        }))
        .child(button("annotation-rotate", "Rotate", |s, _| {
            if let Some(a) = &mut s.editor.annotation {
                a.send(Command::Rotate);
            }
        }))
        .child(button("annotation-save", "Save image…", |_, tx| {
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
        }))
        .child(button(
            "annotation-project",
            "Save editable project…",
            |_, tx| {
                tx.save_file(
                    rune::FileDialogOptions::default().filter("Annotation project", &["ssxe"]),
                    |s: &mut AppState, result, _| {
                        if let Ok(Some(path)) = result {
                            if let Some(a) = &mut s.editor.annotation {
                                a.send(Command::Project(path));
                            }
                        }
                    },
                )
            },
        ));
    let canvas = el()
        .key("annotation-canvas")
        .grow(1.0)
        .min_h(100.0)
        .w_full()
        .fill(color(0x181a1f))
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
    col()
        .grow(1.0)
        .min_h(0.0)
        .gap(8.0)
        .child(heading("Image editor"))
        .child(tools.scroll_x().h(44.0))
        .child(actions)
        .child(
            field("annotation-label", &a.text, |s| {
                &mut s.editor.annotation.as_mut().unwrap().text
            })
            .on_input_change(|s: &mut AppState, _| {
                if let Some(a) = &mut s.editor.annotation {
                    a.send(Command::Label(a.text.text.clone()));
                }
            }),
        )
        .child(canvas)
        .child(label(&a.message))
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
