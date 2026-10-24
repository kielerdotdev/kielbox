//! One immutable, bounded snapshot of physical window/control rectangles.
//! Inspired by ShareX's WindowsRectangleList: visible/uncloaked windows, DWM
//! frame bounds and child rectangles clipped to their ancestors. No application
//! messages, text, class names, accessibility queries or pixel capture are used.
//! Reference: https://github.com/ShareX/ShareX/blob/develop/ShareX.ScreenCaptureLib/Helpers/WindowsRectangleList.cs
use rcr_toolkit::types::{Point, Rect};
use std::{collections::HashSet, sync::atomic::{AtomicBool, Ordering}, time::{Duration, Instant}};
use windows::{core::BOOL, Win32::{Foundation::{HWND, LPARAM, RECT}, UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowRect, IsIconic, IsWindowVisible, GW_CHILD, GW_HWNDNEXT,
}}};

const MAX_TARGETS: usize = 2048;
const MAX_VISITED: usize = 4096;
const MAX_DEPTH: u8 = 8;
const SNAPSHOT_BUDGET: Duration = Duration::from_millis(50);
static SNAPSHOT_ACTIVE: AtomicBool = AtomicBool::new(false);

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmGetWindowAttribute(hwnd: HWND, attribute: u32, value: *mut std::ffi::c_void, bytes: u32) -> i32;
}

