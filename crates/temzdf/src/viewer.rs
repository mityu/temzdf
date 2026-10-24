//! Viewer state independent of threads and the terminal: layout, view, scroll
//! animation and the tile cache. Each frame it produces the layers to show and
//! the work still needed (display lists to load, tiles to rasterise); the caller
//! performs that work (on threads interactively, inline in headless mode) and
//! feeds results back.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use mupdf::DisplayList;
use termpix::compositor::{Compositor, ImageId, Layer};
use termpix::geom::Rect;
use termpix::raster::{RgbaImage, blit};
use termpix::scroll::KineticScroll;

use crate::layout::{Layout, View};
use crate::pdf::{PageBounds, PixelRect, Target};

/// Tile edge in pixels.
pub const TILE: u32 = 512;
/// Thumbnail width in pixels, shown scaled while sharp tiles are missing (and
/// no tiles of an earlier scale cover them).
const THUMB_WIDTH: f32 = 320.0;
/// Memory budget for cached tiles (the terminal keeps its own copies).
const TILE_BUDGET: usize = 192 << 20;
pub const MIN_SCALE: f32 = 0.1;
pub const MAX_SCALE: f32 = 8.0;

// Everything we draw sits below text (and below cells with a background colour),
// so UI drawn in terminal cells would cover it.
const Z_BACKGROUND: i32 = -1_073_741_900;
const Z_THUMB: i32 = -1_073_741_890;
/// Tiles of an earlier scale, scaled, while tiles at the current scale render.
const Z_STAND_IN: i32 = -1_073_741_885;
const Z_TILE: i32 = -1_073_741_880;
const BACKGROUND: [u8; 4] = [0x3a, 0x3a, 0x3a, 0xff];
const LAYER_BACKGROUND: u32 = 1;
/// Layer keys at or above this are reserved for UI chrome (see `session.rs`).
pub const LAYER_KEYS_END: u32 = 0xf000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileKind {
    /// A `TILE`-sized cell of the page grid at a quantised scale (`scale * 1024`).
    Tile {
        scale_q: u32,
        tx: u32,
        ty: u32,
    },
    Thumb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    /// Document generation (bumped on every reload), so tiles of an older
    /// version of the file can never be mistaken for current ones.
    pub generation: u32,
    pub page: u32,
    pub kind: TileKind,
}

pub struct RenderJob {
    pub key: TileKey,
    pub list: Arc<DisplayList>,
    pub bounds: PageBounds,
    pub scale: f32,
    pub area: PixelRect,
}

pub struct Rendered {
    pub key: TileKey,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Work the viewer needs done, most urgent first.
pub enum Need {
    LoadPage(usize),
    Render(RenderJob),
}

struct Cached {
    image: ImageId,
    layer: u32,
    bytes: usize,
    last_used: u64,
}

fn quantize(scale: f32) -> u32 {
    (scale * 1024.0).round().max(1.0) as u32
}

fn dequantize(q: u32) -> f32 {
    q as f32 / 1024.0
}

pub struct Viewer {
    pub bounds: Vec<PageBounds>,
    pub layout: Layout,
    pub view: View,
    pub scroll_x: KineticScroll,
    pub scroll_y: KineticScroll,
    lists: Vec<Option<Arc<DisplayList>>>,
    lists_requested: HashSet<usize>,
    tiles: HashMap<TileKey, Cached>,
    in_flight: HashSet<TileKey>,
    used_bytes: usize,
    frame_no: u64,
    next_layer: u32,
    background: Option<ImageId>,
    /// Left edge of the page area on screen (pixels), e.g. right of a sidebar.
    origin_x: i32,
    /// Keep the zoom at fit-width when the page area changes.
    fit_mode: bool,
    generation: u32,
    /// See [`Viewer::is_complete`].
    complete: bool,
    /// How pages are covered right after a zoom (see [`StandIn`]).
    pub stand_in_mode: StandIn,
    /// `StandIn::Composed` images by page.
    composed: HashMap<usize, Composed>,
}

/// What covers a page while its tiles at the current scale render (just after
/// a zoom), on top of the page thumbnail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandIn {
    /// Only the thumbnail.
    Thumbnail,
    /// The tiles of the previous scale, as scaled layers. Exact where the
    /// compositor scales (Composited backends); the Layered backend rounds
    /// scaled placements to whole cells, which misplaces them by up to half a cell.
    ScaledTiles,
    /// One image per page composed from the previous scale's tiles. Costs an
    /// upload per zoom step, so only for fast transports (shm, files).
    Composed,
}

