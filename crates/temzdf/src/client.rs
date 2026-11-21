//! Session commands: start a session server, attach this terminal to a
//! session, list and end sessions, run actions (see doc/design/session-server.md).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
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

impl Event {
    fn from_server(msg: FromServer) -> Self {
        match msg {
            FromServer::Msg(m) => Event::Server(*m),
            FromServer::Gone => Event::Gone,
        }
    }
}

use temzdf_client::{Connection, FromServer, REQUEST_TIMEOUT, exit_message, lost_message, utf8};
pub use temzdf_client::{auto_name, first_pane, name_for, request, resolve};

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

/// Fail unless there is a terminal to attach from (checked before starting
/// a session: one started and never attached would stay behind, another on
/// each try, e.g. from an editor's background job).
pub fn need_terminal() -> Result<()> {
    std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").map(drop).map_err(|e| {
        anyhow::anyhow!(
            "no terminal to show the session in ({e}): start temzdf in a terminal (or temzdf-gui); \
             --forward from an editor reaches a session that shows the PDF already"
        )
    })
}

/// Whether attaching session `target` from a pane of session `own` would
/// show `target` inside itself: `own` is `target`, or `target` shows `own`,
/// directly or through the sessions in between (`shown_in` gives the
/// sessions showing a session).
fn shows_itself(target: &str, own: &str, shown_in: impl Fn(&str) -> Vec<String>) -> bool {
    let mut seen = vec![own.to_owned()];
    let mut i = 0;
    while let Some(s) = seen.get(i).cloned() {
        if s == target {
            return true;
        }
        for outer in shown_in(&s) {
            if !seen.contains(&outer) {
                seen.push(outer);
            }
        }
        i += 1;
    }
    false
}

