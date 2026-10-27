//! `temzdf ls-fonts`: the GUI's fonts for the configured font lists. This
//! finds the lists (given, a session's, or the configuration's) and leaves
//! the rest to `temzdf-gui ls-fonts`, which has the fonts built in.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use temzdf_protocol::{ClientMsg, ServerMsg};
use temzdf_scene::{Appearance, FontEntry, Hints};
use temzdf_server::action::PaneSpec;
use temzdf_server::session::{EventSink, Session};
use termpix::geom::{CellSize, Geometry};

/// How long loading the configuration may take.
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// The font lists of session `name`.
pub fn from_session(name: &str) -> Result<Appearance> {
    match crate::client::request(name, ClientMsg::GetAppearance, Duration::from_secs(10))? {
        ServerMsg::Appearance(a) => Ok(a),
        other => bail!("unexpected answer {other:?}"),
    }
}

/// The font lists a new session would get: the configuration loaded in a
/// session of this process, as `temzdf` would start one.
pub fn from_configuration(clean: bool, config: Option<&Path>) -> Result<Appearance> {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap_or_else(|p| p.into_inner()).send(e).is_ok());
    let settings = crate::app::settings(false, crate::app::script_settings(clean, config)?, None);
    let geometry = Geometry { cols: 80, rows: 24, cell: CellSize { width: 10, height: 20 } };
    let mut session = Session::new("ls-fonts".into(), geometry, Hints::default(), settings, sink);
    // The configuration loads before the first pane opens: a program that
    // does nothing.
    session.start(PaneSpec::Command(vec!["true".into()]), None)?;
    let deadline = Instant::now() + LOAD_TIMEOUT;
    while session.is_starting() {
        let left = deadline.saturating_duration_since(Instant::now());
        let ev = rx.recv_timeout(left).context("loading the configuration")?;
        session.handle(ev, Instant::now());
    }
    Ok(session.appearance())
}

/// `NAME[:SIZE]`, as `temzdf-gui ls-fonts --font` takes it.
fn font_arg(f: &FontEntry) -> String {
    match f.size {
        Some(size) => format!("{}:{size}", f.family),
        None => f.family.clone(),
    }
}

/// `$TEMZDF_GUI`, else `temzdf-gui` next to this executable, else on `PATH`.
fn gui_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("TEMZDF_GUI") {
        return p.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("temzdf-gui")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("temzdf-gui"))
}

/// Run `temzdf-gui ls-fonts` for `looks`' fonts, with `rest` (its other
/// options) after.
pub fn run(looks: &Appearance, rest: &[String]) -> Result<()> {
    let gui = gui_binary();
    let mut cmd = std::process::Command::new(&gui);
    cmd.arg("ls-fonts");
    for f in &looks.font {
        cmd.arg("--font").arg(font_arg(f));
    }
    for f in &looks.font_wide {
        cmd.arg("--font-wide").arg(font_arg(f));
    }
    cmd.args(rest);
    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("ls-fonts needs temzdf-gui (next to temzdf or on PATH), which has the fonts built in")
        }
        Err(e) => return Err(e).with_context(|| format!("running {}", gui.display())),
    };
    if !status.success() {
        bail!("temzdf-gui ls-fonts failed ({status})");
    }
    Ok(())
}
