//! The client-side pane tree for the workbench (V14 S2, issue #44).
//!
//! A pure layout structure: leaves render one content pane each (the
//! workbench browser or one terminal), splits divide space with a first/second
//! ratio. Operations — split, close, replace, geometric neighbor focus and
//! the render pass — are all pure functions over this tree, unit-tested
//! without any terminal or server. The run loop owns fetching snapshots for
//! the terminal leaves and drawing what this tree lays out; the resident
//! server holds the terminals themselves, so a closed pane's process keeps
//! running (closing is a view operation, never a stop).

use ratatui::layout::Rect;

/// One draggable split boundary (the mouse hit model for the pane grid).
/// Geometry matches [`PaneNode::render_layout_chrome`] exactly: `pos` IS
/// the shared divider cell the chrome draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitHit {
    pub axis: SplitAxis,
    /// Column of the shared divider cell (Horizontal) or row (Vertical).
    pub pos: u16,
    /// The split node's own rect — the drag's denominator: the pointer
    /// position maps into this rect to compute the new ratio.
    pub area: Rect,
    /// The grab band: the divider cell plus one cell of tolerance on each
    /// side, clamped to `area`.
    pub hit_rect: Rect,
    /// Path from the root to the split node: `true` descends into the
    /// second child. [`PaneNode::set_ratio_at_path`] consumes it.
    pub path: Vec<bool>,
}

/// The split ratio (10..=90, first-side percent) implied by placing the
/// divider at `pos`. The divider cell is the FIRST side's last cell, so
/// `first = pos - origin + 1`. `pos` is an `i32` because a drag can carry
/// it past the area edges; the result clamps into the legal band either
/// way. `None` only when the area is degenerate (zero length).
pub fn ratio_for_divider(axis: SplitAxis, pos: i32, area: Rect) -> Option<u16> {
    let (origin, length) = match axis {
        SplitAxis::Horizontal => (i32::from(area.x), i32::from(area.width)),
        SplitAxis::Vertical => (i32::from(area.y), i32::from(area.height)),
    };
    if length == 0 {
        return None;
    }
    let first = (pos - origin + 1).clamp(0, length);
    Some(((first * 100 / length) as u16).clamp(10, 90))
}

/// Hard cap on rendered panes per frame (the draw budget): a snapshot fetch
/// per terminal leaf per cycle, at most this many leaves. Splitting refuses
/// beyond the cap instead of silently degrading rendering.
pub const MAX_PANES: usize = 9;

/// What one leaf shows. The browser is unique in practice (the app only
/// ever creates one), terminals are unique by id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PaneContent {
    Browser,
    Terminal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SplitAxis {
    /// Left | right.
    Horizontal,
    /// Top / bottom.
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// First/second split ratio as a percentage for the FIRST child
/// (10..=90), so a user can never squeeze a pane to zero.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PaneNode {
    Leaf(PaneContent),
    Split {
        axis: SplitAxis,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
        ratio: u16,
    },
}

impl PaneNode {
    pub fn leaf(content: PaneContent) -> Self {
        PaneNode::Leaf(content)
    }

    pub fn split_new(first: PaneContent, axis: SplitAxis, second: PaneContent) -> Self {
        PaneNode::Split {
            axis,
            first: Box::new(PaneNode::Leaf(first)),
            second: Box::new(PaneNode::Leaf(second)),
            ratio: 50,
        }
    }

    /// Every leaf content, in first-to-last order.
    pub fn leaves(&self) -> Vec<PaneContent> {
        match self {
            PaneNode::Leaf(content) => vec![content.clone()],
            PaneNode::Split { first, second, .. } => {
                let mut all = first.leaves();
                all.extend(second.leaves());
                all
            }
        }
    }

    /// Split the leaf matching `focus`, inserting `new_leaf` as the second
    /// child. Refuses when the tree is at the pane cap or the focus leaf
    /// is absent. Returns whether anything changed.
    pub fn split(&mut self, focus: &PaneContent, axis: SplitAxis, new_leaf: PaneContent) -> bool {
        if self.leaves().len() >= MAX_PANES {
            return false;
        }
        match self {
            PaneNode::Leaf(content) if content == focus => {
                let old = PaneNode::Leaf(content.clone());
                *self = PaneNode::Split {
                    axis,
                    first: Box::new(old),
                    second: Box::new(PaneNode::Leaf(new_leaf)),
                    ratio: 50,
                };
                true
            }
            PaneNode::Leaf(_) => false,
            PaneNode::Split { first, second, .. } => {
                first.split(focus, axis, new_leaf.clone()) || second.split(focus, axis, new_leaf)
            }
        }
    }

