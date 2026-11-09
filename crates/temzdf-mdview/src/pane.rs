//! A graphics-mode pane: where it is scrolled to on its page, its zoom,
//! its search, and the frame it shows. The page it needs (its width and
//! zoom) is the session's to keep, shared by panes that need the same.

use std::time::Instant;
use temzdf_scene::tiles::{self, Schedule, TileCache, Want};
use temzdf_scene::{Frame, ImageKey, Images, Layer, Pointer, Rect as SceneRect};

use termpix_core::input::{MouseButton, MouseEvent, MouseEventKind};
use termpix_core::scroll::ScrollConfig;
use termpix_core::viewport::{Input, Viewport};

use crate::page::{BACKGROUND, Page, Rect, STRIP, StripJob, StripKey};

/// Placement keys of painted strips (the strip cache hands them out).
pub const STRIP_LAYERS: std::ops::Range<u32> = 1..0x7000_0000;
/// Placement keys of the background under strips not painted yet (by strip).
const PLACEHOLDER_LAYERS: u32 = 0x7000_0000;
/// Placement keys of search marks (by hit).
const MARK_LAYERS: u32 = 0x8000_0000;

/// Zoom steps (`zoom-in`, `zoom-out`).
pub const ZOOMS: &[f32] = &[0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// A click: press and release this close (device pixels).
const CLICK_SLOP: f64 = 4.0;

/// What a pane asks the session for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// A link was clicked: its target as written.
    Follow(String),
    /// Ctrl and the wheel: zoom this many steps (in when positive).
    Zoom(i32),
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
    /// Where the pane is on its page (device pixels), scrolled as PDF
    /// pages are.
    pub viewport: Viewport,
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
    /// A strip of the page's background (its width), under strips not
    /// painted yet: as large as shown, as the marks.
    background: Option<(ImageKey, u32)>,
    /// The last frame showed every strip in sight painted.
    complete: bool,
}

impl MdPane {
    pub fn new(width: u32, height: u32) -> Self {
        let mut viewport = Viewport::default();
        viewport.set_view(f64::from(width), f64::from(height));
        Self {
            viewport,
            width,
            height,
            zoom: 1.0,
            search: Search::default(),
            hover: None,
            press: None,
            requests: Vec::new(),
            marks: Default::default(),
            background: None,
            complete: false,
        }
    }

    /// The page this pane needs: its width (device pixels) and zoom.
    pub fn page_key(&self) -> (u32, f32) {
        (self.width.max(1), self.zoom)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        (self.width, self.height) = (width, height);
    }

    pub fn set_scroll_config(&mut self, config: ScrollConfig) {
        self.viewport.set_config(config);
    }

    /// Where the pane is shown on its page now (device pixels).
    pub fn scroll(&self) -> f64 {
        self.viewport.y.offset()
    }

    /// Where it is going (where it is when not scrolling).
    fn destination(&self) -> f64 {
        self.viewport.y.target()
    }

    /// The scrolling limits for `page` at the pane's size.
    fn clamp(&mut self, page: &Page) {
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        self.viewport.set_sizes((w, h), (w, f64::from(page.height())));
    }

    /// Scroll by `dy` pixels, animated.
    pub fn scroll_by(&mut self, page: &Page, dy: f64, now: Instant) {
        self.clamp(page);
        self.viewport.y.scroll_by(dy, now);
    }

    /// Go to `y`, animated (`f64::MAX`: the bottom).
    pub fn glide_to(&mut self, page: &Page, y: f64, now: Instant) {
        self.clamp(page);
        let from = self.destination();
        self.viewport.y.scroll_by(y - from, now);
    }

    /// Show `y` at the top at once.
    pub fn scroll_to(&mut self, page: &Page, y: f64) {
        self.clamp(page);
        self.viewport.y.jump_to(y);
    }

    /// A screenful less a little (`n` of them, up when negative), animated.
    pub fn page_by(&mut self, page: &Page, n: f64, now: Instant) {
        let step = (f64::from(self.height) * 0.9).max(1.0);
        self.scroll_by(page, step * n, now);
    }

    pub fn is_animating(&self) -> bool {
        self.viewport.is_animating()
    }

