//! Pixels in memory shared with the client instead of inside the message:
//! one anonymous object per image (a sealed memfd on Linux, an unlinked POSIX
//! shared memory object elsewhere), its descriptor passed with the message
//! (`SCM_RIGHTS`). Nothing is named, so nothing is left behind when either
//! side ends: the memory goes with the last descriptor or mapping.
//!
//! The server copies the pixels in once; the client maps them read-only and
//! hands the mapping on (to the GPU, or to the terminal's image store) as
//! [`Pixels`], without copying.
//!
//! Unix only so far: on Windows no object is ever made ([`create`] fails,
//! [`crate::shared_memory_wanted`] is false), so every image travels in the
//! message; an unnamed file mapping handed over with `DuplicateHandle` would
//! do there what the descriptor does here.

#[cfg(unix)]
use std::collections::VecDeque;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::IoSliceMut;
use std::io::{self, IoSlice, Read, Write};
#[cfg(unix)]
use std::mem::MaybeUninit;
#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

#[cfg(unix)]
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer, SendAncillaryMessage,
    SendFlags,
};
use temzdf_scene::Pixels;

use crate::Stream;

/// A shared memory object, passed with a message: a descriptor on Unix;
/// none can exist elsewhere.
#[cfg(unix)]
pub type Object = OwnedFd;
/// A shared memory object, passed with a message: a descriptor on Unix;
/// none can exist elsewhere.
#[cfg(not(unix))]
#[derive(Debug)]
pub enum Object {}

/// Smaller images travel inside the message: an object costs a few system
/// calls on each side, and macOS rounds it up to 16 KiB pages.
pub const MIN_SHARED: usize = 64 << 10;
/// At most this many descriptors go with one message (Linux takes 253 per
/// `sendmsg`); further images travel inside it.
pub const MAX_FDS: usize = 250;

/// An object of `pixels.len()` bytes holding `pixels`, which cannot change
/// afterwards where the system can promise it (Linux seals).
#[cfg(unix)]
pub fn create(pixels: &[u8]) -> io::Result<Object> {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{MemfdFlags, SealFlags};
        let fd = rustix::fs::memfd_create("temzdf-pixels", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
        // fallocate: a full tmpfs fails here (ENOSPC), not on a later access.
        rustix::fs::fallocate(&fd, rustix::fs::FallocateFlags::empty(), 0, pixels.len() as u64)?;
        let mut file = File::from(fd);
        file.write_all(pixels)?;
        // Written through write(2), so no writable mapping stands in the way
        // of the write seal.
        rustix::fs::fcntl_add_seals(&file, SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL)?;
        Ok(file.into())
    }
    #[cfg(not(target_os = "linux"))]
    {
        use std::sync::atomic::{AtomicU64, Ordering};

        use rustix::fs::Mode;
        use rustix::shm;
        static UNIQUE: AtomicU64 = AtomicU64::new(0);
        let name = format!("/temzdf-{}-{}", std::process::id(), UNIQUE.fetch_add(1, Ordering::Relaxed));
        let fd = shm::open(
            name.as_str(),
            shm::OFlags::CREATE | shm::OFlags::EXCL | shm::OFlags::RDWR,
            Mode::RUSR | Mode::WUSR,
        )?;
        // Anonymous from here on.
        let _ = shm::unlink(name.as_str());
        rustix::fs::ftruncate(&fd, pixels.len() as u64)?;
        rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
        let file = File::from(fd);
        // SAFETY: the object was just created exclusively and unlinked: no
        // other process can open it, and only this mapping writes to it.
        let mut map = unsafe { memmap2::MmapMut::map_mut(&file)? };
        map[..pixels.len()].copy_from_slice(pixels);
        Ok(file.into())
    }
}

/// [`create`] where there is no shared memory: always an error, so the
/// pixels go in the message.
#[cfg(not(unix))]
pub fn create(_: &[u8]) -> io::Result<Object> {
    Err(io::ErrorKind::Unsupported.into())
}

/// The first `len` bytes of object `fd`, mapped read-only.
#[cfg(not(unix))]
pub fn map(fd: Object, _: usize) -> io::Result<Pixels> {
    match fd {}
}

