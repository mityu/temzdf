//! Controlling terminal: raw mode, terminal modes, reading input, and reliable
//! restoration (on drop, on panic).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::termios::{self, OptionalActions, Termios};

use crate::geom::{CellSize, Geometry};
use crate::input::{Event, Parser};
use crate::output::Output;

/// How long exiting waits for the chunk being written before giving up on a
/// terminal that stopped reading.
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

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
        s.extend_from_slice(b"\x1b[?25l\x1b[2J");
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
        s.extend_from_slice(b"\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?2026l\x1b[?25h");
        if self.alternate_screen {
            s.extend_from_slice(b"\x1b[?1049l");
        }
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
}

static RESTORE: Mutex<Option<RestoreState>> = Mutex::new(None);
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

fn restore_now() {
    let Ok(mut guard) = RESTORE.lock() else { return };
    if let Some(state) = guard.take() {
        match &state.out {
            Some(out) => out.close(&state.reset, CLOSE_TIMEOUT),
            None => {
                let _ = rustix::io::write(&state.fd, &state.reset);
            }
        }
        let _ = termios::tcsetattr(&state.fd, OptionalActions::Now, &state.termios);
    }
}

fn install_panic_hook() {
    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_now();
        previous(info);
    }));
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
        let mut raw = original.clone();
        raw.make_raw();
        termios::tcsetattr(&file, OptionalActions::Flush, &raw)?;
        let restore_fd = file.as_fd().try_clone_to_owned()?;
        // `TERMPIX_TRACE=<path>`: copy of every byte written, for debugging.
        let trace = std::env::var_os("TERMPIX_TRACE").and_then(|p| File::create(p).ok());
        let out = Output::spawn(file.try_clone()?, trace)?;
        *RESTORE.lock().unwrap_or_else(|e| e.into_inner()) = Some(RestoreState {
            fd: restore_fd,
            termios: original,
            reset: modes.disable_sequence(),
            out: Some(out.clone()),
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
        restore_now();
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
}

fn read_timeout(fd: BorrowedFd<'_>, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    let ts = timeout.map(|d| Timespec { tv_sec: d.as_secs() as _, tv_nsec: d.subsec_nanos() as _ });
    loop {
        match rustix::event::poll(&mut fds, ts.as_ref()) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
        }
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
    std::thread::Builder::new().name("termpix-input".into()).spawn(move || {
        let mut parser = Parser::new();
        parser.set_pixel_mouse(pixel_mouse);
        let mut buf = [0u8; 4096];
        loop {
            let timeout = parser.has_pending().then_some(ESC_TIMEOUT);
            let events = match read_timeout(reader.file.as_fd(), &mut buf, timeout) {
                Ok(None) if timeout.is_some() => parser.flush(),
                Ok(None) => continue,
                Ok(Some(0)) | Err(_) => break,
                Ok(Some(n)) => parser.feed(&buf[..n]),
            };
            for ev in events {
                if tx.send(map(TermEvent::Input(ev))).is_err() {
                    return;
                }
            }
        }
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
