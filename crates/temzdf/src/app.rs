//! Interactive standalone mode: one session in this process, shown in this
//! terminal. The event loop joins the terminal frontend (`temzdf-tty`) and the
//! session (`temzdf-server`), which holds the panes and their documents.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use temzdf_pdfview::host::HostOptions;
use temzdf_server::action::PaneSpec;
use temzdf_server::forward::ForwardRequest;
use temzdf_server::session::{EventSink, Outcome, ScriptSettings, Session, SessionEvent, Settings};
use temzdf_tty::{Input, Presented, TtyFrontend};
use termpix::tty::TermEvent;

use crate::config::{Config, ReloadAnchor};

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

/// Session settings from the configuration.
/// How the session runs the configuration script: none with `--clean` or
/// when there is no `config.ts`/`config.js`.
pub fn script_settings(clean: bool) -> Result<Option<ScriptSettings>> {
    if clean {
        return Ok(None);
    }
    let Some(config) = crate::config::script_path()? else { return Ok(None) };
    let exe = std::env::current_exe()?.display().to_string();
    Ok(Some(ScriptSettings { command: vec![exe, "script-host".into()], config }))
}

pub fn settings(config: &Config, serve_forward: bool, script: Option<ScriptSettings>) -> Result<Settings> {
    Ok(Settings {
        keymap: config.keymap()?,
        preview_delay: config.links.preview_delay(),
        anchor_by_text: config.reload.anchor == ReloadAnchor::Text,
        scroll: config.scroll.to_termpix(),
        host: HostOptions {
            // Wait this long after the last change before reloading (see doc/reload.md).
            debounce: Duration::from_millis(config.reload.debounce_ms),
            editor: config.synctex.editor.clone(),
        },
        serve_forward,
        script,
    })
}

/// An event sink feeding `tx`.
pub fn sink<E: Send + 'static>(tx: &Sender<E>, wrap: fn(SessionEvent) -> E) -> EventSink {
    let tx = Mutex::new(tx.clone());
    Arc::new(move |e| tx.lock().unwrap_or_else(|p| p.into_inner()).send(wrap(e)).is_ok())
}

pub fn run(path: &Path, mut opts: Options) -> Result<()> {
    // Libraries (MuPDF, SyncTeX) print warnings to stdout/stderr, which would
    // scribble over the screen; send them to a log file while the UI runs.
    let _stdio = log_path(&opts.config).and_then(|p| termpix::tty::StdioRedirect::to_file(&p).ok());
    let term = opts.config.terminal.clone();
    let mut front = TtyFrontend::open(|caps| {
        let backend = term.backend(caps.backend(), caps.has_backend_override());
        let medium = term.medium(caps.medium(), caps.has_medium_override());
        (backend, medium, term.zlib_level)
    })?;
    if let Some(editor) = opts.editor.take() {
        opts.config.synctex.editor = Some(editor);
    }
    let (tx, rx) = mpsc::channel::<AppEvent>();
    front.spawn_input(tx.clone(), AppEvent::Term)?;
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let settings = settings(&opts.config, true, script_settings(opts.clean)?)?;
    let mut session = Session::new(name, front.geometry(), front.hints(), settings, sink(&tx, AppEvent::Session));
    session.start(PaneSpec::Pdf(path.to_owned()), None)?;

    // Wait for the document before drawing anything.
    while session.is_starting() {
        match rx.recv()? {
            AppEvent::Session(ev) => {
                session.handle(ev, Instant::now());
            }
            AppEvent::Term(TermEvent::Terminate) => return Ok(()),
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
    let open_command = opts.config.links.command();
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
            front.perform(effect, &open_command, report(&tx))?;
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
                Input::Key(key) => session.key(&key, now),
                Input::Mouse(m) => {
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

/// Where logs go: `$TEMZDF_LOG_DIR`, else `log_dir` from the configuration,
/// else the state directory.
pub fn log_dir(config: &Config) -> Option<std::path::PathBuf> {
    log_dir_from(std::env::var_os("TEMZDF_LOG_DIR"), config.log_dir.as_deref(), std::env::var_os("HOME"))
        .or_else(state_dir)
}

fn log_dir_from(
    env: Option<std::ffi::OsString>,
    config: Option<&Path>,
    home: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    if let Some(dir) = env.filter(|d| !d.is_empty()) {
        return Some(dir.into());
    }
    let dir = config?;
    match (dir.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => Some(std::path::PathBuf::from(home).join(rest)),
        _ => Some(dir.to_owned()),
    }
}

/// Where the viewer's (or a client's) own output goes while it runs:
/// `<log dir>/temzdf.log`.
pub fn log_path(config: &Config) -> Option<std::path::PathBuf> {
    let dir = log_dir(config)?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("temzdf.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_dir_precedence() {
        let home = Some("/home/u".into());
        let cfg = Some(Path::new("~/logs"));
        assert_eq!(log_dir_from(Some("/env".into()), cfg, home.clone()), Some("/env".into()));
        assert_eq!(log_dir_from(Some("".into()), cfg, home.clone()), Some("/home/u/logs".into()));
        assert_eq!(log_dir_from(None, Some(Path::new("/abs")), home.clone()), Some("/abs".into()));
        assert_eq!(log_dir_from(None, None, home), None, "then the state directory");
    }
}
