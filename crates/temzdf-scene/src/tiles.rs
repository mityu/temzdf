//! Showing content painted in tiles by worker threads, shared by panes
//! that draw documents of their own (PDF pages, Markdown pages): the
//! painted tiles with the work in flight ([`TileCache`]), which tiles a
//! view needs and how urgently ([`grid`], [`Schedule`]), and the threads
//! doing the work from a queue replaced every frame ([`WorkPool`]). What
//! a tile is and how it is painted is the content's.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::{ImageKey, Images, Rect};

struct Cached {
    image: ImageKey,
    layer: u32,
    bytes: usize,
    last_used: u64,
}

/// Painted tiles by key, as images in the store, with the work asked for
/// and not yet arrived. Shared by the views of one document; tiles least
/// recently used go when over the budget.
pub struct TileCache<K> {
    tiles: HashMap<K, Cached>,
    in_flight: HashSet<K>,
    /// Work in flight when the cache was forgotten: dropped when it
    /// arrives (nothing shows it; it would only hold memory).
    unwanted: HashSet<K>,
    used_bytes: usize,
    /// Clock for least-recently-used eviction, advanced once per composed
    /// frame: when a view that drew at this tick draws again (the views of
    /// one frame share a tick).
    clock: u64,
    /// The views that drew at this tick.
    views: HashSet<u64>,
    /// Bytes of tiles kept at most.
    pub budget: usize,
    /// Placement keys tiles get, in turn (see [`TileCache::alloc_layer`]).
    layers: std::ops::Range<u32>,
    next_layer: u32,
}

