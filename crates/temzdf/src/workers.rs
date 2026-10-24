//! Background threads: the document thread (owns the non-`Send` MuPDF
//! `Document`, loads display lists) and the raster pool (turns display lists into
//! tiles). Results come back to the main loop as [`AppEvent`]s.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};

use mupdf::DisplayList;
use termpix::tty::TermEvent;

use crate::ipc::ForwardRequest;
use crate::pdf::{self, DocInfo, Link, PageRect};
use crate::synctex::{PdfBox, Scanner, SourceLocation};
use crate::viewer::{RenderJob, Rendered, TileKey};

pub enum AppEvent {
    Term(TermEvent),
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
    /// Another process asked for a forward search (`temzdf --forward`).
    ForwardRequest(ForwardRequest),
    ForwardResult(Vec<PdfBox>),
    InverseResult(SourceLocation),
    SynctexFailed(String),
}

pub enum SynctexCmd {
    Forward {
        req: ForwardRequest,
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
pub fn spawn_synctex(pdf: PathBuf, events: Sender<AppEvent>) -> std::io::Result<Sender<SynctexCmd>> {
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
                if events.send(AppEvent::SynctexFailed(msg)).is_err() {
                    return;
                }
                continue;
            };
            let ev = match cmd {
                SynctexCmd::Forward { req, page_hint } => {
                    let boxes = s.forward(&req.file, req.line, req.column, Some(page_hint));
                    if boxes.is_empty() {
                        AppEvent::SynctexFailed(format!("{}:{} is not in the PDF", req.file.display(), req.line))
                    } else {
                        AppEvent::ForwardResult(boxes)
                    }
                }
                SynctexCmd::Inverse { page, x, y } => match s.inverse(page, x, y) {
                    Some(loc) => AppEvent::InverseResult(loc),
                    None => AppEvent::SynctexFailed("no source location here".into()),
                },
                SynctexCmd::Reload => continue,
            };
            if events.send(ev).is_err() {
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
pub fn spawn_search(path: PathBuf, events: Sender<AppEvent>) -> std::io::Result<Sender<SearchCmd>> {
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
                        let _ = events.send(AppEvent::SearchFailed(format!("{e:#}")));
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
                    Ok(rects) => AppEvent::SearchHits { generation: cmd.generation, page, rects },
                    Err(e) => AppEvent::SearchFailed(format!("{e:#}")),
                };
                if events.send(ev).is_err() {
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

pub fn spawn_document(path: PathBuf, events: Sender<AppEvent>) -> std::io::Result<Sender<DocCmd>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("temzdf-doc".into()).spawn(move || {
        let (mut doc, mut bounds) = match pdf::open(&path).and_then(|d| pdf::doc_info(&d).map(|i| (d, i))) {
            Ok((doc, info)) => {
                let bounds = info.bounds.clone();
                let _ = events.send(AppEvent::Opened(Ok(info)));
                (doc, bounds)
            }
            Err(e) => {
                let _ = events.send(AppEvent::Opened(Err(format!("{e:#}"))));
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
                    Ok(list) => AppEvent::PageLoaded(
                        generation,
                        p,
                        list,
                        pdf::page_links(&doc, p, bounds[p]).unwrap_or_default(),
                    ),
                    Err(e) => AppEvent::PageFailed(generation, p, format!("{e:#}")),
                },
                // The old document stays in use until the new one opened fine.
                DocCmd::Reload => match pdf::open(&path).and_then(|d| pdf::doc_info(&d).map(|i| (d, i))) {
                    Ok((d, info)) if !info.bounds.is_empty() => {
                        doc = d;
                        bounds = info.bounds.clone();
                        generation = generation.wrapping_add(1);
                        AppEvent::Reloaded(info)
                    }
                    Ok(_) => AppEvent::ReloadFailed("the document has no pages".into()),
                    Err(e) => AppEvent::ReloadFailed(format!("{e:#}")),
                },
            };
            if events.send(ev).is_err() {
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
pub fn spawn_watcher(path: &Path, events: Sender<AppEvent>) -> notify::Result<notify::RecommendedWatcher> {
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
            let _ = events.send(AppEvent::FileChanged);
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
}

impl RasterPool {
    pub fn new(threads: usize, events: Sender<AppEvent>) -> std::io::Result<Self> {
        let shared = Arc::new((Mutex::new(VecDeque::<RenderJob>::new()), Condvar::new()));
        for i in 0..threads.max(1) {
            let shared = shared.clone();
            let events = events.clone();
            std::thread::Builder::new().name(format!("temzdf-raster-{i}")).spawn(move || {
                loop {
                    let job = {
                        let (lock, cv) = &*shared;
                        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if let Some(job) = q.pop_front() {
                                break job;
                            }
                            q = cv.wait(q).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    let ev = match pdf::render(&job.list, job.bounds, job.scale, job.area) {
                        Ok(pixels) => {
                            AppEvent::Rendered(Rendered { key: job.key, width: job.area.w, height: job.area.h, pixels })
                        }
                        Err(e) => AppEvent::RenderFailed(job.key, format!("{e:#}")),
                    };
                    if events.send(ev).is_err() {
                        break;
                    }
                }
            })?;
        }
        Ok(Self { shared })
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
