//! One open PDF file: its background threads (document, raster pool, search,
//! SyncTeX, file watching), auto-reload scheduling and SyncTeX routing, shared
//! by every pane showing the file. The caller owns the event loop: it feeds
//! [`DocEvent`]s in, collects the panes' work with [`DocumentHost::dispatch`]
//! and performs the returned [`Effect`]s (and hands an inverse search's
//! [`SourceLocation`] to the scripts). See doc/design/reload.md and doc/design/synctex.md.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use temzdf_scene::tiles::{self, Want};
use temzdf_scene::{Effect, Images};

use crate::doc_cache::{DocCache, Need, RenderJob};
use crate::document::SharedDocument;
use crate::pane::{PdfPane, Request};
use crate::pdf::{self, DocInfo};
use crate::workers::{self, DocCmd, DocEvent, RasterPool, SearchCmd, Sink, SynctexCmd};

pub use crate::synctex::SourceLocation;

/// Retry interval while the file is incomplete or fails to open.
const RELOAD_RETRY: Duration = Duration::from_millis(300);
/// Give up retrying after this many attempts (about 30 s); a new change event
/// starts over.
const RELOAD_MAX_ATTEMPTS: u32 = 100;

/// How a host behaves (from the configuration).
#[derive(Clone, Debug, Default)]
pub struct HostOptions {
    /// Wait this long after the file last changed before reloading.
    pub debounce: Duration,
    /// Whether a forward search's file not there is compared to the ones
    /// SyncTeX recorded ignoring case (`o.fileIgnoreCase`).
    pub ignore_case: temzdf_paths::IgnoreCase,
}

/// A forward-search request: show what `line` of `file` produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forward {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
}

/// The threads of a file being opened; [`Opening::finish`] once
/// [`DocEvent::Opened`] arrived.
pub struct Opening {
    path: PathBuf,
    cmd: Sender<DocCmd>,
    pool: RasterPool,
    search: Sender<SearchCmd>,
    synctex: Sender<SynctexCmd>,
    watcher: temzdf_watchfs::Result<temzdf_watchfs::FileWatcher>,
}

impl Opening {
    /// Start the threads for `path`; their events go to `sink`.
    pub fn start(path: &Path, sink: Sink) -> std::io::Result<Self> {
        let snapshot = workers::Snapshot::default();
        let cmd = workers::spawn_document(path.to_owned(), sink.clone(), snapshot.clone())?;
        let pool = workers::raster_pool(tiles::default_threads(), sink.clone())?;
        let search = workers::spawn_search(snapshot, sink.clone())?;
        // Without a watcher the viewer still works, just without auto-reload.
        let watcher = workers::spawn_watcher(path, sink.clone());
        let synctex = workers::spawn_synctex(path.to_owned(), sink)?;
        Ok(Self { path: path.to_owned(), cmd, pool, search, synctex, watcher })
    }

    pub fn finish(self, info: DocInfo, doc: SharedDocument, options: HostOptions) -> DocumentHost {
        let (watcher, watcher_error) = match self.watcher {
            Ok(w) => (Some(w), None),
            Err(e) => (None, Some(format!("auto-reload disabled: {e}"))),
        };
        DocumentHost {
            path: self.path,
            info,
            doc,
            options,
            cmd: self.cmd,
            pool: self.pool,
            search: self.search,
            synctex: self.synctex,
            _watcher: watcher,
            watcher_error,
            reload: ReloadState::default(),
            deferred_forward: None,
            synctex_pending: 0,
            texts_wanted: std::collections::VecDeque::new(),
            text_in_flight: None,
        }
    }
}

