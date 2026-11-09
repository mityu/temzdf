//! A graphics-mode pane: where it is scrolled to on its page, its zoom,
//! its search, and the frame it shows. The page it needs (its width and
//! zoom) is the session's to keep, shared by panes that need the same.

use temzdf_scene::{Frame, ImageKey, Images, Layer, Pointer, Rect as SceneRect};
use termpix_core::input::{MouseButton, MouseEvent, MouseEventKind, ScrollDirection, ScrollUnit};

use crate::page::{Page, Rect};

/// Zoom steps (`zoom-in`, `zoom-out`).
pub const ZOOMS: &[f32] = &[0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// A click: press and release this close (device pixels).
const CLICK_SLOP: f64 = 4.0;

/// What a pane asks the session for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// A link was clicked: its target as written.
    Follow(String),
}

#[derive(Clone, Debug, Default)]
pub struct Search {
    pub query: String,
    pub hits: Vec<Rect>,
    pub current: Option<usize>,
    /// The scroll position before a preview moved it.
    from: Option<f64>,
}

pub struct MdPane {
    /// Device pixels from the page's top.
    pub scroll: f64,
    pub width: u32,
    pub height: u32,
    pub zoom: f32,
    pub search: Search,
    hover: Option<String>,
    press: Option<(f64, f64)>,
    requests: Vec<Request>,
    /// Images of the search's marks by size and whether current: as
    /// large as they are shown (terminals scale a stretched pixel each
    /// their own way).
    marks: std::collections::HashMap<(u32, u32, bool), ImageKey>,
}

