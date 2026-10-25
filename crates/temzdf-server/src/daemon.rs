//! The session server process: one [`Session`] shown to every attached client
//! over a Unix socket (see `temzdf-protocol`). The latest client to attach or
//! send input decides the session's size (tmux's `window-size latest`); every
//! client gets the same frames, each paced by its own acknowledgements, with
//! image changes queued per client (latest wins) so a slow client never holds
//! up the others and catches up with the current state.

use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use temzdf_protocol::{self as proto, ClientMsg, ExitReason, ServerMsg, SessionInfo};
use temzdf_scene::{Frame, Hints, ImageKey, ImageUpdate, Pointer};
use termpix_core::geom::{CellSize, Geometry};

use crate::action::{Action, PaneSpec};
use crate::session::{EventSink, Outcome, Session, SessionEvent, Settings};

const FRAME: Duration = Duration::from_millis(16);
/// Until a client says otherwise.
const DEFAULT_GEOMETRY: Geometry = Geometry { cols: 80, rows: 24, cell: CellSize { width: 10, height: 20 } };

type ClientId = u64;

enum Event {
    Session(SessionEvent),
    Connected(UnixStream),
    Msg(ClientId, ClientMsg),
    Gone(ClientId),
}

struct Client {
    tx: Sender<ServerMsg>,
    /// Said Hello: shows the session (others make one request and go).
    attached: bool,
    /// Image changes not sent yet (the latest for each image).
    pending: HashMap<ImageKey, ImageUpdate>,
    /// A frame was sent and not acknowledged yet.
    awaiting: bool,
    /// The sequence number of the last frame sent.
    shown: u64,
}

impl Client {
    fn queue(&mut self, updates: &[ImageUpdate]) {
        for u in updates {
            let key = match u {
                ImageUpdate::Set(k, _) | ImageUpdate::Remove(k) => *k,
            };
            self.pending.insert(key, u.clone());
        }
    }
}

/// Is a session with this socket alive? Stale sockets are removed.
pub fn probe(socket: &Path) -> Option<SessionInfo> {
    let info = (|| {
        let mut s = UnixStream::connect(socket).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        proto::send(&mut s, &ClientMsg::Info { version: proto::VERSION }).ok()?;
        match proto::recv::<ServerMsg>(&mut s).ok()?? {
            ServerMsg::Info(info) => Some(info),
            _ => None,
        }
    })();
    if info.is_none() && UnixStream::connect(socket).is_err() {
        let _ = std::fs::remove_file(socket);
    }
    info
}

