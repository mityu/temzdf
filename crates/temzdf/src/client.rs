//! Session commands: start a session server, attach this terminal to a
//! session, list and end sessions, run actions (see doc/session-server.md).

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use temzdf_protocol::{self as proto, ClientMsg, ExitReason, ServerMsg};
use temzdf_scene::Frame;
use temzdf_server::daemon;
use temzdf_tty::{Input, Presented};
use termpix::tty::TermEvent;

enum Event {
    Term(TermEvent),
    Server(ServerMsg),
    /// The server closed the connection.
    Gone,
}

/// A session name from a file name: its stem with characters outside
/// `[A-Za-z0-9._-]` replaced, and a number appended while the name is taken.
pub fn name_for(file: &Path) -> Result<String> {
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut base: String =
        stem.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    // Room for "-N" while the socket path still fits.
    let room = proto::max_name_len()?.saturating_sub(4).max(1);
    base = base.trim_start_matches('.').chars().take(room.min(56)).collect();
    if base.is_empty() {
        base = "temzdf".into();
    }
    let taken: Vec<String> = daemon::list()?.into_iter().map(|i| i.name).collect();
    let mut name = base.clone();
    let mut n = 2;
    while taken.contains(&name) {
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
        Some(f) => vec![std::path::absolute(f)?.display().to_string()],
        None => Vec::new(),
    })
}

/// Start a server for session `name` whose first pane shows `pane` (see
/// [`first_pane`]), in the background, and wait until it listens. Its output
/// goes to `$XDG_STATE_HOME/temzdf/<name>.log`.
pub fn spawn_server(
    name: &str,
    pane: &[String],
    config_path: Option<&Path>,
    editor: Option<&str>,
    clean: bool,
) -> Result<()> {
    if !proto::valid_name(name) {
        bail!("invalid session name {name:?} (letters, digits, '.', '_' and '-')");
    }
    let socket = proto::socket_path(name)?;
    if daemon::probe(&socket).is_some() {
        bail!("session {name:?} already exists (attach with `temzdf attach {name}`)");
    }
    let log = crate::app::log_dir().map(|d| d.join(format!("{name}.log")));
    let out = match &log {
        Some(p) => {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::process::Stdio::from(std::fs::OpenOptions::new().create(true).append(true).open(p)?)
        }
        None => std::process::Stdio::null(),
    };
    let err = match &log {
        Some(p) => std::process::Stdio::from(std::fs::OpenOptions::new().append(true).open(p)?),
        None => std::process::Stdio::null(),
    };
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
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
        if daemon::probe(&socket).is_some() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            let tail = log.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            let last = tail.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            bail!("the session server exited ({status}):\n{last}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    bail!("the session server did not start listening")
}

/// The session to attach to: `name`, or the only one.
pub fn resolve(name: Option<&str>) -> Result<String> {
    if let Some(n) = name {
        return Ok(n.to_owned());
    }
    let sessions = daemon::list()?;
    match sessions.as_slice() {
        [one] => Ok(one.name.clone()),
        [] => bail!("no sessions (start one with `temzdf -s NAME FILE`)"),
        many => {
            bail!("several sessions, name one: {}", many.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", "))
        }
    }
}

