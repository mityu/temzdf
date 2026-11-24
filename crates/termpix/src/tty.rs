//! Controlling terminal: raw mode, terminal modes, reading input, and reliable
//! restoration (on drop, on panic).
//!
//! The terminal itself is `unix` (termios, `/dev/tty`, signals) or
//! `windows` (the console, `CONIN$` and `CONOUT$`), with the same API. What
//! is shared lives here: the modes' sequences, the input thread's loop and
//! the wait for replies on exit.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::geom::{CellSize, Geometry};
use crate::input::{Event, Parser};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::wait_fd;
#[cfg(unix)]
pub use unix::{StdioRedirect, Tty, TtyReader, spawn_input};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{StdioRedirect, Tty, TtyReader, spawn_input};

/// Which terminal modes to enable. Defaults enable everything termpix uses.
#[derive(Clone, Copy, Debug)]
pub struct Modes {
    pub alternate_screen: bool,
    /// Any-motion mouse tracking (1003) with SGR encoding (1006).
    pub mouse: bool,
    /// SGR-pixel mouse coordinates (1016). Only enable when the terminal supports it.
    pub pixel_mouse: bool,
    /// Kitty keyboard protocol flags to push (`CSI > flags u`); 0 to skip.
    pub keyboard_flags: u32,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    /// In-band resize reports (2048).
    pub resize_reports: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            alternate_screen: true,
            mouse: true,
            pixel_mouse: false,
            keyboard_flags: 31,
            bracketed_paste: true,
            focus_events: true,
            resize_reports: true,
        }
    }
}

impl Modes {
    fn enable_sequence(&self) -> Vec<u8> {
        let mut s = Vec::new();
        if self.alternate_screen {
            s.extend_from_slice(b"\x1b[?1049h");
        }
        // Save the window title (XTWINOPS 22); applications retitle the window.
        s.extend_from_slice(b"\x1b[22;0t\x1b[?25l\x1b[2J");
        if self.mouse {
            s.extend_from_slice(b"\x1b[?1003h\x1b[?1006h");
        }
        if self.pixel_mouse {
            s.extend_from_slice(b"\x1b[?1016h");
        }
        if self.keyboard_flags != 0 {
            s.extend_from_slice(format!("\x1b[>{}u", self.keyboard_flags).as_bytes());
        }
        if self.bracketed_paste {
            s.extend_from_slice(b"\x1b[?2004h");
        }
        if self.focus_events {
            s.extend_from_slice(b"\x1b[?1004h");
        }
        if self.resize_reports {
            s.extend_from_slice(b"\x1b[?2048h");
        }
        s
    }

    fn disable_sequence(&self) -> Vec<u8> {
        let mut s = Vec::new();
        if self.resize_reports {
            s.extend_from_slice(b"\x1b[?2048l");
        }
        if self.focus_events {
            s.extend_from_slice(b"\x1b[?1004l");
        }
        if self.bracketed_paste {
            s.extend_from_slice(b"\x1b[?2004l");
        }
        if self.keyboard_flags != 0 {
            s.extend_from_slice(b"\x1b[<u");
        }
        if self.pixel_mouse {
            s.extend_from_slice(b"\x1b[?1016l");
        }
        if self.mouse {
            s.extend_from_slice(b"\x1b[?1006l\x1b[?1003l");
        }
        // Free every image we may have left, end any pending synchronized update.
        // Back to the default cursor shape, in case it was changed.
        s.extend_from_slice(GRAPHICS_CLEANUP);
        s.extend_from_slice(b"\x1b[?2026l\x1b[?25h\x1b[0 q");
        if self.alternate_screen {
            s.extend_from_slice(b"\x1b[?1049l");
        }
        // Restore the saved window title.
        s.extend_from_slice(b"\x1b[23;0t");
        s
    }
}

/// How long exiting waits for the terminal to take the reset sequence.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
/// How long exiting waits for replies to earlier queries (see [`drain_replies`]).
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// Primary device attributes: every terminal answers, in order with earlier replies.
const DA1: &[u8] = b"\x1b[c";
/// Delete every image we placed (on exit).
const GRAPHICS_CLEANUP: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

/// How long replies may still come after a DA1 answer. On Windows the
/// console's ConPTY (an older one: the in-box one, WezTerm's) answers DA1
/// itself at once and passes the other queries on to the terminal, whose
/// answers come some 30 ms later (`doc/terminals/conpty.md`, "Inside Windows
/// Terminal and WezTerm"). Elsewhere the terminal answers in order: none.
pub(crate) const LATE_REPLIES: Duration = if cfg!(windows) { Duration::from_millis(200) } else { Duration::ZERO };

