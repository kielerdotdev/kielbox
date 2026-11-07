//! Kielbox's "Quiet" look on Rune: tokens, type and the small set of controls every page
//! shares (buttons, switches, segmented controls, fields, the icon rail and title-bar tabs).
//! Colours, sizes and weights follow the design canvas; views never pick raw colours.
use crate::rune_ui::State;
use rune::{
    Color, El, Icon, Shadow, TextEdit, TextStyle, Tx, col, el, icon, row, text, text_input,
};

/// Window ground behind the rail and title bar.
pub const DEEP: u32 = 0x141414;
/// The content card.
pub const BG: u32 = 0x1d1d1d;
/// Floating surfaces, selected list rows.
pub const BG2: u32 = 0x262626;
pub const BG3: u32 = 0x2f2f2f;
pub const T1: u32 = 0xf2f2f2;
pub const T2: u32 = 0xa8a8a8;
pub const T3: u32 = 0x8f8f8f;
pub const ACCENT: u32 = 0x49c970;
pub const ACCENT_HOVER: u32 = 0x5bd27e;
/// Text on the accent.
pub const INK: u32 = 0x07210e;
pub const REC: u32 = 0xf1484f;
pub const REC_T: u32 = 0xf29b96;
pub const BUSY: u32 = 0xf2cf76;

pub fn c(hex: u32) -> Color {
    crate::rune_ui::color(hex)
}
pub fn white(a: f32) -> Color {
    Color::WHITE.alpha(a)
}
pub fn black(a: f32) -> Color {
    Color::BLACK.alpha(a)
}
/// Hairline separators.
pub fn line() -> Color {
    white(0.08)
}

pub const BODY: TextStyle = TextStyle::new(14.0, 440.0).lh(20.0);
pub const UI: TextStyle = TextStyle::new(13.0, 530.0).lh(16.0);
pub const SMALL: TextStyle = TextStyle::new(12.5, 440.0).lh(18.0);
pub const LABEL: TextStyle = TextStyle::new(12.0, 530.0).lh(16.0);
pub const RAIL: TextStyle = TextStyle::new(10.5, 530.0).lh(12.0);
pub const H1: TextStyle = TextStyle::new(20.0, 600.0).lh(28.0).tracking(-0.24);
pub const H2: TextStyle = TextStyle::new(15.0, 600.0).lh(20.0);
pub const KBD: TextStyle = TextStyle::new(11.0, 400.0).lh(14.0).mono();
pub const MONO: TextStyle = TextStyle::new(12.0, 400.0).lh(18.0).mono();
pub const NUM: TextStyle = TextStyle::new(12.5, 440.0).lh(18.0).features("'tnum' 1");
pub const BIG: TextStyle = TextStyle::new(44.0, 600.0)
    .lh(44.0)
    .tracking(-0.88)
    .features("'tnum' 1");

pub fn t1(value: impl Into<String>) -> El {
    text(value).font(BODY).color(c(T1))
}
pub fn t2(value: impl Into<String>) -> El {
    text(value).font(BODY).color(c(T2))
}
/// Secondary copy under a control or a heading.
pub fn small(value: impl Into<String>) -> El {
    text(value).font(SMALL).color(c(T2))
}
pub fn h1(value: impl Into<String>) -> El {
    text(value).font(H1).color(c(T1)).role("heading").level(1)
}
pub fn h2(value: impl Into<String>) -> El {
    text(value).font(H2).color(c(T1)).role("heading").level(2)
}
/// A muted section label (`Input`, `Pinned`, `Largest folders`).
pub fn section_label(value: impl Into<String>) -> El {
    text(value)
        .font(LABEL)
        .color(c(T3))
        .role("heading")
        .level(2)
}
/// A keyboard hint drawn inside a control.
pub fn kbd(value: impl Into<String>) -> El {
    text(value).font(KBD).color(c(T3))
}
pub fn hr() -> El {
    el().w_full().h(1.0).fill(line()).a11y_hidden()
}
pub fn dot(size: f32, color: Color) -> El {
    el().size(size, size)
        .radius(size / 2.0)
        .fill(color)
        .a11y_hidden()
}
pub fn spacer() -> El {
    el().grow(1.0)
}
pub fn glyph(i: &'static Icon, size: f32, color: Color) -> El {
    icon(i, size).color(color).a11y_hidden()
}