    /// Remove the leaf matching `focus`; its parent split collapses to the
    /// surviving sibling. Returns the removed content. The root leaf is
    /// never removed (a one-pane tree has nothing to collapse into).
    pub fn close(&mut self, focus: &PaneContent) -> Option<PaneContent> {
        match self {
            PaneNode::Leaf(_) => None,
            PaneNode::Split { first, second, .. } => {
                // A leaf child of this split: collapse to the sibling.
                if matches!(**first, PaneNode::Leaf(ref c) if c == focus) {
                    let removed = first.leaves().into_iter().next()?;
                    *self = (**second).clone();
                    return Some(removed);
                }
                if matches!(**second, PaneNode::Leaf(ref c) if c == focus) {
                    let removed = second.leaves().into_iter().next()?;
                    *self = (**first).clone();
                    return Some(removed);
                }
                first.close(focus).or_else(|| second.close(focus))
            }
        }
    }

    /// Replace one leaf's content in place (e.g. pointing the focused pane
    /// at another terminal).
    pub fn replace(&mut self, focus: &PaneContent, new: PaneContent) -> bool {
        match self {
            PaneNode::Leaf(content) if content == focus => {
                *content = new;
                true
            }
            PaneNode::Leaf(_) => false,
            PaneNode::Split { first, second, .. } => {
                first.replace(focus, new.clone()) || second.replace(focus, new)
            }
        }
    }

    /// The render pass: every leaf with its rectangle, computed with the
    /// same integer math the draw loop uses (percent-of-area per split).
    pub fn render_layout(&self, area: Rect) -> Vec<(PaneContent, Rect)> {
        match self {
            PaneNode::Leaf(content) => vec![(content.clone(), area)],
            PaneNode::Split {
                axis,
                first,
                second,
                ratio,
            } => {
                let ratio = (*ratio).clamp(10, 90);
                let (first_rect, second_rect) = match axis {
                    SplitAxis::Horizontal => {
                        let first_width = area.width.saturating_mul(ratio) / 100;
                        let second_width = area.width.saturating_sub(first_width);
                        (
                            Rect {
                                width: first_width,
                                ..area
                            },
                            Rect {
                                x: area.x + first_width,
                                width: second_width,
                                ..area
                            },
                        )
                    }
                    SplitAxis::Vertical => {
                        let first_height = area.height.saturating_mul(ratio) / 100;
                        let second_height = area.height.saturating_sub(first_height);
                        (
                            Rect {
                                height: first_height,
                                ..area
                            },
                            Rect {
                                y: area.y + first_height,
                                height: second_height,
                                ..area
                            },
                        )
                    }
                };
                let mut all = first.render_layout(first_rect);
                all.extend(second.render_layout(second_rect));
                all
            }
        }
    }

    /// The chrome-reserving render pass for the herdr-style merged
    /// borders: every leaf rect INCLUDES its border cells. Sibling rects
    /// overlap by exactly one shared divider cell (the second side steals
    /// one cell from the first side's edge), so the grid renders a single
    /// line between neighbors. A single leaf stays full-bleed — chrome
    /// only exists once there is something to divide.
    pub fn render_layout_chrome(&self, area: Rect) -> Vec<(PaneContent, Rect)> {
        match self {
            PaneNode::Leaf(content) => vec![(content.clone(), area)],
            PaneNode::Split {
                axis,
                first,
                second,
                ratio,
            } => {
                let ratio = (*ratio).clamp(10, 90);
                let (first_rect, second_rect) = match axis {
                    SplitAxis::Horizontal => {
                        let first_width = area.width.saturating_mul(ratio) / 100;
                        let second_width = area.width.saturating_sub(first_width);
                        (
                            Rect {
                                width: first_width,
                                ..area
                            },
                            Rect {
                                x: area.x + first_width,
                                width: second_width,
                                ..area
                            },
                        )
                    }
                    SplitAxis::Vertical => {
                        let first_height = area.height.saturating_mul(ratio) / 100;
                        let second_height = area.height.saturating_sub(first_height);
                        (
                            Rect {
                                height: first_height,
                                ..area
                            },
                            Rect {
                                y: area.y + first_height,
                                height: second_height,
                                ..area
                            },
                        )
                    }
                };
                let mut all = first.render_layout_chrome(first_rect);
                let mut second_leaves = second.render_layout_chrome(second_rect);
                // Steal the shared divider cell from the first side's edge
                // so both sides draw their border on the SAME cells. The
                // steal is skipped when either side is degenerate.
                let stealable = match axis {
                    SplitAxis::Horizontal => {
                        first_rect.width > 0 && second_rect.width > 0 && second_rect.x > 0
                    }
                    SplitAxis::Vertical => {
                        first_rect.height > 0 && second_rect.height > 0 && second_rect.y > 0
                    }
                };
                if stealable {
                    for (_, rect) in second_leaves.iter_mut() {
                        match axis {
                            SplitAxis::Horizontal if rect.x == second_rect.x => {
                                rect.x -= 1;
                                rect.width += 1;
                            }
                            SplitAxis::Vertical if rect.y == second_rect.y => {
                                rect.y -= 1;
                                rect.height += 1;
                            }
                            _ => {}
                        }
                    }
                }
                all.extend(second_leaves);
                all
            }
        }
    }

