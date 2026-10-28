//! The session's socket: messages from the server arrive on a reader thread
//! (and wake the event loop through `wake`), messages to it are written
//! from any thread.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use temzdf_protocol::{self as proto, ClientMsg, ServerMsg};

/// What the reader thread passes on.
pub enum FromServer {
    Msg(ServerMsg),
    /// The server closed the connection.
    Gone,
}

#[derive(Clone)]
pub struct Connection {
    writer: Arc<Mutex<UnixStream>>,
}

impl Connection {
    /// Connect to session `name`; `wake` is called after each message is
    /// queued on the returned receiver.
    pub fn open(name: &str, wake: impl Fn() + Send + 'static) -> Result<(Self, Receiver<FromServer>)> {
        let socket = proto::socket_path(name)?;
        let stream = UnixStream::connect(&socket).with_context(|| format!("no session {name:?}"))?;
        let mut reader = stream.try_clone()?;
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new().name("temzdf-gui-server".into()).spawn(move || {
            loop {
                let msg = match proto::recv::<ServerMsg>(&mut reader) {
                    Ok(Some(m)) => FromServer::Msg(m),
                    _ => FromServer::Gone,
                };
                let gone = matches!(msg, FromServer::Gone);
                if tx.send(msg).is_err() {
                    return;
                }
                wake();
                if gone {
                    return;
                }
            }
        })?;
        Ok((Self { writer: Arc::new(Mutex::new(stream)) }, rx))
    }

    pub fn send(&self, msg: &ClientMsg) -> std::io::Result<()> {
        let mut w = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        proto::send(&mut *w, msg)
    }
}

/// End session `name`.
pub fn kill(name: &str) -> Result<()> {
    let mut s = UnixStream::connect(proto::socket_path(name)?)?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    proto::send(&mut s, &ClientMsg::Kill)?;
    let _ = proto::recv::<ServerMsg>(&mut s)?;
    Ok(())
}

/// The `temzdf` executable that runs session servers: next to this one,
/// else on `PATH`.
pub fn temzdf_exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("temzdf")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("temzdf"))
}

/// The session to show: `name` (started with `file` if it does not
/// exist), else a new one for `file`, else the only one, else a new shell.
/// Returns its name and whether it was started here; one started here runs
/// no configuration script if `clean`.
pub fn session_for(name: Option<&str>, file: Option<&Path>, clean: bool) -> Result<(String, bool)> {
    let exists = |n: &str| proto::discovery::list().is_ok_and(|l| l.iter().any(|s| s.name == n));
    // A name of its own (`-s`) taken meanwhile is an error; one picked here
    // is picked again.
    let start = |choose: &dyn Fn() -> Result<String>, retry: bool| -> Result<(String, bool)> {
        let pane = temzdf_client::first_pane(file)?;
        let spawn = |n: &str| temzdf_client::spawn_server(&temzdf_exe(), n, &pane, None, None, clean);
        Ok((temzdf_client::spawn_named(choose, retry, spawn)?, true))
    };
    match (name, file) {
        (Some(n), _) if exists(n) => Ok((n.to_owned(), false)),
        (Some(n), _) => start(&|| Ok(n.to_owned()), false),
        // Named as `temzdf [FILE]` names sessions: alfa, bravo, ...
        (None, Some(_)) => start(&temzdf_client::auto_name, true),
        (None, None) => match proto::discovery::list()?.as_slice() {
            [one] => Ok((one.name.clone(), false)),
            [] => start(&temzdf_client::auto_name, true),
            many => bail!(
                "several sessions, name one with -s: {}",
                many.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        },
    }
}