fn raised(e: El) -> El {
    e.box_shadow(Shadow {
        y: 1.0,
        blur: 2.0,
        color: black(0.4),
        ..Shadow::default()
    })
    .box_shadow(Shadow {
        spread: 0.5,
        color: white(0.18),
        ..Shadow::default()
    })
}
/// Popovers, toasts and the search palette.
pub fn floating(e: El) -> El {
    e.fill(c(BG2))
        .radius(10.0)
        .box_shadow(Shadow {
            y: 16.0,
            blur: 32.0,
            color: black(0.35),
            ..Shadow::default()
        })
        .box_shadow(Shadow {
            y: 8.0,
            blur: 16.0,
            color: black(0.3),
            ..Shadow::default()
        })
        .box_shadow(Shadow {
            spread: 0.5,
            color: white(0.16),
            ..Shadow::default()
        })
}
fn ring(e: El, color: Color) -> El {
    e.box_shadow(Shadow {
        spread: 0.5,
        color,
        ..Shadow::default()
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// Raised neutral.
    Neutral,
    /// The one accent action on a surface.
    Primary,
    /// No surface until hovered.
    Ghost,
    /// Stop recording.
    Rec,
}

type Action = Box<dyn Fn(&mut State, &mut Tx)>;

/// A button: `Btn::new("key", "Title").primary().icon(&icons::PLAY).on(|s, tx| ..)`.
pub struct Btn {
    key: String,
    title: String,
    variant: Variant,
    icon: Option<&'static Icon>,
    trailing: Option<&'static Icon>,
    hint: Option<String>,
    height: f32,
    icon_only: bool,
    wide: bool,
    disabled: bool,
    checked: Option<bool>,
    lead: Option<El>,
    action: Option<Action>,
}
impl Btn {
    pub fn new(key: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            variant: Variant::Neutral,
            icon: None,
            trailing: None,
            hint: None,
            height: 28.0,
            icon_only: false,
            wide: false,
            disabled: false,
            checked: None,
            lead: None,
            action: None,
        }
    }
    /// An icon-only button; `title` becomes its accessible name.
    pub fn icon_only(key: impl Into<String>, title: impl Into<String>, i: &'static Icon) -> Self {
        let mut b = Self::new(key, title).ghost();
        b.icon = Some(i);
        b.icon_only = true;
        b
    }
    pub fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }
    pub fn ghost(mut self) -> Self {
        self.variant = Variant::Ghost;
        self
    }
    pub fn rec(mut self) -> Self {
        self.variant = Variant::Rec;
        self
    }
    pub fn variant(mut self, v: Variant) -> Self {
        self.variant = v;
        self
    }
    pub fn icon(mut self, i: &'static Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn trailing(mut self, i: &'static Icon) -> Self {
        self.trailing = Some(i);
        self
    }
    /// A leading mark that isn't an icon (the record dot).
    pub fn lead(mut self, mark: El) -> Self {
        self.lead = Some(mark);
        self
    }
    /// A shortcut shown at the trailing edge.
    pub fn hint(mut self, keys: impl Into<String>) -> Self {
        let keys = keys.into();
        if !keys.is_empty() {
            self.hint = Some(keys);
        }
        self
    }
    pub fn h(mut self, height: f32) -> Self {
        self.height = height;
        self
    }
    /// Fills its row, the label centred unless a hint pushes it left.
    pub fn wide(mut self) -> Self {
        self.wide = true;
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }
    pub fn on(mut self, f: impl Fn(&mut State, &mut Tx) + 'static) -> Self {
        self.action = Some(Box::new(f));
        self
    }
    pub fn build(self) -> El {
        let (fill, hover, fg) = match self.variant {
            Variant::Neutral => (white(0.06), white(0.1), c(T1)),
            Variant::Primary => (c(ACCENT), c(ACCENT_HOVER), c(INK)),
            Variant::Ghost => (Color::TRANSPARENT, white(0.06), c(T2)),
            Variant::Rec => (c(REC).alpha(0.14), c(REC).alpha(0.22), c(REC_T)),
        };
        let fg = if self.checked == Some(true) && self.variant == Variant::Ghost {
            c(T1)
        } else {
            fg
        };
        let large = self.height >= 40.0;
        let mut b = row()
            .key(self.key)
            .role("button")
            .label(self.title.clone())
            .focusable(true)
            .h(self.height)
            .gap(7.0)
            .center_y()
            .radius(6.0)
            .fill(fill)
            .hover(hover)
            .press(hover)
            .color(fg);
        b = if self.icon_only {
            b.w(self.height).justify_center()
        } else {
            b.px(if large { 14.0 } else { 10.0 })
        };
        b = match self.variant {
            Variant::Neutral => raised(b),
            Variant::Primary => b
                .box_shadow(Shadow {
                    y: 1.0,
                    blur: 2.0,
                    color: black(0.4),
                    ..Shadow::default()
                })
                .box_shadow(Shadow {
                    spread: 0.5,
                    color: white(0.22),
                    inset: true,
                    ..Shadow::default()
                }),
            Variant::Rec => b
                .box_shadow(Shadow {
                    y: 1.0,
                    blur: 2.0,
                    color: black(0.4),
                    ..Shadow::default()
                })
                .box_shadow(Shadow {
                    spread: 0.5,
                    color: c(REC).alpha(0.55),
                    inset: true,
                    ..Shadow::default()
                }),
            Variant::Ghost => b,
        };
        if self.wide {
            b = b.grow(1.0).min_w(0.0);
            if self.hint.is_none() && self.trailing.is_none() {
                b = b.justify_center();
            }
        }
        if let Some(on) = self.checked {
            b = b.checked(on);
        }
        if let Some(lead) = self.lead {
            b = b.child(lead);
        }
        if let Some(i) = self.icon {
            b = b.child(glyph(i, if self.icon_only { 16.0 } else { 16.0 }, fg));
        }
        if !self.icon_only {
            let font = if large {
                TextStyle::new(14.5, 530.0).lh(18.0)
            } else {
                UI
            };
            b = b.child(text(self.title).font(font).color(fg).single_line());
        }
        if let Some(i) = self.trailing {
            b = b.child(glyph(i, 14.0, fg));
        }
        if let Some(hint) = self.hint {
            let hint_color = if self.variant == Variant::Primary {
                c(INK).alpha(0.7)
            } else {
                c(T3)
            };
            b = b.child(spacer()).child(kbd(hint).color(hint_color));
        }
        if self.disabled {
            b = b.disabled(true).opacity(0.45);
        } else if let Some(action) = self.action {
            b = b.on_press_tx(move |s: &mut State, tx| action(s, tx));
        }
        b
    }
}

