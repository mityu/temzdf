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

use temzdf_client::utf8;
pub use temzdf_client::{auto_name, first_pane, name_for, resolve};

/// Start a server for session `name` (see [`temzdf_client::spawn_server`])
/// with this executable.
pub fn spawn_server(
    name: &str,
    pane: &[String],
    config_path: Option<&Path>,
    editor: Option<&str>,
    clean: bool,
) -> Result<()> {
    temzdf_client::spawn_server(&std::env::current_exe()?, name, pane, config_path, editor, clean)
}

/// Attach this terminal to session `name` until detached or the session ends.
/// Returns a line to print once the terminal is restored.
pub fn attach(name: &str) -> Result<String> {
    // From one of the session's own panes: it would show itself inside
    // itself, each frame feeding the next.
    if std::env::var("TEMZDF_SESSION_NAME").is_ok_and(|own| own == name) {
        bail!("this terminal is in session {name:?}: attaching would show it inside itself");
    }
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
                input @ (Input::Resized(_) | Input::Key(_) | Input::Mouse(_) | Input::Paste(_) | Input::Focus(_)) => {
                    let msg = match input {
                        Input::Resized(g) => ClientMsg::Resize(g),
                        Input::Key(k) => ClientMsg::Key(k),
                        Input::Mouse(m) => ClientMsg::Mouse(m),
                        Input::Paste(text) => ClientMsg::Paste(text),
                        Input::Focus(gained) => ClientMsg::Focus(gained),
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
        let tabs = if s.tabs > 1 { format!("{} tabs, ", s.tabs) } else { String::new() };
        println!("{}  ({tabs}{} panes, {} buffers, {clients})  {}", s.name, s.panes, s.buffers, files.join(" "));
        if verbose {
            println!("    pid: {}", s.pid);
            if let Some(log) = &s.log {
                println!("    log: {}", log.display());
            }
        }
    }
    for name in other {
        println!("{name}  (another temzdf version; end it with that version's `temzdf kill-session {name}`)");
    }
    Ok(())
}

pub fn kill_session(name: &str) -> Result<()> {
    match request(name, ClientMsg::Kill, REQUEST_TIMEOUT)? {
        ServerMsg::Done(Ok(())) => Ok(()),
        other => bail!("unexpected answer {other:?}"),
    }
}

/// End every session, those of other versions too when they still take the
/// request; one that fails does not stop the others.
pub fn kill_all_sessions() -> Result<()> {
    let (sessions, other) = daemon::list_all()?;
    let mut failed = Vec::new();
    for name in sessions.into_iter().map(|s| s.name).chain(other) {
        if let Err(e) = kill_session(&name) {
            failed.push(format!("{name}: {e:#}"));
        }
    }
    if !failed.is_empty() {
        bail!("some sessions did not end:\n{}", failed.join("\n"));
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
/// command). `new-pane`, `open-float` and `new-tab` `[--cwd DIR] [FILE | -- COMMAND...]`
/// get `--cwd` (the caller's directory unless given; relative to it), and
/// FILE is relative to that directory; `edit FILE` is relative to `cwd`.
fn resolve_paths(words: &mut Vec<String>, cwd: &Path) -> Result<()> {
    let existing = |p: PathBuf| -> Result<String> {
        if !p.exists() {
            bail!("{}: no such file", p.display());
        }
        utf8(&p)
    };
    match words.first().map(String::as_str) {
        Some("new-pane" | "open-float" | "new-tab") => {
            if words.get(1).is_none_or(|w| w != "--cwd") {
                words.splice(1..1, ["--cwd".to_owned(), utf8(cwd)?]);
            }
            let Some(dir) = words.get(2).map(|d| cwd.join(d)) else { bail!("--cwd: which directory?") };
            words[2] = utf8(&dir)?;
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
        // A path that is not UTF-8 is refused, not mangled.
        use std::os::unix::ffi::OsStrExt;
        let odd = std::ffi::OsStr::from_bytes(b"/tmp/\xff.pdf");
        assert!(utf8(Path::new(odd)).is_err());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