/// The first `len` bytes of object `fd`, mapped read-only.
#[cfg(unix)]
pub fn map(fd: Object, len: usize) -> io::Result<Pixels> {
    let size = rustix::fs::fstat(&fd)?.st_size;
    if u64::try_from(size).map_or(true, |size| size < len as u64) {
        // Mapped anyway, reading past the end would be SIGBUS.
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("shared pixels: {size} bytes, not {len}")));
    }
    let file = File::from(fd);
    // SAFETY: the object cannot shrink or change while mapped: on Linux the
    // server sealed it (shrinking, growing and writing) before sending it;
    // elsewhere the server, the same user behind a socket in a 0700
    // directory, unlinked it at once and never writes to it after sending.
    let map = unsafe { memmap2::MmapOptions::new().len(len).populate().map(&file)? };
    Ok(Pixels::from_owner(map))
}

/// Writes take at most this many parts at a time (IOV_MAX on Linux and
/// macOS).
const MAX_PARTS: usize = 1024;

/// Write `parts` to `stream` in order, the objects `fds` with the first
/// bytes. `parts` is used up (advanced past what was written).
#[cfg(unix)]
pub fn send_with_fds(stream: &Stream, mut parts: &mut [IoSlice<'_>], fds: &[Object]) -> io::Result<()> {
    let fds: Vec<BorrowedFd<'_>> = fds.iter().map(|fd| fd.as_fd()).collect();
    let fds = fds.as_slice();
    let mut space =
        vec![MaybeUninit::uninit(); if fds.is_empty() { 0 } else { rustix::cmsg_space!(ScmRights(MAX_FDS)) }];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !fds.is_empty() && !control.push(SendAncillaryMessage::ScmRights(fds)) {
        return Err(io::Error::other(format!("{} descriptors in one message", fds.len())));
    }
    let mut first = !fds.is_empty();
    while !parts.is_empty() {
        let some = &parts[..parts.len().min(MAX_PARTS)];
        let n = if first {
            rustix::net::sendmsg(&stream.0, some, &mut control, SendFlags::empty()).map_err(io::Error::from)
        } else {
            (&mut &*stream).write_vectored(some)
        };
        match n {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                first = false;
                IoSlice::advance_slices(&mut parts, n);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// [`send_with_fds`] where no objects exist: `parts` written in order.
#[cfg(not(unix))]
pub fn send_with_fds(stream: &Stream, mut parts: &mut [IoSlice<'_>], fds: &[Object]) -> io::Result<()> {
    if let Some(fd) = fds.first() {
        match *fd {}
    }
    while !parts.is_empty() {
        let some = &parts[..parts.len().min(MAX_PARTS)];
        match (&mut &*stream).write_vectored(some) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => IoSlice::advance_slices(&mut parts, n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// A socket's reading side that keeps the descriptors arriving with the
/// bytes, in order, for [`crate::recv_server`] (on Unix; elsewhere it only
/// reads).
pub struct FdReader {
    stream: Stream,
    #[cfg(unix)]
    fds: VecDeque<Object>,
    #[cfg(unix)]
    received: usize,
}

impl FdReader {
    pub fn new(stream: Stream) -> Self {
        Self {
            stream,
            #[cfg(unix)]
            fds: VecDeque::new(),
            #[cfg(unix)]
            received: 0,
        }
    }

    /// How many descriptors came so far (images in shared memory).
    pub fn fds_received(&self) -> usize {
        #[cfg(unix)]
        {
            self.received
        }
        #[cfg(not(unix))]
        {
            0
        }
    }

    /// The next descriptor received.
    pub fn take_fd(&mut self) -> Option<Object> {
        #[cfg(unix)]
        {
            self.fds.pop_front()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

#[cfg(not(unix))]
impl Read for FdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

#[cfg(unix)]
impl Read for FdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS + 3))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        #[cfg(target_os = "linux")]
        let flags = RecvFlags::CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = RecvFlags::empty();
        let got = rustix::net::recvmsg(&self.stream.0, &mut [IoSliceMut::new(buf)], &mut control, flags)?;
        for msg in control.drain() {
            if let RecvAncillaryMessage::ScmRights(fds) = msg {
                for fd in fds {
                    #[cfg(not(target_os = "linux"))]
                    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
                    self.fds.push_back(fd);
                    self.received += 1;
                }
            }
        }
        if got.flags.contains(ReturnFlags::CTRUNC) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "descriptors were dropped"));
        }
        Ok(got.bytes)
    }
}

#[cfg(unix)]
impl AsFd for FdReader {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.0.as_fd()
    }
}
