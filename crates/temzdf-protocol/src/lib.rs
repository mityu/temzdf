//! The session protocol: messages between a session server and its clients
//! (and the `temzdf` commands that query or control it), their framing on a
//! Unix socket, and where sessions' sockets live.
//!
//! One server process per session listens on
//! `<runtime dir>/temzdf/sessions/<name>.sock` (mode 0700 directory). Each
//! message is a little-endian `u32` length followed by the message encoded with
//! `postcard`. A connection starts with [`ClientMsg::Hello`] (a client that will
//! show the session) or a single request ([`ClientMsg::Info`],
//! [`ClientMsg::Kill`], [`ClientMsg::Action`]) answered once.

pub mod discovery;
pub mod script;

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use temzdf_scene::{Appearance, Effect, Frame, Hints, ImageUpdate};
use termpix_core::geom::Geometry;
use termpix_core::input::{KeyEvent, MouseEvent};

/// Bumped on incompatible changes; client and server must agree.
pub const VERSION: u32 = 10;
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
}

/// Write one message.
pub fn send<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let body = postcard::to_stdvec(msg).map_err(io::Error::other)?;
    let len = u32::try_from(body.len()).map_err(|_| io::Error::other("message too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// Read one message; `Ok(None)` at a clean end of the stream.
pub fn recv<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
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
    postcard::from_bytes(&body).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// The per-user directory for sockets: `$XDG_RUNTIME_DIR`, else the per-user
/// temporary directory on macOS, else `<temp dir>/temzdf-<uid>`.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return dir.into();
    }
    // Not $TMPDIR on macOS: `nix develop` points it at a per-shell directory,
    // so processes started from different shells would miss each other.
    #[cfg(target_os = "macos")]
    if let Some(dir) = darwin_user_temp_dir() {
        return dir;
    }
    std::env::temp_dir().join(format!("temzdf-{}", rustix::process::getuid().as_raw()))
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
        io::Error::new(io::ErrorKind::PermissionDenied, format!("{}: {why}; remove it and try again", d.display()))
    };
    let mut d = dir;
    loop {
        let m = std::fs::symlink_metadata(d)?;
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

/// Session names: letters, digits, `.`, `_` and `-`, at most 64 characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
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
    #[cfg_attr(all(miri, target_os = "macos"), ignore = "Miri on macOS gives another user id than the files' owner")]
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
        // (Another user's directory cannot be made here; the owner check is
        // the same path as the symlink check.)
        std::fs::remove_dir_all(&tmp).unwrap();
    }
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
        round_trip(&ClientMsg::Hello { version: VERSION, geometry, hints: Hints::default() });
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
        round_trip(&ServerMsg::Frame { seq: 9, updates: images.take_updates(), frame, complete: true });
        round_trip(&ServerMsg::Effect(Effect::Copy("x".into())));
        round_trip(&ServerMsg::Exit(ExitReason::Error("bad".into())));
        round_trip(&ServerMsg::Done(Err("no".into())));
    }

    #[test]
    fn pixels_travel_as_bytes() {
        let px: Arc<[u8]> = vec![7u8; 4096].into();
        let mut images = Images::new();
        images.add(32, 32, px);
        let msg = ServerMsg::Frame { seq: 0, updates: images.take_updates(), frame: Frame::default(), complete: false };
        let mut buf = Vec::new();
        send(&mut buf, &msg).unwrap();
        assert!(buf.len() < 4096 + 64, "{} bytes", buf.len());
    }

    #[test]
    fn oversized_and_garbled_messages_are_errors() {
        let mut huge = (u32::MAX).to_le_bytes().to_vec();
        huge.extend([0; 8]);
        assert!(recv::<ClientMsg>(&mut huge.as_slice()).is_err());
        let garbled = [3u8, 0, 0, 0, 0xff, 0xff, 0xff];
        assert!(recv::<ClientMsg>(&mut garbled.as_slice()).is_err());
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
    }
}
