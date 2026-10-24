//! Size-proportional disk usage tiles, independent of the UI toolkit.

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tile {
    pub(crate) id: usize,
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

#[derive(Debug, Default)]
pub(crate) struct NestedMap {
    /// Directory rectangles in parent-before-child order. Reserve their top
    /// 12 pixels for a label; descendant tiles stay inside a one-pixel inset.
    pub(crate) frames: Vec<Tile>,
    /// Files, or directory aggregates when more detail cannot fit the budget.
    /// An aggregate uses its directory's ID, so IDs need not be unique.
    pub(crate) leaves: Vec<Tile>,
}

/// A bounded nested map: at most 1,024 output rectangles and node lookups,
/// 128 inspected children per folder, and 12 levels of subdivision. Unvisited
/// children retain their area in a tile bearing the parent directory's ID.
/// Children should already be ordered largest first.
pub(crate) fn nested_layout<'a>(
    root: usize,
    width: f32,
    height: f32,
    node: impl Fn(usize) -> (u64, &'a [usize]),
) -> NestedMap {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return NestedMap::default();
    }
    let (bytes, children) = node(root);
    let mut builder = NestedBuilder {
        node,
        lookups: 1,
        map: NestedMap::default(),
    };
    if bytes > 0 {
        builder.visit(
            NodeInfo {
                id: root,
                bytes,
                children,
            },
            Tile {
                id: root,
                x: 0.0,
                y: 0.0,
                width,
                height,
            },
            0,
            1024,
        );
    }
    builder.map
}

#[derive(Clone, Copy)]
struct NodeInfo<'a> {
    id: usize,
    bytes: u64,
    children: &'a [usize],
}

struct NestedBuilder<'a, F: Fn(usize) -> (u64, &'a [usize])> {
    node: F,
    lookups: usize,
    map: NestedMap,
}

impl<'a, F: Fn(usize) -> (u64, &'a [usize])> NestedBuilder<'a, F> {
    fn count(&self) -> usize {
        self.map.frames.len() + self.map.leaves.len()
    }

    fn visit(&mut self, info: NodeInfo<'a>, tile: Tile, depth: usize, budget: usize) {
        if info.children.is_empty()
            || tile.width < 10.0
            || tile.height < 28.0
            || depth >= 12
            || budget < 3
            || self.lookups >= 1024
        {
            self.map.leaves.push(tile);
            return;
        }
        let start_count = self.count();
        // Reserve one rectangle for this frame and another for the unvisited
        // tail. A very large folder never enumerates every child during render.
        let inspect = info
            .children
            .len()
            .min(128)
            .min(budget - 2)
            .min(1024 - self.lookups);
        let mut children = Vec::with_capacity(inspect);
        let mut selected_bytes = 0u64;
        for &id in &info.children[..inspect] {
            let (bytes, descendants) = (self.node)(id);
            self.lookups += 1;
            selected_bytes = selected_bytes.saturating_add(bytes);
            children.push(NodeInfo {
                id,
                bytes,
                children: descendants,
            });
        }
        let mut weights: Vec<_> = children
            .iter()
            .enumerate()
            .map(|(index, child)| (index, child.bytes))
            .collect();
        if inspect < info.children.len() {
            weights.push((children.len(), info.bytes.saturating_sub(selected_bytes)));
        }
        let tiles = layout(&weights, tile.width - 2.0, tile.height - 14.0);
        if tiles.is_empty() {
            self.map.leaves.push(tile);
            return;
        }
        self.map.frames.push(tile);
        let mut remaining = tiles.len();
        for child_tile in tiles {
            remaining -= 1;
            let child_bounds = Tile {
                id: children
                    .get(child_tile.id)
                    .map_or(info.id, |child| child.id),
                x: tile.x + 1.0 + child_tile.x,
                y: tile.y + 13.0 + child_tile.y,
                width: child_tile.width,
                height: child_tile.height,
            };
            // Keep one slot for every sibling, even if an earlier subtree uses
            // the rest of the available detail budget.
            let child_budget = budget - (self.count() - start_count) - remaining;
            if let Some(child) = children.get(child_tile.id) {
                self.visit(*child, child_bounds, depth + 1, child_budget);
            } else {
                self.map.leaves.push(child_bounds);
            }
        }
    }
}