    /// Drop terminal leaves whose id is no longer live (an attach after a
    /// restart may find fewer sessions). Uses [`Self::close`], so every
    /// removal collapses its split to the surviving sibling and the tree
    /// can never empty past the root browser. Returns how many leaves were
    /// pruned.
    pub fn prune_dead_terminals(&mut self, live: &std::collections::HashSet<String>) -> usize {
        let dead: Vec<PaneContent> = self
            .leaves()
            .into_iter()
            .filter(|content| matches!(content, PaneContent::Terminal(id) if !live.contains(id)))
            .collect();
        let mut pruned = 0;
        for content in dead {
            if self.close(&content).is_some() {
                pruned += 1;
            }
        }
        pruned
    }

    /// Every split boundary, outermost first, with the tree path to each
    /// split node. The parent is pushed before its children, so a hit test
    /// over the list resolves coinciding dividers to the outer split —
    /// the one whose band the pointer actually grabbed first.
    pub fn splits(&self, area: Rect) -> Vec<SplitHit> {
        fn walk(node: &PaneNode, area: Rect, path: Vec<bool>, out: &mut Vec<SplitHit>) {
            let PaneNode::Split {
                axis,
                first,
                second,
                ratio,
            } = node
            else {
                return;
            };
            let ratio = (*ratio).clamp(10, 90) as u32;
            let (first_len, second_len) = match axis {
                SplitAxis::Horizontal => {
                    let first_len = (area.width as u32 * ratio / 100) as u16;
                    (first_len, area.width.saturating_sub(first_len))
                }
                SplitAxis::Vertical => {
                    let first_len = (area.height as u32 * ratio / 100) as u16;
                    (first_len, area.height.saturating_sub(first_len))
                }
            };
            // A degenerate side has no divider cell between the siblings —
            // nothing to grab (the chrome skips these rects too).
            if first_len == 0 || second_len == 0 {
                return;
            }
            let (pos, hit_rect) = match axis {
                SplitAxis::Horizontal => {
                    let pos = area.x + first_len - 1;
                    let hit_x = pos.saturating_sub(1).max(area.x);
                    let hit_right = (pos + 1).min(area.x + area.width - 1);
                    (
                        pos,
                        Rect {
                            x: hit_x,
                            y: area.y,
                            width: hit_right - hit_x + 1,
                            height: area.height,
                        },
                    )
                }
                SplitAxis::Vertical => {
                    let pos = area.y + first_len - 1;
                    let hit_y = pos.saturating_sub(1).max(area.y);
                    let hit_bottom = (pos + 1).min(area.y + area.height - 1);
                    (
                        pos,
                        Rect {
                            x: area.x,
                            y: hit_y,
                            width: area.width,
                            height: hit_bottom - hit_y + 1,
                        },
                    )
                }
            };
            out.push(SplitHit {
                axis: *axis,
                pos,
                area,
                hit_rect,
                path: path.clone(),
            });
            let (first_rect, second_rect) = match axis {
                SplitAxis::Horizontal => (
                    Rect {
                        width: first_len,
                        ..area
                    },
                    Rect {
                        x: area.x + first_len,
                        width: second_len,
                        ..area
                    },
                ),
                SplitAxis::Vertical => (
                    Rect {
                        height: first_len,
                        ..area
                    },
                    Rect {
                        y: area.y + first_len,
                        height: second_len,
                        ..area
                    },
                ),
            };
            let mut first_path = path.clone();
            first_path.push(false);
            let mut second_path = path;
            second_path.push(true);
            walk(first, first_rect, first_path, out);
            walk(second, second_rect, second_path, out);
        }
        let mut out = Vec::new();
        walk(self, area, Vec::new(), &mut out);
        out
    }

