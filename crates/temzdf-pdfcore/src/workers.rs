//! Background threads: the document thread (owns the non-`Send` MuPDF
//! `Document`, loads display lists), the raster pool (turns display lists into
//! tiles), search, file watching and SyncTeX. Results come back as
//! [`DocEvent`]s through a [`Sink`], so the caller can tag them (e.g. with the
//! document they belong to) on the way into its own event channel.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};

use mupdf::DisplayList;

use crate::doc_cache::{RenderJob, Rendered, TileKey};
use crate::pdf::{self, DocInfo, Link, PageRect};
use crate::synctex::{PdfBox, Scanner, SourceLocation};

/// Where worker threads send their results. Returns false once nobody listens
/// any more (the thread then stops).
pub type Sink = Arc<dyn Fn(DocEvent) -> bool + Send + Sync>;

/// A sink feeding a channel, converting events on the way.
pub fn channel_sink<E: Send + 'static>(tx: Sender<E>, wrap: impl Fn(DocEvent) -> E + Send + Sync + 'static) -> Sink {
    let tx = std::sync::Mutex::new(tx);
    Arc::new(move |e| tx.lock().unwrap_or_else(|p| p.into_inner()).send(wrap(e)).is_ok())
}

pub enum DocEvent {
    Opened(Result<DocInfo, String>),
    /// A display list (and the page's links) for document generation `.0`.
    PageLoaded(u32, usize, Arc<DisplayList>, Vec<Link>),
    PageFailed(u32, usize, String),
    Rendered(Rendered),
    RenderFailed(TileKey, String),
    SearchHits {
        generation: u64,
        page: usize,
        rects: Vec<PageRect>,
    },
    SearchFailed(String),
    /// The watched file (or its directory entry) changed.
    FileChanged,
    /// The document thread now holds the reloaded document.
    Reloaded(DocInfo),
    /// Reloading failed (e.g. the file is still being written); retry later.
    ReloadFailed(String),
    ForwardResult(Vec<PdfBox>),
    InverseResult(SourceLocation),
    SynctexFailed(String),
}

pub enum SynctexCmd {
    /// Where `line` (and `column`, 0 if unknown) of `file` ended up.
    Forward {
        file: PathBuf,
        line: u32,
        column: u32,
        page_hint: usize,
    },
    Inverse {
        page: usize,
        x: f32,
        y: f32,
    },
    /// The PDF was rebuilt: read the SyncTeX file again on next use.
    Reload,
}

/// SyncTeX queries run here: parsing a big `.synctex.gz` takes a while and must
/// not stall drawing. The file is (re)read lazily on the first query after
/// start-up or a reload.
pub fn spawn_synctex(pdf: PathBuf, events: Sink) -> std::io::Result<Sender<SynctexCmd>> {
    let (tx, rx) = mpsc::channel::<SynctexCmd>();
    std::thread::Builder::new().name("temzdf-synctex".into()).spawn(move || {
        let mut scanner: Option<Scanner> = None;
        for cmd in rx {
            if matches!(cmd, SynctexCmd::Reload) {
                scanner = None;
                continue;
            }
            if scanner.is_none() {
                scanner = Scanner::open(&pdf);
            }
            let Some(s) = scanner.as_mut() else {
                let name = pdf.with_extension("synctex.gz");
                let msg = format!("no SyncTeX data ({} missing; compile with -synctex=1)", name.display());
                if !events(DocEvent::SynctexFailed(msg)) {
                    return;
                }
                continue;
            };
            let ev = match cmd {
                SynctexCmd::Forward { file, line, column, page_hint } => {
                    let boxes = s.forward(&file, line, column, Some(page_hint));
                    if boxes.is_empty() {
                        DocEvent::SynctexFailed(format!("{}:{} is not in the PDF", file.display(), line))
                    } else {
                        DocEvent::ForwardResult(boxes)
                    }
                }
                SynctexCmd::Inverse { page, x, y } => match s.inverse(page, x, y) {
                    Some(loc) => DocEvent::InverseResult(loc),
                    None => DocEvent::SynctexFailed("no source location here".into()),
                },
                SynctexCmd::Reload => continue,
            };
            if !events(ev) {
                return;
            }
        }
    })?;
    Ok(tx)
}

/// A search request: find `query` on every page, starting at `start_page`.
pub struct SearchCmd {
    pub generation: u64,
    pub query: String,
    pub start_page: usize,
    /// Document generation; the worker reopens the file when it changes.
    pub doc_generation: u32,
}

