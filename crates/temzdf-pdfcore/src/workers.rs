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
/// start-up, a reload, or a change of the file itself (TeX writes it after the
/// PDF: a query in between read the old one).
pub fn spawn_synctex(pdf: PathBuf, events: Sink) -> std::io::Result<Sender<SynctexCmd>> {
    let (tx, rx) = mpsc::channel::<SynctexCmd>();
    std::thread::Builder::new().name("temzdf-synctex".into()).spawn(move || {
        let mut scanner: Option<Scanner> = None;
        let mut seen = None;
        for cmd in rx {
            if matches!(cmd, SynctexCmd::Reload) {
                scanner = None;
                continue;
            }
            let stamp = synctex_stamp(&pdf);
            if stamp != seen {
                scanner = None;
            }
            if scanner.is_none() {
                scanner = Scanner::open(&pdf);
                seen = stamp;
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

/// The SyncTeX file next to `pdf` as it is now (modification time and size),
/// to notice it changed.
fn synctex_stamp(pdf: &Path) -> Option<(std::time::SystemTime, u64)> {
    ["synctex.gz", "synctex"].iter().find_map(|ext| {
        let m = std::fs::metadata(pdf.with_extension(ext)).ok()?;
        Some((m.modified().ok()?, m.len()))
    })
}

/// The document's bytes as the document thread last opened them, with that
/// version's generation: the search thread opens the very same version
/// (reading the file itself, it could get a newer one, whose page count the
/// viewer does not know yet).
#[derive(Clone, Default)]
pub struct Snapshot(Arc<std::sync::Mutex<Option<Version>>>);

/// A generation and its bytes.
type Version = (u32, Arc<[u8]>);

impl Snapshot {
    fn set(&self, generation: u32, bytes: Arc<[u8]>) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some((generation, bytes));
    }

    fn get(&self) -> Option<Version> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// A search request: find `query` on every page, starting at `start_page`.
pub struct SearchCmd {
    pub generation: u64,
    pub query: String,
    pub start_page: usize,
    /// Document generation; the worker reopens the file when it changes.
    pub doc_generation: u32,
}

/// The search worker opens its own copy of the document (the version in
/// `snapshot`) so long searches do not delay page loading. Each pane (the
/// generation's upper 32 bits are its search base) has one search: a newer
/// command of the pane replaces it, and the panes' searches take turns a
/// page at a time (after a reload every pane searches again).
pub fn spawn_search(snapshot: Snapshot, events: Sink) -> std::io::Result<Sender<SearchCmd>> {
    let (tx, rx) = mpsc::channel::<SearchCmd>();
    std::thread::Builder::new().name("temzdf-search".into()).spawn(move || {
        let mut doc: Option<(u32, pdf::Doc, Vec<pdf::PageBounds>)> = None;
        // Each pane's search and how many of its pages are done.
        let mut jobs: Vec<(SearchCmd, usize)> = Vec::new();
        let pane = |c: &SearchCmd| c.generation >> 32;
        let mut turn = 0;
        loop {
            let take = |jobs: &mut Vec<(SearchCmd, usize)>, cmd: SearchCmd| {
                jobs.retain(|(j, _)| pane(j) != pane(&cmd));
                jobs.push((cmd, 0));
            };
            if jobs.is_empty() {
                match rx.recv() {
                    Ok(c) => take(&mut jobs, c),
                    Err(_) => return,
                }
            }
            for c in rx.try_iter() {
                take(&mut jobs, c);
            }
            turn %= jobs.len();
            let generation = jobs[turn].0.doc_generation;
            if doc.as_ref().is_none_or(|d| d.0 != generation) {
                // Only the version the request was made on: another one means
                // the request is stale (a reload follows with a new one).
                let opened = snapshot
                    .get()
                    .filter(|s| s.0 == generation)
                    .map(|(_, bytes)| pdf::open_bytes(&bytes).and_then(|d| pdf::page_bounds(&d).map(|b| (d, b))));
                match opened {
                    Some(Ok((d, b))) => doc = Some((generation, d, b)),
                    Some(Err(e)) => {
                        events(DocEvent::SearchFailed(format!("{e:#}")));
                        jobs.remove(turn);
                        continue;
                    }
                    None => {
                        jobs.remove(turn);
                        continue;
                    }
                }
            }
            let Some((_, doc, bounds)) = doc.as_ref() else { continue };
            let n = bounds.len();
            let (cmd, done) = &mut jobs[turn];
            if *done >= n {
                jobs.remove(turn);
                continue;
            }
            let page = (cmd.start_page + *done) % n;
            *done += 1;
            let ev = match pdf::search_page(doc, page, bounds[page], &cmd.query) {
                Ok(rects) => DocEvent::SearchHits { generation: cmd.generation, page, rects },
                Err(e) => DocEvent::SearchFailed(format!("{e:#}")),
            };
            if !events(ev) {
                return;
            }
            turn += 1;
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

/// How long opening a document keeps trying (TeX may be writing it).
const FIRST_OPEN_FOR: std::time::Duration = std::time::Duration::from_secs(5);
const FIRST_OPEN_RETRY: std::time::Duration = std::time::Duration::from_millis(300);

/// The document thread: opens `path` (a snapshot of it, see [`pdf::open`],
/// shared with the search thread through `snapshot`) and loads pages.
pub fn spawn_document(path: PathBuf, events: Sink, snapshot: Snapshot) -> std::io::Result<Sender<DocCmd>> {
    let open = move |path: &Path| -> anyhow::Result<(Arc<[u8]>, pdf::Doc, DocInfo)> {
        let bytes = pdf::read(path)?;
        let doc = anyhow::Context::with_context(pdf::open_bytes(&bytes), || format!("cannot open {}", path.display()))?;
        let info = pdf::doc_info(&doc)?;
        Ok((bytes, doc, info))
    };
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("temzdf-doc".into()).spawn(move || {
        // Started while TeX writes the file: wait for it, as reloads do.
        let first = || {
            let deadline = std::time::Instant::now() + FIRST_OPEN_FOR;
            loop {
                let complete = pdf::looks_complete(&path);
                if let Err(e) = &complete
                    && e.kind() == std::io::ErrorKind::NotFound
                {
                    return Err(anyhow::anyhow!("cannot open {}: no such file", path.display()));
                }
                // Waiting is for files TeX is writing; at the deadline, or for
                // a file that is no PDF at all, MuPDF has the last word.
                let last = std::time::Instant::now() >= deadline || pdf::not_a_pdf(&path).unwrap_or(false);
                let result = match complete {
                    Ok(complete) if complete || last => open(&path).and_then(|(bytes, doc, info)| {
                        if info.bounds.is_empty() {
                            Err(anyhow::anyhow!("the document has no pages"))
                        } else {
                            Ok((bytes, doc, info))
                        }
                    }),
                    Ok(_) => Err(anyhow::anyhow!("{} looks incomplete (no %%EOF at its end)", path.display())),
                    Err(e) => Err(e.into()),
                };
                if result.is_ok() || last {
                    return result;
                }
                std::thread::sleep(FIRST_OPEN_RETRY);
            }
        };
        let (mut doc, mut bounds) = match first() {
            Ok((bytes, doc, info)) => {
                snapshot.set(0, bytes);
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
                DocCmd::Reload => match open(&path) {
                    Ok((bytes, d, info)) if !info.bounds.is_empty() => {
                        doc = d;
                        bounds = info.bounds.clone();
                        generation = generation.wrapping_add(1);
                        snapshot.set(generation, bytes);
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
        // Under the lock: a worker between checking `stop` and waiting would
        // otherwise miss the notification and wait forever.
        let _queue = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
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

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn opening_waits_for_a_file_being_written() {
        let path = std::env::temp_dir().join(format!("temzdf-writing-{}.pdf", std::process::id()));
        let full = std::fs::read(pdf::fixture("sample.pdf")).unwrap();
        std::fs::write(&path, &full[..full.len() / 2]).unwrap();
        let (tx, rx) = mpsc::channel::<DocEvent>();
        let _cmds = spawn_document(path.clone(), channel_sink(tx, |e| e), Snapshot::default()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        std::fs::write(&path, &full).unwrap();
        let opened = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert!(matches!(opened, DocEvent::Opened(Ok(_))), "the half-written file was taken");
        std::fs::remove_file(&path).unwrap();
        // A missing file fails at once.
        let (tx, rx) = mpsc::channel::<DocEvent>();
        let start = std::time::Instant::now();
        let _cmds = spawn_document(path.clone(), channel_sink(tx, |e| e), Snapshot::default()).unwrap();
        let failed = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert!(matches!(failed, DocEvent::Opened(Err(e)) if e.contains("no such file")));
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn searches_of_two_panes_both_complete() {
        use std::time::Duration;
        let (tx, rx) = mpsc::channel::<DocEvent>();
        let sink = channel_sink(tx, |e| e);
        let snapshot = Snapshot::default();
        let _doc = spawn_document(pdf::fixture("sample.pdf"), sink.clone(), snapshot.clone()).unwrap();
        let pages = match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            DocEvent::Opened(Ok(info)) => info.bounds.len(),
            _ => panic!("not opened"),
        };
        let search = spawn_search(snapshot, sink).unwrap();
        // Two panes (search bases 1 << 32 and 2 << 32) re-running their
        // queries one right after the other, as after a reload; then the
        // first pane's newer query replaces its own only.
        let gens = [(1u64 << 32) + 1, (2u64 << 32) + 1, (1u64 << 32) + 2];
        for generation in gens {
            search.send(SearchCmd { generation, query: "the".into(), start_page: 0, doc_generation: 0 }).unwrap();
        }
        let mut done = std::collections::HashMap::<u64, usize>::new();
        while gens[1..].iter().any(|g| done.get(g).copied().unwrap_or(0) < pages) {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(DocEvent::SearchHits { generation, .. }) => *done.entry(generation).or_default() += 1,
                Ok(_) => {}
                Err(_) => panic!("pages searched per pane: {done:?} of {pages}"),
            }
        }
        assert!(done.get(&gens[0]).copied().unwrap_or(0) < pages, "replaced by its newer query");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn a_pdf_with_bytes_after_its_eof_still_opens() {
        // Valid (the spec allows %%EOF anywhere near the end): only a hint
        // that TeX is done, never a reason to refuse the file.
        let dir = std::env::temp_dir().join(format!("temzdf-padded-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let open = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            let (tx, rx) = mpsc::channel::<DocEvent>();
            let start = std::time::Instant::now();
            let _cmds = spawn_document(path, channel_sink(tx, |e| e), Snapshot::default()).unwrap();
            let opened = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            (opened, start.elapsed())
        };
        let mut padded = std::fs::read(pdf::fixture("sample.pdf")).unwrap();
        padded.extend(std::iter::repeat_n(b' ', 2048));
        let (opened, took) = open("padded.pdf", &padded);
        let error = |e: &DocEvent| match e {
            DocEvent::Opened(Err(e)) => e.clone(),
            _ => String::new(),
        };
        assert!(matches!(opened, DocEvent::Opened(Ok(_))), "refused a complete PDF: {}", error(&opened));
        assert!(took < std::time::Duration::from_secs(1), "waited {took:?}");
        // Not a PDF: MuPDF's own error, at once.
        let (failed, took) = open("notes.txt", b"just some notes, no PDF here\n");
        assert!(matches!(&failed, DocEvent::Opened(Err(e)) if !e.contains("looks incomplete")), "{}", error(&failed));
        assert!(took < std::time::Duration::from_secs(1), "waited {took:?}");
        // Other bytes after %%EOF: waited for, then opened all the same.
        let mut junk = std::fs::read(pdf::fixture("sample.pdf")).unwrap();
        junk.extend(std::iter::repeat_n(b'x', 2048));
        let (opened, took) = open("junk.pdf", &junk);
        assert!(matches!(opened, DocEvent::Opened(Ok(_))), "{}", error(&opened));
        assert!(took >= FIRST_OPEN_FOR, "{took:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn synctex_data_follows_its_file() {
        let dir = std::env::temp_dir().join(format!("temzdf-synctex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("sample.pdf");
        std::fs::copy(pdf::fixture("sample.pdf"), &pdf).unwrap();
        std::fs::copy(pdf::fixture("sample.pdf").with_extension("synctex.gz"), pdf.with_extension("synctex.gz"))
            .unwrap();
        let (tx, rx) = mpsc::channel::<DocEvent>();
        let cmds = spawn_synctex(pdf.clone(), channel_sink(tx, |e| e)).unwrap();
        let forward = || SynctexCmd::Forward { file: "sample.tex".into(), line: 20, column: 0, page_hint: 0 };
        let answer = || rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
        cmds.send(forward()).unwrap();
        assert!(matches!(answer(), DocEvent::ForwardResult(_)));
        // The file goes (a build without SyncTeX, or TeX about to write a new
        // one): the data read before is not used any more.
        std::fs::remove_file(pdf.with_extension("synctex.gz")).unwrap();
        cmds.send(forward()).unwrap();
        assert!(matches!(answer(), DocEvent::SynctexFailed(m) if m.contains("no SyncTeX data")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dropping_a_pool_ends_its_threads() {
        // Each worker holds the sink; all of them gone closes the channel.
        // Dropped at once, the workers are still starting: many rounds give
        // a lost wakeup a chance to show.
        for _ in 0..300 {
            let (tx, rx) = mpsc::channel::<DocEvent>();
            drop(RasterPool::new(4, channel_sink(tx, |e| e)).unwrap());
            let end = rx.recv_timeout(std::time::Duration::from_secs(5));
            assert!(matches!(end, Err(mpsc::RecvTimeoutError::Disconnected)), "a worker is still waiting");
        }
    }

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