    /// Set the ratio of the split at `path` (clamped 10..=90). Returns
    /// false when the path no longer resolves to a split — the tree's
    /// topology changed under a drag, which must then be cancelled.
    pub fn set_ratio_at_path(&mut self, path: &[bool], ratio: u16) -> bool {
        let ratio = ratio.clamp(10, 90);
        let mut node = self;
        for &go_second in path {
            match node {
                PaneNode::Split { first, second, .. } => {
                    node = if go_second { second } else { first };
                }
                PaneNode::Leaf(_) => return false,
            }
        }
        match node {
            PaneNode::Split { ratio: target, .. } => {
                *target = ratio;
                true
            }
            PaneNode::Leaf(_) => false,
        }
    }

    /// The geometric neighbor of the focused leaf in `direction`: among all
    /// leaves whose rect touches the focused rect across that direction's
    /// edge, the one with the largest overlap along the shared edge wins.
    pub fn neighbor(
        &self,
        area: Rect,
        focus: &PaneContent,
        direction: Direction,
    ) -> Option<PaneContent> {
        let layout = self.render_layout(area);
        let focus_rect = layout
            .iter()
            .find(|(content, _)| content == focus)
            .map(|(_, rect)| *rect)?;
        let mut best: Option<(PaneContent, u16)> = None;
        for (content, rect) in &layout {
            if content == focus {
                continue;
            }
            let overlap_along_edge = match direction {
                Direction::Left | Direction::Right => {
                    let top = focus_rect.y.max(rect.y);
                    let bottom = (focus_rect.y + focus_rect.height).min(rect.y + rect.height);
                    bottom.saturating_sub(top)
                }
                Direction::Up | Direction::Down => {
                    let left = focus_rect.x.max(rect.x);
                    let right = (focus_rect.x + focus_rect.width).min(rect.x + rect.width);
                    right.saturating_sub(left)
                }
            };
            let adjacent = match direction {
                Direction::Left => rect.x + rect.width == focus_rect.x,
                Direction::Right => focus_rect.x + focus_rect.width == rect.x,
                Direction::Up => rect.y + rect.height == focus_rect.y,
                Direction::Down => focus_rect.y + focus_rect.height == rect.y,
            };
            if adjacent && overlap_along_edge > 0 {
                let better = match &best {
                    None => true,
                    Some((_, best_overlap)) => overlap_along_edge > *best_overlap,
                };
                if better {
                    best = Some((content.clone(), overlap_along_edge));
                }
            }
        }
        best.map(|(content, _)| content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(id: &str) -> PaneContent {
        PaneContent::Terminal(id.to_string())
    }

    fn sample_tree() -> PaneNode {
        // browser | (t1 / t2)
        PaneNode::Split {
            axis: SplitAxis::Horizontal,
            first: Box::new(PaneNode::leaf(PaneContent::Browser)),
            second: Box::new(PaneNode::split_new(
                term("t1"),
                SplitAxis::Vertical,
                term("t2"),
            )),
            ratio: 50,
        }
    }

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        }
    }

    #[test]
    fn render_pass_assigns_disjoint_adjacent_rects() {
        let tree = sample_tree();
        let layout = tree.render_layout(area());
        assert_eq!(layout.len(), 3);
        let browser = layout
            .iter()
            .find(|(c, _)| *c == PaneContent::Browser)
            .unwrap()
            .1;
        assert_eq!(browser.width, 50);
        let t1 = layout.iter().find(|(c, _)| *c == term("t1")).unwrap().1;
        let t2 = layout.iter().find(|(c, _)| *c == term("t2")).unwrap().1;
        assert_eq!(t1.height, 20);
        assert_eq!(t2.height, 20);
        assert_eq!(t1.y, 0);
        assert_eq!(t2.y, 20);
        // No leaf overlaps the browser column.
        assert!(t1.x >= 50 && t2.x >= 50);
    }

