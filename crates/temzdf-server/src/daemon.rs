//! The session server process: one [`Session`] shown to every attached client
//! over a Unix socket (see `temzdf-protocol`). The latest client to attach or
//! send input decides the session's size (tmux's `window-size latest`); every
//! client gets the same frames, each paced by its own acknowledgements, with
//! image changes queued per client (latest wins) so a slow client never holds
//! up the others and catches up with the current state.

use std::collections::{HashMap, VecDeque};
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

use crate::action::PaneSpec;
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
    /// Its terminal's size and frontend, as last reported: they apply
    /// whenever it becomes the latest client.
    geometry: Option<Geometry>,
    hints: Option<Hints>,
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
    Ok(list_all()?.0)
}

/// Every live session, and the names of live sessions that did not answer in
/// a form this version understands (servers of another temzdf version).
pub fn list_all() -> Result<(Vec<SessionInfo>, Vec<String>)> {
    let dir = proto::sessions_dir()?;
    let mut out = Vec::new();
    let mut other = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "sock") {
            continue;
        }
        match probe(&path) {
            Some(info) => out.push(info),
            // probe removed it if nothing listens.
            None if path.exists() => {
                other.extend(path.file_stem().map(|n| n.to_string_lossy().into_owned()));
            }
            None => {}
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    other.sort();
    Ok((out, other))
}

/// Run the session `name`, starting with a pane showing `first`, until it
/// ends. Returns once the last pane closed or the session was killed.
/// `log` is where the server's output goes (reported to `temzdf ls`).
pub fn run(name: &str, first: &PaneSpec, log: Option<PathBuf>, settings: Settings) -> Result<()> {
    if !proto::valid_name(name) {
        bail!("invalid session name {name:?}");
    }
    let socket = proto::socket_path(name)?;
    let (_lock, listener, _cleanup) = claim_socket(name, &socket)?;
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
    session.start(first.clone(), None)?;
    let mut clients: HashMap<ClientId, Client> = HashMap::new();
    let mut next_client: ClientId = 1;
    let mut latest: Option<ClientId> = None;
    let mut last_frame: Option<(u64, Frame, bool)> = None;
    let mut seq = 0u64;
    let mut dirty = true;
    let mut shown_pointer = Pointer::Default;
    // `temzdf action` requests that arrived while a script ran.
    let mut queued: VecDeque<(ClientId, Vec<String>)> = VecDeque::new();
    // When the last frame was made: busy terminals get at most one per FRAME.
    let mut made = Instant::now() - FRAME;
    let mut socket_checked = Instant::now();

    loop {
        // A session nobody can reach (its socket removed, or replaced by
        // another server's) ends: tests' temporary directories go, and
        // orphans must not live on.
        if socket_checked.elapsed() >= SOCKET_CHECK {
            socket_checked = Instant::now();
            if !_cleanup.still_ours() {
                eprintln!("temzdf server: the socket is gone; ending session {name}");
                for c in clients.values() {
                    let _ = c.tx.send(ServerMsg::Exit(ExitReason::SessionEnded));
                }
                break;
            }
        }
        // A new frame when something changed and a client can take it; clients
        // still busy with an earlier one get the latest when they acknowledge.
        if !session.is_starting() && session.error().is_none() {
            if dirty
                && made.elapsed() >= FRAME
                && !session.holds_frames()
                && clients.values().any(|c| c.attached && !c.awaiting)
            {
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
        let can_send = clients.values().any(|c| c.attached && !c.awaiting) && !session.holds_frames();
        let timeout = wait_for(dirty, session.is_animating(), can_send, made, session.next_deadline(), Instant::now())
            .min(SOCKET_CHECK);
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
                    Client {
                        tx: ctx,
                        attached: false,
                        pending: HashMap::new(),
                        awaiting: false,
                        shown: 0,
                        geometry: None,
                        hints: None,
                    },
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
                    (c.geometry, c.hints) = (usable(geometry), Some(hints));
                    become_latest(&mut session, &mut latest, &clients, id);
                    dirty = true;
                }
                ClientMsg::Key(k) => {
                    dirty |= become_latest(&mut session, &mut latest, &clients, id);
                    outcome = session.key(&k, now);
                    if outcome == Outcome::Detach {
                        outcome = detach(&mut clients, id);
                    }
                }
                ClientMsg::Mouse(m) => {
                    dirty |= become_latest(&mut session, &mut latest, &clients, id);
                    outcome = session.mouse(&m, now);
                    dirty |= session.pointer() != shown_pointer;
                }
                ClientMsg::Paste(text) => {
                    dirty |= become_latest(&mut session, &mut latest, &clients, id);
                    outcome = session.paste(&text);
                }
                ClientMsg::Resize(g) => {
                    if let Some(c) = clients.get_mut(&id)
                        && let Some(g) = usable(g)
                    {
                        c.geometry = Some(g);
                    }
                    become_latest(&mut session, &mut latest, &clients, id);
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
                            tabs: session.tab_count(),
                            panes: session.pane_count(),
                            buffers: session.buffer_count(),
                            clients: clients.values().filter(|c| c.attached).count(),
                            files: session.files(),
                            log: log.clone(),
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
                    // Scripts run synchronously: actions wait, like input.
                    if session.holds_input() {
                        queued.push_back((id, words));
                    } else {
                        outcome = run_action(&mut session, &mut clients, id, &words, now);
                    }
                }
            },
        }
        while !session.holds_input()
            && let Some((id, words)) = queued.pop_front()
        {
            outcome = outcome.merge(run_action(&mut session, &mut clients, id, &words, Instant::now()));
        }
        outcome = outcome.merge(session.poll(Instant::now()));
        // The latest client left (went, detached): another takes over.
        dirty |= hand_over(&mut session, &mut latest, &clients);
        match outcome {
            Outcome::Quit => {
                // The first pane could not open: the clients say why.
                let reason = match session.error() {
                    Some(e) => {
                        eprintln!("temzdf server: {e}");
                        ExitReason::Error(e.to_owned())
                    }
                    None => ExitReason::SessionEnded,
                };
                for c in clients.values() {
                    let _ = c.tx.send(ServerMsg::Exit(reason.clone()));
                }
                break;
            }
            // A script asked to detach: the operating client goes.
            Outcome::Detach => {
                if let Some(id) = latest {
                    detach(&mut clients, id);
                }
                dirty = true;
            }
            Outcome::Redraw => dirty = true,
            Outcome::None => {}
        }
    }
    // Let writer threads deliver the last messages.
    drop(clients);
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