pub struct DocumentHost {
    pub path: PathBuf,
    pub info: DocInfo,
    pub doc: SharedDocument,
    options: HostOptions,
    cmd: Sender<DocCmd>,
    pool: RasterPool,
    search: Sender<SearchCmd>,
    synctex: Sender<SynctexCmd>,
    _watcher: Option<temzdf_watchfs::FileWatcher>,
    /// Why auto-reload is off, shown once.
    pub watcher_error: Option<String>,
    reload: ReloadState,
    /// A forward search that arrived while a reload was pending: an editor's
    /// compile callback (vimtex) sends it right after the PDF changed, and the
    /// old document or SyncTeX data would put it in the wrong place.
    deferred_forward: Option<Forward>,
    /// SyncTeX queries sent and not answered yet (each gets one answer).
    synctex_pending: usize,
    /// Pages whose text alone copies wait for ([`Request::ReadText`]), to
    /// ask for one at a time: page loads for the view go in between.
    texts_wanted: std::collections::VecDeque<usize>,
    /// The page whose text was asked for and has not come.
    text_in_flight: Option<usize>,
}

impl DocumentHost {
    /// New reload and inverse-search options (from the configuration).
    pub fn set_options(&mut self, options: HostOptions) {
        self.options = options;
    }

    /// A forward search for this file, shown in `pane`.
    pub fn forward(&mut self, req: Forward, pane: &PdfPane) {
        if self.reload.due.is_some() || self.reload.in_flight {
            self.deferred_forward = Some(req);
        } else {
            self.send_forward(req, pane);
        }
    }

    fn send_forward(&mut self, req: Forward, pane: &PdfPane) {
        let page_hint = pane.viewer.current_page();
        let cmd = SynctexCmd::Forward {
            file: req.file,
            line: req.line,
            column: req.column,
            page_hint,
            ignore_case: self.options.ignore_case,
        };
        if self.synctex.send(cmd).is_ok() {
            self.synctex_pending += 1;
        }
    }

    /// Whether every SyncTeX query sent has its answer (a forward search
    /// waiting for a reload is not sent yet): `--render-frame` waits for
    /// them as for the other workers.
    pub fn synctex_idle(&self) -> bool {
        self.synctex_pending == 0
    }

    /// Handle an event of this file's threads. `panes` are the panes showing it;
    /// SyncTeX results go to the first one (the caller puts the pane that asked
    /// first). Returns whether they need redrawing, and where an inverse
    /// search found the source (for the caller to hand on: the scripts'
    /// `SynctexInverse`).
    pub fn handle(
        &mut self,
        ev: DocEvent,
        panes: &mut [&mut PdfPane],
        images: &mut Images,
        now: Instant,
    ) -> (bool, Option<SourceLocation>) {
        let message = |panes: &mut [&mut PdfPane], m: String| {
            for p in panes.iter_mut() {
                p.message = Some(m.clone());
            }
        };
        let mut found = None;
        match ev {
            DocEvent::Opened(_) => return (false, None),
            DocEvent::PageText(generation, p, text) => {
                if self.text_in_flight == Some(p) {
                    self.text_in_flight = None;
                }
                let ok = text.is_ok();
                {
                    let mut d = self.doc.borrow_mut();
                    // Of an older version: its copies were dropped.
                    if generation == d.cache.generation() {
                        if let Ok(text) = text {
                            d.set_text(p, text);
                        }
                    } else {
                        drop(d);
                        self.read_next_text();
                        return (false, None);
                    }
                }
                for pane in panes.iter_mut() {
                    pane.text_arrived(p, ok);
                }
                self.read_next_text();
                return (false, None);
            }
            DocEvent::PageLoaded(generation, p, list, links) => {
                let mut d = self.doc.borrow_mut();
                if generation != d.cache.generation() {
                    return (false, None);
                }
                let (current, in_use): (Vec<_>, Vec<_>) = panes.iter().map(|p| p.viewer.retention()).unzip();
                d.cache.page_loaded(generation, p, list, &current, &in_use);
                d.set_links(p, links);
                drop(d);
                for pane in panes.iter_mut() {
                    pane.page_ready();
                }
            }
            DocEvent::PageFailed(generation, p, e) => {
                self.doc.borrow_mut().cache.page_failed(generation, p);
                message(panes, format!("page {}: {e}", p + 1));
            }
            DocEvent::Rendered(r) => self.doc.borrow_mut().cache.rendered(images, r),
            DocEvent::RenderFailed(key, e) => {
                self.doc.borrow_mut().cache.render_failed(key);
                message(panes, format!("render: {e}"));
            }
            DocEvent::SearchHits { generation, page, hits } => {
                // Search generations are unique per pane; the others ignore it.
                let mut changed = false;
                for pane in panes.iter_mut() {
                    changed |= pane.search_hits(generation, page, hits.clone(), now);
                }
                return (changed, None);
            }
            DocEvent::SearchFailed(e) => message(panes, format!("search: {e}")),
            DocEvent::FileChanged => {
                self.reload.changed(now, self.options.debounce);
                for pane in panes.iter_mut() {
                    pane.reloading = true;
                }
            }
            DocEvent::ForwardResult(boxes) => {
                self.synctex_pending = self.synctex_pending.saturating_sub(1);
                if let Some(pane) = panes.first_mut() {
                    pane.forward_result(boxes, now);
                }
            }
            DocEvent::InverseResult(loc) => {
                self.synctex_pending = self.synctex_pending.saturating_sub(1);
                found = Some(loc);
            }
            DocEvent::SynctexFailed(e) => {
                self.synctex_pending = self.synctex_pending.saturating_sub(1);
                if let Some(pane) = panes.first_mut() {
                    pane.message = Some(format!("SyncTeX: {e}"));
                }
            }
            DocEvent::Reloaded(info) => {
                let _ = self.synctex.send(SynctexCmd::Reload);
                // The copies waiting for them are dropped (before_reload).
                self.texts_wanted.clear();
                self.reload.in_flight = false;
                self.reload.attempts = 0;
                for pane in panes.iter_mut() {
                    pane.before_reload();
                }
                self.doc.borrow_mut().replace(images, info.bounds.clone());
                for pane in panes.iter_mut() {
                    pane.after_reload(images, info.clone());
                    pane.reloading = self.reload.due.is_some();
                }
                self.info = info;
            }
            DocEvent::ReloadFailed(e) => {
                self.reload.in_flight = false;
                if !self.reload.retry(now) {
                    for pane in panes.iter_mut() {
                        pane.reloading = false;
                    }
                    message(panes, format!("reload failed: {e}"));
                }
            }
        }
        (true, found)
    }