struct Composed {
    /// Current scale, old scale, screen rect.
    id: (u32, u32, Rect),
    image: ImageId,
    layer: u32,
    used: u64,
}

impl Viewer {
    /// `width` x `height` is the pixel size of the page area (the screen minus UI bars).
    pub fn new(bounds: Vec<PageBounds>, width: u32, height: u32) -> Self {
        let layout = Layout::new(&bounds.iter().map(|b| (b.width(), b.height())).collect::<Vec<_>>());
        let screen_w = width as f32;
        let view = View {
            scale: View::fit_width_scale(&layout, screen_w).clamp(MIN_SCALE, MAX_SCALE),
            scroll_x: 0.0,
            scroll_y: 0.0,
            screen_w,
            screen_h: height as f32,
        };
        let n = bounds.len();
        let mut v = Self {
            bounds,
            layout,
            view,
            scroll_x: KineticScroll::default(),
            scroll_y: KineticScroll::default(),
            lists: vec![None; n],
            lists_requested: HashSet::new(),
            tiles: HashMap::new(),
            in_flight: HashSet::new(),
            used_bytes: 0,
            frame_no: 0,
            next_layer: LAYER_BACKGROUND + 1,
            background: None,
            origin_x: 0,
            fit_mode: true,
            generation: 0,
            complete: false,
            stand_in_mode: StandIn::Thumbnail,
            composed: HashMap::new(),
        };
        v.update_scroll_bounds();
        v
    }

    pub fn page_count(&self) -> usize {
        self.bounds.len()
    }

    /// The page at the vertical centre of the screen.
    pub fn current_page(&self) -> usize {
        let (_, y) = self.view.screen_to_doc(&self.layout, 0.0, self.view.screen_h / 2.0);
        self.layout.page_at(y)
    }

    pub fn is_animating(&self) -> bool {
        self.scroll_x.is_animating() || self.scroll_y.is_animating()
    }

    /// Advance scroll animations. Returns true if the view moved.
    pub fn tick(&mut self, now: Instant) -> bool {
        let a = self.scroll_x.tick(now);
        let b = self.scroll_y.tick(now);
        self.sync_view();
        a || b
    }

    fn sync_view(&mut self) {
        self.view.scroll_x = self.scroll_x.offset() as f32;
        self.view.scroll_y = self.scroll_y.offset() as f32;
    }

    fn update_scroll_bounds(&mut self) {
        self.scroll_x.set_bounds(0.0, self.view.max_scroll_x(&self.layout) as f64);
        self.scroll_y.set_bounds(0.0, self.view.max_scroll_y(&self.layout) as f64);
        self.sync_view();
    }

    /// Jump (no animation) so the view's top-left is at the given doc-space pixels.
    pub fn jump_to(&mut self, scroll_x: f32, scroll_y: f32) {
        self.scroll_x.jump_to(scroll_x as f64);
        self.scroll_y.jump_to(scroll_y as f64);
        self.sync_view();
    }

    /// Scroll so the top of `page` (0-based) is at the top of the screen.
    pub fn goto_page(&mut self, page: usize, animate: bool, now: Instant) {
        self.goto(Target { page, y: None }, animate, now);
    }

