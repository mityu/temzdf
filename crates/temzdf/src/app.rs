//! Interactive mode: terminal setup and the event loop. What is shown and how
//! input is interpreted lives in `temzdf-pdfview` (`pane.rs`).

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use temzdf_scene::{Images, Pointer};
use termpix::compositor::{Backend, Compositor};
use termpix::graphics::Medium;
use termpix::input::Event;
use termpix::tty::{self, Modes, TermEvent, Tty};

use crate::config::{Config, ReloadAnchor};
use crate::ipc::{self, ForwardRequest};
use crate::pane::{Action, PdfPane};
use crate::viewer::StandIn;
use crate::workers::{self, DocEvent};
use temzdf_pdfview::document::Document;
use temzdf_pdfview::host::{Forward, HostOptions, Opening};
use temzdf_scene::Effect;
use temzdf_tty::{Presenter, Screen};

const FRAME: Duration = Duration::from_millis(16);
/// Raw bytes of not-yet-shown images sent per idle iteration (about four
/// 512 px tiles); small enough that input is never kept waiting.
const UPLOAD_AHEAD_BATCH: usize = 4 << 20;
/// How often to look whether queued output was written, while it is.
const OUTPUT_POLL: Duration = Duration::from_millis(5);
/// Longest wait for the visible pages before showing the first frame.
const FIRST_FRAME_WAIT: Duration = Duration::from_millis(1500);

pub struct Options {
    /// 0-based page to show first.
    pub page: Option<usize>,
    pub scale: Option<f32>,
    pub config: Config,
    /// Forward search to perform once the document is shown.
    pub forward: Option<ForwardRequest>,
    /// Inverse search command (overrides the config file).
    pub editor: Option<String>,
}

/// Everything the main loop waits for.
pub enum AppEvent {
    Term(TermEvent),
    Doc(DocEvent),
    /// Another process asked for a forward search (`temzdf --forward`).
    ForwardRequest(ForwardRequest),
    /// Opening an external link failed.
    OpenFailed(String),
}

impl From<DocEvent> for AppEvent {
    fn from(e: DocEvent) -> Self {
        Self::Doc(e)
    }
}

fn forward(req: ForwardRequest) -> Forward {
    Forward { file: req.file, line: req.line, column: req.column }
}

