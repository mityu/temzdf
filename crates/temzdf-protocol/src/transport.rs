//! The sockets sessions and forward search talk over: Unix domain sockets
//! on every system, std's on Unix and `uds_windows`'s on Windows (AF_UNIX,
//! Windows 10 1803 and later; the socket is a file, reached through its
//! directory's ACL). Only what both provide is here, so code using these
//! types builds for both; the frontends and the server never name a
//! platform's socket type.
//!
//! Also here: telling a socket (or lock) file from one that replaced it at
//! the same path ([`file_id`]).

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::net as sys;

#[cfg(windows)]
use uds_windows as sys;

/// A connected socket.
#[derive(Debug)]
pub struct Stream(pub(crate) sys::UnixStream);

impl Stream {
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        sys::UnixStream::connect(path).map(Self)
    }

    /// Two sockets connected to each other (tests).
    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = sys::UnixStream::pair()?;
        Ok((Self(a), Self(b)))
    }

    /// Another handle on the same socket (one thread reads, another writes).
    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_write_timeout(timeout)
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.0.shutdown(how)
    }
}

impl Read for &Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.0).read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        (&self.0).read_vectored(bufs)
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        (&*self).read_vectored(bufs)
    }
}

impl Write for &Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.0).write(buf)
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        (&self.0).write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&self.0).flush()
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        (&*self).write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self).flush()
    }
}

/// A listening socket.
#[derive(Debug)]
pub struct Listener(sys::UnixListener);

impl Listener {
    /// Listen at `path` (a file made here; it fails if one is there).
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        sys::UnixListener::bind(path).map(Self)
    }

    pub fn accept(&self) -> io::Result<Stream> {
        self.0.accept().map(|(s, _)| Stream(s))
    }

    /// Connections as they come, without end (as std's `incoming`).
    pub fn incoming(&self) -> impl Iterator<Item = io::Result<Stream>> + '_ {
        std::iter::repeat_with(|| self.accept())
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }
}

/// What tells a file from another one made at the same path later.
pub type FileId = (u64, u64);

/// The identity of the file at `path` (not followed if it is a link), or
/// none when nothing is there. On Unix its device and inode. On Windows
/// only whether it is there (std has no stable file index, and the
/// AF_UNIX socket is a reparse point other crates do not open): a file
/// replaced by another one is taken for the same file, a gap noted in
/// `doc/design/session-server.md`.
pub fn file_id(path: &Path) -> Option<FileId> {
    let m = std::fs::symlink_metadata(path).ok()?;
    Some(id_of(&m))
}

/// [`file_id`] of an open file.
pub fn file_id_of(file: &std::fs::File) -> Option<FileId> {
    file.metadata().ok().map(|m| id_of(&m))
}

#[cfg(unix)]
fn id_of(m: &std::fs::Metadata) -> FileId {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino())
}

#[cfg(not(unix))]
fn id_of(_: &std::fs::Metadata) -> FileId {
    (0, 0)
}
