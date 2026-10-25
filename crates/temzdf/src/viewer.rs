//! Viewer state independent of threads and the terminal: layout, view, scroll
//! animation and the tile cache. Each frame it produces the layers to show and
//! the work still needed (display lists to load, tiles to rasterise); the caller
//! performs that work (on threads interactively, inline in headless mode) and
//! feeds results back.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use mupdf::DisplayList;
use temzdf_scene::{ImageKey, Images, Layer};
use termpix_core::geom::Rect;
use termpix_core::raster::{RgbaImage, RgbaRef, blit};
use termpix_core::scroll::KineticScroll;

use crate::doc_cache::{DocCache, LAYER_BACKGROUND, dequantize, quantize};
pub use crate::doc_cache::{LAYER_KEYS_END, Need, RenderJob, Rendered, TILE, TileKey, TileKind};
use crate::layout::{Layout, View};
use crate::pdf::{PageBounds, PixelRect, Target};

/// Thumbnail width in pixels, shown scaled while sharp tiles are missing (and
/// no tiles of an earlier scale cover them).
const THUMB_WIDTH: f32 = 320.0;
/// Points kept above a jump target, so the context above it stays visible.
pub const TARGET_MARGIN: f32 = 4.0 * crate::layout::GAP_PT;
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
/// Layers of the [`Peek`] window: a tile shown there and in the page view
/// needs a second layer key.
const LAYER_PEEK: u32 = 0xf800_0000;

/// A second window onto the document, drawn from the same tiles at the same
/// scale (link previews). `rect` (screen pixels) shows the document from
/// doc-space pixels `(scroll_x, scroll_y)` at its top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Peek {
    pub rect: Rect,
    pub scroll_x: f32,
    pub scroll_y: f32,
    /// The background goes at `z`, tiles at `z + 1`.
    pub z: i32,
}

