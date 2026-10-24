//! Pixel-space capture selection, independent of window DPI and desktop origin.
use crate::types::Rect;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
#[derive(Clone, Copy)]
enum Drag {
    Create(Point),
    Move {
        start: Point,
        rect: Rect,
    },
    Resize {
        start: Point,
        rect: Rect,
        x: i8,
        y: i8,
    },
}
pub struct Selection {
    pub rect: Option<Rect>,
    pub size: (u32, u32),
    drag: Option<Drag>,
}
impl Selection {
    pub fn new(width: u32, height: u32, selected: bool) -> Self {
        Self {
            rect: selected.then(|| Rect::new(0, 0, width, height)),
            size: (width, height),
            drag: None,
        }
    }
    pub fn down(&mut self, p: Point, tolerance: f32) {
        self.drag = Some(if let Some(r) = self.rect {
            let x = if (p.x - r.x as f32).abs() <= tolerance {
                -1
            } else if (p.x - (r.x as f32 + r.width as f32)).abs() <= tolerance {
                1
            } else {
                0
            };
            let y = if (p.y - r.y as f32).abs() <= tolerance {
                -1
            } else if (p.y - (r.y as f32 + r.height as f32)).abs() <= tolerance {
                1
            } else {
                0
            };
            let within = p.x >= r.x as f32 - tolerance
                && p.x <= r.x as f32 + r.width as f32 + tolerance
                && p.y >= r.y as f32 - tolerance
                && p.y <= r.y as f32 + r.height as f32 + tolerance;
            if within && (x != 0 || y != 0) {
                Drag::Resize {
                    start: p,
                    rect: r,
                    x,
                    y,
                }
            } else if within {
                Drag::Move { start: p, rect: r }
            } else {
                Drag::Create(p)
            }
        } else {
            Drag::Create(p)
        });
        if matches!(self.drag, Some(Drag::Create(_))) {
            self.rect = None;
        }
    }
    pub fn update(&mut self, p: Point, square: bool) {
        let Some(drag) = self.drag else {
            return;
        };
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        self.rect = match drag {
            Drag::Create(start) => {
                let mut dx = p.x - start.x;
                let mut dy = p.y - start.y;
                if square {
                    let side = dx.abs().min(dy.abs());
                    dx = side * dx.signum();
                    dy = side * dy.signum();
                }
                rectangle(start.x, start.y, start.x + dx, start.y + dy, w, h)
            }
            Drag::Move { start, rect } => Some(Rect::new(
                (rect.x as f32 + p.x - start.x)
                    .round()
                    .clamp(0., w - rect.width as f32) as i32,
                (rect.y as f32 + p.y - start.y)
                    .round()
                    .clamp(0., h - rect.height as f32) as i32,
                rect.width,
                rect.height,
            )),
            Drag::Resize { start, rect, x, y } => {
                let (mut left, mut top, mut right, mut bottom) = (
                    rect.x as f32,
                    rect.y as f32,
                    rect.x as f32 + rect.width as f32,
                    rect.y as f32 + rect.height as f32,
                );
                if x < 0 {
                    left = (left + p.x - start.x).clamp(0., right - 1.);
                }
                if x > 0 {
                    right = (right + p.x - start.x).clamp(left + 1., w);
                }
                if y < 0 {
                    top = (top + p.y - start.y).clamp(0., bottom - 1.);
                }
                if y > 0 {
                    bottom = (bottom + p.y - start.y).clamp(top + 1., h);
                }
                rectangle(left, top, right, bottom, w, h)
            }
        };
    }
    pub fn up(&mut self) {
        self.drag = None;
    }
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }
    pub fn full(&mut self) {
        self.rect = Some(Rect::new(0, 0, self.size.0, self.size.1));
        self.drag = None;
    }
    pub fn clear(&mut self) {
        self.rect = None;
        self.drag = None;
    }
    pub fn nudge(&mut self, dx: i32, dy: i32, resize: bool) {
        if let Some(r) = self.rect.as_mut() {
            if resize {
                r.width =
                    (r.width as i64 + dx as i64).clamp(1, self.size.0 as i64 - r.x as i64) as u32;
                r.height =
                    (r.height as i64 + dy as i64).clamp(1, self.size.1 as i64 - r.y as i64) as u32;
            } else {
                r.x = (r.x + dx).clamp(0, (self.size.0 - r.width) as i32);
                r.y = (r.y + dy).clamp(0, (self.size.1 - r.height) as i32);
            }
        }
    }
}
fn rectangle(x1: f32, y1: f32, x2: f32, y2: f32, w: f32, h: f32) -> Option<Rect> {
    let l = x1.min(x2).floor().clamp(0., w);
    let t = y1.min(y2).floor().clamp(0., h);
    let r = x1.max(x2).ceil().clamp(0., w);
    let b = y1.max(y2).ceil().clamp(0., h);
    (r > l && b > t).then(|| Rect::new(l as i32, t as i32, (r - l) as u32, (b - t) as u32))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn p(x: f32, y: f32) -> Point {
        Point { x, y }
    }
    #[test]
    fn create_move_resize_and_keyboard_keep_selection_in_bounds() {
        let mut s = Selection::new(1000, 600, false);
        s.down(p(400., 300.), 5.);
        s.update(p(100., 100.), false);
        s.up();
        assert_eq!(s.rect, Some(Rect::new(100, 100, 300, 200)));
        s.down(p(250., 200.), 5.);
        s.update(p(1300., 1000.), false);
        s.up();
        assert_eq!(s.rect, Some(Rect::new(700, 400, 300, 200)));
        s.down(p(700., 400.), 5.);
        s.update(p(650., 350.), false);
        s.up();
        assert_eq!(s.rect, Some(Rect::new(650, 350, 350, 250)));
        s.nudge(1, 1, true);
        assert_eq!(s.rect.unwrap().width, 350);
        s.nudge(-1, -1, false);
        assert_eq!(s.rect.unwrap().x, 649);
    }
    #[test]
    fn square_selection_clips_and_zero_area_is_not_accepted() {
        let mut s = Selection::new(100, 100, false);
        s.down(p(10., 10.), 3.);
        s.update(p(90., 50.), true);
        s.up();
        assert_eq!(s.rect, Some(Rect::new(10, 10, 40, 40)));
        s.clear();
        s.down(p(20., 20.), 3.);
        s.update(p(20., 80.), false);
        assert!(s.rect.is_none());
    }
}
