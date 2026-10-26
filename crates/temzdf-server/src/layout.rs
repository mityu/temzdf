//! Pane layout: a binary tree of splits over the screen, in cells. Panes
//! side by side or stacked are separated by one cell for the border. A split
//! halves its area when made and keeps its first half's size after that: when
//! the screen changes size, the last (right or bottom) pane takes the
//! difference, as in Vim.

use termpix_core::geom::Rect;

pub type PaneId = u32;

/// How a split arranges its two halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    /// Side by side (a vertical border between them).
    Horizontal,
    /// One above the other.
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The smallest pane, in cells: splits that would go below are refused.
const MIN_COLS: u32 = 10;
const MIN_ROWS: u32 = 4;

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Leaf(PaneId),
    /// `size`: the width (side by side) or height of `a`, in cells.
    Split {
        split: Split,
        a: Box<Node>,
        b: Box<Node>,
        size: u32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    root: Node,
}

impl Layout {
    pub fn new(pane: PaneId) -> Self {
        Self { root: Node::Leaf(pane) }
    }

    /// The panes in order (left to right, top to bottom within each split).
    pub fn panes(&self) -> Vec<PaneId> {
        fn walk(n: &Node, out: &mut Vec<PaneId>) {
            match n {
                Node::Leaf(p) => out.push(*p),
                Node::Split { a, b, .. } => {
                    walk(a, out);
                    walk(b, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }

    /// Each pane's rectangle in `area` (cells), with a one-cell border
    /// between neighbours.
    pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        fn walk(n: &Node, r: Rect, out: &mut Vec<(PaneId, Rect)>) {
            match n {
                Node::Leaf(p) => out.push((*p, r)),
                Node::Split { split, a, b, size } => {
                    let (ra, rb) = parts(*split, r, *size, min_extent(b, *split));
                    walk(a, ra, out);
                    walk(b, rb, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, area, &mut out);
        out
    }

    /// Split `pane`'s area in two, `new` going right of or below it. Refused
    /// (false) when a half would be too small.
    pub fn split(&mut self, pane: PaneId, new: PaneId, split: Split, area: Rect) -> bool {
        self.split_at(Some(pane), new, split, false, area)
    }

    /// Split `target`'s area (the whole layout for `None`: the new pane gets
    /// the full width or height at that edge, as Vim's `:topleft` and
    /// `:botright`), `new` going before (left/above) or after it.
    pub fn split_at(&mut self, target: Option<PaneId>, new: PaneId, split: Split, before: bool, area: Rect) -> bool {
        let r = match target {
            Some(pane) => match self.rects(area).into_iter().find(|(p, _)| *p == pane) {
                Some((_, r)) => r,
                None => return false,
            },
            None => area,
        };
        let (a, b) = halves(split, r);
        let fits = |r: Rect| r.w >= MIN_COLS && r.h >= MIN_ROWS;
        if !fits(a) || !fits(b) {
            return false;
        }
        let size = match split {
            Split::Horizontal => a.w,
            Split::Vertical => a.h,
        };
        let join = |old: Node| {
            let (a, b) = if before { (Node::Leaf(new), old) } else { (old, Node::Leaf(new)) };
            Node::Split { split, a: Box::new(a), b: Box::new(b), size }
        };
        let Some(pane) = target else {
            let old = std::mem::replace(&mut self.root, Node::Leaf(new));
            self.root = join(old);
            return true;
        };
        fn walk(n: &mut Node, pane: PaneId, join: &dyn Fn(Node) -> Node) -> bool {
            match n {
                Node::Leaf(p) if *p == pane => {
                    let old = std::mem::replace(n, Node::Leaf(pane));
                    *n = join(old);
                    true
                }
                Node::Leaf(_) => false,
                Node::Split { a, b, .. } => walk(a, pane, join) || walk(b, pane, join),
            }
        }
        walk(&mut self.root, pane, &join)
    }

    /// Remove a pane; its sibling takes the space. False for the last pane.
    pub fn remove(&mut self, pane: PaneId) -> bool {
        fn walk(n: &mut Node, pane: PaneId) -> bool {
            let Node::Split { a, b, .. } = n else { return false };
            let keep = match (a.as_ref(), b.as_ref()) {
                (Node::Leaf(p), _) if *p == pane => Some(std::mem::replace(b.as_mut(), Node::Leaf(0))),
                (_, Node::Leaf(p)) if *p == pane => Some(std::mem::replace(a.as_mut(), Node::Leaf(0))),
                _ => None,
            };
            match keep {
                Some(k) => {
                    *n = k;
                    true
                }
                None => walk(a, pane) || walk(b, pane),
            }
        }
        walk(&mut self.root, pane)
    }

    /// The pane next to `pane` in `dir`: among those beyond its edge that
    /// overlap it across, the nearest, then the one most in line with it.
    pub fn neighbor(&self, pane: PaneId, dir: Direction, area: Rect) -> Option<PaneId> {
        let rects = self.rects(area);
        let (_, from) = *rects.iter().find(|(p, _)| *p == pane)?;
        let centre = |r: &Rect| (r.x as i64 * 2 + r.w as i64, r.y as i64 * 2 + r.h as i64);
        let c = centre(&from);
        rects
            .iter()
            .filter(|(p, _)| *p != pane)
            .filter_map(|(p, r)| {
                let overlap_x = (r.x as i64) < from.right() && r.right() > from.x as i64;
                let overlap_y = (r.y as i64) < from.bottom() && r.bottom() > from.y as i64;
                let (beyond, gap) = match dir {
                    Direction::Left => (r.right() <= from.x as i64 && overlap_y, from.x as i64 - r.right()),
                    Direction::Right => (r.x as i64 >= from.right() && overlap_y, r.x as i64 - from.right()),
                    Direction::Up => (r.bottom() <= from.y as i64 && overlap_x, from.y as i64 - r.bottom()),
                    Direction::Down => (r.y as i64 >= from.bottom() && overlap_x, r.y as i64 - from.bottom()),
                };
                let rc = centre(r);
                let off_axis = match dir {
                    Direction::Left | Direction::Right => (rc.1 - c.1).abs(),
                    Direction::Up | Direction::Down => (rc.0 - c.0).abs(),
                };
                beyond.then_some((gap, off_axis, *p))
            })
            .min()
            .map(|(_, _, p)| p)
    }

    /// The pane after `pane` in order, wrapping around.
    pub fn next(&self, pane: PaneId) -> PaneId {
        let panes = self.panes();
        let i = panes.iter().position(|p| *p == pane).unwrap_or(0);
        panes[(i + 1) % panes.len()]
    }

    /// The border at cell (`col`, `row`) and how its split arranges the panes
    /// (`Horizontal`: a vertical border, dragged sideways).
    pub fn border_at(&self, area: Rect, col: i32, row: i32) -> Option<(BorderId, Split)> {
        let inside = |r: Rect| col >= r.x && (col as i64) < r.right() && row >= r.y && (row as i64) < r.bottom();
        let mut path = Vec::new();
        let (mut n, mut r) = (&self.root, area);
        while let Node::Split { split, a, b, size } = n {
            let (ra, rb) = parts(*split, r, *size, min_extent(b, *split));
            if inside(ra) {
                (n, r) = (a, ra);
                path.push(false);
            } else if inside(rb) {
                (n, r) = (b, rb);
                path.push(true);
            } else {
                return inside(r).then_some((BorderId(path), *split));
            }
        }
        None
    }

    /// How the split of border `id` arranges the panes.
    pub fn split_of(&self, id: &BorderId) -> Option<Split> {
        match self.node(&id.0)? {
            Node::Split { split, .. } => Some(*split),
            Node::Leaf(_) => None,
        }
    }

    /// Move border `id` so that it is at cell `col` (a vertical border) or
    /// `row`, as far as the panes' minimum sizes allow. False if it did not move.
    pub fn drag_border(&mut self, id: &BorderId, col: i32, row: i32, area: Rect) -> bool {
        let Some((r, split)) = self.split_rect(&id.0, area) else { return false };
        let to = match split {
            Split::Horizontal => col - r.x,
            Split::Vertical => row - r.y,
        };
        self.set_size(&id.0, to, area)
    }

    /// Move one of `pane`'s borders across `split` by `cells` (negative: left
    /// or up): the right (bottom) border if it has one, else the left (top).
    /// False if nothing moved.
    pub fn move_border(&mut self, pane: PaneId, split: Split, cells: i32, area: Rect) -> bool {
        let Some((path, _)) = self.pane_border(pane, split) else { return false };
        let Some(current) = self.size_of(&path, area) else { return false };
        self.set_size(&path, current + cells, area)
    }

    /// Make `pane` `cells` wide (`Horizontal`) or high, moving its right
    /// (bottom) border, else its left (top) one. False if nothing changed.
    pub fn set_pane_size(&mut self, pane: PaneId, split: Split, cells: u32, area: Rect) -> bool {
        let Some((_, r)) = self.rects(area).into_iter().find(|(p, _)| *p == pane) else { return false };
        let now = match split {
            Split::Horizontal => r.w,
            Split::Vertical => r.h,
        };
        let Some((_, after)) = self.pane_border(pane, split) else { return false };
        let grow = cells as i32 - now as i32;
        self.move_border(pane, split, if after { -grow } else { grow }, area)
    }

    /// The split owning the border `pane` would move across `split`, and
    /// whether the pane is after it (the border is its left or top one).
    fn pane_border(&self, pane: PaneId, split: Split) -> Option<(Vec<bool>, bool)> {
        // The splits from the root down to the pane, with the side taken.
        fn find(n: &Node, pane: PaneId, path: &mut Vec<bool>) -> bool {
            match n {
                Node::Leaf(p) => *p == pane,
                Node::Split { a, b, .. } => {
                    for (side, child) in [(false, a), (true, b)] {
                        path.push(side);
                        if find(child, pane, path) {
                            return true;
                        }
                        path.pop();
                    }
                    false
                }
            }
        }
        let mut path = Vec::new();
        if !find(&self.root, pane, &mut path) {
            return None;
        }
        // Each split on the way, nearest first: (its path, the side taken).
        let mut splits = Vec::new();
        let mut n = &self.root;
        for (i, &side) in path.iter().enumerate() {
            let Node::Split { split: s, a, b, .. } = n else { break };
            if *s == split {
                splits.push((path[..i].to_vec(), side));
            }
            n = if side { b } else { a };
        }
        splits.iter().rev().find(|(_, side)| !side).or_else(|| splits.last()).cloned()
    }

    /// The rectangle and kind of the split at `path`.
    fn split_rect(&self, path: &[bool], area: Rect) -> Option<(Rect, Split)> {
        let (mut n, mut r) = (&self.root, area);
        for &side in path {
            let Node::Split { split, a, b, size } = n else { return None };
            let (ra, rb) = parts(*split, r, *size, min_extent(b, *split));
            (n, r) = if side { (b, rb) } else { (a, ra) };
        }
        match n {
            Node::Split { split, .. } => Some((r, *split)),
            Node::Leaf(_) => None,
        }
    }

    /// The first half's size of the split at `path`, as shown.
    fn size_of(&self, path: &[bool], area: Rect) -> Option<i32> {
        let (r, split) = self.split_rect(path, area)?;
        let Node::Split { size, b, .. } = self.node(path)? else { return None };
        let (a, _) = parts(split, r, *size, min_extent(b, split));
        Some(match split {
            Split::Horizontal => a.w as i32,
            Split::Vertical => a.h as i32,
        })
    }

    fn node(&self, path: &[bool]) -> Option<&Node> {
        let mut n = &self.root;
        for &side in path {
            let Node::Split { a, b, .. } = n else { return None };
            n = if side { b } else { a };
        }
        Some(n)
    }

    /// Set the first half's size of the split at `path` to `to`, clamped so
    /// both halves keep their panes' minimum sizes. False if it did not change.
    fn set_size(&mut self, path: &[bool], to: i32, area: Rect) -> bool {
        let Some((r, split)) = self.split_rect(path, area) else { return false };
        let Some(before) = self.size_of(path, area) else { return false };
        let mut n = &mut self.root;
        for &side in path {
            let Node::Split { a, b, .. } = n else { return false };
            n = if side { b } else { a };
        }
        let Node::Split { a, b, size, .. } = n else { return false };
        let room = match split {
            Split::Horizontal => r.w,
            Split::Vertical => r.h,
        }
        .saturating_sub(1) as i32;
        let (lo, hi) = (min_extent(a, split) as i32, room - min_extent(b, split) as i32);
        if lo > hi {
            return false;
        }
        let to = to.clamp(lo, hi);
        *size = to as u32;
        to != before
    }
}

/// A border between two panes (or groups of panes): the split it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorderId(Vec<bool>);

/// The fewest cells `n` needs across `split` (width for `Horizontal`).
fn min_extent(n: &Node, split: Split) -> u32 {
    match n {
        Node::Leaf(_) => match split {
            Split::Horizontal => MIN_COLS,
            Split::Vertical => MIN_ROWS,
        },
        Node::Split { split: s, a, b, .. } if *s == split => min_extent(a, split) + 1 + min_extent(b, split),
        Node::Split { a, b, .. } => min_extent(a, split).max(min_extent(b, split)),
    }
}

/// The two halves of `r` with a one-cell border between them.
fn halves(split: Split, r: Rect) -> (Rect, Rect) {
    let extent = match split {
        Split::Horizontal => r.w,
        Split::Vertical => r.h,
    };
    parts(split, r, extent.saturating_sub(1) / 2, min_extent(&Node::Leaf(0), split))
}

/// `r` in two with a one-cell border between them, the first `size` cells
/// wide (or high) if the second keeps its minimum `min_b`; the second takes
/// the rest.
fn parts(split: Split, r: Rect, size: u32, min_b: u32) -> (Rect, Rect) {
    match split {
        Split::Horizontal => {
            let room = r.w.saturating_sub(1);
            let a = size.min(room.saturating_sub(min_b));
            let b = room - a;
            (Rect::new(r.x, r.y, a, r.h), Rect::new(r.x + a as i32 + 1, r.y, b, r.h))
        }
        Split::Vertical => {
            let room = r.h.saturating_sub(1);
            let a = size.min(room.saturating_sub(min_b));
            let b = room - a;
            (Rect::new(r.x, r.y, r.w, a), Rect::new(r.x, r.y + a as i32 + 1, r.w, b))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_before_and_at_the_screen_edge() {
        let area = Rect::new(0, 0, 81, 41);
        let mut l = Layout::new(1);
        assert!(l.split_at(Some(1), 2, Split::Horizontal, true, area));
        assert_eq!(l.panes(), [2, 1], "left of it");
        assert!(l.split_at(None, 3, Split::Vertical, false, area));
        assert_eq!(l.panes(), [2, 1, 3]);
        let rects = l.rects(area);
        let r3 = rects.iter().find(|(p, _)| *p == 3).unwrap().1;
        assert_eq!((r3.x, r3.w), (0, 81), "full width at the bottom");
        assert!(l.split_at(None, 4, Split::Horizontal, true, area));
        let r4 = l.rects(area).into_iter().find(|(p, _)| *p == 4).unwrap().1;
        assert_eq!((r4.x, r4.y, r4.h), (0, 0, 41), "full height at the left");
    }

    const AREA: Rect = Rect::new(0, 0, 81, 31);

    #[test]
    fn splits_share_the_area_with_a_border() {
        let mut l = Layout::new(1);
        assert_eq!(l.rects(AREA), vec![(1, AREA)]);
        assert!(l.split(1, 2, Split::Horizontal, AREA));
        assert_eq!(l.rects(AREA), vec![(1, Rect::new(0, 0, 40, 31)), (2, Rect::new(41, 0, 40, 31))]);
        assert!(l.split(2, 3, Split::Vertical, AREA));
        assert_eq!(l.rects(AREA)[1..], [(2, Rect::new(41, 0, 40, 15)), (3, Rect::new(41, 16, 40, 15))]);
        assert_eq!(l.panes(), vec![1, 2, 3]);
        // Too small to split again and again.
        assert!(l.split(3, 4, Split::Vertical, AREA));
        assert!(!l.split(4, 5, Split::Vertical, AREA));
    }

    #[test]
    fn removing_gives_the_space_to_the_sibling() {
        let mut l = Layout::new(1);
        l.split(1, 2, Split::Horizontal, AREA);
        l.split(2, 3, Split::Vertical, AREA);
        assert!(l.remove(2));
        assert_eq!(l.rects(AREA)[1], (3, Rect::new(41, 0, 40, 31)));
        assert!(l.remove(1));
        assert_eq!(l.rects(AREA), vec![(3, AREA)]);
        assert!(!l.remove(3), "the last pane stays");
    }

    #[test]
    fn the_last_pane_takes_a_change_of_the_screen_size() {
        let mut l = Layout::new(1);
        l.split(1, 2, Split::Horizontal, AREA);
        l.split(2, 3, Split::Vertical, AREA);
        let big = Rect::new(0, 0, 101, 41);
        assert_eq!(
            l.rects(big),
            vec![(1, Rect::new(0, 0, 40, 41)), (2, Rect::new(41, 0, 60, 15)), (3, Rect::new(41, 16, 60, 25))],
            "the first pane keeps its width, the top right one its height"
        );
        // Too small for the sizes: the first panes give up space, down to the
        // minimum of the last.
        let small = Rect::new(0, 0, 45, 31);
        assert_eq!(l.rects(small)[0].1.w, 45 - 1 - MIN_COLS);
        // New splits halve what they split.
        l.split(1, 4, Split::Horizontal, big);
        assert_eq!(l.rects(big)[..2], [(1, Rect::new(0, 0, 19, 41)), (4, Rect::new(20, 0, 20, 41))]);
    }

    fn rect_of(l: &Layout, pane: PaneId) -> Rect {
        l.rects(AREA).into_iter().find(|(p, _)| *p == pane).unwrap().1
    }

    /// 1 | 2 over 3 (`AREA`: 81x31).
    fn three() -> Layout {
        let mut l = Layout::new(1);
        l.split(1, 2, Split::Horizontal, AREA);
        l.split(2, 3, Split::Vertical, AREA);
        l
    }

    #[test]
    fn borders_are_found_by_their_cells() {
        let l = three();
        let (vertical, s) = l.border_at(AREA, 40, 20).unwrap();
        assert_eq!(s, Split::Horizontal);
        let (horizontal, s) = l.border_at(AREA, 60, 15).unwrap();
        assert_eq!(s, Split::Vertical);
        assert_ne!(vertical, horizontal);
        assert_eq!(l.border_at(AREA, 40, 0).unwrap().0, vertical, "the whole height");
        assert!(l.border_at(AREA, 39, 20).is_none() && l.border_at(AREA, 41, 14).is_none(), "inside panes");
        assert!(l.border_at(AREA, 81, 0).is_none(), "outside");
    }

    #[test]
    fn dragging_a_border_resizes_both_sides_within_the_minimum() {
        let mut l = three();
        let (border, _) = l.border_at(AREA, 40, 20).unwrap();
        assert!(l.drag_border(&border, 30, 5, AREA));
        assert_eq!(rect_of(&l, 1), Rect::new(0, 0, 30, 31));
        assert_eq!((rect_of(&l, 2).x, rect_of(&l, 2).w), (31, 50));
        assert!(!l.drag_border(&border, 30, 9, AREA), "the same column");
        l.drag_border(&border, 2, 0, AREA);
        assert_eq!(rect_of(&l, 1).w, MIN_COLS);
        l.drag_border(&border, 200, 0, AREA);
        assert_eq!(rect_of(&l, 2).w, MIN_COLS);
        let (border, _) = l.border_at(AREA, 75, 15).unwrap();
        l.drag_border(&border, 0, 0, AREA);
        assert_eq!(rect_of(&l, 2).h, MIN_ROWS);
    }

    #[test]
    fn a_border_keeps_the_panes_beyond_nested_splits() {
        // 1 | (2 | 3): the outer border stops where 2 and 3 are both minimal.
        let mut l = Layout::new(1);
        l.split(1, 2, Split::Horizontal, AREA);
        l.split(2, 3, Split::Horizontal, AREA);
        let (outer, _) = l.border_at(AREA, 40, 0).unwrap();
        l.drag_border(&outer, 80, 0, AREA);
        assert_eq!((rect_of(&l, 2).w, rect_of(&l, 3).w), (MIN_COLS, MIN_COLS));
        assert_eq!(rect_of(&l, 1).w, 81 - 2 * (MIN_COLS + 1));
    }

    #[test]
    fn panes_move_their_right_border_else_their_left() {
        let mut l = three();
        assert!(l.move_border(1, Split::Horizontal, 5, AREA));
        assert_eq!(rect_of(&l, 1).w, 45);
        // Pane 2 has no right border: its left one moves.
        assert!(l.move_border(2, Split::Horizontal, -3, AREA));
        assert_eq!(rect_of(&l, 1).w, 42);
        // Pane 3 has no bottom border: its top one moves up.
        assert!(l.move_border(3, Split::Vertical, -5, AREA));
        assert_eq!((rect_of(&l, 2).h, rect_of(&l, 3).h), (10, 20));
        assert!(!l.move_border(1, Split::Vertical, 1, AREA), "no border that way");
        assert!(l.set_pane_size(3, Split::Vertical, 25, AREA));
        assert_eq!(rect_of(&l, 3).h, 25);
        assert!(l.set_pane_size(1, Split::Horizontal, 20, AREA));
        assert_eq!(rect_of(&l, 1).w, 20);
        assert!(!l.set_pane_size(1, Split::Horizontal, 20, AREA), "already");
    }

    #[test]
    fn neighbours_and_order() {
        let mut l = Layout::new(1);
        l.split(1, 2, Split::Horizontal, AREA);
        l.split(2, 3, Split::Vertical, AREA);
        assert_eq!(l.neighbor(1, Direction::Right, AREA), Some(2), "both are as near and as far off; the first wins");
        assert_eq!(l.neighbor(3, Direction::Left, AREA), Some(1));
        assert_eq!(l.neighbor(3, Direction::Up, AREA), Some(2));
        assert_eq!(l.neighbor(2, Direction::Down, AREA), Some(3));
        assert_eq!(l.neighbor(1, Direction::Left, AREA), None);
        assert_eq!(l.next(1), 2);
        assert_eq!(l.next(3), 1);
    }
}
