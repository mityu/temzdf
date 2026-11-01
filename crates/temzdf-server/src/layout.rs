//! Pane layout: a binary tree of splits over the screen, in cells. Panes
//! side by side or stacked are separated by one cell for the border. A split
//! halves its area when made and keeps its first half's size after that: when
//! the screen changes size, the last (right or bottom) pane takes the
//! difference, as in Vim. A pane can have a fixed width or height (Vim's
//! `winfixwidth`, `winfixheight`): it keeps it when the screen changes size
//! and when panes are equalized, and needs no more than it.

use std::collections::HashMap;

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

/// Panes with a fixed size: across which splits (`Vertical`: the height),
/// and how many cells.
type Fixed = HashMap<PaneId, (Split, u32)>;

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    root: Node,
    fixed: Fixed,
}

impl Layout {
    pub fn new(pane: PaneId) -> Self {
        Self { root: Node::Leaf(pane), fixed: Fixed::new() }
    }

    /// `pane`'s fixed size, if it has one.
    pub fn fixed(&self, pane: PaneId) -> Option<(Split, u32)> {
        self.fixed.get(&pane).copied()
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
        fn walk(n: &Node, r: Rect, fx: &Fixed, out: &mut Vec<(PaneId, Rect)>) {
            match n {
                Node::Leaf(p) => out.push((*p, r)),
                Node::Split { split, a, b, size } => {
                    let (ra, rb) = split_parts(*split, r, *size, a, b, fx);
                    walk(a, ra, fx, out);
                    walk(b, rb, fx, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, area, &self.fixed, &mut out);
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
        self.split_sized(target, new, split, before, None, area)
    }

    /// As [`Layout::split_at`], `new` `size` cells wide (`Horizontal`) or
    /// high and fixed at that when given.
    pub fn split_sized(
        &mut self,
        target: Option<PaneId>,
        new: PaneId,
        split: Split,
        before: bool,
        size: Option<u32>,
        area: Rect,
    ) -> bool {
        let r = match target {
            Some(pane) => match self.rects(area).into_iter().find(|(p, _)| *p == pane) {
                Some((_, r)) => r,
                None => return false,
            },
            None => area,
        };
        let (a, b) = halves(split, r);
        let fits = |r: Rect| r.w >= MIN_COLS && r.h >= MIN_ROWS;
        let size = match size {
            // The old pane keeps its minimum (or its own fixed size), and
            // the new one its minimum across.
            Some(n) => {
                let (extent, across, min_across) = match split {
                    Split::Horizontal => (r.w, r.h, MIN_ROWS),
                    Split::Vertical => (r.h, r.w, MIN_COLS),
                };
                if across < min_across {
                    return false;
                }
                let old = match target.and_then(|t| self.fixed.get(&t)) {
                    Some((s, f)) if *s == split => *f,
                    _ => min_extent(&Node::Leaf(PaneId::MAX), split, &self.fixed),
                };
                if n == 0 || n + 1 + old > extent {
                    return false;
                }
                self.fixed.insert(new, (split, n));
                // The first half's size (the new pane's when it goes first).
                if before { n } else { extent - 1 - n }
            }
            None if !fits(a) || !fits(b) => return false,
            None => match split {
                Split::Horizontal => a.w,
                Split::Vertical => a.h,
            },
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
        let removed = self.remove_node(pane);
        if removed {
            self.fixed.remove(&pane);
        }
        removed
    }

    fn remove_node(&mut self, pane: PaneId) -> bool {
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

    /// Put pane `new` where `old` is. False when `old` is not in the layout.
    pub fn replace(&mut self, old: PaneId, new: PaneId) -> bool {
        let replaced = self.replace_leaf(old, new);
        if replaced && let Some(f) = self.fixed.remove(&old) {
            self.fixed.insert(new, f);
        }
        replaced
    }

    fn replace_leaf(&mut self, old: PaneId, new: PaneId) -> bool {
        fn walk(n: &mut Node, old: PaneId, new: PaneId) -> bool {
            match n {
                Node::Leaf(p) if *p == old => {
                    *p = new;
                    true
                }
                Node::Leaf(_) => false,
                Node::Split { a, b, .. } => walk(a, old, new) || walk(b, old, new),
            }
        }
        walk(&mut self.root, old, new)
    }

    /// Move `pane` to an edge of the screen, the full width or height there
    /// (Vim's `<C-w>H` and friends). False when it is the only pane or there
    /// is no room (nothing changes then).
    pub fn move_to_edge(&mut self, pane: PaneId, side: Direction, area: Rect) -> bool {
        let before = self.clone();
        if !self.remove_node(pane) {
            return false;
        }
        let (split, first) = match side {
            Direction::Left => (Split::Horizontal, true),
            Direction::Right => (Split::Horizontal, false),
            Direction::Up => (Split::Vertical, true),
            Direction::Down => (Split::Vertical, false),
        };
        if self.split_at(None, pane, split, first, area) && self.settle(area) {
            return true;
        }
        *self = before;
        false
    }

    /// Give every split a size its halves fit in (each keeps its panes'
    /// smallest size), as shown on `area`; after moves that put subtrees in
    /// places sized for others. False when the layout cannot fit at all.
    fn settle(&mut self, area: Rect) -> bool {
        fn walk(n: &mut Node, r: Rect, fx: &Fixed) -> bool {
            let Node::Split { split, a, b, size } = n else {
                return r.w >= min_extent(n, Split::Horizontal, fx) && r.h >= min_extent(n, Split::Vertical, fx);
            };
            let room = match split {
                Split::Horizontal => r.w,
                Split::Vertical => r.h,
            }
            .saturating_sub(1);
            let (lo, min_b) = (min_extent(a, *split, fx), min_extent(b, *split, fx));
            if lo + min_b > room {
                return false;
            }
            *size = (*size).clamp(lo, room - min_b);
            let (ra, rb) = split_parts(*split, r, *size, a, b, fx);
            walk(a, ra, fx) && walk(b, rb, fx)
        }
        walk(&mut self.root, area, &self.fixed)
    }

    /// Rotate the panes in the row or column `pane` is in (the splits of
    /// one kind around it) `times` places, each taking the next one's place,
    /// the last the first's (`reverse`: the other way), as Vim's `<C-w>r`
    /// and `<C-w>R` with a count.
    /// The places keep their sizes. False with nothing to rotate.
    pub fn rotate(&mut self, pane: PaneId, reverse: bool, times: usize, area: Rect) -> bool {
        let before = self.clone();
        if self.rotate_places(pane, reverse, times) && self.settle(area) {
            return true;
        }
        *self = before;
        false
    }

    fn rotate_places(&mut self, pane: PaneId, reverse: bool, times: usize) -> bool {
        let Some(path) = self.path_to(pane) else { return false };
        let Some(parent) = path.len().checked_sub(1) else { return false };
        let Some(Node::Split { split, .. }) = self.node(&path[..parent]) else { return false };
        let split = *split;
        // Up through the splits of the same kind: the whole row or column.
        let mut top = parent;
        while top > 0 && matches!(self.node(&path[..top - 1]), Some(Node::Split { split: s, .. }) if *s == split) {
            top -= 1;
        }
        let mut n = &mut self.root;
        for &side in &path[..top] {
            let Node::Split { a, b, .. } = n else { return false };
            n = if side { b } else { a };
        }
        fn places<'a>(n: &'a mut Node, split: Split, out: &mut Vec<&'a mut Node>) {
            if matches!(n, Node::Split { split: s, .. } if *s == split) {
                if let Node::Split { a, b, .. } = n {
                    places(a, split, out);
                    places(b, split, out);
                }
            } else {
                out.push(n);
            }
        }
        let mut slots = Vec::new();
        places(n, split, &mut slots);
        let mut items: Vec<Node> = slots.iter_mut().map(|s| std::mem::replace(&mut **s, Node::Leaf(0))).collect();
        // `times` places on: as many times round as fits takes no time.
        let n = times % items.len();
        if reverse {
            items.rotate_left(n);
        } else {
            items.rotate_right(n);
        }
        for (slot, item) in slots.into_iter().zip(items) {
            *slot = item;
        }
        true
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
            let (ra, rb) = split_parts(*split, r, *size, a, b, &self.fixed);
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
        let path = self.path_to(pane)?;
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

    /// The sides taken from the root down to `pane` (`true`: the second half).
    fn path_to(&self, pane: PaneId) -> Option<Vec<bool>> {
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
        find(&self.root, pane, &mut path).then_some(path)
    }

    /// The rectangle and kind of the split at `path`.
    fn split_rect(&self, path: &[bool], area: Rect) -> Option<(Rect, Split)> {
        let (mut n, mut r) = (&self.root, area);
        for &side in path {
            let Node::Split { split, a, b, size } = n else { return None };
            let (ra, rb) = split_parts(*split, r, *size, a, b, &self.fixed);
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
        let Node::Split { size, a, b, .. } = self.node(path)? else { return None };
        let (a, _) = split_parts(split, r, *size, a, b, &self.fixed);
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
        // A fixed pane on either side changes its own size (one cell at least).
        let fixed_leaf = |n: &Node| match n {
            Node::Leaf(p) => self.fixed.get(p).filter(|(s, _)| *s == split).map(|_| *p),
            Node::Split { .. } => None,
        };
        let (fa, fb) = (fixed_leaf(a), fixed_leaf(b));
        let (min_a, min_b) = (
            if fa.is_some() { 1 } else { min_extent(a, split, &self.fixed) as i32 },
            if fb.is_some() { 1 } else { min_extent(b, split, &self.fixed) as i32 },
        );
        let (lo, hi) = (min_a, room - min_b);
        if lo > hi {
            return false;
        }
        let to = to.clamp(lo, hi);
        if let Some(p) = fb {
            self.fixed.insert(p, (split, (room - to) as u32));
        } else if let Some(p) = fa {
            self.fixed.insert(p, (split, to as u32));
        } else if fixed_extent(a, split, &self.fixed).is_some() || fixed_extent(b, split, &self.fixed).is_some() {
            return false;
        }
        *size = to as u32;
        to != before
    }

    /// Make the panes the same size, as Vim's `<C-w>=`: across every split
    /// (only side-by-side ones for `Some(Horizontal)`, stacked ones for
    /// `Some(Vertical)`), each half gets room for the panes in a row (or
    /// column) of it, so neighbours in a row come out equal however the
    /// splits nest. False if nothing changed.
    pub fn equalize(&mut self, only: Option<Split>, area: Rect) -> bool {
        fn walk(n: &mut Node, r: Rect, only: Option<Split>, fx: &Fixed) -> bool {
            let Node::Split { split, a, b, size } = n else { return false };
            let split = *split;
            let mut changed = false;
            let (ua, ub) = (in_a_row(a, split, fx), in_a_row(b, split, fx));
            if only.is_none_or(|o| o == split) && ua + ub > 0 {
                let extent = match split {
                    Split::Horizontal => r.w,
                    Split::Vertical => r.h,
                };
                // Units of the same size with one-cell borders between them,
                // in what fixed panes (and their borders) leave: the first
                // half ends where its units and fixed panes do.
                let (xa, xb) = (fixed_cells(a, split, fx), fixed_cells(b, split, fx));
                let free = (extent + 1).saturating_sub(xa + xb);
                let want = (free as u64 * ua as u64 * 2 / (ua + ub) as u64).div_ceil(2) as u32 + xa;
                let (lo, hi) =
                    (min_extent(a, split, fx), extent.saturating_sub(1).saturating_sub(min_extent(b, split, fx)));
                let want = want.saturating_sub(1).clamp(lo, hi.max(lo));
                changed = *size != want;
                *size = want;
            }
            let (ra, rb) = split_parts(split, r, *size, a, b, fx);
            let ca = walk(a, ra, only, fx);
            let cb = walk(b, rb, only, fx);
            changed | ca | cb
        }
        let before = self.rects(area);
        walk(&mut self.root, area, only, &self.fixed);
        self.rects(area) != before
    }

    /// Give `pane` all the width (`Horizontal`) or height the other panes'
    /// minimum sizes leave, as Vim's `<C-w>|` and `<C-w>_`: every split
    /// across that direction on the way to it leans its way. False if
    /// nothing changed.
    pub fn maximize(&mut self, pane: PaneId, split: Split, area: Rect) -> bool {
        let Some(path) = self.path_to(pane) else { return false };
        let before = self.rects(area);
        for i in 0..path.len() {
            let here = &path[..i];
            let Some((r, s)) = self.split_rect(here, area) else { continue };
            if s != split {
                continue;
            }
            let extent = match split {
                Split::Horizontal => r.w,
                Split::Vertical => r.h,
            };
            // set_size clamps to the other half's minimum.
            let to = if path[i] { 0 } else { extent as i32 };
            self.set_size(here, to, area);
        }
        self.rects(area) != before
    }
}

/// How many panes of `n` stand in a row across `split` (side by side for
/// `Horizontal`), at most: what `equalize` shares the room by.
/// (Fixed panes do not count.)
fn in_a_row(n: &Node, split: Split, fx: &Fixed) -> u32 {
    match n {
        Node::Leaf(p) if fx.get(p).is_some_and(|(s, _)| *s == split) => 0,
        Node::Leaf(_) => 1,
        Node::Split { split: s, a, b, .. } if *s == split => in_a_row(a, split, fx) + in_a_row(b, split, fx),
        Node::Split { a, b, .. } => in_a_row(a, split, fx).max(in_a_row(b, split, fx)),
    }
}

/// The cells fixed panes of `n` take across `split`, each with a border:
/// what `equalize` leaves out of the share.
fn fixed_cells(n: &Node, split: Split, fx: &Fixed) -> u32 {
    match n {
        Node::Leaf(p) => fx.get(p).filter(|(s, _)| *s == split).map_or(0, |(_, f)| f + 1),
        Node::Split { split: s, a, b, .. } if *s == split => fixed_cells(a, split, fx) + fixed_cells(b, split, fx),
        Node::Split { a, b, .. } => fixed_cells(a, split, fx).min(fixed_cells(b, split, fx)),
    }
}

/// The size of `n` across `split` when fixed panes decide it all.
fn fixed_extent(n: &Node, split: Split, fx: &Fixed) -> Option<u32> {
    match n {
        Node::Leaf(p) => fx.get(p).filter(|(s, _)| *s == split).map(|(_, f)| *f),
        Node::Split { split: s, a, b, .. } if *s == split => {
            Some(fixed_extent(a, split, fx)? + 1 + fixed_extent(b, split, fx)?)
        }
        Node::Split { a, b, .. } => Some(fixed_extent(a, split, fx)?.max(fixed_extent(b, split, fx)?)),
    }
}

/// A border between two panes (or groups of panes): the split it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorderId(Vec<bool>);

/// The fewest cells `n` needs across `split` (width for `Horizontal`): a
/// fixed pane its size.
fn min_extent(n: &Node, split: Split, fx: &Fixed) -> u32 {
    match n {
        Node::Leaf(p) => match (fx.get(p), split) {
            (Some((s, f)), _) if *s == split => *f,
            (_, Split::Horizontal) => MIN_COLS,
            (_, Split::Vertical) => MIN_ROWS,
        },
        Node::Split { split: s, a, b, .. } if *s == split => min_extent(a, split, fx) + 1 + min_extent(b, split, fx),
        Node::Split { a, b, .. } => min_extent(a, split, fx).max(min_extent(b, split, fx)),
    }
}

/// How the split of `a` and `b` divides `r`: a fixed second half keeps its
/// size (the first takes the rest), else a fixed first half, else the first
/// is `size` cells (see [`parts`]).
fn split_parts(split: Split, r: Rect, size: u32, a: &Node, b: &Node, fx: &Fixed) -> (Rect, Rect) {
    let room = match split {
        Split::Horizontal => r.w,
        Split::Vertical => r.h,
    }
    .saturating_sub(1);
    let size = match (fixed_extent(b, split, fx), fixed_extent(a, split, fx)) {
        (Some(fb), _) => room.saturating_sub(fb),
        (None, Some(fa)) => fa,
        (None, None) => size,
    };
    parts(split, r, size, if fixed_extent(b, split, fx).is_some() { 0 } else { min_extent(b, split, fx) })
}

/// The two halves of `r` with a one-cell border between them.
fn halves(split: Split, r: Rect) -> (Rect, Rect) {
    let extent = match split {
        Split::Horizontal => r.w,
        Split::Vertical => r.h,
    };
    parts(split, r, extent.saturating_sub(1) / 2, min_extent(&Node::Leaf(PaneId::MAX), split, &Fixed::new()))
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

    fn widths(l: &Layout, area: Rect) -> Vec<(PaneId, u32, u32)> {
        l.rects(area).into_iter().map(|(p, r)| (p, r.w, r.h)).collect()
    }

    #[test]
    fn a_fixed_pane_keeps_its_size_through_resizes_and_equalizing() {
        let mut l = Layout::new(1);
        assert!(l.split(1, 2, Split::Horizontal, AREA));
        // One row under pane 1, below its minimum, fixed.
        assert!(l.split_sized(Some(1), 3, Split::Vertical, false, Some(1), AREA));
        assert_eq!(widths(&l, AREA), [(1, 40, 29), (3, 40, 1), (2, 40, 31)]);
        // A taller screen: the pane above takes the difference.
        let tall = Rect::new(0, 0, 81, 41);
        assert_eq!(widths(&l, tall), [(1, 40, 39), (3, 40, 1), (2, 40, 41)]);
        assert!(l.move_border(1, Split::Horizontal, 10, AREA));
        assert!(l.equalize(None, AREA));
        assert_eq!(widths(&l, AREA), [(1, 40, 29), (3, 40, 1), (2, 40, 31)], "equalized");
        // Two panes stacked beside a stack with a fixed one share what it
        // leaves.
        let mut m = Layout::new(1);
        assert!(m.split(1, 2, Split::Vertical, AREA));
        assert!(m.split_sized(Some(2), 3, Split::Vertical, false, Some(1), AREA));
        assert!(m.move_border(1, Split::Vertical, -5, AREA));
        assert!(m.equalize(None, AREA));
        // 31 = 14 + 1 + 14 + 1 + 1
        assert_eq!(widths(&m, AREA), [(1, 81, 14), (2, 81, 14), (3, 81, 1)]);
        // Resizing it changes its fixed size; the border drags too.
        assert!(l.set_pane_size(3, Split::Vertical, 3, AREA));
        assert_eq!((l.fixed(3), widths(&l, AREA)[0]), (Some((Split::Vertical, 3)), (1, 40, 27)));
        let (id, _) = l.border_at(AREA, 5, 27).expect("the border above it");
        assert!(l.drag_border(&id, 5, 25, AREA));
        assert_eq!(l.fixed(3), Some((Split::Vertical, 5)));
        // Too big for the pane it splits: refused, nothing fixed.
        assert!(!l.split_sized(Some(2), 4, Split::Vertical, false, Some(28), AREA));
        assert_eq!(l.fixed(4), None);
        // Gone with its pane.
        assert!(l.remove(3));
        assert_eq!((l.fixed(3), widths(&l, AREA)[0]), (None, (1, 40, 31)));
    }

    #[test]
    fn equalize_shares_by_the_panes_in_a_row() {
        // 1 | 2 | 3 made by splitting 2 again: 40, 19, 20 wide.
        let mut l = Layout::new(1);
        assert!(l.split(1, 2, Split::Horizontal, AREA));
        assert!(l.split(2, 3, Split::Horizontal, AREA));
        assert!(l.split(3, 4, Split::Vertical, AREA));
        assert!(l.move_border(4, Split::Vertical, 5, AREA));
        assert!(l.equalize(Some(Split::Horizontal), AREA));
        // 81 = 26 + 1 + 27 + 1 + 26; the stack in the third column is left.
        assert_eq!(widths(&l, AREA), [(1, 26, 31), (2, 27, 31), (3, 26, 20), (4, 26, 10)]);
        assert!(l.equalize(None, AREA));
        assert_eq!(widths(&l, AREA), [(1, 26, 31), (2, 27, 31), (3, 26, 15), (4, 26, 15)]);
        assert!(!l.equalize(None, AREA), "already equal");
        // A stack beside a pane takes as much as one pane: half.
        let mut l = Layout::new(1);
        assert!(l.split(1, 2, Split::Horizontal, AREA));
        assert!(l.split(2, 3, Split::Vertical, AREA));
        assert!(l.move_border(1, Split::Horizontal, 12, AREA));
        assert!(l.equalize(None, AREA));
        assert_eq!(widths(&l, AREA), [(1, 40, 31), (2, 40, 15), (3, 40, 15)]);
    }

    #[test]
    fn maximize_leaves_the_others_their_minimum() {
        let mut l = Layout::new(1);
        assert!(l.split(1, 2, Split::Horizontal, AREA));
        assert!(l.split(2, 3, Split::Horizontal, AREA));
        assert!(l.split(1, 4, Split::Vertical, AREA));
        // The middle pane: the split before it leans right, the one after left.
        assert!(l.maximize(2, Split::Horizontal, AREA));
        assert_eq!(
            widths(&l, AREA),
            [(1, MIN_COLS, 15), (4, MIN_COLS, 15), (2, 81 - 2 * MIN_COLS - 2, 31), (3, MIN_COLS, 31)]
        );
        assert!(!l.maximize(2, Split::Horizontal, AREA), "already");
        assert!(l.maximize(4, Split::Vertical, AREA));
        assert_eq!(widths(&l, AREA)[..2], [(1, MIN_COLS, MIN_ROWS), (4, MIN_COLS, 31 - MIN_ROWS - 1)]);
        assert!(!l.maximize(3, Split::Vertical, AREA), "nothing stacked with it");
    }

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
    fn panes_move_to_an_edge_of_the_screen() {
        let mut l = three();
        assert!(l.move_to_edge(3, Direction::Left, AREA));
        assert_eq!(l.panes(), [3, 1, 2]);
        assert_eq!(rect_of(&l, 3), Rect::new(0, 0, 40, 31), "the full height at the left");
        assert!(l.move_to_edge(1, Direction::Down, AREA));
        assert_eq!(rect_of(&l, 1), Rect::new(0, 16, 81, 15), "the full width at the bottom");
        let mut one = Layout::new(1);
        assert!(!one.move_to_edge(1, Direction::Up, AREA), "the only pane");
        // No room: nothing changes.
        let small = Rect::new(0, 0, 21, 31);
        let mut l = Layout::new(1);
        l.split(1, 2, Split::Horizontal, small);
        let before = l.clone();
        assert!(!l.move_to_edge(1, Direction::Up, Rect::new(0, 0, 21, 7)));
        assert_eq!(l, before);
    }

    /// Every pane at least its smallest size.
    fn assert_min(l: &Layout, area: Rect) {
        for (p, r) in l.rects(area) {
            assert!(r.w >= MIN_COLS && r.h >= MIN_ROWS, "pane {p} is {r:?}: {:?}", l.rects(area));
        }
    }

    #[test]
    fn moves_and_rotations_keep_every_pane_at_its_smallest_size() {
        // Six side by side on 101 columns; the first to the right edge
        // used to leave a pane 6 columns wide.
        let area = Rect::new(0, 0, 101, 31);
        let mut l = Layout::new(1);
        l.split(1, 2, Split::Horizontal, area);
        l.split(1, 3, Split::Horizontal, area);
        l.split(3, 5, Split::Horizontal, area);
        l.split(2, 4, Split::Horizontal, area);
        l.split(4, 6, Split::Horizontal, area);
        assert_min(&l, area);
        assert!(l.move_to_edge(1, Direction::Right, area));
        assert_min(&l, area);
        // 1 | ((2 | 3) / 4) with 1 narrowed to 10: rotating put the column
        // into 10 columns, pane 2 0 wide.
        let mut l = Layout::new(1);
        l.split(1, 2, Split::Horizontal, AREA);
        l.split(2, 4, Split::Vertical, AREA);
        l.split(2, 3, Split::Horizontal, AREA);
        l.set_pane_size(1, Split::Horizontal, 10, AREA);
        assert_eq!(rect_of(&l, 1).w, 10);
        assert!(l.rotate(1, false, 1, AREA));
        assert_eq!(l.panes(), [2, 3, 4, 1]);
        assert_min(&l, AREA);
    }

    #[test]
    fn panes_rotate_within_their_row_and_keep_the_places_sizes() {
        // 1 | 2 | 3, the first one wider.
        let mut l = Layout::new(1);
        l.split(1, 2, Split::Horizontal, AREA);
        l.split(2, 3, Split::Horizontal, AREA);
        l.move_border(1, Split::Horizontal, 10, AREA);
        let widths = |l: &Layout| l.rects(AREA).iter().map(|(_, r)| r.w).collect::<Vec<_>>();
        let before = widths(&l);
        assert!(l.rotate(2, false, 1, AREA));
        assert_eq!(l.panes(), [3, 1, 2]);
        assert_eq!(widths(&l), before, "the places keep their sizes");
        assert!(l.rotate(2, true, 1, AREA));
        assert_eq!(l.panes(), [1, 2, 3]);
        // A column inside a row turns as one.
        let mut l = three();
        assert!(l.rotate(1, false, 1, AREA));
        assert_eq!(l.panes(), [2, 3, 1]);
        // Within the column: 2 and 3 swap.
        assert!(l.rotate(2, false, 1, AREA));
        assert_eq!(l.panes(), [3, 2, 1]);
        assert!(!Layout::new(1).rotate(1, false, 1, AREA), "nothing to rotate");
        // A count goes round: two places in a column of two is where it
        // started, and the largest count takes no time.
        let before = l.panes();
        assert!(l.rotate(2, false, 2, AREA));
        assert_eq!(l.panes(), before);
        assert!(l.rotate(2, false, usize::MAX, AREA));
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