    #[test]
    fn split_hit_positions_are_the_drawn_divider_cells() {
        // The hit model must agree with the chrome pass cell-for-cell: a
        // grabbed divider is exactly the line the user sees.
        let tree = sample_tree(); // browser | (t1 / t2)
        let a = area();
        let chrome = tree.render_layout_chrome(a);
        let browser = chrome
            .iter()
            .find(|(c, _)| *c == PaneContent::Browser)
            .unwrap()
            .1;
        let t1 = chrome.iter().find(|(c, _)| *c == term("t1")).unwrap().1;
        let t2 = chrome.iter().find(|(c, _)| *c == term("t2")).unwrap().1;

        let hits = tree.splits(a);
        assert_eq!(hits.len(), 2, "one hit per split node");
        let root = &hits[0];
        assert_eq!(root.path, Vec::<bool>::new());
        assert_eq!(root.axis, SplitAxis::Horizontal);
        assert_eq!(root.pos, browser.x + browser.width - 1, "root divider");
        assert_eq!(root.area, a);
        // The grab band covers the divider cell ±1, clamped to the area.
        assert!(root.hit_rect.x <= root.pos && root.pos <= root.hit_rect.x + root.hit_rect.width);
        assert!(contains_point(root.hit_rect, root.pos, 20));
        assert!(contains_point(root.hit_rect, root.pos - 1, 20));
        assert!(!contains_point(root.hit_rect, root.pos - 2, 20));

        let nested = &hits[1];
        assert_eq!(
            nested.path,
            vec![true],
            "the t1/t2 split is the second child"
        );
        assert_eq!(nested.axis, SplitAxis::Vertical);
        assert_eq!(nested.pos, t1.y + t1.height - 1, "nested divider");
        assert_eq!(nested.pos, t2.y, "t1's bottom border IS t2's top border");
        assert!(contains_point(nested.hit_rect, 60, nested.pos));
    }

    #[test]
    fn ratio_for_divider_round_trips_the_integer_geometry() {
        let a = area();
        for ratio in [10u16, 25, 50, 73, 90] {
            let mut tree = sample_tree();
            assert!(tree.set_ratio_at_path(&[], ratio));
            let pos = tree.splits(a)[0].pos;
            assert_eq!(
                ratio_for_divider(SplitAxis::Horizontal, i32::from(pos), a),
                Some(ratio),
                "pos {pos} must read back as ratio {ratio}"
            );
        }
        // Past the edges clamps into the legal band; degenerate area is None.
        assert_eq!(
            ratio_for_divider(SplitAxis::Horizontal, -50, a),
            Some(10),
            "far left clamps to the floor"
        );
        assert_eq!(
            ratio_for_divider(SplitAxis::Horizontal, 10_000, a),
            Some(90),
            "far right clamps to the ceiling"
        );
        assert_eq!(
            ratio_for_divider(SplitAxis::Horizontal, 0, Rect::new(0, 0, 0, 10)),
            None
        );
    }

    #[test]
    fn set_ratio_at_path_writes_clamps_and_reports_dead_paths() {
        let mut tree = sample_tree();
        assert!(tree.set_ratio_at_path(&[true], 30));
        match &tree {
            PaneNode::Split { second, ratio, .. } => {
                assert_eq!(*ratio, 50, "the root's own ratio is untouched");
                assert!(matches!(**second, PaneNode::Split { ratio: 30, .. }));
            }
            _ => panic!("root is a split"),
        }
        // Clamped into the never-zero band on both sides.
        assert!(tree.set_ratio_at_path(&[], 5));
        assert!(matches!(tree, PaneNode::Split { ratio: 10, .. }));
        assert!(tree.set_ratio_at_path(&[], 95));
        assert!(matches!(tree, PaneNode::Split { ratio: 90, .. }));
        // A path through a leaf is dead: the caller cancels the drag.
        assert!(
            !tree.set_ratio_at_path(&[false], 50),
            "first child is a leaf"
        );
        assert!(
            !tree.set_ratio_at_path(&[true, true, true], 50),
            "past the leaves"
        );
        let mut leaf = PaneNode::leaf(PaneContent::Browser);
        assert!(!leaf.set_ratio_at_path(&[], 50));
    }

