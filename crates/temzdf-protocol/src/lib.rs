//! The session protocol: messages between a session server and its clients
//! (and the `temzdf` commands that query or control it), their framing on a
//! Unix socket, and where sessions' sockets live.
//!
//! One server process per session listens on
//! `<runtime dir>/temzdf/sessions/<name>.sock` (mode 0700 directory). Each
//! message is a little-endian `u32` length followed by the message encoded with
//! `postcard`; a frame's large images follow it, their pixels as they are
//! (`trailing`), or come in shared memory (`shared`, [`send_server`]). A
//! connection starts with [`ClientMsg::Hello`] (a client that will
//! show the session) or a single request ([`ClientMsg::Info`],
//! [`ClientMsg::Kill`], [`ClientMsg::Action`]) answered once.

pub mod discovery;
pub mod script;
pub mod shared;

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use temzdf_scene::{Appearance, Effect, Frame, Hints, Image, ImageKey, ImageUpdate};
use termpix_core::geom::Geometry;
use termpix_core::input::{KeyEvent, MouseEvent};

/// Bumped on incompatible changes; client and server must agree.
pub const VERSION: u32 = 14;
/// Larger messages are refused (a frame with every image of a full cache is
/// about 200 MB).
const MAX_MESSAGE: usize = 512 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// Attach and show the session at this size, with this frontend.
    Hello {
        version: u32,
        geometry: Geometry,
        hints: Hints,
        /// The session whose pane the client runs in, if any (nested).
        inside: Option<String>,
        /// Frames may bring large images' pixels in shared memory
        /// ([`recv_server`]), not inside the message.
        shared_memory: bool,
    },
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    /// Text an input method committed (Japanese, dead keys, an emoji
    /// palette): not keys, and not a paste (no brackets for terminals).
    Text(String),
    /// The client's window or terminal gained (true) or lost the focus.
    Focus(bool),
    Resize(Geometry),
    /// The frame with this number is on screen: send the next one.
    FrameAck(u64),
    /// Something the frontend could not do (e.g. open a link).
    Report(String),
    Detach,
    /// Requests answered once (no Hello): describe the session, end it, run
    /// an action (its text form, see `temzdf-server`'s `Action`), or tell
    /// how GUI clients should look (`ServerMsg::Appearance`).
    Info {
        version: u32,
    },
    Kill,
    Action(Vec<String>),
    GetAppearance,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Image changes to apply, then the frame to show. `complete`: every
    /// visible page is fully rendered (half-rendered first frames may wait).
    Frame {
        seq: u64,
        updates: Vec<ImageUpdate>,
        frame: Frame,
        complete: bool,
        /// Images set through shared memory, one descriptor each, in this
        /// order, with the message. Only on the wire: [`recv_server`] turns
        /// them into `updates`.
        shared: Vec<ImageHead>,
        /// Images set by pixels that follow the message, in this order. Only
        /// on the wire, as `shared`.
        trailing: Vec<ImageHead>,
    },
    Effect(Effect),
    /// How to look (GUI clients): when attaching and when it changes.
    Appearance(Appearance),
    /// The connection ends.
    Exit(ExitReason),
    Info(SessionInfo),
    /// The answer to an action: `Err` with a message when it failed.
    Done(Result<(), String>),
}

/// An image whose pixels (`width * height * 4` bytes) come apart from the
/// message: in shared memory, or after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageHead {
    pub key: ImageKey,
    pub width: u32,
    pub height: u32,
}

