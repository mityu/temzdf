//! The script host's file access, done natively on the server's side of the
//! guest (crates/temzdf-script-runner's imports `temzdf.fs_*`) and by
//! [`crate::link::StreamLink`]: the OS's own paths and `std::fs`, not WASI,
//! whose sandboxing refuses Windows' drive paths and symbolic links to
//! absolute paths. Read-only. Never panics: it runs inside the server.

use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use temzdf_protocol::script_fs::{DirEntry, FileInfo, FsError, FsErrorKind, FsOp, FsReply, FsResult};

use crate::native_process::Children;

/// The largest file read at once: a read takes a few times its size in the
/// guest (the bytes, then a string), well inside its memory cap.
pub const READ_LIMIT: u64 = 64 << 20;
/// The most entries a directory is listed with at once.
pub const DIR_LIMIT: usize = 100_000;

/// Do `op`, with the limits above.
pub fn run(op: &FsOp) -> FsResult {
    run_with(op, READ_LIMIT, DIR_LIMIT)
}

/// Do `op`, with other limits (tests).
pub fn run_with(op: &FsOp, read_limit: u64, dir_limit: usize) -> FsResult {
    match op {
        FsOp::ReadFile(p) => read_file(p, read_limit).map(FsReply::Bytes),
        FsOp::Stat(p) => Ok(FsReply::Info(info(&fs::metadata(absolute(p)?).map_err(|e| error(p, &e))?))),
        FsOp::Lstat(p) => Ok(FsReply::Info(info(&fs::symlink_metadata(absolute(p)?).map_err(|e| error(p, &e))?))),
        FsOp::ReadDir(p) => read_dir(p, dir_limit).map(FsReply::Entries),
        FsOp::RealPath(p) => {
            let real = fs::canonicalize(absolute(p)?).map_err(|e| error(p, &e))?;
            Ok(FsReply::Path(plain(&real.to_string_lossy()).into_owned()))
        }
        FsOp::Exepath(name) => {
            Ok(FsReply::Exe(crate::native_process::exepath(name).map(|p| plain(&p.to_string_lossy()).into_owned())))
        }
        // Started by `Workers` only, each on a thread of its own.
        FsOp::Run(_) | FsOp::Kill(_) => {
            Err(FsError { kind: FsErrorKind::Other, message: "a command is not run here".into() })
        }
    }
}

/// Threads doing operations as they are started ([`Workers::start`]), each
/// result handed to the function given (from that thread). Dropped, they
/// end once their current operation does; nobody waits for them (one stuck
/// on a hung file system must not hold up a reload).
pub struct Workers {
    jobs: Option<Sender<(u64, FsOp)>>,
    done: Arc<dyn Fn(u64, FsResult) + Send + Sync>,
    /// The commands running (`FsOp::Run`), killed when these go.
    children: Arc<Children>,
}

impl Workers {
    /// How many operations run at once.
    pub const THREADS: usize = 4;

    pub fn new(done: impl Fn(u64, FsResult) + Send + Sync + 'static) -> Self {
        let done: Arc<dyn Fn(u64, FsResult) + Send + Sync> = Arc::new(done);
        let (tx, rx) = mpsc::channel::<(u64, FsOp)>();
        let rx = Arc::new(Mutex::new(rx));
        let mut started = 0;
        for i in 0..Self::THREADS {
            let (rx, done) = (rx.clone(), done.clone());
            let spawned = std::thread::Builder::new().name(format!("temzdf-script-fs-{i}")).spawn(move || {
                loop {
                    // One thread waits for the next job, the others for the lock.
                    let job = match rx.lock() {
                        Ok(rx) => rx.recv(),
                        Err(_) => return,
                    };
                    let Ok((id, op)) = job else { return };
                    done(id, run(&op));
                }
            });
            started += usize::from(spawned.is_ok());
        }
        // No thread: done where started (still answered, later than asked).
        Self { jobs: (started > 0).then_some(tx), done, children: Arc::default() }
    }

    /// The commands running, to kill from elsewhere (the host stopping).
    pub fn children(&self) -> Arc<Children> {
        self.children.clone()
    }

    /// Start operation `id`: a command on a thread of its own (answered
    /// when it ends), a kill at once (answering nothing), the rest on the
    /// threads above.
    pub fn start(&self, id: u64, op: FsOp) {
        let op = match op {
            FsOp::Run(spec) => {
                let (done, children) = (self.done.clone(), self.children.clone());
                let spawned = std::thread::Builder::new()
                    .name("temzdf-process".into())
                    .spawn(move || done(id, crate::native_process::run(id, &spec, &children)));
                if spawned.is_err() {
                    let why = FsError { kind: FsErrorKind::Other, message: "no thread to run a command on".into() };
                    (self.done)(id, Err(why));
                }
                return;
            }
            FsOp::Kill(target) => return self.children.kill(target),
            op => op,
        };
        if let Some(jobs) = &self.jobs
            && jobs.send((id, op.clone())).is_ok()
        {
            return;
        }
        (self.done)(id, run(&op));
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.children.kill_all();
    }
}