    /// Start a due reload (only once the file looks completely written) and
    /// send a deferred forward search once no reload is pending. Returns
    /// whether the panes need redrawing.
    pub fn poll(&mut self, panes: &mut [&mut PdfPane], now: Instant) -> bool {
        let mut changed = false;
        if let Some(due) = self.reload.due
            && now >= due
            && !self.reload.in_flight
        {
            self.reload.due = None;
            if pdf::looks_complete(&self.path).unwrap_or(false) {
                self.reload.in_flight = true;
                let _ = self.cmd.send(DocCmd::Reload);
            } else if !self.reload.retry(now) {
                for pane in panes.iter_mut() {
                    pane.reloading = false;
                    pane.message = Some("reload: the file stays incomplete or missing".into());
                }
                changed = true;
            }
        }
        if let Some(pane) = panes.first()
            && let Some(req) = self.deferred_forward.take_if(|_| self.reload.due.is_none() && !self.reload.in_flight)
        {
            self.send_forward(req, pane);
        }
        changed
    }

    /// When [`DocumentHost::poll`] has something to do next.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.reload.due
    }

    /// Hand this frame's work (from every pane showing the file, most urgent
    /// first) to the threads. Work asked for twice is done once; queued work
    /// this frame still wants stays queued, in its new place.
    pub fn dispatch(&mut self, needs: Vec<Need>) {
        let mut d = self.doc.borrow_mut();
        let (loads, wants) = claim(&mut d.cache, needs);
        for p in loads {
            let _ = self.cmd.send(DocCmd::LoadPage(p));
        }
        for key in self.pool.replace_wanted(wants) {
            d.cache.tiles.cancelled(key);
        }
    }

    /// No pane shows the document (it stays open, hidden): its tiles and
    /// lists go ([`Document::forget`](crate::document::Document::forget)),
    /// and so does the work queued for them; what is being rendered still
    /// arrives, and is dropped then.
    pub fn forget(&mut self, images: &mut Images) {
        self.doc.borrow_mut().forget(images);
        self.dispatch(Vec::new());
    }

    /// Carry out a pane's request; what only the frontend can do is returned.
    pub fn request(&mut self, req: Request) -> Option<Effect> {
        match req {
            Request::Search { generation, query, start_page, doc_generation } => {
                let _ = self.search.send(SearchCmd { generation, query, start_page, doc_generation });
                None
            }
            Request::Inverse { page, x, y } => {
                if self.synctex.send(SynctexCmd::Inverse { page, x, y }).is_ok() {
                    self.synctex_pending += 1;
                }
                None
            }
            Request::OpenUri(uri) => Some(Effect::OpenUri { uri, command: None }),
            Request::ReadText(pages) => {
                for p in pages {
                    if !self.texts_wanted.contains(&p) && self.text_in_flight != Some(p) {
                        self.texts_wanted.push_back(p);
                    }
                }
                self.read_next_text();
                None
            }
            // The session's (it knows who waits for which copy).
            Request::Copy { .. } | Request::Copying { .. } | Request::CopyDropped { .. } => None,
            // The session's to say, before it hands requests here.
            Request::NotFound(_) => None,
        }
    }
}

