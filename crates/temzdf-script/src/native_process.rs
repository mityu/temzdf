//! The script host's commands (`temzdf/process`), run natively on the
//! server's side of the guest as its file access is ([`crate::native_fs`],
//! whose [`crate::native_fs::Workers`] start them): finding a command as
//! Vim's `exepath()` does, and running one to its end with its output
//! collected. No shell. Commands still running when the host stops are
//! killed ([`Children::kill_all`]). Never panics: it runs inside the server.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use temzdf_protocol::script_fs::{Exit, FsError, FsErrorKind, FsResult, RunSpec};

/// The most a command may write to each of its stdout and stderr: as a
/// file read at once ([`crate::native_fs::READ_LIMIT`]).
pub const OUTPUT_LIMIT: u64 = crate::native_fs::READ_LIMIT;

/// How long a killed command's output is waited for: a process it started
/// may hold its pipes open after it.
const AFTER_KILL: Duration = Duration::from_secs(1);

/// The commands running, by operation id, to kill.
#[derive(Default)]
pub struct Children {
    running: Mutex<HashMap<u64, Arc<Mutex<Child>>>>,
    /// Set by [`Children::kill_all`]: nothing starts after it.
    closed: AtomicBool,
}

impl Children {
    /// Kill the command operation `id` runs, if it does.
    pub fn kill(&self, id: u64) {
        let child = self.running.lock().ok().and_then(|r| r.get(&id).cloned());
        if let Some(child) = child {
            kill(&child);
        }
    }

    /// Kill every command running, and any started later (the host stops).
    pub fn kill_all(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let all: Vec<_> = self.running.lock().map(|r| r.values().cloned().collect()).unwrap_or_default();
        for child in all {
            kill(&child);
        }
    }

    /// Keep `child` as operation `id`'s; false (and killed) once closed.
    fn add(&self, id: u64, child: &Arc<Mutex<Child>>) -> bool {
        if let Ok(mut running) = self.running.lock()
            && !self.closed.load(Ordering::SeqCst)
        {
            running.insert(id, child.clone());
            return true;
        }
        kill(child);
        false
    }

    fn remove(&self, id: u64) {
        if let Ok(mut running) = self.running.lock() {
            running.remove(&id);
        }
    }
}

fn kill(child: &Mutex<Child>) {
    if let Ok(mut c) = child.lock() {
        let _ = c.kill();
    }
}

/// Where command `name` is, as [`run`] finds it: the server's `PATH`.
pub fn exepath(name: &str) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    exepath_in(name, std::env::var_os("PATH").as_deref(), std::env::var_os("PATHEXT").as_deref(), &cwd)
}

/// Where command `name` is with `path` as `PATH` (and on Windows `pathext`
/// as `PATHEXT`): a name with a directory in it is that file (relative ones
/// from `cwd`), any other the first in `path`'s directories; on Unix a
/// regular file someone may execute, on Windows one with an extension of
/// `pathext`'s (added when the name has none of them).
pub fn exepath_in(name: &str, path: Option<&OsStr>, pathext: Option<&OsStr>, cwd: &Path) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let exts = extensions(pathext);
    let found = |base: PathBuf| candidates(base, &exts).into_iter().find(|p| executable(p));
    let separated = name.contains('/') || (cfg!(windows) && (name.contains('\\') || name.contains(':')));
    if separated {
        return found(cwd.join(name));
    }
    std::env::split_paths(path?).find_map(|dir| found(cwd.join(dir).join(name)))
}

/// `PATHEXT`'s extensions, lower case (Windows; none elsewhere).
fn extensions(pathext: Option<&OsStr>) -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let list = pathext.map_or_else(|| ".COM;.EXE;.BAT;.CMD".into(), |p| p.to_string_lossy().into_owned());
    list.split(';').filter(|e| !e.is_empty()).map(str::to_ascii_lowercase).collect()
}