/// An on/off switch, 30 × 18, accent when on.
pub fn switch(
    key: impl Into<String>,
    name: impl Into<String>,
    on: bool,
    action: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    let track = el()
        .key(key)
        .role("switch")
        .label(name)
        .checked(on)
        .focusable(true)
        .size(30.0, 18.0)
        .radius(9.0)
        .fill(if on { c(ACCENT) } else { white(0.08) })
        .on_press_tx(action)
        .child(
            el().key("knob")
                .size(14.0, 14.0)
                .radius(7.0)
                .fill(if on { Color::WHITE } else { c(0xd9d9d9) })
                .shadow(1.0, 2.0, black(0.4))
                .absolute(if on { 14.0 } else { 2.0 }, 2.0)
                .animate_layout(),
        );
    ring(track, white(if on { 0.2 } else { 0.16 }))
}

/// A labelled switch row inside a page.
pub fn switch_line(
    key: &str,
    title: &str,
    on: bool,
    action: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    row()
        .gap(10.0)
        .center_y()
        .child(switch(key, title, on, action))
        .child(t1(title))
}

/// Mutually exclusive options in one sunken track.
pub fn segmented(
    key: &str,
    name: &str,
    options: Vec<(String, bool)>,
    pick: impl Fn(&mut State, usize) + Clone + 'static,
) -> El {
    let mut group = row()
        .key(key.to_string())
        .role("radiogroup")
        .label(name.to_string())
        .gap(2.0)
        .padding(2.0)
        .radius(8.0)
        .fill(black(0.22));
    group = ring(group, white(0.1));
    for (i, (title, on)) in options.into_iter().enumerate() {
        let pick = pick.clone();
        let mut b = row()
            .key(format!("{key}-{i}"))
            .role("radio")
            .label(title.clone())
            .checked(on)
            .focusable(true)
            .h(24.0)
            .px(10.0)
            .center_y()
            .radius(6.0)
            .on_press(move |s: &mut State| pick(s, i))
            .child(
                text(title)
                    .font(UI)
                    .color(c(if on { T1 } else { T2 }))
                    .single_line(),
            );
        b = if on {
            raised(b.fill(white(0.1)))
        } else {
            b.hover(white(0.04))
        };
        group = group.child(b);
    }
    group
}

/// Filter chips: text buttons, the selected one raised.
pub fn chips(
    key: &str,
    options: &[&str],
    selected: usize,
    pick: impl Fn(&mut State, usize) + Clone + 'static,
) -> El {
    let mut group = row().key(key.to_string()).role("radiogroup").gap(2.0);
    for (i, title) in options.iter().enumerate() {
        let pick = pick.clone();
        let on = i == selected;
        let mut b = row()
            .key(format!("{key}-{i}"))
            .role("radio")
            .label(title.to_string())
            .checked(on)
            .focusable(true)
            .h(28.0)
            .px(10.0)
            .center_y()
            .radius(6.0)
            .on_press(move |s: &mut State| pick(s, i))
            .child(
                text(title.to_string())
                    .font(UI)
                    .color(c(if on { T1 } else { T2 }))
                    .single_line(),
            );
        b = if on {
            raised(b.fill(white(0.08)))
        } else {
            b.hover(white(0.04))
        };
        group = group.child(b);
    }
    group
}

