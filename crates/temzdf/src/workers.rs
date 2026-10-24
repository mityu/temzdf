//! Background threads: the document thread (owns the non-`Send` MuPDF
//! `Document`, loads display lists) and the raster pool (turns display lists into
//! tiles). Results come back to the main loop as [`AppEvent`]s.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};

use mupdf::DisplayList;
use termpix::tty::TermEvent;

use crate::pdf::{self, DocInfo, Link, PageRect};
use crate::viewer::{RenderJob, Rendered, TileKey};

pub enum AppEvent {
    Term(TermEvent),
    Opened(Result<DocInfo, String>),
    PageLoaded(usize, Arc<DisplayList>, Vec<Link>),
    PageFailed(usize, String),
    Rendered(Rendered),
    RenderFailed(TileKey, String),
    SearchHits { generation: u64, page: usize, rects: Vec<PageRect> },
    SearchFailed(String),
}

/// A search request: find `query` on every page, starting at `start_page`.
pub struct SearchCmd {
    pub generation: u64,
    pub query: String,
    pub start_page: usize,
}

/// The search worker opens its own copy of the document so long searches do not
/// delay page loading. A newer command interrupts the current search.
pub fn spawn_search(path: PathBuf, events: Sender<AppEvent>) -> std::io::Result<Sender<SearchCmd>> {
    let (tx, rx) = mpsc::channel::<SearchCmd>();
    std::thread::Builder::new().name("temzdf-search".into()).spawn(move || {
        let doc = match pdf::open(&path).and_then(|d| pdf::page_bounds(&d).map(|b| (d, b))) {
            Ok(v) => v,
            Err(e) => {
                let _ = events.send(AppEvent::SearchFailed(format!("{e:#}")));
                return;
            }
        };
        let (doc, bounds) = doc;
        let mut pending = None;
        loop {
            let cmd = match pending.take() {
                Some(c) => c,
                None => match rx.recv() {
                    Ok(c) => c,
                    Err(_) => return,
                },
            };
            let n = bounds.len();
            for i in 0..n {
                // A newer query replaces this one.
                if let Some(newer) = rx.try_iter().last() {
                    pending = Some(newer);
                    break;
                }
                let page = (cmd.start_page + i) % n;
                let ev = match pdf::search_page(&doc, page, bounds[page], &cmd.query) {
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
}

pub fn spawn_document(path: PathBuf, events: Sender<AppEvent>) -> std::io::Result<Sender<DocCmd>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("temzdf-doc".into()).spawn(move || {
        let (doc, bounds) = match pdf::open(&path).and_then(|d| pdf::doc_info(&d).map(|i| (d, i))) {
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
        for cmd in rx {
            let ev = match cmd {
                DocCmd::LoadPage(p) => match pdf::display_list(&doc, p) {
                    // A page whose links cannot be read still renders.
                    Ok(list) => AppEvent::PageLoaded(p, list, pdf::page_links(&doc, p, bounds[p]).unwrap_or_default()),
                    Err(e) => AppEvent::PageFailed(p, format!("{e:#}")),
                },
            };
            if events.send(ev).is_err() {
                break;
            }
        }
    })?;
    Ok(tx)
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
