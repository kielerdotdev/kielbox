use gpui::{prelude::*, *};
use std::borrow::Cow;

pub const BG: u32 = 0x0b0c0e;
pub const PANEL: u32 = 0x131417;
pub const LINE: u32 = 0x232428;
pub const MUTED: u32 = 0xa1a1aa;
pub const ACCENT: u32 = 0x3ecf8e;
pub const TEXT: u32 = 0xededef;
pub const SOFT: u32 = 0x1f2024;

pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            "icons/camera.svg" => include_bytes!("../../../assets/icons/camera.svg"),
            "icons/video-camera.svg" => include_bytes!("../../../assets/icons/video-camera.svg"),
            "icons/capture-area.svg" => include_bytes!("../../../assets/icons/capture-area.svg"),
            "icons/capture-window.svg" => include_bytes!("../../../assets/icons/capture-window.svg"),
            "icons/capture-settings.svg" => include_bytes!("../../../assets/icons/capture-settings.svg"),
            "icons/arrow-left.svg" => include_bytes!("../../../assets/icons/arrow-left.svg"),
            "icons/check.svg" => include_bytes!("../../../assets/icons/check.svg"),
            "icons/chevron.svg" => include_bytes!("../../../assets/icons/chevron.svg"),
            "icons/close.svg" => include_bytes!("../../../assets/icons/close.svg"),
            "icons/copy.svg" => include_bytes!("../../../assets/icons/copy.svg"),
            "icons/download.svg" => include_bytes!("../../../assets/icons/download.svg"),
            "icons/film.svg" => include_bytes!("../../../assets/icons/film.svg"),
            "icons/folder.svg" => include_bytes!("../../../assets/icons/folder.svg"),
            "icons/grid.svg" => include_bytes!("../../../assets/icons/grid.svg"),
            "icons/landscape.svg" => include_bytes!("../../../assets/icons/landscape.svg"),
            "icons/list.svg" => include_bytes!("../../../assets/icons/list.svg"),
            "icons/mic.svg" => include_bytes!("../../../assets/icons/mic.svg"),
            "icons/monitor.svg" => include_bytes!("../../../assets/icons/monitor.svg"),
            "icons/muted.svg" => include_bytes!("../../../assets/icons/muted.svg"),
            "icons/pause.svg" => include_bytes!("../../../assets/icons/pause.svg"),
            "icons/play.svg" => include_bytes!("../../../assets/icons/play.svg"),
            "icons/plus.svg" => include_bytes!("../../../assets/icons/plus.svg"),
            "icons/portrait.svg" => include_bytes!("../../../assets/icons/portrait.svg"),
            "icons/record.svg" => include_bytes!("../../../assets/icons/record.svg"),
            "icons/redo.svg" => include_bytes!("../../../assets/icons/redo.svg"),
            "icons/search.svg" => include_bytes!("../../../assets/icons/search.svg"),
            "icons/sort.svg" => include_bytes!("../../../assets/icons/sort.svg"),
            "icons/speaker.svg" => include_bytes!("../../../assets/icons/speaker.svg"),
            "icons/split.svg" => include_bytes!("../../../assets/icons/split.svg"),
            "icons/star.svg" => include_bytes!("../../../assets/icons/star.svg"),
            "icons/stop.svg" => include_bytes!("../../../assets/icons/stop.svg"),
            "icons/trash.svg" => include_bytes!("../../../assets/icons/trash.svg"),
            "icons/undo.svg" => include_bytes!("../../../assets/icons/undo.svg"),
            "icons/capture-select.svg" => {
                include_bytes!("../../../assets/icons/capture-select.svg")
            }
            "icons/capture-arrow.svg" => include_bytes!("../../../assets/icons/capture-arrow.svg"),
            "icons/capture-rect.svg" => include_bytes!("../../../assets/icons/capture-rect.svg"),
            "icons/capture-ellipse.svg" => {
                include_bytes!("../../../assets/icons/capture-ellipse.svg")
            }
            "icons/capture-pen.svg" => include_bytes!("../../../assets/icons/capture-pen.svg"),
            "icons/capture-text.svg" => include_bytes!("../../../assets/icons/capture-text.svg"),
            "icons/capture-step.svg" => include_bytes!("../../../assets/icons/capture-step.svg"),
            "icons/capture-highlight.svg" => {
                include_bytes!("../../../assets/icons/capture-highlight.svg")
            }
            "icons/capture-blur.svg" => include_bytes!("../../../assets/icons/capture-blur.svg"),
            "icons/capture-pixel.svg" => include_bytes!("../../../assets/icons/capture-pixel.svg"),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }
    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}
pub fn init(cx: &mut App) {
    if let Err(error) = cx.text_system().add_fonts(vec![
        Cow::Borrowed(include_bytes!("../../../assets/fonts/Geist.ttf")),
        Cow::Borrowed(include_bytes!("../../../assets/fonts/GeistMono.ttf")),
    ]) {
        eprintln!("Bundled fonts: {error}");
    }
}
pub fn icon(name: &'static str) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(px(16.))
        .flex_shrink_0()
        .text_color(rgb(MUTED))
}
pub fn logo() -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().size(px(6.)).rounded_full().bg(rgb(ACCENT)))
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(15.))
                .child("rcr"),
        )
}
pub fn muted(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .child(text.into())
}
pub fn mono(text: impl Into<SharedString>) -> Div {
    muted(text).font_family("Geist Mono")
}
pub fn button(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
    button_with_hover(id, text, 0x2a2b30)
}
/// Supply the hover color at construction: GPUI permits one hover style per element.
pub fn button_with_hover(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    hover_color: u32,
) -> Stateful<Div> {
    div()
        .id(id)
        .tab_index(0)
        .border_1()
        .border_color(rgba(0x00000000))
        .focus(|s| s.border_color(rgb(ACCENT)))
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .h(px(32.))
        .px_3()
        .rounded(px(8.))
        .bg(rgb(SOFT))
        .text_size(px(13.))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(hover_color)))
        .child(text.into())
}
pub fn toggle(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .tab_index(0)
        .border_1()
        .border_color(rgba(0x00000000))
        .focus(|s| s.border_color(rgb(TEXT)))
        .relative()
        .w(px(32.))
        .h(px(18.))
        .flex_shrink_0()
        .rounded_full()
        .bg(rgb(if on { ACCENT } else { 0x35363c }))
        .cursor_pointer()
        .child(
            div()
                .absolute()
                .top(px(2.))
                .left(px(if on { 16. } else { 2. }))
                .size(px(14.))
                .rounded_full()
                .bg(rgb(if on { 0x0b3020 } else { 0xededef })),
        )
}
pub fn panel() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(12.))
        .bg(rgb(PANEL))
}
