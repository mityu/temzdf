//! Small geometry types shared by the crate. All values are pixels unless noted.

/// A rectangle with an integer origin and size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(&self) -> i64 {
        self.x as i64 + self.w as i64
    }

    pub const fn bottom(&self) -> i64 {
        self.y as i64 + self.h as i64
    }

    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && (x as i64) < self.right() && y >= self.y && (y as i64) < self.bottom()
    }

    /// The overlapping part of two rectangles, if any.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 <= x0 as i64 || y1 <= y0 as i64 {
            return None;
        }
        Some(Rect::new(x0, y0, (x1 - x0 as i64) as u32, (y1 - y0 as i64) as u32))
    }

    /// This rectangle without `other`: up to four disjoint rectangles (the
    /// full-width bands above and below `other`, then the parts left and
    /// right of it), none when `other` covers it all.
    pub fn subtract(&self, other: &Rect) -> Vec<Rect> {
        let Some(cut) = self.intersect(other) else { return vec![*self] };
        let mut out = Vec::with_capacity(4);
        let span = |a: i64, b: i64| (b - a) as u32;
        if cut.y > self.y {
            out.push(Rect::new(self.x, self.y, self.w, span(self.y as i64, cut.y as i64)));
        }
        if cut.bottom() < self.bottom() {
            out.push(Rect::new(self.x, cut.bottom() as i32, self.w, span(cut.bottom(), self.bottom())));
        }
        if cut.x > self.x {
            out.push(Rect::new(self.x, cut.y, span(self.x as i64, cut.x as i64), cut.h));
        }
        if cut.right() < self.right() {
            out.push(Rect::new(cut.right() as i32, cut.y, span(cut.right(), self.right()), cut.h));
        }
        out
    }
}

/// Size of one terminal cell in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CellSize {
    pub width: u32,
    pub height: u32,
}

/// Terminal geometry: grid size and cell size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
    pub cell: CellSize,
}

impl Geometry {
    pub fn width_px(&self) -> u32 {
        self.cols as u32 * self.cell.width
    }

    pub fn height_px(&self) -> u32 {
        self.rows as u32 * self.cell.height
    }

    pub fn screen_rect(&self) -> Rect {
        Rect::new(0, 0, self.width_px(), self.height_px())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.intersect(&Rect::new(-5, 5, 10, 10)), Some(Rect::new(0, 5, 5, 5)));
        assert_eq!(a.intersect(&Rect::new(10, 0, 5, 5)), None);
    }

    #[test]
    fn subtract_leaves_disjoint_pieces_covering_the_rest() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.subtract(&Rect::new(20, 0, 5, 5)), [a], "apart");
        assert!(a.subtract(&Rect::new(-1, -1, 12, 12)).is_empty(), "covered");
        assert_eq!(
            a.subtract(&Rect::new(3, 4, 2, 3)),
            [Rect::new(0, 0, 10, 4), Rect::new(0, 7, 10, 3), Rect::new(0, 4, 3, 3), Rect::new(5, 4, 5, 3)],
            "a hole: four pieces"
        );
        // Every pixel is in exactly one piece or in the cut, for overlaps on
        // every side.
        for other in [Rect::new(-5, -5, 8, 8), Rect::new(6, 2, 10, 3), Rect::new(2, 8, 3, 9), Rect::new(0, 0, 10, 4)] {
            let pieces = a.subtract(&other);
            for y in 0..10 {
                for x in 0..10 {
                    let n = pieces.iter().filter(|p| p.contains(x, y)).count() + other.contains(x, y) as usize;
                    assert_eq!(n, 1, "({x}, {y}) minus {other:?}: {pieces:?}");
                }
            }
        }
    }
}
