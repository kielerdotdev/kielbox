//! The renderer: `Document` → pixels.
//!
//! # Pipeline
//!
//! 1. Work out the output rectangle (whole canvas or a viewport) in *output pixels*
//!    (`canvas pixels × scale`, origin = top-left of the padded canvas).
//! 2. Grow it to the *work rectangle*: effect objects (blur, pixelate, magnifier) read the
//!    pixels beneath them, so everything they need must be painted even if it lies outside
//!    the viewport. Iterated to a fixed point.
//! 3. Paint into a premultiplied `tiny-skia` pixmap: background, base image, then objects
//!    bottom to top. Objects that need opacity, a shadow or a blend mode are painted into
//!    a private layer and composited; the rest paint directly.
//! 4. Un-premultiply the requested part into an RGBA8 [`Frame`].
//!
//! At scale 1 the base image is copied, not resampled, and everything is deterministic
//! (pure Rust, no system fonts), so exports are identical on every OS up to the
//! rasteriser's own float determinism (golden tests use a tiny tolerance for that reason).

use std::{cell::RefCell, sync::Arc};

use rayon::prelude::*;
use image_effects::{BlurMethod, Lens, LensShape};
use image_types::{Frame, Rect};
use tiny_skia::{
    BlendMode as SkBlend, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient,
    Paint, Path, PathBuilder, Pattern, Pixmap, PixmapPaint, Shader, SpreadMode, Stroke, StrokeDash,
    Transform,
};

use crate::{
    doc::Document,
    geom::{Color, PointF, RectF},
    object::{
        ArrowHeads, BuiltinSticker, EffectBox, GridPattern, HeadStyle, Object, ObjectKind,
        StickerSource, TextContent,
    },
    shapes,
    style::{BlendMode, Fill, Style},
    text::TextEngine,
};

/// How the base image is resampled when the scale is not 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BaseFilter {
    /// Nearest when zoomed in by 2× or more, bilinear otherwise.
    #[default]
    Auto,
    /// Crisp pixels.
    Nearest,
    /// Bilinear.
    Bilinear,
    /// Bicubic.
    Bicubic,
}

/// Options for [`Renderer::render`].
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Output pixels per image pixel (zoom). 1.0 gives the pixel-exact export.
    pub scale: f32,
    /// Only render this part of the (scaled) canvas, in output pixels relative to the
    /// top-left of the padded canvas. The returned frame has exactly this size; parts
    /// outside the canvas are transparent. `None` renders the whole canvas.
    pub viewport: Option<Rect>,
    /// Draw thin dotted outlines around every object's bounds (debug aid; off for exports).
    pub include_guides: bool,
    /// Base-image resampling.
    pub base_filter: BaseFilter,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { scale: 1.0, viewport: None, include_guides: false, base_filter: BaseFilter::Auto }
    }
}

/// A lower-density raster covering an unchanged logical preview viewport.
/// Integer cover edges share the global output grid; consumers crop overscan
/// while mapping raster pixels back through `density`, without changing zoom.
#[derive(Clone, Copy, Debug)]
pub struct PreviewRasterPlan {
    pub density: f32,
    pub viewport: Rect,
    /// Largest work/returned pixmap area, excluding cached source pixels.
    pub work_pixels: u64,
}

struct BaseCache {
    frame: Arc<Frame>,
    rev: u64,
    pixmap: Arc<Pixmap>,
    opaque: bool,
}

/// Paints documents. Owns the [`TextEngine`] and caches converted images.
pub struct Renderer {
    text: TextEngine,
    base: Option<BaseCache>,
    images: Vec<(Arc<Frame>, Arc<Pixmap>)>,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer").finish_non_exhaustive()
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static THREAD_RENDERER: RefCell<Renderer> = RefCell::new(Renderer::new());
}

/// Renders with a per-thread [`Renderer`] (convenient for one-off exports).
pub fn render(doc: &Document, opts: &RenderOptions) -> Frame {
    THREAD_RENDERER.with(|r| r.borrow_mut().render(doc, opts))
}

/// Runs `f` with the per-thread text engine (for measuring text without a session).
pub fn with_text_engine<R>(f: impl FnOnce(&mut TextEngine) -> R) -> R {
    THREAD_RENDERER.with(|r| f(&mut r.borrow_mut().text))
}

struct Ctx {
    scale: f32,
    work: Rect,
    /// Canvas offset of the image origin in output pixels (padding × scale).
    off: (f32, f32),
    view: Transform,
}

/// Keep the unrounded global affine transform separate from a target layer's
/// integer origin. Canonical stroke chunks must not infer it from a local matrix.
#[derive(Clone, Copy)]
struct DrawTransform {
    global: Transform,
    origin: (i32, i32),
}
impl DrawTransform {
    fn local(self) -> Transform {
        self.global.post_translate(-(self.origin.0 as f32), -(self.origin.1 as f32))
    }
    fn pre_concat(self, transform: Transform) -> Self {
        Self { global: self.global.pre_concat(transform), ..self }
    }
    fn pre_translate(self, x: f32, y: f32) -> Self {
        Self { global: self.global.pre_translate(x, y), ..self }
    }
    fn pre_scale(self, x: f32, y: f32) -> Self {
        Self { global: self.global.pre_scale(x, y), ..self }
    }
    fn at_origin(self, x: i32, y: i32) -> Self { Self { origin: (x, y), ..self } }
}

impl Ctx {
    fn draw_transform(&self) -> DrawTransform {
        DrawTransform {
            global: Transform::from_row(self.scale, 0.0, 0.0, self.scale, self.off.0, self.off.1),
            origin: (self.work.x, self.work.y),
        }
    }
    /// Image-space rectangle → work-pixmap pixels, rounded outward.
    fn dev(&self, r: RectF) -> Rect {
        RectF::new(
            r.x * self.scale + self.off.0 - self.work.x as f32,
            r.y * self.scale + self.off.1 - self.work.y as f32,
            r.w * self.scale,
            r.h * self.scale,
        )
        .to_outer_rect()
    }

    fn pt(&self, p: PointF) -> PointF {
        PointF::new(
            p.x * self.scale + self.off.0 - self.work.x as f32,
            p.y * self.scale + self.off.1 - self.work.y as f32,
        )
    }
}

fn solid(color: Color) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(color.to_skia()),
        blend_mode: SkBlend::SourceOver,
        anti_alias: true,
        force_hq_pipeline: false,
    }
}

fn fill_paint(fill: &Fill, bounds: RectF) -> Option<Paint<'static>> {
    match *fill {
        Fill::None => None,
        Fill::Solid { color } => (!color.is_transparent()).then(|| solid(color)),
        Fill::Gradient { from, to, angle } => {
            let c = bounds.center();
            let a = angle.to_radians();
            let (dx, dy) = (a.cos(), a.sin());
            let half = f32::midpoint(bounds.w * dx.abs(), bounds.h * dy.abs());
            let shader = LinearGradient::new(
                tiny_skia::Point::from_xy(c.x - dx * half, c.y - dy * half),
                tiny_skia::Point::from_xy(c.x + dx * half, c.y + dy * half),
                vec![GradientStop::new(0.0, from.to_skia()), GradientStop::new(1.0, to.to_skia())],
                SpreadMode::Pad,
                Transform::identity(),
            )?;
            Some(Paint {
                shader,
                blend_mode: SkBlend::SourceOver,
                anti_alias: true,
                force_hq_pipeline: false,
            })
        }
    }
}