    /// Scroll to a target: the page top, or a position inside the page (placed a
    /// little below the top of the screen so the context above stays visible).
    pub fn goto(&mut self, target: Target, animate: bool, now: Instant) {
        let Some(b) = self.layout.pages.get(target.page.min(self.page_count().saturating_sub(1))) else { return };
        let y_pt = match target.y {
            Some(y) => b.y + y.clamp(0.0, b.h) - 4.0 * crate::layout::GAP_PT,
            None => b.y - crate::layout::GAP_PT,
        };
        let px = (y_pt * self.view.scale) as f64;
        if animate {
            self.scroll_y.scroll_by(px - self.scroll_y.target(), now);
        } else {
            self.scroll_y.jump_to(px);
            self.sync_view();
        }
    }

    /// Screen pixel of a page-space point (points from the page's top-left).
    pub fn page_to_screen(&self, page: usize, x: f32, y: f32) -> (f32, f32) {
        let b = self.layout.pages[page];
        let (sx, sy) = self.view.doc_to_screen(&self.layout, b.x + x, b.y + y);
        (sx + self.origin_x as f32, sy)
    }

    /// The page under a screen pixel, and the page-space point there.
    pub fn screen_to_page(&self, sx: f32, sy: f32) -> Option<(usize, f32, f32)> {
        let (dx, dy) = self.view.screen_to_doc(&self.layout, sx - self.origin_x as f32, sy);
        let page = self.layout.page_at(dy);
        let b = self.layout.pages.get(page)?;
        let (x, y) = (dx - b.x, dy - b.y);
        (x >= 0.0 && y >= 0.0 && x <= b.w && y <= b.h).then_some((page, x, y))
    }

    /// Like [`Viewer::screen_to_page`], but points off any page map to the
    /// nearest page, clamped into it (for dragging selections past edges).
    pub fn screen_to_page_clamped(&self, sx: f32, sy: f32) -> (usize, f32, f32) {
        let (dx, dy) = self.view.screen_to_doc(&self.layout, sx - self.origin_x as f32, sy);
        let page = self.layout.page_at(dy);
        let b = self.layout.pages[page];
        (page, (dx - b.x).clamp(0.0, b.w), (dy - b.y).clamp(0.0, b.h))
    }

    pub fn display_list(&self, page: usize) -> Option<Arc<DisplayList>> {
        self.lists.get(page)?.clone()
    }

    /// Pages intersecting the page area.
    pub fn visible_pages(&self) -> std::ops::Range<usize> {
        let top = self.view.scroll_y / self.view.scale;
        self.layout.pages_in(top, top + self.view.screen_h / self.view.scale)
    }

    /// The page and page-space y at the top of the screen.
    pub fn top_position(&self) -> Target {
        let (_, y) = self.view.screen_to_doc(&self.layout, 0.0, 0.0);
        let page = self.layout.page_at(y);
        let b = self.layout.pages[page];
        Target { page, y: Some((y - b.y).clamp(0.0, b.h)) }
    }

    /// Multiply the zoom by `factor`, keeping the screen point `anchor` fixed.
    pub fn zoom(&mut self, factor: f32, anchor: (f32, f32)) {
        self.set_scale(self.view.scale * factor, anchor);
    }

    /// Set an explicit zoom; this leaves fit-width mode.
    pub fn set_scale(&mut self, scale: f32, anchor: (f32, f32)) {
        self.fit_mode = false;
        self.apply_scale(scale, anchor);
    }

    fn apply_scale(&mut self, scale: f32, anchor: (f32, f32)) {
        // Settle any animation first so the anchor maths uses the displayed position.
        self.jump_to(self.view.scroll_x, self.view.scroll_y);
        self.view.zoom_at(&self.layout, scale.clamp(MIN_SCALE, MAX_SCALE), anchor.0, anchor.1);
        self.update_scroll_bounds();
        self.jump_to(self.view.scroll_x, self.view.scroll_y);
    }

    /// Zoom so the widest page fills the page area, and keep doing so when the
    /// area changes size, until the zoom is set explicitly.
    pub fn fit_width(&mut self) {
        self.fit_mode = true;
        let scale = View::fit_width_scale(&self.layout, self.view.screen_w);
        self.apply_scale(scale, (self.view.screen_w / 2.0, 0.0));
    }

