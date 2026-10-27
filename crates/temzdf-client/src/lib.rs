//! What every temzdf client shares (the terminal client in `crates/temzdf`,
//! the GUI in `crates/temzdf-gui`): finding sessions and starting their
//! servers, where logs go, and carrying out effects the session asks for in
//! the client's environment (links, inverse search). No terminal and no
//! window code.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use temzdf_protocol as proto;

/// Names given to sessions started without one, in order (the NATO phonetic
/// alphabet, ICAO spellings).
const NAMES: [&str; 26] = [
    "alfa", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliett", "kilo", "lima",
    "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango", "uniform", "victor", "whiskey", "xray",
    "yankee", "zulu",
];

/// A name for a new session nobody named: the first of [`NAMES`] not taken.
pub fn auto_name() -> Result<String> {
    let is_taken = taken_names()?;
    Ok(pick_name(|n| is_taken(n)))
}

/// Whether a session name is in use: a live session (of this temzdf version
/// or another: its server would refuse the name too), or a server that holds
/// its lock (starting, not listening yet).
fn taken_names() -> Result<impl Fn(&str) -> bool> {
    let (sessions, other) = proto::discovery::list_all()?;
    let live: Vec<String> = sessions.into_iter().map(|i| i.name).chain(other).collect();
    Ok(move |n: &str| live.iter().any(|l| l == n) || proto::discovery::name_locked(n))
}

/// The first of [`NAMES`] not taken; then `alfa-2`, `bravo-2`, ...,
/// `alfa-3`, and so on.
fn pick_name(taken: impl Fn(&str) -> bool) -> String {
    (1..)
        .flat_map(|round| NAMES.iter().map(move |n| if round == 1 { (*n).to_owned() } else { format!("{n}-{round}") }))
        .find(|n| !taken(n))
        .expect("names never run out")
}

/// `spawn_server` found the name taken by another server (one started at
/// the same time may win it).
#[derive(Debug)]
pub struct NameTaken(pub String);

impl std::fmt::Display for NameTaken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session {:?} already exists (attach with `temzdf attach {}`)", self.0, self.0)
    }
}

impl std::error::Error for NameTaken {}

/// Start a session under a name `choose` gives: again with the next name
/// when another server took it meanwhile (`retry`, for names temzdf picks),
/// or the error. Returns the name. See [`spawn_server`].
pub fn spawn_named(
    choose: impl Fn() -> Result<String>,
    retry: bool,
    spawn: impl Fn(&str) -> Result<()>,
) -> Result<String> {
    for _ in 0..20 {
        let name = choose()?;
        match spawn(&name) {
            Ok(()) => return Ok(name),
            Err(e) if retry && e.downcast_ref::<NameTaken>().is_some() => continue,
            Err(e) => return Err(e),
        }
    }
    bail!("no free session name (other servers keep taking them)")
}

/// Whether `info` is the server `spawn_server` started (process `pid`), not
/// another one that took the name at the same time.
fn started(info: &proto::SessionInfo, pid: u32) -> bool {
    info.pid == pid
}