impl<K: Copy + Eq + Hash> TileCache<K> {
    /// A cache keeping `budget` bytes of tiles, whose placements take keys
    /// from `layers`.
    pub fn new(budget: usize, layers: std::ops::Range<u32>) -> Self {
        Self {
            tiles: HashMap::new(),
            in_flight: HashSet::new(),
            unwanted: HashSet::new(),
            used_bytes: 0,
            clock: 0,
            views: HashSet::new(),
            budget,
            next_layer: layers.start,
            layers,
        }
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn contains(&self, key: K) -> bool {
        self.tiles.contains_key(&key)
    }

    pub fn is_in_flight(&self, key: K) -> bool {
        self.in_flight.contains(&key)
    }

    /// Nothing asked for is still being painted.
    pub fn is_idle(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// The work for `key` was handed to the threads.
    pub fn mark_in_flight(&mut self, key: K) {
        self.in_flight.insert(key);
    }

    /// The work for `key` was dropped before it ran, or its result is not
    /// wanted after all.
    pub fn cancelled(&mut self, key: K) {
        self.in_flight.remove(&key);
        self.unwanted.remove(&key);
    }

    /// A tile arrived: into the store, unless nothing wants it (the cache
    /// was forgotten meanwhile, or it is there already). Whether it was
    /// kept.
    pub fn rendered(&mut self, images: &mut Images, key: K, width: u32, height: u32, pixels: Vec<u8>) -> bool {
        self.in_flight.remove(&key);
        if self.unwanted.remove(&key) || self.tiles.contains_key(&key) {
            return false;
        }
        let bytes = pixels.len();
        let image = images.add(width, height, pixels);
        let layer = self.alloc_layer();
        self.used_bytes += bytes;
        self.tiles.insert(key, Cached { image, layer, bytes, last_used: self.clock });
        true
    }

    /// Nothing shows the content for now: its tiles go. Work in flight
    /// still arrives, and is dropped then.
    pub fn forget(&mut self, images: &mut Images) {
        self.drop_tiles(images, |_| true);
        self.unwanted.extend(self.in_flight.iter().copied());
    }

    /// Every tile goes, and the work in flight is no longer counted (its
    /// results are told apart by their keys, as a new version's are).
    pub fn clear(&mut self, images: &mut Images) {
        self.drop_tiles(images, |_| true);
        self.in_flight.clear();
    }

    /// The tiles `drop` picks go (work in flight is left alone).
    pub fn drop_tiles(&mut self, images: &mut Images, mut drop: impl FnMut(&K) -> bool) {
        let used = &mut self.used_bytes;
        self.tiles.retain(|k, c| {
            let go = drop(k);
            if go {
                images.remove(c.image);
                *used -= c.bytes;
            }
            !go
        });
    }

    /// View `view` starts drawing: tiles touched from now on count as used
    /// by this frame. Several views (panes) drawing one composed frame
    /// share its tick, so none evicts what another just used.
    pub fn begin_frame(&mut self, view: u64) {
        if !self.views.insert(view) {
            self.clock += 1;
            self.views.clear();
            self.views.insert(view);
        }
    }

    /// A tile's image and placement key, marking it used this frame.
    pub fn touch(&mut self, key: K) -> Option<(ImageKey, u32)> {
        let clock = self.clock;
        self.tiles.get_mut(&key).map(|c| {
            c.last_used = clock;
            (c.image, c.layer)
        })
    }

    /// The tiles and when each was last used (the clock's tick).
    pub fn iter_used(&self) -> impl Iterator<Item = (&K, u64)> {
        self.tiles.iter().map(|(k, c)| (k, c.last_used))
    }

    /// A placement key for an image shown in a view (tiles get theirs when
    /// they arrive): the next of the cache's keys, around when they run out.
    pub fn alloc_layer(&mut self) -> u32 {
        let l = self.next_layer;
        self.next_layer = if l + 1 >= self.layers.end { self.layers.start } else { l + 1 };
        l
    }

    /// Drop least recently used tiles over budget, never ones used this
    /// frame or the one before (views that have not drawn this frame yet
    /// still show those).
    pub fn evict(&mut self, images: &mut Images) {
        if self.used_bytes <= self.budget {
            return;
        }
        let mut old: Vec<(u64, K)> =
            self.tiles.iter().filter(|(_, c)| c.last_used + 1 < self.clock).map(|(k, c)| (c.last_used, *k)).collect();
        old.sort_by_key(|(t, _)| *t);
        for (_, key) in old {
            if self.used_bytes <= self.budget {
                break;
            }
            if let Some(c) = self.tiles.remove(&key) {
                images.remove(c.image);
                self.used_bytes -= c.bytes;
            }
        }
    }
}

/// One tile of a grid laid over content shown on a screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub tx: u32,
    pub ty: u32,
    /// The tile in the content's pixels: its top-left and size.
    pub area: (u32, u32, u32, u32),
    /// Where it goes on the screen.
    pub dst: Rect,
    /// On the screen (else only near it).
    pub on_screen: bool,
}

/// The tiles (`tile` pixels, the last ones in a row or column smaller) of
/// content `size` pixels whose top-left is at `origin` on the screen,
/// that are on `screen` or within a screen of it every way: those to show
/// and those to paint ahead.
pub fn grid(origin: (i32, i32), size: (u32, u32), tile: (u32, u32), screen: Rect) -> impl Iterator<Item = Cell> {
    let (tw, th) = (tile.0.max(1), tile.1.max(1));
    let (w, h) = (screen.w as f32, screen.h as f32);
    (0..size.1.div_ceil(th)).flat_map(move |ty| {
        (0..size.0.div_ceil(tw)).filter_map(move |tx| {
            let area = (tx * tw, ty * th, tw.min(size.0 - tx * tw), th.min(size.1 - ty * th));
            let dst = Rect::new(origin.0 + area.0 as i32, origin.1 + area.1 as i32, area.2, area.3);
            let on_screen = dst.intersect(&screen).is_some();
            let near = dst.y as f32 <= screen.y as f32 + h + h
                && dst.bottom() as f32 >= screen.y as f32 - h
                && dst.x as f32 <= screen.x as f32 + w * 2.0
                && dst.right() as f32 >= screen.x as f32 - w;
            (on_screen || near).then_some(Cell { tx, ty, area, dst, on_screen })
        })
    })
}

/// A frame's work, most urgent first: what is on the screen before what
/// is near it, each by a priority (smaller first, as the distance of a
/// tile from the screen's centre).
pub struct Schedule<N> {
    urgent: Vec<(f32, N)>,
    later: Vec<(f32, N)>,
}

impl<N> Default for Schedule<N> {
    fn default() -> Self {
        Self { urgent: Vec::new(), later: Vec::new() }
    }
}

impl<N> Schedule<N> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, urgent: bool, priority: f32, need: N) {
        if urgent { &mut self.urgent } else { &mut self.later }.push((priority, need));
    }

    /// Keep only the work `keep` picks.
    pub fn retain(&mut self, mut keep: impl FnMut(&N) -> bool) {
        self.urgent.retain(|(_, n)| keep(n));
        self.later.retain(|(_, n)| keep(n));
    }

    pub fn is_empty(&self) -> bool {
        self.urgent.is_empty() && self.later.is_empty()
    }

    /// The work in order.
    pub fn into_sorted(mut self) -> impl Iterator<Item = N> {
        self.urgent.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.later.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.urgent.into_iter().chain(self.later).map(|(_, n)| n)
    }
}

/// Work a [`WorkPool`] does.
pub trait Job: Send + 'static {
    type Key: Send + 'static;
    type Output: Send + 'static;
    fn key(&self) -> Self::Key;
    fn run(self) -> Self::Output;
}

/// A frame's want of a tile: work to queue, or work queued before to
/// keep (where the frame wants it now).
pub enum Want<J: Job> {
    New(J),
    Queued(J::Key),
}

