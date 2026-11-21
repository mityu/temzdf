//! The session's socket, for every client: messages from the server arrive
//! on a reader thread, messages to it are written from any thread; one-shot
//! requests; waiting for a frame to settle; the messages when a
//! connection ends.

use std::io;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use temzdf_protocol::{self as proto, ClientMsg, ExitReason, ServerMsg};
use temzdf_scene::Hints;
use termpix_core::geom::Geometry;

/// How long one-shot requests wait for the session by default.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// What the reader thread passes on.
pub enum FromServer {
    Msg(Box<ServerMsg>),
    /// The server closed the connection.
    Gone,
}

/// A client's connection to a session.
#[derive(Clone)]
pub struct Connection {
    writer: Arc<Mutex<UnixStream>>,
}

impl Connection {
    /// Connect to session `name`. A reader thread passes each message (with
    /// `TEMZDF_TIMING`, how long a frame took to read is logged) and at last
    /// [`FromServer::Gone`] to `tx`, through `wrap` (for a channel that
    /// carries the client's other events too), and calls `wake` after each.
    pub fn open<T: Send + 'static>(
        name: &str,
        tx: Sender<T>,
        wrap: fn(FromServer) -> T,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let stream = connect(name)?;
        let mut reader = proto::shared::FdReader::new(stream.try_clone()?);
        std::thread::Builder::new().name("temzdf-server-in".into()).spawn(move || {
            loop {
                let msg = match proto::recv_server(&mut reader) {
                    Ok(Some((m, took))) => {
                        log_received(&m, took);
                        FromServer::Msg(Box::new(m))
                    }
                    _ => FromServer::Gone,
                };
                let gone = matches!(msg, FromServer::Gone);
                if tx.send(wrap(msg)).is_err() {
                    return;
                }
                wake();
                if gone {
                    return;
                }
            }
        })?;
        Ok(Self { writer: Arc::new(Mutex::new(stream)) })
    }

    pub fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        let mut w = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        proto::send(&mut *w, msg)
    }

    /// Attach: show the session at `geometry` with this frontend's `hints`;
    /// `inside` names the session this client runs in, if any.
    pub fn hello(&self, geometry: Geometry, hints: Hints, inside: Option<String>) -> io::Result<()> {
        self.send(&ClientMsg::Hello {
            version: proto::VERSION,
            geometry,
            hints,
            inside,
            shared_memory: proto::shared_memory_wanted(),
        })
    }
}

/// With `TEMZDF_TIMING`: how long a frame took to read and decode.
fn log_received(msg: &ServerMsg, took: Duration) {
    if proto::timing()
        && let Some((seq, images, bytes)) = proto::frame_size(msg)
    {
        eprintln!(
            "timing: received frame {seq} ({images} images, {bytes} bytes of pixels) in {:.2} ms",
            proto::ms(took)
        );
    }
}

/// Connect to session `name`'s socket.
pub fn connect(name: &str) -> Result<UnixStream> {
    let socket = proto::socket_path(name)?;
    UnixStream::connect(&socket).with_context(|| format!("no session {name:?}"))
}

/// Send a one-shot request to session `name` and wait for the answer.
pub fn request(name: &str, msg: ClientMsg, timeout: Duration) -> Result<ServerMsg> {
    let mut s = connect(name)?;
    s.set_read_timeout(Some(timeout))?;
    proto::send(&mut s, &msg)?;
    match proto::recv(&mut s) {
        Ok(answer) => answer.context("no answer from the session"),
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
            bail!(
                "no answer from session {name:?} within {:.1} s (a script may be running there)",
                timeout.as_secs_f64()
            )
        }
        Err(e) => Err(e.into()),
    }
}

/// What a message did to the frame [`settle`] waits for.
pub enum Settling {
    /// A frame arrived, complete or not.
    Frame { complete: bool },
    /// Newer frames will come before it settles (the client resized).
    Unsettled,
    /// Nothing for the frame.
    Other,
}

/// How long the frame may keep changing after it was first complete (the
/// resize to this geometry, late tiles).
const SETTLE: Duration = Duration::from_millis(500);
/// Longest wait for a complete frame.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Pass session `name`'s messages from `rx` to `on` until a complete frame
/// came and no newer one for a moment (offscreen renders take that one).
/// Fails when the session ends the connection or no complete frame comes.
pub fn settle(name: &str, rx: &Receiver<FromServer>, mut on: impl FnMut(ServerMsg) -> Result<Settling>) -> Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    // When the latest frame was complete: settle from there.
    let mut complete_at: Option<Instant> = None;
    loop {
        let wait = match complete_at {
            Some(t) => (t + SETTLE).saturating_duration_since(Instant::now()),
            None => deadline.saturating_duration_since(Instant::now()),
        };
        let msg = match rx.recv_timeout(wait) {
            Ok(FromServer::Msg(m)) => *m,
            Ok(FromServer::Gone) | Err(RecvTimeoutError::Disconnected) => bail!("{}", lost_message(name)),
            Err(RecvTimeoutError::Timeout) if complete_at.is_some() => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {
                bail!("no complete frame from session {name:?} within {} s", TIMEOUT.as_secs())
            }
        };
        if let ServerMsg::Exit(reason) = &msg {
            bail!("{}", exit_message(reason, name));
        }
        match on(msg)? {
            Settling::Frame { complete } => complete_at = complete.then(Instant::now),
            Settling::Unsettled => complete_at = None,
            Settling::Other => {}
        }
    }
}

/// What to tell the user when session `name` ends the connection.
pub fn exit_message(reason: &ExitReason, name: &str) -> String {
    match reason {
        ExitReason::Detached => format!("[detached from session {name}]"),
        ExitReason::SessionEnded => format!("[session {name} ended]"),
        ExitReason::Killed => format!("[session {name} was killed]"),
        ExitReason::Error(e) => format!("[session {name}: {e}]"),
    }
}

/// What to tell the user when the connection to session `name` broke
/// without a reason.
pub fn lost_message(name: &str) -> String {
    format!("[lost session {name}]")
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn settle_after(msgs: Vec<FromServer>) -> String {
        let (tx, rx) = mpsc::channel();
        for m in msgs {
            tx.send(m).unwrap();
        }
        drop(tx);
        let err = settle("alfa", &rx, |_| Ok(Settling::Other)).unwrap_err();
        format!("{err:#}")
    }

    #[test]
    fn settle_says_why_the_session_went() {
        let exit = FromServer::Msg(Box::new(ServerMsg::Exit(ExitReason::Killed)));
        assert_eq!(settle_after(vec![exit]), "[session alfa was killed]");
        assert_eq!(settle_after(vec![FromServer::Gone]), "[lost session alfa]");
        // The reader thread went without a word.
        assert_eq!(settle_after(vec![]), "[lost session alfa]");
    }
}
