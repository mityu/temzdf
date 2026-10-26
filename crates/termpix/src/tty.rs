//! Controlling terminal: raw mode, terminal modes, reading input, and reliable
//! restoration (on drop, on panic).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use rustix::event::Timespec;
use rustix::termios::{self, OptionalActions, Termios};

use crate::geom::{CellSize, Geometry};
use crate::input::{Event, Parser};
use crate::output::Output;

/// How long exiting waits for the terminal to take the reset sequence.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
/// How long exiting waits for replies to earlier queries (see [`drain_replies`]).
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// Primary device attributes: every terminal answers, in order with earlier replies.
const DA1: &[u8] = b"\x1b[c";

/// Set to stop the input thread (on exit), and whether it still runs.
static INPUT_STOP: AtomicBool = AtomicBool::new(false);
static INPUT_RUNNING: AtomicBool = AtomicBool::new(false);

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
        s.extend_from_slice(b"\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?2026l\x1b[?25h\x1b[0 q");
        if self.alternate_screen {
            s.extend_from_slice(b"\x1b[?1049l");
        }
        // Restore the saved window title.
        s.extend_from_slice(b"\x1b[23;0t");
        s
    }
}

/// What the panic hook needs to restore the terminal.
struct RestoreState {
    fd: rustix::fd::OwnedFd,
    termios: Termios,
    reset: Vec<u8>,
    /// Queued output, dropped before the reset is written.
    out: Option<Output<File>>,
    /// The thread that opened the terminal: its panic ends the program.
    owner: std::thread::ThreadId,
}

static RESTORE: Mutex<Option<RestoreState>> = Mutex::new(None);
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Put the terminal back. With `drain_input`, also swallow replies to queries
/// still on their way (graphics fences, probes): otherwise they arrive after
/// exit and the shell shows them as typed text (`Gi=8323307;OK`).
fn restore_now(drain_input: bool) {
    let Ok(mut guard) = RESTORE.lock() else { return };
    if let Some(state) = guard.take() {
        let mut last = state.reset.clone();
        if drain_input {
            last.extend_from_slice(DA1);
        }
        match &state.out {
            Some(out) => out.close(&last, CLOSE_TIMEOUT),
            None => {
                let _ = rustix::io::write(&state.fd, &last);
            }
        }
        if drain_input {
            drain_replies(state.fd.as_fd());
            let _ = termios::tcflush(&state.fd, termios::QueueSelector::IFlush);
        }
        let _ = termios::tcsetattr(&state.fd, OptionalActions::Now, &state.termios);
    }
}

/// Read and discard input until the reply to the DA1 query sent last (at most
/// [`REPLY_TIMEOUT`]). Terminals answer in order, so every earlier reply has
/// arrived by then.
fn drain_replies(fd: BorrowedFd<'_>) {
    let deadline = std::time::Instant::now() + REPLY_TIMEOUT;
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        match read_timeout(fd, &mut buf, Some(left)) {
            Ok(Some(n)) if n > 0 => seen.extend_from_slice(&buf[..n]),
            Ok(None) => continue,
            _ => return,
        }
        if has_da1_reply(&seen) {
            return;
        }
    }
}

/// Whether `bytes` contain a DA1 reply, `ESC [ ? <digits and ;> c`.
fn has_da1_reply(bytes: &[u8]) -> bool {
    bytes.windows(3).enumerate().any(|(i, w)| {
        w == b"\x1b[?" && {
            let rest = &bytes[i + 3..];
            let end = rest.iter().position(|b| !(b.is_ascii_digit() || *b == b';'));
            end.is_some_and(|e| rest[e] == b'c')
        }
    })
}

fn install_panic_hook() {
    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_after_panic();
        previous(info);
    }));
}

/// Put the terminal back for a panic on the thread that opened it, which
/// ends the program. Other threads' panics may be caught (`catch_unwind`
/// around a library that panics) or end only that thread: restoring then
/// would leave the program running with its screen gone and its output
/// dropped.
fn restore_after_panic() {
    let owner = RESTORE.lock().ok().and_then(|g| g.as_ref().map(|s| s.owner));
    if owner == Some(std::thread::current().id()) {
        restore_now(false);
    }
}