/// Lay out every nonzero item. Callers choose any aggregation or item limit.
/// Invalid or empty viewports produce no tiles.
pub(crate) fn layout(items: &[(usize, u64)], width: f32, height: f32) -> Vec<Tile> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Vec::new();
    }
    let mut items: Vec<_> = items
        .iter()
        .copied()
        .filter(|(_, size)| *size > 0)
        .collect();
    // Stable sorting also keeps equal-sized tiles from jumping between refreshes.
    items.sort_by_key(|item| std::cmp::Reverse(item.1));
    let total = items.iter().map(|(_, size)| *size as f64).sum();
    let mut tiles = Vec::with_capacity(items.len());
    partition(
        &items,
        total,
        [0.0, 0.0, width as f64, height as f64],
        &mut tiles,
    );
    tiles
}

fn partition(items: &[(usize, u64)], total: f64, bounds: [f64; 4], tiles: &mut Vec<Tile>) {
    let [x, y, width, height] = bounds;
    if items.is_empty() {
        return;
    }
    if items.len() == 1 {
        tiles.push(Tile {
            id: items[0].0,
            x: x as f32,
            y: y as f32,
            width: width as f32,
            height: height as f32,
        });
        return;
    }

    // Bisect by weight along the longest axis. Recursing on balanced groups
    // avoids long strips for large groups of similarly sized files.
    let mut split = 1;
    let mut left_weight = items[0].1 as f64;
    while split + 1 < items.len() {
        let next = left_weight + items[split].1 as f64;
        if (next - total * 0.5).abs() >= (left_weight - total * 0.5).abs() {
            break;
        }
        left_weight = next;
        split += 1;
    }
    // Sum each child independently: total - left_weight can round to zero
    // when a huge file is followed by a few one-byte files.
    let right_weight: f64 = items[split..].iter().map(|(_, size)| *size as f64).sum();
    let fraction = left_weight / (left_weight + right_weight);
    let (left_bounds, right_bounds) = if width >= height {
        let left_width = width * fraction;
        (
            [x, y, left_width, height],
            [x + left_width, y, width - left_width, height],
        )
    } else {
        let top_height = height * fraction;
        (
            [x, y, width, top_height],
            [x, y + top_height, width, height - top_height],
        )
    };
    partition(&items[..split], left_weight, left_bounds, tiles);
    partition(&items[split..], right_weight, right_bounds, tiles);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_geometry(tiles: &[Tile], width: f32, height: f32) {
        let epsilon = width.max(height) * 0.00001;
        for tile in tiles {
            for value in [tile.x, tile.y, tile.width, tile.height] {
                assert!(value.is_finite() && value >= 0.0, "{tile:?}");
            }
            assert!(tile.x + tile.width <= width + epsilon, "{tile:?}");
            assert!(tile.y + tile.height <= height + epsilon, "{tile:?}");
        }
        for (index, a) in tiles.iter().enumerate() {
            for b in &tiles[index + 1..] {
                let overlap_x = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
                let overlap_y = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
                assert!(
                    overlap_x <= epsilon || overlap_y <= epsilon,
                    "{a:?} overlaps {b:?}"
                );
            }
        }
    }

    #[test]
    fn areas_follow_weights_without_gaps_or_overlaps() {
        let items = [(4, 33), (7, 17), (9, 9), (2, 3), (8, 1)];
        for (width, height) in [(1000.0, 400.0), (150.0, 900.0), (1.0, 1.0)] {
            let tiles = layout(&items, width, height);
            assert_geometry(&tiles, width, height);
            let total: u64 = items.iter().map(|(_, size)| size).sum();
            let mut area = 0.0;
            for tile in tiles {
                let size = items.iter().find(|(id, _)| *id == tile.id).unwrap().1;
                let expected = width as f64 * height as f64 * size as f64 / total as f64;
                let actual = tile.width as f64 * tile.height as f64;
                assert!((actual - expected).abs() <= expected * 0.00001);
                area += actual;
            }
            assert!((area - width as f64 * height as f64).abs() < area * 0.00001);
        }
    }

    #[test]
    fn zeros_are_omitted_and_single_item_fills_viewport() {
        assert!(layout(&[], 100.0, 100.0).is_empty());
        assert!(layout(&[(0, 0)], 100.0, 100.0).is_empty());
        assert_eq!(
            layout(&[(0, 0), (3, 20)], 100.0, 80.0),
            vec![Tile {
                id: 3,
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 80.0,
            }]
        );
    }

    #[test]
    fn invalid_viewports_are_empty() {
        for value in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(layout(&[(1, 1)], value, 100.0).is_empty());
            assert!(layout(&[(1, 1)], 100.0, value).is_empty());
        }
    }

    #[test]
    fn extreme_weights_do_not_overflow_or_drop_items() {
        let items = [(1, u64::MAX), (2, u64::MAX), (3, 1), (4, 1)];
        let tiles = layout(&items, 1024.0, 768.0);
        assert_eq!(tiles.len(), items.len());
        assert_geometry(&tiles, 1024.0, 768.0);
    }

    #[test]
    fn large_equal_groups_are_balanced_and_not_silently_limited() {
        let items: Vec<_> = (0..1024).map(|id| (id, 1)).collect();
        let tiles = layout(&items, 1024.0, 1024.0);
        assert_eq!(tiles.len(), items.len());
        assert_geometry(&tiles, 1024.0, 1024.0);
        for tile in tiles {
            assert_eq!(tile.width, 32.0);
            assert_eq!(tile.height, 32.0);
        }
    }

    #[test]
    fn nested_subtrees_fill_content_and_preserve_header_space() {
        let nodes: Vec<(u64, Vec<usize>)> = vec![
            (100, vec![1, 2]),
            (60, vec![3, 4]),
            (40, vec![]),
            (40, vec![]),
            (20, vec![]),
        ];
        let map = nested_layout(0, 500.0, 300.0, |id| (nodes[id].0, &nodes[id].1));
        assert_eq!(
            map.frames.iter().map(|tile| tile.id).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_geometry(&map.leaves, 500.0, 300.0);
        assert_eq!(map.leaves.len(), 3);
        let decoration_area: f64 = map
            .frames
            .iter()
            .map(|tile| {
                tile.width as f64 * tile.height as f64
                    - (tile.width as f64 - 2.0) * (tile.height as f64 - 14.0)
            })
            .sum();
        let leaf_area: f64 = map
            .leaves
            .iter()
            .map(|tile| tile.width as f64 * tile.height as f64)
            .sum();
        assert!((decoration_area + leaf_area - 150_000.0).abs() < 0.05);
        for tile in &map.leaves {
            let parent = if tile.id == 2 {
                &map.frames[0]
            } else {
                &map.frames[1]
            };
            assert!(tile.y >= parent.y + 13.0);
            assert!(tile.x >= parent.x + 1.0);
            assert!(tile.x + tile.width <= parent.x + parent.width - 0.999);
        }
    }

    #[test]
    fn unvisited_children_keep_their_proportional_area() {
        let children: Vec<_> = (1..=10_000).collect();
        let calls = std::cell::Cell::new(0);
        let map = nested_layout(0, 1000.0, 1000.0, |id| {
            calls.set(calls.get() + 1);
            if id == 0 {
                (10_000, children.as_slice())
            } else {
                (1, &[])
            }
        });
        assert_eq!(calls.get(), 129);
        assert_eq!(map.frames.len(), 1);
        let aggregate = map.leaves.iter().find(|tile| tile.id == 0).unwrap();
        let fraction = aggregate.width as f64 * aggregate.height as f64 / (998.0 * 986.0);
        assert!((fraction - 9872.0 / 10_000.0).abs() < 0.00001);
        assert_geometry(&map.leaves, 1000.0, 1000.0);
    }

    #[test]
    fn nested_map_bounds_both_work_and_output() {
        let mut nodes = vec![(1024u64, Vec::new())];
        for _ in 0..32 {
            let id = nodes.len();
            nodes[0].1.push(id);
            nodes.push((32, Vec::new()));
            for _ in 0..32 {
                let leaf = nodes.len();
                nodes[id].1.push(leaf);
                nodes.push((1, Vec::new()));
            }
        }
        let calls = std::cell::Cell::new(0);
        let map = nested_layout(0, 8000.0, 8000.0, |id| {
            calls.set(calls.get() + 1);
            (nodes[id].0, &nodes[id].1)
        });
        assert!(calls.get() <= 1024);
        assert!(map.frames.len() + map.leaves.len() <= 1024);
        assert_geometry(&map.leaves, 8000.0, 8000.0);
    }

    #[test]
    fn deep_or_tiny_directories_become_aggregate_leaves() {
        let children: Vec<Vec<usize>> = (0..100).map(|id| vec![id + 1]).collect();
        let map = nested_layout(0, 1000.0, 1000.0, |id| (100, children[id].as_slice()));
        assert_eq!(map.frames.len(), 12);
        assert_eq!(map.leaves.len(), 1);
        assert_eq!(map.leaves[0].id, 12);
        let tiny = nested_layout(0, 5.0, 5.0, |id| (100, children[id].as_slice()));
        assert!(tiny.frames.is_empty());
        assert_eq!(tiny.leaves.len(), 1);
        assert_eq!(tiny.leaves[0].width, 5.0);
    }

    #[test]
    fn invalid_nested_viewport_does_not_read_nodes() {
        let map = nested_layout(0, f32::NAN, 100.0, |_| panic!("unexpected node lookup"));
        assert!(map.frames.is_empty() && map.leaves.is_empty());
    }
}