/// Every live session, by name.
pub fn list() -> Result<Vec<SessionInfo>> {
    let dir = proto::sessions_dir()?;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "sock")
            && let Some(info) = probe(&path)
        {
            out.push(info);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Run the session `name`, starting with a pane showing `first`, until it
/// ends. Returns once the last pane closed or the session was killed.
pub fn run(name: &str, first: &PaneSpec, settings: Settings) -> Result<()> {
    if !proto::valid_name(name) {
        bail!("invalid session name {name:?}");
    }
    let socket = proto::socket_path(name)?;
    if probe(&socket).is_some() {
        bail!("session {name:?} already exists");
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).with_context(|| format!("cannot listen on {}", socket.display()))?;
    let _cleanup = RemoveOnDrop(socket.clone());
    eprintln!("temzdf server: session {name} on {} (pid {})", socket.display(), std::process::id());

    let (tx, rx) = mpsc::channel::<Event>();
    let events: EventSink = {
        let tx = Mutex::new(tx.clone());
        Arc::new(move |e| tx.lock().unwrap_or_else(|p| p.into_inner()).send(Event::Session(e)).is_ok())
    };
    {
        let tx = tx.clone();
        std::thread::Builder::new().name("temzdf-accept".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                if tx.send(Event::Connected(stream)).is_err() {
                    return;
                }
            }
        })?;
    }
    let mut session = Session::new(name.to_owned(), DEFAULT_GEOMETRY, Hints::default(), settings, events);
    session.new_pane(first, None)?;
    let mut clients: HashMap<ClientId, Client> = HashMap::new();
    let mut next_client: ClientId = 1;
    let mut latest: Option<ClientId> = None;
    let mut last_frame: Option<(u64, Frame, bool)> = None;
    let mut seq = 0u64;
    let mut dirty = true;
    let mut shown_pointer = Pointer::Default;
    // When the last frame was made: busy terminals get at most one per FRAME.
    let mut made = Instant::now() - FRAME;

    loop {
        // A new frame when something changed and a client can take it; clients
        // still busy with an earlier one get the latest when they acknowledge.
        if !session.is_starting() && session.error().is_none() {
            if dirty && made.elapsed() >= FRAME && clients.values().any(|c| c.attached && !c.awaiting) {
                made = Instant::now();
                let frame = session.frame();
                let updates = session.images().take_updates();
                for c in clients.values_mut().filter(|c| c.attached) {
                    c.queue(&updates);
                }
                seq += 1;
                shown_pointer = frame.pointer;
                last_frame = Some((seq, frame, session.is_complete()));
                dirty = false;
            } else if !clients.values().any(|c| c.attached) {
                // Nobody to send them to; attaching clients get a snapshot.
                session.images().take_updates();
            }
            if let Some((seq, frame, complete)) = &last_frame {
                for c in clients.values_mut().filter(|c| c.attached && !c.awaiting && c.shown < *seq) {
                    let updates = c.pending.drain().map(|(_, u)| u).collect();
                    let msg = ServerMsg::Frame { seq: *seq, updates, frame: frame.clone(), complete: *complete };
                    c.awaiting = c.tx.send(msg).is_ok();
                    c.shown = *seq;
                }
            }
        }
        for effect in session.take_effects() {
            if let Some(c) = latest.and_then(|id| clients.get(&id)) {
                let _ = c.tx.send(ServerMsg::Effect(effect));
            }
        }
        let mut timeout = if session.is_animating() { FRAME } else { Duration::from_secs(3600) };
        if dirty {
            timeout = timeout.min(FRAME.saturating_sub(made.elapsed()));
        }
        if let Some(due) = session.next_deadline() {
            timeout = timeout.min(due.saturating_duration_since(Instant::now()));
        }
        let event = match rx.recv_timeout(timeout) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let now = Instant::now();
        let mut outcome = Outcome::None;
        match event {
            None => {}
            Some(Event::Session(ev)) => outcome = session.handle(ev, now),
            Some(Event::Connected(stream)) => {
                let id = next_client;
                next_client += 1;
                start_reader(id, &stream, tx.clone())?;
                let (ctx, crx) = mpsc::channel::<ServerMsg>();
                start_writer(stream, crx)?;
                clients.insert(
                    id,
                    Client { tx: ctx, attached: false, pending: HashMap::new(), awaiting: false, shown: 0 },
                );
            }
            Some(Event::Gone(id)) => {
                clients.remove(&id);
            }
            Some(Event::Msg(id, msg)) => match msg {
                ClientMsg::Hello { version, geometry, hints } => {
                    let Some(c) = clients.get_mut(&id) else { continue };
                    if version != proto::VERSION {
                        let why = format!("client version {version}, server version {}", proto::VERSION);
                        let _ = c.tx.send(ServerMsg::Exit(ExitReason::Error(why)));
                        clients.remove(&id);
                        continue;
                    }
                    c.attached = true;
                    c.pending = session.images().snapshot().into_iter().map(|u| (key_of(&u), u)).collect();
                    become_latest(&mut session, &mut latest, id, Some((geometry, Some(hints))));
                    dirty = true;
                }
                ClientMsg::Key(k) => {
                    become_latest(&mut session, &mut latest, id, None);
                    outcome = session.key(&k, now);
                    if outcome == Outcome::Detach {
                        outcome = detach(&mut clients, id);
                    }
                }
                ClientMsg::Mouse(m) => {
                    outcome = session.mouse(&m, now);
                    dirty |= session.pointer() != shown_pointer;
                }
                ClientMsg::Paste(text) => outcome = session.paste(&text),
                ClientMsg::Resize(g) => {
                    become_latest(&mut session, &mut latest, id, Some((g, None)));
                    dirty = true;
                }
                ClientMsg::FrameAck(_) => {
                    if let Some(c) = clients.get_mut(&id) {
                        c.awaiting = false;
                    }
                }
                ClientMsg::Report(m) => {
                    session.report(m);
                    dirty = true;
                }
                ClientMsg::Detach => outcome = detach(&mut clients, id),
                ClientMsg::Info { .. } => {
                    if let Some(c) = clients.remove(&id) {
                        let info = SessionInfo {
                            version: proto::VERSION,
                            name: name.to_owned(),
                            pid: std::process::id(),
                            panes: session.pane_count(),
                            clients: clients.values().filter(|c| c.attached).count(),
                            files: session.files(),
                        };
                        let _ = c.tx.send(ServerMsg::Info(info));
                    }
                }
                ClientMsg::Kill => {
                    if let Some(c) = clients.remove(&id) {
                        let _ = c.tx.send(ServerMsg::Done(Ok(())));
                    }
                    for c in clients.values() {
                        let _ = c.tx.send(ServerMsg::Exit(ExitReason::Killed));
                    }
                    break;
                }
                ClientMsg::Action(words) => {
                    let result = Action::parse(&words).and_then(|a| session.action(a, now));
                    let action_outcome = *result.as_ref().unwrap_or(&Outcome::Redraw);
                    let result = result.map(|_| ()).map_err(|e| format!("{e:#}"));
                    if let Some(c) = clients.remove(&id) {
                        let _ = c.tx.send(ServerMsg::Done(result));
                    }
                    outcome = match action_outcome {
                        Outcome::Detach => {
                            for c in clients.values() {
                                let _ = c.tx.send(ServerMsg::Exit(ExitReason::Detached));
                            }
                            clients.clear();
                            Outcome::Redraw
                        }
                        o => o,
                    };
                }
            },
        }
        match outcome {
            Outcome::Quit => {
                if let Some(e) = session.error() {
                    eprintln!("temzdf server: {e}");
                }
                for c in clients.values() {
                    let _ = c.tx.send(ServerMsg::Exit(ExitReason::SessionEnded));
                }
                break;
            }
            Outcome::Redraw | Outcome::Detach => dirty = true,
            Outcome::None => {}
        }
        dirty |= session.tick(Instant::now());
    }
    // Let writer threads deliver the last messages.
    drop(clients);
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