/// Attach this terminal to session `name` until detached or the session ends.
/// Returns a line to print once the terminal is restored.
pub fn attach(name: &str) -> Result<String> {
    // From one of the session's own panes, or from a session it shows: it
    // would show itself inside itself, each frame feeding the next.
    let inside = temzdf_client::current_session();
    let shown_in = |s: &str| {
        proto::socket_path(s).ok().and_then(|p| proto::discovery::probe(&p)).map(|i| i.shown_in).unwrap_or_default()
    };
    if let Some(own) = &inside
        && shows_itself(name, own, shown_in)
    {
        bail!("this terminal is in session {own:?}, which {name:?} shows: attaching would show it inside itself");
    }
    let _stdio = crate::app::log_path().and_then(|p| termpix::tty::StdioRedirect::to_file(&p).ok());
    let mut front = crate::app::open_frontend()?;
    let (tx, rx) = mpsc::channel::<Event>();
    front.spawn_input(tx.clone(), Event::Term)?;
    let conn = Connection::open(name, tx, Event::from_server, || {})?;
    let send = |msg: ClientMsg| conn.send(&msg);
    conn.hello(front.geometry(), front.hints(), inside).with_context(|| lost_message(name))?;
    // The latest frame received and not shown yet (its sequence number is
    // acknowledged once it is), and the last one shown (shown again when the
    // terminal lost images).
    let mut pending: Option<(u64, Arc<Frame>, bool)> = None;
    let mut last: Option<Arc<Frame>> = None;
    let reason = loop {
        if let Some((seq, frame, complete)) = &mut pending
            && front.ready()
        {
            let start = std::time::Instant::now();
            let presented = front.present(frame, *complete)?;
            if proto::timing() && *seq > 0 {
                eprintln!("timing: presented frame {seq} ({presented:?}) in {:.2} ms", proto::ms(start.elapsed()));
            }
            match presented {
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
            // One arm per input: a new variant must say what it does here.
            Event::Term(ev) => {
                let msg = match front.handle(ev)? {
                    Input::Terminate => {
                        let _ = send(ClientMsg::Detach);
                        break Some(ExitReason::Detached);
                    }
                    Input::Resized(g) => Some(ClientMsg::Resize(g)),
                    Input::Key(k) => Some(ClientMsg::Key(k)),
                    Input::Mouse(m) => Some(ClientMsg::Mouse(m)),
                    Input::Paste(text) => Some(ClientMsg::Paste(text)),
                    Input::Focus(gained) => Some(ClientMsg::Focus(gained)),
                    Input::Clipboard { ticket, result } => Some(ClientMsg::Clipboard { ticket, result }),
                    Input::Handled(redraw) => {
                        if redraw && pending.is_none() {
                            pending = last.clone().map(|f| (0, f, true));
                        }
                        None
                    }
                };
                if let Some(msg) = msg
                    && send(msg).is_err()
                {
                    break ended(&rx);
                }
            }
            Event::Server(ServerMsg::Frame { seq, updates, frame, complete, .. }) => {
                front.apply(updates);
                pending = Some((seq, frame, complete));
            }
            Event::Server(ServerMsg::Effect(effect)) => {
                let c = conn.clone();
                front.perform(effect, move |m| {
                    let _ = c.send(&ClientMsg::Report(m));
                })?;
            }
            Event::Server(ServerMsg::Exit(reason)) => break Some(reason),
            Event::Server(_) => {}
            Event::Gone => break None,
        }
    };
    drop(front);
    match reason {
        // Failures: an error, so the command's exit status says so.
        Some(reason @ ExitReason::Error(_)) => bail!("{}", exit_message(&reason, name)),
        Some(reason) => Ok(exit_message(&reason, name)),
        None => bail!("{}", lost_message(name)),
    }
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

pub fn list_sessions(verbose: bool) -> Result<()> {
    let daemon::Listing { sessions, other, silent } = daemon::list_all()?;
    if sessions.is_empty() && other.is_empty() && silent.is_empty() {
        println!("no sessions");
    }
    // The session whose pane this runs in, marked as zellij does.
    let current = temzdf_client::current_session();
    for s in sessions {
        let files: Vec<String> = s.files.iter().map(|f| f.display().to_string()).collect();
        let clients = if s.clients == 0 { "detached".to_owned() } else { format!("{} attached", s.clients) };
        let tabs = if s.tabs > 1 { format!("{} tabs, ", s.tabs) } else { String::new() };
        let mut line =
            format!("{}  ({tabs}{} panes, {} buffers, {clients})  {}", s.name, s.panes, s.buffers, files.join(" "));
        if current.as_deref() == Some(s.name.as_str()) {
            line = format!("{} (current)", line.trim_end());
        }
        println!("{line}");
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
    for name in silent {
        println!("{name}  (not answering: busy)");
    }
    Ok(())
}

/// End session `name`: as its `kill-session` command, which its
/// configuration's SessionEndPre hooks may refuse (unsaved changes), or
/// with `force` at once.
pub fn kill_session(name: &str, force: bool) -> Result<()> {
    if !force {
        return action(name, vec!["kill-session".into()], REQUEST_TIMEOUT)
            .map_err(|e| anyhow::anyhow!("{e:#} (kill-session --force ends it anyway)"));
    }
    match request(name, ClientMsg::Kill, REQUEST_TIMEOUT)? {
        ServerMsg::Done(Ok(())) => Ok(()),
        other => bail!("unexpected answer {other:?}"),
    }
}

/// End every session, those of other versions too when they still take the
/// request; one that fails does not stop the others.
pub fn kill_all_sessions(force: bool) -> Result<()> {
    let daemon::Listing { sessions, other, silent } = daemon::list_all()?;
    let mut failed = Vec::new();
    // Another version's sessions only take the plain request. One that did
    // not answer in time is busy, not another version: asked as asked (its
    // SessionEndPre hooks may keep unsaved text).
    let sessions = sessions
        .into_iter()
        .map(|s| (s.name, force))
        .chain(silent.into_iter().map(|n| (n, force)))
        .chain(other.into_iter().map(|n| (n, true)));
    for (name, force) in sessions {
        if let Err(e) = kill_session(&name, force) {
            failed.push(format!("{name}: {e:#}"));
        }
    }
    if !failed.is_empty() {
        bail!("some sessions did not end:\n{}", failed.join("\n"));
    }
    Ok(())
}

/// `temzdf copy`: record `text` in session `name`'s copy history.
pub fn copy(name: &str, text: String, register: Option<String>, line: bool) -> Result<()> {
    let kind = Some(if line { temzdf_scene::CopyKind::Line } else { temzdf_scene::CopyKind::Char });
    match request(name, ClientMsg::Copy { text, register, kind }, REQUEST_TIMEOUT)? {
        ServerMsg::Done(Ok(())) => Ok(()),
        ServerMsg::Done(Err(e)) => bail!("{e}"),
        other => bail!("unexpected answer {other:?}"),
    }
}

/// `temzdf paste`: an entry of session `name`'s copy history.
pub fn paste(name: &str, register: Option<String>, index: usize) -> Result<String> {
    match request(name, ClientMsg::GetCopy { register, index }, REQUEST_TIMEOUT)? {
        ServerMsg::Copied(Ok(text)) => Ok(text),
        ServerMsg::Copied(Err(e)) => bail!("{e}"),
        other => bail!("unexpected answer {other:?}"),
    }
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
/// PDFs exist (a missing text file is a new one; opening happens later in
/// the server, too late to fail the
/// command). `new-pane`, `open-float` and `new-tab` `[--cwd DIR] [FILE | -- COMMAND...]`
/// get `--cwd` (the caller's directory unless given; relative to it), and
/// FILE is relative to that directory; `edit FILE` is relative to `cwd`.
fn resolve_paths(words: &mut Vec<String>, cwd: &Path) -> Result<()> {
    let existing = |p: PathBuf| -> Result<String> {
        if temzdf_client::must_exist(&p) {
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
    fn sessions_do_not_show_themselves_through_others() {
        // a shows b (a client of b runs in a pane of a), b shows c.
        let shown_in = |s: &str| match s {
            "b" => vec!["a".to_owned()],
            "c" => vec!["b".to_owned(), "c".to_owned()],
            _ => vec![],
        };
        assert!(shows_itself("c", "c", shown_in), "its own pane");
        assert!(shows_itself("a", "c", shown_in), "from c, a shows c through b");
        assert!(shows_itself("b", "c", shown_in));
        assert!(!shows_itself("c", "a", shown_in), "a shows c: c may not show a, a may show c");
        assert!(!shows_itself("d", "c", shown_in));
    }

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
        // A missing text file is a new one.
        let mut words = w(&["new-pane", "new.txt"]);
        resolve_paths(&mut words, &tmp).unwrap();
        assert_eq!(words[3], t("new.txt"));
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
