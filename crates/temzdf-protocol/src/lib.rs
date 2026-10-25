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

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use temzdf_scene::{Effect, Frame, Hints, ImageUpdate};
use termpix_core::geom::Geometry;
use termpix_core::input::{KeyEvent, MouseEvent};

/// Bumped on incompatible changes; client and server must agree.
pub const VERSION: u32 = 2;
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
    Resize(Geometry),
    /// The frame with this number is on screen: send the next one.
    FrameAck(u64),
    /// Something the frontend could not do (e.g. open a link).
    Report(String),
    Detach,
    /// Requests answered once (no Hello): describe the session, end it, or run
    /// an action (its text form, see `temzdf-server`'s `Action`).
    Info {
        version: u32,
    },
    Kill,
    Action(Vec<String>),
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
    pub panes: usize,
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
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
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

/// Where session sockets live (created, mode 0700).
pub fn sessions_dir() -> io::Result<PathBuf> {
    let dir = runtime_dir().join("temzdf").join("sessions");
    std::fs::create_dir_all(&dir)?;
    use std::os::unix::fs::PermissionsExt;
    for d in [dir.parent().unwrap_or(&dir), &dir] {
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

/// Session names: letters, digits, `.`, `_` and `-`, at most 64 characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn socket_path(name: &str) -> io::Result<PathBuf> {
    Ok(sessions_dir()?.join(format!("{name}.sock")))
}

#[cfg(test)]
mod tests {
    use super::*;
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
