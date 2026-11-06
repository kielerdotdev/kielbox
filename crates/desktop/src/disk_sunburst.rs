//! Native radial disk map. Directory enumeration and byte aggregation happen
//! elsewhere; drawing and hit testing inspect only a bounded slice of the tree.
use gpui::{prelude::*, *};
use ::toolkit::disk_usage::DiskUsage;
use std::{
    cell::Cell,
    collections::VecDeque,
    f64::consts::{FRAC_PI_2, TAU},
    rc::Rc,
    sync::Arc,
};

const RINGS: usize = 5;
const MAX_SECTORS: usize = 512;
const MAX_CHILDREN: usize = 64;

#[derive(Clone, Copy, Debug)]
struct Sector {
    id: usize,
    branch: usize,
    ring: usize,
    start: f64,
    end: f64,
}

#[derive(Clone, Copy)]
struct Pending<'a> {
    id: usize,
    bytes: u64,
    children: &'a [usize],
    ring: usize,
    branch: usize,
    start: f64,
    end: f64,
}

fn sectors<'a>(root: usize, node: impl Fn(usize) -> (u64, &'a [usize])) -> Vec<Sector> {
    let (bytes, children) = node(root);
    if bytes == 0 {
        return Vec::new();
    }
    let mut queue = VecDeque::from([Pending {
        id: root,
        bytes,
        children,
        ring: 0,
        branch: root,
        start: 0.0,
        end: TAU,
    }]);
    let mut output = Vec::new();
    let mut lookups = 1;
    while let Some(parent) = queue.pop_front() {
        if parent.children.is_empty() || parent.ring >= RINGS || output.len() >= MAX_SECTORS {
            continue;
        }
        // Reserve an aggregate sector before inspecting children. Every omitted
        // file retains its angular share, even for million-entry directories.
        let inspect = parent
            .children
            .len()
            .min(MAX_CHILDREN)
            .min(MAX_SECTORS - output.len() - 1)
            .min(MAX_SECTORS - lookups);
        let mut children = Vec::with_capacity(inspect + 1);
        let mut inspected_bytes = 0u64;
        for &id in &parent.children[..inspect] {
            let (bytes, descendants) = node(id);
            lookups += 1;
            inspected_bytes = inspected_bytes.saturating_add(bytes);
            if bytes > 0 {
                children.push(Pending {
                    id,
                    bytes,
                    children: descendants,
                    ring: parent.ring + 1,
                    branch: if parent.ring == 0 { id } else { parent.branch },
                    start: 0.0,
                    end: 0.0,
                });
            }
        }
        if inspect < parent.children.len() {
            let remainder = parent.bytes.saturating_sub(inspected_bytes);
            if remainder > 0 {
                children.push(Pending {
                    id: parent.id,
                    bytes: remainder,
                    children: &[],
                    ring: parent.ring + 1,
                    branch: parent.branch,
                    start: 0.0,
                    end: 0.0,
                });
            }
        }
        let total: f64 = children.iter().map(|child| child.bytes as f64).sum();
        let count = children.len();
        let mut angle = parent.start;
        for (index, mut child) in children.into_iter().enumerate() {
            child.start = angle;
            child.end = if index + 1 == count {
                parent.end
            } else {
                angle + (parent.end - parent.start) * child.bytes as f64 / total
            };
            output.push(Sector {
                id: child.id,
                branch: child.branch,
                ring: parent.ring,
                start: child.start,
                end: child.end,
            });
            angle = child.end;
            if !child.children.is_empty() {
                queue.push_back(child);
            }
        }
    }
    output
}

fn ring_count(sectors: &[Sector]) -> usize {
    sectors
        .iter()
        .map(|sector| sector.ring + 1)
        .max()
        .unwrap_or(1)
        .clamp(1, RINGS)
}

fn hit(sectors: &[Sector], side: f32, x: f32, y: f32, root: usize) -> Option<usize> {
    if !side.is_finite() || side <= 0.0 || !x.is_finite() || !y.is_finite() {
        return None;
    }
    let dx = x - side * 0.5;
    let dy = y - side * 0.5;
    let radius = dx.hypot(dy);
    let outer = side * 0.48;
    let hole = side * 0.17;
    if radius < hole {
        return Some(root);
    }
    if radius >= outer {
        return None;
    }
    let ring = ((radius - hole) / ((outer - hole) / ring_count(sectors) as f32)).floor() as usize;
    let angle = (f64::from(dy).atan2(f64::from(dx)) + FRAC_PI_2).rem_euclid(TAU);
    sectors
        .iter()
        .find(|sector| sector.ring == ring && angle >= sector.start && angle < sector.end)
        .map(|sector| sector.id)
}