impl ImageHead {
    /// Its bytes of pixels.
    pub fn bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExitReason {
    Detached,
    /// The last pane closed.
    SessionEnded,
    Killed,
    /// Client and server versions differ, or the request was malformed.
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub version: u32,
    pub name: String,
    pub pid: u32,
    pub tabs: usize,
    pub panes: usize,
    /// Documents and terminals, shown or hidden.
    pub buffers: usize,
    pub clients: usize,
    /// The files open in the session.
    pub files: Vec<PathBuf>,
    /// Where the server's output goes.
    pub log: Option<PathBuf>,
    /// The sessions whose panes its attached clients run in: the sessions
    /// that show this one.
    pub shown_in: Vec<String>,
}

/// Write one message.
pub fn send<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let body = postcard::to_stdvec(msg).map_err(io::Error::other)?;
    let len = u32::try_from(body.len()).map_err(|_| io::Error::other("message too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// Read one message; `Ok(None)` at a clean end of the stream. Messages
/// from the server go through [`recv_server`] instead.
pub fn recv<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> io::Result<Option<T>> {
    Ok(recv_timed(r)?.map(|(msg, _)| msg))
}

/// [`recv`], with how long the message took once its first bytes came
/// (reading and decoding, not waiting for it).
pub fn recv_timed<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> io::Result<Option<(T, Duration)>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let start = Instant::now();
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("message of {len} bytes")));
    }
    // Grown as bytes come, not allocated from the length alone (a bogus
    // length would take hundreds of megabytes for nothing).
    let mut body = Vec::new();
    r.take(len as u64).read_to_end(&mut body)?;
    if body.len() < len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let msg = postcard::from_bytes(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some((msg, start.elapsed())))
}

/// Images smaller than this travel inside the message (UI parts); larger
/// ones after it, their bytes written straight from the image.
const MIN_TRAILING: usize = 4 << 10;

/// Send a message from the server to a client. A frame's large images do not
/// go through the encoder: with `shared_memory` (the client said so in its
/// Hello) their pixels go in shared memory, a descriptor each with the
/// message (`shared`); otherwise, or when that memory cannot be made, their
/// bytes follow the message as they are (`trailing`), written from the
/// images themselves.
pub fn send_server(stream: &UnixStream, msg: ServerMsg, shared_memory: bool) -> io::Result<()> {
    let (msg, fds, after) = match msg {
        ServerMsg::Frame { seq, updates, frame, complete, mut shared, mut trailing } => {
            let (mut inline, mut fds, mut after) = (Vec::new(), Vec::new(), Vec::new());
            for u in updates {
                let ImageUpdate::Set(key, image) = u else {
                    inline.push(u);
                    continue;
                };
                let head = ImageHead { key, width: image.width, height: image.height };
                let len = image.pixels.len();
                if shared_memory
                    && len >= shared::MIN_SHARED
                    && fds.len() < shared::MAX_FDS
                    && let Ok(fd) = shared::create(&image.pixels)
                {
                    fds.push(fd);
                    shared.push(head);
                    continue;
                }
                if len >= MIN_TRAILING && len == image.width as usize * image.height as usize * 4 {
                    trailing.push(head);
                    after.push(image.pixels);
                } else {
                    inline.push(ImageUpdate::Set(key, image));
                }
            }
            (ServerMsg::Frame { seq, updates: inline, frame, complete, shared, trailing }, fds, after)
        }
        msg => (msg, Vec::new(), Vec::new()),
    };
    let body = postcard::to_stdvec(&msg).map_err(io::Error::other)?;
    let len = u32::try_from(body.len()).map_err(|_| io::Error::other("message too large"))?;
    let len = len.to_le_bytes();
    let mut parts = vec![io::IoSlice::new(&len), io::IoSlice::new(&body)];
    parts.extend(after.iter().map(|p| io::IoSlice::new(p)));
    let borrowed: Vec<_> = fds.iter().map(|fd| fd.as_fd()).collect();
    shared::send_with_fds(stream, &mut parts, &borrowed)
}

/// Read a message from the server ([`send_server`]): a frame's images in
/// shared memory are mapped and those after it read, all into `updates`;
/// with how long it took once its first bytes came. Every reader of frames
/// must use this, not [`recv`]: the pixels after a frame would be taken for
/// the next message.
pub fn recv_server(r: &mut shared::FdReader) -> io::Result<Option<(ServerMsg, Duration)>> {
    let Some((mut msg, took)) = recv_timed::<ServerMsg>(r)? else { return Ok(None) };
    let start = Instant::now();
    if let ServerMsg::Frame { updates, shared, trailing, .. } = &mut msg {
        for s in shared.drain(..) {
            let fd = r
                .take_fd()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "a shared image without its descriptor"))?;
            let pixels = shared::map(fd, s.bytes())?;
            updates.push(ImageUpdate::Set(s.key, Image { width: s.width, height: s.height, pixels }));
        }
        let total = trailing.iter().map(|t| t.bytes() as u64).sum::<u64>();
        if total > MAX_MESSAGE as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{total} bytes of pixels after a frame")));
        }
        for t in trailing.drain(..) {
            // Read into exactly its size, not zeroed first, not grown.
            let mut pixels = Vec::with_capacity(t.bytes());
            r.take(t.bytes() as u64).read_to_end(&mut pixels)?;
            if pixels.len() < t.bytes() {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            updates.push(ImageUpdate::Set(t.key, Image { width: t.width, height: t.height, pixels: pixels.into() }));
        }
    }
    Ok(Some((msg, took + start.elapsed())))
}