pub fn run(path: &Path, mut opts: Options) -> Result<()> {
    // Libraries (MuPDF, SyncTeX) print warnings to stdout/stderr, which would
    // scribble over the screen; send them to a log file while the UI runs.
    let _stdio = log_path().and_then(|p| termpix::tty::StdioRedirect::to_file(&p).ok());
    let mut tty = Tty::open(Modes::default())?;
    let caps = termpix::probe::probe(&mut tty, Duration::from_secs(1))?;
    if !caps.kitty_graphics {
        drop(tty);
        match &caps.tmux {
            Some(t) if !t.passthrough => {
                bail!("inside tmux, graphics need passthrough: run `tmux set -g allow-passthrough on`")
            }
            Some(t) => bail!(
                "inside tmux, graphics need an outer terminal with Unicode placeholder support \
                 (kitty, Ghostty); tmux reports {:?}",
                t.outer
            ),
            None if caps.in_tmux => bail!("inside tmux, but the `tmux` command could not be queried"),
            None => bail!("this terminal does not support the kitty graphics protocol"),
        }
    }
    if caps.pixel_mouse {
        tty.enable_pixel_mouse()?;
    }
    let term = &opts.config.terminal;
    let backend = term.backend(caps.backend(), caps.has_backend_override());
    let medium = term.medium(caps.medium(), caps.has_medium_override());
    let mut comp = Compositor::new(backend, medium, caps.geometry);
    let mut images = Images::new();
    let mut presenter = Presenter::new();
    // In the log (stderr is redirected): scripts/e2e.sh reads the geometry to
    // render the expected screen headlessly.
    eprintln!("temzdf: {} with {backend:?} over {medium:?}", geometry_line(caps.geometry));
    comp.encoder_mut().set_zlib_level(term.zlib_level);
    if caps.in_tmux {
        comp.set_tmux_passthrough(true);
        tty.set_tmux_passthrough();
    }

    let (tx, rx) = mpsc::channel::<AppEvent>();
    tty::spawn_input(tty.reader()?, caps.pixel_mouse, tx.clone(), AppEvent::Term)?;
    let opening = Opening::start(path, workers::channel_sink(tx.clone(), AppEvent::Doc))?;
    // Forward-search requests from other processes; a second viewer of the same
    // file just goes without.
    let _ipc = ipc::serve(path, tx.clone(), AppEvent::ForwardRequest).ok();
    if let Some(editor) = opts.editor.take() {
        opts.config.synctex.editor = Some(editor);
    }

    // Wait for the document before drawing anything.
    let info = loop {
        match rx.recv()? {
            AppEvent::Doc(DocEvent::Opened(Ok(info))) => break info,
            AppEvent::Doc(DocEvent::Opened(Err(e))) => {
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
    let doc = Document::shared(info.bounds.clone());
    let mut session = PdfPane::with_document(name.clone(), info.clone(), doc.clone(), caps.geometry);
    session.anchor_by_text = opts.config.reload.anchor == ReloadAnchor::Text;
    session.keymap = opts.config.keymap()?;
    session.preview_delay = opts.config.links.preview_delay();
    session.viewer.stand_in_mode = match (comp.backend(), comp.encoder_mut().medium()) {
        (Backend::Layered, Medium::Direct { .. }) => StandIn::Thumbnail,
        (Backend::Layered, _) => StandIn::Composed,
        _ => StandIn::ScaledTiles,
    };
    session.viewer.scroll_x.config = opts.config.scroll.to_termpix();
    session.viewer.scroll_y.config = opts.config.scroll.to_termpix();
    let open_command = opts.config.links.command();
    // Wait this long after the last change before reloading (see doc/reload.md).
    let host_options = HostOptions {
        debounce: Duration::from_millis(opts.config.reload.debounce_ms),
        editor: opts.config.synctex.editor.clone(),
    };
    let mut host = opening.finish(info, doc, host_options);
    if let Some(e) = host.watcher_error.take() {
        session.message = Some(e);
    }
    if let Some(req) = opts.forward.take() {
        host.forward(forward(req), &session);
    }
    if let Some(scale) = opts.scale {
        session.viewer.set_scale(scale, (0.0, 0.0));
    }
    if let Some(page) = opts.page {
        session.viewer.goto_page(page, false, Instant::now());
    }

    let mut dirty = true;
    let mut screen = Screen::new();
    let mut shown_pointer = Pointer::Default;
    let started = Instant::now();
    let mut first_shown = false;
    let mut last_present: Option<Instant> = None;
    // Composited frames resend whole strips of the screen; multiplexers (zellij
    // 0.45 forwards them uncompressed) and inline links pay for every one, so
    // cap their rate. Layered frames are tiny placement updates.
    let composited = matches!(comp.backend(), Backend::Composited { .. } | Backend::Placeholders { .. });
    // tmux passes our (compressed) bytes through as they are; zellij re-encodes
    // every image uncompressed; inline transfers without a multiplexer usually
    // mean SSH.
    let slow_link = caps.in_zellij || (!caps.in_tmux && matches!(comp.encoder_mut().medium(), Medium::Direct { .. }));
    let min_interval = match (composited, slow_link) {
        (false, _) => Duration::ZERO,
        (true, false) => Duration::from_millis(33),
        (true, true) => Duration::from_millis(200),
    };
    loop {
        // On a slow link, wait until the terminal caught up with earlier frames;
        // the frame drawn afterwards shows the latest state anyway. Output is
        // written by a background thread (input stays responsive meanwhile);
        // frames queued behind it would only pile up.
        let writing = tty.pending_output() > 0;
        let congested = dirty && (writing || comp.is_congested(Instant::now()));
        let mut held = false;
        if dirty && !congested {
            let (frame, needs) = session.frame(&mut images);
            presenter.sync(&mut comp, &mut images);
            host.dispatch(needs);
            // Do not show half-rendered first frames (each costs a full upload in
            // Composited mode), and respect the frame-rate cap.
            let hold_first = !first_shown && !session.viewer.is_complete() && started.elapsed() < FIRST_FRAME_WAIT;
            let too_soon = last_present.is_some_and(|t| t.elapsed() < min_interval);
            held = hold_first || too_soon;
            if !held {
                let bytes = screen.render(&frame, &presenter, &mut comp)?;
                if !bytes.is_empty() {
                    tty.write_all(bytes)?;
                }
                first_shown = true;
                last_present = Some(Instant::now());
                dirty = false;
            }
        }
        // Idle and the link is free: send pre-rendered neighbour tiles now, so
        // scrolling to them later costs only a placement (matters over SSH).
        let mut uploading = false;
        if !dirty && !writing && !comp.is_congested(Instant::now()) && comp.has_pending_uploads() {
            let mut ahead = Vec::new();
            uploading = comp.upload_ahead(&mut ahead, UPLOAD_AHEAD_BATCH)?;
            if !ahead.is_empty() {
                tty.write_all(&ahead)?;
            }
        }
        // Requests are dispatched every iteration: some (Ctrl+click inverse search)
        // do not change the screen, so they must not wait for a redraw.
        for req in session.take_requests() {
            if let Some(effect) = host.request(req) {
                perform(effect, &open_command, &tx, &mut tty, caps.in_tmux)?;
            }
        }
        let mut timeout = if session.is_animating() { FRAME } else { Duration::from_secs(3600) };
        for due in [host.next_deadline(), session.next_deadline()].into_iter().flatten() {
            timeout = timeout.min(due.saturating_duration_since(Instant::now()));
        }
        if congested || held || uploading {
            // Re-check soon: the acknowledgement may never come, the frame-rate
            // interval passes, or the first frame stops waiting.
            timeout = timeout.min(Duration::from_millis(30));
        }
        if tty.pending_output() > 0 {
            // Nothing signals the end of a write; poll for it.
            timeout = timeout.min(OUTPUT_POLL);
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
                    eprintln!("temzdf: resized to {}", geometry_line(g));
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
                    tty.write_all(&temzdf_tty::pointer_sequence(shown_pointer))?;
                }
            }
            Some(AppEvent::Term(TermEvent::Input(Event::Paste(text)))) => {
                dirty |= session.paste(&text) == Action::Redraw;
            }
            Some(AppEvent::Term(_)) => {}
            Some(AppEvent::ForwardRequest(req)) => host.forward(forward(req), &session),
            Some(AppEvent::OpenFailed(e)) => {
                session.message = Some(e);
                dirty = true;
            }
            Some(AppEvent::Doc(ev)) => {
                let (changed, effect) = host.handle(ev, &mut [&mut session], &mut images, now);
                presenter.sync(&mut comp, &mut images);
                dirty |= changed;
                if let Some(effect) = effect {
                    perform(effect, &open_command, &tx, &mut tty, caps.in_tmux)?;
                }
            }
        }
        dirty |= host.poll(&mut [&mut session], Instant::now());
        dirty |= session.tick(Instant::now());
        // A highlight expired.
        dirty |= session.next_deadline().is_some_and(|d| Instant::now() >= d);
    }
    Ok(())
}

/// "geometry COLSxROWS cells of WxH px" (the format scripts/e2e.sh parses).
fn geometry_line(g: termpix::geom::Geometry) -> String {
    format!("geometry {}x{} cells of {}x{} px", g.cols, g.rows, g.cell.width, g.cell.height)
}

/// `$XDG_STATE_HOME/temzdf/temzdf.log` (falling back to `~/.local/state`).
fn log_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state")))?;
    Some(base.join("temzdf").join("temzdf.log"))
}

