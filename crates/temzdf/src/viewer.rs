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
use termpix::geom::{Geometry, Rect};
use termpix::scroll::KineticScroll;

use crate::layout::{Layout, View};
use crate::pdf::{PageBounds, PixelRect};

/// Tile edge in pixels.
pub const TILE: u32 = 512;
/// Thumbnail width in pixels, shown scaled while sharp tiles are missing.
const THUMB_WIDTH: f32 = 320.0;
/// Memory budget for cached tiles (the terminal keeps its own copies).
const TILE_BUDGET: usize = 192 << 20;
pub const MIN_SCALE: f32 = 0.1;
pub const MAX_SCALE: f32 = 8.0;

// Everything we draw sits below text (and below cells with a background colour),
// so UI drawn in terminal cells would cover it.
const Z_BACKGROUND: i32 = -1_073_741_900;
const Z_THUMB: i32 = -1_073_741_890;
const Z_TILE: i32 = -1_073_741_880;
const BACKGROUND: [u8; 4] = [0x3a, 0x3a, 0x3a, 0xff];
const LAYER_BACKGROUND: u32 = 1;

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
}

impl Viewer {
    pub fn new(bounds: Vec<PageBounds>, geometry: Geometry) -> Self {
        let layout = Layout::new(&bounds.iter().map(|b| (b.width(), b.height())).collect::<Vec<_>>());
        let screen_w = geometry.width_px() as f32;
        let view = View {
            scale: View::fit_width_scale(&layout, screen_w).clamp(MIN_SCALE, MAX_SCALE),
            scroll_x: 0.0,
            scroll_y: 0.0,
            screen_w,
            screen_h: geometry.height_px() as f32,
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
        let Some(b) = self.layout.pages.get(page.min(self.page_count().saturating_sub(1))) else { return };
        let target = (b.y - crate::layout::GAP_PT) * self.view.scale;
        if animate {
            self.scroll_y.scroll_by(target as f64 - self.scroll_y.target(), now);
        } else {
            self.scroll_y.jump_to(target as f64);
            self.sync_view();
        }
    }

    /// Multiply the zoom by `factor`, keeping the screen point `anchor` fixed.
    pub fn zoom(&mut self, factor: f32, anchor: (f32, f32)) {
        self.set_scale(self.view.scale * factor, anchor);
    }

    pub fn set_scale(&mut self, scale: f32, anchor: (f32, f32)) {
        // Settle any animation first so the anchor maths uses the displayed position.
        self.jump_to(self.view.scroll_x, self.view.scroll_y);
        self.view.zoom_at(&self.layout, scale.clamp(MIN_SCALE, MAX_SCALE), anchor.0, anchor.1);
        self.update_scroll_bounds();
        self.jump_to(self.view.scroll_x, self.view.scroll_y);
    }

    pub fn fit_width(&mut self) {
        let scale = View::fit_width_scale(&self.layout, self.view.screen_w);
        self.set_scale(scale, (self.view.screen_w / 2.0, 0.0));
    }

    /// The terminal was resized: keep the doc point at the top-left in place.
    pub fn resize(&mut self, geometry: Geometry) {
        let (dx, dy) = self.view.screen_to_doc(&self.layout, 0.0, 0.0);
        self.view.screen_w = geometry.width_px() as f32;
        self.view.screen_h = geometry.height_px() as f32;
        let (sx, sy) = self.view.doc_to_screen(&self.layout, dx, dy);
        self.view.scroll_x += sx;
        self.view.scroll_y += sy;
        self.view.clamp(&self.layout);
        self.update_scroll_bounds();
        self.jump_to(self.view.scroll_x, self.view.scroll_y);
    }

    pub fn page_loaded(&mut self, page: usize, list: Arc<DisplayList>) {
        if let Some(slot) = self.lists.get_mut(page) {
            *slot = Some(list);
        }
        self.lists_requested.remove(&page);
    }

    /// A page failed to load; forget the request so it is retried later.
    pub fn page_failed(&mut self, page: usize) {
        self.lists_requested.remove(&page);
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
        if self.tiles.contains_key(&r.key) {
            return;
        }
        let bytes = r.pixels.len();
        let image = comp.add_image(r.width, r.height, r.pixels);
        let layer = self.next_layer;
        self.next_layer = self.next_layer.checked_add(1).unwrap_or(LAYER_BACKGROUND + 1);
        self.used_bytes += bytes;
        self.tiles.insert(r.key, Cached { image, layer, bytes, last_used: self.frame_no });
    }

    /// Build this frame's layers and the outstanding work, most urgent first.
    pub fn frame(&mut self, comp: &mut Compositor) -> (Vec<Layer>, Vec<Need>) {
        self.frame_no += 1;
        self.sync_view();
        let view = self.view;
        let screen = Rect::new(0, 0, view.screen_w as u32, view.screen_h as u32);
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

        for page in prefetch.clone() {
            let is_visible = visible.contains(&page);
            let queue = if is_visible { &mut urgent } else { &mut later };
            let Some(list) = self.lists[page].clone() else {
                if !self.lists_requested.contains(&page) {
                    queue.push((0.0, Need::LoadPage(page)));
                }
                continue;
            };
            let bounds = self.bounds[page];
            let pb = self.layout.pages[page];
            let (sx, sy) = view.doc_to_screen(&self.layout, pb.x, pb.y);
            let (sx, sy) = (sx.round() as i32, sy.round() as i32);
            let (pw, ph) = bounds.pixel_size(scale);
            let page_rect = Rect::new(sx, sy, pw, ph);

            // Thumbnail first: it is cheap and covers the page while tiles render.
            let thumb_key = TileKey { page: page as u32, kind: TileKind::Thumb };
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
                    let key = TileKey { page: page as u32, kind: TileKind::Tile { scale_q, tx, ty } };
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
            if missing_on_screen && let Some((image, layer)) = thumb_cached {
                let (tw, th) = bounds.pixel_size(thumb_scale);
                layers.push(Layer { key: layer, image, src: Rect::new(0, 0, tw, th), dst: page_rect, z: Z_THUMB });
            }
        }

        urgent.sort_by(|a, b| a.0.total_cmp(&b.0));
        later.sort_by(|a, b| a.0.total_cmp(&b.0));
        let needs = urgent.into_iter().chain(later).map(|(_, n)| n).collect();
        self.evict(comp);
        (layers, needs)
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
