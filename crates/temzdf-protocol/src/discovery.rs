//! Finding the sessions: the sockets in [`crate::sessions_dir`] and what
//! their servers say about themselves (clients and the server share this).

use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::{ClientMsg, ServerMsg, SessionInfo, VERSION};

/// Is a session with this socket alive? Stale sockets are removed.
pub fn probe(socket: &Path) -> Option<SessionInfo> {
    let info = (|| {
        let mut s = UnixStream::connect(socket).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        crate::send(&mut s, &ClientMsg::Info { version: VERSION }).ok()?;
        match crate::recv::<ServerMsg>(&mut s).ok()?? {
            ServerMsg::Info(info) => Some(info),
            _ => None,
        }
    })();
    // A stale socket goes, unless its server holds the session's lock: one
    // that is starting (bound, not yet answering) keeps it.
    if info.is_none() && UnixStream::connect(socket).is_err() && !locked(socket) {
        let _ = std::fs::remove_file(socket);
    }
    info
}

/// Whether a server holds the lock of the session whose socket is `socket`
/// (`NAME.lock` next to it).
fn locked(socket: &std::path::Path) -> bool {
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(socket.with_extension("lock")) else { return false };
    matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// Every live session, by name.
pub fn list() -> io::Result<Vec<SessionInfo>> {
    Ok(list_all()?.0)
}

/// Every live session, and the names of live sessions that did not answer in
/// a form this version understands (servers of another temzdf version).
pub fn list_all() -> io::Result<(Vec<SessionInfo>, Vec<String>)> {
    let dir = crate::sessions_dir()?;
    let mut out = Vec::new();
    let mut other = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "sock") {
            continue;
        }
        match probe(&path) {
            Some(info) => out.push(info),
            // probe removed it if nothing listens.
            None if path.exists() => {
                other.extend(path.file_stem().map(|n| n.to_string_lossy().into_owned()));
            }
            None => {}
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    other.sort();
    Ok((out, other))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore = "Miri has no Unix sockets")]
    fn probing_keeps_the_socket_of_a_session_whose_lock_is_held() {
        let dir = std::env::temp_dir().join(format!("temzdf-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        // A socket nothing answers on (its listener gone).
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        let lock = std::fs::File::create(socket.with_extension("lock")).unwrap();
        lock.lock().unwrap();
        assert!(probe(&socket).is_none());
        assert!(socket.exists(), "its server holds the lock: starting, not stale");
        drop(lock);
        assert!(probe(&socket).is_none());
        assert!(!socket.exists(), "stale");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
