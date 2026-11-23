//! How the host talks to the session: [`Link`]. In the server it is the
//! WebAssembly guest's imports (crates/temzdf-script-guest); [`StreamLink`]
//! carries the same messages over a byte stream (the tests, and anything that
//! runs the host natively).

use std::io::{self, Read, Write};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use temzdf_protocol::script::{FromHost, ToHost};
use temzdf_protocol::script_fs::{FsOp, FsResult, ToGuest};

use crate::native_fs::Workers;

/// Synchronous file access ([`Link::files`]).
pub type Files = Rc<dyn Fn(&FsOp) -> FsResult>;

/// What [`Link::recv`] got.
#[derive(Debug)]
pub enum Received {
    Msg(ToHost),
    /// An operation [`Link::fs_start`] started is done.
    FsDone {
        id: u64,
        result: FsResult,
    },
    /// Nothing came in time.
    Timeout,
    /// The session went away.
    Closed,
}

/// The host's side of the conversation with the session.
pub trait Link {
    /// Send a message; an error means the session went away.
    fn send(&mut self, msg: &FromHost) -> io::Result<()>;
    /// The next message, waiting at most `timeout` (`None`: as long as it
    /// takes).
    fn recv(&mut self, timeout: Option<Duration>) -> Received;
    /// How many [`ToHost::Interrupt`]s the session has sent so far, counted
    /// as they are sent, before their messages are read: QuickJS's interrupt
    /// handler calls it while JavaScript runs, often, so it must be cheap.
    fn interrupts(&self) -> Box<dyn Fn() -> u64>;
    /// File access done where the OS's paths are understood
    /// ([`crate::native_fs`]), waited for: the module loader's.
    fn files(&self) -> Files;
    /// Start a file operation without waiting for it: [`Link::recv`]
    /// gives its result ([`Received::FsDone`]) once done, among the
    /// session's messages.
    fn fs_start(&mut self, id: u64, op: FsOp);
}

/// A [`Link`] over a byte stream with `temzdf_protocol`'s framing: a thread
/// reads the messages (and counts interrupts as they come).
pub struct StreamLink {
    out: Box<dyn Write>,
    rx: Receiver<ToGuest>,
    interrupts: Arc<AtomicU64>,
    workers: Workers,
}

impl StreamLink {
    pub fn new(input: impl Read + Send + 'static, output: impl Write + 'static) -> io::Result<Self> {
        let interrupts = Arc::new(AtomicU64::new(0));
        let (tx, rx) = mpsc::channel();
        let done = tx.clone();
        let workers = Workers::new(move |id, result| {
            let _ = done.send(ToGuest::FsDone { id, result });
        });
        let count = interrupts.clone();
        let mut input = io::BufReader::new(input);
        std::thread::Builder::new().name("script-host-in".into()).spawn(move || {
            loop {
                let msg = match temzdf_protocol::recv::<ToHost>(&mut input) {
                    Ok(Some(m)) => m,
                    _ => ToHost::Shutdown,
                };
                if msg == ToHost::Interrupt {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                let last = msg == ToHost::Shutdown;
                if tx.send(ToGuest::Msg(msg)).is_err() || last {
                    return;
                }
            }
        })?;
        Ok(Self { out: Box::new(output), rx, interrupts, workers })
    }
}

impl Link for StreamLink {
    fn send(&mut self, msg: &FromHost) -> io::Result<()> {
        temzdf_protocol::send(&mut self.out, msg)
    }

    fn recv(&mut self, timeout: Option<Duration>) -> Received {
        let got = match timeout {
            Some(t) => self.rx.recv_timeout(t),
            None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match got {
            Ok(ToGuest::Msg(m)) => Received::Msg(m),
            Ok(ToGuest::FsDone { id, result }) => Received::FsDone { id, result },
            Err(RecvTimeoutError::Timeout) => Received::Timeout,
            Err(RecvTimeoutError::Disconnected) => Received::Closed,
        }
    }

    fn interrupts(&self) -> Box<dyn Fn() -> u64> {
        let n = self.interrupts.clone();
        Box::new(move || n.load(Ordering::SeqCst))
    }

    fn files(&self) -> Files {
        Rc::new(crate::native_fs::run)
    }

    fn fs_start(&mut self, id: u64, op: FsOp) {
        self.workers.start(id, op);
    }
}
