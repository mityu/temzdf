//! The terminal on Windows: not yet. [`Tty::open`] and
//! [`StdioRedirect::to_file`] fail with [`io::ErrorKind::Unsupported`], so
//! none of the rest can be reached (the types have no values); the console
//! implementation replaces this.

use std::convert::Infallible;
use std::io::{self, Read, Write};
use std::sync::mpsc::Sender;
use std::time::Duration;

use super::{Modes, TermEvent, Winsize};

fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "no terminal support on Windows yet")
}

/// The controlling terminal (not on Windows yet).
pub struct Tty {
    never: Infallible,
}

impl Tty {
    /// Fails: the Windows console is not supported yet.
    pub fn open(_modes: Modes) -> io::Result<Self> {
        Err(unsupported())
    }

    pub fn without_graphics(&mut self) {
        match self.never {}
    }

    pub fn set_tmux_passthrough(&mut self) {
        match self.never {}
    }

    pub fn enable_pixel_mouse(&mut self) -> io::Result<()> {
        match self.never {}
    }

    pub fn write_all(&mut self, _bytes: &[u8]) -> io::Result<()> {
        match self.never {}
    }

    pub fn pending_output(&self) -> usize {
        match self.never {}
    }

    pub fn drain(&self, _timeout: Duration) -> bool {
        match self.never {}
    }

    pub fn read_timeout(&self, _buf: &mut [u8], _timeout: Option<Duration>) -> io::Result<Option<usize>> {
        match self.never {}
    }

    pub fn winsize(&self) -> io::Result<Winsize> {
        match self.never {}
    }

    pub fn reader(&self) -> io::Result<TtyReader> {
        match self.never {}
    }
}

/// Restores the terminal on Unix; here only for the same API.
impl Drop for Tty {
    fn drop(&mut self) {
        match self.never {}
    }
}

impl Write for Tty {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        match self.never {}
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.never {}
    }
}

/// Read side of the terminal (not on Windows yet).
pub struct TtyReader {
    never: Infallible,
}

impl Read for TtyReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        match self.never {}
    }
}

/// Spawn the input threads (not on Windows yet: there is no reader).
pub fn spawn_input<T: Send + 'static>(
    reader: TtyReader,
    _pixel_mouse: bool,
    _tx: Sender<T>,
    _map: impl Fn(TermEvent) -> T + Send + Sync + Clone + 'static,
) -> io::Result<()> {
    match reader.never {}
}

/// Redirects stdout and stderr to a file (not on Windows yet).
pub struct StdioRedirect {
    _never: Infallible,
}

impl StdioRedirect {
    /// Fails: not on Windows yet.
    pub fn to_file(_log: &std::path::Path) -> io::Result<Self> {
        Err(unsupported())
    }
}
