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
                    let (ra, rb) = parts(*split, r, *size);
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
}

/// The two halves of `r` with a one-cell border between them.
fn halves(split: Split, r: Rect) -> (Rect, Rect) {
    let extent = match split {
        Split::Horizontal => r.w,
        Split::Vertical => r.h,
    };
    parts(split, r, extent.saturating_sub(1) / 2)
}

/// `r` in two with a one-cell border between them, the first `size` cells
/// wide (or high) if the second keeps its minimum; the second takes the rest.
fn parts(split: Split, r: Rect, size: u32) -> (Rect, Rect) {
    match split {
        Split::Horizontal => {
            let room = r.w.saturating_sub(1);
            let a = size.min(room.saturating_sub(MIN_COLS));
            let b = room - a;
            (Rect::new(r.x, r.y, a, r.h), Rect::new(r.x + a as i32 + 1, r.y, b, r.h))
        }
        Split::Vertical => {
            let room = r.h.saturating_sub(1);
            let a = size.min(room.saturating_sub(MIN_ROWS));
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
