//! The view-independent part of showing a document: per-page display lists and
//! rendered tiles, with the bookkeeping of work in flight. Views (see
//! `viewer.rs`) decide what to show and ask for what is missing; several views
//! of the same document can share one cache (see `doc/session-server.md`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mupdf::DisplayList;
use temzdf_scene::{ImageKey, Images};

use crate::pdf::{PageBounds, PixelRect};

/// Tile edge in pixels.
pub const TILE: u32 = 512;
/// Memory budget for cached tiles (frontends keep their own copies).
const TILE_BUDGET: usize = 192 << 20;
/// Display lists kept at most (about 70 KiB each for a LaTeX text page, and
/// 0.2 ms to load again); the ones farthest from the view go first.
pub const MAX_LISTS: usize = 256;
/// Layer key of a view's background; tile and stand-in placement keys follow.
pub const LAYER_BACKGROUND: u32 = 1;
/// Layer keys at or above this are reserved for UI chrome (see `temzdf-pdfview`'s `pane.rs`).
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

/// Work a view needs done, most urgent first.
pub enum Need {
    LoadPage(usize),
    Render(RenderJob),
}

struct Cached {
    image: ImageKey,
    layer: u32,
    bytes: usize,
    last_used: u64,
}

pub fn quantize(scale: f32) -> u32 {
    (scale * 1024.0).round().max(1.0) as u32
}

pub fn dequantize(q: u32) -> f32 {
    q as f32 / 1024.0
}

pub struct DocCache {
    pub bounds: Vec<PageBounds>,
    lists: Vec<Option<Arc<DisplayList>>>,
    lists_requested: HashSet<usize>,
    tiles: HashMap<TileKey, Cached>,
    in_flight: HashSet<TileKey>,
    used_bytes: usize,
    /// Clock for least-recently-used eviction, advanced once per frame.
    clock: u64,
    next_layer: u32,
    generation: u32,
    /// See [`MAX_LISTS`].
    pub max_lists: usize,
}

impl DocCache {
    pub fn new(bounds: Vec<PageBounds>) -> Self {
        let n = bounds.len();
        Self {
            bounds,
            lists: vec![None; n],
            lists_requested: HashSet::new(),
            tiles: HashMap::new(),
            in_flight: HashSet::new(),
            used_bytes: 0,
            clock: 0,
            next_layer: LAYER_BACKGROUND + 1,
            generation: 0,
            max_lists: MAX_LISTS,
        }
    }

    pub fn page_count(&self) -> usize {
        self.bounds.len()
    }

    /// Generation of the document being shown (see [`TileKey::generation`]).
    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn display_list(&self, page: usize) -> Option<Arc<DisplayList>> {
        self.lists.get(page)?.clone()
    }

    /// Whether loading the page's display list was asked for and is pending.
    pub fn is_requested(&self, page: usize) -> bool {
        self.lists_requested.contains(&page)
    }

    /// A display list arrived. Lists for an older generation are ignored. Keeps
    /// at most `max_lists`, dropping the ones farthest from `current` (the page
    /// being read) but never those in `in_use`; they load again when needed.
    pub fn page_loaded(
        &mut self,
        generation: u32,
        page: usize,
        list: Arc<DisplayList>,
        current: usize,
        in_use: std::ops::Range<usize>,
    ) {
        if generation != self.generation {
            return;
        }
        if let Some(slot) = self.lists.get_mut(page) {
            *slot = Some(list);
        }
        self.lists_requested.remove(&page);
        let loaded: Vec<usize> = (0..self.lists.len()).filter(|&p| self.lists[p].is_some()).collect();
        if loaded.len() <= self.max_lists {
            return;
        }
        let mut by_distance: Vec<usize> = loaded.into_iter().filter(|p| !in_use.contains(p)).collect();
        by_distance.sort_by_key(|&p| std::cmp::Reverse(p.abs_diff(current)));
        let excess = by_distance.len().saturating_sub(self.max_lists.saturating_sub(in_use.len()));
        for p in by_distance.into_iter().take(excess) {
            self.lists[p] = None;
        }
    }

    /// A page failed to load; forget the request so it is retried later.
    pub fn page_failed(&mut self, generation: u32, page: usize) {
        if generation == self.generation {
            self.lists_requested.remove(&page);
        }
    }

    /// A new version of the document: forget every list and tile.
    pub fn replace(&mut self, images: &mut Images, bounds: Vec<PageBounds>) {
        self.generation = self.generation.wrapping_add(1);
        for (_, c) in self.tiles.drain() {
            images.remove(c.image);
        }
        self.used_bytes = 0;
        self.in_flight.clear();
        self.lists = vec![None; bounds.len()];
        self.lists_requested.clear();
        self.bounds = bounds;
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

    pub fn is_in_flight(&self, key: TileKey) -> bool {
        self.in_flight.contains(&key)
    }

    pub fn rendered(&mut self, images: &mut Images, r: Rendered) {
        self.in_flight.remove(&r.key);
        if r.key.generation != self.generation || self.tiles.contains_key(&r.key) {
            return;
        }
        let bytes = r.pixels.len();
        let image = images.add(r.width, r.height, r.pixels);
        let layer = self.alloc_layer();
        self.used_bytes += bytes;
        self.tiles.insert(r.key, Cached { image, layer, bytes, last_used: self.clock });
    }

    /// Start a frame: tiles touched from now on count as used by it.
    pub fn begin_frame(&mut self) {
        self.clock += 1;
    }

    /// A cached tile's image and placement key, marking it used this frame.
    pub fn touch(&mut self, key: TileKey) -> Option<(ImageKey, u32)> {
        let clock = self.clock;
        self.tiles.get_mut(&key).map(|c| {
            c.last_used = clock;
            (c.image, c.layer)
        })
    }

    /// The other scale a page's tiles were used at most recently, if any.
    pub fn latest_other_scale(&self, page: usize, scale_q: u32) -> Option<u32> {
        let generation = self.generation;
        self.tiles
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
    }

    /// A placement key for an image shown in a view (tiles get theirs when
    /// they arrive).
    pub fn alloc_layer(&mut self) -> u32 {
        let layer = self.next_layer;
        self.next_layer =
            if self.next_layer + 1 >= LAYER_KEYS_END { LAYER_BACKGROUND + 1 } else { self.next_layer + 1 };
        layer
    }

    /// Drop least recently used tiles over budget, never ones used this frame.
    pub fn evict(&mut self, images: &mut Images) {
        if self.used_bytes <= TILE_BUDGET {
            return;
        }
        let mut old: Vec<(u64, TileKey)> =
            self.tiles.iter().filter(|(_, c)| c.last_used < self.clock).map(|(k, c)| (c.last_used, *k)).collect();
        old.sort_by_key(|(t, _)| *t);
        for (_, key) in old {
            if self.used_bytes <= TILE_BUDGET {
                break;
            }
            if let Some(c) = self.tiles.remove(&key) {
                images.remove(c.image);
                self.used_bytes -= c.bytes;
            }
        }
    }
}