fn absolute(path: &str) -> Result<&Path, FsError> {
    let p = Path::new(path);
    if p.is_absolute() { Ok(p) } else { Err(fail(FsErrorKind::NotAbsolute, path, "not an absolute path")) }
}

fn fail(kind: FsErrorKind, path: &str, what: &str) -> FsError {
    FsError { kind, message: format!("{path}: {what}") }
}

/// An I/O error as a kind and, for the common ones, a message the same on
/// every OS (no `os error N`, whose numbers differ).
fn error(path: &str, e: &io::Error) -> FsError {
    let (kind, what) = match e.kind() {
        io::ErrorKind::NotFound => (FsErrorKind::NotFound, "no such file or directory".to_owned()),
        io::ErrorKind::PermissionDenied => (FsErrorKind::PermissionDenied, "permission denied".to_owned()),
        io::ErrorKind::IsADirectory => (FsErrorKind::IsADirectory, "is a directory".to_owned()),
        io::ErrorKind::NotADirectory => (FsErrorKind::NotADirectory, "not a directory".to_owned()),
        // Rare ones: the OS's own words.
        _ => (FsErrorKind::Other, e.to_string()),
    };
    fail(kind, path, &what)
}

/// Only regular files: a FIFO would block the read (and the guest, whose
/// module loader waits for it), a device may never end.
fn regular(path: &str, meta: &Metadata) -> Result<(), FsError> {
    if meta.is_dir() {
        Err(fail(FsErrorKind::IsADirectory, path, "is a directory"))
    } else if !meta.is_file() {
        Err(fail(FsErrorKind::NotAFile, path, "not a regular file"))
    } else {
        Ok(())
    }
}

fn read_file(path: &str, limit: u64) -> Result<Vec<u8>, FsError> {
    let too_large = || fail(FsErrorKind::TooLarge, path, &format!("larger than {} bytes", limit));
    let p = absolute(path)?;
    // Before opening: opening a FIFO blocks.
    let meta = fs::metadata(p).map_err(|e| error(path, &e))?;
    regular(path, &meta)?;
    if meta.len() > limit {
        return Err(too_large());
    }
    let file = File::open(p).map_err(|e| error(path, &e))?;
    // What was opened, in case it changed since.
    regular(path, &file.metadata().map_err(|e| error(path, &e))?)?;
    // Not trusting the size: files that say 0 (/proc), files that grow.
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|e| error(path, &e))?;
    if bytes.len() as u64 > limit {
        return Err(too_large());
    }
    Ok(bytes)
}

fn read_dir(path: &str, limit: usize) -> Result<Vec<DirEntry>, FsError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(absolute(path)?).map_err(|e| error(path, &e))? {
        let entry = entry.map_err(|e| error(path, &e))?;
        if entries.len() == limit {
            return Err(fail(FsErrorKind::TooMany, path, &format!("more than {limit} entries")));
        }
        let t = entry.file_type().map_err(|e| error(path, &e))?;
        entries.push(DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_file: t.is_file(),
            is_directory: t.is_dir(),
            is_symlink: t.is_symlink(),
        });
    }
    Ok(entries)
}

fn info(meta: &Metadata) -> FileInfo {
    FileInfo {
        is_file: meta.is_file(),
        is_directory: meta.is_dir(),
        is_symlink: meta.is_symlink(),
        size: meta.len(),
        mtime: millis(meta.modified()),
        atime: millis(meta.accessed()),
        birthtime: millis(meta.created()),
    }
}

/// Milliseconds since the Unix epoch (negative before it).
fn millis(t: io::Result<SystemTime>) -> Option<f64> {
    Some(match t.ok()?.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64() * 1000.0,
        Err(e) => -e.duration().as_secs_f64() * 1000.0,
    })
}