fn stroke_of(style: &Style) -> Stroke {
    let sw = style.stroke_width.max(0.0);
    let dash = style
        .dash
        .pattern()
        .and_then(|p| StrokeDash::new(p.iter().map(|v| (v * sw).max(0.01)).collect(), 0.0));
    Stroke {
        width: sw,
        miter_limit: 4.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        dash,
    }
}

fn sk_blend(b: BlendMode) -> SkBlend {
    match b {
        BlendMode::Normal => SkBlend::SourceOver,
        BlendMode::Multiply => SkBlend::Multiply,
        BlendMode::Screen => SkBlend::Screen,
        BlendMode::Overlay => SkBlend::Overlay,
        BlendMode::Darken => SkBlend::Darken,
        BlendMode::Lighten => SkBlend::Lighten,
    }
}

fn premul(c: u8, a: u8) -> u8 {
    ((u32::from(c) * u32::from(a) + 127) / 255) as u8
}

fn unpremul(c: u8, a: u8) -> u8 {
    if a == 0 || a == 255 {
        c
    } else {
        ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
    }
}

fn frame_to_pixmap(f: &Frame) -> Option<Pixmap> {
    let size = tiny_skia::IntSize::from_wh(f.width(), f.height())?;
    let w4 = f.width() as usize * 4;
    let mut data = vec![0u8; w4 * f.height() as usize];
    data.par_chunks_mut(w4).enumerate().for_each(|(y, row)| {
        for (d, s) in row.chunks_exact_mut(4).zip(f.row(y as u32).chunks_exact(4)) {
            let a = s[3];
            d[0] = premul(s[0], a);
            d[1] = premul(s[1], a);
            d[2] = premul(s[2], a);
            d[3] = a;
        }
    });
    Pixmap::from_vec(data, size)
}

/// Copies `r` (inside the pixmap) out as a straight-alpha frame.
fn pixmap_region_to_frame(pm: &Pixmap, r: Rect) -> Frame {
    let w4 = r.width as usize * 4;
    let mut data = vec![0u8; w4 * r.height as usize];
    let stride = pm.width() as usize * 4;
    let src = pm.data();
    if w4 > 0 {
        data.par_chunks_mut(w4).enumerate().for_each(|(y, row)| {
            let o = (r.y as usize + y) * stride + r.x as usize * 4;
            for (d, s) in row.chunks_exact_mut(4).zip(src[o..o + w4].chunks_exact(4)) {
                let a = s[3];
                d[0] = unpremul(s[0], a);
                d[1] = unpremul(s[1], a);
                d[2] = unpremul(s[2], a);
                d[3] = a;
            }
        });
    }
    Frame::from_rgba8(r.width, r.height, data).expect("exact-size buffer")
}

/// Writes a straight-alpha frame back into the pixmap at `r`.
fn frame_to_pixmap_region(pm: &mut Pixmap, r: Rect, f: &Frame) {
    let stride = pm.width() as usize * 4;
    let w4 = r.width as usize * 4;
    if w4 == 0 {
        return;
    }
    let dst = pm.data_mut();
    for y in 0..r.height as usize {
        let o = (r.y as usize + y) * stride + r.x as usize * 4;
        for (d, s) in dst[o..o + w4].chunks_exact_mut(4).zip(f.row(y as u32).chunks_exact(4)) {
            let a = s[3];
            d[0] = premul(s[0], a);
            d[1] = premul(s[1], a);
            d[2] = premul(s[2], a);
            d[3] = a;
        }
    }
}

fn pm_bounds(pm: &Pixmap) -> Rect {
    Rect::new(0, 0, pm.width(), pm.height())
}

impl Renderer {
    /// Creates a renderer with a fresh text engine.
    pub fn new() -> Self {
        Self { text: TextEngine::new(), base: None, images: Vec::new() }
    }

    /// The text engine (for layout queries that must match what is painted).
    pub fn text_mut(&mut self) -> &mut TextEngine {
        &mut self.text
    }

    /// Bound interactive effect workspaces without changing document geometry,
    /// logical zoom, or export rendering. Individual effect layers are clipped
    /// to this workspace; source caches and blur scratch are additional memory.
    pub fn plan_preview_raster(
        doc: &Document,
        scale: f32,
        viewport: Rect,
        pixel_budget: u64,
    ) -> PreviewRasterPlan {
        let scale = if scale.is_finite() && scale > 0. { scale.min(64.) } else { 1. };
        // A viewport straddling the global origin needs a 2×2 integer cover.
        let budget = pixel_budget.max(4);
        let plan = |density: f32| {
            let d = f64::from(density);
            let left = (f64::from(viewport.x) * d).floor() as i64;
            let top = (f64::from(viewport.y) * d).floor() as i64;
            let right = ((f64::from(viewport.x) + f64::from(viewport.width)) * d).ceil() as i64;
            let bottom = ((f64::from(viewport.y) + f64::from(viewport.height)) * d).ceil() as i64;
            let viewport = Rect::new(left as i32, top as i32,
                (right - left).clamp(1, i64::from(u32::MAX)) as u32,
                (bottom - top).clamp(1, i64::from(u32::MAX)) as u32);
            let raster_scale = scale * density;
            let (cw, ch) = doc.canvas_size();
            let full = Rect::new(0, 0,
                ((cw as f32 * raster_scale).round() as u32).max(1),
                ((ch as f32 * raster_scale).round() as u32).max(1));
            let work = viewport.intersect(full)
                .map(|visible| Self::work_rect(doc, raster_scale, visible, full));
            let work_pixels = work.map_or(0, |work| u64::from(work.width) * u64::from(work.height))
                .max(u64::from(viewport.width) * u64::from(viewport.height));
            PreviewRasterPlan { density, viewport, work_pixels }
        };
        let mut density = 1.;
        for _ in 0..64 {
            let preview = plan(density);
            if preview.work_pixels <= budget { return preview; }
            // Margin handles outward integer rounding and effect-bound transitions.
            let factor = ((budget as f64 / preview.work_pixels as f64).sqrt() * 0.95).clamp(0.05, 0.9);
            density *= factor as f32;
        }
        // Even maximal u32 canvases fit in one pixel at this density. Never turn
        // an extreme imported document into an unbounded interactive allocation.
        plan(1e-12)
    }

    /// Renders the document. See [`RenderOptions`].
    pub fn render(&mut self, doc: &Document, opts: &RenderOptions) -> Frame {
        let scale =
            if opts.scale.is_finite() && opts.scale > 0.0 { opts.scale.min(64.0) } else { 1.0 };
        let (cw, ch) = doc.canvas_size();
        let fw = if cw == 0 { 0 } else { ((cw as f32 * scale).round() as u32).max(1) };
        let fh = if ch == 0 { 0 } else { ((ch as f32 * scale).round() as u32).max(1) };
        let full = Rect::new(0, 0, fw, fh);
        let out = opts.viewport.unwrap_or(full);
        let Some(vis) = out.intersect(full) else {
            return image_effects::solid_frame(out.width, out.height, [0; 4]);
        };
        let work = Self::work_rect(doc, scale, vis, full);
        let Some(pm) = self.paint(doc, scale, work, opts) else {
            return image_effects::solid_frame(out.width, out.height, [0; 4]);
        };
        // Copy the visible part into the result.
        let inner = Rect::new(vis.x - work.x, vis.y - work.y, vis.width, vis.height);
        let frame = pixmap_region_to_frame(&pm, inner);
        if out == vis {
            return frame;
        }
        let mut result = image_effects::solid_frame(out.width, out.height, [0; 4]);
        let (dx, dy) = ((vis.x - out.x) as usize * 4, (vis.y - out.y) as usize);
        let w4 = vis.width as usize * 4;
        let out_stride = result.stride();
        result
            .data_mut()
            .par_chunks_mut(out_stride)
            .enumerate()
            .skip(dy)
            .take(vis.height as usize)
            .for_each(|(y, row)| {
                row[dx..dx + w4].copy_from_slice(&frame.row((y - dy) as u32)[..w4]);
            });
        result
    }