/// The files `base` may be: itself on Unix; on Windows itself when it has
/// one of `exts`, else itself with each.
fn candidates(base: PathBuf, exts: &[String]) -> Vec<PathBuf> {
    if !cfg!(windows) {
        return vec![base];
    }
    let has = base
        .extension()
        .is_some_and(|e| exts.iter().any(|x| x.trim_start_matches('.') == e.to_string_lossy().to_ascii_lowercase()));
    if has {
        return vec![base];
    }
    exts.iter()
        .map(|e| {
            let mut s = OsString::from(base.as_os_str());
            s.push(e);
            PathBuf::from(s)
        })
        .collect()
}

#[cfg(unix)]
fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file())
}

fn fail(kind: FsErrorKind, message: String) -> FsError {
    FsError { kind, message }
}

/// Run `spec` to its end as operation `id` (killed by `children`), its
/// output at most [`OUTPUT_LIMIT`] each.
pub fn run(id: u64, spec: &RunSpec, children: &Children) -> FsResult {
    run_with(id, spec, children, OUTPUT_LIMIT)
}

/// [`run`] with another output limit (tests).
pub fn run_with(id: u64, spec: &RunSpec, children: &Children, limit: u64) -> FsResult {
    let name = &spec.program;
    let cwd = match &spec.cwd {
        Some(d) if Path::new(d).is_absolute() => PathBuf::from(d),
        Some(d) => return Err(fail(FsErrorKind::NotAbsolute, format!("{d}: not an absolute path"))),
        None => std::env::current_dir().map_err(|e| fail(FsErrorKind::Other, format!("no working directory: {e}")))?,
    };
    // The command is looked for in the PATH it is given, if any.
    let given = |key: &str| {
        spec.env
            .iter()
            .rev()
            .find(|(k, _)| if cfg!(windows) { k.eq_ignore_ascii_case(key) } else { k == key })
            .map(|(_, v)| OsString::from(v))
    };
    let path = given("PATH").or_else(|| std::env::var_os("PATH"));
    let pathext = given("PATHEXT").or_else(|| std::env::var_os("PATHEXT"));
    let Some(program) = exepath_in(name, path.as_deref(), pathext.as_deref(), &cwd) else {
        return Err(fail(FsErrorKind::NotFound, format!("{name}: command not found")));
    };
    let mut cmd = Command::new(&program);
    cmd.args(&spec.args)
        .current_dir(&cwd)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // The server has no console: without this, each console program
        // would open a window of its own.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| {
        let kind = match e.kind() {
            std::io::ErrorKind::NotFound => FsErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied => FsErrorKind::PermissionDenied,
            _ => FsErrorKind::Other,
        };
        fail(kind, format!("{name}: {e}"))
    })?;
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let child = Arc::new(Mutex::new(child));
    if !children.add(id, &child) {
        return Err(fail(FsErrorKind::Other, format!("{name}: the script host stopped")));
    }
    if let (Some(mut pipe), Some(input)) = (stdin, spec.stdin.clone()) {
        // A command that stops reading early closes the pipe: not an error.
        let _ = std::thread::Builder::new().name("temzdf-process-stdin".into()).spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    let too_large = Arc::new(AtomicBool::new(false));
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        let (child, too_large) = (child.clone(), too_large.clone());
        let read = move || {
            let mut out = Vec::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(limit + 1).read_to_end(&mut out);
            }
            if out.len() as u64 > limit {
                too_large.store(true, Ordering::SeqCst);
                kill(&child);
                out.truncate(limit as usize);
            }
            let _ = tx.send(out);
        };
        // No thread: no output (the sender goes with the closure).
        let _ = std::thread::Builder::new().name("temzdf-process-output".into()).spawn(read);
        rx
    };
    let out = reader(stdout.map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = reader(stderr.map(|p| Box::new(p) as Box<dyn Read + Send>));
    let (status, timed_out) = wait(&child, spec.timeout_ms.map(Duration::from_millis));
    children.remove(id);
    let killed = timed_out || too_large.load(Ordering::SeqCst) || status.as_ref().is_some_and(|s| s.code().is_none());
    let collect = |rx: mpsc::Receiver<Vec<u8>>| {
        if killed { rx.recv_timeout(AFTER_KILL).unwrap_or_default() } else { rx.recv().unwrap_or_default() }
    };
    let (stdout, stderr) = (collect(out), collect(err));
    if too_large.load(Ordering::SeqCst) {
        return Err(fail(FsErrorKind::TooLarge, format!("{name}: wrote more than {} bytes", limit)));
    }
    if timed_out {
        let ms = spec.timeout_ms.unwrap_or_default();
        return Err(fail(FsErrorKind::TimedOut, format!("{name}: still running after {ms} ms")));
    }
    let status = status.ok_or_else(|| fail(FsErrorKind::Other, format!("{name}: its end could not be told")))?;
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    #[cfg(not(unix))]
    let signal = None;
    Ok(temzdf_protocol::script_fs::FsReply::Exited(Exit { code: status.code(), signal, stdout, stderr }))
}