/// Stable root-branch swatches, also used by the adjacent ranked folder list.
pub(crate) fn branch_color(id: usize) -> u32 {
    const COLORS: [u32; 12] = [
        0x43b9a6, 0x628cde, 0xa786d5, 0xd17dad, 0xe49a61, 0xcbb85d, 0x7bbd75, 0x4ca6c9, 0x8177d2,
        0xc47980, 0x95b159, 0x58b6bd,
    ];
    COLORS[id.wrapping_mul(7) % COLORS.len()]
}

fn arc_path(
    center: Point<Pixels>,
    inner: f32,
    outer: f32,
    start: f64,
    end: f64,
) -> Option<Path<Pixels>> {
    if outer <= inner || end <= start {
        return None;
    }
    let at = |radius: f32, angle: f64| {
        point(
            center.x + px(radius * (angle - FRAC_PI_2).cos() as f32),
            center.y + px(radius * (angle - FRAC_PI_2).sin() as f32),
        )
    };
    let mut path = PathBuilder::fill();
    path.move_to(at(outer, start));
    // Two arcs handle a complete 360-degree ring as well as narrow sectors.
    let middle = (start + end) * 0.5;
    for angle in [middle, end] {
        path.arc_to(
            point(px(outer), px(outer)),
            px(0.0),
            false,
            true,
            at(outer, angle),
        );
    }
    path.line_to(at(inner, end));
    for angle in [middle, start] {
        path.arc_to(
            point(px(inner), px(inner)),
            px(0.0),
            false,
            false,
            at(inner, angle),
        );
    }
    path.close();
    path.build().ok()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render<V: 'static>(
    scan: Arc<DiskUsage>,
    root: usize,
    selected: usize,
    hovered: Option<usize>,
    side: f32,
    cx: &mut Context<V>,
    select: fn(&mut V, usize, &mut Context<V>),
    hover: fn(&mut V, Option<usize>, &mut Context<V>),
) -> AnyElement {
    let side = if side.is_finite() { side.max(1.0) } else { 1.0 };
    let geometry = Rc::new(sectors(root, |id| {
        (scan.nodes[id].bytes, scan.nodes[id].children.as_slice())
    }));
    let bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let measure = bounds.clone();
    let paint_sectors = geometry.clone();
    let move_sectors = geometry.clone();
    let click_bounds = bounds.clone();
    let hole = side * 0.17;
    let root_node = &scan.nodes[root];
    let name = if root == 0 {
        scan.root_path
            .file_name()
            .unwrap_or(scan.root_path.as_os_str())
            .to_string_lossy()
            .into_owned()
    } else {
        root_node.name.to_string_lossy().into_owned()
    };
    let parent = root_node.parent.unwrap_or(root);
    let bytes = format_bytes(root_node.bytes);
    div()
        .id("disk-sunburst")
        .relative()
        .size(px(side))
        .flex_shrink_0()
        .cursor_pointer()
        .child(
            canvas(
                move |area, _, _| measure.set(Some(area)),
                move |area, _, window, _| {
                    let center = area.origin + point(px(side * 0.5), px(side * 0.5));
                    let outer = side * 0.48;
                    let ring_width = (outer - hole) / ring_count(&paint_sectors) as f32;
                    for sector in paint_sectors.iter() {
                        let inner = hole + ring_width * sector.ring as f32;
                        let outer = inner + ring_width;
                        let angular_gap =
                            (0.7 / outer as f64).min((sector.end - sector.start) * 0.15);
                        let Some(path) = arc_path(
                            center,
                            inner + 0.5,
                            outer - 0.5,
                            sector.start + angular_gap,
                            sector.end - angular_gap,
                        ) else {
                            continue;
                        };
                        let mut color = rgb(branch_color(sector.branch));
                        let shade = 1.0 - sector.ring as f32 * 0.065;
                        color.r *= shade;
                        color.g *= shade;
                        color.b *= shade;
                        if hovered == Some(sector.id) || selected == sector.id {
                            color.r += (1.0 - color.r) * 0.30;
                            color.g += (1.0 - color.g) * 0.30;
                            color.b += (1.0 - color.b) * 0.30;
                        }
                        window.paint_path(path, color);
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .child(
            div()
                .absolute()
                .left(px(side * 0.5 - hole + 3.0))
                .top(px(side * 0.5 - hole + 3.0))
                .size(px(hole * 2.0 - 6.0))
                .rounded_full()
                .bg(rgb(0x15191c))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .child(
                    div()
                        .w(px(hole * 1.6))
                        .text_center()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(rgb(0xe8edf0))
                        .child(name),
                )
                .child(
                    div()
                        .text_size(px((side * 0.04).clamp(17., 26.)))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(0xffffff))
                        .child(bytes),
                )
                .child(div().text_size(px(10.0)).text_color(rgb(0x8d979e)).child(
                    if root_node.parent.is_some() {
                        "↑ Parent folder"
                    } else {
                        "Scan root"
                    },
                )),
        )
        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            if let Some(area) = bounds.get() {
                let point = event.position - area.origin;
                let id = hit(&move_sectors, side, point.x.into(), point.y.into(), root);
                if id != hovered {
                    hover(this, id, cx);
                }
            }
        }))
        .on_hover(cx.listener(move |this, inside: &bool, _, cx| {
            if !inside && hovered.is_some() {
                hover(this, None, cx);
            }
        }))
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            let Some(area) = click_bounds.get() else {
                return;
            };
            let point = event.position() - area.origin;
            if let Some(id) = hit(&geometry, side, point.x.into(), point.y.into(), root) {
                // Only the central hole navigates upwards; an aggregate
                // sector still selects its own directory.
                let dx = f32::from(point.x) - side * 0.5;
                let dy = f32::from(point.y) - side * 0.5;
                select(this, if dx.hypot(dy) < hole { parent } else { id }, cx);
            }
        }))
        .into_any_element()
}