    /// The rectangle (output pixels) that must be painted to render `vis` correctly.
    fn work_rect(doc: &Document, scale: f32, vis: Rect, full: Rect) -> Rect {
        let (ox, oy) = doc.canvas_offset();
        let dev = |r: RectF| -> Rect {
            RectF::new(
                (r.x + ox as f32) * scale,
                (r.y + oy as f32) * scale,
                r.w * scale,
                r.h * scale,
            )
            .to_outer_rect()
        };
        let mut work = vis;
        for _ in 0..8 {
            let mut next = work;
            for o in doc.objects().iter().filter(|o| o.visible) {
                let needs: Vec<RectF> = match &o.kind {
                    ObjectKind::Blur(_) | ObjectKind::Pixelate(_) => vec![o.bounds().inflate(1.0)],
                    ObjectKind::Magnify(m) => {
                        // Its shadow is rasterized into an object-sized layer before
                        // blurring. Clipping that layer to a tile changes the filter.
                        vec![o.render_bounds(), m.source_rect().inflate(2.0)]
                    }
                    _ if o.style.shadow.is_some() => vec![o.render_bounds()],
                    _ => continue,
                };
                if dev(needs[0]).intersect(work).is_some() {
                    for n in needs {
                        next = next.union(dev(n));
                    }
                }
            }
            let next = next.intersect(full).unwrap_or(work);
            if next == work {
                break;
            }
            work = next;
        }
        work
    }

    fn base_pixmap(&mut self, doc: &Document) -> Option<(Arc<Pixmap>, bool)> {
        if let Some(c) = &self.base
            && Arc::ptr_eq(&c.frame, doc.base())
            && c.rev == doc.base_revision()
        {
            return Some((c.pixmap.clone(), c.opaque));
        }
        let f = doc.base();
        let pm = Arc::new(frame_to_pixmap(f)?);
        let opaque =
            (0..f.height()).into_par_iter().all(|y| f.row(y).chunks_exact(4).all(|p| p[3] == 255));
        self.base = Some(BaseCache {
            frame: f.clone(),
            rev: doc.base_revision(),
            pixmap: pm.clone(),
            opaque,
        });
        Some((pm, opaque))
    }

    fn image_pixmap(&mut self, f: &Arc<Frame>) -> Option<Arc<Pixmap>> {
        if let Some((_, p)) = self.images.iter().find(|(k, _)| Arc::ptr_eq(k, f)) {
            return Some(p.clone());
        }
        let p = Arc::new(frame_to_pixmap(f)?);
        if self.images.len() >= 32 {
            self.images.remove(0);
        }
        self.images.push((f.clone(), p.clone()));
        Some(p)
    }

    fn paint(
        &mut self,
        doc: &Document,
        scale: f32,
        work: Rect,
        opts: &RenderOptions,
    ) -> Option<Pixmap> {
        let mut pm = Pixmap::new(work.width, work.height)?;
        let (ox, oy) = doc.canvas_offset();
        let off = (ox as f32 * scale, oy as f32 * scale);
        let ctx = Ctx {
            scale,
            work,
            off,
            view: Transform::from_row(
                scale,
                0.0,
                0.0,
                scale,
                off.0 - work.x as f32,
                off.1 - work.y as f32,
            ),
        };
        self.draw_base(doc, &ctx, &mut pm, opts);
        let mut spotlights_done = false;
        for o in doc.objects().iter().filter(|o| o.visible) {
            self.draw_object(doc, &ctx, &mut pm, o, &mut spotlights_done);
        }
        if opts.include_guides {
            for o in doc.objects().iter().filter(|o| o.visible) {
                draw_guide(&ctx, &mut pm, o.bounds());
            }
        }
        Some(pm)
    }

    #[allow(clippy::float_cmp)] // scale 1.0 is the sentinel for the pixel-exact export path
    fn draw_base(&mut self, doc: &Document, ctx: &Ctx, pm: &mut Pixmap, opts: &RenderOptions) {
        let (cw, ch) = doc.canvas_size();
        let s = ctx.scale;
        // Canvas background over the whole canvas.
        let canvas =
            RectF::new(-(ctx.work.x as f32), -(ctx.work.y as f32), cw as f32 * s, ch as f32 * s);
        if let (Some(paint), Some(r)) =
            (fill_paint(&doc.canvas().background, canvas), shapes::sk_rect(canvas))
        {
            let mut paint = paint;
            paint.anti_alias = false;
            fill_rect_clamped(pm, r, &paint);
        }
        let Some((base, opaque)) = self.base_pixmap(doc) else { return };
        let (w, h) = doc.image_size();
        // Fast path: pixel-exact 1× export of an opaque, unpadded image.
        if s == 1.0
            && opaque
            && ctx.work.intersect(Rect::new(0, 0, w, h)) == Some(ctx.work)
            && doc.canvas().padding == crate::doc::Padding::default()
        {
            let stride = w as usize * 4;
            let row_bytes = ctx.work.width as usize * 4;
            for (y, row) in pm.data_mut().chunks_exact_mut(row_bytes).enumerate() {
                let offset = (ctx.work.y as usize + y) * stride + ctx.work.x as usize * 4;
                row.copy_from_slice(&base.data()[offset..offset + row_bytes]);
            }
            return;
        }
        let quality = match opts.base_filter {
            BaseFilter::Nearest => FilterQuality::Nearest,
            BaseFilter::Bilinear => FilterQuality::Bilinear,
            BaseFilter::Bicubic => FilterQuality::Bicubic,
            BaseFilter::Auto => {
                if s == 1.0 || s >= 2.0 {
                    FilterQuality::Nearest
                } else {
                    FilterQuality::Bilinear
                }
            }
        };
        if matches!(quality, FilterQuality::Nearest | FilterQuality::Bilinear) {
            draw_base_global_phase(&base, ctx, pm, quality);
            return;
        }
        let rect = RectF::new(
            ctx.off.0 - ctx.work.x as f32,
            ctx.off.1 - ctx.work.y as f32,
            w as f32 * s,
            h as f32 * s,
        );
        let Some(r) = shapes::sk_rect(rect) else { return };
        let paint = Paint {
            shader: Pattern::new(
                Pixmap::as_ref(&base),
                SpreadMode::Pad,
                quality,
                1.0,
                Transform::from_row(s, 0.0, 0.0, s, rect.x, rect.y),
            ),
            blend_mode: SkBlend::SourceOver,
            anti_alias: false,
            force_hq_pipeline: false,
        };
        fill_rect_clamped(pm, r, &paint);
    }