/// Where a pool's results go; false when nobody listens any more (the
/// thread ends).
pub type JobSink<O> = Arc<dyn Fn(O) -> bool + Send + Sync>;

type Queue<J> = Arc<(Mutex<VecDeque<J>>, Condvar)>;

/// Worker threads with a replaceable queue: each frame the views' whole
/// prioritised list of missing work replaces what is queued, and work no
/// longer wanted is dropped before it starts.
pub struct WorkPool<J: Job> {
    shared: Queue<J>,
    stop: Arc<AtomicBool>,
}

impl<J: Job> Drop for WorkPool<J> {
    /// Let the threads finish once their current job is done.
    fn drop(&mut self) {
        // Under the lock: a worker between checking `stop` and waiting would
        // otherwise miss the notification and wait forever.
        let _queue = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        self.stop.store(true, Ordering::Relaxed);
        self.shared.1.notify_all();
    }
}

impl<J: Job> WorkPool<J> {
    /// `threads` threads named `name-N`, results to `sink`.
    pub fn new(name: &str, threads: usize, sink: JobSink<J::Output>) -> std::io::Result<Self> {
        let shared: Queue<J> = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        for i in 0..threads.max(1) {
            let (shared, stop, sink) = (shared.clone(), stop.clone(), sink.clone());
            std::thread::Builder::new().name(format!("{name}-{i}")).spawn(move || {
                loop {
                    let job = {
                        let (lock, cv) = &*shared;
                        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            if let Some(job) = q.pop_front() {
                                break job;
                            }
                            q = cv.wait(q).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    if !sink(job.run()) {
                        break;
                    }
                }
            })?;
        }
        Ok(Self { shared, stop })
    }

    /// Replace queued (not yet started) jobs with `jobs`. Returns the keys
    /// of the jobs that were dropped.
    pub fn replace(&self, jobs: Vec<J>) -> Vec<J::Key> {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        let dropped = q.drain(..).map(|j| j.key()).collect();
        q.extend(jobs);
        cv.notify_all();
        dropped
    }

    /// Replace the queue with what a frame wants, in its order: new jobs,
    /// and jobs already queued that it still wants (a job wanted but not
    /// queued is running or done). Returns the keys of the queued jobs
    /// not wanted any more, dropped. (Replacing with new jobs alone would
    /// drop work asked for by an earlier frame not started yet, which no
    /// frame asks for again while it counts as in flight.)
    pub fn replace_wanted(&self, wants: Vec<Want<J>>) -> Vec<J::Key>
    where
        J::Key: PartialEq,
    {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut old: Vec<J> = q.drain(..).collect();
        for w in wants {
            match w {
                Want::New(j) => q.push_back(j),
                Want::Queued(k) => {
                    if let Some(i) = old.iter().position(|j| j.key() == k) {
                        q.push_back(old.remove(i));
                    }
                }
            }
        }
        cv.notify_all();
        old.iter().map(Job::key).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn tile(cache: &mut TileCache<u32>, images: &mut Images, key: u32) {
        cache.mark_in_flight(key);
        assert!(cache.rendered(images, key, 10, 10, vec![0; 400]));
    }

    #[test]
    fn tiles_arriving_after_the_cache_was_forgotten_are_dropped() {
        let mut images = Images::new();
        let mut cache = TileCache::new(1 << 20, 1..100);
        cache.mark_in_flight(7);
        cache.forget(&mut images);
        assert!(!cache.rendered(&mut images, 7, 10, 10, vec![0; 400]));
        assert!(cache.is_empty() && images.is_empty() && cache.is_idle(), "nothing shows it");
        // Shown again, the same tile is wanted.
        assert!(cache.rendered(&mut images, 7, 10, 10, vec![0; 400]));
        assert_eq!(cache.len(), 1);
        // Twice is once.
        assert!(!cache.rendered(&mut images, 7, 10, 10, vec![0; 400]));
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn cancelled_work_is_neither_in_flight_nor_unwanted() {
        let mut images = Images::new();
        let mut cache = TileCache::new(1 << 20, 1..100);
        cache.mark_in_flight(1);
        cache.forget(&mut images);
        cache.cancelled(1);
        assert!(!cache.is_in_flight(1) && cache.is_idle());
        assert!(cache.rendered(&mut images, 1, 10, 10, vec![0; 400]), "not dropped as unwanted");
    }

    #[test]
    fn views_of_one_frame_keep_each_others_tiles_and_the_rest_goes_over_budget() {
        let mut images = Images::new();
        let mut cache = TileCache::new(1000, 1..100); // two tiles
        tile(&mut cache, &mut images, 0);
        tile(&mut cache, &mut images, 1);
        // Frame after frame, pane 1 shows tile 0 and pane 2 tile 1; a third
        // tile pushes the cache over its budget.
        for frame in 0..4 {
            cache.begin_frame(1);
            assert!(cache.touch(0).is_some(), "frame {frame}: pane 1 lost its tile");
            cache.evict(&mut images);
            cache.begin_frame(2);
            assert!(cache.touch(1).is_some(), "frame {frame}: pane 2 lost its tile");
            tile(&mut cache, &mut images, 10 + frame);
            cache.evict(&mut images);
        }
        assert!(cache.len() <= 4, "the others went: {}", cache.len());
    }

    #[test]
    fn placement_keys_go_around_their_range() {
        let mut images = Images::new();
        let mut cache = TileCache::new(1 << 20, 5..7);
        let keys: Vec<u32> = (0..3)
            .map(|k| {
                tile(&mut cache, &mut images, k);
                cache.touch(k).unwrap().1
            })
            .collect();
        assert_eq!(keys, [5, 6, 5]);
        assert_eq!(cache.alloc_layer(), 6);
    }

    #[test]
    fn grid_gives_the_tiles_on_and_within_a_screen_of_the_screen() {
        // Content 100 x 1000 at (0, -300), tiles 100 x 64, a 100 x 200 screen.
        let screen = Rect::new(0, 0, 100, 200);
        let cells: Vec<Cell> = grid((0, -300), (100, 1000), (100, 64), screen).collect();
        let rows = |on: bool| cells.iter().filter(|c| c.on_screen == on).map(|c| c.ty).collect::<Vec<_>>();
        // Rows 4..=7 are on the screen (y -44..212), rows from y -200 to 400 near.
        assert_eq!(rows(true), [4, 5, 6, 7]);
        assert_eq!(rows(false), [1, 2, 3, 8, 9, 10]);
        let last = grid((0, -900), (100, 1000), (100, 64), screen).last().unwrap();
        assert_eq!((last.ty, last.area, last.dst), (15, (0, 960, 100, 40), Rect::new(0, 60, 100, 40)));
    }

    #[test]
    fn a_schedule_puts_the_screen_first_then_the_nearest() {
        let mut s = Schedule::new();
        s.push(false, 1.0, "near");
        s.push(true, 5.0, "far on screen");
        s.push(true, 2.0, "on screen");
        s.push(false, 0.5, "nearer");
        s.push(true, 9.0, "gone");
        s.retain(|n| *n != "gone");
        assert_eq!(s.into_sorted().collect::<Vec<_>>(), ["on screen", "far on screen", "nearer", "near"]);
    }

    struct Wait(u32, Arc<(Mutex<bool>, Condvar)>);

    impl Job for Wait {
        type Key = u32;
        type Output = u32;
        fn key(&self) -> u32 {
            self.0
        }
        fn run(self) -> u32 {
            let (lock, cv) = &*self.1;
            let mut go = lock.lock().unwrap();
            while !*go {
                go = cv.wait(go).unwrap();
            }
            self.0
        }
    }

    #[test]
    fn replacing_the_queue_drops_the_jobs_not_started() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let pool = WorkPool::new("test-pool", 1, Arc::new(move |n| tx.lock().unwrap().send(n).is_ok())).unwrap();
        pool.replace(vec![Wait(1, gate.clone())]);
        // Job 1 is running (or about to): wait until the queue is empty.
        while !pool.shared.0.lock().unwrap().is_empty() {
            std::thread::yield_now();
        }
        assert!(pool.replace(vec![Wait(2, gate.clone()), Wait(3, gate.clone())]).is_empty());
        assert_eq!(pool.replace(vec![Wait(4, gate.clone())]), [2, 3]);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let got: Vec<u32> = (0..2).map(|_| rx.recv_timeout(Duration::from_secs(10)).unwrap()).collect();
        assert_eq!(got, [1, 4]);
    }

    #[test]
    fn wanted_work_already_queued_stays_in_the_new_order() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let pool = WorkPool::new("test-pool", 1, Arc::new(move |n| tx.lock().unwrap().send(n).is_ok())).unwrap();
        pool.replace(vec![Wait(1, gate.clone())]);
        while !pool.shared.0.lock().unwrap().is_empty() {
            std::thread::yield_now();
        }
        pool.replace(vec![Wait(2, gate.clone()), Wait(3, gate.clone())]);
        // 3 still wanted (first now), 2 not, 1 is running, 4 is new.
        let wants = vec![Want::Queued(3), Want::Queued(1), Want::New(Wait(4, gate.clone()))];
        assert_eq!(pool.replace_wanted(wants), [2]);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let got: Vec<u32> = (0..3).map(|_| rx.recv_timeout(Duration::from_secs(10)).unwrap()).collect();
        assert_eq!(got, [1, 3, 4]);
    }
}