/// A session name from a file name: its stem with characters outside
/// `[A-Za-z0-9._-]` replaced, and a number appended while the name is taken.
pub fn name_for(file: &Path) -> Result<String> {
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut base: String =
        stem.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    // Room for "-N" while the socket path still fits.
    let room = proto::max_name_len()?.saturating_sub(4).max(1);
    // (Not an option look-alike: the server takes the name as an argument.)
    base = base.trim_start_matches(['.', '-']).chars().take(room.min(56)).collect();
    if base.is_empty() {
        base = "temzdf".into();
    }
    let taken = taken_names()?;
    let mut name = base.clone();
    let mut n = 2;
    while taken(&name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    Ok(name)
}

/// What a new session's first pane shows, as `new-pane` takes it: `file`
/// (made absolute), or a shell.
pub fn first_pane(file: Option<&Path>) -> Result<Vec<String>> {
    Ok(match file {
        // Checked here: the server finds out in the background, too late
        // to fail the command.
        Some(f) if !f.exists() => bail!("{}: no such file", f.display()),
        Some(f) => vec![utf8(&std::path::absolute(f)?)?],
        None => Vec::new(),
    })
}

/// Start a server for session `name` whose first pane shows `pane` (see
/// [`first_pane`]) with the `temzdf` executable, in the background, and wait
/// until it listens. Its output goes to `<log dir>/<name>.log`.
pub fn spawn_server(
    temzdf: &Path,
    name: &str,
    pane: &[String],
    config_path: Option<&Path>,
    editor: Option<&str>,
    clean: bool,
) -> Result<()> {
    if !proto::valid_name(name) {
        bail!("invalid session name {name:?} (letters, digits, '.', '_' and '-', not first '.' or '-')");
    }
    let socket = proto::socket_path(name)?;
    if proto::discovery::probe(&socket).is_some() {
        return Err(NameTaken(name.to_owned()).into());
    }
    let log = log_dir().map(|d| d.join(format!("{name}.log")));
    // The server reports it to clients (`ls`): it could not, and would look
    // like another version's.
    if let Some(p) = &log {
        utf8(p).context("see TEMZDF_LOG_DIR and XDG_STATE_HOME")?;
    }
    let open_log = |p: &Path| -> Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .with_context(|| format!("cannot write the session's log {} (see TEMZDF_LOG_DIR)", p.display()))
    };
    // Where this run's output starts (for the lines shown if it fails).
    let mut start = 0;
    let (out, err) = match &log {
        Some(p) => {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("cannot make the log directory {} (see TEMZDF_LOG_DIR)", dir.display()))?;
            }
            rotate_log(p);
            start = std::fs::metadata(p).map_or(0, |m| m.len());
            (std::process::Stdio::from(open_log(p)?), std::process::Stdio::from(open_log(p)?))
        }
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    let mut cmd = std::process::Command::new(temzdf);
    cmd.arg("server");
    if let Some(p) = &log {
        cmd.arg("--log").arg(std::path::absolute(p)?);
    }
    if let Some(c) = config_path {
        cmd.arg("--config").arg(c);
    }
    if let Some(e) = editor {
        cmd.arg("--editor").arg(e);
    }
    if clean {
        cmd.arg("--clean");
    }
    cmd.arg(name).arg("--").args(pane);
    let mut child = cmd.stdin(std::process::Stdio::null()).stdout(out).stderr(err).spawn()?;
    for _ in 0..200 {
        match proto::discovery::probe(&socket) {
            Some(info) if started(&info, child.id()) => return Ok(()),
            // Another server started at the same time won the name.
            Some(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(NameTaken(name.to_owned()).into());
            }
            None => {}
        }
        if let Some(status) = child.try_wait()? {
            if proto::discovery::name_locked(name) {
                return Err(NameTaken(name.to_owned()).into());
            }
            let last = log.map(|p| last_lines(&p, start, 3)).unwrap_or_default();
            bail!("the session server exited ({status}):\n{last}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // Not listening in time: it must not linger (nor stay a zombie).
    let _ = child.kill();
    let _ = child.wait();
    bail!("the session server did not start listening")
}

/// A log over this size starts afresh; the last one stays as `NAME.log.1`.
pub const LOG_LIMIT: u64 = 1 << 20;

pub fn rotate_log(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_LIMIT) {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let _ = std::fs::rename(path, old);
    }
}

/// The last `n` lines written to `path` after byte `start`.
pub fn last_lines(path: &Path, start: u64, n: usize) -> String {
    let text = std::fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(text.get(start as usize..).unwrap_or_default());
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The session to attach to: `name`, or the only one.
pub fn resolve(name: Option<&str>) -> Result<String> {
    if let Some(n) = name {
        return Ok(n.to_owned());
    }
    let sessions = proto::discovery::list()?;
    match sessions.as_slice() {
        [one] => Ok(one.name.clone()),
        [] => bail!("no sessions (start one with `temzdf [FILE]`)"),
        many => {
            bail!("several sessions, name one: {}", many.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", "))
        }
    }
}

/// A path as the protocol carries it (UTF-8): one that is not would arrive
/// as another path, so it is an error.
pub fn utf8(p: &Path) -> Result<String> {
    p.to_str().map(str::to_owned).with_context(|| format!("{}: paths must be UTF-8 for sessions", p.display()))
}

/// `$XDG_STATE_HOME/temzdf` (falling back to `~/.local/state/temzdf`).
pub fn state_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state")))?;
    Some(base.join("temzdf"))
}