    fn draw_object(
        &mut self,
        doc: &Document,
        ctx: &Ctx,
        pm: &mut Pixmap,
        o: &Object,
        spotlights_done: &mut bool,
    ) {
        match &o.kind {
            ObjectKind::Blur(e) => return Self::effect_blur(ctx, pm, e),
            ObjectKind::Pixelate(e) => return Self::effect_pixelate(ctx, pm, e),
            ObjectKind::Magnify(_) => return Self::effect_magnify(ctx, pm, o),
            ObjectKind::Spotlight(_) => {
                if !*spotlights_done {
                    *spotlights_done = true;
                    Self::effect_spotlights(doc, ctx, pm);
                }
                return;
            }
            ObjectKind::Unknown(_) => return,
            _ => {}
        }
        let st = &o.style;
        let layered = st.opacity < 1.0 || st.shadow.is_some() || st.blend != BlendMode::Normal;
        if !layered {
            self.paint_content(doc, ctx.draw_transform(), pm, o);
            return;
        }
        let Some(bbox) = ctx.dev(o.render_bounds()).intersect(pm_bounds(pm)) else { return };
        let Some(mut layer) = Pixmap::new(bbox.width, bbox.height) else { return };
        let tf = ctx.draw_transform().at_origin(ctx.work.x + bbox.x, ctx.work.y + bbox.y);
        self.paint_content(doc, tf, &mut layer, o);
        composite_layer(pm, &layer, bbox, st, ctx.scale);
    }

    /// Paints one object's own pixels (no opacity/shadow/blend handling).
    fn paint_content(&mut self, doc: &Document, tf: DrawTransform, pm: &mut Pixmap, o: &Object) {
        let st = &o.style;
        match &o.kind {
            ObjectKind::Rectangle(b) => {
                let t = rot_transform(tf, b.rect, b.rotation);
                if let Some(p) = shapes::rounded_rect(b.rect, st.corner_radius) {
                    fill_and_stroke(pm, &p, t, st, b.rect);
                }
            }
            ObjectKind::Ellipse(b) => {
                let t = rot_transform(tf, b.rect, b.rotation);
                if let Some(p) = shapes::ellipse(b.rect) {
                    fill_and_stroke(pm, &p, t, st, b.rect);
                }
            }
            ObjectKind::Line(l) => {
                let mut pb = PathBuilder::new();
                pb.move_to(l.a.x, l.a.y);
                pb.line_to(l.b.x, l.b.y);
                if let Some(p) = pb.finish() {
                    stroke_only(pm, &p, tf, st, st.stroke);
                }
                if l.a == l.b {
                    dot(pm, l.a, st, tf);
                }
            }
            ObjectKind::Arrow(a) => draw_arrow(pm, tf, st, a.a, a.b, &a.heads),
            ObjectKind::Freehand(f) => {
                draw_freehand(pm, tf, st, &f.points, f.smooth, f.arrow.as_ref());
            }
            ObjectKind::Text(t) => {
                let c = &t.content;
                let pad = c.padding.max(0.0);
                let wrap = (!t.auto_width).then(|| (t.rect.w - 2.0 * pad).max(1.0));
                let layout = self.text.layout_content(c, wrap);
                let box_w = wrap.unwrap_or(layout.width) + 2.0 * pad;
                let box_h = layout.height + 2.0 * pad;
                let rect = RectF::new(t.rect.x, t.rect.y, box_w, box_h);
                let base = rot_transform(tf, t.rect, t.rotation);
                self.draw_text_box(pm, base, rect, st, c, &layout);
            }
            ObjectKind::Balloon(b) => {
                if let Some(p) = shapes::balloon_outline(b, st.corner_radius) {
                    fill_and_stroke(
                        pm,
                        &p,
                        tf,
                        st,
                        b.rect.union(&RectF::new(b.tail.x, b.tail.y, 0.0, 0.0)),
                    );
                }
                let c = &b.content;
                let pad = c.padding.max(0.0);
                let wrap = (b.rect.w - 2.0 * pad).max(1.0);
                let layout = self.text.layout_content(c, Some(wrap));
                let origin = crate::text::balloon_text_origin(b, layout.height);
                self.draw_glyphs(pm, tf, origin, c, &layout);
            }
            ObjectKind::Step(s) => {
                let r = RectF::from_center_size(s.center, s.diameter, s.diameter);
                if let Some(p) = shapes::ellipse(r) {
                    fill_and_stroke(pm, &p, tf, st, r);
                }
                let n = doc.step_number(o.id).unwrap_or(1);
                let content = TextContent {
                    text: n.to_string(),
                    font: crate::object::FontSpec {
                        size: s.diameter * 0.52,
                        bold: true,
                        ..Default::default()
                    },
                    color: s.text_color,
                    ..TextContent::default()
                };
                let layout = self.text.layout_content(&content, None);
                let origin =
                    PointF::new(s.center.x - layout.width / 2.0, s.center.y - layout.height / 2.0);
                self.draw_glyphs(pm, tf, origin, &content, &layout);
            }
            ObjectKind::Highlight(h) => {
                let colour = st.solid_fill().unwrap_or(st.stroke);
                if h.points.is_empty() {
                    if let Some(r) = shapes::sk_rect(h.rect) {
                        pm.fill_rect(r, &solid(colour), tf.local(), None);
                    }
                } else if let Some(p) = shapes::freehand_path(&h.points, true) {
                    let stroke = Stroke {
                        width: st.stroke_width.max(1.0),
                        line_cap: LineCap::Square,
                        line_join: LineJoin::Round,
                        ..Stroke::default()
                    };
                    canonical_stroke(pm, &p, &stroke, tf, colour);
                }
            }
            ObjectKind::Image(i) => {
                if let Some(img) = self.image_pixmap(&i.image.0) {
                    draw_bitmap(pm, tf, i.rect, i.rotation, &img);
                }
            }
            ObjectKind::Sticker(s) => match &s.source {
                StickerSource::Builtin { which } => {
                    draw_builtin(pm, tf, st, s.rect, s.rotation, *which);
                }
                StickerSource::Bitmap { image } => {
                    if let Some(img) = self.image_pixmap(&image.0) {
                        draw_bitmap(pm, tf, s.rect, s.rotation, &img);
                    }
                }
                StickerSource::Glyph { text } => {
                    let colour = st.solid_fill().unwrap_or(st.stroke);
                    let content = TextContent {
                        text: text.clone(),
                        font: crate::object::FontSpec {
                            size: s.rect.w.min(s.rect.h).max(1.0) * 0.8,
                            ..Default::default()
                        },
                        color: colour,
                        ..TextContent::default()
                    };
                    let layout = self.text.layout_content(&content, None);
                    let t = rot_transform(tf, s.rect, s.rotation);
                    let origin = PointF::new(
                        s.rect.x + (s.rect.w - layout.width) / 2.0,
                        s.rect.y + (s.rect.h - layout.height) / 2.0,
                    );
                    self.draw_glyphs(pm, t, origin, &content, &layout);
                }
            },
            ObjectKind::Cursor(c) => {
                let size = crate::object::CursorShape::BASE_SIZE * c.scale;
                let (fill, stroke) = shapes::cursor_paths(c.kind, c.pos, size);
                if let (Some(p), Some(colour)) = (fill, st.solid_fill()) {
                    canonical_fill(pm, &p, &solid(colour), FillRule::Winding, tf);
                }
                if let Some(p) = stroke {
                    let mut s2 = st.clone();
                    s2.stroke_width = st.stroke_width.max(1.0) * c.scale.max(0.25);
                    stroke_only(pm, &p, tf, &s2, st.stroke);
                }
            }
            ObjectKind::Grid(g) => draw_grid(pm, tf, st, g.rect, g.pattern, g.spacing),
            _ => {}
        }
    }