impl MdPane {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            scroll: 0.0,
            width,
            height,
            zoom: 1.0,
            search: Search::default(),
            hover: None,
            press: None,
            requests: Vec::new(),
            marks: Default::default(),
        }
    }

    /// The page this pane needs: its width (device pixels) and zoom.
    pub fn page_key(&self) -> (u32, f32) {
        (self.width.max(1), self.zoom)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        (self.width, self.height) = (width, height);
    }

    fn clamp(&mut self, page: &Page) {
        let most = f64::from(page.height().saturating_sub(self.height));
        self.scroll = self.scroll.clamp(0.0, most.max(0.0));
    }

    pub fn scroll_by(&mut self, page: &Page, dy: f64) {
        self.scroll += dy;
        self.clamp(page);
    }

    pub fn scroll_to(&mut self, page: &Page, y: f64) {
        self.scroll = y;
        self.clamp(page);
    }

    /// A screenful less a little (`n` of them, up when negative).
    pub fn page_by(&mut self, page: &Page, n: f64) {
        let step = (f64::from(self.height) * 0.9).max(1.0);
        self.scroll_by(page, step * n);
    }

    /// The source line at the top of the pane.
    pub fn top_line(&self, page: &Page) -> usize {
        page.line_at(self.scroll)
    }

    /// Show source line `line` at the top.
    pub fn show_line(&mut self, page: &Page, line: usize) {
        self.scroll_to(page, page.y_of_line(line));
    }

    /// Show the heading `anchor` names at the top.
    pub fn show_anchor(&mut self, page: &Page, anchor: &str) -> bool {
        match page.anchor_y(anchor) {
            Some(y) => {
                self.scroll_to(page, y);
                true
            }
            None => false,
        }
    }

    /// Zoom one step in (`dir` > 0), out (< 0), or back to 1 (0).
    pub fn zoom_step(&mut self, dir: i32) {
        self.zoom = match dir {
            0 => 1.0,
            d if d > 0 => ZOOMS.iter().copied().find(|z| *z > self.zoom + 1e-3).unwrap_or(self.zoom),
            _ => ZOOMS.iter().rev().copied().find(|z| *z < self.zoom - 1e-3).unwrap_or(self.zoom),
        };
    }

    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// The pointer over the pane: a link's shape over a link.
    pub fn pointer(&self) -> Pointer {
        if self.hover.is_some() { Pointer::Link } else { Pointer::Default }
    }

    /// The link under the pointer, for the status line.
    pub fn hovered(&self) -> Option<&str> {
        self.hover.as_deref()
    }

    /// A mouse event in pane pixels: the wheel scrolls, a click on a link
    /// asks to follow it. Whether the pane changed.
    pub fn mouse(&mut self, page: &Page, m: &MouseEvent, cell_h: u32) -> bool {
        let (x, y) = (f64::from(m.x), f64::from(m.y) + self.scroll);
        match m.kind {
            MouseEventKind::Scroll(d @ (ScrollDirection::Up | ScrollDirection::Down)) => {
                let step = f64::from(cell_h.max(1)) * 3.0;
                self.scroll_by(page, if d == ScrollDirection::Up { -step } else { step });
                true
            }
            MouseEventKind::ScrollBy { dy, unit, .. } if dy != 0 => {
                let px = match unit {
                    ScrollUnit::Line => f64::from(dy) / 100.0 * f64::from(cell_h.max(1)) * 3.0,
                    ScrollUnit::Pixel => f64::from(dy) / 100.0,
                };
                self.scroll_by(page, px);
                true
            }
            MouseEventKind::Press(MouseButton::Left) => {
                self.press = Some((x, y));
                false
            }
            MouseEventKind::Release(MouseButton::Left) => {
                if let Some((px, py)) = self.press.take()
                    && (px - x).abs() <= CLICK_SLOP
                    && (py - y).abs() <= CLICK_SLOP
                    && let Some(href) = page.link_at(x, y)
                {
                    self.requests.push(Request::Follow(href));
                    return true;
                }
                false
            }
            MouseEventKind::Move | MouseEventKind::Drag(_) => {
                let hover = page.link_at(x, y);
                let changed = hover != self.hover;
                self.hover = hover;
                changed
            }
            _ => false,
        }
    }

    /// Search for `query`: its hits marked, the first from the top of the
    /// pane shown. Whether there was one.
    pub fn search_for(&mut self, page: &Page, query: &str) -> bool {
        let found = self.preview(page, query);
        self.search.from = None;
        found
    }

    /// The first hit of `query` shown while it is typed (back to where it
    /// began without one).
    pub fn preview(&mut self, page: &Page, query: &str) -> bool {
        let from = *self.search.from.get_or_insert(self.scroll);
        let hits = page.find(query);
        let current = hits.iter().position(|r| r.y0 >= from).or((!hits.is_empty()).then_some(0));
        self.search = Search { query: query.to_owned(), hits, current, from: Some(from) };
        match current {
            Some(i) => self.reveal(page, i),
            None => self.scroll_to(page, from),
        }
        current.is_some()
    }

    /// Back to where the search began, its marks gone (their images go
    /// with `release`, or the next search).
    pub fn search_cancel(&mut self, page: &Page) {
        if let Some(from) = self.search.from {
            self.scroll_to(page, from);
        }
        self.search = Search::default();
    }

    /// The next hit (`dir` > 0) or the one before; around the ends.
    pub fn search_next(&mut self, page: &Page, dir: i32) -> bool {
        let n = self.search.hits.len();
        if n == 0 {
            return false;
        }
        let i = match self.search.current {
            Some(i) if dir >= 0 => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        };
        self.search.current = Some(i);
        self.reveal(page, i);
        true
    }

    /// Hit `i` in sight (a third from the top when it was not).
    fn reveal(&mut self, page: &Page, i: usize) {
        let r = self.search.hits[i];
        if r.y0 < self.scroll || r.y1 > self.scroll + f64::from(self.height) {
            self.scroll_to(page, r.y0 - f64::from(self.height) / 3.0);
        }
    }

    /// The pane's frame (pane pixels): the page's strips in sight, the
    /// search's marks over them.
    pub fn frame(&mut self, page: &mut Page, images: &mut Images) -> Frame {
        self.clamp(page);
        let top = self.scroll.round() as u32;
        let mut layers = Vec::new();
        for (y, key) in page.strips(top, self.height, images) {
            let rows = crate::page::STRIP.min(page.height() - y);
            layers.push(Layer {
                key: y / crate::page::STRIP,
                image: key,
                src: SceneRect::new(0, 0, page.width, rows),
                dst: SceneRect::new(0, y as i32 - top as i32, page.width, rows),
                z: 0,
            });
        }
        for (i, r) in self.search.hits.iter().enumerate() {
            if r.y1 < self.scroll || r.y0 > self.scroll + f64::from(self.height) {
                continue;
            }
            let current = Some(i) == self.search.current;
            let (w, h) = ((r.x1 - r.x0).ceil().max(1.0) as u32, (r.y1 - r.y0).ceil().max(1.0) as u32);
            let image = *self.marks.entry((w, h, current)).or_insert_with(|| {
                let px: [u8; 4] = if current { [255, 140, 0, 160] } else { [255, 214, 0, 110] };
                images.add(w, h, px.repeat((w * h) as usize))
            });
            layers.push(Layer {
                key: 0x8000_0000 | i as u32,
                image,
                src: SceneRect::new(0, 0, w, h),
                dst: SceneRect::new(r.x0.floor() as i32, (r.y0 - self.scroll).floor() as i32, w, h),
                z: 1,
            });
        }
        Frame { layers, pointer: self.pointer(), ..Frame::default() }
    }

    /// How far down the page the pane is, in percent.
    pub fn percent(&self, page: &Page) -> u32 {
        let most = f64::from(page.height().saturating_sub(self.height));
        if most <= 0.0 { 100 } else { (self.scroll / most * 100.0).round() as u32 }
    }

    /// The pane's own images leave the store.
    pub fn release(&mut self, images: &mut Images) {
        for (_, k) in self.marks.drain() {
            images.remove(k);
        }
    }
}
