//! Pane layout: a binary tree of splits over the screen, in cells. Panes
//! side by side or stacked are separated by one cell for the border.

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
    Split { split: Split, a: Box<Node>, b: Box<Node> },
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

    /// Each pane's rectangle in `area` (cells), halves split evenly with a
    /// one-cell border between them.
    pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        fn walk(n: &Node, r: Rect, out: &mut Vec<(PaneId, Rect)>) {
            match n {
                Node::Leaf(p) => out.push((*p, r)),
                Node::Split { split, a, b } => {
                    let (ra, rb) = halves(*split, r);
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
        let Some(r) = self.rects(area).into_iter().find(|(p, _)| *p == pane).map(|(_, r)| r) else { return false };
        let (a, b) = halves(split, r);
        let fits = |r: Rect| r.w >= MIN_COLS && r.h >= MIN_ROWS;
        if !fits(a) || !fits(b) {
            return false;
        }
        fn walk(n: &mut Node, pane: PaneId, new: PaneId, split: Split) -> bool {
            match n {
                Node::Leaf(p) if *p == pane => {
                    let old = std::mem::replace(n, Node::Leaf(pane));
                    *n = Node::Split { split, a: Box::new(old), b: Box::new(Node::Leaf(new)) };
                    true
                }
                Node::Leaf(_) => false,
                Node::Split { a, b, .. } => walk(a, pane, new, split) || walk(b, pane, new, split),
            }
        }
        walk(&mut self.root, pane, new, split)
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
    match split {
        Split::Horizontal => {
            let a = r.w.saturating_sub(1) / 2;
            let b = r.w.saturating_sub(1 + a);
            (Rect::new(r.x, r.y, a, r.h), Rect::new(r.x + a as i32 + 1, r.y, b, r.h))
        }
        Split::Vertical => {
            let a = r.h.saturating_sub(1) / 2;
            let b = r.h.saturating_sub(1 + a);
            (Rect::new(r.x, r.y, r.w, a), Rect::new(r.x, r.y + a as i32 + 1, r.w, b))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