    fn draw_text_box(
        &mut self,
        pm: &mut Pixmap,
        tf: DrawTransform,
        rect: RectF,
        st: &Style,
        c: &TextContent,
        layout: &crate::text::TextLayout,
    ) {
        if let Some(bg) = c.background.filter(|b| !b.is_transparent()) {
            if let Some(p) = shapes::rounded_rect(rect, st.corner_radius) {
                canonical_fill(pm, &p, &solid(bg), FillRule::Winding, tf);
            }
        } else if let Some(paint) = fill_paint(&st.fill, rect)
            && let Some(p) = shapes::rounded_rect(rect, st.corner_radius)
        {
            canonical_fill(pm, &p, &paint, FillRule::Winding, tf);
        }
        if st.has_stroke()
            && let Some(p) = shapes::rounded_rect(rect, st.corner_radius)
        {
            stroke_only(pm, &p, tf, st, st.stroke);
        }
        let pad = c.padding.max(0.0);
        self.draw_glyphs(pm, tf, PointF::new(rect.x + pad, rect.y + pad), c, layout);
    }

    fn draw_glyphs(
        &mut self,
        pm: &mut Pixmap,
        tf: DrawTransform,
        origin: PointF,
        c: &TextContent,
        layout: &crate::text::TextLayout,
    ) {
        let Some(path) = self.text.text_path(layout) else { return };
        let t = tf.pre_translate(origin.x, origin.y);
        if let Some(o) = c.outline.filter(|o| o.width > 0.0 && !o.color.is_transparent()) {
            let stroke = Stroke {
                width: o.width * 2.0,
                line_join: LineJoin::Round,
                line_cap: LineCap::Round,
                ..Stroke::default()
            };
            canonical_stroke(pm, &path, &stroke, t, o.color);
        }
        if !c.color.is_transparent() {
            canonical_fill(pm, &path, &solid(c.color), FillRule::Winding, t);
        }
    }

    // -------------------------------------------------------------------------------
    // Backdrop effects
    // -------------------------------------------------------------------------------

    fn effect_blur(ctx: &Ctx, pm: &mut Pixmap, e: &EffectBox) {
        let Some(r) = ctx.dev(e.rect.normalized()).intersect(pm_bounds(pm)) else { return };
        let sigma = e.amount * ctx.scale;
        if sigma <= 0.0 {
            return;
        }
        let stride = pm.width() as usize * 4;
        let w4 = r.width as usize * 4;
        let mut buf = vec![0u8; w4 * r.height as usize];
        for y in 0..r.height as usize {
            let o = (r.y as usize + y) * stride + r.x as usize * 4;
            buf[y * w4..(y + 1) * w4].copy_from_slice(&pm.data()[o..o + w4]);
        }
        image_effects::gaussian_blur_premultiplied(
            &mut buf,
            r.width as usize,
            r.height as usize,
            sigma,
            BlurMethod::Auto,
        );
        for y in 0..r.height as usize {
            let o = (r.y as usize + y) * stride + r.x as usize * 4;
            pm.data_mut()[o..o + w4].copy_from_slice(&buf[y * w4..(y + 1) * w4]);
        }
    }

    fn effect_pixelate(ctx: &Ctx, pm: &mut Pixmap, e: &EffectBox) {
        let Some(r) = ctx.dev(e.rect.normalized()).intersect(pm_bounds(pm)) else { return };
        let block = (e.amount * ctx.scale).round().max(1.0) as u32;
        let mut f = pixmap_region_to_frame(pm, r);
        if image_effects::pixelate(&mut f, None, block).is_ok() {
            frame_to_pixmap_region(pm, r, &f);
        }
    }

    fn effect_magnify(ctx: &Ctx, pm: &mut Pixmap, o: &Object) {
        let ObjectKind::Magnify(m) = &o.kind else { return };
        let st = &o.style;
        let lens_dr = ctx.dev(m.rect.normalized());
        let bounds = pm_bounds(pm);
        let shape_rect = m.rect.normalized();
        // Shadow of the lens silhouette, beneath the lens.
        if let Some(sh) = st.shadow
            && let Some(bbox) = ctx.dev(o.render_bounds()).intersect(bounds)
            && let Some(mut layer) = Pixmap::new(bbox.width, bbox.height)
        {
            let tf = ctx.view.post_translate(-(bbox.x as f32), -(bbox.y as f32));
            let p = if m.circular {
                shapes::ellipse(shape_rect)
            } else {
                shapes::rounded_rect(shape_rect, 0.0)
            };
            if let Some(p) = p {
                layer.fill_path(&p, &solid(Color::BLACK), FillRule::Winding, tf, None);
            }
            draw_shadow(pm, &layer, bbox, &sh, ctx.scale, st.opacity);
        }
        let (Some(dst_r), Some(src_r)) =
            (lens_dr.intersect(bounds), ctx.dev(m.source_rect().inflate(2.0)).intersect(bounds))
        else {
            return;
        };
        let src = pixmap_region_to_frame(pm, src_r);
        let mut dst = pixmap_region_to_frame(pm, dst_r);
        let c = ctx.pt(m.source);
        let lens = Lens {
            dst: Rect::new(lens_dr.x - dst_r.x, lens_dr.y - dst_r.y, lens_dr.width, lens_dr.height),
            shape: if m.circular { LensShape::Ellipse } else { LensShape::Rect },
            source_center: image_effects::PointF::new(c.x - src_r.x as f32, c.y - src_r.y as f32),
            zoom: m.zoom,
        };
        if image_effects::magnify(&src, &mut dst, &lens).is_ok() {
            frame_to_pixmap_region(pm, dst_r, &dst);
        }
        // Border.
        if st.has_stroke() {
            let p = if m.circular {
                shapes::ellipse(shape_rect)
            } else {
                shapes::rounded_rect(shape_rect, 0.0)
            };
            if let Some(p) = p {
                stroke_only(pm, &p, ctx.draw_transform(), st, st.stroke);
            }
        }
    }

    /// All visible spotlights together: dim everything except the union of their shapes.
    fn effect_spotlights(doc: &Document, ctx: &Ctx, pm: &mut Pixmap) {
        let spots: Vec<&crate::object::SpotlightShape> = doc
            .objects()
            .iter()
            .filter(|o| o.visible)
            .filter_map(|o| match &o.kind {
                ObjectKind::Spotlight(s) => Some(s),
                _ => None,
            })
            .collect();
        let Some(first) = spots.first() else { return };
        let Some(mut mask) = Pixmap::new(pm.width(), pm.height()) else { return };
        for s in &spots {
            let p =
                if s.ellipse { shapes::ellipse(s.rect) } else { shapes::rounded_rect(s.rect, 0.0) };
            if let Some(p) = p {
                mask.fill_path(&p, &solid(Color::WHITE), FillRule::Winding, ctx.view, None);
            }
        }
        let feather = first.feather * ctx.scale;
        if feather > 0.0 {
            let (w, h) = (mask.width() as usize, mask.height() as usize);
            image_effects::gaussian_blur_premultiplied(
                mask.data_mut(),
                w,
                h,
                feather,
                BlurMethod::Auto,
            );
        }
        let dim = first.dim;
        let da = f32::from(dim.a) / 255.0;
        let dc = [f32::from(dim.r), f32::from(dim.g), f32::from(dim.b)];
        pm.data_mut().par_chunks_mut(4096).zip(mask.data().par_chunks(4096)).for_each(
            |(px, mk)| {
                for (p, m) in px.chunks_exact_mut(4).zip(mk.chunks_exact(4)) {
                    let k = da * (1.0 - f32::from(m[3]) / 255.0);
                    if k <= 0.0 {
                        continue;
                    }
                    for c in 0..3 {
                        p[c] = (f32::from(p[c]) * (1.0 - k) + dc[c] * k).round().clamp(0.0, 255.0)
                            as u8;
                    }
                    p[3] =
                        (f32::from(p[3]) * (1.0 - k) + 255.0 * k).round().clamp(0.0, 255.0) as u8;
                }
            },
        );
    }
}