/// Run a shell command detached from the terminal (inverse search).
fn run_shell(cmd: &str) {
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Open a URI with the configured handler, detached from the terminal. A
/// handler that cannot start or exits with an error (e.g. `xdg-open` over SSH
/// without a display) is reported as [`AppEvent::OpenFailed`].
fn open_uri(command: &[String], uri: &str, tx: &Sender<AppEvent>) {
    let Some((program, args)) = command.split_first() else { return };
    let child = std::process::Command::new(program)
        .args(args)
        .arg(uri)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let uri = uri.to_owned();
    let fail = move |why: String| AppEvent::OpenFailed(format!("cannot open {uri}: {why}"));
    match child {
        Err(e) => {
            let _ = tx.send(fail(format!("{program}: {e}")));
        }
        Ok(mut child) => {
            let (tx, program) = (tx.clone(), program.clone());
            std::thread::spawn(move || match child.wait() {
                Ok(status) if !status.success() => {
                    let _ = tx.send(fail(format!("{program} failed ({status})")));
                }
                _ => {}
            });
        }
    }
}

/// Do what the pane asked the frontend for.
fn perform(effect: Effect, open_command: &[String], tx: &Sender<AppEvent>, tty: &mut Tty, in_tmux: bool) -> Result<()> {
    match effect {
        Effect::Copy(text) => tty.write_all(&temzdf_tty::copy_sequence(&text, in_tmux))?,
        Effect::OpenUri(uri) => open_uri(open_command, &uri, tx),
        Effect::RunShell(cmd) => run_shell(&cmd),
    }
    Ok(())
}
