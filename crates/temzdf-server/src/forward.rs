//! Talking to a running viewer: `temzdf --forward LINE:FILE doc.pdf` sends a
//! forward-search request to the viewer that has `doc.pdf` open.
//!
//! Each viewer listens on a Unix socket at
//! `<runtime dir>/temzdf/<hash of the canonical PDF path>.sock`, the runtime
//! directory being `temzdf_protocol::runtime_dir`: `$XDG_RUNTIME_DIR`, else
//! the per-user temp dir on macOS, else `<temp dir>/temzdf-<uid>`. The protocol
//! is one line per request:
//! `forward\t<line>\t<column>\t<source file>\n`, answered with `ok\n`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use temzdf_pdfview::host::Forward;

/// Parse `LINE[:COLUMN]:FILE` (the file may itself contain colons, not
/// a line break: the request is one line).
pub fn parse_spec(spec: &str) -> Option<Forward> {
    if spec.contains(['\n', '\r']) {
        return None;
    }
    let (line, rest) = spec.split_once(':')?;
    let line = line.parse().ok()?;
    let (column, file) = match rest.split_once(':') {
        Some((c, f)) if c.parse::<u32>().is_ok() && !f.is_empty() => (c.parse().ok()?, f),
        _ => (0, rest),
    };
    (!file.is_empty()).then(|| Forward { file: PathBuf::from(file), line, column })
}

fn encode(req: &Forward) -> String {
    format!("forward\t{}\t{}\t{}\n", req.line, req.column, req.file.display())
}

fn decode(line: &str) -> Option<Forward> {
    let mut it = line.trim_end_matches('\n').splitn(4, '\t');
    if it.next()? != "forward" {
        return None;
    }
    let line = it.next()?.parse().ok()?;
    let column = it.next()?.parse().ok()?;
    let file = PathBuf::from(it.next()?);
    Some(Forward { file, line, column })
}

/// The socket a viewer of `pdf` listens on.
pub fn socket_path(pdf: &Path) -> PathBuf {
    // FNV-1a: a stable hash, so every build of temzdf agrees on socket names.
    let hash = temzdf_texteditor::fnv1a(crate::paths::canonical(pdf).as_os_str().as_encoded_bytes().iter().copied());
    temzdf_protocol::runtime_dir().join("temzdf").join(format!("{hash}.sock"))
}

/// Send a request to the viewer of `pdf`. `Ok(false)` when no viewer is running.
pub fn send_forward(pdf: &Path, req: &Forward) -> std::io::Result<bool> {
    send_to(&socket_path(pdf), req)
}

fn send_to(socket: &Path, req: &Forward) -> std::io::Result<bool> {
    let Ok(mut stream) = UnixStream::connect(socket) else { return Ok(false) };
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    stream.write_all(encode(req).as_bytes())?;
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

/// How long a request waits for the viewer's answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// How long one connection may take to send its request (others wait).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Listen for requests about `pdf`, handing them to `deliver` (which returns
/// false once nobody listens any more). Another live viewer of the same file
/// keeps its socket (an error here); a stale socket left by a crashed viewer is
/// replaced.
pub fn serve(pdf: &Path, deliver: impl Fn(Forward) -> bool + Send + 'static) -> std::io::Result<Server> {
    serve_at(socket_path(pdf), deliver)
}

/// [`serve`] once no socket is there any more (the viewer that had it
/// closed the file): `None` while one is. Cheap enough to ask every few
/// seconds; never connects to a live viewer (that would hold its listener
/// up for a request timeout).
pub fn serve_if_free(pdf: &Path, deliver: impl Fn(Forward) -> bool + Send + 'static) -> Option<Server> {
    serve_if_free_at(socket_path(pdf), deliver)
}

fn serve_if_free_at(path: PathBuf, deliver: impl Fn(Forward) -> bool + Send + 'static) -> Option<Server> {
    if path.symlink_metadata().is_ok() {
        return None;
    }
    serve_at(path, deliver).ok()
}

fn serve_at(path: PathBuf, deliver: impl Fn(Forward) -> bool + Send + 'static) -> std::io::Result<Server> {
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
            let reply = match decode(&line).filter(|_| ok) {
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
        let r = parse_spec("12:main.tex").unwrap();
        assert_eq!((r.line, r.column, r.file.to_str().unwrap()), (12, 0, "main.tex"));
        let r = parse_spec("12:5:/a/b:c.tex").unwrap();
        assert_eq!((r.line, r.column, r.file.to_str().unwrap()), (12, 5, "/a/b:c.tex"));
        assert!(parse_spec("x:main.tex").is_none());
        assert!(parse_spec("3:").is_none());
        assert!(parse_spec("3:a\nb.tex").is_none(), "a line break would cut the request");
        let req = Forward { file: "a b.tex".into(), line: 3, column: 1 };
        assert_eq!(decode(&encode(&req)), Some(req));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri isolates the file system (canonicalize, the runtime directory)")]
    fn socket_names_stay_as_they_were() {
        // Viewers of other builds must find each other: the name is the
        // FNV-1a of the canonical (here: absolute, missing) path.
        let socket = socket_path(Path::new("/nonexistent-temzdf/doc.pdf"));
        assert_eq!(socket.file_name().unwrap(), "3af4a6028ebd1343.sock");
        assert_eq!(socket.parent().unwrap(), temzdf_protocol::runtime_dir().join("temzdf"));
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
        let req = Forward { file: "main.tex".into(), line: 42, column: 0 };
        assert!(send_to(&socket, &req).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), req);
        // A client that connects and sends nothing does not hold others up:
        // while it stays connected, the next one is answered (once its
        // REQUEST_TIMEOUT ran out; no wall-clock bound here, CI machines
        // are slow, but the patient reply's own timeout is far above it).
        let _silent = UnixStream::connect(&socket).unwrap();
        assert!(send_to_patiently(&socket, &req));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), req);
        drop(server);
        assert!(!send_to(&socket, &req).unwrap(), "socket removed");
        // Its thread ended: the delivery function (and its sender) is gone.
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Err(std::sync::mpsc::RecvTimeoutError::Disconnected));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri has no Unix sockets")]
    fn a_second_viewer_takes_the_socket_over_once_the_first_closes() {
        let dir = std::env::temp_dir().join(format!("temzdf-ipc-takeover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("temzdf").join("doc.sock");
        let first = serve_at(socket.clone(), |_| true).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let second = move || {
            let tx = tx.lock().unwrap().clone();
            serve_if_free_at(socket.clone(), move |r| tx.send(r).is_ok())
        };
        assert!(second().is_none(), "the first has it");
        drop(first);
        let server = second().expect("free now");
        let req = Forward { file: "main.tex".into(), line: 7, column: 0 };
        assert!(send_to(&dir.join("temzdf").join("doc.sock"), &req).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), req);
        drop(server);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// send_to, waiting longer for the reply than it does.
    fn send_to_patiently(socket: &Path, req: &Forward) -> bool {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream.set_read_timeout(Some(REQUEST_TIMEOUT * 15)).unwrap();
        stream.write_all(encode(req).as_bytes()).unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        reply.trim() == "ok"
    }
}