/// Sample in global output-pixel coordinates. Inverting a translated f32 Pattern
/// transform separately for every viewport changes interpolation rounding at tile
/// boundaries. This path keeps one phase for full renders, tiles and panned views,
/// without allocating a second full-sized resampled desktop image.
fn draw_base_global_phase(base: &Pixmap, ctx: &Ctx, pm: &mut Pixmap, quality: FilterQuality) {
    let scale = f64::from(ctx.scale);
    let (off_x, off_y) = (f64::from(ctx.off.0), f64::from(ctx.off.1));
    let bilinear = quality == FilterQuality::Bilinear;
    let axis = |global: f64, offset: f64, extent: u32| {
        let source = (global + 0.5 - offset) / scale - if bilinear { 0.5 } else { 0.0 };
        let lower = source.floor();
        let fraction = if bilinear { source - lower } else { 0.0 };
        let max = f64::from(extent.saturating_sub(1));
        (lower.clamp(0.0, max) as usize, (lower + 1.0).clamp(0.0, max) as usize, fraction)
    };
    let columns = (0..pm.width()).map(|x| {
        let global = f64::from(ctx.work.x) + f64::from(x);
        let center = global + 0.5;
        if center < off_x || center >= off_x + f64::from(base.width()) * scale {
            None
        } else { Some(axis(global, off_x, base.width())) }
    }).collect::<Vec<_>>();
    let row_bytes = pm.width() as usize * 4;
    let source_stride = base.width() as usize * 4;
    pm.data_mut().par_chunks_mut(row_bytes).enumerate().for_each(|(y, row)| {
        let global = f64::from(ctx.work.y) + y as f64;
        let center = global + 0.5;
        if center < off_y || center >= off_y + f64::from(base.height()) * scale { return; }
        let (y0, y1, fy) = axis(global, off_y, base.height());
        for (pixel, column) in row.chunks_exact_mut(4).zip(&columns) {
            let Some((x0, x1, fx)) = *column else { continue; };
            let (a, b, c, d) = (
                y0 * source_stride + x0 * 4, y0 * source_stride + x1 * 4,
                y1 * source_stride + x0 * 4, y1 * source_stride + x1 * 4,
            );
            let mut sample = [0.0; 4];
            for channel in 0..4 {
                let top = f64::from(base.data()[a + channel]) * (1.0 - fx)
                    + f64::from(base.data()[b + channel]) * fx;
                let bottom = f64::from(base.data()[c + channel]) * (1.0 - fx)
                    + f64::from(base.data()[d + channel]) * fx;
                sample[channel] = top * (1.0 - fy) + bottom * fy;
            }
            let inverse_alpha = 1.0 - sample[3] / 255.0;
            for channel in 0..4 {
                pixel[channel] = (sample[channel] + f64::from(pixel[channel]) * inverse_alpha)
                    .round().clamp(0.0, 255.0) as u8;
            }
        }
    });
}

#[cfg(test)]
mod viewport_phase_tests {
    use super::*;

    #[test]
    fn nearest_and_bilinear_base_sampling_have_identical_global_viewport_phase() {
        let mut bytes = Vec::new();
        for y in 0..36u32 {
            for x in 0..1400u32 {
                bytes.extend_from_slice(&[(x % 251) as u8, (y * 6) as u8,
                    ((x * 3 + y) % 233) as u8, (80 + x % 176) as u8]);
            }
        }
        let mut doc = Document::new(Frame::from_rgba8(1400, 36, bytes).unwrap()).unwrap();
        doc.set_padding(crate::doc::Padding::uniform(3));
        let mut renderer = Renderer::new();
        for scale in [0.65, 1.25, 2.5] {
            for base_filter in [BaseFilter::Nearest, BaseFilter::Bilinear, BaseFilter::Auto] {
                let options = RenderOptions { scale, base_filter, ..Default::default() };
                let full = renderer.render(&doc, &options);
                for x in [7, 519, 857] {
                    let viewport = Rect::new(x, 5, 513.min(full.width() - x as u32), 13);
                    let actual = renderer.render(&doc, &RenderOptions {
                        viewport: Some(viewport), ..options.clone()
                    });
                    let expected = full.crop(viewport).unwrap();
                    assert_eq!(actual.data().iter().zip(expected.data()).position(|(a, b)| a != b),
                        None, "Base phase differs for {base_filter:?}, scale {scale}, viewport {viewport:?}");
                }
            }
        }
    }
}

/// `fill_rect` with the rectangle clipped to the pixmap first. tiny-skia's non-AA rectangle
/// fill covers one column too many when the rectangle starts left of the pixmap (negative x),
/// which would smear the base image's edge pixel into the padding in viewport renders.
fn fill_rect_clamped(pm: &mut Pixmap, r: tiny_skia::Rect, paint: &Paint) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let (l, t, rt, b) = (r.left().max(0.0), r.top().max(0.0), r.right().min(w), r.bottom().min(h));
    if let Some(c) = tiny_skia::Rect::from_ltrb(l, t, rt, b) {
        pm.fill_rect(c, paint, Transform::identity(), None);
    }
}

fn rot_transform(tf: DrawTransform, rect: RectF, rotation: f32) -> DrawTransform {
    if rotation == 0.0 {
        tf
    } else {
        let c = rect.center();
        tf.pre_concat(Transform::from_rotate_at(rotation.to_degrees(), c.x, c.y))
    }
}

fn fill_and_stroke(pm: &mut Pixmap, path: &Path, tf: DrawTransform, st: &Style, bounds: RectF) {
    if let Some(paint) = fill_paint(&st.fill, bounds) {
        canonical_fill(pm, path, &paint, FillRule::Winding, tf);
    }
    if st.has_stroke() {
        stroke_only(pm, path, tf, st, st.stroke);
    }
}

fn stroke_only(pm: &mut Pixmap, path: &Path, tf: DrawTransform, st: &Style, colour: Color) {
    if st.stroke_width <= 0.0 || colour.is_transparent() {
        return;
    }
    canonical_stroke(pm, path, &stroke_of(st), tf, colour);
}

thread_local! {
    static STROKE_SCRATCH: RefCell<Option<(Pixmap, Option<Rect>)>> = const { RefCell::new(None) };
}

/// Use the same clipped edge builder and shader origin for each global chunk,
/// including glyph fills whose edges can cross an interactive viewport boundary.
fn canonical_fill(pm: &mut Pixmap, path: &Path, paint: &Paint, rule: FillRule, tf: DrawTransform) {
    const CHUNK: i32 = 512;
    let Some(global_path) = path.clone().transform(tf.global) else { return };
    let b = global_path.bounds();
    let bounds = RectF::new(b.left(), b.top(), b.width(), b.height()).inflate(2.).to_outer_rect();
    let Some(visible) = bounds.intersect(Rect::new(tf.origin.0, tf.origin.1, pm.width(), pm.height())) else { return };
    let Some((mut mask, mut previous_ink)) = STROKE_SCRATCH.with(|scratch| scratch.borrow_mut().take())
        .or_else(|| Pixmap::new(CHUNK as u32, CHUNK as u32).map(|mask| (mask, None))) else { return };
    let first_x = visible.x.div_euclid(CHUNK) * CHUNK;
    let first_y = visible.y.div_euclid(CHUNK) * CHUNK;
    for y in (first_y..visible.y + visible.height as i32).step_by(CHUNK as usize) {
        for x in (first_x..visible.x + visible.width as i32).step_by(CHUNK as usize) {
            if let Some(ink) = previous_ink.take() {
                let stride = mask.width() as usize * 4;
                for row in ink.y as usize..ink.y as usize + ink.height as usize {
                    let start = row * stride + ink.x as usize * 4;
                    mask.data_mut()[start..start + ink.width as usize * 4].fill(0);
                }
            }
            mask.fill_path(path, paint, rule,
                tf.global.post_translate(-(x as f32), -(y as f32)), None);
            let ink = bounds.intersect(Rect::new(x, y, CHUNK as u32, CHUNK as u32))
                .expect("Visible canonical chunk intersects fill bounds").translate(-x, -y);
            previous_ink = Some(ink);
            composite_stroke_chunk(pm, &mask, x - tf.origin.0, y - tf.origin.1, ink);
        }
    }
    STROKE_SCRATCH.with(|scratch| *scratch.borrow_mut() = Some((mask, previous_ink)));
}