/// How often the server checks that its socket is still there and its own.
const SOCKET_CHECK: Duration = Duration::from_secs(2);

/// How long a new connection has to send its first message.
const FIRST_MESSAGE_WITHIN: Duration = Duration::from_secs(10);

/// How long the main loop may wait for events. A pending frame (`dirty`)
/// or an animation only shortens the wait while a client can take a frame:
/// otherwise the next frame waits for an event anyway (an acknowledgement,
/// a client attaching), and waking up for it would spin.
fn wait_for(
    dirty: bool,
    animating: bool,
    can_send: bool,
    made: Instant,
    deadline: Option<Instant>,
    now: Instant,
) -> Duration {
    let mut timeout = if animating && can_send { FRAME } else { Duration::from_secs(3600) };
    if dirty && can_send {
        timeout = timeout.min(FRAME.saturating_sub(now.saturating_duration_since(made)));
    }
    if let Some(due) = deadline {
        timeout = timeout.min(due.saturating_duration_since(now));
    }
    timeout
}

/// Run `temzdf action` words for client `id` and answer it.
fn run_action(
    session: &mut Session,
    clients: &mut HashMap<ClientId, Client>,
    id: ClientId,
    words: &[String],
    now: Instant,
) -> Outcome {
    let _ = now;
    let result = session.run_cli(words);
    let action_outcome = *result.as_ref().unwrap_or(&Outcome::Redraw);
    let result = result.map(|_| ()).map_err(|e| format!("{e:#}"));
    if let Some(c) = clients.remove(&id) {
        let _ = c.tx.send(ServerMsg::Done(result));
    }
    // A detach detaches the operating (latest) client, as from a mapping.
    action_outcome
}