/// The search worker opens its own copy of the document so long searches do not
/// delay page loading. A newer command interrupts the current search.
pub fn spawn_search(path: PathBuf, events: Sink) -> std::io::Result<Sender<SearchCmd>> {
    let (tx, rx) = mpsc::channel::<SearchCmd>();
    std::thread::Builder::new().name("temzdf-search".into()).spawn(move || {
        let mut doc: Option<(u32, pdf::Doc, Vec<pdf::PageBounds>)> = None;
        let mut pending = None;
        loop {
            let cmd = match pending.take() {
                Some(c) => c,
                None => match rx.recv() {
                    Ok(c) => c,
                    Err(_) => return,
                },
            };
            if doc.as_ref().is_none_or(|d| d.0 != cmd.doc_generation) {
                match pdf::open(&path).and_then(|d| pdf::page_bounds(&d).map(|b| (d, b))) {
                    Ok((d, b)) => doc = Some((cmd.doc_generation, d, b)),
                    Err(e) => {
                        events(DocEvent::SearchFailed(format!("{e:#}")));
                        continue;
                    }
                }
            }
            let Some((_, doc, bounds)) = doc.as_ref() else { continue };
            let n = bounds.len();
            for i in 0..n {
                // A newer query replaces this one.
                if let Some(newer) = rx.try_iter().last() {
                    pending = Some(newer);
                    break;
                }
                let page = (cmd.start_page + i) % n;
                let ev = match pdf::search_page(doc, page, bounds[page], &cmd.query) {
                    Ok(rects) => DocEvent::SearchHits { generation: cmd.generation, page, rects },
                    Err(e) => DocEvent::SearchFailed(format!("{e:#}")),
                };
                if !events(ev) {
                    return;
                }
            }
        }
    })?;
    Ok(tx)
}

/// Requests for the document thread.
pub enum DocCmd {
    LoadPage(usize),
    /// Open the file again; on success the new document replaces the old one.
    Reload,
}

pub fn spawn_document(path: PathBuf, events: Sink) -> std::io::Result<Sender<DocCmd>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("temzdf-doc".into()).spawn(move || {
        let (mut doc, mut bounds) = match pdf::open(&path).and_then(|d| pdf::doc_info(&d).map(|i| (d, i))) {
            Ok((doc, info)) => {
                let bounds = info.bounds.clone();
                events(DocEvent::Opened(Ok(info)));
                (doc, bounds)
            }
            Err(e) => {
                events(DocEvent::Opened(Err(format!("{e:#}"))));
                return;
            }
        };
        // Counts successful reloads; the viewer counts the same way.
        let mut generation = 0u32;
        for cmd in rx {
            let ev = match cmd {
                DocCmd::LoadPage(p) if p >= bounds.len() => continue,
                DocCmd::LoadPage(p) => match pdf::display_list(&doc, p) {
                    // A page whose links cannot be read still renders.
                    Ok(list) => DocEvent::PageLoaded(
                        generation,
                        p,
                        list,
                        pdf::page_links(&doc, p, bounds[p]).unwrap_or_default(),
                    ),
                    Err(e) => DocEvent::PageFailed(generation, p, format!("{e:#}")),
                },
                // The old document stays in use until the new one opened fine.
                DocCmd::Reload => match pdf::open(&path).and_then(|d| pdf::doc_info(&d).map(|i| (d, i))) {
                    Ok((d, info)) if !info.bounds.is_empty() => {
                        doc = d;
                        bounds = info.bounds.clone();
                        generation = generation.wrapping_add(1);
                        DocEvent::Reloaded(info)
                    }
                    Ok(_) => DocEvent::ReloadFailed("the document has no pages".into()),
                    Err(e) => DocEvent::ReloadFailed(format!("{e:#}")),
                },
            };
            if !events(ev) {
                break;
            }
        }
    })?;
    Ok(tx)
}

/// Watch `path` for changes. LaTeX tools rewrite the file in place or replace it
/// (write a new file, rename over the old one), so the parent directory is
/// watched too and events are filtered by file name; that survives the inode
/// changing. Symlinks are resolved and their target is watched as well. Keep
/// the returned watcher alive.
pub fn spawn_watcher(path: &Path, events: Sink) -> notify::Result<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};

    let abs = std::path::absolute(path)?;
    let mut targets = vec![abs.clone()];
    if let Ok(real) = std::fs::canonicalize(&abs)
        && real != abs
    {
        targets.push(real);
    }
    let names: Vec<_> = targets.iter().filter_map(|t| t.file_name().map(|n| n.to_owned())).collect();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if ev.kind.is_access() {
            return;
        }
        if ev.paths.iter().any(|p| p.file_name().is_some_and(|n| names.iter().any(|m| m == n))) {
            events(DocEvent::FileChanged);
        }
    })?;
    for t in &targets {
        if let Some(dir) = t.parent() {
            watcher.watch(dir, RecursiveMode::NonRecursive)?;
        }
    }
    Ok(watcher)
}