/// tiny-skia uses different edge builders for contained and clipped paths. Draw
/// solid strokes through the same globally anchored chunks for every viewport,
/// so full exports and dirty previews have identical antialiasing. One reusable
/// 512×512 premultiplied mask bounds temporary storage to 1 MiB.
fn canonical_stroke(pm: &mut Pixmap, path: &Path, stroke: &Stroke, tf: DrawTransform, colour: Color) {
    const CHUNK: i32 = 512;
    let resolution = tiny_skia::PathStroker::compute_resolution_scale(&tf.global);
    let dashed = stroke.dash.as_ref().and_then(|dash| path.dash(dash, resolution));
    if stroke.dash.is_some() && dashed.is_none() { return; }
    let source = dashed.as_ref().unwrap_or(path);
    let mut plain = stroke.clone();
    plain.dash = None;
    // Outline normal uniform-scale strokes once. Preserve tiny-skia's own
    // hairline/affine-width handling for thin or anisotropically transformed paths.
    let g = tf.global;
    let x_length = g.sx * g.sx + g.ky * g.ky;
    let y_length = g.kx * g.kx + g.sy * g.sy;
    let uniform = (x_length - y_length).abs() <= 1e-5 * x_length.max(y_length).max(1.)
        && (g.sx * g.kx + g.ky * g.sy).abs() <= 1e-5 * x_length.max(y_length).max(1.);
    let outline = (uniform && plain.width * resolution > 1.)
        .then(|| source.stroke(&plain, resolution).and_then(|path| path.transform(tf.global)))
        .flatten();
    let transformed;
    let bounds = if let Some(outline) = &outline { outline.bounds() } else {
        transformed = match source.clone().transform(tf.global) { Some(path) => path, None => return };
        transformed.bounds()
    };
    let support = if outline.is_some() { 2. } else { plain.width * resolution * plain.miter_limit.max(1.) + 2. };
    let bounds = RectF::new(bounds.left(), bounds.top(), bounds.width(), bounds.height())
        .inflate(support).to_outer_rect();
    let Some(visible) = bounds.intersect(Rect::new(tf.origin.0, tf.origin.1, pm.width(), pm.height())) else { return };
    let Some((mut mask, mut previous_ink)) = STROKE_SCRATCH.with(|scratch| scratch.borrow_mut().take())
        .or_else(|| Pixmap::new(CHUNK as u32, CHUNK as u32).map(|mask| (mask, None))) else { return };
    let paint = solid(colour);
    let first_x = visible.x.div_euclid(CHUNK) * CHUNK;
    let first_y = visible.y.div_euclid(CHUNK) * CHUNK;
    for y in (first_y..visible.y + visible.height as i32).step_by(CHUNK as usize) {
        for x in (first_x..visible.x + visible.width as i32).step_by(CHUNK as usize) {
            if let Some(ink) = previous_ink.take() {
                let stride = mask.width() as usize * 4;
                for row in ink.y as usize..ink.y as usize + ink.height as usize {
                    let start = row * stride + ink.x as usize * 4;
                    mask.data_mut()[start..start + ink.width as usize * 4].fill(0);
                }
            }
            let offset = Transform::from_translate(-(x as f32), -(y as f32));
            if let Some(outline) = &outline {
                mask.fill_path(outline, &paint, FillRule::Winding, offset, None);
            } else {
                mask.stroke_path(source, &paint, &plain,
                    tf.global.post_translate(-(x as f32), -(y as f32)), None);
            }
            // Track everything the rasterizer can paint, not just the part visible
            // in this viewport; the next object may expose a different mask region.
            let ink = bounds.intersect(Rect::new(x, y, CHUNK as u32, CHUNK as u32))
                .expect("Visible canonical chunk intersects stroke bounds")
                .translate(-x, -y);
            previous_ink = Some(ink);
            composite_stroke_chunk(pm, &mask, x - tf.origin.0, y - tf.origin.1, ink);
        }
    }
    STROKE_SCRATCH.with(|scratch| *scratch.borrow_mut() = Some((mask, previous_ink)));
}

fn composite_stroke_chunk(target: &mut Pixmap, chunk: &Pixmap, x: i32, y: i32, ink: Rect) {
    // Integer exclusive bounds avoid tiny-skia Pattern/Pad's negative-offset
    // rectangle overscan, which can blend a chunk's final column twice.
    let source_x = i64::from(ink.x).max(-i64::from(x)).max(0);
    let source_y = i64::from(ink.y).max(-i64::from(y)).max(0);
    let right = (i64::from(ink.x) + i64::from(ink.width)).min(i64::from(chunk.width()))
        .min(i64::from(target.width()) - i64::from(x));
    let bottom = (i64::from(ink.y) + i64::from(ink.height)).min(i64::from(chunk.height()))
        .min(i64::from(target.height()) - i64::from(y));
    if right <= source_x || bottom <= source_y { return; }
    let left = (i64::from(x) + source_x) as usize;
    let top = (i64::from(y) + source_y) as usize;
    let width = (right - source_x) as usize;
    let height = (bottom - source_y) as usize;
    let (source_x, source_y) = (source_x as usize, source_y as usize);
    let stride = target.width() as usize * 4;
    let source_stride = chunk.width() as usize * 4;
    for row in 0..height {
        let start = (top + row) * stride + left * 4;
        let source_start = (source_y + row) * source_stride + source_x * 4;
        for (destination, source) in target.data_mut()[start..start + width * 4].chunks_exact_mut(4)
            .zip(chunk.data()[source_start..source_start + width * 4].chunks_exact(4)) {
            if source[3] == 0 { continue; }
            if source[3] == 255 { destination.copy_from_slice(source); continue; }
            let inverse_alpha = 255 - u32::from(source[3]);
            for channel in 0..4 {
                let product = u32::from(destination[channel]) * inverse_alpha + 128;
                destination[channel] = (u32::from(source[channel]) + ((product + (product >> 8)) >> 8))
                    .min(255) as u8;
            }
        }
    }
}

fn dot(pm: &mut Pixmap, p: PointF, st: &Style, tf: DrawTransform) {
    if let Some(c) = PathBuilder::from_circle(p.x, p.y, (st.stroke_width / 2.0).max(0.5)) {
        canonical_fill(pm, &c, &solid(st.stroke), FillRule::Winding, tf);
    }
}

