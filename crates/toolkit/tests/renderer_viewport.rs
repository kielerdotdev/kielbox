//! Public renderer checks, runnable through the host workspace without the
//! upstream vendor workspace's unavailable development dependencies.
use toolkit::{
    annotation as a, effects,
    types::{Frame, Rect},
};

fn patterned(width: u32, height: u32, transparent: bool) -> Frame {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[
                (x % 251) as u8,
                ((y * 6) % 253) as u8,
                ((x * 3 + y) % 233) as u8,
                if transparent {
                    (80 + x % 176) as u8
                } else {
                    255
                },
            ]);
        }
    }
    Frame::from_rgba8(width, height, pixels).unwrap()
}

fn check_crops(doc: &a::Document, scale: f32, filter: a::BaseFilter, label: &str) {
    let mut renderer = a::Renderer::new();
    let options = a::RenderOptions {
        scale,
        base_filter: filter,
        ..Default::default()
    };
    let full = renderer.render(doc, &options);
    for (x, y) in [(7, 5), (519, 11), (857, 23)] {
        let viewport = Rect::new(
            x,
            y,
            513.min(full.width() - x as u32),
            137.min(full.height() - y as u32),
        );
        let actual = renderer.render(
            doc,
            &a::RenderOptions {
                viewport: Some(viewport),
                ..options.clone()
            },
        );
        let expected = full.crop(viewport).unwrap();
        let first = actual
            .data()
            .iter()
            .zip(expected.data())
            .position(|(a, b)| a != b);
        assert_eq!(
            first, None,
            "{label}: {filter:?}, scale {scale}, viewport {viewport:?}"
        );
    }
}

#[test]
fn public_nearest_bilinear_and_auto_keep_global_phase_with_padding_and_alpha() {
    let mut doc = a::Document::new(patterned(1400, 180, true)).unwrap();
    doc.set_padding(a::doc::Padding::uniform(3));
    for scale in [0.65, 1.25, 2.5] {
        for filter in [
            a::BaseFilter::Nearest,
            a::BaseFilter::Bilinear,
            a::BaseFilter::Auto,
        ] {
            check_crops(&doc, scale, filter, "padded translucent base");
        }
    }
}

fn rectangle(session: &mut a::EditorSession, x: f32, y: f32, width: f32, color: a::Color) {
    session.clear_selection();
    session.set_tool(a::Tool::Rectangle);
    session.set_style(|style| {
        style.stroke_width = width;
        style.stroke = color;
        style.fill = a::Fill::None;
        style.opacity = 0.65;
        style.dash = a::DashStyle::Dash;
    });
    session.pointer_down(a::PointF::new(x, y), a::Modifiers::NONE, None);
    session.pointer_up(a::PointF::new(x + 35.5, y + 23.75), a::Modifiers::NONE);
}

#[test]
fn reused_scratch_keeps_tiny_translucent_strokes_and_hairlines_exact() {
    let mut session = a::EditorSession::from_frame(patterned(1400, 300, false)).unwrap();
    for i in 0..36 {
        rectangle(
            &mut session,
            18.25 + i as f32 * 36.,
            40.25 + (i % 4) as f32 * 42.,
            [0.25, 0.75, 3.5][i % 3],
            a::Color::rgba((i * 7) as u8, 190, 70, 130),
        );
    }
    let doc = session.document().clone();
    for scale in [0.65, 1., 1.25, 2.5] {
        check_crops(
            &doc,
            scale,
            a::BaseFilter::Auto,
            "tiny strokes and hairlines",
        );
    }
    // A fresh render thread starts with an empty scratch mask. Compare it to
    // repeated rendering after a differently positioned, opaque large stroke.
    let clean_doc = doc.clone();
    let clean = std::thread::spawn(move || a::render(&clean_doc, &Default::default()).into_data())
        .join()
        .unwrap();
    let mut previous =
        a::EditorSession::from_frame(effects::solid_frame(1400, 300, [255; 4])).unwrap();
    rectangle(&mut previous, 7., 5., 28., a::Color::rgb(240, 15, 80));
    previous.set_style(|style| style.opacity = 1.);
    for _ in 0..3 {
        let _ = previous.render(&Default::default());
        assert_eq!(
            a::render(&doc, &Default::default()).data(),
            clean.as_slice(),
            "Reused mask retained pixels from another object/document"
        );
    }
}

#[test]
fn gradient_fill_and_text_crop_coverage() {
    let mut session = a::EditorSession::from_frame(patterned(1400, 300, true)).unwrap();
    session.set_tool(a::Tool::Rectangle);
    session.set_style(|style| {
        style.stroke_width = 0.;
        style.fill = a::Fill::Gradient {
            from: a::Color::rgba(13, 97, 205, 160),
            to: a::Color::rgba(223, 44, 19, 210),
            angle: 23.,
        };
    });
    session.pointer_down(a::PointF::new(450.5, 35.25), a::Modifiers::NONE, None);
    session.pointer_up(a::PointF::new(1080.5, 110.75), a::Modifiers::NONE);
    for scale in [0.65, 1., 1.25] {
        check_crops(
            session.document(),
            scale,
            a::BaseFilter::Auto,
            "gradient rectangle only",
        );
    }
    let mut session = a::EditorSession::from_frame(patterned(1400, 300, true)).unwrap();
    session.clear_selection();
    session.set_tool(a::Tool::Text);
    session.pointer_down(a::PointF::new(470.5, 140.25), a::Modifiers::NONE, None);
    session.pointer_up(a::PointF::new(470.5, 140.25), a::Modifiers::NONE);
    session.text_insert("Viewport phase 0123456789");
    session.commit_text_edit();
    session.set_kind_props(|kind| {
        if let a::ObjectKind::Text(text) = kind {
            text.content.font.size = 32.;
            text.content.color = a::Color::rgba(15, 45, 95, 180);
        }
    });
    for scale in [0.65, 1., 1.25] {
        check_crops(
            session.document(),
            scale,
            a::BaseFilter::Auto,
            "glyphs only",
        );
    }
}