/// Attach this terminal to session `name` until detached or the session ends.
/// Returns a line to print once the terminal is restored.
pub fn attach(name: &str) -> Result<String> {
    let socket = proto::socket_path(name)?;
    let stream = UnixStream::connect(&socket).with_context(|| format!("no session {name:?}"))?;
    let _stdio = crate::app::log_path().and_then(|p| termpix::tty::StdioRedirect::to_file(&p).ok());
    let mut front = crate::app::open_frontend()?;
    let (tx, rx) = mpsc::channel::<Event>();
    front.spawn_input(tx.clone(), Event::Term)?;
    let mut reader = std::io::BufReader::new(stream.try_clone()?);
    std::thread::Builder::new().name("temzdf-server-in".into()).spawn(move || {
        while let Ok(Some(msg)) = proto::recv::<ServerMsg>(&mut reader) {
            if tx.send(Event::Server(msg)).is_err() {
                return;
            }
        }
        let _ = tx.send(Event::Gone);
    })?;
    let writer = Arc::new(Mutex::new(stream));
    let send = |msg: ClientMsg| -> Result<()> {
        proto::send(&mut *writer.lock().unwrap_or_else(|p| p.into_inner()), &msg).context("lost the session")
    };
    send(ClientMsg::Hello { version: proto::VERSION, geometry: front.geometry(), hints: front.hints() })?;
    // The latest frame received and not shown yet (its sequence number is
    // acknowledged once it is), and the last one shown (shown again when the
    // terminal lost images).
    let mut pending: Option<(u64, Frame, bool)> = None;
    let mut last: Option<Frame> = None;
    let reason = loop {
        if let Some((seq, frame, complete)) = &mut pending
            && front.ready()
        {
            match front.present(frame, *complete)? {
                Presented::Shown => {
                    if *seq > 0 && send(ClientMsg::FrameAck(*seq)).is_err() {
                        break ended(&rx);
                    }
                    last = pending.take().map(|p| p.1);
                }
                // Not shown yet (or only its text), but ask for newer frames:
                // the server only sends one once this one is acknowledged;
                // only newer frames have the pages a first frame waits for,
                // and a shell's next echo must not wait for held images.
                Presented::Waiting | Presented::Held if *seq > 0 => {
                    if send(ClientMsg::FrameAck(*seq)).is_err() {
                        break ended(&rx);
                    }
                    *seq = 0;
                }
                Presented::Waiting | Presented::Held => {}
            }
        }
        if pending.is_none() {
            front.idle()?;
        }
        let event = match rx.recv_timeout(front.timeout(Duration::from_secs(3600))) {
            Ok(e) => e,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break None,
        };
        match event {
            Event::Term(ev) => match front.handle(ev)? {
                Input::Terminate => {
                    let _ = send(ClientMsg::Detach);
                    break Some(ExitReason::Detached);
                }
                input @ (Input::Resized(_) | Input::Key(_) | Input::Mouse(_) | Input::Paste(_)) => {
                    let msg = match input {
                        Input::Resized(g) => ClientMsg::Resize(g),
                        Input::Key(k) => ClientMsg::Key(k),
                        Input::Mouse(m) => ClientMsg::Mouse(m),
                        Input::Paste(text) => ClientMsg::Paste(text),
                        _ => unreachable!(),
                    };
                    if send(msg).is_err() {
                        break ended(&rx);
                    }
                }
                Input::Handled(true) => {
                    if pending.is_none() {
                        pending = last.clone().map(|f| (0, f, true));
                    }
                }
                Input::Handled(false) => {}
            },
            Event::Server(ServerMsg::Frame { seq, updates, frame, complete }) => {
                front.apply(updates);
                pending = Some((seq, frame, complete));
            }
            Event::Server(ServerMsg::Effect(effect)) => {
                let w = writer.clone();
                front.perform(effect, move |m| {
                    let _ = proto::send(&mut *w.lock().unwrap_or_else(|p| p.into_inner()), &ClientMsg::Report(m));
                })?;
            }
            Event::Server(ServerMsg::Exit(reason)) => break Some(reason),
            Event::Server(_) => {}
            Event::Gone => break None,
        }
    };
    drop(front);
    Ok(match reason {
        Some(ExitReason::Detached) => format!("[detached from session {name}]"),
        Some(ExitReason::SessionEnded) => format!("[session {name} ended]"),
        Some(ExitReason::Killed) => format!("[session {name} was killed]"),
        // Failures: an error, so the command's exit status says so.
        Some(ExitReason::Error(e)) => bail!("[session {name}: {e}]"),
        None => bail!("[lost session {name}]"),
    })
}

/// Sending failed: the server closed the connection. It usually said why
/// just before (the session ended, or was killed, while this client was
/// sending input or an acknowledgement); wait a moment for that message.
fn ended(rx: &mpsc::Receiver<Event>) -> Option<ExitReason> {
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(Event::Server(ServerMsg::Exit(reason))) => return Some(reason),
            Ok(Event::Gone) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

/// Send a one-shot request and wait for the answer.
/// How long one-shot requests wait for the session by default.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

fn request(name: &str, msg: ClientMsg, timeout: Duration) -> Result<ServerMsg> {
    let socket = proto::socket_path(name)?;
    let mut s = UnixStream::connect(&socket).with_context(|| format!("no session {name:?}"))?;
    s.set_read_timeout(Some(timeout))?;
    proto::send(&mut s, &msg)?;
    match proto::recv(&mut s) {
        Ok(answer) => answer.context("no answer from the session"),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
            bail!(
                "no answer from session {name:?} within {:.1} s (a script may be running there)",
                timeout.as_secs_f64()
            )
        }
        Err(e) => Err(e.into()),
    }
}