impl DocumentHost {
    /// Ask for the next page's text copies wait for, unless one is on its
    /// way: one at a time, so that pages the view loads are not held up.
    fn read_next_text(&mut self) {
        if self.text_in_flight.is_some() {
            return;
        }
        if let Some(p) = self.texts_wanted.pop_front()
            && self.cmd.send(DocCmd::LoadText(p)).is_ok()
        {
            self.text_in_flight = Some(p);
        }
    }

    /// Whether pages' text is asked for or on its way (for tests).
    pub fn reading_texts(&self) -> bool {
        self.text_in_flight.is_some() || !self.texts_wanted.is_empty()
    }
}

/// Mark the work as requested in the cache and split it into pages to load
/// and the tiles wanted, in order: new renders, and renders already in flight
/// (kept queued if they have not started). Pages already requested are
/// dropped, and a tile asked for twice (several panes on one document ask for
/// the same tiles) counts once.
fn claim(cache: &mut DocCache, needs: Vec<Need>) -> (Vec<usize>, Vec<Want<RenderJob>>) {
    let (mut loads, mut wants) = (Vec::new(), Vec::new());
    for need in needs {
        match need {
            Need::LoadPage(p) if cache.is_requested(p) => {}
            Need::LoadPage(p) => {
                cache.mark_requested(&need);
                loads.push(p);
            }
            Need::Render(job) => wants.push(Want::New(job)),
            Need::Queued(key) => wants.push(Want::Queued(key)),
        }
    }
    (loads, tiles::claim(wants, |job| cache.tiles.mark_in_flight(job.key)))
}

/// Auto-reload scheduling: a trailing debounce after change events, then
/// retries while the file is incomplete or fails to open (as Okular does).
#[derive(Default)]
struct ReloadState {
    /// When to try next.
    due: Option<Instant>,
    /// A reload request is with the document thread.
    in_flight: bool,
    attempts: u32,
}

impl ReloadState {
    fn changed(&mut self, now: Instant, debounce: Duration) {
        self.due = Some(now + debounce);
        self.attempts = 0;
    }