/// A single-line text field on a sunken, hairline-ringed surface.
pub fn field(
    key: &str,
    name: &str,
    edit: &TextEdit,
    access: impl for<'a> Fn(&'a mut State) -> &'a mut TextEdit + 'static,
) -> El {
    field_lead(None, key, name, edit, access)
}

/// A [`field`] with a leading icon or caption (`Start`, the search glass).
pub fn field_lead(
    lead: Option<El>,
    key: &str,
    name: &str,
    edit: &TextEdit,
    access: impl for<'a> Fn(&'a mut State) -> &'a mut TextEdit + 'static,
) -> El {
    field_shell(lead, input(key, name, edit, access))
}

/// The bare text input of a [`field`], for attaching input handlers before [`field_shell`].
pub fn input(
    key: &str,
    name: &str,
    edit: &TextEdit,
    access: impl for<'a> Fn(&'a mut State) -> &'a mut TextEdit + 'static,
) -> El {
    text_input(edit, TextStyle::new(13.0, 440.0).lh(16.0))
        .key(key.to_string())
        .label(name.to_string())
        .grow(1.0)
        .min_w(0.0)
        .color(c(T1))
        .placeholder(name.to_string(), c(T3))
        .single_line()
        .on_edit(access)
}

/// The sunken, hairline-ringed surface around an [`input`].
pub fn field_shell(lead: Option<El>, input: El) -> El {
    let mut shell = row()
        .h(28.0)
        .px(10.0)
        .gap(8.0)
        .center_y()
        .radius(6.0)
        .fill(white(0.03));
    if let Some(lead) = lead {
        shell = shell.child(lead);
    }
    ring(shell.child(input), white(0.16))
}

/// A settings line: title (and description) left, control right, hairline below.
pub fn setting(title: &str, description: Option<&str>, control: El, last: bool) -> El {
    let mut copy = col().grow(1.0).min_w(0.0).child(t1(title));
    if let Some(d) = description {
        copy = copy.child(small(d));
    }
    let line = row()
        .w_full()
        .gap(16.0)
        .py(12.0)
        .center_y()
        .child(copy)
        .child(control);
    if last {
        line
    } else {
        col().w_full().child(line).child(hr())
    }
}

/// A navigation item in the 72 pt icon rail.
pub fn rail_item(
    key: &str,
    i: &'static Icon,
    title: &str,
    on: bool,
    action: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    let fg = c(if on { T1 } else { T3 });
    let mut item = col()
        .key(key.to_string())
        .role("tab")
        .label(title.to_string())
        .checked(on)
        .focusable(true)
        .w(56.0)
        .pt(8.0)
        .pb(7.0)
        .gap(4.0)
        .items_center()
        .radius(8.0)
        .on_press_tx(action)
        .child(glyph(i, 20.0, fg))
        .child(text(title.to_string()).font(RAIL).color(fg).single_line());
    item = if on {
        raised(item.fill(white(0.08)))
    } else {
        item.hover(white(0.04))
    };
    item
}

/// A pulsing-ring status light: the rail's capture state.
pub fn status_light(color: Color) -> El {
    el().size(20.0, 20.0)
        .center()
        .a11y_hidden()
        .child(
            el().size(20.0, 20.0)
                .radius(10.0)
                .fill(color.alpha(0.14))
                .absolute(0.0, 0.0),
        )
        .child(
            el().size(12.0, 12.0)
                .radius(6.0)
                .fill(color.alpha(0.26))
                .absolute(4.0, 4.0),
        )
        .child(dot(8.0, color))
}

/// A tab in the title bar. `close` adds a close button.
pub fn tab(
    key: &str,
    lead: El,
    title: &str,
    on: bool,
    open: impl Fn(&mut State, &mut Tx) + 'static,
    close: Option<Box<dyn Fn(&mut State, &mut Tx)>>,
) -> El {
    let fg = c(if on { T1 } else { T2 });
    let mut t = row()
        .key(key.to_string())
        .h(28.0)
        .pl(10.0)
        .pr(if close.is_some() { 4.0 } else { 10.0 })
        .gap(4.0)
        .center_y()
        .radius(6.0)
        .no_window_drag()
        .child(
            row()
                .key("open")
                .role("tab")
                .label(title.to_string())
                .checked(on)
                .focusable(true)
                .gap(8.0)
                .center_y()
                .max_w(200.0)
                .on_press_tx(open)
                .child(lead)
                .child(
                    text(title.to_string())
                        .font(UI)
                        .color(fg)
                        .single_line()
                        .truncate(),
                ),
        );
    if let Some(close) = close {
        t = t.child(
            el().key("close")
                .role("button")
                .label(format!("Close {title}"))
                .focusable(true)
                .size(20.0, 20.0)
                .center()
                .radius(4.0)
                .hover(white(0.08))
                .on_press_tx(move |s: &mut State, tx| close(s, tx))
                .child(glyph(&icons::CLOSE, 12.0, c(T3))),
        );
    }
    if on {
        raised(t.fill(c(BG)))
    } else {
        t.hover(white(0.05))
    }
}