    /// Place the page area at `x` with the given size (see [`Viewer::resize`]).
    pub fn set_area(&mut self, x: i32, width: u32, height: u32) {
        self.origin_x = x;
        self.resize(width, height);
    }

    pub fn origin_x(&self) -> i32 {
        self.origin_x
    }

    /// The page area was resized: keep the doc point at the top-left in place
    /// (and re-fit the width in fit-width mode).
    pub fn resize(&mut self, width: u32, height: u32) {
        let (dx, dy) = self.view.screen_to_doc(&self.layout, 0.0, 0.0);
        self.view.screen_w = width as f32;
        self.view.screen_h = height as f32;
        let (sx, sy) = self.view.doc_to_screen(&self.layout, dx, dy);
        self.view.scroll_x += sx;
        self.view.scroll_y += sy;
        self.view.clamp(&self.layout);
        self.update_scroll_bounds();
        self.jump_to(self.view.scroll_x, self.view.scroll_y);
        if self.fit_mode {
            let scale = View::fit_width_scale(&self.layout, self.view.screen_w);
            self.apply_scale(scale, (0.0, 0.0));
        }
    }

    /// Generation of the document being shown (see [`TileKey::generation`]).
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// A display list arrived. Lists for an older generation are ignored.
    pub fn page_loaded(&mut self, generation: u32, page: usize, list: Arc<DisplayList>) {
        if generation != self.generation {
            return;
        }
        if let Some(slot) = self.lists.get_mut(page) {
            *slot = Some(list);
        }
        self.lists_requested.remove(&page);
    }

    /// A page failed to load; forget the request so it is retried later.
    pub fn page_failed(&mut self, generation: u32, page: usize) {
        if generation == self.generation {
            self.lists_requested.remove(&page);
        }
    }

    /// Show a new version of the document (after the file changed), keeping the
    /// reading position: the page at the top of the screen and the position
    /// within it as a fraction of the page height, so small layout changes still
    /// land in the right place. If the page count shrank, the last page is used
    /// with the same fraction. Zoom, fit-width mode and horizontal scroll stay.
    pub fn replace_pages(&mut self, comp: &mut Compositor, bounds: Vec<PageBounds>) {
        let top = self.top_position();
        let fraction = top.y.unwrap_or(0.0) / self.layout.pages[top.page].h.max(1.0);
        let scroll_x = self.view.scroll_x;

        self.generation = self.generation.wrapping_add(1);
        for (_, c) in self.tiles.drain() {
            comp.remove_image(c.image);
        }
        for (_, c) in self.composed.drain() {
            comp.remove_image(c.image);
        }
        self.used_bytes = 0;
        self.in_flight.clear();
        self.lists = vec![None; bounds.len()];
        self.lists_requested.clear();
        self.layout = Layout::new(&bounds.iter().map(|b| (b.width(), b.height())).collect::<Vec<_>>());
        self.bounds = bounds;
        if self.bounds.is_empty() {
            return;
        }
        if self.fit_mode {
            self.view.scale = View::fit_width_scale(&self.layout, self.view.screen_w).clamp(MIN_SCALE, MAX_SCALE);
        }
        let page = top.page.min(self.page_count() - 1);
        let b = self.layout.pages[page];
        self.view.scroll_x = scroll_x;
        self.view.scroll_y = (b.y + fraction * b.h) * self.view.scale;
        self.view.clamp(&self.layout);
        self.update_scroll_bounds();
        self.jump_to(self.view.scroll_x, self.view.scroll_y);
    }

    pub fn mark_requested(&mut self, need: &Need) {
        match need {
            Need::LoadPage(p) => {
                self.lists_requested.insert(*p);
            }
            Need::Render(job) => {
                self.in_flight.insert(job.key);
            }
        }
    }

