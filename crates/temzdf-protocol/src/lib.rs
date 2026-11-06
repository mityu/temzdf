//! The session protocol: messages between a session server and its clients
//! (and the `temzdf` commands that query or control it), their framing on a
//! Unix socket, and where sessions' sockets live.
//!
//! One server process per session listens on
//! `<runtime dir>/temzdf/sessions/<name>.sock` (mode 0700 directory). Each
//! message is a little-endian `u32` length followed by the message encoded with
//! `postcard`; a frame's large images follow it, their pixels as they are
//! (`trailing`), or come in shared memory (`shared`, [`send_server`]). A
//! connection starts with [`ClientMsg::Hello`] (a client that will
//! show the session) or a single request ([`ClientMsg::Info`],
//! [`ClientMsg::Kill`], [`ClientMsg::Action`]) answered once.

pub mod script;

// Everything but the script host's messages and the framing: the guest
// (crates/temzdf-script-guest, WebAssembly) builds without it.
#[cfg(feature = "session")]
pub mod discovery;
#[cfg(feature = "session")]
mod session;
#[cfg(feature = "session")]
pub mod shared;
#[cfg(feature = "session")]
pub use session::*;

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Larger messages are refused (a frame with every image of a full cache is
/// about 200 MB).
pub(crate) const MAX_MESSAGE: usize = 512 << 20;

/// Write one message.
pub fn send<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let body = postcard::to_stdvec(msg).map_err(io::Error::other)?;
    let len = u32::try_from(body.len()).map_err(|_| io::Error::other("message too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// Read one message; `Ok(None)` at a clean end of the stream. Messages
/// from the server go through [`recv_server`] instead.
pub fn recv<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> io::Result<Option<T>> {
    Ok(recv_timed(r)?.map(|(msg, _)| msg))
}

/// [`recv`], with how long the message took once its first bytes came
/// (reading and decoding, not waiting for it).
pub fn recv_timed<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> io::Result<Option<(T, Duration)>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let start = Instant::now();
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("message of {len} bytes")));
    }
    // Grown as bytes come, not allocated from the length alone (a bogus
    // length would take hundreds of megabytes for nothing).
    let mut body = Vec::new();
    r.take(len as u64).read_to_end(&mut body)?;
    if body.len() < len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let msg = postcard::from_bytes(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some((msg, start.elapsed())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bogus_length_does_not_allocate_it() {
        // 500 MB announced, 3 bytes sent: an error, without the allocation.
        let mut bytes = (500u32 << 20).to_le_bytes().to_vec();
        bytes.extend_from_slice(b"abc");
        let e = recv::<String>(&mut bytes.as_slice()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }
}
