//! The session server process: one [`Session`] shown to every attached client
//! over a Unix socket (see `temzdf-protocol`). The latest client to attach or
//! send input (not mere hovering, see [`hovers_elsewhere`]) decides the session's size (tmux's `window-size latest`); every
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
use termpix_core::input::MouseEventKind;

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
    /// Whether its window has the focus, once it said so.
    focused: Option<bool>,
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

pub use temzdf_protocol::discovery::{list, list_all, probe};

/// Run the session `name`, starting with a pane showing `first`, until it
/// ends. Returns once the last pane closed or the session was killed.
/// `log` is where the server's output goes (reported to `temzdf ls`).
pub fn run(name: &str, first: &PaneSpec, log: Option<PathBuf>, settings: Settings) -> Result<()> {
    if !proto::valid_name(name) {
        bail!("invalid session name {name:?}");
    }
    let socket = proto::socket_path(name)?;
    let (mut lock, listener, mut cleanup) = claim_socket(name, &socket)?;
    eprintln!("temzdf server: session {name} on {} (pid {})", socket.display(), std::process::id());

    let (tx, rx) = mpsc::channel::<Event>();
    let events: EventSink = {
        let tx = Mutex::new(tx.clone());
        Arc::new(move |e| tx.lock().unwrap_or_else(|p| p.into_inner()).send(Event::Session(e)).is_ok())
    };
    accept(listener, tx.clone())?;
    let mut session = Session::new(name.to_owned(), DEFAULT_GEOMETRY, Hints::default(), settings, events);
    session.start(first.clone(), None)?;
    let mut clients: HashMap<ClientId, Client> = HashMap::new();
    let mut next_client: ClientId = 1;
    let mut latest: Option<ClientId> = None;
    // What the programs were told last about the clients' focus.
    let mut told_focus: Option<bool> = None;
    let mut last_frame: Option<(u64, Frame, bool)> = None;
    // How GUI clients look, as last sent.
    let mut looks = session.appearance();
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
        // orphans must not live on. One in use carries on (a cleaner of
        // /tmp or of the runtime directory must not kill its programs),
        // listening again where it can.
        if socket_checked.elapsed() >= SOCKET_CHECK {
            socket_checked = Instant::now();
            let attached = clients.values().filter(|c| c.attached).count();
            let answered = || UnixStream::connect(&socket).is_ok();
            match socket_check(cleanup.still_ours(), attached, answered) {
                SocketCheck::Fine => {}
                SocketCheck::End => {
                    eprintln!("temzdf server: the socket is gone; ending session {name}");
                    for c in clients.values() {
                        let _ = c.tx.send(ServerMsg::Exit(ExitReason::SessionEnded));
                    }
                    break;
                }
                SocketCheck::Rebind => match rebind(name, &mut lock) {
                    Ok((listener, c)) => {
                        cleanup = c;
                        accept(listener, tx.clone())?;
                        eprintln!("temzdf server: the socket was gone; listening again on {}", socket.display());
                    }
                    Err(e) => eprintln!("temzdf server: the socket is gone and cannot be made again: {e:#}"),
                },
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
        // The configuration changed how GUI clients look.
        let now_looks = session.appearance();
        if now_looks != looks {
            for c in clients.values().filter(|c| c.attached) {
                let _ = c.tx.send(ServerMsg::Appearance(now_looks.clone()));
            }
            looks = now_looks;
        }
        let can_send = can_send(&session, &clients);
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
                        focused: None,
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
                    let _ = c.tx.send(ServerMsg::Appearance(session.appearance()));
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
                ClientMsg::Mouse(m) if hovers_elsewhere(m.kind, latest, id) => {}
                ClientMsg::Mouse(m) => {
                    dirty |= become_latest(&mut session, &mut latest, &clients, id);
                    outcome = session.mouse(&m, now);
                    dirty |= session.pointer() != shown_pointer;
                }
                ClientMsg::Paste(text) => {
                    dirty |= become_latest(&mut session, &mut latest, &clients, id);
                    outcome = session.paste(&text);
                }
                ClientMsg::Text(text) => {
                    dirty |= become_latest(&mut session, &mut latest, &clients, id);
                    outcome = session.text(&text, Instant::now());
                }
                ClientMsg::Focus(gained) => {
                    if let Some(c) = clients.get_mut(&id) {
                        c.focused = Some(gained);
                    }
                }
                ClientMsg::Resize(g) => dirty |= resized(&mut session, latest, &mut clients, id, g),
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
                ClientMsg::GetAppearance => {
                    if let Some(c) = clients.remove(&id) {
                        let _ = c.tx.send(ServerMsg::Appearance(session.appearance()));
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
        // Programs are told about focus as the clients together have it.
        if let Some(gained) = focus_change(&clients, &mut told_focus) {
            outcome = outcome.merge(session.client_focus(gained));
        }
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
/// Whether a frame could go out now: some client takes one, and the session
/// has one to show (not while it starts: no frame is made then, and waiting
/// on `dirty` alone would spin).
fn can_send(session: &Session, clients: &HashMap<ClientId, Client>) -> bool {
    !session.is_starting()
        && session.error().is_none()
        && clients.values().any(|c| c.attached && !c.awaiting)
        && !session.holds_frames()
}

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

/// Client `id`'s window changed size: kept for when it is the latest, and
/// applied now if it is. Resizing is no use of a client (a tiling window
/// manager resizes windows in the background): it does not make it the
/// latest. Returns whether anything changed.
fn resized(
    session: &mut Session,
    latest: Option<ClientId>,
    clients: &mut HashMap<ClientId, Client>,
    id: ClientId,
    g: Geometry,
) -> bool {
    let Some(c) = clients.get_mut(&id) else { return false };
    let Some(g) = usable(g) else { return false };
    c.geometry = Some(g);
    if latest != Some(id) || !c.attached || g == session.geometry() {
        return false;
    }
    session.resize(g);
    true
}

/// The focus the programs should be told about, when it changed since
/// `told`: gained while any attached client's window has the focus, lost
/// when none has. Several windows switching the focus report it over
/// separate connections in no fixed order, so one client losing it says
/// nothing alone. Nothing while no client said either.
fn focus_change(clients: &HashMap<ClientId, Client>, told: &mut Option<bool>) -> Option<bool> {
    let reported = clients.values().filter(|c| c.attached).filter_map(|c| c.focused);
    let mut any = None;
    for f in reported {
        any = Some(any.unwrap_or(false) || f);
    }
    let now = any?;
    (*told != Some(now)).then(|| {
        *told = Some(now);
        now
    })
}

/// Whether a mouse event from client `id` is only the pointer passing over
/// another client than the latest. Terminals report it over windows that are
/// not focused (and the GUI's window may be in front), so it is no use of the
/// client: it must not take the session's size, and its position is on that
/// client's grid, not the session's. Ignored.
fn hovers_elsewhere(kind: MouseEventKind, latest: Option<ClientId>, id: ClientId) -> bool {
    matches!(kind, MouseEventKind::Move | MouseEventKind::Leave) && latest != Some(id)
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

/// Accept connections on `listener` into the event loop.
fn accept(listener: UnixListener, tx: Sender<Event>) -> Result<()> {
    std::thread::Builder::new().name("temzdf-accept".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            if tx.send(Event::Connected(stream)).is_err() {
                return;
            }
        }
    })?;
    Ok(())
}

/// What the periodic socket check does.
#[derive(Debug, PartialEq)]
enum SocketCheck {
    Fine,
    /// Gone: listen at the path again.
    Rebind,
    End,
}

/// The socket check: a session nobody is attached to ends when its socket
/// is not its own any more; one in use carries on, and listens again unless
/// something else answers at the path.
fn socket_check(still_ours: bool, attached: usize, answered: impl FnOnce() -> bool) -> SocketCheck {
    if still_ours {
        SocketCheck::Fine
    } else if attached == 0 {
        SocketCheck::End
    } else if answered() {
        SocketCheck::Fine
    } else {
        SocketCheck::Rebind
    }
}

/// Listen for session `name` again after its socket was removed. The lock
/// (`NAME.lock`) is taken again if its file went too, so no other server
/// gets the name meanwhile.
fn rebind(name: &str, lock: &mut std::fs::File) -> Result<(UnixListener, RemoveOnDrop)> {
    use std::os::unix::fs::MetadataExt;
    // Makes the directory again if it is gone.
    let socket = proto::socket_path(name)?;
    let lock_path = socket.with_extension("lock");
    let held = lock.metadata().ok().map(|m| (m.dev(), m.ino()));
    if file_id(&lock_path) != held {
        let new = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("cannot open {}", lock_path.display()))?;
        match new.try_lock() {
            Ok(()) => *lock = new,
            Err(std::fs::TryLockError::WouldBlock) => bail!("another server took session {name:?}"),
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(e).context(format!("cannot lock {}", lock_path.display()));
            }
        }
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).with_context(|| format!("cannot listen on {}", socket.display()))?;
    Ok((listener, RemoveOnDrop::new(&socket)))
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
    fn a_session_in_use_outlives_its_socket() {
        let never = || -> bool { panic!("not asked") };
        assert_eq!(socket_check(true, 0, never), SocketCheck::Fine);
        assert_eq!(socket_check(false, 0, never), SocketCheck::End, "nobody can reach it");
        assert_eq!(socket_check(false, 1, || false), SocketCheck::Rebind, "in use: listen again");
        assert_eq!(socket_check(false, 2, || true), SocketCheck::Fine, "another server there: carry on");
    }

    #[test]
    fn a_starting_session_waits_instead_of_spinning() {
        let sink: crate::session::EventSink = std::sync::Arc::new(|_| true);
        let settings = crate::session::Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let geometry = Geometry { cols: 80, rows: 24, cell: termpix_core::geom::CellSize { width: 10, height: 20 } };
        // No pane yet: starting.
        let session = Session::new("t".into(), geometry, Hints::default(), settings, sink);
        assert!(session.is_starting());
        let client = Client {
            tx: mpsc::channel().0,
            attached: true,
            pending: HashMap::new(),
            awaiting: false,
            shown: 0,
            geometry: None,
            hints: None,
            focused: None,
        };
        let clients = HashMap::from([(1, client)]);
        let now = Instant::now();
        let t = wait_for(true, false, can_send(&session, &clients), now - FRAME * 2, session.next_deadline(), now);
        assert!(t > Duration::from_millis(100), "{t:?}");
    }

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
            focused: None,
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
    fn resizing_a_client_does_not_make_it_the_latest() {
        let sink: EventSink = Arc::new(|_| true);
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut session = Session::new("t".into(), DEFAULT_GEOMETRY, Hints::default(), settings, sink);
        let client = |cols| Client {
            tx: mpsc::channel().0,
            attached: true,
            pending: HashMap::new(),
            awaiting: false,
            shown: 0,
            geometry: Some(Geometry { cols, ..DEFAULT_GEOMETRY }),
            hints: Some(Hints::default()),
            focused: None,
        };
        let mut clients = HashMap::from([(1, client(80)), (2, client(100))]);
        let mut latest = None;
        become_latest(&mut session, &mut latest, &clients, 1);
        // A background window resized: kept for later, the session keeps
        // the latest client's size.
        assert!(!resized(&mut session, latest, &mut clients, 2, Geometry { cols: 120, ..DEFAULT_GEOMETRY }));
        assert_eq!((latest, session.geometry().cols), (Some(1), 80));
        // The latest client resized: the session follows.
        assert!(resized(&mut session, latest, &mut clients, 1, Geometry { cols: 90, ..DEFAULT_GEOMETRY }));
        assert_eq!(session.geometry().cols, 90);
        // An empty size changes nothing.
        assert!(!resized(&mut session, latest, &mut clients, 1, Geometry { cols: 0, ..DEFAULT_GEOMETRY }));
        assert_eq!(session.geometry().cols, 90);
        // Using the other one later brings its new size.
        become_latest(&mut session, &mut latest, &clients, 2);
        assert_eq!(session.geometry().cols, 120);
    }

    #[test]
    fn programs_have_the_focus_while_any_client_has_it() {
        let client = |attached, focused| Client {
            tx: mpsc::channel().0,
            attached,
            pending: HashMap::new(),
            awaiting: false,
            shown: 0,
            geometry: None,
            hints: None,
            focused,
        };
        let mut told = None;
        let mut clients = HashMap::from([(1, client(true, None)), (2, client(false, Some(true)))]);
        assert_eq!(focus_change(&clients, &mut told), None, "no attached client said");
        clients.get_mut(&1).unwrap().focused = Some(true);
        assert_eq!(focus_change(&clients, &mut told), Some(true));
        assert_eq!(focus_change(&clients, &mut told), None, "told already");
        // Switching windows: the new one gains it before the old one loses
        // it, or after; either way the programs keep the focus.
        clients.insert(3, client(true, Some(true)));
        clients.get_mut(&1).unwrap().focused = Some(false);
        assert_eq!(focus_change(&clients, &mut told), None);
        clients.get_mut(&3).unwrap().focused = Some(false);
        assert_eq!(focus_change(&clients, &mut told), Some(false), "none has it");
        clients.get_mut(&1).unwrap().focused = Some(true);
        assert_eq!(focus_change(&clients, &mut told), Some(true));
        // The focused client goes.
        clients.remove(&1);
        assert_eq!(focus_change(&clients, &mut told), Some(false));
    }

    #[test]
    fn only_using_a_client_makes_it_the_latest() {
        use termpix_core::input::{MouseButton, ScrollDirection};
        // The pointer passing over another client's window: ignored.
        assert!(hovers_elsewhere(MouseEventKind::Move, Some(1), 2));
        assert!(hovers_elsewhere(MouseEventKind::Leave, Some(1), 2));
        // Over the latest client's: hover effects as usual.
        assert!(!hovers_elsewhere(MouseEventKind::Move, Some(1), 1));
        assert!(!hovers_elsewhere(MouseEventKind::Leave, Some(1), 1));
        // Clicks and scrolling are using the client, wherever they come from.
        for kind in [
            MouseEventKind::Press(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Release(MouseButton::Left),
            MouseEventKind::Scroll(ScrollDirection::Down),
        ] {
            assert!(!hovers_elsewhere(kind, Some(1), 2), "{kind:?}");
        }
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
