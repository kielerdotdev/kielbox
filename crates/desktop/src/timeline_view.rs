//! Pixel-independent timeline viewport. Editing positions always remain in
//! project frames; zooming and panning affect only the visible time window.
#[derive(Clone, Copy)]
pub struct TimelineViewport {
    start: u64,
    zoom: u32,
    following: bool,
}
impl Default for TimelineViewport {
    fn default() -> Self {
        Self {
            start: 0,
            zoom: 0,
            following: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TimelineWindow {
    pub start: u64,
    pub span: u64,
}
impl TimelineWindow {
    pub fn at(self, fraction: f64) -> u64 {
        self.start.saturating_add(
            ((self.span as f64 * fraction.clamp(0., 1.)).floor() as u64)
                .min(self.span.saturating_sub(1)),
        )
    }
    pub fn fraction(self, frame: i64) -> f32 {
        ((frame as f64 - self.start as f64) / self.span as f64) as f32
    }
}
impl TimelineViewport {
    pub fn window(self, extent: u64, fps: u32) -> TimelineWindow {
        let extent = extent.max(1);
        let span = (extent / (1_u64 << self.zoom.min(12)))
            .max(u64::from(fps).min(extent))
            .min(extent);
        TimelineWindow {
            start: self.start.min(extent - span),
            span,
        }
    }
    pub fn fit(&mut self) {
        *self = Self::default();
    }
    pub fn is_following(self) -> bool {
        self.following
    }
    pub fn set_following(&mut self, following: bool, frame: u64, extent: u64, fps: u32) {
        self.following = following;
        self.follow_playhead(frame, extent, fps);
    }
    /// Playback pages only after its position leaves the visible interval.
    /// Explicit seeks still use reveal even when automatic following is off.
    pub fn follow_playhead(&mut self, frame: u64, extent: u64, fps: u32) {
        if self.following {
            self.reveal(frame, extent, fps);
        }
    }
    pub fn zoom(&mut self, inside: bool, fraction: f64, extent: u64, fps: u32) {
        let old = self.window(extent, fps);
        let anchor = old.at(fraction);
        let old_zoom = self.zoom;
        self.zoom = if inside {
            self.zoom.saturating_add(1).min(12)
        } else {
            self.zoom.saturating_sub(1)
        };
        let span = self.window(extent, fps).span;
        if inside && span == old.span {
            self.zoom = old_zoom;
        }
        self.start = anchor
            .saturating_sub((fraction.clamp(0., 1.) * span as f64) as u64)
            .min(extent.saturating_sub(span));
        let new = self.window(extent, fps);
        if new.start != old.start || new.span != old.span {
            self.following = false;
        }
    }
    pub fn pan(&mut self, fraction: f64, extent: u64, fps: u32) {
        let window = self.window(extent, fps);
        self.start = (window.start as f64 + fraction * window.span as f64)
            .round()
            .clamp(0., extent.saturating_sub(window.span) as f64) as u64;
        if self.start != window.start {
            self.following = false;
        }
    }
    pub fn reveal(&mut self, frame: u64, extent: u64, fps: u32) {
        let window = self.window(extent, fps);
        if frame < window.start || frame >= window.start.saturating_add(window.span) {
            self.start = frame
                .saturating_sub(window.span / 2)
                .min(extent.saturating_sub(window.span));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn follow_pages_at_window_edge_without_changing_zoom_or_recentering_each_frame() {
        let mut view = TimelineViewport::default();
        assert!(view.is_following());
        view.zoom(true, 0.5, 7200, 60);
        assert!(!view.is_following());
        view.set_following(true, 2000, 7200, 60);
        let before = view.window(7200, 60);
        view.follow_playhead(before.start + before.span - 1, 7200, 60);
        assert_eq!(view.window(7200, 60).start, before.start);
        let next = before.start + before.span;
        view.follow_playhead(next, 7200, 60);
        let after = view.window(7200, 60);
        assert_eq!(after.span, before.span);
        assert!(
            after.start > before.start && after.start <= next && next < after.start + after.span
        );
        view.follow_playhead(7199, 7200, 60);
        let end = view.window(7200, 60);
        assert_eq!(end.start + end.span, 7200);
    }
    #[test]
    fn deliberate_navigation_suspends_follow_until_reenabled_or_fit() {
        let mut view = TimelineViewport::default();
        view.zoom(true, 0.5, 7200, 60);
        view.set_following(true, 2000, 7200, 60);
        view.pan(-0.25, 7200, 60);
        assert!(!view.is_following());
        let manually_chosen = view.window(7200, 60);
        view.follow_playhead(7199, 7200, 60);
        assert_eq!(view.window(7200, 60).start, manually_chosen.start);
        view.set_following(true, 7199, 7200, 60);
        assert_eq!(view.window(7200, 60).start + manually_chosen.span, 7200);
        view.zoom(true, 0.5, 7200, 60);
        assert!(!view.is_following());
        view.fit();
        assert!(view.is_following());
        assert_eq!(view.window(7200, 60).span, 7200);
    }
    #[test]
    fn zoom_preserves_pointer_frame_and_pan_clamps_to_media() {
        let mut view = TimelineViewport::default();
        let anchor = view.window(7200, 60).at(0.73);
        view.zoom(true, 0.73, 7200, 60);
        assert!(view.window(7200, 60).at(0.73).abs_diff(anchor) <= 1);
        view.pan(-100., 7200, 60);
        assert_eq!(view.window(7200, 60).start, 0);
        view.pan(100., 7200, 60);
        assert_eq!(view.window(7200, 60).start, 3600);
    }
    #[test]
    fn zoom_limits_still_show_one_second_and_short_projects_stay_valid() {
        let mut view = TimelineViewport::default();
        for _ in 0..32 {
            view.zoom(true, 0.5, 7200, 60);
        }
        assert_eq!(view.window(7200, 60).span, 60);
        assert_eq!(view.window(20, 60).span, 20);
        view.reveal(7199, 7200, 60);
        assert_eq!(view.window(7200, 60).start, 7140);
        view.fit();
        assert_eq!(view.window(7200, 60).span, 7200);
        assert_eq!(view.window(7200, 60).start, 0);
        assert_eq!(view.window(0, 60).span, 1);
    }
}