fn format_bytes(bytes: u64) -> String {
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if unit == "B" {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn angles_follow_bytes_and_children_stay_within_parent() {
        let nodes: Vec<(u64, Vec<usize>)> = vec![
            (100, vec![1, 2]),
            (75, vec![3, 4]),
            (25, vec![]),
            (50, vec![]),
            (25, vec![]),
        ];
        let sectors = sectors(0, |id| (nodes[id].0, &nodes[id].1));
        assert_eq!(sectors.len(), 4);
        assert!((sectors[0].end - TAU * 0.75).abs() < 0.000001);
        let parent = sectors[0];
        for child in sectors.iter().filter(|sector| sector.ring == 1) {
            assert!(child.start >= parent.start && child.end <= parent.end);
            assert_eq!(child.branch, parent.id);
        }
    }

    #[test]
    fn aggregate_preserves_small_file_angular_share_without_enumerating_tail() {
        let children: Vec<_> = (1..=10_000).collect();
        let calls = Cell::new(0);
        let sectors = sectors(0, |id| {
            calls.set(calls.get() + 1);
            if id == 0 {
                (10_000, children.as_slice())
            } else {
                (1, &[])
            }
        });
        assert_eq!(calls.get(), 65);
        assert_eq!(sectors.len(), 65);
        let aggregate = sectors.last().unwrap();
        assert_eq!(aggregate.id, 0);
        assert!(((aggregate.end - aggregate.start) / TAU - 9936.0 / 10_000.0).abs() < 0.000001);
        assert_eq!(aggregate.end, TAU);
    }

    #[test]
    fn lookup_sector_and_depth_budgets_are_bounded() {
        let descendants: Vec<_> = (0..2000).map(|id| vec![id + 1]).collect();
        let calls = Cell::new(0);
        let map = sectors(0, |id| {
            calls.set(calls.get() + 1);
            (1, descendants[id].as_slice())
        });
        assert_eq!(map.len(), RINGS);
        assert_eq!(calls.get(), RINGS + 1);
        let broad: Vec<_> = (1..=1000).collect();
        let calls = Cell::new(0);
        let map = sectors(0, |_| {
            calls.set(calls.get() + 1);
            (1000, broad.as_slice())
        });
        assert!(map.len() <= MAX_SECTORS);
        assert!(calls.get() <= MAX_SECTORS);
    }

    #[test]
    fn hit_testing_handles_center_angles_rings_and_outside() {
        let sectors = vec![
            Sector {
                id: 1,
                branch: 1,
                ring: 0,
                start: 0.0,
                end: TAU * 0.5,
            },
            Sector {
                id: 2,
                branch: 2,
                ring: 0,
                start: TAU * 0.5,
                end: TAU,
            },
        ];
        assert_eq!(hit(&sectors, 400.0, 200.0, 200.0, 0), Some(0));
        assert_eq!(hit(&sectors, 400.0, 275.0, 200.0, 0), Some(1));
        assert_eq!(hit(&sectors, 400.0, 125.0, 200.0, 0), Some(2));
        assert_eq!(hit(&sectors, 400.0, 200.0, 20.0, 0), Some(1));
        assert_eq!(hit(&sectors, 400.0, 200.0, 0.0, 0), None);
        assert_eq!(hit(&sectors, 400.0, f32::NAN, 200.0, 0), None);
    }

    #[test]
    fn empty_tree_has_no_sectors() {
        assert!(sectors(0, |_| (0, &[])).is_empty());
    }
}
