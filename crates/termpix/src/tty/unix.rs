//! The terminal on Unix: termios raw mode, `select` on the file
//! descriptor, SIGWINCH through signal-hook.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use rustix::event::Timespec;
use rustix::termios::{self, OptionalActions, Termios};

use super::{CLOSE_TIMEOUT, DA1, GRAPHICS_CLEANUP, InputSource, Modes, TermEvent, Winsize};
use crate::output::Output;

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
            super::drain_replies(|buf, timeout| read_timeout(state.fd.as_fd(), buf, Some(timeout)));
            let _ = termios::tcflush(&state.fd, termios::QueueSelector::IFlush);
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

    /// The terminal has no graphics (or they are turned off): no graphics
    /// cleanup on exit either.
    pub fn without_graphics(&mut self) {
        if let Some(state) = RESTORE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let cleanup: &[u8] = GRAPHICS_CLEANUP;
            if let Some(i) = state.reset.windows(cleanup.len()).position(|w| w == cleanup) {
                state.reset.drain(i..i + cleanup.len());
            }
        }
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
        super::stop_input();
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

impl InputSource for TtyReader {
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
        read_timeout(self.file.as_fd(), buf, Some(timeout))
    }
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
    super::spawn_input_thread(reader, pixel_mouse, tx, map)
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
    /// Append stdout and stderr to `log` (created if missing, 0600).
    pub fn to_file(log: &std::path::Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Only the user's: what a program prints can say more than it should.
        let file = OpenOptions::new().create(true).append(true).mode(0o600).open(log)?;
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
    #[cfg_attr(miri, ignore = "Miri cannot poll file descriptors")]
    fn the_application_is_told_when_input_ends() {
        let (tx, rx) = std::sync::mpsc::channel();
        // The terminal went (end of file), keys still queued before it.
        let (reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"q").unwrap();
        drop(writer);
        let mut reader = TtyReader { file: File::from(std::os::fd::OwnedFd::from(reader)) };
        super::super::input_loop(&mut reader, false, &tx, &|e| e, &AtomicBool::new(false));
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(events[..], [TermEvent::Input(_), TermEvent::Terminate]), "{events:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a tty")]
    fn only_a_panic_on_the_terminals_thread_restores_it() {
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let slave = OpenOptions::new().read(true).write(true).open(name.to_str().unwrap()).unwrap();
        let termios = termios::tcgetattr(&slave).unwrap();
        // The terminal stays open after the restore, as a program's does
        // (macOS drops what is unread when the last slave descriptor closes).
        let _still_open = slave.try_clone().unwrap();
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
    #[cfg_attr(miri, ignore = "Miri cannot call select")]
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
}