/// Whether this client takes pixels in shared memory: yes unless
/// `TEMZDF_PIXELS=socket` (tests pick either way; `doc/config.md`).
pub fn shared_memory_wanted() -> bool {
    std::env::var("TEMZDF_PIXELS").map_or(true, |v| v != "socket")
}

/// Whether to log how long frames take on their way (`TEMZDF_TIMING` set,
/// to anything): the server and the clients write `timing:` lines to their
/// logs (`doc/performance.md`, "Session socket").
pub fn timing() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TEMZDF_TIMING").is_some_and(|v| !v.is_empty()))
}

/// A frame message's sequence number, images set and their bytes of
/// pixels, for timing logs.
pub fn frame_size(msg: &ServerMsg) -> Option<(u64, usize, usize)> {
    let ServerMsg::Frame { seq, updates, .. } = msg else { return None };
    let set = updates.iter().filter_map(|u| match u {
        ImageUpdate::Set(_, image) => Some(image.pixels.len()),
        ImageUpdate::Remove(_) => None,
    });
    let (images, bytes) = set.fold((0, 0), |(n, b), len| (n + 1, b + len));
    Some((*seq, images, bytes))
}

/// Milliseconds, for timing logs.
pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The per-user directory for sockets: `$XDG_RUNTIME_DIR`, else the per-user
/// temporary directory on macOS, else `<temp dir>/temzdf-<uid>`.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return within_runtime_dir(dir.into());
    }
    // Not $TMPDIR on macOS: `nix develop` points it at a per-shell directory,
    // so processes started from different shells would miss each other.
    #[cfg(target_os = "macos")]
    if let Some(dir) = darwin_user_temp_dir() {
        return dir;
    }
    std::env::temp_dir().join(format!("temzdf-{}", rustix::process::getuid().as_raw()))
}

/// `$XDG_RUNTIME_DIR` (`dir`) if it is a directory of this user's, else (a
/// container or CI setting it to `/tmp`) `temzdf-<uid>` in it, as in the
/// shared temporary directory.
fn within_runtime_dir(dir: PathBuf) -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let uid = rustix::process::getuid().as_raw();
    match std::fs::metadata(&dir) {
        Ok(m) if m.is_dir() && m.uid() == uid => dir,
        _ => dir.join(format!("temzdf-{uid}")),
    }
}

/// The per-user temporary directory (`/var/folders/.../T/`, mode 0700),
/// independent of the environment.
#[cfg(target_os = "macos")]
fn darwin_user_temp_dir() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut buf = vec![0u8; 1024];
    // SAFETY: confstr writes at most `buf.len()` bytes, NUL included.
    let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
    if n == 0 || n > buf.len() {
        return None;
    }
    buf.truncate(n - 1);
    Some(std::ffi::OsString::from_vec(buf).into())
}

/// Where session sockets live (created, ours alone; see [`private_dir`]).
pub fn sessions_dir() -> io::Result<PathBuf> {
    let dir = runtime_dir().join("temzdf").join("sessions");
    private_dir(&dir)?;
    Ok(dir)
}

/// Create `dir` under [`runtime_dir`] and make sure nobody else can reach
/// into it, as tmux does: every directory from the runtime directory down
/// must be a real directory (not a symlink) owned by this user, and those
/// temzdf creates are made mode 0700. Without `$XDG_RUNTIME_DIR` the
/// runtime directory is in the shared temporary directory, where another
/// user could create it first and plant a socket: a client attaching to it
/// would run what that server's effects ask for.
pub fn private_dir(dir: &Path) -> io::Result<()> {
    let base = runtime_dir();
    let ours = base.file_name().is_some_and(|n| *n == *format!("temzdf-{}", rustix::process::getuid().as_raw()));
    private_dir_in(&base, ours, dir)
}