/// The controlling terminal in raw mode. Dropping it restores the terminal,
/// dropping output still queued (see [`crate::output`]).
///
/// Writes are queued and sent by a background thread, so they never block on a
/// slow terminal; [`Tty::pending_output`] tells how much is still on its way.
pub struct Tty {
    file: File,
    out: Output<File>,
}

impl Tty {
    /// Open `/dev/tty`, switch to raw mode and enable `modes`.
    pub fn open(modes: Modes) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        let original = termios::tcgetattr(&file)?;
        let restore_fd = file.as_fd().try_clone_to_owned()?;
        // `TERMPIX_TRACE=<path>`: copy of every byte written, for debugging.
        let trace = std::env::var_os("TERMPIX_TRACE").and_then(|p| File::create(p).ok());
        // A separate, non-blocking handle for writing (its own open file
        // description, so reads stay blocking): the writer never gets stuck in
        // a write, and exiting can always get the reset out.
        let writer = OpenOptions::new().write(true).open("/dev/tty")?;
        rustix::fs::fcntl_setfl(&writer, rustix::fs::OFlags::NONBLOCK)?;
        let out = Output::spawn(writer, trace)?;
        // Raw mode last: an error before leaves the terminal as it was.
        let mut raw = original.clone();
        raw.make_raw();
        termios::tcsetattr(&file, OptionalActions::Flush, &raw)?;
        *RESTORE.lock().unwrap_or_else(|e| e.into_inner()) = Some(RestoreState {
            fd: restore_fd,
            termios: original,
            reset: modes.disable_sequence(),
            out: Some(out.clone()),
            owner: std::thread::current().id(),
        });
        install_panic_hook();
        let mut tty = Self { file, out };
        tty.write_all(&modes.enable_sequence())?;
        Ok(tty)
    }

    /// Inside tmux: send the graphics cleanup done on exit through passthrough
    /// too (see [`crate::placeholder::wrap_for_tmux`]).
    pub fn set_tmux_passthrough(&mut self) {
        if let Some(state) = RESTORE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            state.reset = crate::placeholder::wrap_for_tmux(&state.reset);
        }
    }

    /// Turn SGR-pixel mouse coordinates (1016) on, typically after [`crate::probe`]
    /// confirmed support. Restoration on exit is updated accordingly.
    pub fn enable_pixel_mouse(&mut self) -> io::Result<()> {
        if let Some(state) = RESTORE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let mut reset = b"\x1b[?1016l".to_vec();
            reset.extend_from_slice(&state.reset);
            state.reset = reset;
        }
        self.write_all(b"\x1b[?1016h")
    }

    /// Queue `bytes` for the terminal; returns without waiting for them.
    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write(bytes)
    }

    /// Bytes queued but not yet written to the terminal.
    pub fn pending_output(&self) -> usize {
        self.out.pending()
    }

    /// Wait at most `timeout` until all queued output was written.
    pub fn drain(&self, timeout: Duration) -> bool {
        self.out.drain(timeout)
    }

    /// Read available bytes, waiting at most `timeout` (`None` = forever).
    /// Returns `None` on timeout and `Some(0)` at end of file.
    pub fn read_timeout(&self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
        read_timeout(self.file.as_fd(), buf, timeout)
    }

    /// Current size from the kernel. Pixel sizes may be zero.
    pub fn winsize(&self) -> io::Result<Winsize> {
        let ws = termios::tcgetwinsize(&self.file)?;
        Ok(Winsize { rows: ws.ws_row, cols: ws.ws_col, width_px: ws.ws_xpixel, height_px: ws.ws_ypixel })
    }

    /// A second handle for reading input on another thread.
    pub fn reader(&self) -> io::Result<TtyReader> {
        Ok(TtyReader { file: self.file.try_clone()? })
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        // Stop the input thread first: the replies drained below are not input.
        INPUT_STOP.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_millis(300);
        while INPUT_RUNNING.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        restore_now(true);
    }
}

