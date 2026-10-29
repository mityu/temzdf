//! Pixels in memory shared with the client instead of inside the message:
//! one anonymous object per image (a sealed memfd on Linux, an unlinked POSIX
//! shared memory object elsewhere), its descriptor passed with the message
//! (`SCM_RIGHTS`). Nothing is named, so nothing is left behind when either
//! side ends: the memory goes with the last descriptor or mapping.
//!
//! The server copies the pixels in once; the client maps them read-only and
//! hands the mapping on (to the GPU, or to the terminal's image store) as
//! [`Pixels`], without copying.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer, SendAncillaryMessage,
    SendFlags,
};
use temzdf_scene::Pixels;

/// Smaller images travel inside the message: an object costs a few system
/// calls on each side, and macOS rounds it up to 16 KiB pages.
pub const MIN_SHARED: usize = 64 << 10;
/// At most this many descriptors go with one message (Linux takes 253 per
/// `sendmsg`); further images travel inside it.
pub const MAX_FDS: usize = 250;

/// An object of `pixels.len()` bytes holding `pixels`, which cannot change
/// afterwards where the system can promise it (Linux seals).
pub fn create(pixels: &[u8]) -> io::Result<OwnedFd> {
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

/// The first `len` bytes of object `fd`, mapped read-only.
pub fn map(fd: OwnedFd, len: usize) -> io::Result<Pixels> {
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

/// Write `bytes` to `stream`, the descriptors `fds` with its first part.
pub fn send_with_fds(stream: &UnixStream, bytes: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    let mut written = 0;
    if !fds.is_empty() {
        let mut space = vec![MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        if !control.push(SendAncillaryMessage::ScmRights(fds)) {
            return Err(io::Error::other(format!("{} descriptors in one message", fds.len())));
        }
        written = loop {
            match rustix::net::sendmsg(stream, &[IoSlice::new(bytes)], &mut control, SendFlags::empty()) {
                Ok(n) => break n,
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        };
    }
    let mut stream = stream;
    stream.write_all(&bytes[written..])?;
    stream.flush()
}

/// A socket's reading side that keeps the descriptors arriving with the
/// bytes, in order, for [`crate::recv_server`].
pub struct FdReader {
    stream: UnixStream,
    fds: VecDeque<OwnedFd>,
}

impl FdReader {
    pub fn new(stream: UnixStream) -> Self {
        Self { stream, fds: VecDeque::new() }
    }

    /// The next descriptor received.
    pub fn take_fd(&mut self) -> Option<OwnedFd> {
        self.fds.pop_front()
    }
}

impl Read for FdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS + 3))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        #[cfg(target_os = "linux")]
        let flags = RecvFlags::CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = RecvFlags::empty();
        let got = rustix::net::recvmsg(&self.stream, &mut [IoSliceMut::new(buf)], &mut control, flags)?;
        for msg in control.drain() {
            if let RecvAncillaryMessage::ScmRights(fds) = msg {
                for fd in fds {
                    #[cfg(not(target_os = "linux"))]
                    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
                    self.fds.push_back(fd);
                }
            }
        }
        if got.flags.contains(ReturnFlags::CTRUNC) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "descriptors were dropped"));
        }
        Ok(got.bytes)
    }
}

impl AsFd for FdReader {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}