/// Read and discard input until the reply to the DA1 query sent last (at
/// most [`REPLY_TIMEOUT`]), then while more comes within [`LATE_REPLIES`]
/// (Windows). Terminals answer in order, so every earlier reply has arrived
/// by then. `read` reads as `Tty::read_timeout` does.
fn drain_replies(mut read: impl FnMut(&mut [u8], Duration) -> io::Result<Option<usize>>) {
    let deadline = Instant::now() + REPLY_TIMEOUT;
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    let mut da1 = false;
    loop {
        let mut left = deadline.saturating_duration_since(Instant::now());
        if da1 {
            left = left.min(LATE_REPLIES);
        }
        if left.is_zero() {
            return;
        }
        match read(&mut buf, left) {
            Ok(Some(n)) if n > 0 => seen.extend_from_slice(&buf[..n]),
            // Quiet for LATE_REPLIES after the DA1 answer: done.
            Ok(None) if da1 => return,
            Ok(None) => continue,
            _ => return,
        }
        if !da1 && has_da1_reply(&seen) {
            da1 = true;
        }
    }
}

/// Whether `bytes` contain a DA1 reply, `ESC [ ? <digits and ;> c`.
pub(crate) fn has_da1_reply(bytes: &[u8]) -> bool {
    bytes.windows(3).enumerate().any(|(i, w)| {
        w == b"\x1b[?" && {
            let rest = &bytes[i + 3..];
            let end = rest.iter().position(|b| !(b.is_ascii_digit() || *b == b';'));
            end.is_some_and(|e| rest[e] == b'c')
        }
    })
}

/// Set to stop the input thread (on exit), and whether it still runs.
static INPUT_STOP: AtomicBool = AtomicBool::new(false);
static INPUT_RUNNING: AtomicBool = AtomicBool::new(false);

/// Stop the input thread, waiting a moment for it (before the replies to
/// the last queries are read on exit: they are not input).
fn stop_input() {
    INPUT_STOP.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_millis(300);
    while INPUT_RUNNING.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// How long to wait after a lone `ESC` before reporting it as the Escape key.
const ESC_TIMEOUT: Duration = Duration::from_millis(25);

/// Where the input thread reads from.
trait InputSource {
    /// Read available bytes, waiting at most `timeout`: `None` on timeout,
    /// `Some(0)` at end of file.
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>>;

    /// Whether the window's size changed since the last call (where no
    /// signal says so: Windows).
    fn resized(&mut self) -> bool {
        false
    }
}

/// Run [`input_loop`] on a new thread.
fn spawn_input_thread<T: Send + 'static>(
    mut source: impl InputSource + Send + 'static,
    pixel_mouse: bool,
    tx: Sender<T>,
    map: impl Fn(TermEvent) -> T + Send + 'static,
) -> io::Result<()> {
    INPUT_STOP.store(false, Ordering::SeqCst);
    INPUT_RUNNING.store(true, Ordering::SeqCst);
    std::thread::Builder::new().name("termpix-input".into()).spawn(move || {
        input_loop(&mut source, pixel_mouse, &tx, &map, &INPUT_STOP);
        INPUT_RUNNING.store(false, Ordering::SeqCst);
    })?;
    Ok(())
}

/// Parse input from `source` into `tx` until `stop` (or nobody listens).
/// Ending otherwise (a read error, end of file, a panic) leaves the
/// application without keys, in raw mode where Ctrl-C sends no signal: it
/// is told to exit ([`TermEvent::Terminate`]).
fn input_loop<T>(
    source: &mut impl InputSource,
    pixel_mouse: bool,
    tx: &Sender<T>,
    map: &impl Fn(TermEvent) -> T,
    stop: &AtomicBool,
) {
    /// Sends `Terminate` when dropped armed, unwinding included.
    struct Guard<'a, T, F: Fn(TermEvent) -> T> {
        tx: &'a Sender<T>,
        map: &'a F,
        armed: bool,
    }
    impl<T, F: Fn(TermEvent) -> T> Drop for Guard<'_, T, F> {
        fn drop(&mut self) {
            if self.armed {
                let _ = self.tx.send((self.map)(TermEvent::Terminate));
            }
        }
    }
    let mut guard = Guard { tx, map, armed: true };
    let mut parser = Parser::new();
    parser.set_pixel_mouse(pixel_mouse);
    let mut buf = [0u8; 4096];
    // Wake up now and then to notice `stop` (and, on Windows, a resize).
    let idle = Duration::from_millis(50);
    while !stop.load(Ordering::SeqCst) {
        let pending = parser.has_pending();
        let timeout = if pending { ESC_TIMEOUT } else { idle };
        let read = source.read(&mut buf, timeout);
        let mut events = Vec::new();
        if source.resized() {
            events.push(TermEvent::WindowChanged);
        }
        let input = match read {
            Ok(None) if pending => parser.flush(),
            Ok(None) => Vec::new(),
            Ok(Some(0)) | Err(_) => {
                // Still say what came.
                for ev in events {
                    let _ = tx.send(map(ev));
                }
                return;
            }
            Ok(Some(n)) => parser.feed(&buf[..n]),
        };
        events.extend(input.into_iter().map(TermEvent::Input));
        for ev in events {
            if tx.send(map(ev)).is_err() {
                guard.armed = false;
                return;
            }
        }
    }
    guard.armed = false;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Winsize {
    pub rows: u16,
    pub cols: u16,
    pub width_px: u16,
    pub height_px: u16,
}