#[derive(Clone, Copy)]
struct Target { rect: Rect, parent: Option<usize> }
#[derive(Clone)]
struct WindowTargets { rect: Rect, targets: Vec<Target> }
#[derive(Clone, Default)]
pub(crate) struct TargetSnapshot {
    // EnumWindows supplies top-level Z order, foremost first. Never select a
    // control belonging to a lower window through an overlapping upper window.
    roots: Vec<WindowTargets>,
    truncated: bool,
}
struct Admission;
impl Admission {
    fn acquire(gate: &'static AtomicBool) -> Option<AdmissionGuard> {
        gate.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok()?;
        Some(AdmissionGuard(gate))
    }
}
struct AdmissionGuard(&'static AtomicBool);
impl Drop for AdmissionGuard {
    fn drop(&mut self) { self.0.store(false, Ordering::Release); }
}
impl TargetSnapshot {
    /// Run on a worker before creating the overlay. The time limit is checked
    /// between non-message Win32 calls; it cannot preempt an OS call in flight.
    /// `exclude_window` is an opaque HWND value, compared only, never dereferenced.
    pub(crate) fn capture(desktop: Rect, exclude_window: Option<usize>) -> Self {
        let Some(_admission) = Admission::acquire(&SNAPSHOT_ACTIVE) else {
            return Self { truncated: true, ..Default::default() };
        };
        if desktop.is_empty() { return Self::default(); }
        let mut scan = Scan { desktop, exclude_window, started: Instant::now(),
            visited: HashSet::new(), targets: 0, result: Self::default() };
        // Callback/context are synchronous and remain on this worker's stack.
        let result = unsafe { EnumWindows(Some(visit_top), LPARAM((&mut scan as *mut Scan) as isize)) };
        if result.is_err() { scan.result.truncated = true; }
        scan.result
    }
    pub(crate) fn hit_test(&self, point: Point) -> Option<Rect> {
        let root = self.roots.iter().find(|root| root.rect.contains(point))?;
        let mut parent = None;
        let mut selected = None;
        for _ in 0..=MAX_DEPTH {
            // Direct children are captured in sibling Z order. Descend only
            // into the foremost containing branch: a deeper rear control must
            // never win through a shallower foreground sibling.
            let Some((index, target)) = root.targets.iter().enumerate()
                .find(|(_, target)| target.parent == parent && target.rect.contains(point)) else { break; };
            selected = Some(target.rect);
            parent = Some(index);
        }
        selected
    }
    #[cfg(test)]
    fn len(&self) -> usize { self.roots.iter().map(|root| root.targets.len()).sum() }
    #[cfg(test)]
    fn truncated(&self) -> bool { self.truncated }
    /// Synthetic physical geometry for owned UI fixtures; never touches Win32.
    pub(crate) fn from_rectangles(rectangles: Vec<Rect>) -> Self {
        let truncated = rectangles.len() > MAX_TARGETS;
        let mut rectangles: Vec<_> = rectangles.into_iter().take(MAX_TARGETS)
            .filter(|rect| rect.width >= 3 && rect.height >= 3).collect();
        rectangles.sort_by_key(|rect| std::cmp::Reverse(rect.area()));
        let mut targets: Vec<Target> = Vec::with_capacity(rectangles.len());
        for rect in rectangles {
            let parent = targets.iter().enumerate()
                .filter(|(_, target)| target.rect != rect && target.rect.intersect(rect) == Some(rect))
                .min_by_key(|(_, target)| target.rect.area()).map(|(index, _)| index);
            targets.push(Target { rect, parent });
        }
        let Some(rect) = targets.iter().map(|target| target.rect).reduce(Rect::union) else {
            return Self { truncated, ..Default::default() };
        };
        Self { roots: vec![WindowTargets { rect, targets }], truncated }
    }
}
struct Scan {
    desktop: Rect,
    exclude_window: Option<usize>,
    started: Instant,
    visited: HashSet<usize>,
    targets: usize,
    result: TargetSnapshot,
}
impl Scan {
    fn stopped(&mut self) -> bool {
        let stop = self.targets >= MAX_TARGETS || self.visited.len() >= MAX_VISITED
            || self.started.elapsed() >= SNAPSHOT_BUDGET;
        self.result.truncated |= stop;
        stop
    }
    fn rectangle(&mut self, hwnd: HWND, clip: Rect, top: bool) -> Option<Rect> {
        if self.stopped() { return None; }
        let raw = hwnd.0 as usize;
        if !self.visited.insert(raw) || self.exclude_window == Some(raw) { return None; }
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() { return None; }
        let mut bounds = RECT::default();
        if top {
            let mut cloaked = 0u32;
            let status = unsafe { DwmGetWindowAttribute(hwnd, 14,
                (&mut cloaked as *mut u32).cast(), std::mem::size_of::<u32>() as u32) };
            if status >= 0 && cloaked != 0 { return None; }
        }
        let extended = top && unsafe { DwmGetWindowAttribute(hwnd, 9,
            (&mut bounds as *mut RECT).cast(), std::mem::size_of::<RECT>() as u32) } >= 0;
        // DWM may return an empty rectangle before a newly created window has
        // been composed. ShareX also falls back to GetWindowRect in that case.
        let usable_extended = extended && bounds.right > bounds.left && bounds.bottom > bounds.top;
        if !usable_extended && unsafe { GetWindowRect(hwnd, &mut bounds) }.is_err() { return None; }
        clipped_rect(bounds, clip)
    }
    fn children(&mut self, parent: HWND, clip: Rect, depth: u8, parent_index: usize, targets: &mut Vec<Target>) {
        if self.stopped() { return; }
        let mut child = unsafe { GetWindow(parent, GW_CHILD) }.ok();
        if depth > MAX_DEPTH {
            self.result.truncated |= child.is_some();
            return;
        }
        while let Some(hwnd) = child {
            if self.stopped() { break; }
            // Resolve next before descending: windows may disappear/reparent,
            // and global visit/time limits still bound a changing sibling list.
            child = unsafe { GetWindow(hwnd, GW_HWNDNEXT) }.ok();
            if let Some(rect) = self.rectangle(hwnd, clip, false) {
                let index = targets.len();
                targets.push(Target { rect, parent: Some(parent_index) });
                self.targets += 1;
                self.children(hwnd, rect, depth + 1, index, targets);
            }
        }
    }
}
unsafe extern "system" fn visit_top(hwnd: HWND, context: LPARAM) -> BOOL {
    let scan = unsafe { &mut *(context.0 as *mut Scan) };
    if scan.stopped() { return BOOL(0); }
    if let Some(rect) = scan.rectangle(hwnd, scan.desktop, true) {
        let mut root = WindowTargets { rect, targets: vec![Target { rect, parent: None }] };
        scan.targets += 1;
        scan.children(hwnd, rect, 1, 0, &mut root.targets);
        scan.result.roots.push(root);
    }
    BOOL::from(!scan.stopped())
}
fn clipped_rect(bounds: RECT, clip: Rect) -> Option<Rect> {
    let width = i64::from(bounds.right).checked_sub(i64::from(bounds.left))?;
    let height = i64::from(bounds.bottom).checked_sub(i64::from(bounds.top))?;
    let rect = Rect::new(bounds.left, bounds.top, u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    rect.intersect(clip).filter(|rect| rect.width >= 3 && rect.height >= 3)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Native interop only on wholly owned, nonactivating offscreen STATIC
    /// windows. No input, screenshots, desktop pixels or private strings.
    #[test]
    #[ignore = "Creates owned offscreen HWNDs; root runs this native interop proof serially"]
    fn owned_offscreen_windows_snapshot_nested_controls_clipping_exclusion_and_cleanup() -> anyhow::Result<()> {
        use anyhow::{Context, ensure};
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, GetSystemMetrics,
            IsWindow, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE, WS_CHILD};
        struct OwnedWindow(Option<HWND>);
        impl Drop for OwnedWindow {
            fn drop(&mut self) {
                if let Some(hwnd) = self.0.take() { let _ = unsafe { DestroyWindow(hwnd) }; }
            }
        }
        let desktop = Rect::new(-30000, -30000, 400, 300);
        // Refuse instead of showing anything if an unusual monitor arrangement
        // actually reaches this fixture's offscreen coordinates.
        let occupied = unsafe { Rect::new(GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(0) as u32, GetSystemMetrics(SM_CYVIRTUALSCREEN).max(0) as u32) };
        ensure!(occupied.intersect(desktop).is_none(), "Owned interop fixture is not offscreen");
        let parent = unsafe { CreateWindowExW(WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            windows::core::w!("STATIC"), windows::core::w!(""), WS_POPUP | WS_VISIBLE,
            desktop.x, desktop.y, 400, 300, None, None, None, None) }.context("Create owned offscreen parent")?;
        let mut owned = OwnedWindow(Some(parent));
        let pane = unsafe { CreateWindowExW(WS_EX_NOACTIVATE,
            windows::core::w!("STATIC"), windows::core::w!(""), WS_CHILD | WS_VISIBLE,
            30, 40, 150, 100, Some(parent), None, None, None) }.context("Create owned pane")?;
        let nested = unsafe { CreateWindowExW(WS_EX_NOACTIVATE,
            windows::core::w!("STATIC"), windows::core::w!(""), WS_CHILD | WS_VISIBLE,
            10, 15, 40, 30, Some(pane), None, None, None) }.context("Create owned nested control")?;
        let clipped = unsafe { CreateWindowExW(WS_EX_NOACTIVATE,
            windows::core::w!("STATIC"), windows::core::w!(""), WS_CHILD | WS_VISIBLE,
            130, 70, 60, 60, Some(pane), None, None, None) }.context("Create owned clipped control")?;
        let snapshot = TargetSnapshot::capture(desktop, None);
        ensure!(snapshot.hit_test(Point::new(-29955, -29940)) == Some(Rect::new(-29960, -29945, 40, 30)),
            "Owned nested child geometry/hierarchy was not detected");
        ensure!(snapshot.hit_test(Point::new(-29835, -29885)) == Some(Rect::new(-29840, -29890, 20, 30)),
            "Owned child was not clipped to its parent pane");
        ensure!(snapshot.hit_test(Point::new(-29700, -29750)) == Some(desktop),
            "Owned parent fallback geometry differs");
        let excluded = TargetSnapshot::capture(desktop, Some(parent.0 as usize));
        ensure!(excluded.hit_test(Point::new(-29955, -29940)).is_none(), "Excluded owned parent leaked a child target");
        unsafe { DestroyWindow(parent) }.context("Destroy owned offscreen hierarchy")?;
        owned.0 = None;
        for hwnd in [parent, pane, nested, clipped] {
            ensure!(!unsafe { IsWindow(Some(hwnd)) }.as_bool(), "Owned HWND survived parent destruction");
        }
        Ok(())
    }
    #[test]
    fn deepest_smallest_control_wins_only_inside_foremost_window() {
        let front = Rect::new(-200, 0, 400, 400);
        let deep = Rect::new(-150, 50, 80, 100);
        let smaller = Rect::new(-140, 60, 20, 20);
        let back = Rect::new(-300, 0, 800, 500);
        let snapshot = TargetSnapshot { roots: vec![
            WindowTargets { rect: front, targets: vec![Target { rect: front, parent: None },
                Target { rect: deep, parent: Some(0) }, Target { rect: smaller, parent: Some(1) }] },
            WindowTargets { rect: back, targets: vec![Target { rect: back, parent: None },
                Target { rect: smaller, parent: Some(0) }] },
        ], truncated: false };
        assert_eq!(snapshot.hit_test(Point::new(-135, 65)), Some(smaller));
        assert_eq!(snapshot.hit_test(Point::new(-100, 120)), Some(deep));
        assert_eq!(snapshot.hit_test(Point::new(0, 100)), Some(front));
        assert_eq!(snapshot.hit_test(Point::new(300, 100)), Some(back));
        assert_eq!(snapshot.hit_test(Point::new(900, 900)), None);
    }
    #[test]
    fn clipping_preserves_negative_physical_origins_and_rejects_invalid_tiny_rects() {
        let desktop = Rect::new(-1920, -200, 3840, 1280);
        assert_eq!(clipped_rect(RECT { left: -2000, top: -300, right: -1000, bottom: 100 }, desktop),
            Some(Rect::new(-1920, -200, 920, 300)));
        for bounds in [RECT { left: 0, top: 0, right: -1, bottom: 4 },
            RECT { left: 0, top: 0, right: 2, bottom: 30 },
            RECT { left: 3000, top: 0, right: 4000, bottom: 30 }] {
            assert!(clipped_rect(bounds, desktop).is_none());
        }
    }
    #[test]
    fn foreground_sibling_occludes_deeper_rear_controls_and_truncated_roots_still_occlude() {
        let whole = Rect::new(0, 0, 400, 400);
        let front = Rect::new(10, 10, 100, 100);
        let rear = Rect::new(0, 0, 300, 300);
        let rear_control = Rect::new(20, 20, 20, 20);
        let snapshot = TargetSnapshot { roots: vec![WindowTargets { rect: whole, targets: vec![
            Target { rect: whole, parent: None }, Target { rect: front, parent: Some(0) },
            Target { rect: rear, parent: Some(0) }, Target { rect: rear_control, parent: Some(2) },
        ] }], truncated: false };
        assert_eq!(snapshot.hit_test(Point::new(25, 25)), Some(front));
        assert_eq!(snapshot.hit_test(Point::new(120, 120)), Some(rear));
        let truncated = TargetSnapshot { roots: vec![
            WindowTargets { rect: whole, targets: vec![Target { rect: whole, parent: None }] },
            WindowTargets { rect: rear, targets: vec![Target { rect: rear_control, parent: None }] },
        ], truncated: true };
        assert_eq!(truncated.hit_test(Point::new(25, 25)), Some(whole));
    }
    #[test]
    fn snapshot_count_and_time_budgets_stop_without_any_native_calls() {
        let mut scan = Scan { desktop: Rect::new(0, 0, 100, 100), exclude_window: None,
            started: Instant::now(), visited: HashSet::new(), targets: MAX_TARGETS,
            result: TargetSnapshot::default() };
        assert!(scan.stopped());
        assert!(scan.result.truncated());
        scan.targets = 0;
        scan.started = Instant::now() - SNAPSHOT_BUDGET * 2;
        assert!(scan.stopped());
        scan.started = Instant::now();
        scan.visited = (0..MAX_VISITED).collect();
        assert!(scan.stopped());
    }
    #[test]
    fn injection_is_bounded_and_default_has_no_targets() {
        assert_eq!(TargetSnapshot::default().hit_test(Point::new(0, 0)), None);
        let snapshot = TargetSnapshot::from_rectangles(vec![Rect::new(-10, -10, 20, 20); MAX_TARGETS + 1]);
        assert_eq!(snapshot.len(), MAX_TARGETS);
        assert!(snapshot.truncated());
        assert_eq!(snapshot.hit_test(Point::new(10, 0)), None);
        let nested = TargetSnapshot::from_rectangles(vec![Rect::new(-100, 0, 200, 200), Rect::new(-50, 30, 30, 40)]);
        assert_eq!(nested.hit_test(Point::new(-40, 40)), Some(Rect::new(-50, 30, 30, 40)));
        static GATE: AtomicBool = AtomicBool::new(false);
        let guard = Admission::acquire(&GATE).unwrap();
        assert!(Admission::acquire(&GATE).is_none());
        drop(guard);
        assert!(Admission::acquire(&GATE).is_some());
    }
}