/// [`private_dir`] for runtime directory `base`; `base_ours`: temzdf made it
/// (its mode is set too).
fn private_dir_in(base: &Path, base_ours: bool, dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    std::fs::create_dir_all(dir)?;
    let uid = rustix::process::getuid().as_raw();
    let unsafe_dir = |d: &Path, why: &str| {
        // Removing is advice for what temzdf made (or someone planted in
        // its place), not for the system's runtime directory.
        let hint = if d == base && !base_ours { "" } else { "; remove it and try again" };
        io::Error::new(io::ErrorKind::PermissionDenied, format!("{}: {why}{hint}", d.display()))
    };
    let mut d = dir;
    loop {
        // The system's runtime directory may be a symlink (macOS's /tmp).
        let m = if d == base && !base_ours { std::fs::metadata(d)? } else { std::fs::symlink_metadata(d)? };
        if m.file_type().is_symlink() || !m.is_dir() {
            return Err(unsafe_dir(d, "not a directory"));
        }
        if m.uid() != uid {
            return Err(unsafe_dir(d, &format!("owned by user {}, not by you", m.uid())));
        }
        let at_base = d == base;
        if m.mode() & 0o077 != 0 && (!at_base || base_ours) {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
        }
        match d.parent() {
            Some(p) if !at_base && d.starts_with(base) => d = p,
            _ => return Ok(()),
        }
    }
}

/// Session names: letters, digits, `.`, `_` and `-`, at most 64 characters,
/// not starting with `.` (hidden files) or `-` (an option to the server).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with(['.', '-'])
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn socket_path(name: &str) -> io::Result<PathBuf> {
    socket_in(&sessions_dir()?, name)
}

/// The size of `sockaddr_un`'s path, NUL included.
#[cfg(target_os = "macos")]
const SUN_PATH: usize = 104;
#[cfg(not(target_os = "macos"))]
const SUN_PATH: usize = 108;

/// The longest session name whose socket path fits (see [`socket_path`]).
pub fn max_name_len() -> io::Result<usize> {
    Ok(max_name_in(&sessions_dir()?))
}

fn max_name_in(dir: &Path) -> usize {
    // "<dir>/<name>.sock" and the NUL.
    (SUN_PATH - 1).saturating_sub(dir.as_os_str().len() + "/.sock".len()).min(64)
}

