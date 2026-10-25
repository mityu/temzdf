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
}
