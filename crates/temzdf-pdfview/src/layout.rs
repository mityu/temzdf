//! Continuous vertical page layout.
//!
//! Everything is laid out in PDF points ("doc space") and scaled uniformly by
//! the zoom factor (screen pixels per point), so layout is linear in the zoom:
//! `doc_px = doc_pt * scale`. Gaps and margins are in points too.

/// Space between pages and around the document, in points.
pub const GAP_PT: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBox {
    /// Top-left in doc space (points).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub pages: Vec<PageBox>,
    /// Size of the whole document in points, margins included.
    pub width: f32,
    pub height: f32,
}

impl Layout {
    /// Stack pages of the given sizes (points) vertically, centred horizontally.
    pub fn new(sizes: &[(f32, f32)]) -> Self {
        let max_w = sizes.iter().map(|s| s.0).fold(0.0, f32::max);
        let width = max_w + 2.0 * GAP_PT;
        let mut y = GAP_PT;
        let pages = sizes
            .iter()
            .map(|&(w, h)| {
                let b = PageBox { x: (width - w) / 2.0, y, w, h };
                y += h + GAP_PT;
                b
            })
            .collect();
        Self { pages, width, height: y }
    }

    /// Indices of pages intersecting the doc-space band `[top, bottom)`.
    pub fn pages_in(&self, top: f32, bottom: f32) -> std::ops::Range<usize> {
        let first = self.pages.partition_point(|p| p.y + p.h <= top);
        let last = self.pages.partition_point(|p| p.y < bottom);
        first..last.max(first)
    }

    /// The page containing the doc-space y, or the nearest one below it.
    pub fn page_at(&self, y: f32) -> usize {
        self.pages.partition_point(|p| p.y + p.h + GAP_PT <= y).min(self.pages.len().saturating_sub(1))
    }
}

/// Where the viewport is and how zoomed it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Screen pixels per point.
    pub scale: f32,
    /// Doc-space pixels (points * scale) at the top-left of the screen.
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub screen_w: f32,
    pub screen_h: f32,
}

impl View {
    /// Horizontal screen offset of the document: centred when narrower than the screen.
    pub fn center_x(&self, layout: &Layout) -> f32 {
        ((self.screen_w - layout.width * self.scale) / 2.0).max(0.0)
    }

    pub fn max_scroll_x(&self, layout: &Layout) -> f32 {
        (layout.width * self.scale - self.screen_w).max(0.0)
    }

    pub fn max_scroll_y(&self, layout: &Layout) -> f32 {
        (layout.height * self.scale - self.screen_h).max(0.0)
    }

    /// Screen pixel -> doc-space point.
    pub fn screen_to_doc(&self, layout: &Layout, sx: f32, sy: f32) -> (f32, f32) {
        ((sx - self.center_x(layout) + self.scroll_x) / self.scale, (sy + self.scroll_y) / self.scale)
    }

    /// Doc-space point -> screen pixel.
    pub fn doc_to_screen(&self, layout: &Layout, dx: f32, dy: f32) -> (f32, f32) {
        (dx * self.scale - self.scroll_x + self.center_x(layout), dy * self.scale - self.scroll_y)
    }

    /// Change the scale keeping the doc point under screen pixel `(sx, sy)` fixed.
    pub fn zoom_at(&mut self, layout: &Layout, new_scale: f32, sx: f32, sy: f32) {
        let (dx, dy) = self.screen_to_doc(layout, sx, sy);
        self.scale = new_scale;
        self.scroll_x = dx * new_scale + self.center_x(layout) - sx;
        self.scroll_y = dy * new_scale - sy;
        self.clamp(layout);
    }

    pub fn clamp(&mut self, layout: &Layout) {
        self.scroll_x = self.scroll_x.clamp(0.0, self.max_scroll_x(layout));
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y(layout));
    }

    /// Scale at which the widest page fills the screen width.
    pub fn fit_width_scale(layout: &Layout, screen_w: f32) -> f32 {
        if layout.width <= 0.0 { 1.0 } else { screen_w / layout.width }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::new(&[(600.0, 800.0), (400.0, 800.0), (600.0, 800.0)])
    }

    #[test]
    fn stacks_and_centres() {
        let l = layout();
        assert_eq!(l.width, 612.0);
        assert_eq!(l.pages[0], PageBox { x: 6.0, y: 6.0, w: 600.0, h: 800.0 });
        assert_eq!(l.pages[1].x, 106.0);
        assert_eq!(l.pages[1].y, 812.0);
        assert_eq!(l.height, 6.0 + 3.0 * 806.0);
    }

    #[test]
    fn visible_pages() {
        let l = layout();
        assert_eq!(l.pages_in(0.0, 10.0), 0..1);
        assert_eq!(l.pages_in(805.0, 813.0), 0..2);
        assert_eq!(l.pages_in(1700.0, 5000.0), 2..3);
        assert_eq!(l.page_at(810.0), 0);
        assert_eq!(l.page_at(813.0), 1);
        assert_eq!(l.page_at(1e9), 2);
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let l = layout();
        let mut v = View { scale: 1.0, scroll_x: 0.0, scroll_y: 500.0, screen_w: 1000.0, screen_h: 600.0 };
        let before = v.screen_to_doc(&l, 500.0, 300.0);
        v.zoom_at(&l, 2.5, 500.0, 300.0);
        let after = v.screen_to_doc(&l, 500.0, 300.0);
        assert!((before.0 - after.0).abs() < 1e-3 && (before.1 - after.1).abs() < 1e-3, "{before:?} {after:?}");
        let (sx, sy) = v.doc_to_screen(&l, after.0, after.1);
        assert!((sx - 500.0).abs() < 1e-3 && (sy - 300.0).abs() < 1e-3);
    }

    #[test]
    fn narrow_document_is_centred() {
        let l = layout();
        let v = View { scale: 1.0, scroll_x: 0.0, scroll_y: 0.0, screen_w: 1012.0, screen_h: 600.0 };
        assert_eq!(v.center_x(&l), 200.0);
        assert_eq!(v.max_scroll_x(&l), 0.0);
    }
}
