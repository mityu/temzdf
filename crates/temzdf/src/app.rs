//! Interactive standalone mode: one session in this process, shown in this
//! terminal. The event loop joins the terminal frontend (`temzdf-tty`) and the
//! session (`temzdf-server`), which holds the panes and their documents.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use temzdf_server::action::PaneSpec;
use temzdf_server::forward::ForwardRequest;
use temzdf_server::session::{EventSink, Outcome, ScriptSettings, Session, SessionEvent, Settings};
use temzdf_tty::{Input, Presented, TtyFrontend};
use termpix::tty::TermEvent;

use temzdf_protocol::script::ScriptValue;

const FRAME: Duration = Duration::from_millis(16);

pub struct Options {
    /// 0-based page to show first.
    pub page: Option<usize>,
    pub scale: Option<f32>,
    /// The configuration script given with `--config`.
    pub config: Option<PathBuf>,
    /// Forward search to perform once the document is shown.
    pub forward: Option<ForwardRequest>,
    /// Inverse search command (overrides the config file).
    pub editor: Option<String>,
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

/// How the session runs the script host: the shipped defaults, then the
/// configuration (`explicit`, else `config.ts`/`config.js`) unless `clean`.
pub fn script_settings(clean: bool, explicit: Option<&Path>) -> Result<Option<ScriptSettings>> {
    let config = match (clean, explicit) {
        (true, _) => None,
        (false, Some(p)) if !p.is_file() => bail!("no configuration file {}", p.display()),
        (false, Some(p)) => Some(std::path::absolute(p)?),
        (false, None) => crate::config::script_path()?,
    };
    let exe = std::env::current_exe()?.display().to_string();
    Ok(Some(ScriptSettings { command: vec![exe, "script-host".into()], config }))
}

/// Session settings; `editor` (`--editor`) wins over the configuration's.
pub fn settings(serve_forward: bool, script: Option<ScriptSettings>, editor: Option<&str>) -> Settings {
    let overrides =
        editor.map(|e| vec![("synctexeditor".to_owned(), ScriptValue::Str(e.to_owned()))]).unwrap_or_default();
    Settings { serve_forward, script, overrides }
}

/// The zlib level for inline image transfers: `$TEMZDF_ZLIB_LEVEL`, else
/// termpix's default (do not use 1 with Ghostty 1.3.1, see
/// doc/terminal-compat.md).
pub fn zlib_level() -> u32 {
    std::env::var("TEMZDF_ZLIB_LEVEL")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .map_or(termpix::graphics::DEFAULT_ZLIB_LEVEL, |l| l.min(9))
}

/// Open this terminal as a frontend: what detection found (and the
/// `TERMPIX_BACKEND`/`TERMPIX_MEDIUM` overrides).
pub fn open_frontend() -> Result<TtyFrontend> {
    TtyFrontend::open(|caps| (caps.backend(), caps.medium(), zlib_level()))
}

/// An event sink feeding `tx`.
pub fn sink<E: Send + 'static>(tx: &Sender<E>, wrap: fn(SessionEvent) -> E) -> EventSink {
    let tx = Mutex::new(tx.clone());
    Arc::new(move |e| tx.lock().unwrap_or_else(|p| p.into_inner()).send(wrap(e)).is_ok())
}

pub fn run(path: &Path, mut opts: Options) -> Result<()> {
    // Libraries (MuPDF, SyncTeX) print warnings to stdout/stderr, which would
    // scribble over the screen; send them to a log file while the UI runs.
    let _stdio = log_path().and_then(|p| termpix::tty::StdioRedirect::to_file(&p).ok());
    let mut front = open_frontend()?;
    let (tx, rx) = mpsc::channel::<AppEvent>();
    front.spawn_input(tx.clone(), AppEvent::Term)?;
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let settings = settings(true, script_settings(opts.clean, opts.config.as_deref())?, opts.editor.as_deref());
    let mut session = Session::new(name, front.geometry(), front.hints(), settings, sink(&tx, AppEvent::Session));
    session.start(PaneSpec::Pdf(path.to_owned()), None)?;

    // Wait for the document before drawing anything.
    while session.is_starting() {
        match rx.recv()? {
            AppEvent::Session(ev) => {
                session.handle(ev, Instant::now());
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
    if let Some(pane) = session.focused_pane() {
        if let Some(scale) = opts.scale {
            pane.viewer.set_scale(scale, (0.0, 0.0));
        }
        if let Some(page) = opts.page {
            pane.viewer.goto_page(page, false, Instant::now());
        }
    }
    if let Some(req) = opts.forward.take() {
        session.forward(req);
    }
    let report = |tx: &Sender<AppEvent>| {
        let tx = tx.clone();
        move |e| {
            let _ = tx.send(AppEvent::OpenFailed(e));
        }
    };

    // TEMZDF_RECORD_INPUT=FILE: what this viewer handles, for replaying
    // (scripts/e2e.sh judges mouse actions with it).
    let mut recorder = crate::headless::Recorder::from_env()?;
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
                Input::Handled(true) => Outcome::Redraw,
                Input::Handled(false) => Outcome::None,
            },
            Some(AppEvent::Session(ev)) => {
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

/// `$XDG_STATE_HOME/temzdf` (falling back to `~/.local/state/temzdf`).
pub fn state_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state")))?;
    Some(base.join("temzdf"))
}

/// Where logs go: `$TEMZDF_LOG_DIR`, else the state directory.
pub fn log_dir() -> Option<PathBuf> {
    std::env::var_os("TEMZDF_LOG_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).or_else(state_dir)
}

/// Where the viewer's (or a client's) own output goes while it runs:
/// `<log dir>/temzdf.log`.
pub fn log_path() -> Option<PathBuf> {
    let dir = log_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("temzdf.log"))
}
