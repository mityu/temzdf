//! Finding the sessions: the sockets in [`crate::sessions_dir`] and what
//! their servers say about themselves (clients and the server share this).

use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::{ClientMsg, ServerMsg, SessionInfo, VERSION};

/// Is a session with this socket alive? Stale sockets are removed.
pub fn probe(socket: &Path) -> Option<SessionInfo> {
    match probe_detail(socket) {
        Probe::Info(info) => Some(info),
        _ => None,
    }
}

/// What a session's socket said to [`probe_detail`].
#[derive(Debug)]
pub enum Probe {
    Info(SessionInfo),
    /// It answered in a form this version does not understand: a server of
    /// another temzdf version.
    Other,
    /// It took the connection but did not answer in time: busy (its main
    /// loop held up), not necessarily another version.
    NoAnswer,
    /// Nothing listens (a stale socket is removed).
    Gone,
}

/// [`probe`], telling a server that did not answer in time from one that
/// answered in another form.
pub fn probe_detail(socket: &Path) -> Probe {
    probe_within(socket, Duration::from_secs(2))
}

fn probe_within(socket: &Path, timeout: Duration) -> Probe {
    let answer = (|| {
        let Ok(mut s) = UnixStream::connect(socket) else { return Probe::Gone };
        if s.set_read_timeout(Some(timeout)).is_err()
            || crate::send(&mut s, &ClientMsg::Info { version: VERSION }).is_err()
        {
            return Probe::Other;
        }
        match crate::recv::<ServerMsg>(&mut s) {
            Ok(Some(ServerMsg::Info(info))) => Probe::Info(info),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Probe::NoAnswer,
            _ => Probe::Other,
        }
    })();
    // A stale socket goes, unless its server holds the session's lock: one
    // that is starting (bound, not yet answering) keeps it.
    if !matches!(answer, Probe::Info(_)) && UnixStream::connect(socket).is_err() && !locked(socket) {
        let _ = std::fs::remove_file(socket);
        return Probe::Gone;
    }
    answer
}

/// Whether a server holds the lock of session `name` (it may be starting:
/// locked, not listening yet).
pub fn name_locked(name: &str) -> bool {
    crate::socket_path(name).is_ok_and(|s| locked(&s))
}

/// Whether a server holds the lock of the session whose socket is `socket`
/// (`NAME.lock` next to it).
fn locked(socket: &std::path::Path) -> bool {
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(socket.with_extension("lock")) else { return false };
    matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// Every live session, by name.
pub fn list() -> io::Result<Vec<SessionInfo>> {
    Ok(list_all()?.sessions)
}

/// The live sessions, as [`list_all`] finds them.
#[derive(Debug, Default)]
pub struct Listing {
    /// Sessions of this version, by name.
    pub sessions: Vec<SessionInfo>,
    /// Names of live sessions that answered in a form this version does not
    /// understand (servers of another temzdf version).
    pub other: Vec<String>,
    /// Names of live sessions that did not answer in time (busy).
    pub silent: Vec<String>,
}

impl Listing {
    /// Every name in use.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.sessions.iter().map(|i| i.name.as_str()).chain(self.other.iter().chain(&self.silent).map(String::as_str))
    }
}

/// Every live session: those that answered, those of another version, and
/// those that did not answer in time.
pub fn list_all() -> io::Result<Listing> {
    let dir = crate::sessions_dir()?;
    let mut out = Listing::default();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "sock") {
            continue;
        }
        let name = || path.file_stem().map(|n| n.to_string_lossy().into_owned());
        match probe_detail(&path) {
            Probe::Info(info) => out.sessions.push(info),
            Probe::Other => out.other.extend(name()),
            Probe::NoAnswer => out.silent.extend(name()),
            Probe::Gone => {}
        }
    }
    out.sessions.sort_by(|a, b| a.name.cmp(&b.name));
    out.other.sort();
    out.silent.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore = "Miri has no Unix sockets")]
    fn a_server_too_busy_to_answer_is_not_another_version() {
        let dir = std::env::temp_dir().join(format!("temzdf-probe-busy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Takes connections (the kernel's backlog) and never answers.
        let busy = dir.join("busy.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&busy).unwrap();
        assert!(matches!(probe_within(&busy, Duration::from_millis(100)), Probe::NoAnswer));
        assert!(busy.exists());
        // Answers with something else: another version.
        let other = dir.join("other.sock");
        let listener = std::os::unix::net::UnixListener::bind(&other).unwrap();
        // (Kept listening: a socket nothing listens on is stale.)
        let _kept = listener.try_clone().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut len = [0u8; 4];
            std::io::Read::read_exact(&mut s, &mut len).unwrap();
            std::io::Write::write_all(&mut s, &[3, 0, 0, 0, 0xff, 0xff, 0xff]).unwrap();
        });
        assert!(matches!(probe_within(&other, Duration::from_secs(5)), Probe::Other));
        let _ = std::fs::remove_dir_all(&dir);
    }

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