/// Where logs go: `$TEMZDF_LOG_DIR`, else the state directory.
pub fn log_dir() -> Option<PathBuf> {
    std::env::var_os("TEMZDF_LOG_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).or_else(state_dir)
}

/// Where the viewer's (or a client's) own output goes while it runs:
/// `<log dir>/temzdf.log`.
pub fn log_path() -> Option<PathBuf> {
    let dir = log_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("temzdf.log"))
}

/// The system's handler for links (`OpenUri` without a command).
pub fn default_opener() -> Vec<String> {
    vec![if cfg!(target_os = "macos") { "open" } else { "xdg-open" }.to_owned()]
}

/// Run a shell command detached from the terminal (inverse search).
pub fn run_shell(cmd: &str) {
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    // Reaped when it ends (every inverse search left a zombie before).
    if let Ok(mut child) = child {
        std::thread::spawn(move || child.wait());
    }
}

/// Links that may be opened: URIs (a scheme first, so never something an
/// opener could take for an option) of schemes meant for viewing. They come
/// from documents and from programs in terminal panes, neither trusted, so a
/// `file:` link must name a document, not something the opener would run
/// (see [`viewable_file`]).
pub fn openable(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once(':') else { return false };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" | "mailto" | "ftp" => true,
        "file" => file_path(rest).is_some_and(|p| viewable_file(&p)),
        _ => false,
    }
}