/// Raster worker pool with a replaceable queue: each frame the main loop submits
/// the full, prioritised list of missing tiles, and jobs no longer wanted are
/// dropped before they start.
pub struct RasterPool {
    shared: Arc<(Mutex<VecDeque<RenderJob>>, Condvar)>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for RasterPool {
    /// Let the threads finish once their current job is done.
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.shared.1.notify_all();
    }
}

impl RasterPool {
    pub fn new(threads: usize, events: Sink) -> std::io::Result<Self> {
        let shared = Arc::new((Mutex::new(VecDeque::<RenderJob>::new()), Condvar::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        for i in 0..threads.max(1) {
            let shared = shared.clone();
            let stop = stop.clone();
            let events = events.clone();
            std::thread::Builder::new().name(format!("temzdf-raster-{i}")).spawn(move || {
                loop {
                    let job = {
                        let (lock, cv) = &*shared;
                        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            if let Some(job) = q.pop_front() {
                                break job;
                            }
                            q = cv.wait(q).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    let ev = match pdf::render(&job.list, job.bounds, job.scale, job.area) {
                        Ok(pixels) => {
                            DocEvent::Rendered(Rendered { key: job.key, width: job.area.w, height: job.area.h, pixels })
                        }
                        Err(e) => DocEvent::RenderFailed(job.key, format!("{e:#}")),
                    };
                    if !events(ev) {
                        break;
                    }
                }
            })?;
        }
        Ok(Self { shared, stop })
    }

    /// Replace queued (not yet started) jobs with `jobs`. Returns the keys of the
    /// jobs that were dropped.
    pub fn replace(&self, jobs: Vec<RenderJob>) -> Vec<TileKey> {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        let dropped = q.drain(..).map(|j| j.key).collect();
        q.extend(jobs);
        cv.notify_all();
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc_cache::{TILE, TileKind};
    use crate::pdf::PixelRect;

    /// Raster throughput by pool size. Not a regular test (timing depends on
    /// the machine); run it with
    /// `cargo test --release -p temzdf-pdfcore raster_throughput -- --ignored --nocapture`
    /// (`TEMZDF_BENCH_PDF=doc.pdf` for another document).
    #[test]
    #[ignore]
    fn raster_throughput() {
        let path = std::env::var_os("TEMZDF_BENCH_PDF").map_or_else(|| pdf::fixture("sample.pdf"), PathBuf::from);
        let doc = pdf::open(&path).unwrap();
        let bounds = pdf::doc_info(&doc).unwrap().bounds;
        let pages = bounds.len().min(8);
        let lists: Vec<_> = (0..pages).map(|p| pdf::display_list(&doc, p).unwrap()).collect();
        // Fit width on a 1800 px wide (HiDPI) screen.
        let scale = 1800.0 / bounds[0].width();
        let jobs = || {
            let mut jobs = Vec::new();
            for (p, list) in lists.iter().enumerate() {
                let (pw, ph) = bounds[p].pixel_size(scale);
                for ty in 0..ph.div_ceil(TILE) {
                    for tx in 0..pw.div_ceil(TILE) {
                        let area = PixelRect {
                            x: tx * TILE,
                            y: ty * TILE,
                            w: TILE.min(pw - tx * TILE),
                            h: TILE.min(ph - ty * TILE),
                        };
                        let kind = TileKind::Tile { scale_q: 0, tx, ty };
                        let key = TileKey { generation: 0, page: p as u32, kind };
                        jobs.push(RenderJob { key, list: list.clone(), bounds: bounds[p], scale, area });
                    }
                }
            }
            jobs
        };
        let n = jobs().len();
        // Warm up: the first render of a page decodes its images (MuPDF keeps
        // them in its store), which would be charged to the first pool size.
        for job in jobs() {
            pdf::render(&job.list, job.bounds, job.scale, job.area).unwrap();
        }
        for threads in [1, 2, 4, 6, 8, 12] {
            let (tx, rx) = mpsc::channel::<DocEvent>();
            let pool = RasterPool::new(threads, channel_sink(tx, |e| e)).unwrap();
            let start = std::time::Instant::now();
            pool.replace(jobs());
            for _ in 0..n {
                assert!(matches!(rx.recv().unwrap(), DocEvent::Rendered(_)));
            }
            let dt = start.elapsed();
            eprintln!(
                "BENCH threads={threads:2} tiles={n} total={:7.1} ms  per tile={:6.2} ms  tiles/s={:6.0}",
                dt.as_secs_f64() * 1e3,
                dt.as_secs_f64() * 1e3 / n as f64,
                n as f64 / dt.as_secs_f64()
            );
        }
    }
}