/// Windows' canonical paths are verbatim (`\\?\C:\x`, `\\?\UNC\server\x`):
/// as users write them (`C:\x`, `\\server\x`), as Deno's `realPath` does.
fn plain(p: &str) -> std::borrow::Cow<'_, str> {
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}").into();
    }
    match p.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.into(),
        _ => p.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("temzdf-native-fs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn s(p: &Path) -> String {
        p.to_str().unwrap().to_owned()
    }

    fn kind(r: FsResult) -> FsErrorKind {
        r.unwrap_err().kind
    }

    #[test]
    fn reads_regular_files_up_to_the_limit() {
        let d = dir("read");
        let f = d.join("a.txt");
        fs::write(&f, "abcd").unwrap();
        assert_eq!(run_with(&FsOp::ReadFile(s(&f)), 4, 10), Ok(FsReply::Bytes(b"abcd".to_vec())));
        let e = run_with(&FsOp::ReadFile(s(&f)), 3, 10).unwrap_err();
        assert_eq!(e.kind, FsErrorKind::TooLarge);
        assert_eq!(e.message, format!("{}: larger than 3 bytes", s(&f)));
        assert_eq!(kind(run(&FsOp::ReadFile(s(&d)))), FsErrorKind::IsADirectory);
        let missing = run(&FsOp::ReadFile(s(&d.join("none")))).unwrap_err();
        assert_eq!(missing.kind, FsErrorKind::NotFound);
        assert!(missing.message.ends_with(": no such file or directory"), "{}", missing.message);
    }

    /// Files that say they are empty, as /proc's: the size is not trusted.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_limit_holds_for_files_that_misstate_their_size() {
        assert_eq!(fs::metadata("/proc/self/status").unwrap().len(), 0);
        assert_eq!(kind(run_with(&FsOp::ReadFile("/proc/self/status".into()), 3, 10)), FsErrorKind::TooLarge);
    }

    #[test]
    fn relative_paths_are_refused() {
        for op in [FsOp::ReadFile("a.txt".into()), FsOp::Stat(".".into()), FsOp::ReadDir("x".into())] {
            assert_eq!(kind(run(&op)), FsErrorKind::NotAbsolute);
        }
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start mkfifo")]
    fn a_fifo_is_not_opened() {
        let d = dir("fifo");
        let f = d.join("pipe");
        assert!(std::process::Command::new("mkfifo").arg(&f).status().unwrap().success());
        // Opening it would block until a writer came.
        assert_eq!(kind(run(&FsOp::ReadFile(s(&f)))), FsErrorKind::NotAFile);
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_to_absolute_paths_are_followed() {
        let d = dir("link");
        let f = d.join("real.ts");
        fs::write(&f, "x").unwrap();
        let l = d.join("link.ts");
        std::os::unix::fs::symlink(&f, &l).unwrap();
        assert_eq!(run(&FsOp::ReadFile(s(&l))), Ok(FsReply::Bytes(b"x".to_vec())));
        let Ok(FsReply::Info(i)) = run(&FsOp::Lstat(s(&l))) else { panic!() };
        assert!(i.is_symlink && !i.is_file);
        let Ok(FsReply::Info(i)) = run(&FsOp::Stat(s(&l))) else { panic!() };
        assert!(i.is_file && !i.is_symlink && i.size == 1 && i.mtime.is_some());
        let Ok(FsReply::Path(p)) = run(&FsOp::RealPath(s(&l))) else { panic!() };
        assert_eq!(p, s(&fs::canonicalize(&f).unwrap()));
    }

    #[test]
    fn directories_are_listed_up_to_the_limit() {
        let d = dir("list");
        fs::write(d.join("f"), "").unwrap();
        fs::create_dir(d.join("sub")).unwrap();
        let Ok(FsReply::Entries(mut es)) = run(&FsOp::ReadDir(s(&d))) else { panic!() };
        es.sort_by(|a, b| a.name.cmp(&b.name));
        let flags: Vec<_> = es.iter().map(|e| (e.name.as_str(), e.is_file, e.is_directory)).collect();
        assert_eq!(flags, [("f", true, false), ("sub", false, true)]);
        assert_eq!(kind(run_with(&FsOp::ReadDir(s(&d)), 10, 1)), FsErrorKind::TooMany);
        assert_eq!(kind(run(&FsOp::ReadDir(s(&d.join("f"))))), FsErrorKind::NotADirectory);
    }

    #[test]
    fn workers_answer_every_operation_started() {
        let d = dir("workers");
        fs::write(d.join("a"), "x").unwrap();
        let (tx, rx) = mpsc::channel();
        let w = Workers::new(move |id, r| tx.send((id, r)).unwrap());
        for id in 0..20 {
            let p = if id % 2 == 0 { d.join("a") } else { d.join("none") };
            w.start(id, FsOp::ReadFile(s(&p)));
        }
        let mut got: Vec<_> = (0..20).map(|_| rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap()).collect();
        got.sort_by_key(|(id, _)| *id);
        for (id, r) in got {
            assert_eq!(r.is_ok(), id % 2 == 0, "{id}: {r:?}");
        }
    }

    #[test]
    fn verbatim_windows_paths_are_made_plain() {
        assert_eq!(plain(r"\\?\C:\a\b"), r"C:\a\b");
        assert_eq!(plain(r"\\?\UNC\server\share\x"), r"\\server\share\x");
        assert_eq!(plain(r"\\?\Volume{1}\x"), r"\\?\Volume{1}\x");
        assert_eq!(plain("/a/b"), "/a/b");
    }
}