/// The local path of a `file:` URI's rest (`///p`, `//localhost/p`, `/p`),
/// percent-decoded; none for another host.
fn file_path(rest: &str) -> Option<PathBuf> {
    let path = match rest.strip_prefix("//") {
        Some(r) => {
            let (host, path) = r.split_at(r.find('/')?);
            if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
                return None;
            }
            path
        }
        None => rest,
    };
    let path = path.split(['?', '#']).next()?;
    let mut bytes = Vec::new();
    let mut it = path.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hex = [it.next()?, it.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Whether a local file is safe to hand to the opener: a regular file (not a
/// directory: macOS `.app` bundles are those), not executable, and not of a
/// kind openers launch (`.desktop` on Linux, `.command` on macOS, ...).
fn viewable_file(path: &Path) -> bool {
    const LAUNCHED: &[&str] = &[
        "app", "appimage", "bat", "bin", "cmd", "com", "command", "desktop", "exe", "fish", "inetloc", "jar", "js",
        "lnk", "msi", "pkg", "pl", "ps1", "py", "rb", "run", "scpt", "sh", "terminal", "tool", "url", "vbs", "webloc",
        "workflow", "zsh", "bash",
    ];
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else { return false };
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    meta.is_file() && meta.permissions().mode() & 0o111 == 0 && ext.is_none_or(|e| !LAUNCHED.contains(&e.as_str()))
}

/// Open a URI with the configured handler, detached from the terminal. A
/// handler that cannot start or exits with an error (e.g. `xdg-open` over SSH
/// without a display) is reported through `on_failure`.
pub fn open_uri(command: &[String], uri: &str, on_failure: impl Fn(String) + Send + 'static) {
    if !openable(uri) {
        on_failure(format!("not opening {uri}: only http, https, mailto, ftp links and file links to documents open"));
        return;
    }
    let Some((program, args)) = command.split_first() else { return };
    let child = std::process::Command::new(program)
        .args(args)
        .arg(uri)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let uri = uri.to_owned();
    let fail = move |why: String| on_failure(format!("cannot open {uri}: {why}"));
    match child {
        Err(e) => fail(format!("{program}: {e}")),
        Ok(mut child) => {
            let program = program.clone();
            std::thread::spawn(move || match child.wait() {
                Ok(status) if !status.success() => fail(format!("{program} failed ({status})")),
                _ => {}
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn unnamed_sessions_take_the_first_free_word() {
        let taken = |names: &'static [&'static str]| move |n: &str| names.contains(&n);
        assert_eq!(pick_name(taken(&[])), "alfa");
        assert_eq!(pick_name(taken(&["alfa", "charlie", "mine"])), "bravo");
        assert_eq!(pick_name(|n| NAMES.contains(&n)), "alfa-2");
        assert_eq!(pick_name(|n| NAMES.contains(&n) || n == "alfa-2"), "bravo-2");
        // Each fits a session name.
        assert!(NAMES.iter().all(|n| proto::valid_name(n)));
    }

    #[test]
    fn another_server_on_the_socket_is_not_ours() {
        let info = |pid| proto::SessionInfo {
            version: proto::VERSION,
            name: "alfa".into(),
            pid,
            tabs: 1,
            panes: 1,
            buffers: 1,
            clients: 0,
            files: vec![],
            log: None,
        };
        assert!(started(&info(42), 42));
        assert!(!started(&info(41), 42), "a server started at the same time");
        // A name another server took is retried with the next one.
        let names = std::cell::RefCell::new(vec!["bravo", "alfa"]);
        let spawned = std::cell::RefCell::new(Vec::new());
        let got = spawn_named(
            || Ok(names.borrow_mut().pop().unwrap().to_owned()),
            true,
            |n| {
                spawned.borrow_mut().push(n.to_owned());
                if n == "alfa" { Err(NameTaken(n.into()).into()) } else { Ok(()) }
            },
        )
        .unwrap();
        assert_eq!((got.as_str(), spawned.borrow().len()), ("bravo", 2));
        // A name the user gave is not.
        let e = spawn_named(|| Ok("alfa".into()), false, |n| Err(NameTaken(n.into()).into())).unwrap_err();
        assert!(e.to_string().contains("already exists"));
    }

    #[test]
    fn only_viewing_schemes_open() {
        for uri in ["https://example.org", "HTTP://x", "mailto:a@b", "ftp://h/f"] {
            assert!(openable(uri), "{uri}");
        }
        for uri in ["--help", "-x", "javascript:alert(1)", "smb://h/s", "vscode://cmd", "relative/path", ""] {
            assert!(!openable(uri), "{uri}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot set file permissions")]
    fn file_links_open_documents_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("temzdf-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Run.app")).unwrap();
        let file = |name: &str, mode: u32| {
            let p = dir.join(name);
            std::fs::write(&p, "x").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            format!("file://{}", p.display())
        };
        let doc = file("paper with space.pdf", 0o644);
        assert!(openable(&doc), "{doc}");
        assert!(openable(&doc.replace(' ', "%20")), "percent-encoded");
        assert!(openable(&doc.replace("file://", "file://localhost")));
        for bad in [
            file("x.command", 0o644),
            file("app.desktop", 0o644),
            file("run.SH", 0o644),
            file("tool", 0o755),
            format!("file://{}", dir.join("Run.app").display()),
            format!("file://{}", dir.join("missing.pdf").display()),
            doc.replace("file://", "file://elsewhere"),
        ] {
            assert!(!openable(&bad), "{bad}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn logs_rotate_and_failures_show_this_runs_lines() {
        let dir = std::env::temp_dir().join(format!("temzdf-logs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("s.log");
        std::fs::write(&log, vec![b'x'; LOG_LIMIT as usize + 1]).unwrap();
        rotate_log(&log);
        assert!(!log.exists() && dir.join("s.log.1").exists(), "the big log moved aside");
        std::fs::write(&log, "old run\nold error\n").unwrap();
        let start = std::fs::metadata(&log).unwrap().len();
        std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(b"a\nb\nc\nd\n").unwrap();
        assert_eq!(last_lines(&log, start, 3), "b\nc\nd");
        assert_eq!(last_lines(&log, start + 6, 3), "d", "only this run's");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