    /// Schedule another attempt; false once we give up.
    fn retry(&mut self, now: Instant) -> bool {
        self.attempts += 1;
        if self.attempts >= RELOAD_MAX_ATTEMPTS {
            self.due = None;
            return false;
        }
        self.due = Some(now + RELOAD_RETRY);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    #[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
    fn work_asked_for_twice_is_claimed_once() {
        use crate::doc_cache::{TileKey, TileKind};
        use crate::pdf::PixelRect;
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        let bounds = pdf::page_bounds(&doc).unwrap();
        let list = pdf::display_list(&doc, 0).unwrap();
        let mut cache = DocCache::new(bounds.clone());
        let job = |tx| {
            let key = TileKey { generation: 0, page: 0, kind: TileKind::Tile { scale_q: 1024, tx, ty: 0 } };
            let area = PixelRect { x: 0, y: 0, w: 8, h: 8 };
            Need::Render(RenderJob { key, list: list.clone(), bounds: bounds[0], scale: 1.0, area })
        };
        let key = |w: &Want<RenderJob>| match w {
            Want::New(j) => (true, j.key),
            Want::Queued(k) => (false, *k),
        };
        let needs = vec![Need::LoadPage(1), job(0), job(1), Need::LoadPage(1), job(0)];
        let (loads, wants) = claim(&mut cache, needs);
        assert_eq!(loads, vec![1]);
        let tile = |tx| TileKey { generation: 0, page: 0, kind: TileKind::Tile { scale_q: 1024, tx, ty: 0 } };
        assert_eq!(wants.iter().map(key).collect::<Vec<_>>(), [(true, tile(0)), (true, tile(1))]);
        // The next frame asks again while they are in flight, in another
        // order: nothing new, both kept in that order.
        let needs = vec![Need::Queued(tile(1)), job(0), Need::LoadPage(1), Need::Queued(tile(1))];
        let (loads, wants) = claim(&mut cache, needs);
        assert!(loads.is_empty());
        assert_eq!(wants.iter().map(key).collect::<Vec<_>>(), [(false, tile(1)), (false, tile(0))]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    #[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
    fn work_still_queued_survives_the_next_frame() {
        use std::sync::mpsc;
        use termpix_core::geom::{CellSize, Geometry};
        let path = pdf::fixture("sample.pdf");
        let doc = pdf::open(&path).unwrap();
        let info = pdf::doc_info(&doc).unwrap();
        let geometry = Geometry { cols: 160, rows: 50, cell: CellSize { width: 10, height: 20 } };
        let mut pane = PdfPane::new("sample.pdf".into(), info.clone(), geometry);
        for p in 0..pane.viewer.page_count() {
            pane.viewer.page_loaded(0, p, pdf::display_list(&doc, p).unwrap());
        }
        let (tx, rx) = mpsc::channel();
        let sink = temzdf_scene::sink::channel_sink(tx, |e| e);
        let mut host =
            Opening::start(&path, sink).unwrap().finish(info, pane.viewer.doc.clone(), HostOptions::default());
        let mut images = Images::new();
        // Two frames before any tile arrives, as when the threads have just
        // started: the second must keep the work the first queued.
        for _ in 0..2 {
            let (_, needs) = pane.frame(&mut images);
            host.dispatch(needs);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while !host.doc.borrow().cache.is_idle() {
            let wait = deadline.saturating_duration_since(Instant::now());
            let ev = rx.recv_timeout(wait).expect("tiles stopped arriving");
            if let ev @ (DocEvent::Rendered(_) | DocEvent::RenderFailed(..)) = ev {
                host.handle(ev, &mut [&mut pane], &mut images, Instant::now());
            }
        }
        // Nothing was dropped on the way: the next frame has every tile.
        let (_, needs) = pane.frame(&mut images);
        let renders = needs.iter().filter(|n| !matches!(n, Need::LoadPage(_))).count();
        assert_eq!(renders, 0, "tiles queued by the first frame were dropped");
        assert!(pane.viewer.is_complete());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    #[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
    fn a_copy_reads_pages_never_shown_one_at_a_time() {
        use std::sync::mpsc;
        use termpix_core::geom::{CellSize, Geometry};
        let path = pdf::fixture("sample.pdf");
        let doc = pdf::open(&path).unwrap();
        let info = pdf::doc_info(&doc).unwrap();
        let geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };
        let selection = crate::selection::Selection { anchor: (0, 0), head: (2, 5) };
        // What the copy is with every page shown.
        let mut all = PdfPane::new("sample.pdf".into(), info.clone(), geometry);
        for p in 0..3 {
            all.viewer.page_loaded(0, p, pdf::display_list(&doc, p).unwrap());
        }
        all.set_selection(selection);
        all.copy();
        let [Request::Copy { text: expected, .. }] = &all.take_requests()[..] else { panic!("a copy") };

        // Only page 0 shown.
        let mut pane = PdfPane::new("sample.pdf".into(), info.clone(), geometry);
        pane.viewer.page_loaded(0, 0, pdf::display_list(&doc, 0).unwrap());
        let (tx, rx) = mpsc::channel();
        let sink = temzdf_scene::sink::channel_sink(tx, |e| e);
        let mut host =
            Opening::start(&path, sink).unwrap().finish(info, pane.viewer.doc.clone(), HostOptions::default());
        let mut images = Images::new();
        pane.set_selection(selection);
        let id = pane.copy().unwrap();
        for req in pane.take_requests() {
            host.request(req);
        }
        // One page asked for, the other waiting its turn.
        assert_eq!((host.text_in_flight, Vec::from(host.texts_wanted.clone())), (Some(1), vec![2]));
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut copied = None;
        while copied.is_none() {
            let wait = deadline.saturating_duration_since(Instant::now());
            let ev = rx.recv_timeout(wait).expect("the text stopped arriving");
            // Each answers the one page asked for; the next is asked for then.
            if let DocEvent::PageText(_, p, _) = &ev {
                assert_eq!(Some(*p), host.text_in_flight, "the page asked for");
            }
            host.handle(ev, &mut [&mut pane], &mut images, Instant::now());
            for req in pane.take_requests() {
                match req {
                    Request::Copy { id: got, text, note } => copied = Some((got, text, note)),
                    req => panic!("{req:?}"),
                }
            }
        }
        assert_eq!(copied, Some((id, expected.clone(), None)));
        assert!(!host.reading_texts());
        // The text read alone stays for the view (no extraction again).
        assert!(host.doc.borrow().texts[1].is_some());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    #[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
    fn a_forgotten_document_drops_its_queued_work() {
        use std::sync::mpsc;
        use termpix_core::geom::{CellSize, Geometry};
        let path = pdf::fixture("sample.pdf");
        let doc = pdf::open(&path).unwrap();
        let info = pdf::doc_info(&doc).unwrap();
        let geometry = Geometry { cols: 160, rows: 50, cell: CellSize { width: 10, height: 20 } };
        let mut pane = PdfPane::new("sample.pdf".into(), info.clone(), geometry);
        for p in 0..pane.viewer.page_count() {
            pane.viewer.page_loaded(0, p, pdf::display_list(&doc, p).unwrap());
        }
        let (tx, rx) = mpsc::channel();
        let sink = temzdf_scene::sink::channel_sink(tx, |e| e);
        let mut host =
            Opening::start(&path, sink).unwrap().finish(info, pane.viewer.doc.clone(), HostOptions::default());
        let mut images = Images::new();
        let (_, needs) = pane.frame(&mut images);
        let keys: Vec<_> = needs
            .iter()
            .filter_map(|n| match n {
                Need::Render(j) => Some(j.key),
                _ => None,
            })
            .collect();
        assert!(keys.len() > 8, "too few tiles to tell: {}", keys.len());
        host.dispatch(needs);
        host.forget(&mut images);
        // Only what the (at most four) threads had started is still coming.
        let in_flight =
            |host: &DocumentHost| keys.iter().filter(|k| host.doc.borrow().cache.tiles.is_in_flight(**k)).count();
        assert!(in_flight(&host) <= 4, "{} of {} tiles still to be rendered", in_flight(&host), keys.len());
        let deadline = Instant::now() + Duration::from_secs(30);
        while !host.doc.borrow().cache.is_idle() {
            let wait = deadline.saturating_duration_since(Instant::now());
            let ev = rx.recv_timeout(wait).expect("tiles stopped arriving");
            if let ev @ (DocEvent::Rendered(_) | DocEvent::RenderFailed(..)) = ev {
                host.handle(ev, &mut [&mut pane], &mut images, Instant::now());
            }
        }
        assert!(host.doc.borrow().cache.tiles.is_empty(), "a tile of the forgotten document was kept");
    }

    #[test]
    fn reload_retries_then_gives_up() {
        let now = Instant::now();
        let mut r = ReloadState::default();
        r.changed(now, Duration::from_millis(200));
        assert_eq!(r.due, Some(now + Duration::from_millis(200)));
        for _ in 1..RELOAD_MAX_ATTEMPTS {
            assert!(r.retry(now));
        }
        assert!(!r.retry(now));
        assert!(r.due.is_none());
    }
}
