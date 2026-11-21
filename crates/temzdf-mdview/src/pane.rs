//! A graphics-mode pane: where it is scrolled to on its page, its zoom,
//! its magnification (a pinch's: the page shown larger, laid out as it
//! is), its search, its selection, and the frame it shows. The page it needs (its width and
//! zoom) is the session's to keep, shared by panes that need the same.

use std::time::Instant;
use temzdf_scene::tiles::{self, Schedule, TileCache, Want};
use temzdf_scene::{Frame, ImageKey, Images, Layer, Pointer, Rect as SceneRect, Swatches, theme};

use termpix_core::input::{MouseButton, MouseEvent, MouseEventKind, PinchPhase};
use termpix_core::scroll::ScrollConfig;
use termpix_core::viewport::{EdgeScroll, Input, Viewport, is_click};
use termpix_ui::StatusBar;

use crate::page::{BACKGROUND, Caret, Page, PageId, Rect, STRIP, StripJob, StripKey, TILE};

/// Placement keys of painted strips (the strip cache hands them out).
pub const STRIP_LAYERS: std::ops::Range<u32> = 1..0x7000_0000;
/// Placement keys of the background under strips not painted yet (by strip).
const PLACEHOLDER_LAYERS: u32 = 0x7000_0000;
/// Placement keys of search marks (by hit).
const MARK_LAYERS: u32 = 0x8000_0000;
/// Placement keys of the selection's boxes (by box).
const SELECTION_LAYERS: u32 = 0xC000_0000;

/// Zoom steps (`zoom-in`, `zoom-out`).
pub const ZOOMS: &[f32] = &[0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// The most a pinch magnifies.
pub const MAX_MAGNIFY: f64 = 5.0;

/// What a pane asks the session for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// A link was clicked: its target as written.
    Follow(String),
    /// Ctrl and the wheel: zoom this many steps (in when positive).
    Zoom(i32),
    /// Text was selected (released, or copied again): its text.
    Copy(String),
    /// The search found nothing (the session's notice says so).
    NotFound(String),
}

/// Text selected on a page, from `anchor` (where the drag began) to
/// `head`; on a page laid out again it is gone (carets are the page's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    page: PageId,
    anchor: Caret,
    head: Caret,
}

#[derive(Clone, Debug, Default)]
struct Search {
    query: String,
    /// Where the hits are, on the page searched (`page`).
    hits: Vec<Rect>,
    current: Option<usize>,
    /// The page the hits are on: one laid out since (a zoom, a new width,
    /// the file changed) is searched again.
    page: Option<PageId>,
    /// The scroll position before a preview moved it.
    from: Option<f64>,
}

pub struct MdPane {
    /// Where the pane is on its page magnified (device pixels), scrolled
    /// as PDF pages are.
    pub viewport: Viewport,
    pub width: u32,
    pub height: u32,
    pub zoom: f32,
    /// How much a pinch magnified the page (1 to [`MAX_MAGNIFY`]): shown
    /// larger without laying it out again, as browsers do. Separate from
    /// `zoom`, which lays it out again; it stays until pinched back.
    magnify: f64,
    /// Fingers are pinching: the strips are shown stretched, nothing is
    /// painted at the magnification until they stop.
    pinching: bool,
    search: Search,
    hover: Option<String>,
    press: Option<(f64, f64)>,
    requests: Vec<Request>,
    selection: Option<Selection>,
    /// A drag is selecting.
    selecting: bool,
    /// The pointer during a selecting drag (pane pixels), for scrolling
    /// past the edge.
    pointer: (f64, f64),
    /// Pixels a second the selecting drag scrolls (pointer past an edge),
    /// and when it last did.
    autoscroll: EdgeScroll,
    /// The selection's boxes for the selection they are of.
    selection_rects: Option<(Selection, Vec<Rect>)>,
    /// The images of the search's marks and the selection's boxes, as
    /// large as they are shown.
    swatches: Swatches,
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
            magnify: 1.0,
            pinching: false,
            search: Search::default(),
            hover: None,
            press: None,
            requests: Vec::new(),
            selection: None,
            selecting: false,
            pointer: (0.0, 0.0),
            autoscroll: EdgeScroll::default(),
            selection_rects: None,
            swatches: Swatches::default(),
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

    /// How much a pinch magnified the page.
    pub fn magnify(&self) -> f64 {
        self.magnify
    }

    /// Where the top of the pane is on its page now (device pixels of the
    /// page as laid out).
    pub fn scroll(&self) -> f64 {
        self.viewport.y.offset() / self.magnify
    }