/// `<dir>/<name>.sock`, or an error saying so when that is longer than a
/// socket path may be (on macOS the per-user directory leaves about 30
/// characters for the name).
fn socket_in(dir: &Path, name: &str) -> io::Result<PathBuf> {
    if name.len() > max_name_in(dir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "session name {name:?} is too long for a socket in {} (at most {} characters)",
                dir.display(),
                max_name_in(dir)
            ),
        ));
    }
    Ok(dir.join(format!("{name}.sock")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bogus_length_does_not_allocate_it() {
        // 500 MB announced, 3 bytes sent: an error, without the allocation.
        let mut bytes = (500u32 << 20).to_le_bytes().to_vec();
        bytes.extend_from_slice(b"abc");
        let e = recv::<ClientMsg>(&mut bytes.as_slice()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn socket_paths_fit_a_sockaddr() {
        let dir = Path::new("/var/folders/xy/abcdefghijklmnopqrstuvwxyz012345/T/temzdf/sessions");
        let max = max_name_in(dir);
        let longest = socket_in(dir, &"n".repeat(max)).unwrap();
        assert!(longest.as_os_str().len() < SUN_PATH, "{} bytes", longest.as_os_str().len());
        let e = socket_in(dir, &"n".repeat(max + 1)).unwrap_err();
        assert!(e.to_string().contains(&format!("at most {max} characters")), "{e}");
        assert_eq!(max_name_in(Path::new("/run/user/1000/temzdf/sessions")), 64, "the name rule's limit");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri's getuid is always 1000, not the files' owner unless that is 1000 (GHA, macOS)")]
    fn private_dirs_are_ours_alone() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join(format!("temzdf-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        // A runtime directory temzdf made: it and what is below become 0700.
        let base = tmp.join("ours");
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o777)).unwrap();
        private_dir_in(&base, true, &base.join("temzdf/sessions")).unwrap();
        for d in [base.clone(), base.join("temzdf"), base.join("temzdf/sessions")] {
            assert_eq!(mode(&d), 0o700, "{}", d.display());
        }
        // The system's runtime directory keeps its mode.
        let system = tmp.join("system");
        std::fs::create_dir(&system).unwrap();
        std::fs::set_permissions(&system, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir_in(&system, false, &system.join("temzdf")).unwrap();
        assert_eq!((mode(&system), mode(&system.join("temzdf"))), (0o755, 0o700));
        // A symlink planted where a directory should be is refused.
        let elsewhere = tmp.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let linked = tmp.join("linked");
        std::fs::create_dir(&linked).unwrap();
        std::os::unix::fs::symlink(&elsewhere, linked.join("temzdf")).unwrap();
        let e = private_dir_in(&linked, true, &linked.join("temzdf/sessions")).unwrap_err();
        assert!(e.to_string().contains("not a directory"), "{e}");
        // So is a file.
        let file = tmp.join("file");
        std::fs::create_dir(&file).unwrap();
        std::fs::write(file.join("temzdf"), "").unwrap();
        assert!(private_dir_in(&file, true, &file.join("temzdf")).is_err());
        // The system's runtime directory through a symlink (macOS's /tmp).
        let via = tmp.join("via");
        std::os::unix::fs::symlink(&system, &via).unwrap();
        private_dir_in(&via, false, &via.join("temzdf/sessions")).unwrap();
        // (Another user's directory cannot be made here; the owner check is
        // the same path as the symlink check.)
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri's getuid is always 1000, not the files' owner unless that is 1000 (GHA, macOS)")]
    fn a_runtime_directory_not_ours_gets_one_of_ours_inside() {
        let uid = rustix::process::getuid().as_raw();
        let tmp = std::env::temp_dir().join(format!("temzdf-runtime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(within_runtime_dir(tmp.clone()), tmp, "ours: as it is");
        let link = tmp.with_extension("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&tmp, &link).unwrap();
        assert_eq!(within_runtime_dir(link.clone()), link, "ours through a symlink");
        if uid != 0 {
            // XDG_RUNTIME_DIR=/ (root's, as /tmp is).
            assert_eq!(within_runtime_dir("/".into()), Path::new("/").join(format!("temzdf-{uid}")));
        }
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
    }
    use std::collections::HashMap;
    use std::sync::Arc;
    use temzdf_scene::{Cell, CursorShape, ImageKey, Images, Layer, Pointer, Rect, TextGrid};
    use termpix_core::geom::CellSize;
    use termpix_core::input::{KeyCode, Modifiers};

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(msg: &T) {
        let mut buf = Vec::new();
        send(&mut buf, msg).unwrap();
        let mut r = buf.as_slice();
        assert_eq!(recv::<T>(&mut r).unwrap().as_ref(), Some(msg));
        assert_eq!(recv::<T>(&mut r).unwrap(), None, "clean end");
    }

    #[test]
    fn messages_round_trip() {
        let geometry = Geometry { cols: 80, rows: 24, cell: CellSize { width: 10, height: 20 } };
        round_trip(&ClientMsg::Hello {
            version: VERSION,
            geometry,
            hints: Hints::default(),
            inside: Some("a".into()),
            shared_memory: true,
        });
        round_trip(&ClientMsg::Key(KeyEvent::new(KeyCode::Char('s'), Modifiers::CTRL)));
        round_trip(&ClientMsg::Action(vec!["new-pane".into(), "a b.pdf".into()]));
        let mut images = Images::new();
        let key: ImageKey = images.add(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let layer = Layer { key: 7, image: key, src: Rect::new(0, 0, 2, 1), dst: Rect::new(-3, 4, 20, 10), z: -5 };
        let frame = Frame {
            layers: vec![layer],
            text: vec![TextGrid {
                col: 3,
                row: 1,
                rows: vec![vec![
                    Cell { text: "数".into(), width: 2, fg: temzdf_scene::Color::Rgb([1, 2, 3]), ..Cell::blank() },
                    Cell::default(),
                ]],
            }],
            cursor: Some((1, 2)),
            cursor_shape: CursorShape::Bar { blink: true },
            pointer: Pointer::Link,
            title: "doc — 1 / 3".into(),
        };
        round_trip(&ServerMsg::Frame {
            seq: 9,
            updates: images.take_updates(),
            frame,
            complete: true,
            shared: vec![],
            trailing: vec![],
        });
        round_trip(&ServerMsg::Effect(Effect::Copy("x".into())));
        round_trip(&ServerMsg::Exit(ExitReason::Error("bad".into())));
        round_trip(&ServerMsg::Done(Err("no".into())));
    }

    #[test]
    fn pixels_travel_as_bytes() {
        let px: Arc<[u8]> = vec![7u8; 4096].into();
        let mut images = Images::new();
        images.add(32, 32, px);
        let msg = ServerMsg::Frame {
            seq: 0,
            updates: images.take_updates(),
            frame: Frame::default(),
            complete: false,
            shared: vec![],
            trailing: vec![],
        };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        assert!(buf.len() < 4096 + 64, "{} bytes", buf.len());
    }

    /// A frame of square images of these sides, each of its own pixels.
    fn frame_of(sizes: &[u32]) -> (ServerMsg, HashMap<ImageKey, Image>) {
        let mut images = Images::new();
        for (i, &side) in sizes.iter().enumerate() {
            let px: Vec<u8> = (0..side * side * 4).map(|b| (b as usize * 7 + i * 13) as u8).collect();
            images.add(side, side, px);
        }
        let updates = images.take_updates();
        let want = updates
            .iter()
            .filter_map(|u| match u {
                ImageUpdate::Set(k, i) => Some((*k, i.clone())),
                ImageUpdate::Remove(_) => None,
            })
            .collect();
        (
            ServerMsg::Frame {
                seq: 3,
                updates,
                frame: Frame::default(),
                complete: true,
                shared: vec![],
                trailing: vec![],
            },
            want,
        )
    }

    fn got_images(msg: &ServerMsg) -> HashMap<ImageKey, Image> {
        let ServerMsg::Frame { updates, shared, trailing, .. } = msg else { panic!("{msg:?}") };
        assert!(shared.is_empty() && trailing.is_empty(), "recv_server puts them into updates");
        updates
            .iter()
            .filter_map(|u| match u {
                ImageUpdate::Set(k, i) => Some((*k, i.clone())),
                ImageUpdate::Remove(_) => None,
            })
            .collect()
    }

    #[test]
    #[cfg_attr(miri, ignore = "sockets and shared memory")]
    fn frames_arrive_whole_through_shared_memory_or_after_the_message() {
        // 128 px is exactly the smallest size that goes shared, 32 px the
        // smallest that follows the message; two of the same size tell their
        // places apart only by their pixels.
        let (msg, want) = frame_of(&[128, 128, 256, 32, 8]);
        for shared_memory in [false, true] {
            let (tx, rx) = UnixStream::pair().unwrap();
            let mut rx = shared::FdReader::new(rx);
            // From another thread: a frame can be more than the socket holds.
            let sender = {
                let msg = msg.clone();
                std::thread::spawn(move || {
                    for m in [msg.clone(), msg, ServerMsg::Done(Ok(()))] {
                        send_server(&tx, m, shared_memory).unwrap();
                    }
                })
            };
            // On the wire first: which images went where.
            let (raw, _) = recv_timed::<ServerMsg>(&mut rx).unwrap().unwrap();
            let ServerMsg::Frame { updates, shared, trailing, .. } = &raw else { panic!() };
            let (inside, fds, after) = if shared_memory { (1, 3, 1) } else { (1, 0, 4) };
            let counts = (updates.len(), shared.len(), trailing.len());
            assert_eq!(counts, (inside, fds, after), "shared_memory: {shared_memory}");
            for _ in 0..fds {
                assert!(rx.take_fd().is_some());
            }
            assert!(rx.take_fd().is_none());
            let skip = trailing.iter().map(|t| t.bytes() as u64).sum();
            assert_eq!(io::copy(&mut (&mut rx).take(skip), &mut io::sink()).unwrap(), skip);
            // Then as clients see it: every image at its key.
            let (got, _) = recv_server(&mut rx).unwrap().unwrap();
            assert_eq!(got_images(&got), want, "shared_memory: {shared_memory}");
            assert!(rx.take_fd().is_none(), "no descriptor left over");
            // Other messages are unchanged.
            assert_eq!(recv_server(&mut rx).unwrap().unwrap().0, ServerMsg::Done(Ok(())));
            sender.join().unwrap();
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "sockets and shared memory")]
    fn images_beyond_the_descriptor_limit_go_inside_the_message() {
        let sides = vec![128; shared::MAX_FDS + 10];
        let (msg, want) = frame_of(&sides);
        let (tx, rx) = UnixStream::pair().unwrap();
        let mut rx = shared::FdReader::new(rx);
        let sender = std::thread::spawn(move || send_server(&tx, msg, true).unwrap());
        let (got, _) = recv_server(&mut rx).unwrap().unwrap();
        sender.join().unwrap();
        assert_eq!(got_images(&got), want);
    }

    /// A frame announcing pixels after it, as a broken or hostile server
    /// would send it, with `after` bytes of them.
    fn frame_announcing(width: u32, height: u32, after: usize) -> Vec<u8> {
        let head = ImageHead { key: Images::new().add(1, 1, vec![0; 4]), width, height };
        let msg = ServerMsg::Frame {
            seq: 1,
            updates: vec![],
            frame: Frame::default(),
            complete: true,
            shared: vec![],
            trailing: vec![head],
        };
        let mut bytes = Vec::new();
        send(&mut bytes, &msg).unwrap();
        bytes.extend(std::iter::repeat_n(7, after));
        bytes
    }

    #[test]
    #[cfg_attr(miri, ignore = "sockets")]
    fn pixels_after_a_frame_are_checked() {
        let read = |bytes: Vec<u8>| {
            let (mut tx, rx) = UnixStream::pair().unwrap();
            let mut rx = shared::FdReader::new(rx);
            let sender = std::thread::spawn(move || tx.write_all(&bytes));
            let got = recv_server(&mut rx).map(|m| m.map(|m| m.0));
            sender.join().unwrap().unwrap();
            got
        };
        // More than a message may hold: refused before anything is allocated.
        let err = read(frame_announcing(65535, 65535, 0)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        // The connection ends partway.
        let err = read(frame_announcing(32, 32, 100)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "{err}");
        // Whole: the pixels are the image's.
        let got = read(frame_announcing(32, 32, 32 * 32 * 4)).unwrap().unwrap();
        let ServerMsg::Frame { updates, .. } = got else { panic!() };
        assert!(matches!(&updates[..], [ImageUpdate::Set(_, i)] if i.pixels.iter().all(|&b| b == 7)));
    }

    #[test]
    #[cfg_attr(miri, ignore = "shared memory")]
    fn shared_memory_smaller_than_announced_is_an_error() {
        // Past any page rounding: macOS sizes the object in 16 KiB pages.
        let fd = shared::create(&[1; 4096]).unwrap();
        assert_eq!(shared::map(fd, 1 << 20).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let fd = shared::create(&[1, 2, 3, 4]).unwrap();
        assert_eq!(&*shared::map(fd, 4).unwrap(), &[1, 2, 3, 4]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "memfd_create")]
    fn shared_pixels_cannot_change_after_they_were_sent() {
        let file = std::fs::File::from(shared::create(&[1; 4096]).unwrap());
        assert!((&file).write_all(&[2]).is_err(), "sealed against writes");
        assert!(file.set_len(1).is_err(), "and against shrinking");
    }

    #[test]
    fn oversized_and_garbled_messages_are_errors() {
        let mut huge = (u32::MAX).to_le_bytes().to_vec();
        huge.extend([0; 8]);
        assert!(recv::<ClientMsg>(&mut huge.as_slice()).is_err());
        let garbled = [3u8, 0, 0, 0, 0xff, 0xff, 0xff];
        assert!(recv::<ClientMsg>(&mut garbled.as_slice()).is_err());
    }

    /// What a frame of N 512 px RGBA tiles costs between the session server
    /// and a client on this machine, through the socket and through shared
    /// memory (`doc/performance.md`):
    /// `cargo test --release -p temzdf-protocol socket_throughput -- --ignored --nocapture`.
    /// `send` is the server's part, `total` until the client has every
    /// image and has read all their bytes once (as an upload would); `copy`
    /// is one memcpy of the pixels into a fresh buffer.
    #[test]
    #[ignore]
    fn socket_throughput() {
        use std::time::{Duration, Instant};
        const TILE: u32 = 512;
        const RUNS: usize = 7;
        let median = |mut v: Vec<Duration>| {
            v.sort();
            v[v.len() / 2].as_secs_f64() * 1e3
        };
        let (tx, rx) = UnixStream::pair().unwrap();
        let mut rx = shared::FdReader::new(rx);
        for n in [1usize, 4, 12, 30] {
            let mut images = Images::new();
            for i in 0..n {
                let px: Vec<u8> = (0..TILE * TILE * 4).map(|b| (b as usize * 31 + i) as u8).collect();
                images.add(TILE, TILE, px);
            }
            let updates = images.take_updates();
            let bytes: usize =
                updates.iter().map(|u| if let ImageUpdate::Set(_, i) = u { i.pixels.len() } else { 0 }).sum();
            let msg = ServerMsg::Frame {
                seq: 1,
                updates,
                frame: Frame::default(),
                complete: true,
                shared: vec![],
                trailing: vec![],
            };
            for shared_memory in [false, true] {
                let (mut send, mut total, mut copy) = (vec![], vec![], vec![]);
                // The first run warms up the socket and the allocator.
                for run in 0..=RUNS {
                    let sender = {
                        let tx = tx.try_clone().unwrap();
                        let msg = msg.clone();
                        std::thread::spawn(move || {
                            let t = Instant::now();
                            send_server(&tx, msg, shared_memory).unwrap();
                            (t, t.elapsed())
                        })
                    };
                    let (got, _) = recv_server(&mut rx).unwrap().unwrap();
                    let ServerMsg::Frame { updates, .. } = &got else { panic!("{got:?}") };
                    let sum = updates.iter().fold(0u64, |acc, u| match u {
                        ImageUpdate::Set(_, i) => i.pixels.iter().step_by(64).fold(acc, |a, &b| a + b as u64),
                        ImageUpdate::Remove(_) => acc,
                    });
                    std::hint::black_box(sum);
                    let (start, d_send) = sender.join().unwrap();
                    let d_total = start.elapsed();
                    let t = Instant::now();
                    let mut out = vec![0u8; bytes];
                    let mut at = 0;
                    if let ServerMsg::Frame { updates, .. } = &msg {
                        for u in updates {
                            if let ImageUpdate::Set(_, i) = u {
                                out[at..at + i.pixels.len()].copy_from_slice(&i.pixels);
                                at += i.pixels.len();
                            }
                        }
                    }
                    std::hint::black_box(&out);
                    let d_copy = t.elapsed();
                    if run > 0 {
                        send.push(d_send);
                        total.push(d_total);
                        copy.push(d_copy);
                    }
                }
                let total_ms = median(total);
                eprintln!(
                    "BENCH tiles={n:2} MB={:5.1} {:6}  send={:6.2} ms  total={:6.2} ms ({:5.0} MB/s)  copy={:5.2} ms",
                    bytes as f64 / 1e6,
                    if shared_memory { "shared" } else { "socket" },
                    median(send),
                    total_ms,
                    bytes as f64 / 1e3 / total_ms,
                    median(copy),
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[cfg_attr(miri, ignore = "confstr returns nothing under Miri")]
    fn darwin_user_temp_dir_exists() {
        let dir = darwin_user_temp_dir().unwrap();
        assert!(dir.is_dir(), "{dir:?}");
    }

    #[test]
    fn session_names() {
        assert!(valid_name("thesis") && valid_name("my-paper_2.v1"));
        assert!(!valid_name("") && !valid_name("../x") && !valid_name(".hidden") && !valid_name("a b"));
        assert!(!valid_name(&"x".repeat(65)));
        // The server takes NAME as a positional argument: no option look-alikes.
        assert!(!valid_name("-foo") && !valid_name("--"));
    }
}