/// Input or a new size makes a client the latest: the session follows its
/// size (and frontend).
fn become_latest(
    session: &mut Session,
    latest: &mut Option<ClientId>,
    id: ClientId,
    geometry: Option<(Geometry, Option<Hints>)>,
) {
    if let Some((g, hints)) = geometry
        && (*latest != Some(id) || g != session.geometry() || hints.is_some())
    {
        session.resize(g);
        if let Some(h) = hints {
            session.set_hints(h);
        }
    }
    *latest = Some(id);
}

fn key_of(u: &ImageUpdate) -> ImageKey {
    match u {
        ImageUpdate::Set(k, _) | ImageUpdate::Remove(k) => *k,
    }
}

/// The client `id` detaches (its key press asked to).
fn detach(clients: &mut HashMap<ClientId, Client>, id: ClientId) -> Outcome {
    if let Some(c) = clients.remove(&id) {
        let _ = c.tx.send(ServerMsg::Exit(ExitReason::Detached));
    }
    Outcome::Redraw
}

fn start_reader(id: ClientId, stream: &UnixStream, tx: Sender<Event>) -> Result<()> {
    let mut r = BufReader::new(stream.try_clone()?);
    std::thread::Builder::new().name(format!("temzdf-client-{id}")).spawn(move || {
        loop {
            match proto::recv::<ClientMsg>(&mut r) {
                Ok(Some(msg)) => {
                    if tx.send(Event::Msg(id, msg)).is_err() {
                        return;
                    }
                }
                _ => {
                    let _ = tx.send(Event::Gone(id));
                    return;
                }
            }
        }
    })?;
    Ok(())
}

fn start_writer(mut stream: UnixStream, rx: mpsc::Receiver<ServerMsg>) -> Result<()> {
    std::thread::Builder::new().name("temzdf-client-out".into()).spawn(move || {
        for msg in rx {
            let exit = matches!(msg, ServerMsg::Exit(_) | ServerMsg::Info(_) | ServerMsg::Done(_));
            if proto::send(&mut stream, &msg).is_err() || exit {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return;
            }
        }
    })?;
    Ok(())
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