fn draw_arrow(pm: &mut Pixmap, tf: DrawTransform, st: &Style, a: PointF, b: PointF, h: &ArrowHeads) {
    let len = h.length(st.stroke_width);
    let end = shapes::head_geometry(h.end, b, a, len, h.angle);
    let start = shapes::head_geometry(h.start, a, b, len, h.angle);
    // Shorten the shaft so it does not poke through filled heads.
    let (sa, sb) = (start.shaft_end, end.shaft_end);
    if sa.distance(sb) > 1e-3 && (sb - sa).dot(b - a) > 0.0 {
        let mut pb = PathBuilder::new();
        pb.move_to(sa.x, sa.y);
        pb.line_to(sb.x, sb.y);
        if let Some(p) = pb.finish() {
            stroke_only(pm, &p, tf, st, st.stroke);
        }
    } else if a.distance(b) > 1e-3 && h.end == HeadStyle::None && h.start == HeadStyle::None {
        dot(pm, a, st, tf);
    }
    draw_heads(pm, tf, st, &[start, end]);
}

fn draw_heads(pm: &mut Pixmap, tf: DrawTransform, st: &Style, heads: &[shapes::HeadGeom]) {
    for g in heads {
        if let Some(p) = &g.fill {
            canonical_fill(pm, p, &solid(st.stroke), FillRule::Winding, tf);
        }
        if let Some(p) = &g.stroke {
            let mut s2 = st.clone();
            s2.dash = crate::style::DashStyle::Solid;
            stroke_only(pm, p, tf, &s2, st.stroke);
        }
    }
}

fn draw_freehand(
    pm: &mut Pixmap,
    tf: DrawTransform,
    st: &Style,
    pts: &[PointF],
    smooth: bool,
    arrow: Option<&ArrowHeads>,
) {
    let Some(first) = pts.first() else { return };
    let Some(path) = shapes::freehand_path(pts, smooth) else {
        dot(pm, *first, st, tf);
        return;
    };
    stroke_only(pm, &path, tf, st, st.stroke);
    if let Some(h) = arrow {
        let len = h.length(st.stroke_width);
        let mut geoms = Vec::new();
        for (style, at_end) in [(h.end, true), (h.start, false)] {
            let tip = if at_end { pts[pts.len() - 1] } else { pts[0] };
            if let Some(r) = shapes::direction_ref(pts, at_end, len * 0.6) {
                geoms.push(shapes::head_geometry(style, tip, r, len, h.angle));
            }
        }
        draw_heads(pm, tf, st, &geoms);
    }
}

fn draw_bitmap(pm: &mut Pixmap, tf: DrawTransform, rect: RectF, rotation: f32, img: &Pixmap) {
    let r = rect.normalized();
    if r.is_empty() {
        return;
    }
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let t = rot_transform(tf, r, rotation).pre_translate(r.x, r.y).pre_scale(r.w / iw, r.h / ih);
    let Some(rect) = tiny_skia::Rect::from_xywh(0.0, 0.0, iw, ih) else { return };
    let paint = Paint {
        shader: Pattern::new(
            img.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Bicubic,
            1.0,
            Transform::identity(),
        ),
        blend_mode: SkBlend::SourceOver,
        anti_alias: true,
        force_hq_pipeline: false,
    };
    pm.fill_path(&PathBuilder::from_rect(rect), &paint, FillRule::Winding, t.local(), None);
}

fn draw_builtin(
    pm: &mut Pixmap,
    tf: DrawTransform,
    st: &Style,
    rect: RectF,
    rotation: f32,
    which: BuiltinSticker,
) {
    let r = rect.normalized();
    let Some((path, rule)) = shapes::builtin_sticker(which) else { return };
    let colour = st.solid_fill().unwrap_or(st.stroke);
    let t = rot_transform(tf, r, rotation).pre_translate(r.x, r.y).pre_scale(r.w, r.h);
    canonical_fill(pm, &path, &solid(colour), rule, t);
}

fn draw_grid(
    pm: &mut Pixmap,
    tf: DrawTransform,
    st: &Style,
    rect: RectF,
    pat: GridPattern,
    spacing: f32,
) {
    if let (Some(paint), Some(r)) = (fill_paint(&st.fill, rect), shapes::sk_rect(rect)) {
        canonical_fill(pm, &PathBuilder::from_rect(r), &paint, FillRule::Winding, tf);
    }
    if !st.has_stroke() {
        return;
    }
    if pat == GridPattern::Dots {
        let mut pb = PathBuilder::new();
        let r = (st.stroke_width / 2.0).max(0.5);
        for p in shapes::grid_dots(rect, spacing) {
            pb.push_circle(p.x, p.y, r);
        }
        if let Some(p) = pb.finish() {
            canonical_fill(pm, &p, &solid(st.stroke), FillRule::Winding, tf);
        }
        return;
    }
    let mut pb = PathBuilder::new();
    for (a, b) in shapes::grid_segments(rect, pat, spacing) {
        pb.move_to(a.x, a.y);
        pb.line_to(b.x, b.y);
    }
    if let Some(p) = pb.finish() {
        let mut s2 = st.clone();
        s2.dash = crate::style::DashStyle::Solid;
        let stroke = Stroke { line_cap: LineCap::Butt, ..stroke_of(&s2) };
        canonical_stroke(pm, &p, &stroke, tf, st.stroke);
    }
}

fn draw_guide(ctx: &Ctx, pm: &mut Pixmap, bounds: RectF) {
    let Some(p) = shapes::rounded_rect(bounds, 0.0) else { return };
    let stroke =
        Stroke { width: 1.0, dash: StrokeDash::new(vec![3.0, 3.0], 0.0), ..Stroke::default() };
    pm.stroke_path(&p, &solid(Color::rgb(0, 160, 255)), &stroke, ctx.view, None);
}

/// Composites a private object layer onto the canvas with the object's opacity, blend mode
/// and (beneath it) its soft shadow.
fn composite_layer(pm: &mut Pixmap, layer: &Pixmap, bbox: Rect, st: &Style, scale: f32) {
    if let Some(sh) = st.shadow {
        draw_shadow(pm, layer, bbox, &sh, scale, st.opacity);
    }
    let paint = PixmapPaint {
        opacity: st.opacity.clamp(0.0, 1.0),
        blend_mode: sk_blend(st.blend),
        quality: FilterQuality::Nearest,
    };
    pm.draw_pixmap(bbox.x, bbox.y, layer.as_ref(), &paint, Transform::identity(), None);
}

fn draw_shadow(
    pm: &mut Pixmap,
    layer: &Pixmap,
    bbox: Rect,
    sh: &crate::style::Shadow,
    scale: f32,
    opacity: f32,
) {
    let Some(mut tint) = Pixmap::new(layer.width(), layer.height()) else { return };
    let col = sh.color;
    for (d, s) in tint.data_mut().chunks_exact_mut(4).zip(layer.data().chunks_exact(4)) {
        let a = ((u32::from(s[3]) * u32::from(col.a) + 127) / 255) as u8;
        d[0] = premul(col.r, a);
        d[1] = premul(col.g, a);
        d[2] = premul(col.b, a);
        d[3] = a;
    }
    let sigma = sh.blur * scale;
    if sigma > 0.0 {
        let (w, h) = (tint.width() as usize, tint.height() as usize);
        image_effects::gaussian_blur_premultiplied(tint.data_mut(), w, h, sigma, BlurMethod::Auto);
    }
    let paint = PixmapPaint {
        opacity: opacity.clamp(0.0, 1.0),
        blend_mode: SkBlend::SourceOver,
        quality: FilterQuality::Nearest,
    };
    pm.draw_pixmap(
        bbox.x + (sh.dx * scale).round() as i32,
        bbox.y + (sh.dy * scale).round() as i32,
        tint.as_ref(),
        &paint,
        Transform::identity(),
        None,
    );
}