impl Winsize {
    /// Geometry derived from the kernel's pixel size, if it reports one.
    pub fn geometry(&self) -> Option<Geometry> {
        if self.rows == 0 || self.cols == 0 || self.width_px == 0 || self.height_px == 0 {
            return None;
        }
        Some(Geometry {
            cols: self.cols,
            rows: self.rows,
            cell: CellSize {
                width: self.width_px as u32 / self.cols as u32,
                height: self.height_px as u32 / self.rows as u32,
            },
        })
    }

    /// [`Winsize::geometry`], or where the kernel reports no pixel size
    /// (mosh, WezTerm on WSL, some multiplexers) the new grid with the cell
    /// size known so far.
    pub fn geometry_or(&self, cell: CellSize) -> Option<Geometry> {
        if self.rows == 0 || self.cols == 0 {
            return None;
        }
        Some(self.geometry().unwrap_or(Geometry { cols: self.cols, rows: self.rows, cell }))
    }
}

/// Things the input thread reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    Input(Event),
    /// SIGWINCH (on Windows: the console's window size changed): re-read
    /// the window size.
    WindowChanged,
    /// SIGTERM / SIGHUP, or the input thread ended (the terminal went, or
    /// a bug): the application should exit.
    Terminate,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Input from a list of reads (`None`: a timeout); the end of the list
    /// is the end of file.
    struct Script(std::collections::VecDeque<(Option<&'static [u8]>, bool)>);

    impl InputSource for Script {
        fn read(&mut self, buf: &mut [u8], _timeout: Duration) -> io::Result<Option<usize>> {
            Ok(match self.0.front() {
                None => Some(0),
                Some((None, _)) => None,
                Some((Some(bytes), _)) => {
                    buf[..bytes.len()].copy_from_slice(bytes);
                    Some(bytes.len())
                }
            })
        }

        fn resized(&mut self) -> bool {
            self.0.pop_front().is_some_and(|(_, resized)| resized)
        }
    }

    #[test]
    fn the_application_is_told_when_input_ends_and_of_resizes() {
        let (tx, rx) = std::sync::mpsc::channel();
        // A resize noticed with a timeout, then with a key; then the
        // terminal went (end of file).
        let script = [(None, true), (Some(&b"q"[..]), true), (None, false)];
        input_loop(&mut Script(script.into()), false, &tx, &|e| e, &AtomicBool::new(false));
        let events: Vec<_> = rx.try_iter().collect();
        assert!(
            matches!(
                events[..],
                [TermEvent::WindowChanged, TermEvent::WindowChanged, TermEvent::Input(_), TermEvent::Terminate]
            ),
            "{events:?}"
        );
        // Stopped on purpose: nothing.
        input_loop(&mut Script([(None, true)].into()), false, &tx, &|e| e, &AtomicBool::new(true));
        assert_eq!(rx.try_iter().count(), 0);
    }

    #[test]
    fn exiting_waits_for_late_replies_only_where_they_come() {
        // The DA1 answer, then a reply of the terminal behind ConPTY's.
        let mut reads = vec![Some(&b"\x1b[?61;6;7c"[..]), Some(&b"\x1b[6;22;10t"[..]), None].into_iter();
        let mut asked = Vec::new();
        drain_replies(|buf, timeout| {
            asked.push(timeout);
            Ok(reads.next().flatten().map(|b| {
                buf[..b.len()].copy_from_slice(b);
                b.len()
            }))
        });
        if LATE_REPLIES.is_zero() {
            assert_eq!(asked.len(), 1, "stops at the DA1 answer");
        } else {
            assert_eq!(asked.len(), 3, "reads until quiet");
            assert!(asked[1..].iter().all(|t| *t <= LATE_REPLIES), "{asked:?}");
        }
    }

    #[test]
    fn finds_the_da1_reply_behind_other_replies() {
        assert!(has_da1_reply(b"\x1b_Gi=8323307;OK\x1b\\\x1b[?62;22;52c"));
        assert!(has_da1_reply(b"\x1b[?1c"));
        assert!(!has_da1_reply(b"\x1b_Gi=8323307;OK\x1b\\\x1b[?62;22"));
        assert!(!has_da1_reply(b"\x1b[?2026;2$y"));
    }

    #[test]
    fn resizes_without_pixel_sizes_keep_the_cell_size() {
        let cell = CellSize { width: 9, height: 18 };
        let with_px = Winsize { rows: 30, cols: 100, width_px: 1000, height_px: 600 };
        assert_eq!(with_px.geometry_or(cell).unwrap().cell, CellSize { width: 10, height: 20 });
        let without = Winsize { rows: 40, cols: 120, width_px: 0, height_px: 0 };
        assert_eq!(without.geometry(), None);
        assert_eq!(without.geometry_or(cell), Some(Geometry { cols: 120, rows: 40, cell }));
        assert_eq!(Winsize { rows: 0, cols: 0, width_px: 0, height_px: 0 }.geometry_or(cell), None);
        let report = crate::input::ResizeReport { rows: 40, cols: 120, height_px: 800, width_px: 1200 };
        assert_eq!(report.geometry(), Some(Geometry { cols: 120, rows: 40, cell: CellSize { width: 10, height: 20 } }));
    }
}
