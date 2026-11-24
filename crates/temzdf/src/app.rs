//! Interactive standalone mode: one session in this process, shown in this
//! terminal. The event loop joins the terminal frontend (`temzdf-tty`) and the
//! session (`temzdf-server`), which holds the panes and their documents.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use temzdf_server::action::PaneSpec;
use temzdf_server::forward::Forward;
use temzdf_server::session::{EventSink, Outcome, Session, SessionEvent};
use temzdf_tty::{Input, Presented};
use termpix::tty::TermEvent;

const FRAME: Duration = Duration::from_millis(16);

pub struct Options {
    /// 0-based page to show first.
    pub page: Option<usize>,
    pub scale: Option<f32>,
    /// The configuration script given with `--config`.
    pub config: Option<PathBuf>,
    /// Forward search to perform once the document is shown.
    pub forward: Option<Forward>,
    /// Do not run the configuration script.
    pub clean: bool,
}

/// Everything the main loop waits for.
pub enum AppEvent {
    Term(TermEvent),
    Session(SessionEvent),
    /// Opening an external link failed.
    OpenFailed(String),
}

/// An event sink feeding `tx`.
pub fn sink<E: Send + 'static>(tx: &Sender<E>, wrap: fn(SessionEvent) -> E) -> EventSink {
    temzdf_scene::sink::channel_sink(tx.clone(), wrap)
}

/// The session name for a file: its file name.
pub fn session_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Show `page` and `scale` (as given on the command line) in the focused pane.
pub fn apply_view(session: &mut Session, page: Option<usize>, scale: Option<f32>, now: Instant) {
    if let Some(pane) = session.focused_pane() {
        if let Some(scale) = scale {
            pane.viewer.set_scale(scale, (0.0, 0.0));
        }
        if let Some(page) = page {
            pane.viewer.goto_page(page, false, now);
        }
    }
}

pub fn run(path: &Path, mut opts: Options) -> Result<()> {
    // Libraries (MuPDF, SyncTeX) print warnings to stdout/stderr, which would
    // scribble over the screen; send them to a log file while the UI runs.
    let _stdio = log_path().and_then(|p| termpix::tty::StdioRedirect::to_file(&p).ok());
    let mut front = crate::attach::open_frontend()?;
    let (tx, rx) = mpsc::channel::<AppEvent>();
    front.spawn_input(tx.clone(), AppEvent::Term)?;
    let name = session_name(path);
    let script = crate::config::script_settings(opts.clean, opts.config.as_deref())?;
    let settings = crate::config::settings(true, script);
    let mut session = Session::new(name, front.geometry(), front.hints(), settings, sink(&tx, AppEvent::Session));
    session.start(PaneSpec::File(path.to_owned()), None)?;
    // TEMZDF_RECORD_INPUT=FILE: what this viewer handles, for replaying
    // (scripts/e2e.sh judges mouse actions with it). From here: forward
    // searches that come while the document opens count too.
    let mut recorder = crate::headless::Recorder::from_env()?;

    // Wait for the document before drawing anything.
    while session.is_starting() {
        match rx.recv()? {
            AppEvent::Session(ev) => {
                let now = Instant::now();
                if let (Some(r), SessionEvent::Forward(_, req)) = (&mut recorder, &ev) {
                    r.forward(req, now);
                }
                session.handle(ev, now);
            }
            AppEvent::Term(TermEvent::Terminate) => return Ok(()),
            // Resizes count already (keys wait for the document).
            AppEvent::Term(ev) => {
                if let Input::Resized(g) = front.handle(ev)? {
                    session.resize(g);
                }
            }
            _ => {}
        }
    }
    if let Some(e) = session.error() {
        let e = e.to_owned();
        drop(front);
        bail!("{e}");
    }
    apply_view(&mut session, opts.page, opts.scale, Instant::now());
    if let Some(req) = opts.forward.take() {
        let now = Instant::now();
        if let Some(r) = &mut recorder {
            r.forward(&req, now);
        }
        session.forward(req, now);
    }
    let report = |tx: &Sender<AppEvent>| {
        let tx = tx.clone();
        move |e| {
            let _ = tx.send(AppEvent::OpenFailed(e));
        }
    };

    let mut dirty = true;
    loop {
        if dirty && front.ready() && !session.holds_frames() {
            let frame = session.frame();
            front.sync(session.images());
            if front.present(&frame, session.is_complete())? == Presented::Shown {
                dirty = false;
            }
        }
        if !dirty {
            front.idle()?;
        }
        // Effects are handled every iteration: some (Ctrl+click inverse search)
        // do not change the screen, so they must not wait for a redraw.
        for effect in session.take_effects() {
            front.perform(effect, report(&tx))?;
        }
        let mut timeout = if session.is_animating() { FRAME } else { Duration::from_secs(3600) };
        if let Some(due) = session.next_deadline() {
            timeout = timeout.min(due.saturating_duration_since(Instant::now()));
        }
        let event = match rx.recv_timeout(front.timeout(timeout)) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let now = Instant::now();
        let outcome = match event {
            None => Outcome::None,
            Some(AppEvent::Term(ev)) => match front.handle(ev)? {
                Input::Terminate => break,
                Input::Resized(g) => {
                    session.resize(g);
                    Outcome::Redraw
                }
                Input::Key(key) => {
                    if let Some(r) = &mut recorder {
                        r.key(&key, now);
                    }
                    session.key(&key, now)
                }
                Input::Mouse(m) => {
                    if let Some(r) = &mut recorder {
                        r.mouse(&m, now);
                    }
                    let o = session.mouse(&m, now);
                    front.set_pointer(session.pointer())?;
                    o
                }
                Input::Paste(text) => session.paste(&text),
                Input::Clipboard { ticket, result } => session.clipboard_answer(ticket, result),
                Input::Focus(gained) => session.client_focus(gained),
                Input::Handled(true) => Outcome::Redraw,
                Input::Handled(false) => Outcome::None,
            },
            Some(AppEvent::Session(ev)) => {
                if let (Some(r), SessionEvent::Forward(_, req)) = (&mut recorder, &ev) {
                    r.forward(req, now);
                }
                let o = session.handle(ev, now);
                front.sync(session.images());
                o
            }
            Some(AppEvent::OpenFailed(e)) => {
                session.report(e);
                Outcome::Redraw
            }
        };
        let outcome = outcome.merge(session.poll(Instant::now()));
        match outcome {
            Outcome::Quit => break,
            Outcome::Detach => {
                session.report("not in a session: nothing to detach from".into());
                dirty = true;
            }
            Outcome::Redraw => dirty = true,
            Outcome::None => {}
        }
    }
    Ok(())
}

pub use temzdf_client::log_path;