    /// Where it is going (where it is when not scrolling).
    fn destination(&self) -> f64 {
        self.viewport.y.target() / self.magnify
    }

    /// A height on the page as laid out, on the page magnified (the
    /// bottom, `f64::MAX`, stays the bottom).
    fn magnified(&self, y: f64) -> f64 {
        (y * self.magnify).min(f64::MAX)
    }

    /// The scrolling limits for `page` at the pane's size.
    fn clamp(&mut self, page: &Page) {
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        let m = self.magnify;
        self.viewport.set_sizes((w, h), (w * m, f64::from(page.height()) * m));
    }

    /// Scroll by `dy` pixels, animated.
    pub fn scroll_by(&mut self, page: &Page, dy: f64, now: Instant) {
        self.clamp(page);
        self.viewport.y.scroll_by(dy, now);
    }

    /// Go to `y`, animated (`f64::MAX`: the bottom).
    pub fn glide_to(&mut self, page: &Page, y: f64, now: Instant) {
        self.clamp(page);
        let from = self.viewport.y.target();
        self.viewport.y.scroll_by(self.magnified(y) - from, now);
    }

    /// Show `y` at the top at once.
    pub fn scroll_to(&mut self, page: &Page, y: f64) {
        self.clamp(page);
        self.viewport.y.jump_to(self.magnified(y));
    }

    /// A screenful less a little (`n` of them, up when negative), animated.
    pub fn page_by(&mut self, page: &Page, n: f64, now: Instant) {
        let step = (f64::from(self.height) * 0.9).max(1.0);
        self.scroll_by(page, step * n, now);
    }

    pub fn is_animating(&self) -> bool {
        self.viewport.is_animating() || self.autoscroll.is_active()
    }

    /// Advance the scrolling to `now` (a selecting drag past the edge
    /// scrolls `page` and selects on). Whether the pane moved.
    pub fn tick(&mut self, page: Option<&Page>, now: Instant) -> bool {
        let mut moved = self.viewport.tick(now);
        if self.autoscroll.is_active()
            && let Some(page) = page
        {
            let by = self.autoscroll.step(now);
            self.clamp(page);
            let before = self.viewport.y.offset();
            self.viewport.y.jump_to(before + by);
            if self.viewport.y.offset() != before {
                self.extend_selection(page);
                moved = true;
            }
        }
        moved
    }

    /// The selection's head to the pointer.
    fn extend_selection(&mut self, page: &Page) {
        let (ox, oy) = self.viewport.offset();
        let (x, y) = ((self.pointer.0 + ox) / self.magnify, (self.pointer.1 + oy) / self.magnify);
        if let Some(sel) = self.selection.as_mut()
            && sel.page == page.id
            && let Some(head) = page.caret_at(x, y)
        {
            sel.head = head;
        }
    }

    /// Whether some text is selected.
    pub fn has_selection(&self) -> bool {
        self.selection.is_some_and(|s| s.anchor != s.head)
    }

    /// Copy the selection again (`y`); whether there was one.
    pub fn copy_selection(&mut self, page: &Page) -> bool {
        match self.selection {
            Some(s) if s.page == page.id && s.anchor != s.head => {
                self.requests.push(Request::Copy(page.selection_text(s.anchor, s.head)));
                true
            }
            _ => false,
        }
    }