/// A client's terminal size, if it is one to lay panes out in (a client
/// reporting no rows, columns or cell size is ignored).
fn usable(g: Geometry) -> Option<Geometry> {
    (g.cols > 0 && g.rows > 0 && g.cell.width > 0 && g.cell.height > 0).then_some(g)
}

/// Client `id` drives the session now (it attached, typed, pointed or
/// resized): the session takes its size, and its frontend's hints when it
/// was not the latest before. Returns whether anything changed.
fn become_latest(
    session: &mut Session,
    latest: &mut Option<ClientId>,
    clients: &HashMap<ClientId, Client>,
    id: ClientId,
) -> bool {
    let Some(c) = clients.get(&id).filter(|c| c.attached) else { return false };
    let new = *latest != Some(id);
    let mut changed = new;
    if let Some(g) = c.geometry
        && g != session.geometry()
    {
        session.resize(g);
        changed = true;
    }
    if new && let Some(h) = c.hints {
        session.set_hints(h);
    }
    *latest = Some(id);
    changed
}

/// The latest client is gone: the attached client that came last takes
/// over (its size; effects such as copies go to it). Returns whether
/// anything changed.
fn hand_over(session: &mut Session, latest: &mut Option<ClientId>, clients: &HashMap<ClientId, Client>) -> bool {
    if latest.is_some_and(|id| clients.get(&id).is_some_and(|c| c.attached)) {
        return false;
    }
    *latest = None;
    match clients.iter().filter(|(_, c)| c.attached).map(|(id, _)| *id).max() {
        Some(id) => become_latest(session, latest, clients, id),
        None => false,
    }
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
    // A connection that says nothing goes after a while (each holds a
    // thread); once it spoke, it may stay quiet as long as it likes.
    stream.set_read_timeout(Some(FIRST_MESSAGE_WITHIN))?;
    let quiet = stream.try_clone()?;
    let mut r = BufReader::new(stream.try_clone()?);
    std::thread::Builder::new().name(format!("temzdf-client-{id}")).spawn(move || {
        loop {
            match proto::recv::<ClientMsg>(&mut r) {
                Ok(Some(msg)) => {
                    let _ = quiet.set_read_timeout(None);
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

/// Take the socket of session `name`: lock the session (`NAME.lock`, held
/// for the server's life, so two servers started at once cannot both
/// think the name free and unlink each other's socket), refuse a socket
/// something answers on (a live session, maybe of another version), and
/// replace only a stale one.
fn claim_socket(name: &str, socket: &Path) -> Result<(std::fs::File, UnixListener, RemoveOnDrop)> {
    let lock_path = socket.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("cannot open {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => bail!("session {name:?} already exists (or is starting)"),
        Err(std::fs::TryLockError::Error(e)) => return Err(e).context(format!("cannot lock {}", lock_path.display())),
    }
    if UnixStream::connect(socket).is_ok() {
        if probe(socket).is_some() {
            bail!("session {name:?} already exists");
        }
        bail!("a session {name:?} of another temzdf version is running (end it with that version)");
    }
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).with_context(|| format!("cannot listen on {}", socket.display()))?;
    let cleanup = RemoveOnDrop::new(socket);
    Ok((lock, listener, cleanup))
}

/// Removes the socket when the server ends, unless another one replaced it
/// meanwhile (a server that lost its socket must not take the new one's).
#[derive(Debug)]
struct RemoveOnDrop {
    path: PathBuf,
    id: Option<(u64, u64)>,
}

impl RemoveOnDrop {
    fn new(path: &Path) -> Self {
        Self { path: path.to_owned(), id: file_id(path) }
    }

    /// The socket is still there, and the one this server bound.
    fn still_ours(&self) -> bool {
        self.id.is_some() && file_id(&self.path) == self.id
    }
}

fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if self.id.is_some() && file_id(&self.path) == self.id {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore = "Miri has no Unix sockets")]
    fn sockets_are_claimed_once_and_never_taken_from_a_live_server() {
        let dir = std::env::temp_dir().join(format!("temzdf-claim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        let first = claim_socket("s", &socket).unwrap();
        // A second server for the same name, started at the same time.
        let e = claim_socket("s", &socket).unwrap_err();
        assert!(e.to_string().contains("already exists"), "{e}");
        drop(first);
        assert!(!socket.exists(), "the server's own socket goes with it");
        // Something listens that does not answer as this version: kept.
        let other = UnixListener::bind(&socket).unwrap();
        let e = claim_socket("s", &socket).unwrap_err();
        assert!(e.to_string().contains("another temzdf version"), "{e}");
        assert!(socket.exists());
        drop(other);
        // Stale (nobody listens): replaced.
        let (lock, listener, cleanup) = claim_socket("s", &socket).unwrap();
        // Another server's socket replaced ours meanwhile: ours does not
        // take it along when it ends.
        std::fs::remove_file(&socket).unwrap();
        let _theirs = UnixListener::bind(&socket).unwrap();
        drop((lock, listener, cleanup));
        assert!(socket.exists(), "the new socket stays");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empty_sizes_are_ignored() {
        assert!(usable(DEFAULT_GEOMETRY).is_some());
        assert!(usable(Geometry { cols: 0, ..DEFAULT_GEOMETRY }).is_none());
        let no_cell = Geometry { cell: CellSize { width: 0, height: 20 }, ..DEFAULT_GEOMETRY };
        assert!(usable(no_cell).is_none());
    }

    #[test]
    fn the_remaining_client_takes_over_size_and_effects() {
        let sink: EventSink = Arc::new(|_| true);
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut session = Session::new("t".into(), DEFAULT_GEOMETRY, Hints::default(), settings, sink);
        let client = |cols, rows| Client {
            tx: mpsc::channel().0,
            attached: true,
            pending: HashMap::new(),
            awaiting: false,
            shown: 0,
            geometry: Some(Geometry { cols, rows, ..DEFAULT_GEOMETRY }),
            hints: Some(Hints::default()),
        };
        let mut clients = HashMap::from([(1, client(80, 24)), (2, client(200, 60))]);
        let mut latest = None;
        assert!(become_latest(&mut session, &mut latest, &clients, 1));
        assert!(become_latest(&mut session, &mut latest, &clients, 2));
        assert_eq!((latest, session.geometry().cols), (Some(2), 200));
        assert!(!hand_over(&mut session, &mut latest, &clients), "still there");
        // The big one detaches: the small one's size applies again.
        clients.remove(&2);
        assert!(hand_over(&mut session, &mut latest, &clients));
        assert_eq!((latest, session.geometry().cols, session.geometry().rows), (Some(1), 80, 24));
        clients.remove(&1);
        assert!(!hand_over(&mut session, &mut latest, &clients));
        assert_eq!(latest, None, "nobody to send effects to");
    }

    #[test]
    fn nothing_to_send_means_no_short_waits() {
        let now = Instant::now();
        let made = now - FRAME * 2;
        let long = Duration::from_secs(3600);
        // A frame is due and a client can take it: right away.
        assert_eq!(wait_for(true, false, true, made, None, now), Duration::ZERO);
        // Nobody can take it (detached, or all awaiting acknowledgements):
        // no busy loop.
        assert_eq!(wait_for(true, false, false, made, None, now), long);
        assert_eq!(wait_for(false, true, false, made, None, now), long);
        assert_eq!(wait_for(false, true, true, made, None, now), FRAME);
        // Deadlines (timers, debounces) still count.
        assert_eq!(wait_for(true, true, false, made, Some(now + FRAME), now), FRAME);
    }
}
