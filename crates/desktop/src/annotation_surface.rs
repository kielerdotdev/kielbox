//! Stable GPU tiles: changing an annotation replaces only the affected image tiles.
use crate::image_editor::render_image;
use gpui::{App, Bounds, ContentMask, Corners, Pixels, RenderImage, Window, fill, point, px, rgb, size};
use rcr_toolkit::{image_editor::Preview, types::Rect};
use std::{collections::BTreeMap, sync::Arc};
/// Contrast against both light and dark pixels; only painted into the UI, never the image.
pub fn outline(window: &mut Window, r: Bounds<Pixels>) {
    for (thickness, color) in [(px(3.), rgb(0x000000)), (px(1.), rgb(0xffffff))] {
        let half = thickness / 2.;
        for b in [
            Bounds::new(
                point(r.left() - half, r.top() - half),
                size(r.size.width + thickness, thickness),
            ),
            Bounds::new(
                point(r.left() - half, r.bottom() - half),
                size(r.size.width + thickness, thickness),
            ),
            Bounds::new(
                point(r.left() - half, r.top() - half),
                size(thickness, r.size.height + thickness),
            ),
            Bounds::new(
                point(r.right() - half, r.top() - half),
                size(thickness, r.size.height + thickness),
            ),
        ] {
            window.paint_quad(fill(b, color));
        }
    }
    for p in [
        r.origin,
        point(r.right(), r.top()),
        point(r.left(), r.bottom()),
        point(r.right(), r.bottom()),
    ] {
        window.paint_quad(fill(
            Bounds::new(p - point(px(3.), px(3.)), size(px(6.), px(6.))),
            rgb(0x000000),
        ));
        window.paint_quad(fill(
            Bounds::new(p - point(px(2.), px(2.)), size(px(4.), px(4.))),
            rgb(0x3ecf8e),
        ));
    }
}
#[derive(Clone)]
pub struct Tile {
    rect: Rect,
    image: Arc<RenderImage>,
}
#[derive(Default)]
pub struct Surface {
    tiles: BTreeMap<(i32, i32), Tile>,
    retired: Vec<Arc<RenderImage>>,
    width: u32,
    height: u32,
    density: f32,
    offset: (f32, f32),
}
/// Retire atlas keys after current window/entity leases return to the app.
/// This does not depend on another paint of the image's former owner.
pub fn retire_deferred(images: Vec<Arc<RenderImage>>, cx: &mut App) {
    if images.is_empty() { return; }
    cx.defer(move |cx| {
        for image in images { cx.drop_image(image, None); }
    });
}
impl Surface {
    pub fn clear(&mut self) {
        for tile in std::mem::take(&mut self.tiles).into_values() {
            self.retired.push(tile.image);
        }
    }
    fn take_images(&mut self) -> Vec<Arc<RenderImage>> {
        let mut images = std::mem::take(&mut self.retired);
        images.extend(std::mem::take(&mut self.tiles).into_values().map(|tile| tile.image));
        self.width = 0;
        self.height = 0;
        self.density = 0.;
        self.offset = (0., 0.);
        images
    }
    pub fn clear_deferred(&mut self, cx: &mut App) {
        retire_deferred(self.take_images(), cx);
    }
    pub fn image_weak_refs(&self) -> Vec<std::sync::Weak<RenderImage>> {
        self.tiles.values().map(|tile| Arc::downgrade(&tile.image))
            .chain(self.retired.iter().map(Arc::downgrade)).collect()
    }
    pub fn apply(&mut self, preview: &mut Preview) {
        if preview.reset {
            self.clear();
        }
        self.width = preview.width;
        self.height = preview.height;
        self.density = preview.raster_density;
        self.offset = preview.raster_offset;
        for patch in preview.tiles.drain(..) {
            if let Some(image) = render_image(patch.rect.width, patch.rect.height, patch.bgra) {
                let tile = Tile {
                    rect: patch.rect,
                    image,
                };
                if let Some(old) = self.tiles.insert((patch.rect.x, patch.rect.y), tile) {
                    self.retired.push(old.image);
                }
            }
        }
    }
    pub fn ready(&self) -> bool {
        !self.tiles.is_empty()
    }
    pub fn snapshot(&self) -> Paint {
        Paint {
            tiles: self.tiles.values().cloned().collect(),
            width: self.width,
            height: self.height,
            density: self.density,
            offset: self.offset,
        }
    }
    pub fn release(&mut self, window: &mut Window) {
        for image in self.retired.drain(..) {
            let _ = window.drop_image(image);
        }
    }
}
pub struct Paint {
    tiles: Vec<Tile>,
    width: u32,
    height: u32,
    density: f32,
    offset: (f32, f32),
}
impl Paint {
    /// Read an existing preview pixel for the region magnifier without copying
    /// the captured frame or creating another GPU image. Coordinates are in
    /// source pixels, before the preview's bounded raster scaling.
    pub fn sample_bgra(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height || !self.density.is_finite() || self.density <= 0. {
            return None;
        }
        let rx = (((x as f32 + 0.5) - self.offset.0) * self.density).floor() as i64;
        let ry = (((y as f32 + 0.5) - self.offset.1) * self.density).floor() as i64;
        for tile in &self.tiles {
            let tx = rx - i64::from(tile.rect.x);
            let ty = ry - i64::from(tile.rect.y);
            if tx < 0 || ty < 0 || tx >= i64::from(tile.rect.width) || ty >= i64::from(tile.rect.height) { continue; }
            let offset = (ty as usize).checked_mul(tile.rect.width as usize)?.checked_add(tx as usize)?.checked_mul(4)?;
            return tile.image.as_bytes(0)?.get(offset..offset.checked_add(4)?)?.try_into().ok();
        }
        None
    }
    pub fn draw(self, bounds: Bounds<Pixels>, window: &mut Window) {
        if self.width == 0 || self.height == 0 || !self.density.is_finite() || self.density <= 0. {
            return;
        }
        // Raster cover edges can extend fractionally outside the logical view.
        // Clip only painting; tool and overlay coordinates keep their logical zoom.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for tile in self.tiles {
                let r = tile.rect;
                let target = Bounds::new(
                    bounds.origin
                        + point(
                            bounds.size.width * (self.offset.0 + r.x as f32 / self.density)
                                / self.width as f32,
                            bounds.size.height * (self.offset.1 + r.y as f32 / self.density)
                                / self.height as f32,
                        ),
                    size(
                        bounds.size.width * r.width as f32 / self.density / self.width as f32,
                        bounds.size.height * r.height as f32 / self.density / self.height as f32,
                    ),
                );
                let _ = window.paint_image(target, Corners::default(), tile.image, 0, false);
            }
        });
    }
}
#[cfg(test)]
mod sampling_tests {
    use super::*;
    #[test]
    fn closing_surface_detaches_active_and_retired_pixels_without_another_paint() {
        let active = crate::image_editor::render_image(2, 1, vec![1; 8]).unwrap();
        let retired = crate::image_editor::render_image(1, 1, vec![2; 4]).unwrap();
        let active_weak = Arc::downgrade(&active);
        let retired_weak = Arc::downgrade(&retired);
        let mut surface = Surface::default();
        surface.tiles.insert((0, 0), Tile { rect: Rect::new(0, 0, 2, 1), image: active });
        surface.retired.push(retired);
        let images = surface.take_images();
        assert!(!surface.ready());
        assert!(surface.image_weak_refs().is_empty());
        assert_eq!(images.len(), 2);
        assert!(active_weak.upgrade().is_some() && retired_weak.upgrade().is_some());
        drop(images);
        assert!(active_weak.upgrade().is_none() && retired_weak.upgrade().is_none());
    }
    #[test]
    fn magnifier_samples_existing_tiles_and_respects_source_edges_and_raster_geometry() {
        let image = crate::image_editor::render_image(2, 1, vec![1, 2, 3, 255, 7, 8, 9, 255]).unwrap();
        let mut paint = Paint { tiles: vec![Tile { rect: Rect::new(0, 0, 2, 1), image }],
            width: 2, height: 1, density: 1., offset: (0., 0.) };
        assert_eq!(paint.sample_bgra(0, 0), Some([1, 2, 3, 255]));
        assert_eq!(paint.sample_bgra(1, 0), Some([7, 8, 9, 255]));
        assert_eq!(paint.sample_bgra(2, 0), None);
        assert_eq!(paint.sample_bgra(0, 1), None);
        paint.width = 20;
        paint.height = 30;
        paint.offset = (10., 20.);
        paint.density = 0.5;
        assert_eq!(paint.sample_bgra(10, 20), Some([1, 2, 3, 255]));
        assert_eq!(paint.sample_bgra(12, 20), Some([7, 8, 9, 255]));
        assert_eq!(paint.sample_bgra(9, 20), None);
    }
}
