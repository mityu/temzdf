//! One open PDF file: its background threads (document, raster pool, search,
//! SyncTeX, file watching), auto-reload scheduling and SyncTeX routing, shared
//! by every pane showing the file. The caller owns the event loop: it feeds
//! [`DocEvent`]s in, collects the panes' work with [`DocumentHost::dispatch`]
//! and performs the returned [`Effect`]s. See doc/reload.md and doc/synctex.md.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use temzdf_scene::{Effect, Images};

use crate::doc_cache::{DocCache, Need, RenderJob};
use crate::document::SharedDocument;
use crate::pane::{PdfPane, Request};
use crate::pdf::{self, DocInfo};
use crate::workers::{self, DocCmd, DocEvent, RasterPool, SearchCmd, Sink, SynctexCmd};

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
    /// Inverse search command template (`%{input}`, `%{line}`, `%{column}`).
    pub editor: Option<String>,
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
    watcher: notify::Result<notify::RecommendedWatcher>,
}

impl Opening {
    /// Start the threads for `path`; their events go to `sink`.
    pub fn start(path: &Path, sink: Sink) -> std::io::Result<Self> {
        let cmd = workers::spawn_document(path.to_owned(), sink.clone())?;
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4));
        let pool = RasterPool::new(threads, sink.clone())?;
        let search = workers::spawn_search(path.to_owned(), sink.clone())?;
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
    _watcher: Option<notify::RecommendedWatcher>,
    /// Why auto-reload is off, shown once.
    pub watcher_error: Option<String>,
    reload: ReloadState,
    /// A forward search that arrived while a reload was pending: an editor's
    /// compile callback (vimtex) sends it right after the PDF changed, and the
    /// old document or SyncTeX data would put it in the wrong place.
    deferred_forward: Option<Forward>,
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

    fn send_forward(&self, req: Forward, pane: &PdfPane) {
        let page_hint = pane.viewer.current_page();
        let _ =
            self.synctex.send(SynctexCmd::Forward { file: req.file, line: req.line, column: req.column, page_hint });
    }

    /// Handle an event of this file's threads. `panes` are the panes showing it;
    /// SyncTeX results go to the first one (the caller puts the pane that asked
    /// first). Returns whether they need redrawing, and what the frontend should do.
    pub fn handle(
        &mut self,
        ev: DocEvent,
        panes: &mut [&mut PdfPane],
        images: &mut Images,
        now: Instant,
    ) -> (bool, Option<Effect>) {
        let message = |panes: &mut [&mut PdfPane], m: String| {
            for p in panes.iter_mut() {
                p.message = Some(m.clone());
            }
        };
        let mut effect = None;
        match ev {
            DocEvent::Opened(_) => return (false, None),
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
                self.doc.borrow_mut().cache.render_cancelled(key);
                message(panes, format!("render: {e}"));
            }
            DocEvent::SearchHits { generation, page, rects } => {
                // Search generations are unique per pane; the others ignore it.
                let mut changed = false;
                for pane in panes.iter_mut() {
                    changed |= pane.search_hits(generation, page, rects.clone(), now);
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
                if let Some(pane) = panes.first_mut() {
                    pane.forward_result(boxes, now);
                }
            }
            DocEvent::InverseResult(loc) => {
                match self.options.editor.as_deref().and_then(|t| editor_command(t, &loc.file, loc.line, loc.column)) {
                    Some(cmd) => effect = Some(Effect::RunShell(cmd)),
                    None => {
                        let m = format!(
                            "{}:{} (set [synctex] editor in the config to open it)",
                            loc.file.display(),
                            loc.line
                        );
                        if let Some(pane) = panes.first_mut() {
                            pane.message = Some(m);
                        }
                    }
                }
            }
            DocEvent::SynctexFailed(e) => {
                if let Some(pane) = panes.first_mut() {
                    pane.message = Some(format!("SyncTeX: {e}"));
                }
            }
            DocEvent::Reloaded(info) => {
                let _ = self.synctex.send(SynctexCmd::Reload);
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
        (true, effect)
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
    /// first) to the threads. Work asked for twice is done once.
    pub fn dispatch(&mut self, needs: Vec<Need>) {
        let mut d = self.doc.borrow_mut();
        let (loads, jobs) = claim(&mut d.cache, needs);
        for p in loads {
            let _ = self.cmd.send(DocCmd::LoadPage(p));
        }
        for key in self.pool.replace(jobs) {
            d.cache.render_cancelled(key);
        }
    }

    /// Carry out a pane's request; what only the frontend can do is returned.
    pub fn request(&mut self, req: Request) -> Option<Effect> {
        match req {
            Request::Search { generation, query, start_page, doc_generation } => {
                let _ = self.search.send(SearchCmd { generation, query, start_page, doc_generation });
                None
            }
            Request::Inverse { page, x, y } => {
                let _ = self.synctex.send(SynctexCmd::Inverse { page, x, y });
                None
            }
            Request::OpenUri(uri) => Some(Effect::OpenUri { uri, command: None }),
            Request::Copy(text) => Some(Effect::Copy(text)),
        }
    }
}

/// Mark the work as requested in the cache and split it into pages to load
/// and tiles to render, dropping what is already requested (several panes on
/// one document ask for the same tiles).
fn claim(cache: &mut DocCache, needs: Vec<Need>) -> (Vec<usize>, Vec<RenderJob>) {
    let (mut loads, mut jobs) = (Vec::new(), Vec::new());
    for need in needs {
        match &need {
            Need::LoadPage(p) if cache.is_requested(*p) => continue,
            Need::Render(job) if cache.is_in_flight(job.key) => continue,
            _ => {}
        }
        cache.mark_requested(&need);
        match need {
            Need::LoadPage(p) => loads.push(p),
            Need::Render(job) => jobs.push(job),
        }
    }
    (loads, jobs)
}

/// The inverse-search shell command for a source location: `template` with
/// `%{input}`, `%{line}` and `%{column}` replaced (the file shell-quoted).
/// `None` for an empty template.
pub fn editor_command(template: &str, file: &Path, line: u32, column: u32) -> Option<String> {
    if template.trim().is_empty() {
        return None;
    }
    Some(
        template
            .replace("%{input}", &shell_quote(&file.to_string_lossy()))
            .replace("%{line}", &line.to_string())
            .replace("%{column}", &column.to_string()),
    )
}

/// Quote for POSIX sh: wrap in single quotes, escaping embedded ones.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
    fn editor_command_substitutes_and_quotes() {
        let t = "nvim --server /tmp/nv --remote-send ':e %{input}<CR>:%{line}<CR>'";
        let cmd = editor_command(t, Path::new("/doc/it's.tex"), 12, 0).unwrap();
        assert_eq!(cmd, "nvim --server /tmp/nv --remote-send ':e '/doc/it'\\''s.tex'<CR>:12<CR>'");
        assert!(editor_command(" ", Path::new("a.tex"), 1, 0).is_none());
    }

    #[test]
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
        let needs = vec![Need::LoadPage(1), job(0), job(1), Need::LoadPage(1), job(0)];
        let (loads, jobs) = claim(&mut cache, needs);
        assert_eq!(loads, vec![1]);
        assert_eq!(jobs.len(), 2);
        // The next frame asks again while they are in flight: nothing new.
        let (loads, jobs) = claim(&mut cache, vec![job(0), Need::LoadPage(1)]);
        assert!(loads.is_empty() && jobs.is_empty());
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