pub fn list_sessions(verbose: bool) -> Result<()> {
    let (sessions, other) = daemon::list_all()?;
    if sessions.is_empty() && other.is_empty() {
        println!("no sessions");
    }
    for s in sessions {
        let files: Vec<String> = s.files.iter().map(|f| f.display().to_string()).collect();
        let clients = if s.clients == 0 { "detached".to_owned() } else { format!("{} attached", s.clients) };
        println!("{}  ({} panes, {} buffers, {clients})  {}", s.name, s.panes, s.buffers, files.join(" "));
        if verbose {
            println!("    pid: {}", s.pid);
            if let Some(log) = &s.log {
                println!("    log: {}", log.display());
            }
        }
    }
    for name in other {
        println!("{name}  (another temzdf version; end it with `temzdf kill-session {name}`)");
    }
    Ok(())
}

pub fn kill_session(name: &str) -> Result<()> {
    match request(name, ClientMsg::Kill, REQUEST_TIMEOUT)? {
        ServerMsg::Done(Ok(())) => Ok(()),
        other => bail!("unexpected answer {other:?}"),
    }
}

pub fn kill_all_sessions() -> Result<()> {
    for s in daemon::list()? {
        kill_session(&s.name)?;
    }
    Ok(())
}

/// Run an action in session `name`. The server runs elsewhere: a new pane's
/// file is made absolute, and its terminal starts in this directory.
pub fn action(name: &str, mut words: Vec<String>, timeout: Duration) -> Result<()> {
    resolve_paths(&mut words, &std::env::current_dir()?)?;
    match request(name, ClientMsg::Action(words), timeout)? {
        ServerMsg::Done(Ok(())) => Ok(()),
        ServerMsg::Done(Err(e)) => bail!("{e}"),
        other => bail!("unexpected answer {other:?}"),
    }
}

/// Paths in `temzdf action` words are the caller's: resolve them against
/// `cwd` here (the server has its own working directory) and check that
/// files exist (opening happens later in the server, too late to fail the
/// command). `new-pane` and `open-float [--cwd DIR] [FILE | -- COMMAND...]`
/// get `--cwd` (the caller's directory unless given; relative to it), and
/// FILE is relative to that directory; `edit FILE` is relative to `cwd`.
fn resolve_paths(words: &mut Vec<String>, cwd: &Path) -> Result<()> {
    let existing = |p: PathBuf| -> Result<String> {
        if !p.exists() {
            bail!("{}: no such file", p.display());
        }
        Ok(p.display().to_string())
    };
    match words.first().map(String::as_str) {
        Some("new-pane" | "open-float") => {
            if words.get(1).is_none_or(|w| w != "--cwd") {
                words.splice(1..1, ["--cwd".to_owned(), cwd.display().to_string()]);
            }
            let Some(dir) = words.get(2).map(|d| cwd.join(d)) else { bail!("--cwd: which directory?") };
            words[2] = dir.display().to_string();
            if let Some(file) = words.get_mut(3)
                && !file.starts_with('-')
            {
                *file = existing(dir.join(&*file))?;
            }
        }
        Some("edit") => {
            if let Some(file) = words.get_mut(1) {
                *file = existing(cwd.join(&*file))?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_paths_are_the_callers() {
        let tmp = std::env::temp_dir().join(format!("temzdf-paths-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a.pdf"), "").unwrap();
        std::fs::write(tmp.join("sub/b.pdf"), "").unwrap();
        let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let t = |p: &str| tmp.join(p).display().to_string();
        let mut words = w(&["new-pane", "a.pdf"]);
        resolve_paths(&mut words, &tmp).unwrap();
        let here = tmp.display().to_string();
        assert_eq!(words, [w(&["new-pane", "--cwd"]), vec![here.clone(), t("a.pdf")]].concat());
        // FILE is relative to --cwd DIR, itself relative to the caller.
        let mut words = w(&["open-float", "--cwd", "sub", "b.pdf"]);
        resolve_paths(&mut words, &tmp).unwrap();
        assert_eq!(words, [w(&["open-float", "--cwd"]), vec![t("sub"), t("sub/b.pdf")]].concat());
        let mut words = w(&["open-float", "--", "htop"]);
        resolve_paths(&mut words, &tmp).unwrap();
        assert_eq!(words, [w(&["open-float", "--cwd"]), vec![here], w(&["--", "htop"])].concat());
        let mut words = w(&["edit", "sub/b.pdf"]);
        resolve_paths(&mut words, &tmp).unwrap();
        assert_eq!(words, ["edit".to_owned(), t("sub/b.pdf")]);
        // A missing file fails here, not later in the server.
        let e = resolve_paths(&mut w(&["edit", "missing.pdf"]), &tmp).unwrap_err();
        assert!(e.to_string().contains("no such file"), "{e}");
        assert!(resolve_paths(&mut w(&["new-pane", "missing.pdf"]), &tmp).is_err());
        let mut words = w(&["split", "right"]);
        resolve_paths(&mut words, &tmp).unwrap();
        assert_eq!(words, w(&["split", "right"]));
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