    /// A render job was dropped before it ran.
    pub fn render_cancelled(&mut self, key: TileKey) {
        self.in_flight.remove(&key);
    }

    pub fn rendered(&mut self, comp: &mut Compositor, r: Rendered) {
        self.in_flight.remove(&r.key);
        if r.key.generation != self.generation || self.tiles.contains_key(&r.key) {
            return;
        }
        let bytes = r.pixels.len();
        let image = comp.add_image(r.width, r.height, r.pixels);
        let layer = self.alloc_layer();
        self.used_bytes += bytes;
        self.tiles.insert(r.key, Cached { image, layer, bytes, last_used: self.frame_no });
    }

    /// Build this frame's layers and the outstanding work, most urgent first.
    pub fn frame(&mut self, comp: &mut Compositor) -> (Vec<Layer>, Vec<Need>) {
        self.frame_no += 1;
        self.sync_view();
        let view = self.view;
        let ox = self.origin_x;
        let screen = Rect::new(ox, 0, view.screen_w as u32, view.screen_h as u32);
        let mut layers = Vec::new();

        let bg = *self.background.get_or_insert_with(|| comp.add_image(1, 1, BACKGROUND.to_vec()));
        layers.push(Layer {
            key: LAYER_BACKGROUND,
            image: bg,
            src: Rect::new(0, 0, 1, 1),
            dst: screen,
            z: Z_BACKGROUND,
        });

        let top = view.scroll_y / view.scale;
        let bottom = (view.scroll_y + view.screen_h) / view.scale;
        let margin = view.screen_h / view.scale;
        let visible = self.layout.pages_in(top, bottom);
        let prefetch = self.layout.pages_in(top - margin, bottom + margin);

        let scale_q = quantize(view.scale);
        let scale = dequantize(scale_q);
        let mut urgent = Vec::new();
        let mut later = Vec::new();
        let centre = (view.screen_w / 2.0, view.screen_h / 2.0);

        let mut complete = true;
        for page in prefetch.clone() {
            let is_visible = visible.contains(&page);
            let queue = if is_visible { &mut urgent } else { &mut later };
            let Some(list) = self.lists[page].clone() else {
                complete &= !is_visible;
                if !self.lists_requested.contains(&page) {
                    queue.push((0.0, Need::LoadPage(page)));
                }
                continue;
            };
            let bounds = self.bounds[page];
            let pb = self.layout.pages[page];
            let (sx, sy) = view.doc_to_screen(&self.layout, pb.x, pb.y);
            let (sx, sy) = (sx.round() as i32 + ox, sy.round() as i32);
            let (pw, ph) = bounds.pixel_size(scale);
            let page_rect = Rect::new(sx, sy, pw, ph);

            // Thumbnail first: it is cheap and covers the page while tiles render.
            let thumb_key = TileKey { generation: self.generation, page: page as u32, kind: TileKind::Thumb };
            let thumb_scale = THUMB_WIDTH / bounds.width();
            let thumb_cached = self.touch(thumb_key);
            if thumb_cached.is_none() && !self.in_flight.contains(&thumb_key) {
                let (tw, th) = bounds.pixel_size(thumb_scale);
                let area = PixelRect { x: 0, y: 0, w: tw, h: th };
                queue.push((
                    -1.0,
                    Need::Render(RenderJob { key: thumb_key, list: list.clone(), bounds, scale: thumb_scale, area }),
                ));
            }

            let mut missing_on_screen = false;
            for ty in 0..ph.div_ceil(TILE) {
                for tx in 0..pw.div_ceil(TILE) {
                    let area = PixelRect {
                        x: tx * TILE,
                        y: ty * TILE,
                        w: TILE.min(pw - tx * TILE),
                        h: TILE.min(ph - ty * TILE),
                    };
                    let dst = Rect::new(sx + area.x as i32, sy + area.y as i32, area.w, area.h);
                    let on_screen = dst.intersect(&screen).is_some();
                    let near = dst.y as f32 <= view.screen_h + view.screen_h && dst.bottom() as f32 >= -view.screen_h;
                    if !on_screen && !near {
                        continue;
                    }
                    let key = TileKey {
                        generation: self.generation,
                        page: page as u32,
                        kind: TileKind::Tile { scale_q, tx, ty },
                    };
                    match self.touch(key) {
                        Some((image, layer)) => {
                            if on_screen {
                                layers.push(Layer {
                                    key: layer,
                                    image,
                                    src: Rect::new(0, 0, area.w, area.h),
                                    dst,
                                    z: Z_TILE,
                                });
                            }
                        }
                        None => {
                            missing_on_screen |= on_screen;
                            if !self.in_flight.contains(&key) {
                                let cx = dst.x as f32 + dst.w as f32 / 2.0 - centre.0;
                                let cy = dst.y as f32 + dst.h as f32 / 2.0 - centre.1;
                                let job = RenderJob { key, list: list.clone(), bounds, scale, area };
                                let q = if on_screen { &mut urgent } else { &mut later };
                                q.push((cx.hypot(cy), Need::Render(job)));
                            }
                        }
                    }
                }
            }
            complete &= !missing_on_screen;
            if missing_on_screen {
                self.stand_in(comp, page, scale_q, (sx, sy), &screen, &mut layers);
            }
            if missing_on_screen && let Some((image, layer)) = thumb_cached {
                let (tw, th) = bounds.pixel_size(thumb_scale);
                layers.push(Layer { key: layer, image, src: Rect::new(0, 0, tw, th), dst: page_rect, z: Z_THUMB });
            }
        }
        self.complete = complete;

        urgent.sort_by(|a, b| a.0.total_cmp(&b.0));
        later.sort_by(|a, b| a.0.total_cmp(&b.0));
        let needs = urgent.into_iter().chain(later).map(|(_, n)| n).collect();
        // Composed stand-ins not shown this frame are done with.
        let frame = self.frame_no;
        self.composed.retain(|_, c| {
            let keep = c.used == frame;
            if !keep {
                comp.remove_image(c.image);
            }
            keep
        });
        self.evict(comp);
        (layers, needs)
    }