impl Write for Tty {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out.write(buf)?;
        Ok(buf.len())
    }

    /// Wait until everything queued was written.
    fn flush(&mut self) -> io::Result<()> {
        while !self.out.drain(Duration::from_secs(1)) {}
        // Reports a failed write.
        self.out.write(b"")
    }
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

/// Wait until `fd` is readable (or writable), at most `timeout` (`None` =
/// forever). Returns whether it is.
///
/// `select`, not `poll`: macOS's `poll` does not support terminal devices
/// (it reports `POLLNVAL` for `/dev/tty`). One code path everywhere, so the
/// one macOS needs is the one exercised on Linux too.
pub(crate) fn wait_fd(fd: BorrowedFd<'_>, write: bool, timeout: Option<Duration>) -> io::Result<bool> {
    use rustix::event::{FdSetElement, fd_set_insert, fd_set_num_elements, select};
    use std::os::fd::AsRawFd;
    let raw = fd.as_raw_fd();
    let ts = timeout.map(|d| Timespec { tv_sec: d.as_secs() as _, tv_nsec: d.subsec_nanos() as _ });
    loop {
        let mut set = vec![FdSetElement::default(); fd_set_num_elements(1, raw + 1)];
        fd_set_insert(&mut set, raw);
        let (read_set, write_set) = if write { (None, Some(&mut set[..])) } else { (Some(&mut set[..]), None) };
        // SAFETY: the only fd in the set is `fd`, which is open (borrowed).
        match unsafe { select(raw + 1, read_set, write_set, None, ts.as_ref()) } {
            Ok(n) => return Ok(n > 0),
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

fn read_timeout(fd: BorrowedFd<'_>, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
    if !wait_fd(fd, false, timeout)? {
        return Ok(None);
    }
    match rustix::io::read(fd, buf) {
        Ok(n) => Ok(Some(n)),
        Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Read side of the terminal, for an input thread.
pub struct TtyReader {
    file: File,
}

impl Read for TtyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

/// How long to wait after a lone `ESC` before reporting it as the Escape key.
const ESC_TIMEOUT: Duration = Duration::from_millis(25);

/// Things the input thread reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    Input(Event),
    /// SIGWINCH: re-read the window size.
    WindowChanged,
    /// SIGTERM / SIGHUP: the application should exit.
    Terminate,
}

/// Spawn threads that parse terminal input and watch signals, sending events
/// through `map(event)` into `tx`. They stop when the receiver is dropped.
pub fn spawn_input<T: Send + 'static>(
    reader: TtyReader,
    pixel_mouse: bool,
    tx: Sender<T>,
    map: impl Fn(TermEvent) -> T + Send + Sync + Clone + 'static,
) -> io::Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGWINCH,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ])?;
    {
        let tx = tx.clone();
        let map = map.clone();
        std::thread::Builder::new().name("termpix-signals".into()).spawn(move || {
            for sig in signals.forever() {
                let ev =
                    if sig == signal_hook::consts::SIGWINCH { TermEvent::WindowChanged } else { TermEvent::Terminate };
                if tx.send(map(ev)).is_err() {
                    break;
                }
            }
        })?;
    }
    INPUT_STOP.store(false, Ordering::SeqCst);
    INPUT_RUNNING.store(true, Ordering::SeqCst);
    std::thread::Builder::new().name("termpix-input".into()).spawn(move || {
        let mut parser = Parser::new();
        parser.set_pixel_mouse(pixel_mouse);
        let mut buf = [0u8; 4096];
        // Wake up now and then to notice INPUT_STOP.
        let idle = Duration::from_millis(50);
        while !INPUT_STOP.load(Ordering::SeqCst) {
            let pending = parser.has_pending();
            let timeout = if pending { ESC_TIMEOUT } else { idle };
            let events = match read_timeout(reader.file.as_fd(), &mut buf, Some(timeout)) {
                Ok(None) if pending => parser.flush(),
                Ok(None) => continue,
                Ok(Some(0)) | Err(_) => break,
                Ok(Some(n)) => parser.feed(&buf[..n]),
            };
            for ev in events {
                if tx.send(map(TermEvent::Input(ev))).is_err() {
                    INPUT_RUNNING.store(false, Ordering::SeqCst);
                    return;
                }
            }
        }
        INPUT_RUNNING.store(false, Ordering::SeqCst);
    })?;
    Ok(())
}