/// Wait for `child` to end, killing it after `timeout`: its status (none if
/// it cannot be told) and whether it timed out. Polled, the lock taken
/// only for a moment, so that [`Children::kill`] can take it meanwhile.
fn wait(child: &Mutex<Child>, timeout: Option<Duration>) -> (Option<std::process::ExitStatus>, bool) {
    let deadline = timeout.map(|t| Instant::now() + t);
    let mut pause = Duration::from_millis(1);
    let mut timed_out = false;
    loop {
        match child.lock().map(|mut c| c.try_wait()) {
            Ok(Ok(Some(status))) => return (Some(status), timed_out),
            Ok(Ok(None)) => {}
            _ => return (None, timed_out),
        }
        if !timed_out && deadline.is_some_and(|d| Instant::now() >= d) {
            timed_out = true;
            kill(child);
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temzdf_protocol::script_fs::FsReply;

    fn exited(r: FsResult) -> Exit {
        match r {
            Ok(FsReply::Exited(e)) => e,
            other => panic!("not an exit: {other:?}"),
        }
    }

    /// A directory of its own for a test, removed after it.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!("temzdf-native-process-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn kind(r: FsResult) -> FsErrorKind {
        r.expect_err("an error").kind
    }

    #[cfg(unix)]
    fn sh(script: &str) -> RunSpec {
        RunSpec { program: "sh".into(), args: vec!["-c".into(), script.into()], ..RunSpec::default() }
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot run commands")]
    fn commands_are_found_where_they_may_be_run() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Scratch::new("found");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let file = |p: &Path, mode: u32| {
            std::fs::write(p, "").unwrap();
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        file(&a.join("tool"), 0o644);
        file(&b.join("tool"), 0o755);
        file(&a.join("first"), 0o700);
        file(&b.join("first"), 0o755);
        let path = std::env::join_paths([&a, &b]).unwrap();
        let find = |name: &str| exepath_in(name, Some(&path), None, dir.path());
        assert_eq!(find("tool"), Some(b.join("tool")), "past the one nobody may execute");
        assert_eq!(find("first"), Some(a.join("first")), "the first directory's");
        assert_eq!(find("none"), None);
        assert_eq!(find(""), None);
        assert_eq!(find("b/tool"), Some(dir.path().join("b/tool")), "a directory in the name: from cwd");
        assert_eq!(find("a/tool"), None, "not executable");
        assert_eq!(find(&b.join("tool").display().to_string()), Some(b.join("tool")));
        assert_eq!(exepath_in("tool", None, None, dir.path()), None, "no PATH");
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot run commands")]
    fn commands_run_to_their_end_with_their_output() {
        let children = Children::default();
        let mut spec = sh("cat; echo err >&2; exit 3");
        spec.stdin = Some(b"in\n".to_vec());
        let e = exited(run(1, &spec, &children));
        assert_eq!((e.code, e.signal), (Some(3), None));
        assert_eq!((e.stdout.as_slice(), e.stderr.as_slice()), (&b"in\n"[..], &b"err\n"[..]));
        // No stdin: an empty one, not the server's.
        assert_eq!(exited(run(2, &sh("cat"), &children)).stdout, b"");
        // The environment and the directory given.
        let dir = Scratch::new("run");
        let real = std::fs::canonicalize(dir.path()).unwrap();
        let mut spec = sh("echo \"$K\"; pwd -P");
        spec.env = vec![("K".into(), "v".into())];
        spec.cwd = Some(real.display().to_string());
        assert_eq!(
            String::from_utf8(exited(run(3, &spec, &children)).stdout).unwrap(),
            format!("v\n{}\n", real.display())
        );
        spec.cwd = Some("relative".into());
        assert_eq!(kind(run(4, &spec, &children)), FsErrorKind::NotAbsolute);
        let missing = RunSpec { program: "temzdf-no-such-command".into(), ..RunSpec::default() };
        assert_eq!(kind(run(5, &missing, &children)), FsErrorKind::NotFound);
        assert!(children.running.lock().unwrap().is_empty(), "none left");
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot run commands")]
    fn commands_are_stopped_past_their_limits() {
        let children = Children::default();
        assert_eq!(kind(run_with(1, &sh("head -c 2000 /dev/zero"), &children, 1000)), FsErrorKind::TooLarge);
        assert_eq!(exited(run_with(2, &sh("head -c 1000 /dev/zero"), &children, 1000)).stdout.len(), 1000);
        let mut slow = sh("sleep 30");
        slow.timeout_ms = Some(50);
        let start = Instant::now();
        assert_eq!(kind(run(3, &slow, &children)), FsErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot run commands")]
    fn commands_are_killed_one_or_all() {
        let children = Arc::new(Children::default());
        let started = |id: u64| {
            let c = children.clone();
            let t = std::thread::spawn(move || run(id, &sh("exec sleep 30"), &c));
            while !children.running.lock().unwrap().contains_key(&id) {
                std::thread::sleep(Duration::from_millis(5));
            }
            t
        };
        let one = started(1);
        children.kill(1);
        let e = exited(one.join().unwrap());
        assert_eq!((e.code, e.signal), (None, Some(9)), "killed");
        let (two, three) = (started(2), started(3));
        children.kill_all();
        for t in [two, three] {
            assert_eq!(exited(t.join().unwrap()).signal, Some(9));
        }
        let after = run(4, &sh("true"), &children);
        assert_eq!(kind(after), FsErrorKind::Other, "nothing starts after");
    }

    #[cfg(windows)]
    #[test]
    fn commands_are_found_with_pathext_and_run() {
        let dir = Scratch::new("pathext");
        std::fs::write(dir.path().join("tool.cmd"), "@echo off\r\n").unwrap();
        std::fs::write(dir.path().join("tool.txt"), "").unwrap();
        let path = dir.path().as_os_str();
        let exts = OsStr::new(".EXE;.CMD");
        let find = |name: &str| exepath_in(name, Some(path), Some(exts), dir.path());
        assert_eq!(find("tool"), Some(dir.path().join("tool.cmd")), "the extension added");
        assert_eq!(find("TOOL.CMD"), Some(dir.path().join("TOOL.CMD")), "one it has");
        assert_eq!(find("tool.txt"), None, "not one of PATHEXT's");
        let found = exepath("cmd").expect("cmd.exe on PATH");
        assert!(found.to_string_lossy().to_ascii_lowercase().ends_with("cmd.exe"), "{found:?}");
        let children = Children::default();
        let spec =
            RunSpec { program: "cmd".into(), args: vec!["/C".into(), "echo hi& exit 3".into()], ..RunSpec::default() };
        let e = exited(run(1, &spec, &children));
        assert_eq!((e.code, String::from_utf8_lossy(&e.stdout).trim()), (Some(3), "hi"));
    }
}