    /// Whether the last frame showed every visible page at full resolution
    /// (no missing tiles, no placeholder thumbnails).
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// While tiles at the current scale are missing (just after a zoom), show
    /// the page's tiles from the scale it was last shown at, scaled to the
    /// current one: much sharper than the thumbnail. Current tiles cover them as
    /// they arrive. `origin` is the page's top-left on screen.
    fn stand_in(
        &mut self,
        comp: &mut Compositor,
        page: usize,
        scale_q: u32,
        origin: (i32, i32),
        screen: &Rect,
        layers: &mut Vec<Layer>,
    ) {
        let generation = self.generation;
        // The other scale used most recently for this page.
        let Some(old_q) = self
            .tiles
            .iter()
            .filter_map(|(k, c)| match k.kind {
                TileKind::Tile { scale_q: q, .. }
                    if k.generation == generation && k.page == page as u32 && q != scale_q =>
                {
                    Some((c.last_used, q))
                }
                _ => None,
            })
            .max()
            .map(|(_, q)| q)
        else {
            return;
        };
        let bounds = self.bounds[page];
        let (opw, oph) = bounds.pixel_size(dequantize(old_q));
        let f = dequantize(scale_q) as f64 / dequantize(old_q) as f64;
        let at = |v: u32| (v as f64 * f).round() as i32;
        // Old tiles on screen: (image, source size, destination).
        let mut parts = Vec::new();
        for ty in 0..oph.div_ceil(TILE) {
            for tx in 0..opw.div_ceil(TILE) {
                let (x0, y0) = (tx * TILE, ty * TILE);
                let (x1, y1) = ((x0 + TILE).min(opw), (y0 + TILE).min(oph));
                let dst = Rect::new(
                    origin.0 + at(x0),
                    origin.1 + at(y0),
                    (at(x1) - at(x0)).max(1) as u32,
                    (at(y1) - at(y0)).max(1) as u32,
                );
                if dst.intersect(screen).is_none() {
                    continue;
                }
                let key = TileKey { generation, page: page as u32, kind: TileKind::Tile { scale_q: old_q, tx, ty } };
                if let Some((image, layer)) = self.touch(key) {
                    parts.push((image, layer, Rect::new(0, 0, x1 - x0, y1 - y0), dst));
                }
            }
        }
        if parts.is_empty() {
            return;
        }
        match self.stand_in_mode {
            StandIn::Thumbnail => {}
            StandIn::ScaledTiles => {
                for (image, layer, src, dst) in parts {
                    layers.push(Layer { key: layer, image, src, dst, z: Z_STAND_IN });
                }
            }
            StandIn::Composed => {
                // One unscaled image of the page's visible part, composed here:
                // terminals scaling images themselves (Layered) snap to cells.
                let page_rect = Rect::new(origin.0, origin.1, at(opw).max(1) as u32, at(oph).max(1) as u32);
                let Some(vis) = page_rect.intersect(screen) else { return };
                let id = (scale_q, old_q, vis);
                let frame = self.frame_no;
                if let Some(c) = self.composed.get_mut(&page)
                    && c.id == id
                {
                    c.used = frame;
                    layers.push(Layer {
                        key: c.layer,
                        image: c.image,
                        src: Rect::new(0, 0, vis.w, vis.h),
                        dst: vis,
                        z: Z_STAND_IN,
                    });
                    return;
                }
                let mut img = RgbaImage::new(vis.w, vis.h, [0, 0, 0, 0]);
                for (image, _, src, dst) in &parts {
                    if let Some(px) = comp.image_pixels(*image) {
                        let at = Rect::new(dst.x - vis.x, dst.y - vis.y, dst.w, dst.h);
                        blit(&mut img, at, px, *src);
                    }
                }
                let image = comp.add_image(vis.w, vis.h, img.data);
                let layer = self.alloc_layer();
                if let Some(old) = self.composed.insert(page, Composed { id, image, layer, used: frame }) {
                    comp.remove_image(old.image);
                }
                layers.push(Layer { key: layer, image, src: Rect::new(0, 0, vis.w, vis.h), dst: vis, z: Z_STAND_IN });
            }
        }
    }