pub struct Viewer {
    /// Display lists and tiles (shareable between views of one document).
    pub doc: DocCache,
    pub layout: Layout,
    pub view: View,
    pub scroll_x: KineticScroll,
    pub scroll_y: KineticScroll,
    /// Frames built so far (stand-ins not shown in the latest one are dropped).
    frame_no: u64,
    /// Page-area-sized background image and its size. Not a scaled 1x1 image:
    /// terminals filter scaled images against a transparent border (kitty
    /// darkens half a source pixel along each edge, i.e. half the screen).
    background: Option<(ImageKey, u32, u32)>,
    /// Left edge of the page area on screen (pixels), e.g. right of a sidebar.
    origin_x: i32,
    /// Keep the zoom at fit-width when the page area changes.
    fit_mode: bool,
    /// See [`Viewer::is_complete`].
    complete: bool,
    /// How pages are covered right after a zoom (see [`StandIn`]).
    pub stand_in_mode: StandIn,
    /// `StandIn::Composed` images by page.
    composed: HashMap<usize, Composed>,
    /// Shown above the page view while set; its tiles are rendered first.
    pub peek: Option<Peek>,
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
    image: ImageKey,
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
        let mut v = Self {
            doc: DocCache::new(bounds),
            layout,
            view,
            scroll_x: KineticScroll::default(),
            scroll_y: KineticScroll::default(),
            frame_no: 0,
            background: None,
            origin_x: 0,
            fit_mode: true,
            complete: false,
            stand_in_mode: StandIn::Thumbnail,
            composed: HashMap::new(),
            peek: None,
        };
        v.update_scroll_bounds();
        v
    }

    pub fn page_count(&self) -> usize {
        self.doc.page_count()
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

    /// After `view` was changed directly (zoom, resize, new layout): update the
    /// scroll limits and move the scrollers to the view's position (clamped),
    /// ending any animation. The view is the source of truth here; reading the
    /// scrollers back instead would undo the change (zooming then anchored at
    /// the top-left corner instead of the pointer).
    fn update_scroll_bounds(&mut self) {
        let (x, y) = (self.view.scroll_x, self.view.scroll_y);
        self.scroll_x.set_bounds(0.0, self.view.max_scroll_x(&self.layout) as f64);
        self.scroll_y.set_bounds(0.0, self.view.max_scroll_y(&self.layout) as f64);
        self.jump_to(x, y);
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
        if self.layout.pages.is_empty() {
            return;
        }
        let px = (self.target_top(target) * self.view.scale) as f64;
        if animate {
            self.scroll_y.scroll_by(px - self.scroll_y.target(), now);
        } else {
            self.scroll_y.jump_to(px);
            self.sync_view();
        }
    }

    /// The doc-space y (points) that [`Viewer::goto`] puts at the top of the
    /// screen for `target` (unclamped). The document must have pages.
    pub fn target_top(&self, target: Target) -> f32 {
        let b = self.layout.pages[target.page.min(self.page_count() - 1)];
        match target.y {
            Some(y) => b.y + y.clamp(0.0, b.h) - TARGET_MARGIN,
            None => b.y - crate::layout::GAP_PT,
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
        self.doc.display_list(page)
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
        if self.fit_mode {
            let scale = View::fit_width_scale(&self.layout, self.view.screen_w);
            self.apply_scale(scale, (0.0, 0.0));
        }
    }

    /// Generation of the document being shown (see [`TileKey::generation`]).
    pub fn generation(&self) -> u32 {
        self.doc.generation()
    }

    /// A display list arrived (see [`DocCache::page_loaded`]); pages in the
    /// peek window are in use however far away they are.
    pub fn page_loaded(&mut self, generation: u32, page: usize, list: Arc<DisplayList>) {
        let current = self.current_page();
        let peeked = self.peek.map_or(0..0, |p| self.peek_pages(&p));
        self.doc.page_loaded(generation, page, list, current, peeked);
    }

    /// A page failed to load; forget the request so it is retried later.
    pub fn page_failed(&mut self, generation: u32, page: usize) {
        self.doc.page_failed(generation, page);
    }

    /// Show a new version of the document (after the file changed), keeping the
    /// reading position: the page at the top of the screen and the position
    /// within it as a fraction of the page height, so small layout changes still
    /// land in the right place. If the page count shrank, the last page is used
    /// with the same fraction. Zoom, fit-width mode and horizontal scroll stay.
    pub fn replace_pages(&mut self, images: &mut Images, bounds: Vec<PageBounds>) {
        let top = self.top_position();
        let fraction = top.y.unwrap_or(0.0) / self.layout.pages[top.page].h.max(1.0);
        let scroll_x = self.view.scroll_x;

        self.layout = Layout::new(&bounds.iter().map(|b| (b.width(), b.height())).collect::<Vec<_>>());
        self.doc.replace(images, bounds);
        for (_, c) in self.composed.drain() {
            images.remove(c.image);
        }
        if self.doc.bounds.is_empty() {
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
    }

    pub fn mark_requested(&mut self, need: &Need) {
        self.doc.mark_requested(need);
    }

    /// A render job was dropped before it ran.
    pub fn render_cancelled(&mut self, key: TileKey) {
        self.doc.render_cancelled(key);
    }

    pub fn rendered(&mut self, images: &mut Images, r: Rendered) {
        self.doc.rendered(images, r);
    }

    /// Build this frame's layers and the outstanding work, most urgent first.
    pub fn frame(&mut self, images: &mut Images) -> (Vec<Layer>, Vec<Need>) {
        self.frame_no += 1;
        self.doc.begin_frame();
        self.sync_view();
        let view = self.view;
        let ox = self.origin_x;
        let screen = Rect::new(ox, 0, view.screen_w as u32, view.screen_h as u32);
        let mut layers = Vec::new();

        let (sw, sh) = (screen.w.max(1), screen.h.max(1));
        let bg = match self.background {
            Some((id, w, h)) if (w, h) == (sw, sh) => id,
            old => {
                if let Some((id, ..)) = old {
                    images.remove(id);
                }
                let id = images.add(sw, sh, BACKGROUND.repeat(sw as usize * sh as usize));
                self.background = Some((id, sw, sh));
                id
            }
        };
        layers.push(Layer {
            key: LAYER_BACKGROUND,
            image: bg,
            src: Rect::new(0, 0, sw, sh),
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
            let Some(list) = self.doc.display_list(page) else {
                complete &= !is_visible;
                if !self.doc.is_requested(page) {
                    queue.push((0.0, Need::LoadPage(page)));
                }
                continue;
            };
            let bounds = self.doc.bounds[page];
            let pb = self.layout.pages[page];
            let (sx, sy) = view.doc_to_screen(&self.layout, pb.x, pb.y);
            let (sx, sy) = (sx.round() as i32 + ox, sy.round() as i32);
            let (pw, ph) = bounds.pixel_size(scale);
            let page_rect = Rect::new(sx, sy, pw, ph);

            // Thumbnail first: it is cheap and covers the page while tiles render.
            let thumb_key = TileKey { generation: self.doc.generation(), page: page as u32, kind: TileKind::Thumb };
            let thumb_scale = THUMB_WIDTH / bounds.width();
            let thumb_cached = self.doc.touch(thumb_key);
            if thumb_cached.is_none() && !self.doc.is_in_flight(thumb_key) {
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
                        generation: self.doc.generation(),
                        page: page as u32,
                        kind: TileKind::Tile { scale_q, tx, ty },
                    };
                    match self.doc.touch(key) {
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
                            if !self.doc.is_in_flight(key) {
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
                self.stand_in(images, page, scale_q, (sx, sy), &screen, &mut layers);
            }
            if missing_on_screen && let Some((image, layer)) = thumb_cached {
                let (tw, th) = bounds.pixel_size(thumb_scale);
                layers.push(Layer { key: layer, image, src: Rect::new(0, 0, tw, th), dst: page_rect, z: Z_THUMB });
            }
        }
        self.complete = complete;

        // The peek window's work goes first, and only once.
        let mut peek_needs = Vec::new();
        if let Some(peek) = self.peek {
            self.peek_frame(&peek, bg, (sw, sh), scale_q, &mut layers, &mut peek_needs);
            let dup = |n: &Need| {
                peek_needs.iter().any(|p| match (p, n) {
                    (Need::LoadPage(a), Need::LoadPage(b)) => a == b,
                    (Need::Render(a), Need::Render(b)) => a.key == b.key,
                    _ => false,
                })
            };
            urgent.retain(|(_, n)| !dup(n));
            later.retain(|(_, n)| !dup(n));
        }
        urgent.sort_by(|a, b| a.0.total_cmp(&b.0));
        later.sort_by(|a, b| a.0.total_cmp(&b.0));
        let needs = peek_needs.into_iter().chain(urgent.into_iter().chain(later).map(|(_, n)| n)).collect();
        // Composed stand-ins not shown this frame are done with.
        let frame = self.frame_no;
        self.composed.retain(|_, c| {
            let keep = c.used == frame;
            if !keep {
                images.remove(c.image);
            }
            keep
        });
        self.doc.evict(images);
        (layers, needs)
    }

    /// Pages intersecting the peek window.
    fn peek_pages(&self, p: &Peek) -> std::ops::Range<usize> {
        let s = self.view.scale;
        self.layout.pages_in(p.scroll_y / s, (p.scroll_y + p.rect.h as f32) / s)
    }

    /// The page under a screen pixel inside the peek window, and the page-space point there.
    pub fn peek_to_page(&self, p: &Peek, sx: f32, sy: f32) -> Option<(usize, f32, f32)> {
        if !p.rect.contains(sx as i32, sy as i32) {
            return None;
        }
        let s = self.view.scale;
        let dx = (sx - p.rect.x as f32 + p.scroll_x) / s;
        let dy = (sy - p.rect.y as f32 + p.scroll_y) / s;
        let page = self.layout.page_at(dy);
        let b = self.layout.pages.get(page)?;
        let (x, y) = (dx - b.x, dy - b.y);
        (x >= 0.0 && y >= 0.0 && x <= b.w && y <= b.h).then_some((page, x, y))
    }

    /// Layers of the peek window: the background, then the visible parts of
    /// the tiles under it; missing tiles and display lists become needs.
    fn peek_frame(
        &mut self,
        p: &Peek,
        bg: ImageKey,
        bg_size: (u32, u32),
        scale_q: u32,
        layers: &mut Vec<Layer>,
        needs: &mut Vec<Need>,
    ) {
        let r = p.rect;
        let bg_src = Rect::new(0, 0, r.w.min(bg_size.0), r.h.min(bg_size.1));
        layers.push(Layer { key: LAYER_PEEK, image: bg, src: bg_src, dst: r, z: p.z });
        let scale = dequantize(scale_q);
        let mut key = LAYER_PEEK + 1;
        for page in self.peek_pages(p) {
            let Some(list) = self.doc.display_list(page) else {
                if !self.doc.is_requested(page) {
                    needs.push(Need::LoadPage(page));
                }
                continue;
            };
            let bounds = self.doc.bounds[page];
            let pb = self.layout.pages[page];
            let ox = r.x + (pb.x * scale - p.scroll_x).round() as i32;
            let oy = r.y + (pb.y * scale - p.scroll_y).round() as i32;
            let (pw, ph) = bounds.pixel_size(scale);
            // The range of tiles overlapping the window.
            let span = |from: i64, to: i64, size: u32| {
                let a = from.clamp(0, size as i64) as u32;
                let b = to.clamp(0, size as i64) as u32;
                a / TILE..b.div_ceil(TILE)
            };
            for ty in span(r.y as i64 - oy as i64, r.bottom() - oy as i64, ph) {
                for tx in span(r.x as i64 - ox as i64, r.right() - ox as i64, pw) {
                    let area = PixelRect {
                        x: tx * TILE,
                        y: ty * TILE,
                        w: TILE.min(pw - tx * TILE),
                        h: TILE.min(ph - ty * TILE),
                    };
                    let dst = Rect::new(ox + area.x as i32, oy + area.y as i32, area.w, area.h);
                    let Some(vis) = dst.intersect(&r) else { continue };
                    let tile = TileKey {
                        generation: self.doc.generation(),
                        page: page as u32,
                        kind: TileKind::Tile { scale_q, tx, ty },
                    };
                    match self.doc.touch(tile) {
                        Some((image, _)) => {
                            let src = Rect::new(vis.x - dst.x, vis.y - dst.y, vis.w, vis.h);
                            layers.push(Layer { key, image, src, dst: vis, z: p.z + 1 });
                            key += 1;
                        }
                        None if !self.doc.is_in_flight(tile) => {
                            needs.push(Need::Render(RenderJob { key: tile, list: list.clone(), bounds, scale, area }));
                        }
                        None => {}
                    }
                }
            }
        }
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
        images: &mut Images,
        page: usize,
        scale_q: u32,
        origin: (i32, i32),
        screen: &Rect,
        layers: &mut Vec<Layer>,
    ) {
        let generation = self.doc.generation();
        // The other scale used most recently for this page.
        let Some(old_q) = self.doc.latest_other_scale(page, scale_q) else { return };
        let bounds = self.doc.bounds[page];
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
                if let Some((image, layer)) = self.doc.touch(key) {
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
                    if let Some(px) = images.get(*image) {
                        let px = RgbaRef { width: px.width, height: px.height, data: &px.pixels };
                        let at = Rect::new(dst.x - vis.x, dst.y - vis.y, dst.w, dst.h);
                        blit(&mut img, at, px, *src);
                    }
                }
                let image = images.add(vis.w, vis.h, img.data);
                let layer = self.doc.alloc_layer();
                if let Some(old) = self.composed.insert(page, Composed { id, image, layer, used: frame }) {
                    images.remove(old.image);
                }
                layers.push(Layer { key: layer, image, src: Rect::new(0, 0, vis.w, vis.h), dst: vis, z: Z_STAND_IN });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf;
    use crate::present::Presenter;
    use termpix::compositor::{Backend, Compositor};
    use termpix::geom::{CellSize, Geometry};
    use termpix::graphics::Medium;
    use termpix::mock::MockTerminal;

    const GEOMETRY: Geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };

    /// Do the viewer's work inline until it needs nothing more.
    fn settle(v: &mut Viewer, images: &mut Images, doc: &pdf::Doc) {
        loop {
            let (_, needs) = v.frame(images);
            if needs.is_empty() {
                return;
            }
            for need in needs {
                match need {
                    Need::LoadPage(p) => v.page_loaded(v.generation(), p, pdf::display_list(doc, p).unwrap()),
                    Need::Render(job) => {
                        let pixels = pdf::render(&job.list, job.bounds, job.scale, job.area).unwrap();
                        v.rendered(images, Rendered { key: job.key, width: job.area.w, height: job.area.h, pixels });
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
        let mut images = Images::new();
        let mut presenter = Presenter::new();
        let mut term = MockTerminal::new(GEOMETRY, [0, 0, 0, 255]);
        let mut out = Vec::new();
        let mut present = |layers: &[Layer], images: &mut Images, out: &mut Vec<u8>| {
            presenter.sync(&mut comp, images);
            comp.present(&presenter.layers(layers), out).unwrap();
        };
        settle(&mut v, &mut images, &doc);
        let (layers, _) = v.frame(&mut images);
        present(&layers, &mut images, &mut out);
        v.zoom(1.7, (300.0, 200.0));
        let (layers, _) = v.frame(&mut images);
        let stand_ins = layers.iter().filter(|l| l.z == Z_STAND_IN).count();
        present(&layers, &mut images, &mut out);
        term.feed(&out);
        let early = term.render();
        settle(&mut v, &mut images, &doc);
        let (layers, _) = v.frame(&mut images);
        assert!(!layers.iter().any(|l| l.z == Z_STAND_IN), "stand-ins go once tiles are in");
        out.clear();
        present(&layers, &mut images, &mut out);
        term.feed(&out);
        (early, term.render(), stand_ins)
    }

    #[test]
    fn resizing_keeps_the_top_left_doc_point() {
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        let bounds = pdf::doc_info(&doc).unwrap().bounds;
        let mut v = Viewer::new(bounds, 900, 560);
        v.set_scale(2.0, (0.0, 0.0));
        v.jump_to(300.0, 1500.0);
        let before = v.view.screen_to_doc(&v.layout, 0.0, 0.0);
        v.resize(700, 400);
        let after = v.view.screen_to_doc(&v.layout, 0.0, 0.0);
        assert!((before.0 - after.0).abs() < 0.01 && (before.1 - after.1).abs() < 0.01, "{before:?} {after:?}");
    }

    #[test]
    fn display_lists_far_from_the_view_are_dropped_and_reloaded() {
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        let bounds = pdf::doc_info(&doc).unwrap().bounds;
        let mut v = Viewer::new(bounds, GEOMETRY.width_px(), GEOMETRY.height_px());
        v.doc.max_lists = 2;
        for p in 0..3 {
            v.page_loaded(0, p, pdf::display_list(&doc, p).unwrap());
        }
        // Reading page 1: page 3 is the farthest.
        assert!(v.display_list(0).is_some() && v.display_list(1).is_some());
        assert!(v.display_list(2).is_none());
        // Going there asks for it again (and drops page 1's).
        v.goto_page(2, false, Instant::now());
        let (_, needs) = v.frame(&mut Images::new());
        assert!(needs.iter().any(|n| matches!(n, Need::LoadPage(2))));
        v.page_loaded(0, 2, pdf::display_list(&doc, 2).unwrap());
        assert!(v.display_list(2).is_some() && v.display_list(0).is_none());
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
