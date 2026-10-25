//! Interactive mode: the event loop joining the terminal frontend
//! (`temzdf-tty`), the document's threads (`DocumentHost`) and the pane
//! (`temzdf-pdfview`), which decides what is shown and how input is interpreted.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use temzdf_pdfview::document::Document;
use temzdf_pdfview::host::{Forward, HostOptions, Opening};
use temzdf_scene::Images;
use temzdf_tty::{Input, Presented, TtyFrontend};
use termpix::tty::TermEvent;

use crate::config::{Config, ReloadAnchor};
use crate::ipc::{self, ForwardRequest};
use crate::pane::{Action, PdfPane};
use crate::viewer::StandIn;
use crate::workers::{self, DocEvent};

const FRAME: Duration = Duration::from_millis(16);

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

fn forward(req: ForwardRequest) -> Forward {
    Forward { file: req.file, line: req.line, column: req.column }
}

pub fn run(path: &Path, mut opts: Options) -> Result<()> {
    // Libraries (MuPDF, SyncTeX) print warnings to stdout/stderr, which would
    // scribble over the screen; send them to a log file while the UI runs.
    let _stdio = log_path().and_then(|p| termpix::tty::StdioRedirect::to_file(&p).ok());
    let term = opts.config.terminal.clone();
    let mut front = TtyFrontend::open(|caps| {
        let backend = term.backend(caps.backend(), caps.has_backend_override());
        let medium = term.medium(caps.medium(), caps.has_medium_override());
        (backend, medium, term.zlib_level)
    })?;
    let mut images = Images::new();

    let (tx, rx) = mpsc::channel::<AppEvent>();
    front.spawn_input(tx.clone(), AppEvent::Term)?;
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
                drop(front);
                bail!("{e}");
            }
            AppEvent::Term(TermEvent::Terminate) => return Ok(()),
            _ => {}
        }
    };
    if info.bounds.is_empty() {
        drop(front);
        bail!("the document has no pages");
    }
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let doc = Document::shared(info.bounds.clone());
    let mut session = PdfPane::with_document(name, info.clone(), doc.clone(), front.geometry());
    session.anchor_by_text = opts.config.reload.anchor == ReloadAnchor::Text;
    session.keymap = opts.config.keymap()?;
    session.preview_delay = opts.config.links.preview_delay();
    session.viewer.stand_in_mode = StandIn::for_hints(front.hints());
    session.viewer.scroll_x.config = opts.config.scroll.to_termpix();
    session.viewer.scroll_y.config = opts.config.scroll.to_termpix();
    let open_command = opts.config.links.command();
    let host_options = HostOptions {
        // Wait this long after the last change before reloading (see doc/reload.md).
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
    let report = |tx: &Sender<AppEvent>| {
        let tx = tx.clone();
        move |e| {
            let _ = tx.send(AppEvent::OpenFailed(e));
        }
    };

    let mut dirty = true;
    loop {
        if dirty && front.ready() {
            let (frame, needs) = session.frame(&mut images);
            front.sync(&mut images);
            host.dispatch(needs);
            if front.present(&frame, session.viewer.is_complete())? == Presented::Shown {
                dirty = false;
            }
        }
        if !dirty {
            front.idle()?;
        }
        // Requests are handled every iteration: some (Ctrl+click inverse search)
        // do not change the screen, so they must not wait for a redraw.
        for req in session.take_requests() {
            if let Some(effect) = host.request(req) {
                front.perform(effect, &open_command, report(&tx))?;
            }
        }
        let mut timeout = if session.is_animating() { FRAME } else { Duration::from_secs(3600) };
        for due in [host.next_deadline(), session.next_deadline()].into_iter().flatten() {
            timeout = timeout.min(due.saturating_duration_since(Instant::now()));
        }
        let event = match rx.recv_timeout(front.timeout(timeout)) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let now = Instant::now();
        match event {
            None => {}
            Some(AppEvent::Term(ev)) => match front.handle(ev)? {
                Input::Terminate => break,
                Input::Resized(g) => {
                    session.resize(g);
                    dirty = true;
                }
                Input::Key(key) => match session.key(&key, now) {
                    Action::Quit => break,
                    Action::Redraw => dirty = true,
                    Action::None => {}
                },
                Input::Mouse(m) => {
                    dirty |= session.mouse(&m, now);
                    front.set_pointer(session.pointer())?;
                }
                Input::Paste(text) => dirty |= session.paste(&text) == Action::Redraw,
                Input::Handled(redraw) => dirty |= redraw,
            },
            Some(AppEvent::ForwardRequest(req)) => host.forward(forward(req), &session),
            Some(AppEvent::OpenFailed(e)) => {
                session.message = Some(e);
                dirty = true;
            }
            Some(AppEvent::Doc(ev)) => {
                let (changed, effect) = host.handle(ev, &mut [&mut session], &mut images, now);
                front.sync(&mut images);
                dirty |= changed;
                if let Some(effect) = effect {
                    front.perform(effect, &open_command, report(&tx))?;
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

/// `$XDG_STATE_HOME/temzdf/temzdf.log` (falling back to `~/.local/state`).
fn log_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state")))?;
    Some(base.join("temzdf").join("temzdf.log"))
}
