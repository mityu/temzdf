//! The view-independent part of showing a document: per-page display lists and
//! rendered tiles, with the bookkeeping of work in flight. Views (see
//! `viewer.rs`) decide what to show and ask for what is missing; several views
//! of the same document can share one cache (see `doc/design/session-server.md`).

use std::collections::HashSet;
use std::sync::Arc;

use mupdf::DisplayList;
use temzdf_scene::tiles::TileCache;
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
    /// A render asked for earlier, still in flight, that this frame still
    /// wants: kept queued if it has not started.
    Queued(TileKey),
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
    /// Pages and tiles that failed in this generation: not asked for again
    /// until the document changes (else each failure, redrawing, asks
    /// again at once, in a busy loop).
    failed_pages: HashSet<usize>,
    failed_tiles: HashSet<TileKey>,
    /// Rendered tiles (within [`TILE_BUDGET`]) and renders in flight.
    pub tiles: TileCache<TileKey>,
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
            failed_pages: HashSet::new(),
            failed_tiles: HashSet::new(),
            tiles: TileCache::new(TILE_BUDGET, LAYER_BACKGROUND + 1..LAYER_KEYS_END),
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
    /// at most `max_lists`, dropping the ones farthest from every page in
    /// `current` (the pages being read, one per view) but never those in
    /// `in_use`; they load again when needed.
    pub fn page_loaded(
        &mut self,
        generation: u32,
        page: usize,
        list: Arc<DisplayList>,
        current: &[usize],
        in_use: &[std::ops::Range<usize>],
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
        let used = |p: &usize| in_use.iter().any(|r| r.contains(p));
        let kept = loaded.iter().filter(|p| used(p)).count();
        let mut by_distance: Vec<usize> = loaded.into_iter().filter(|p| !used(p)).collect();
        let distance = |p: usize| current.iter().map(|&c| p.abs_diff(c)).min().unwrap_or(0);
        by_distance.sort_by_key(|&p| std::cmp::Reverse(distance(p)));
        let excess = by_distance.len().saturating_sub(self.max_lists.saturating_sub(kept));
        for p in by_distance.into_iter().take(excess) {
            self.lists[p] = None;
        }
    }

    /// A page failed to load: not asked for again in this generation.
    pub fn page_failed(&mut self, generation: u32, page: usize) {
        if generation == self.generation {
            self.lists_requested.remove(&page);
            self.failed_pages.insert(page);
        }
    }

    /// Whether loading the page failed in this generation.
    pub fn page_has_failed(&self, page: usize) -> bool {
        self.failed_pages.contains(&page)
    }

    /// A render job failed: not asked for again in this generation.
    pub fn render_failed(&mut self, key: TileKey) {
        self.tiles.cancelled(key);
        if key.generation == self.generation {
            self.failed_tiles.insert(key);
        }
    }

    /// Whether rendering the tile failed in this generation.
    pub fn tile_has_failed(&self, key: TileKey) -> bool {
        self.failed_tiles.contains(&key)
    }

    /// Nothing shows the document for now: drop its tiles and display lists
    /// (they load again when it is shown). Work in flight still arrives.
    pub fn forget(&mut self, images: &mut Images) {
        self.tiles.forget(images);
        self.lists.iter_mut().for_each(|l| *l = None);
    }

    /// A new version of the document: forget every list and tile.
    pub fn replace(&mut self, images: &mut Images, bounds: Vec<PageBounds>) {
        self.generation = self.generation.wrapping_add(1);
        self.tiles.clear(images);
        self.lists = vec![None; bounds.len()];
        self.lists_requested.clear();
        self.failed_pages.clear();
        self.failed_tiles.clear();
        self.bounds = bounds;
    }

    pub fn mark_requested(&mut self, need: &Need) {
        match need {
            Need::LoadPage(p) => {
                self.lists_requested.insert(*p);
            }
            Need::Render(job) => self.tiles.mark_in_flight(job.key),
            Need::Queued(_) => {}
        }
    }

    /// A render job was dropped before it ran.
    pub fn render_cancelled(&mut self, key: TileKey) {
        self.tiles.cancelled(key);
    }

    /// Nothing asked for is still being loaded or rendered.
    pub fn is_idle(&self) -> bool {
        self.tiles.is_idle() && self.lists_requested.is_empty()
    }

    pub fn is_in_flight(&self, key: TileKey) -> bool {
        self.tiles.is_in_flight(key)
    }

    /// A tile arrived (dropped when of an older version of the file).
    pub fn rendered(&mut self, images: &mut Images, r: Rendered) {
        if r.key.generation != self.generation {
            self.tiles.cancelled(r.key);
            return;
        }
        self.tiles.rendered(images, r.key, r.width, r.height, r.pixels);
    }

    /// View `view` starts drawing (see [`TileCache::begin_frame`]).
    pub fn begin_frame(&mut self, view: u64) {
        self.tiles.begin_frame(view);
    }

    /// A cached tile's image and placement key, marking it used this frame.
    pub fn touch(&mut self, key: TileKey) -> Option<(ImageKey, u32)> {
        self.tiles.touch(key)
    }

    /// The other scale a page's tiles were used at most recently, if any.
    pub fn latest_other_scale(&self, page: usize, scale_q: u32) -> Option<u32> {
        let generation = self.generation;
        self.tiles
            .iter_used()
            .filter_map(|(k, used)| match k.kind {
                TileKind::Tile { scale_q: q, .. }
                    if k.generation == generation && k.page == page as u32 && q != scale_q =>
                {
                    Some((used, q))
                }
                _ => None,
            })
            .max()
            .map(|(_, q)| q)
    }

    /// A placement key for an image shown in a view (tiles get theirs when
    /// they arrive).
    pub fn alloc_layer(&mut self) -> u32 {
        self.tiles.alloc_layer()
    }

    /// Drop least recently used tiles over budget (see [`TileCache::evict`]).
    pub fn evict(&mut self, images: &mut Images) {
        self.tiles.evict(images);
    }
}