    fn alloc_layer(&mut self) -> u32 {
        let layer = self.next_layer;
        self.next_layer =
            if self.next_layer + 1 >= LAYER_KEYS_END { LAYER_BACKGROUND + 1 } else { self.next_layer + 1 };
        layer
    }

    fn touch(&mut self, key: TileKey) -> Option<(ImageId, u32)> {
        let frame = self.frame_no;
        self.tiles.get_mut(&key).map(|c| {
            c.last_used = frame;
            (c.image, c.layer)
        })
    }

    /// Drop least recently used tiles over budget, never ones used this frame.
    fn evict(&mut self, comp: &mut Compositor) {
        if self.used_bytes <= TILE_BUDGET {
            return;
        }
        let mut old: Vec<(u64, TileKey)> =
            self.tiles.iter().filter(|(_, c)| c.last_used < self.frame_no).map(|(k, c)| (c.last_used, *k)).collect();
        old.sort_by_key(|(t, _)| *t);
        for (_, key) in old {
            if self.used_bytes <= TILE_BUDGET {
                break;
            }
            if let Some(c) = self.tiles.remove(&key) {
                comp.remove_image(c.image);
                self.used_bytes -= c.bytes;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf;
    use termpix::compositor::Backend;
    use termpix::geom::{CellSize, Geometry};
    use termpix::graphics::Medium;
    use termpix::mock::MockTerminal;

    const GEOMETRY: Geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };

    /// Do the viewer's work inline until it needs nothing more.
    fn settle(v: &mut Viewer, comp: &mut Compositor, doc: &pdf::Doc) {
        loop {
            let (_, needs) = v.frame(comp);
            if needs.is_empty() {
                return;
            }
            for need in needs {
                match need {
                    Need::LoadPage(p) => v.page_loaded(v.generation(), p, pdf::display_list(doc, p).unwrap()),
                    Need::Render(job) => {
                        let pixels = pdf::render(&job.list, job.bounds, job.scale, job.area).unwrap();
                        v.rendered(comp, Rendered { key: job.key, width: job.area.w, height: job.area.h, pixels });
                    }
                }
            }
        }
    }

    /// Mean absolute difference per channel after averaging 4x4 blocks: how far
    /// the content is from where it should be, mostly regardless of how sharp it
    /// is (the mock terminal scales with nearest neighbour, so scaled tiles are
    /// jagged and the blurry thumbnail would win a per-pixel comparison).
    fn diff(a: &termpix::raster::RgbaImage, b: &termpix::raster::RgbaImage) -> f64 {
        let block = |img: &termpix::raster::RgbaImage, bx: u32, by: u32, ch: usize| -> f64 {
            let mut sum = 0u32;
            for y in by * 4..by * 4 + 4 {
                for x in bx * 4..bx * 4 + 4 {
                    sum += img.pixel(x, y)[ch] as u32;
                }
            }
            sum as f64 / 16.0
        };
        let (bw, bh) = (a.width / 4, a.height / 4);
        let mut total = 0.0;
        for by in 0..bh {
            for bx in 0..bw {
                for ch in 0..3 {
                    total += (block(a, bx, by, ch) - block(b, bx, by, ch)).abs();
                }
            }
        }
        total / (bw * bh * 3) as f64
    }

    /// The screen right after zooming in (before any new tile arrives), and once
    /// everything is rendered, and how many stand-in layers the first one had.
    fn zoom_frames(backend: Backend, mode: StandIn) -> (termpix::raster::RgbaImage, termpix::raster::RgbaImage, usize) {
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        let bounds = pdf::doc_info(&doc).unwrap().bounds;
        let mut v = Viewer::new(bounds, GEOMETRY.width_px(), GEOMETRY.height_px());
        v.stand_in_mode = mode;
        let mut comp = Compositor::new(backend, Medium::Direct { compress: false }, GEOMETRY);
        let mut term = MockTerminal::new(GEOMETRY, [0, 0, 0, 255]);
        let mut out = Vec::new();
        settle(&mut v, &mut comp, &doc);
        let (layers, _) = v.frame(&mut comp);
        comp.present(&layers, &mut out).unwrap();
        v.zoom(1.7, (300.0, 200.0));
        let (layers, _) = v.frame(&mut comp);
        let stand_ins = layers.iter().filter(|l| l.z == Z_STAND_IN).count();
        comp.present(&layers, &mut out).unwrap();
        term.feed(&out);
        let early = term.render();
        settle(&mut v, &mut comp, &doc);
        let (layers, _) = v.frame(&mut comp);
        assert!(!layers.iter().any(|l| l.z == Z_STAND_IN), "stand-ins go once tiles are in");
        out.clear();
        comp.present(&layers, &mut out).unwrap();
        term.feed(&out);
        (early, term.render(), stand_ins)
    }

    #[test]
    fn zooming_shows_old_tiles_scaled_until_new_ones_arrive() {
        for (backend, mode) in [(Backend::composited(), StandIn::ScaledTiles), (Backend::Layered, StandIn::Composed)] {
            let (early, done, count) = zoom_frames(backend, mode);
            assert!(count > 0, "{mode:?}: old-scale tiles stand in");
            let (thumb_early, thumb_done, _) = zoom_frames(backend, StandIn::Thumbnail);
            assert_eq!(diff(&done, &thumb_done), 0.0, "the final frames agree");
            let with = diff(&early, &done);
            let without = diff(&thumb_early, &done);
            assert!(with < without * 0.5, "{mode:?}: stand-ins {with:.3} vs thumbnail {without:.3}");
        }
    }
}
