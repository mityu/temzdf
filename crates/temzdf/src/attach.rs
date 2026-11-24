//! Attaching this terminal to a session (the terminal frontend; on Windows
//! the console, see `termpix::tty`).

use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use temzdf_client::{Connection, FromServer, exit_message, lost_message};
use temzdf_protocol::{self as proto, ClientMsg, ExitReason, ServerMsg};
use temzdf_scene::Frame;
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

/// The zlib level for inline image transfers: `$TEMZDF_ZLIB_LEVEL`, else
/// termpix's default (do not use 1 with Ghostty 1.3.1, see
/// doc/terminals/terminal-compat.md).
pub fn zlib_level() -> u32 {
    std::env::var("TEMZDF_ZLIB_LEVEL")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .map_or(termpix::graphics::DEFAULT_ZLIB_LEVEL, |l| l.min(9))
}

/// Open this terminal as a frontend: what detection found (and the
/// `TERMPIX_BACKEND`/`TERMPIX_MEDIUM` overrides).
pub fn open_frontend() -> Result<temzdf_tty::TtyFrontend> {
    temzdf_tty::TtyFrontend::open(|caps| (caps.backend(), caps.medium(), zlib_level()))
}

/// Fail unless there is a terminal to attach from (checked before starting
/// a session: one started and never attached would stay behind, another on
/// each try, e.g. from an editor's background job). On Windows: a console
/// (a program started without one, detached, has none).
pub fn need_terminal() -> Result<()> {
    let tty = if cfg!(windows) { "CONOUT$" } else { "/dev/tty" };
    std::fs::OpenOptions::new().read(true).write(true).open(tty).map(drop).map_err(|e| {
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
    let _stdio = temzdf_client::log_path().and_then(|p| termpix::tty::StdioRedirect::to_file(&p).ok());
    let mut front = open_frontend()?;
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
            // The terminal holds the clipboard and pastes with its own key:
            // the answer says so (and lets the input the session holds go).
            Event::Server(ServerMsg::Effect(temzdf_scene::Effect::Paste { ticket, .. })) => {
                let why = "in a terminal, paste with the terminal's own key (Ctrl+Shift+V, Cmd+V)";
                let _ = conn.send(&ClientMsg::Clipboard { ticket, result: Err(why.into()) });
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
}
