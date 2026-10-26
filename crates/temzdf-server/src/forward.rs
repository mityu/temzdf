//! Talking to a running viewer: `temzdf --forward LINE:FILE doc.pdf` sends a
//! forward-search request to the viewer that has `doc.pdf` open.
//!
//! Each viewer listens on a Unix socket at
//! `$XDG_RUNTIME_DIR/temzdf/<hash of the canonical PDF path>.sock` (falling back
//! to the per-user temp dir on macOS, else the system temp dir). The protocol
//! is one line per request:
//! `forward\t<line>\t<column>\t<source file>\n`, answered with `ok\n`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A forward-search request: show what `line` of `file` produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardRequest {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
}

impl ForwardRequest {
    /// Parse `LINE[:COLUMN]:FILE` (the file may itself contain colons).
    pub fn parse_spec(spec: &str) -> Option<Self> {
        let (line, rest) = spec.split_once(':')?;
        let line = line.parse().ok()?;
        let (column, file) = match rest.split_once(':') {
            Some((c, f)) if c.parse::<u32>().is_ok() && !f.is_empty() => (c.parse().ok()?, f),
            _ => (0, rest),
        };
        (!file.is_empty()).then(|| Self { file: PathBuf::from(file), line, column })
    }

    fn encode(&self) -> String {
        format!("forward\t{}\t{}\t{}\n", self.line, self.column, self.file.display())
    }

    fn decode(line: &str) -> Option<Self> {
        let mut it = line.trim_end_matches('\n').splitn(4, '\t');
        if it.next()? != "forward" {
            return None;
        }
        let line = it.next()?.parse().ok()?;
        let column = it.next()?.parse().ok()?;
        let file = PathBuf::from(it.next()?);
        Some(Self { file, line, column })
    }
}

/// FNV-1a: a stable hash, so every build of temzdf agrees on socket names.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// The socket a viewer of `pdf` listens on.
pub fn socket_path(pdf: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(pdf).unwrap_or_else(|_| std::path::absolute(pdf).unwrap_or(pdf.into()));
    let hash = fnv1a(canonical.as_os_str().as_encoded_bytes());
    temzdf_protocol::runtime_dir().join("temzdf").join(format!("{hash:016x}.sock"))
}

/// Send a request to the viewer of `pdf`. `Ok(false)` when no viewer is running.
pub fn send_forward(pdf: &Path, req: &ForwardRequest) -> std::io::Result<bool> {
    send_to(&socket_path(pdf), req)
}

fn send_to(socket: &Path, req: &ForwardRequest) -> std::io::Result<bool> {
    let Ok(mut stream) = UnixStream::connect(socket) else { return Ok(false) };
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(req.encode().as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(reply.trim() == "ok")
}

/// Listening socket; dropped, its thread ends and the socket is removed.
pub struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        // The thread waits in accept: wake it to see it must end.
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// How long one connection may take to send its request (others wait).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Listen for requests about `pdf`, handing them to `deliver` (which returns
/// false once nobody listens any more). Another live viewer of the same file
/// keeps its socket (an error here); a stale socket left by a crashed viewer is
/// replaced.
pub fn serve(pdf: &Path, deliver: impl Fn(ForwardRequest) -> bool + Send + 'static) -> std::io::Result<Server> {
    serve_at(socket_path(pdf), deliver)
}

fn serve_at(path: PathBuf, deliver: impl Fn(ForwardRequest) -> bool + Send + 'static) -> std::io::Result<Server> {
    if let Some(dir) = path.parent() {
        temzdf_protocol::private_dir(dir)?;
    }
    if UnixStream::connect(&path).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "another viewer has this file open"));
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    std::thread::Builder::new().name("temzdf-ipc".into()).spawn(move || {
        for stream in listener.incoming() {
            if stopped.load(Ordering::SeqCst) {
                return;
            }
            let Ok(mut stream) = stream else { continue };
            // A client that sends nothing must not hold the others up.
            let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
            let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));
            let mut line = String::new();
            let ok = BufReader::new(&stream).read_line(&mut line).is_ok();
            let reply = match ForwardRequest::decode(&line).filter(|_| ok) {
                Some(req) => {
                    if !deliver(req) {
                        return;
                    }
                    "ok\n"
                }
                None => "error\n",
            };
            let _ = stream.write_all(reply.as_bytes());
        }
    })?;
    Ok(Server { path, stop })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_specs() {
        let r = ForwardRequest::parse_spec("12:main.tex").unwrap();
        assert_eq!((r.line, r.column, r.file.to_str().unwrap()), (12, 0, "main.tex"));
        let r = ForwardRequest::parse_spec("12:5:/a/b:c.tex").unwrap();
        assert_eq!((r.line, r.column, r.file.to_str().unwrap()), (12, 5, "/a/b:c.tex"));
        assert!(ForwardRequest::parse_spec("x:main.tex").is_none());
        assert!(ForwardRequest::parse_spec("3:").is_none());
        let req = ForwardRequest { file: "a b.tex".into(), line: 3, column: 1 };
        assert_eq!(ForwardRequest::decode(&req.encode()), Some(req));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri has no Unix sockets")]
    fn round_trip_through_the_socket() {
        let dir = std::env::temp_dir().join(format!("temzdf-ipc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A private socket, so tests never touch a real viewer's.
        let socket = dir.join("temzdf").join("doc.sock");
        let (tx, rx) = std::sync::mpsc::channel();
        let server = serve_at(socket.clone(), move |r| tx.send(r).is_ok()).unwrap();
        assert!(serve_at(socket.clone(), |_| true).is_err(), "second viewer");
        let req = ForwardRequest { file: "main.tex".into(), line: 42, column: 0 };
        assert!(send_to(&socket, &req).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), req);
        // A client that connects and sends nothing does not hold others up.
        let _silent = UnixStream::connect(&socket).unwrap();
        let start = std::time::Instant::now();
        assert!(send_to_patiently(&socket, &req));
        assert!(start.elapsed() < Duration::from_secs(4), "{:?}", start.elapsed());
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), req);
        drop(server);
        assert!(!send_to(&socket, &req).unwrap(), "socket removed");
        // Its thread ended: the delivery function (and its sender) is gone.
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Err(std::sync::mpsc::RecvTimeoutError::Disconnected));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// send_to, waiting longer for the reply than it does.
    fn send_to_patiently(socket: &Path, req: &ForwardRequest) -> bool {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream.write_all(req.encode().as_bytes()).unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        reply.trim() == "ok"
    }
}