/// A 16:9 tile for a capture without a decoded thumbnail: its kind, centred.
pub fn tile(i: &'static Icon, caption: &str, selected: bool) -> El {
    let mut t = col()
        .w_full()
        .aspect_ratio(16.0 / 9.0)
        .radius(6.0)
        .fill(c(0x0e0e0e))
        .center()
        .gap(6.0)
        .child(glyph(i, 26.0, c(T3)))
        .child(text(caption.to_string()).font(KBD).color(c(T3)));
    t = ring(t, white(0.1));
    if selected {
        t = t.outline(2.0, 2.0, c(T1));
    }
    t
}

/// A short horizontal level or proportion bar.
pub fn meter(width: f32, fraction: f32, color: Color) -> El {
    let fraction = fraction.clamp(0.0, 1.0);
    row()
        .w(width)
        .h(4.0)
        .radius(2.0)
        .fill(c(0x3a3b40))
        .a11y_hidden()
        .child(
            el().w((width * fraction).max(0.0))
                .h(4.0)
                .radius(2.0)
                .fill(color),
        )
}

/// Line icons from the design, on a 24 pt grid, drawn in the element's colour.
pub mod icons {
    use rune::Icon;
    macro_rules! line_icon {
        ($name:ident, $body:literal) => {
            pub static $name: Icon = Icon {
                name: concat!("kielbox-", stringify!($name)),
                view: 24.0,
                body: concat!(
                    "<g fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\">",
                    $body,
                    "</g>"
                ),
            };
        };
    }
    /// The Kielbox mark, in its own colours.
    pub static LOGO: Icon = Icon {
        name: "kielbox-logo",
        view: 24.0,
        body: "<rect x=\"2\" y=\"2\" width=\"20\" height=\"10\" rx=\"3.2\" fill=\"#49C970\"/><path d=\"M10.4 4.6v4.8l4-2.4z\" fill=\"#141414\"/><rect x=\"2\" y=\"13.8\" width=\"9.1\" height=\"8.2\" rx=\"3\" fill=\"#F2F2F2\"/><rect x=\"12.9\" y=\"13.8\" width=\"9.1\" height=\"8.2\" rx=\"3\" fill=\"#F2F2F2\"/>",
    };
    line_icon!(
        LIBRARY,
        "<rect x=\"6.5\" y=\"3\" width=\"14.5\" height=\"13\" rx=\"2.5\"/><path d=\"M3 8v10a3 3 0 0 0 3 3h10\"/><path d=\"m12 7.3 3.8 2.2-3.8 2.2z\"/>"
    );
    line_icon!(
        CAPTURE,
        "<circle cx=\"12\" cy=\"12\" r=\"9\"/><circle cx=\"12\" cy=\"12\" r=\"3.6\" fill=\"currentColor\" stroke=\"none\"/>"
    );
    line_icon!(
        TOOLS,
        "<rect x=\"3\" y=\"8\" width=\"18\" height=\"12\" rx=\"2.5\"/><path d=\"M8.5 8V6a2 2 0 0 1 2-2h3a2 2 0 0 1 2 2v2\"/><path d=\"M3 13.5h18\"/><path d=\"M10 12v3h4v-3\"/>"
    );
    line_icon!(
        SETTINGS,
        "<path d=\"M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1\"/><circle cx=\"15\" cy=\"6\" r=\"2\"/><circle cx=\"9\" cy=\"12\" r=\"2\"/><circle cx=\"17\" cy=\"18\" r=\"2\"/>"
    );
    line_icon!(
        SEARCH,
        "<circle cx=\"11\" cy=\"11\" r=\"7\"/><path d=\"m20 20-3.5-3.5\"/>"
    );
    line_icon!(
        DISPLAY,
        "<rect x=\"3\" y=\"4\" width=\"18\" height=\"12\" rx=\"2\"/><path d=\"M8.5 20h7M12 16v4\"/>"
    );
    line_icon!(CHEVRON_DOWN, "<path d=\"m6 9 6 6 6-6\"/>");
    line_icon!(CHEVRON_UP, "<path d=\"m6 15 6-6 6 6\"/>");
    line_icon!(
        REGION,
        "<path d=\"M6 2.5V16a2 2 0 0 0 2 2h13.5\"/><path d=\"M18 21.5V8a2 2 0 0 0-2-2H2.5\"/>"
    );
    line_icon!(
        WINDOW,
        "<rect x=\"3\" y=\"4\" width=\"18\" height=\"16\" rx=\"2.5\"/><path d=\"M3 9h18\"/><path d=\"M6 6.5h.01M8.5 6.5h.01\"/>"
    );
    line_icon!(CLOSE, "<path d=\"M6.5 6.5l11 11M17.5 6.5l-11 11\"/>");
    line_icon!(BACK, "<path d=\"M19 12H5M11 18l-6-6 6-6\"/>");
    line_icon!(
        RESCAN,
        "<path d=\"M20 11.5a8 8 0 1 0-2.3 5.7\"/><path d=\"M20 4.5v7h-7\"/>"
    );
    line_icon!(
        EXTERNAL,
        "<path d=\"M14 4h6v6M20 4l-9 9\"/><path d=\"M18 14v4.5A1.5 1.5 0 0 1 16.5 20h-11A1.5 1.5 0 0 1 4 18.5v-11A1.5 1.5 0 0 1 5.5 6H10\"/>"
    );
    line_icon!(
        UNDO,
        "<path d=\"M9 14 4 9l5-5\"/><path d=\"M4 9h10.5a5.5 5.5 0 0 1 0 11H11\"/>"
    );
    line_icon!(
        REDO,
        "<path d=\"m15 14 5-5-5-5\"/><path d=\"M20 9H9.5a5.5 5.5 0 0 0 0 11H13\"/>"
    );
    line_icon!(
        PLAY,
        "<path d=\"M7.5 4.8v14.4L19.5 12z\" fill=\"currentColor\"/>"
    );
    line_icon!(
        PAUSE,
        "<rect x=\"6\" y=\"5\" width=\"4\" height=\"14\" rx=\"1.2\" fill=\"currentColor\" stroke=\"none\"/><rect x=\"14\" y=\"5\" width=\"4\" height=\"14\" rx=\"1.2\" fill=\"currentColor\" stroke=\"none\"/>"
    );
    line_icon!(
        PREV,
        "<path d=\"M18 5.5v13L8.5 12z\" fill=\"currentColor\"/><path d=\"M6 5.5v13\"/>"
    );
    line_icon!(
        NEXT,
        "<path d=\"M6 5.5v13l9.5-6.5z\" fill=\"currentColor\"/><path d=\"M18 5.5v13\"/>"
    );
    line_icon!(
        SPLIT,
        "<path d=\"M12 3v18\"/><path d=\"m8 8-4 4 4 4M16 8l4 4-4 4\"/>"
    );
    line_icon!(
        TRASH,
        "<path d=\"M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3\"/>"
    );
    line_icon!(
        STAR,
        "<path d=\"m12 3.5 2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8L3.5 9.7l5.9-.9z\"/>"
    );
    line_icon!(
        STAR_FILLED,
        "<path d=\"m12 3.5 2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8L3.5 9.7l5.9-.9z\" fill=\"currentColor\"/>"
    );
    line_icon!(
        DOWNLOAD,
        "<path d=\"M12 4v11M7.5 10.5 12 15l4.5-4.5\"/><path d=\"M5 20h14\"/>"
    );
    line_icon!(
        POWER,
        "<path d=\"M12 3.5v8\"/><path d=\"M6.8 6.8a7.5 7.5 0 1 0 10.4 0\"/>"
    );
    line_icon!(
        HISTORY,
        "<path d=\"M3.5 12a8.5 8.5 0 1 0 2.6-6.1L3.5 8.5\"/><path d=\"M3.5 3.5v5h5\"/><path d=\"M12 8v4.2l2.8 1.8\"/>"
    );
    line_icon!(
        WARNING,
        "<path d=\"M10.3 4.2 2.6 17.5A2 2 0 0 0 4.3 20.5h15.4a2 2 0 0 0 1.7-3L13.7 4.2a2 2 0 0 0-3.4 0z\"/><path d=\"M12 9.5v4M12 17h.01\"/>"
    );
    line_icon!(CHECK, "<path d=\"m5 12.5 4.5 4.5L19 7.5\"/>");
    line_icon!(
        MIC,
        "<rect x=\"9\" y=\"3\" width=\"6\" height=\"11\" rx=\"3\"/><path d=\"M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21\"/>"
    );
    line_icon!(
        SPEAKER,
        "<path d=\"M11 5 6.5 9H3.5v6h3L11 19z\"/><path d=\"M15.5 9a4.5 4.5 0 0 1 0 6M18.5 6a8.5 8.5 0 0 1 0 12\"/>"
    );
    line_icon!(
        LOCK,
        "<rect x=\"5\" y=\"10.5\" width=\"14\" height=\"10\" rx=\"2.5\"/><path d=\"M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5\"/>"
    );
    line_icon!(
        DISK,
        "<path d=\"M20.5 13A8.5 8.5 0 1 1 11 3.5V13z\"/><path d=\"M14.5 3.6a8.5 8.5 0 0 1 5.9 5.9h-5.9z\"/>"
    );
    line_icon!(
        PICKER,
        "<path d=\"m12 8 4 4\"/><path d=\"M5 19.5 4.5 16l8.5-8.5 3.5 3.5L8 19.5z\"/><path d=\"m14.5 6 2-2a2.1 2.1 0 0 1 3 3l-2 2\"/>"
    );
    line_icon!(
        TEXT_SCAN,
        "<path d=\"M4 8V6a2 2 0 0 1 2-2h2M16 4h2a2 2 0 0 1 2 2v2M20 16v2a2 2 0 0 1-2 2h-2M8 20H6a2 2 0 0 1-2-2v-2\"/><path d=\"M8 9h8M8 12h8M8 15h5\"/>"
    );
    line_icon!(
        QR,
        "<rect x=\"3.5\" y=\"3.5\" width=\"6.5\" height=\"6.5\" rx=\"1\"/><rect x=\"14\" y=\"3.5\" width=\"6.5\" height=\"6.5\" rx=\"1\"/><rect x=\"3.5\" y=\"14\" width=\"6.5\" height=\"6.5\" rx=\"1\"/><path d=\"M14 14h3v3h-3zM17.5 17.5h3v3h-3zM14 20.5h1.5M20.5 14v1.5\"/>"
    );
    line_icon!(
        CLIPBOARD,
        "<rect x=\"6\" y=\"4\" width=\"12\" height=\"17\" rx=\"2\"/><path d=\"M9 4V2.8h6V4\"/><path d=\"M9 10h6M9 14h4\"/>"
    );
    line_icon!(
        IMAGE,
        "<rect x=\"3\" y=\"4\" width=\"18\" height=\"16\" rx=\"2.5\"/><circle cx=\"9\" cy=\"10\" r=\"1.8\"/><path d=\"m20.5 16-4.5-4.5-8.5 8.5\"/>"
    );
    line_icon!(
        BEAUTIFY,
        "<rect x=\"5.5\" y=\"6.5\" width=\"13\" height=\"11\" rx=\"1.5\"/><rect x=\"2.5\" y=\"3.5\" width=\"19\" height=\"17\" rx=\"3.5\"/>"
    );
    line_icon!(
        EFFECTS,
        "<circle cx=\"12\" cy=\"12\" r=\"3.5\"/><path d=\"M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.3 5.3l2.1 2.1M16.6 16.6l2.1 2.1M5.3 18.7l2.1-2.1M16.6 7.4l2.1-2.1\"/>"
    );
    line_icon!(
        COMBINE,
        "<rect x=\"3\" y=\"3.5\" width=\"8\" height=\"17\" rx=\"2\"/><rect x=\"13\" y=\"3.5\" width=\"8\" height=\"17\" rx=\"2\"/>"
    );
    line_icon!(
        GRID_SPLIT,
        "<rect x=\"3\" y=\"3.5\" width=\"18\" height=\"17\" rx=\"2.5\"/><path d=\"M3 12h18M12 3.5v17\"/>"
    );
    line_icon!(
        CONVERT,
        "<path d=\"M4.5 9.5A7.5 7.5 0 0 1 18 6.5l1.5 1.5M19.5 14.5A7.5 7.5 0 0 1 6 17.5L4.5 16\"/><path d=\"M19.5 4v4h-4M4.5 20v-4h4\"/>"
    );
    line_icon!(
        THUMBNAILS,
        "<rect x=\"3.5\" y=\"3.5\" width=\"7\" height=\"7\" rx=\"1.5\"/><rect x=\"13.5\" y=\"3.5\" width=\"7\" height=\"7\" rx=\"1.5\"/><rect x=\"3.5\" y=\"13.5\" width=\"7\" height=\"7\" rx=\"1.5\"/><rect x=\"13.5\" y=\"13.5\" width=\"7\" height=\"7\" rx=\"1.5\"/>"
    );
    line_icon!(
        FILM,
        "<rect x=\"3\" y=\"4\" width=\"18\" height=\"16\" rx=\"2.5\"/><path d=\"M7.5 4v16M16.5 4v16M3 9h4.5M3 15h4.5M16.5 9H21M16.5 15H21\"/>"
    );
    line_icon!(
        HASH,
        "<path d=\"M5 9h15M4 15h15M10 3.5 8 20.5M16 3.5l-2 17\"/>"
    );
    line_icon!(
        INDEX,
        "<path d=\"M4 5h6M4 5v14M4 12h4M4 19h4\"/><rect x=\"10\" y=\"9.5\" width=\"10\" height=\"5\" rx=\"1.5\"/><rect x=\"10\" y=\"16.5\" width=\"10\" height=\"5\" rx=\"1.5\"/><rect x=\"12\" y=\"2.5\" width=\"8\" height=\"5\" rx=\"1.5\"/>"
    );
    line_icon!(
        TERMINAL,
        "<rect x=\"3\" y=\"4\" width=\"18\" height=\"16\" rx=\"2.5\"/><path d=\"m7 9.5 3 2.5-3 2.5M12.5 15H17\"/>"
    );
    // Annotation tools.
    line_icon!(SELECT, "<path d=\"M5.5 3.5 19 10l-6 2-2 6.5z\"/>");
    line_icon!(
        RECT,
        "<rect x=\"4\" y=\"5.5\" width=\"16\" height=\"13\" rx=\"1.5\"/>"
    );
    line_icon!(ARROW, "<path d=\"M5 19 19 5M9 5h10v10\"/>");
    line_icon!(TEXT, "<path d=\"M5 7V4.5h14V7M12 4.5v15M9 19.5h6\"/>");
    line_icon!(
        HIGHLIGHT,
        "<path d=\"m9 11-5.5 5.5V20h8.5l3-3\"/><path d=\"m21 11.5-4.6 4.6a2 2 0 0 1-2.8 0l-5.2-5.2a2 2 0 0 1 0-2.8L13 3.5\"/>"
    );
    line_icon!(
        BLUR,
        "<rect x=\"4\" y=\"4\" width=\"4.5\" height=\"4.5\" fill=\"currentColor\" stroke=\"none\"/><rect x=\"13\" y=\"4\" width=\"4.5\" height=\"4.5\" fill=\"currentColor\" stroke=\"none\" opacity=\".55\"/><rect x=\"8.5\" y=\"8.5\" width=\"4.5\" height=\"4.5\" fill=\"currentColor\" stroke=\"none\" opacity=\".75\"/><rect x=\"17.5\" y=\"8.5\" width=\"2.5\" height=\"4.5\" fill=\"currentColor\" stroke=\"none\" opacity=\".4\"/><rect x=\"4\" y=\"13\" width=\"4.5\" height=\"4.5\" fill=\"currentColor\" stroke=\"none\" opacity=\".45\"/><rect x=\"13\" y=\"13\" width=\"4.5\" height=\"4.5\" fill=\"currentColor\" stroke=\"none\"/><rect x=\"8.5\" y=\"17.5\" width=\"4.5\" height=\"2.5\" fill=\"currentColor\" stroke=\"none\" opacity=\".6\"/>"
    );
    line_icon!(
        STEP,
        "<circle cx=\"12\" cy=\"12\" r=\"8.5\"/><path d=\"M10.8 9.2 12.6 8v8\"/>"
    );
    line_icon!(
        PIN,
        "<path d=\"M12 16.5V21M8.5 3.5h7l-1 5.5 3 3V14h-11v-2l3-3z\"/>"
    );
    line_icon!(
        FREEHAND,
        "<path d=\"M3.5 17c2.5-6 5-9 7-9s1.5 5 3.5 5 3-4 6.5-6\"/>"
    );
    line_icon!(LINE, "<path d=\"M5 19 19 5\"/>");
    line_icon!(
        ELLIPSE,
        "<ellipse cx=\"12\" cy=\"12\" rx=\"8.5\" ry=\"6.5\"/>"
    );
    line_icon!(
        CROP,
        "<path d=\"M6 2.5V16a2 2 0 0 0 2 2h13.5\"/><path d=\"M18 21.5V8a2 2 0 0 0-2-2H2.5\"/>"
    );
    line_icon!(
        ROTATE,
        "<path d=\"M20 11.5a8 8 0 1 0-2.3 5.7\"/><path d=\"M20 4.5v7h-7\"/>"
    );
    line_icon!(
        FILE,
        "<path d=\"M14 3.5H7a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8.5z\"/><path d=\"M14 3.5v5h5\"/>"
    );
    line_icon!(
        FOLDER,
        "<path d=\"M3.5 7.5a2 2 0 0 1 2-2h4l2 2.5h7a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z\"/>"
    );
}