/// Redirects the process's stdout and stderr (file descriptors 1 and 2) to a
/// file while alive, and restores them when dropped.
///
/// A full-screen UI drawn through `/dev/tty` would be corrupted by anything a
/// library prints to stdout/stderr (C libraries often do); with this guard such
/// output lands in the log file instead.
pub struct StdioRedirect {
    saved_out: rustix::fd::OwnedFd,
    saved_err: rustix::fd::OwnedFd,
}

impl StdioRedirect {
    /// Append stdout and stderr to `log` (created if missing).
    pub fn to_file(log: &std::path::Path) -> io::Result<Self> {
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(log)?;
        let saved_out = rustix::io::dup(rustix::stdio::stdout())?;
        let saved_err = rustix::io::dup(rustix::stdio::stderr())?;
        rustix::stdio::dup2_stdout(&file)?;
        rustix::stdio::dup2_stderr(&file)?;
        Ok(Self { saved_out, saved_err })
    }
}

impl Drop for StdioRedirect {
    fn drop(&mut self) {
        let _ = io::stdout().flush();
        let _ = rustix::stdio::dup2_stdout(&self.saved_out);
        let _ = rustix::stdio::dup2_stderr(&self.saved_err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_panic_on_the_terminals_thread_restores_it() {
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let slave = OpenOptions::new().read(true).write(true).open(name.to_str().unwrap()).unwrap();
        let termios = termios::tcgetattr(&slave).unwrap();
        *RESTORE.lock().unwrap() = Some(RestoreState {
            fd: slave.into(),
            termios,
            reset: b"RESET".to_vec(),
            out: None,
            owner: std::thread::current().id(),
        });
        install_panic_hook();
        // A panic another thread catches (a library's, around a worker's
        // call) leaves the terminal as it is.
        std::thread::spawn(|| {
            let _ = std::panic::catch_unwind(|| panic!("caught on a worker"));
        })
        .join()
        .unwrap();
        assert!(RESTORE.lock().unwrap().is_some(), "still the program's");
        // A panic on the terminal's thread ends the program: restored.
        let _ = std::panic::catch_unwind(|| panic!("on the terminal's thread"));
        assert!(RESTORE.lock().unwrap().is_none());
        let mut buf = [0u8; 64];
        let n = read_timeout(master.as_fd(), &mut buf, Some(Duration::from_secs(1))).unwrap().unwrap_or(0);
        assert_eq!(&buf[..n], b"RESET");
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

    #[test]
    fn wait_fd_reports_readiness_and_times_out() {
        use std::io::Write;
        let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        let t = std::time::Instant::now();
        assert!(!wait_fd(a.as_fd(), false, Some(Duration::from_millis(30))).unwrap());
        assert!(t.elapsed() >= Duration::from_millis(25));
        assert!(wait_fd(a.as_fd(), true, Some(Duration::ZERO)).unwrap(), "an empty socket is writable");
        b.write_all(b"x").unwrap();
        assert!(wait_fd(a.as_fd(), false, Some(Duration::from_secs(1))).unwrap());
        let mut buf = [0u8; 4];
        assert_eq!(read_timeout(a.as_fd(), &mut buf, Some(Duration::ZERO)).unwrap(), Some(1));
    }

    #[test]
    fn finds_the_da1_reply_behind_other_replies() {
        assert!(has_da1_reply(b"\x1b_Gi=8323307;OK\x1b\\\x1b[?62;22;52c"));
        assert!(has_da1_reply(b"\x1b[?1c"));
        assert!(!has_da1_reply(b"\x1b_Gi=8323307;OK\x1b\\\x1b[?62;22"));
        assert!(!has_da1_reply(b"\x1b[?2026;2$y"));
    }
}