    /// Advance the scrolling to `now`. Whether the pane moved.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.viewport.tick(now)
    }

    /// The source line at the top of the pane (where it is going: a
    /// layout done while it scrolls keeps the place it scrolls to).
    pub fn top_line(&self, page: &Page) -> usize {
        page.line_at(self.destination())
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

    /// A mouse event in pane pixels: the wheel scrolls (Ctrl asks to
    /// zoom), a click on a link asks to follow it. Whether the pane
    /// changed.
    pub fn mouse(&mut self, page: &Page, m: &MouseEvent, cell_h: u32, now: Instant) -> bool {
        let (x, y) = (f64::from(m.x), f64::from(m.y) + self.scroll());
        if m.kind.is_scroll() {
            self.clamp(page);
            return match self.viewport.input(m, 3.0 * f64::from(cell_h.max(1)), now) {
                Input::Scrolled => true,
                Input::Zoom(n) => {
                    self.requests.push(Request::Zoom(n));
                    true
                }
                Input::Ignored => false,
            };
        }
        match m.kind {
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
        let from = *self.search.from.get_or_insert(self.destination());
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
        self.clamp(page);
        self.viewport.reveal(r.y0, r.y1, None);
    }

    /// The pane's frame (pane pixels): the page's strips in sight (from
    /// `strips`, the page's painted strips shared by the panes on it; the
    /// background where one is not painted yet), the search's marks over
    /// them; and the strips to paint, in sight first, then those within a
    /// screen above and below (tagged `tag`; those being painted as
    /// wanted still). `view` tells this pane from
    /// others drawing from `strips` in one frame.
    pub fn frame(
        &mut self,
        view: u64,
        page: &mut Page,
        strips: &mut TileCache<StripKey>,
        tag: u32,
        images: &mut Images,
    ) -> (Frame, Vec<Want<StripJob>>) {
        self.clamp(page);
        strips.begin_frame(view);
        let scroll = self.scroll();
        let top = scroll.round() as u32;
        let mut layers = Vec::new();
        let screen = SceneRect::new(0, 0, self.width.max(1), self.height.max(1));
        let centre = (screen.w as f32 / 2.0, screen.h as f32 / 2.0);
        let mut work = Schedule::new();
        self.complete = true;
        let size = (page.width, page.height());
        for cell in tiles::grid((0, -(top as i32)), size, (page.width.max(1), STRIP), screen) {
            let key = StripKey { page: page.id, strip: cell.ty };
            let src = SceneRect::new(0, 0, cell.area.2, cell.area.3);
            if let Some((image, layer)) = strips.touch(key) {
                if cell.on_screen {
                    layers.push(Layer { key: layer, image, src, dst: cell.dst, z: 0 });
                }
                continue;
            }
            if cell.on_screen {
                self.complete = false;
                let image = match self.background {
                    Some((image, w)) if w == page.width => image,
                    old => {
                        if let Some((image, _)) = old {
                            images.remove(image);
                        }
                        let w = page.width.max(1);
                        let image = images.add(w, STRIP, BACKGROUND.repeat((w * STRIP) as usize));
                        self.background = Some((image, w));
                        image
                    }
                };
                layers.push(Layer { key: PLACEHOLDER_LAYERS + cell.ty, image, src, dst: cell.dst, z: 0 });
            }
            let want = if strips.is_in_flight(key) {
                Some(Want::Queued((tag, key)))
            } else {
                page.record(cell.ty, tag).map(Want::New)
            };
            if let Some(want) = want {
                let d = cell.dst;
                let dy = d.y as f32 + d.h as f32 / 2.0 - centre.1;
                let dx = d.x as f32 + d.w as f32 / 2.0 - centre.0;
                work.push(cell.on_screen, dx.hypot(dy), want);
            }
        }
        for (i, r) in self.search.hits.iter().enumerate() {
            if r.y1 < scroll || r.y0 > scroll + f64::from(self.height) {
                continue;
            }
            let current = Some(i) == self.search.current;
            let (w, h) = ((r.x1 - r.x0).ceil().max(1.0) as u32, (r.y1 - r.y0).ceil().max(1.0) as u32);
            let image = *self.marks.entry((w, h, current)).or_insert_with(|| {
                let px: [u8; 4] = if current { [255, 140, 0, 160] } else { [255, 214, 0, 110] };
                images.add(w, h, px.repeat((w * h) as usize))
            });
            layers.push(Layer {
                key: MARK_LAYERS | i as u32,
                image,
                src: SceneRect::new(0, 0, w, h),
                dst: SceneRect::new(r.x0.floor() as i32, (r.y0 - scroll).floor() as i32, w, h),
                z: 1,
            });
        }
        (Frame { layers, pointer: self.pointer(), ..Frame::default() }, work.into_sorted().collect())
    }

    /// Whether the last frame showed every strip in sight painted.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// How far down the page the pane is, in percent.
    pub fn percent(&self, page: &Page) -> u32 {
        let most = f64::from(page.height().saturating_sub(self.height));
        if most <= 0.0 { 100 } else { (self.scroll() / most * 100.0).round() as u32 }
    }

    /// The pane's own images leave the store.
    pub fn release(&mut self, images: &mut Images) {
        for (_, k) in self.marks.drain() {
            images.remove(k);
        }
        if let Some((k, _)) = self.background.take() {
            images.remove(k);
        }
    }
}