    /// Forget the selection (`<Esc>`); whether there was one.
    pub fn clear_selection(&mut self) -> bool {
        self.selecting = false;
        self.autoscroll.stop();
        self.selection.take().is_some()
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

    /// A mouse event in pane pixels: the wheel scrolls (Ctrl asks to
    /// zoom), a pinch magnifies, a click on a link asks to follow it, a
    /// drag from text selects (past the top or bottom it scrolls) and its
    /// release asks to copy. Whether the pane changed.
    pub fn mouse(&mut self, page: &Page, m: &MouseEvent, cell_h: u32, now: Instant) -> bool {
        let (ox, oy) = self.viewport.offset();
        let (x, y) = ((f64::from(m.x) + ox) / self.magnify, (f64::from(m.y) + oy) / self.magnify);
        if let MouseEventKind::Pinch { delta, phase } = m.kind {
            return self.pinch(page, delta, phase, (f64::from(m.x), f64::from(m.y)));
        }
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
                let had = self.clear_selection();
                if page.link_at(x, y).is_none()
                    && page.has_text_at(x, y)
                    && let Some(at) = page.caret_at(x, y)
                {
                    self.selection = Some(Selection { page: page.id, anchor: at, head: at });
                    self.selecting = true;
                    self.pointer = (f64::from(m.x), f64::from(m.y));
                }
                had
            }
            MouseEventKind::Release(MouseButton::Left) => {
                if std::mem::take(&mut self.selecting) {
                    self.autoscroll.stop();
                    self.press = None;
                    if self.has_selection() {
                        self.copy_selection(page);
                    } else {
                        self.selection = None;
                    }
                    return true;
                }
                if let Some((px, py)) = self.press.take()
                    && is_click((px, py), (x, y))
                    && let Some(href) = page.link_at(x, y)
                {
                    self.requests.push(Request::Follow(href));
                    return true;
                }
                false
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selecting => {
                self.pointer = (f64::from(m.x), f64::from(m.y));
                self.autoscroll.drag(f64::from(m.y), f64::from(self.height), cell_h, now);
                let before = self.selection;
                self.extend_selection(page);
                self.selection != before
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

    /// A pinch: the magnification times `1 + delta / 10000` (kept between
    /// 1 and [`MAX_MAGNIFY`]), the page's point under `at` (pane pixels)
    /// staying there. When the fingers leave, it is rounded to hundredths
    /// (what tiles are painted at), and to 1 near it. Whether the pane
    /// changed.
    fn pinch(&mut self, page: &Page, delta: i32, phase: PinchPhase, at: (f64, f64)) -> bool {
        let was = self.pinching;
        self.pinching = phase != PinchPhase::End;
        let old = self.magnify;
        let mut new = (old * (1.0 + f64::from(delta) / 10_000.0)).clamp(1.0, MAX_MAGNIFY);
        if !self.pinching {
            new = (new * 100.0).round() / 100.0;
        }
        if new != old {
            let (ox, oy) = self.viewport.offset();
            self.magnify = new;
            self.clamp(page);
            let f = new / old;
            self.viewport.jump_to((at.0 + ox) * f - at.0, (at.1 + oy) * f - at.1);
        }
        // (The end of a pinch paints the page at its magnification.)
        new != old || (was && !self.pinching && new != 1.0)
    }

    /// Search for `query`: its hits marked, the first from the top of the
    /// pane shown. Whether there was one (else the session is asked to
    /// say so).
    pub fn search_for(&mut self, page: &Page, query: &str) -> bool {
        let found = self.preview(page, query);
        self.search.from = None;
        if !found && !query.is_empty() {
            self.requests.push(Request::NotFound(query.to_owned()));
        }
        found
    }

    /// The first hit of `query` shown while it is typed (back to where it
    /// began without one).
    pub fn preview(&mut self, page: &Page, query: &str) -> bool {
        let from = *self.search.from.get_or_insert(self.destination());
        let hits = page.find(query);
        let current = hits.iter().position(|r| r.y0 >= from).or((!hits.is_empty()).then_some(0));
        self.search = Search { query: query.to_owned(), hits, current, from: Some(from), page: Some(page.id) };
        match current {
            Some(i) => self.reveal(page, i),
            None => self.scroll_to(page, from),
        }
        current.is_some()
    }

    /// Back to where the search began, its marks no longer drawn (their
    /// images stay cached until `release`, or until too many piled up).
    pub fn search_cancel(&mut self, page: &Page) {
        if let Some(from) = self.search.from {
            self.scroll_to(page, from);
        }
        self.search = Search::default();
    }

    /// The search's hits on `page`: found again on a page laid out since
    /// the search (the current hit stays the same one, by its number).
    fn follow_search(&mut self, page: &Page) {
        if self.search.page.is_some_and(|p| p != page.id) {
            let hits = page.find(&self.search.query);
            self.search.current = self.search.current.filter(|&i| i < hits.len());
            self.search.hits = hits;
            self.search.page = Some(page.id);
        }
    }

    /// The next hit (`dir` > 0) or the one before; around the ends.
    /// Whether there was one (else the session is asked to say so, when
    /// there is a search).
    pub fn search_next(&mut self, page: &Page, dir: i32) -> bool {
        self.follow_search(page);
        let n = self.search.hits.len();
        if n == 0 {
            if !self.search.query.is_empty() {
                self.requests.push(Request::NotFound(self.search.query.clone()));
            }
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
        self.viewport.reveal(self.magnified(r.y0), self.magnified(r.y1), None);
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
        self.follow_search(page);
        // A pinch makes new sizes each step.
        self.swatches.trim(images);
        if self.magnify != 1.0 {
            return self.frame_magnified(view, page, strips, tag, images);
        }
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
            let key = StripKey::strip(page.id, cell.ty);
            let src = SceneRect::new(0, 0, cell.area.2, cell.area.3);
            if let Some((image, layer)) = strips.touch(key) {
                if cell.on_screen {
                    layers.push(Layer { key: layer, image, src, dst: cell.dst, z: 0 });
                }
                continue;
            }
            if cell.on_screen {
                self.complete = false;
                let image = self.background(page, images);
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
        self.overlays(page, 1.0, (0.0, scroll), images, &mut layers);
        (Frame { layers, pointer: self.pointer(), ..Frame::default() }, work.into_sorted().collect())
    }

    /// The search's marks and the selection's boxes in sight, onto
    /// `layers`: page pixels magnified `m` times, less `(ox, oy)`, are
    /// pane pixels.
    fn overlays(&mut self, page: &Page, m: f64, (ox, oy): (f64, f64), images: &mut Images, layers: &mut Vec<Layer>) {
        let height = f64::from(self.height);
        let place = |r: Rect| {
            let (y0, y1) = ((r.y0 * m - oy).floor(), (r.y1 * m - oy).ceil());
            if y1 < 0.0 || y0 > height {
                return None;
            }
            let (x0, x1) = ((r.x0 * m - ox).floor(), (r.x1 * m - ox).ceil());
            Some(SceneRect::new(x0 as i32, y0 as i32, (x1 - x0).max(1.0) as u32, (y1 - y0).max(1.0) as u32))
        };
        for i in 0..self.search.hits.len() {
            if let Some(dst) = place(self.search.hits[i]) {
                layers.push(self.mark(i, dst, images));
            }
        }
        for (i, r) in self.selection_boxes(page).into_iter().enumerate() {
            if let Some(dst) = place(r) {
                layers.push(self.selection_layer(i, dst, images));
            }
        }
    }

    /// The boxes (page pixels) of the selection, if it is of `page` (one
    /// of a page laid out since is dropped).
    fn selection_boxes(&mut self, page: &Page) -> Vec<Rect> {
        let Some(sel) = self.selection else { return Vec::new() };
        if sel.page != page.id {
            self.clear_selection();
            return Vec::new();
        }
        match &self.selection_rects {
            Some((of, rects)) if *of == sel => rects.clone(),
            _ => {
                let rects = page.selection_rects(sel.anchor, sel.head);
                self.selection_rects = Some((sel, rects.clone()));
                rects
            }
        }
    }

    /// The selection's box `i` at `dst` (pane pixels).
    fn selection_layer(&mut self, i: usize, dst: SceneRect, images: &mut Images) -> Layer {
        let (w, h) = (dst.w, dst.h);
        let image = self.swatches.get(images, w, h, theme::SELECTION);
        Layer { key: SELECTION_LAYERS | i as u32, image, src: SceneRect::new(0, 0, w, h), dst, z: 1 }
    }

    /// Search hit `i`'s mark at `dst` (pane pixels).
    fn mark(&mut self, i: usize, dst: SceneRect, images: &mut Images) -> Layer {
        let current = Some(i) == self.search.current;
        let (w, h) = (dst.w, dst.h);
        let image = self.swatches.get(images, w, h, if current { theme::CURRENT_HIT } else { theme::HIT });
        Layer { key: MARK_LAYERS | i as u32, image, src: SceneRect::new(0, 0, w, h), dst, z: 1 }
    }

    /// The background under strips not painted yet, as wide as the page.
    fn background(&mut self, page: &Page, images: &mut Images) -> ImageKey {
        match self.background {
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
        }
    }

    /// [`MdPane::frame`] magnified: the page's tiles painted at the
    /// magnification in sight; where one is not painted yet (and while
    /// fingers pinch, everywhere), the strips stretched under them (the
    /// background where a strip is not painted either). Tiles are painted
    /// only for what is in sight (a screen of them around would not fit
    /// the strips' memory at the largest magnifications).
    fn frame_magnified(
        &mut self,
        view: u64,
        page: &mut Page,
        strips: &mut TileCache<StripKey>,
        tag: u32,
        images: &mut Images,
    ) -> (Frame, Vec<Want<StripJob>>) {
        self.clamp(page);
        strips.begin_frame(view);
        let m = self.magnify;
        let (ox, oy) = self.viewport.offset();
        let (ox, oy) = (ox.round(), oy.round());
        let screen = SceneRect::new(0, 0, self.width.max(1), self.height.max(1));
        let centre = (f64::from(screen.w) / 2.0, f64::from(screen.h) / 2.0);
        let mut layers = Vec::new();
        let mut work = Schedule::new();
        let mut covered = !self.pinching;
        if !self.pinching {
            let magnify = (m * 100.0).round() as u32;
            let size = page.magnified_size(magnify);
            for cell in tiles::grid((-ox as i32, -oy as i32), size, (TILE, TILE), screen).filter(|c| c.on_screen) {
                let key = StripKey { page: page.id, strip: cell.ty, magnify, col: cell.tx };
                if let Some((image, layer)) = strips.touch(key) {
                    let src = SceneRect::new(0, 0, cell.area.2, cell.area.3);
                    layers.push(Layer { key: layer, image, src, dst: cell.dst, z: 0 });
                    continue;
                }
                covered = false;
                let want = if strips.is_in_flight(key) {
                    Some(Want::Queued((tag, key)))
                } else {
                    page.record_magnified(magnify, cell.tx, cell.ty, tag).map(Want::New)
                };
                if let Some(want) = want {
                    let d = cell.dst;
                    let dx = f64::from(d.x) + f64::from(d.w) / 2.0 - centre.0;
                    let dy = f64::from(d.y) + f64::from(d.h) / 2.0 - centre.1;
                    work.push(true, dx.hypot(dy) as f32, want);
                }
            }
        }
        self.complete = covered;
        if !covered {
            // The strips in sight, stretched, under the tiles.
            let x0 = -ox as i32;
            let x1 = (f64::from(page.width) * m - ox).round() as i32;
            let first = (oy / m / f64::from(STRIP)).floor().max(0.0) as u32;
            let last = ((oy + f64::from(screen.h)) / m / f64::from(STRIP)).ceil() as u32;
            let mut under = Vec::new();
            for strip in first..last.min(page.strips()) {
                let top = strip * STRIP;
                let rows = STRIP.min(page.height() - top);
                let y0 = (f64::from(top) * m - oy).round() as i32;
                let y1 = (f64::from(top + rows) * m - oy).round() as i32;
                let dst = SceneRect::new(x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
                let src = SceneRect::new(0, 0, page.width.max(1), rows);
                let key = StripKey::strip(page.id, strip);
                if let Some((image, layer)) = strips.touch(key) {
                    under.push(Layer { key: layer, image, src, dst, z: 0 });
                    continue;
                }
                let image = self.background(page, images);
                under.push(Layer { key: PLACEHOLDER_LAYERS + strip, image, src, dst, z: 0 });
                let want = if strips.is_in_flight(key) {
                    Some(Want::Queued((tag, key)))
                } else {
                    page.record(strip, tag).map(Want::New)
                };
                if let Some(want) = want {
                    let dy = f64::from(y0) + f64::from(y1 - y0) / 2.0 - centre.1;
                    work.push(true, dy.abs() as f32, want);
                }
            }
            under.append(&mut layers);
            layers = under;
        }
        self.overlays(page, m, (ox, oy), images, &mut layers);
        (Frame { layers, pointer: self.pointer(), ..Frame::default() }, work.into_sorted().collect())
    }

    /// Whether the last frame showed every strip in sight painted.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// How far down the page the pane is, in percent.
    pub fn percent(&self, page: &Page) -> u32 {
        let most = f64::from(page.height()) * self.magnify - f64::from(self.height);
        if most <= 0.0 { 100 } else { (self.viewport.y.offset() / most * 100.0).round() as u32 }
    }

    /// What the pane's status line (drawn by the session below it) says:
    /// the file's `name` (or the link under the pointer); on the right the
    /// search, the zoom and how far down `page` (the one it shows, when
    /// laid out) it is.
    pub fn status(&self, page: Option<&Page>, name: &str) -> StatusBar {
        let percent = page.map_or(0, |p| self.percent(p));
        let zoom = if (self.zoom - 1.0).abs() > 1e-3 { format!("  {:.0}%z", self.zoom * 100.0) } else { String::new() };
        let search = if self.search.query.is_empty() {
            String::new()
        } else {
            let ordinal = self.search.current.map(|i| i + 1);
            format!("  {}", temzdf_scene::search::hit_count(ordinal, self.search.hits.len(), true))
        };
        StatusBar {
            left: format!(" {}", self.hover.as_deref().unwrap_or(name)),
            right: format!("markdown{search}{zoom}  {percent}% "),
            ..Default::default()
        }
    }

    /// The pane's own images leave the store.
    pub fn release(&mut self, images: &mut Images) {
        self.swatches.release(images);
        if let Some((k, _)) = self.background.take() {
            images.remove(k);
        }
    }
}
