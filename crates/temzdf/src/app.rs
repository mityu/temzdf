//! Interactive mode: terminal setup and the event loop. What is shown and how
//! input is interpreted lives in `session.rs`.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use termpix::compositor::Compositor;
use termpix::input::Event;
use termpix::tty::{self, Modes, TermEvent, Tty};

use crate::config::Config;
use crate::pdf;
use crate::session::{Action, Pointer, Request, Session};
use crate::viewer::Need;
use crate::workers::{self, AppEvent, DocCmd, RasterPool, SearchCmd};

const FRAME: Duration = Duration::from_millis(16);
/// Wait this long after the last change before reloading (see doc/reload.md).
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(200);
/// Retry interval while the file is incomplete or fails to open.
const RELOAD_RETRY: Duration = Duration::from_millis(300);
/// Give up retrying after this many attempts (about 30 s); a new change event
/// starts over.
const RELOAD_MAX_ATTEMPTS: u32 = 100;

pub struct Options {
    /// 0-based page to show first.
    pub page: Option<usize>,
    pub scale: Option<f32>,
    pub config: Config,
}

pub fn run(path: &Path, opts: Options) -> Result<()> {
    let mut tty = Tty::open(Modes::default())?;
    let caps = termpix::probe::probe(&mut tty, Duration::from_secs(1))?;
    if !caps.kitty_graphics {
        drop(tty);
        bail!("this terminal does not support the kitty graphics protocol");
    }
    if caps.pixel_mouse {
        tty.enable_pixel_mouse()?;
    }
    let term = &opts.config.terminal;
    let backend = term.backend(caps.backend(), caps.has_backend_override());
    let medium = term.medium(caps.medium(), caps.has_medium_override());
    let mut comp = Compositor::new(backend, medium, caps.geometry);
    comp.encoder_mut().set_zlib_level(term.zlib_level);

    let (tx, rx) = mpsc::channel::<AppEvent>();
    tty::spawn_input(tty.reader()?, caps.pixel_mouse, tx.clone(), AppEvent::Term)?;
    let doc = workers::spawn_document(path.to_owned(), tx.clone())?;
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4));
    let pool = RasterPool::new(threads, tx.clone())?;
    let search = workers::spawn_search(path.to_owned(), tx.clone())?;
    // Keep the watcher alive for the whole session. Without it the viewer still
    // works, just without auto-reload.
    let watcher = workers::spawn_watcher(path, tx.clone());

    // Wait for the document before drawing anything.
    let info = loop {
        match rx.recv()? {
            AppEvent::Opened(Ok(info)) => break info,
            AppEvent::Opened(Err(e)) => {
                drop(tty);
                bail!("{e}");
            }
            AppEvent::Term(TermEvent::Terminate) => return Ok(()),
            _ => {}
        }
    };
    if info.bounds.is_empty() {
        drop(tty);
        bail!("the document has no pages");
    }
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let mut session = Session::new(name.clone(), info, caps.geometry);
    session.viewer.scroll_x.config = opts.config.scroll.to_termpix();
    session.viewer.scroll_y.config = opts.config.scroll.to_termpix();
    let open_command = opts.config.links.command();
    if let Err(e) = &watcher {
        session.message = Some(format!("auto-reload disabled: {e}"));
    }
    let mut reload = ReloadState::default();
    if let Some(scale) = opts.scale {
        session.viewer.set_scale(scale, (0.0, 0.0));
    }
    if let Some(page) = opts.page {
        session.viewer.goto_page(page, false, Instant::now());
    }

    let mut dirty = true;
    let mut out = Vec::new();
    let mut title = String::new();
    let mut shown_cursor = None;
    let mut shown_pointer = Pointer::Default;
    loop {
        if dirty {
            let (layers, needs) = session.frame(&mut comp);
            dispatch(&mut session, &doc, &pool, needs);
            out.clear();
            let t = format!("{name} — {}", session.page_text());
            if t != title {
                // Control characters would end the OSC early.
                let clean: String = t.chars().filter(|c| !c.is_control()).collect();
                out.extend_from_slice(format!("\x1b]2;{clean}\x1b\\").as_bytes());
                title = t;
            }
            comp.present(&layers, &mut out)?;
            // Show the text cursor only at an input caret (IME preedit follows it).
            let cursor = session.cursor();
            if cursor != shown_cursor {
                match cursor {
                    Some((col, row)) => {
                        out.extend_from_slice(format!("\x1b[{};{}H\x1b[?25h", row + 1, col + 1).as_bytes())
                    }
                    None => out.extend_from_slice(b"\x1b[?25l"),
                }
                shown_cursor = cursor;
            } else if let Some((col, row)) = cursor
                && !out.is_empty()
            {
                // Placing images moves the cursor; put it back.
                out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
            }
            if !out.is_empty() {
                tty.write_all(&out)?;
            }
            for req in session.take_requests() {
                match req {
                    Request::Search { generation, query, start_page, doc_generation } => {
                        let _ = search.send(SearchCmd { generation, query, start_page, doc_generation });
                    }
                    Request::OpenUri(uri) => open_uri(&open_command, &uri),
                    Request::Copy(text) => {
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
                        tty.write_all(format!("\x1b]52;c;{b64}\x1b\\").as_bytes())?;
                    }
                }
            }
            dirty = false;
        }
        let mut timeout = if session.viewer.is_animating() { FRAME } else { Duration::from_secs(3600) };
        if let Some(due) = reload.due {
            timeout = timeout.min(due.saturating_duration_since(Instant::now()));
        }
        let event = match rx.recv_timeout(timeout) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let now = Instant::now();
        match event {
            None => {}
            Some(AppEvent::Term(TermEvent::Terminate)) => break,
            Some(AppEvent::Term(TermEvent::WindowChanged))
            | Some(AppEvent::Term(TermEvent::Input(Event::Resize(_)))) => {
                if let Some(g) = tty.winsize()?.geometry()
                    && g != session.geometry()
                {
                    comp.set_geometry(g);
                    session.resize(g);
                    dirty = true;
                }
            }
            Some(AppEvent::Term(TermEvent::Input(Event::Graphics(reply)))) => dirty |= comp.handle_reply(&reply),
            Some(AppEvent::Term(TermEvent::Input(Event::Key(key)))) => match session.key(&key, now) {
                Action::Quit => break,
                Action::Redraw => dirty = true,
                Action::None => {}
            },
            Some(AppEvent::Term(TermEvent::Input(Event::Mouse(m)))) => {
                dirty |= session.mouse(&m, now);
                if session.pointer() != shown_pointer {
                    shown_pointer = session.pointer();
                    let name = match shown_pointer {
                        Pointer::Default => "default",
                        Pointer::Link => "pointer",
                    };
                    tty.write_all(format!("\x1b]22;{name}\x1b\\").as_bytes())?;
                }
            }
            Some(AppEvent::Term(TermEvent::Input(Event::Paste(text)))) => {
                dirty |= session.paste(&text) == Action::Redraw;
            }
            Some(AppEvent::Term(_)) => {}
            Some(AppEvent::Opened(_)) => {}
            Some(AppEvent::PageLoaded(generation, p, list, links)) => {
                if generation == session.viewer.generation() {
                    session.viewer.page_loaded(generation, p, list);
                    session.page_links(p, links);
                    dirty = true;
                }
            }
            Some(AppEvent::PageFailed(generation, p, e)) => {
                session.viewer.page_failed(generation, p);
                session.message = Some(format!("page {}: {e}", p + 1));
                dirty = true;
            }
            Some(AppEvent::Rendered(r)) => {
                session.viewer.rendered(&mut comp, r);
                dirty = true;
            }
            Some(AppEvent::SearchHits { generation, page, rects }) => {
                dirty |= session.search_hits(generation, page, rects, now);
            }
            Some(AppEvent::SearchFailed(e)) => {
                session.message = Some(format!("search: {e}"));
                dirty = true;
            }
            Some(AppEvent::RenderFailed(key, e)) => {
                session.viewer.render_cancelled(key);
                session.message = Some(format!("render: {e}"));
                dirty = true;
            }
            Some(AppEvent::FileChanged) => {
                reload.changed(now);
                session.reloading = true;
                dirty = true;
            }
            Some(AppEvent::Reloaded(info)) => {
                reload.in_flight = false;
                reload.attempts = 0;
                session.reload(&mut comp, info);
                session.reloading = reload.due.is_some();
                dirty = true;
            }
            Some(AppEvent::ReloadFailed(e)) => {
                reload.in_flight = false;
                if !reload.retry(now) {
                    session.reloading = false;
                    session.message = Some(format!("reload failed: {e}"));
                }
                dirty = true;
            }
        }
        // Start a due reload: only once the file looks completely written.
        if let Some(due) = reload.due
            && Instant::now() >= due
            && !reload.in_flight
        {
            reload.due = None;
            if pdf::looks_complete(path).unwrap_or(false) {
                reload.in_flight = true;
                let _ = doc.send(DocCmd::Reload);
            } else if !reload.retry(Instant::now()) {
                session.reloading = false;
                session.message = Some("reload: the file stays incomplete or missing".into());
                dirty = true;
            }
        }
        dirty |= session.viewer.tick(Instant::now());
    }
    Ok(())
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
    fn changed(&mut self, now: Instant) {
        self.due = Some(now + RELOAD_DEBOUNCE);
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

/// Open a URI with the configured handler, detached from the terminal.
fn open_uri(command: &[String], uri: &str) {
    let Some((program, args)) = command.split_first() else { return };
    let _ = std::process::Command::new(program)
        .args(args)
        .arg(uri)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn dispatch(session: &mut Session, doc: &Sender<DocCmd>, pool: &RasterPool, needs: Vec<Need>) {
    let mut jobs = Vec::new();
    for need in needs {
        session.viewer.mark_requested(&need);
        match need {
            Need::LoadPage(p) => {
                let _ = doc.send(DocCmd::LoadPage(p));
            }
            Need::Render(job) => jobs.push(job),
        }
    }
    for key in pool.replace(jobs) {
        session.viewer.render_cancelled(key);
    }
}