    #[test]
    fn splits_skip_degenerate_splits_but_still_walk_live_children() {
        // A 1-wide area makes the root's first side zero: no divider to
        // grab, and no hits at all from this subtree.
        let tree = sample_tree();
        assert!(tree.splits(Rect::new(0, 0, 1, 40)).is_empty());
        // A degenerate SECOND side (ratio 90 of a 10-wide area = 9/1) still
        // has a divider; its children are walked with their real rects —
        // the nested split stays live even in a 1-cell-wide column.
        let mut narrow = sample_tree();
        assert!(narrow.set_ratio_at_path(&[], 90));
        let hits = narrow.splits(Rect::new(0, 0, 10, 40));
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].pos, 8);
        assert_eq!(hits[1].area.width, 1, "the t1/t2 column is one cell wide");
        assert_eq!(hits[1].pos, 19);
    }

    fn contains_point(rect: Rect, x: u16, y: u16) -> bool {
        x >= rect.x && y >= rect.y && x < rect.x + rect.width && y < rect.y + rect.height
    }

    #[test]
    fn chrome_layout_shares_divider_cells_for_merged_borders() {
        let tree = sample_tree();
        let layout = tree.render_layout_chrome(area());
        assert_eq!(layout.len(), 3);
        let browser = layout
            .iter()
            .find(|(c, _)| *c == PaneContent::Browser)
            .unwrap()
            .1;
        let t1 = layout.iter().find(|(c, _)| *c == term("t1")).unwrap().1;
        let t2 = layout.iter().find(|(c, _)| *c == term("t2")).unwrap().1;
        // The browser's right border column IS t1/t2's left border column:
        // one shared divider cell, not a double wall.
        assert_eq!(browser.x + browser.width - 1, t1.x);
        assert_eq!(t1.x, t2.x);
        // t1's bottom border row IS t2's top border row.
        assert_eq!(t1.y + t1.height - 1, t2.y);
        // A single leaf stays full-bleed — no chrome to draw.
        let single = PaneNode::leaf(term("solo"));
        assert_eq!(
            single.render_layout_chrome(area()),
            vec![(term("solo"), area())]
        );
    }

    #[test]
    fn split_grows_the_tree_and_respects_the_cap() {
        let mut tree = sample_tree();
        assert!(tree.split(&term("t2"), SplitAxis::Horizontal, term("t3")));
        assert_eq!(tree.leaves().len(), 4);
        // Fill to the cap, then refuse.
        for i in 4..MAX_PANES {
            assert!(tree.split(&term("t3"), SplitAxis::Vertical, term(&format!("t{i}"))));
        }
        assert_eq!(tree.leaves().len(), MAX_PANES);
        assert!(!tree.split(&term("t3"), SplitAxis::Vertical, term("overflow")));
    }

    #[test]
    fn close_collapses_to_the_sibling_and_cannot_empty_the_tree() {
        let mut tree = sample_tree();
        assert_eq!(tree.close(&term("t2")), Some(term("t2")));
        assert_eq!(tree.leaves(), vec![PaneContent::Browser, term("t1")]);
        // The root leaf is irremovable.
        let mut single = PaneNode::leaf(PaneContent::Browser);
        assert_eq!(single.close(&PaneContent::Browser), None);
    }

    #[test]
    fn neighbor_focus_follows_geometry() {
        let tree = sample_tree();
        let a = area();
        assert_eq!(
            tree.neighbor(a, &PaneContent::Browser, Direction::Right),
            Some(term("t1")),
            "browser's right neighbor is t1 (the top-right pane)"
        );
        assert_eq!(
            tree.neighbor(a, &PaneContent::Browser, Direction::Left),
            None
        );
        assert_eq!(
            tree.neighbor(a, &term("t2"), Direction::Up),
            Some(term("t1"))
        );
        assert_eq!(
            tree.neighbor(a, &term("t2"), Direction::Left),
            Some(PaneContent::Browser)
        );
    }

    #[test]
    fn prune_degrades_dead_terminals_and_collapses_browsers() {
        let mut tree = sample_tree(); // browser | (t1 / t2)
        // t1 dies; t2 survives: t1's split collapses to t2, leaving
        // browser | t2.
        let live: std::collections::HashSet<String> = ["t2".to_string()].into();
        assert_eq!(tree.prune_dead_terminals(&live), 1);
        assert_eq!(tree.leaves(), vec![PaneContent::Browser, term("t2")]);
        // Everything terminal dead: the tree degrades to a single browser
        // (the root leaf is irremovable).
        let mut tree2 = sample_tree();
        assert_eq!(
            tree2.prune_dead_terminals(&std::collections::HashSet::new()),
            2
        );
        assert_eq!(tree2.leaves(), vec![PaneContent::Browser]);
    }

    #[test]
    fn layout_tree_survives_a_json_round_trip() {
        let tree = sample_tree();
        let json = serde_json::to_string(&tree).expect("serialize");
        let back: PaneNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, tree);
    }

    #[test]
    fn replace_points_a_pane_at_another_terminal() {
        let mut tree = sample_tree();
        assert!(tree.replace(&term("t1"), term("t9")));
        assert!(tree.leaves().contains(&term("t9")));
        assert!(!tree.leaves().contains(&term("t1")));
    }
}
