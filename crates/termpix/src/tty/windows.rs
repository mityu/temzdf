//! The terminal on Windows: the process's console (`CONIN$`, `CONOUT$`) in
//! virtual terminal mode, so the terminal's escape sequences pass both ways
//! as on Unix. The terminal is whatever runs the console through its ConPTY
//! (Windows Terminal, WezTerm, ...): see `doc/terminals/conpty.md`.
//!
//! - Input: `ENABLE_VIRTUAL_TERMINAL_INPUT` without line input, echo and
//!   processed input (Ctrl+C comes as the byte 0x03, as in raw mode on
//!   Unix), read as UTF-16 with `ReadConsoleW` and turned into UTF-8 here
//!   (no input code page involved). Waiting with a timeout is
//!   `WaitForSingleObject` on the input handle, which is signalled for any
//!   input record: those that give no character (key releases, a modifier
//!   alone, focus, the window's size) are read and dropped first, so a read
//!   never blocks past its timeout on them.
//! - Output: `ENABLE_VIRTUAL_TERMINAL_PROCESSING` and
//!   `DISABLE_NEWLINE_AUTO_RETURN`, output code page UTF-8, written by
//!   [`Output`]'s thread with `WriteFile`. A console cannot write without
//!   blocking, so a terminal that stops reading holds the writer in its
//!   write, and exiting waits for it ([`Output::close`]'s timeout does not
//!   bound that here).
//! - Resizes: no signal; the input thread looks at the console's window
//!   size each time it wakes (at least every 50 ms), and a
//!   `WINDOW_BUFFER_SIZE_EVENT` wakes it ([`super::TermEvent::WindowChanged`]).
//! - The console tells no pixel sizes: [`Tty::winsize`] gives the grid
//!   only, and the cell size comes from the terminal (`CSI 16 t`, see
//!   [`crate::probe`]).
//!
//! - Control events: Ctrl+Break, the console's window closing, logging off
//!   and shutting down end the program as SIGTERM and SIGHUP do on Unix
//!   ([`super::TermEvent::Terminate`], [`spawn_input`]), not at once, so
//!   that the console is put back (Ctrl+C is a key here, no event).
//!
//! The modes and code page are put back on drop and on a panic of the
//! thread that opened the console, as on Unix.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS,
    ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT, FlushConsoleInputBuffer,
    GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle, INPUT_RECORD, KEY_EVENT,
    PeekConsoleInputW, ReadConsoleInputW, ReadConsoleW, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE,
    SetConsoleCtrlHandler, SetConsoleMode, SetConsoleOutputCP, SetStdHandle,
};
use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};
use windows_sys::core::BOOL;

use super::{CLOSE_TIMEOUT, DA1, GRAPHICS_CLEANUP, InputSource, Modes, TermEvent, Winsize};
use crate::output::{Output, OutputSink};

/// UTF-8, as a console code page.
const CP_UTF8: u32 = 65001;

fn handle(h: &impl AsRawHandle) -> HANDLE {
    h.as_raw_handle() as HANDLE
}

fn check(ok: windows_sys::core::BOOL) -> io::Result<()> {
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

fn console_mode(h: HANDLE) -> io::Result<CONSOLE_MODE> {
    let mut mode = 0;
    // SAFETY: `mode` is a valid out pointer; a bad handle only fails.
    check(unsafe { GetConsoleMode(h, &mut mode) })?;
    Ok(mode)
}

fn set_console_mode(h: HANDLE, mode: CONSOLE_MODE) -> io::Result<()> {
    // SAFETY: plain value arguments; a bad handle only fails.
    check(unsafe { SetConsoleMode(h, mode) })
}

/// The input mode: VT input, no line editing, echo or Ctrl+C signal, no
/// mouse records or QuickEdit (the mouse comes as VT input), window size
/// records (they wake the input thread).
fn raw_input_mode(original: CONSOLE_MODE) -> CONSOLE_MODE {
    (original
        & !(ENABLE_LINE_INPUT
            | ENABLE_ECHO_INPUT
            | ENABLE_PROCESSED_INPUT
            | ENABLE_MOUSE_INPUT
            | ENABLE_QUICK_EDIT_MODE))
        | ENABLE_VIRTUAL_TERMINAL_INPUT
        | ENABLE_WINDOW_INPUT
        | ENABLE_EXTENDED_FLAGS
}

/// The output mode: escape sequences interpreted, line feeds as written.
fn raw_output_mode(original: CONSOLE_MODE) -> CONSOLE_MODE {
    original | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN
}

/// The console's output handle, written by [`Output`]'s thread.
struct ConsoleOut(File);

impl Write for ConsoleOut {
    /// Writes whole UTF-8 characters only (the console decodes each write
    /// on its own), unless what is left is not one.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let whole = utf8_prefix(buf);
        self.0.write(if whole == 0 { buf } else { &buf[..whole] })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Blocking writes: the default wait (they never return `WouldBlock`).
impl OutputSink for ConsoleOut {}

/// The length of `buf` without an incomplete UTF-8 character at its end.
fn utf8_prefix(buf: &[u8]) -> usize {
    // A character is at most 4 bytes: look at the last 3 for its start.
    for back in 1..=buf.len().min(3) {
        let b = buf[buf.len() - back];
        if b & 0xc0 == 0x80 {
            continue; // a continuation byte
        }
        let len = match b {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        return if len > back { buf.len() - back } else { buf.len() };
    }
    buf.len()
}

/// What the panic hook needs to restore the console.
struct RestoreState {
    input: File,
    output: File,
    input_mode: CONSOLE_MODE,
    output_mode: CONSOLE_MODE,
    output_cp: u32,
    reset: Vec<u8>,
    out: Output<ConsoleOut>,
    /// The thread that opened the console: its panic ends the program.
    owner: std::thread::ThreadId,
}

static RESTORE: Mutex<Option<RestoreState>> = Mutex::new(None);
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Put the console back. With `drain_input`, also swallow replies to queries
/// still on their way (see the Unix version).
fn restore_now(drain_input: bool) {
    let Ok(mut guard) = RESTORE.lock() else { return };
    if let Some(state) = guard.take() {
        let mut last = state.reset.clone();
        if drain_input {
            last.extend_from_slice(DA1);
        }
        state.out.close(&last, CLOSE_TIMEOUT);
        let input = handle(&state.input);
        if drain_input {
            super::drain_replies(|buf, timeout| read_console(input, buf, Some(timeout)));
            // SAFETY: an open console input handle.
            unsafe { FlushConsoleInputBuffer(input) };
        }
        let _ = set_console_mode(input, state.input_mode);
        let _ = set_console_mode(handle(&state.output), state.output_mode);
        // SAFETY: a plain value.
        unsafe { SetConsoleOutputCP(state.output_cp) };
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

/// Put the console back for a panic on the thread that opened it (see the
/// Unix version for why only that one).
fn restore_after_panic() {
    let owner = RESTORE.lock().ok().and_then(|g| g.as_ref().map(|s| s.owner));
    if owner == Some(std::thread::current().id()) {
        restore_now(false);
    }
}

/// The process's console in VT mode. Dropping it restores the console,
/// dropping output still queued (see [`crate::output`]).
///
/// Writes are queued and sent by a background thread; [`Tty::pending_output`]
/// tells how much is still on its way.
pub struct Tty {
    input: File,
    output: File,
    out: Output<ConsoleOut>,
}

/// Open the console's input or output (`CONIN$`, `CONOUT$`): the console's
/// own, whatever the standard handles were redirected to.
fn open_console(name: &str) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(name)
}

impl Tty {
    /// Open the console, switch it to VT mode and enable `modes`.
    pub fn open(modes: Modes) -> io::Result<Self> {
        let input = open_console("CONIN$")?;
        let output = open_console("CONOUT$")?;
        let input_mode = console_mode(handle(&input))?;
        let output_mode = console_mode(handle(&output))?;
        // SAFETY: no arguments.
        let output_cp = unsafe { GetConsoleOutputCP() };
        // `TERMPIX_TRACE=<path>`: copy of every byte written, for debugging.
        let trace = std::env::var_os("TERMPIX_TRACE").and_then(|p| File::create(p).ok());
        let out = Output::spawn(ConsoleOut(output.try_clone()?), trace)?;
        // The modes last: an error before leaves the console as it was.
        let restore = RestoreState {
            input: input.try_clone()?,
            output: output.try_clone()?,
            input_mode,
            output_mode,
            output_cp,
            reset: modes.disable_sequence(),
            out: out.clone(),
            owner: std::thread::current().id(),
        };
        let set = || -> io::Result<()> {
            set_console_mode(handle(&output), raw_output_mode(output_mode))?;
            // SAFETY: a plain value.
            check(unsafe { SetConsoleOutputCP(CP_UTF8) })?;
            set_console_mode(handle(&input), raw_input_mode(input_mode))?;
            // SAFETY: an open console input handle. What was typed before
            // is not for us.
            check(unsafe { FlushConsoleInputBuffer(handle(&input)) })
        };
        if let Err(e) = set() {
            let _ = set_console_mode(handle(&input), input_mode);
            let _ = set_console_mode(handle(&output), output_mode);
            // SAFETY: a plain value.
            unsafe { SetConsoleOutputCP(output_cp) };
            return Err(e);
        }
        *RESTORE.lock().unwrap_or_else(|e| e.into_inner()) = Some(restore);
        install_panic_hook();
        let mut tty = Self { input, output, out };
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
    /// Returns `None` on timeout.
    pub fn read_timeout(&self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
        read_console(handle(&self.input), buf, timeout)
    }

    /// The console window's size in cells; no pixel sizes.
    pub fn winsize(&self) -> io::Result<Winsize> {
        window_size(handle(&self.output))
    }

    /// A second handle for reading input on another thread.
    pub fn reader(&self) -> io::Result<TtyReader> {
        let output = self.output.try_clone()?;
        let last = window_size(handle(&output)).ok();
        Ok(TtyReader { input: self.input.try_clone()?, output, last })
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        // Control events from now on end the program at once, as without a
        // console to put back.
        *CONTROL.lock().unwrap_or_else(|e| e.into_inner()) = None;
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

fn window_size(output: HANDLE) -> io::Result<Winsize> {
    // SAFETY: plain data, zero is a valid value.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid out pointer.
    check(unsafe { GetConsoleScreenBufferInfo(output, &mut info) })?;
    let w = info.srWindow;
    let span = |a: i16, b: i16| u16::try_from(i32::from(b) - i32::from(a) + 1).unwrap_or(0);
    Ok(Winsize { rows: span(w.Top, w.Bottom), cols: span(w.Left, w.Right), width_px: 0, height_px: 0 })
}

/// What the input records waiting say.
#[derive(Debug, Default, PartialEq, Eq)]
struct Pending {
    /// Records before the first that gives characters: read and dropped.
    skip: usize,
    /// Whether a record gives characters ([`gives_characters`]).
    characters: bool,
}

fn pending(records: &[INPUT_RECORD]) -> Pending {
    let mut p = Pending::default();
    for r in records {
        if gives_characters(r) {
            p.characters = true;
            break;
        }
        p.skip += 1;
    }
    p
}

/// Whether reading `r` as text gives characters: a key press with a
/// character, or of a key VT input encodes (arrows, function keys: the
/// console turns them into escape sequences as they are read), not a
/// modifier alone.
fn gives_characters(r: &INPUT_RECORD) -> bool {
    if u32::from(r.EventType) != KEY_EVENT {
        return false;
    }
    // SAFETY: a key event's record holds a KeyEvent.
    let key = unsafe { r.Event.KeyEvent };
    // SAFETY: both members of the union are a u16.
    let ch = unsafe { key.uChar.UnicodeChar };
    const MODIFIERS: [u16; 11] = [
        0x10, 0x11, 0x12, // Shift, Control, Alt
        0x14, 0x90, 0x91, // Caps Lock, Num Lock, Scroll Lock
        0x5b, 0x5c, // the Windows keys
        0xa0, 0xa2, 0xa4, // left Shift, Control, Alt (and their right ones below)
    ];
    key.bKeyDown != 0
        && (ch != 0 || !(MODIFIERS.contains(&key.wVirtualKeyCode) || matches!(key.wVirtualKeyCode, 0xa1 | 0xa3 | 0xa5)))
}

/// How many input records to look at in one go.
const PEEK: usize = 64;

/// Wait at most `timeout` for characters on the console input `input`
/// (`None`: forever), dropping records that give none (a window size
/// record among them wakes the input thread, which then looks at the
/// size). Returns whether characters are there.
fn wait_characters(input: HANDLE, timeout: Option<Duration>) -> io::Result<bool> {
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        let mut records = [INPUT_RECORD::default(); PEEK];
        let mut n = 0;
        // SAFETY: `records` holds PEEK records; `n` is a valid out pointer.
        check(unsafe { PeekConsoleInputW(input, records.as_mut_ptr(), PEEK as u32, &mut n) })?;
        let p = pending(&records[..n as usize]);
        if p.skip > 0 {
            let mut read = 0;
            // SAFETY: as above; at most the records seen, which are there.
            check(unsafe { ReadConsoleInputW(input, records.as_mut_ptr(), p.skip as u32, &mut read) })?;
        }
        if p.characters {
            return Ok(true);
        }
        if p.skip > 0 {
            // More may wait behind the records dropped.
            continue;
        }
        let ms = match deadline {
            None => INFINITE,
            Some(d) => {
                let left = d.saturating_duration_since(Instant::now());
                // Round up: a wait of 0 ms for 0.5 ms left would spin.
                u32::try_from(left.as_micros().div_ceil(1000)).unwrap_or(INFINITE - 1)
            }
        };
        // SAFETY: an open handle.
        match unsafe { WaitForSingleObject(input, ms) } {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => return Ok(false),
            _ => return Err(io::Error::last_os_error()),
        }
    }
}

/// Read what is there as UTF-8 into `buf` (at least 4 bytes), waiting at
/// most `timeout` (`None` = forever). Returns `None` on timeout.
fn read_console(input: HANDLE, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
    if !wait_characters(input, timeout)? {
        return Ok(None);
    }
    // A UTF-16 unit is at most 3 bytes of UTF-8 (a pair is 4), and the
    // second half of a pair cut in two may come on top (below): room for
    // one byte more than 3 per unit.
    let mut units = vec![0u16; (buf.len().saturating_sub(1) / 3).max(1)];
    let mut n = 0;
    // SAFETY: `units` holds `units.len()` u16; `n` is a valid out pointer.
    check(unsafe { ReadConsoleW(input, units.as_mut_ptr().cast(), units.len() as u32, &mut n, std::ptr::null()) })?;
    units.truncate(n as usize);
    // A pair cut in two: its second half is the next record's.
    if units.last().is_some_and(|u| (0xd800..0xdc00).contains(u)) {
        let mut low = 0u16;
        let mut m = 0;
        // SAFETY: as above, one unit.
        check(unsafe { ReadConsoleW(input, (&raw mut low).cast(), 1, &mut m, std::ptr::null()) })?;
        if m == 1 {
            units.push(low);
        }
    }
    Ok(Some(encode_utf8(&units, buf)))
}

/// `units` as UTF-8 into `buf` (a lone surrogate as U+FFFD); returns the
/// length. `buf` holds 3 bytes per unit, 4 per pair.
fn encode_utf8(units: &[u16], buf: &mut [u8]) -> usize {
    let mut len = 0;
    for c in char::decode_utf16(units.iter().copied()) {
        let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
        len += c.encode_utf8(&mut buf[len..]).len();
    }
    len
}

/// Read side of the console, for an input thread.
pub struct TtyReader {
    input: File,
    output: File,
    /// The window's size last seen.
    last: Option<Winsize>,
}

impl Read for TtyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(read_console(handle(&self.input), buf, None)?.unwrap_or(0))
    }
}

impl InputSource for TtyReader {
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
        read_console(handle(&self.input), buf, Some(timeout))
    }

    /// The window's size differs from the last one seen (looked at each
    /// time the thread wakes; a size record only wakes it: the console
    /// sends them for its buffer too).
    fn resized(&mut self) -> bool {
        let Ok(now) = window_size(handle(&self.output)) else { return false };
        let changed = self.last != Some(now);
        self.last = Some(now);
        changed
    }
}

/// Spawn the thread that reads and parses console input and notices
/// resizes, sending events through `map(event)` into `tx`. It stops when
/// the receiver is dropped. The console's control events come as
/// [`TermEvent::Terminate`] ([`on_control`]).
pub fn spawn_input<T: Send + 'static>(
    reader: TtyReader,
    pixel_mouse: bool,
    tx: Sender<T>,
    map: impl Fn(TermEvent) -> T + Send + Sync + Clone + 'static,
) -> io::Result<()> {
    {
        let (tx, map) = (tx.clone(), map.clone());
        *CONTROL.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Box::new(move || tx.send(map(TermEvent::Terminate)).is_ok()));
    }
    static HANDLER: std::sync::Once = std::sync::Once::new();
    let mut installed = Ok(());
    HANDLER.call_once(|| {
        // SAFETY: a handler with the signature Windows calls.
        installed = check(unsafe { SetConsoleCtrlHandler(Some(on_control), 1) });
    });
    installed?;
    super::spawn_input_thread(reader, pixel_mouse, tx, map)
}

/// Tells the program to end, false when nobody listens any more.
type Terminate = Box<dyn Fn() -> bool + Send>;

/// Where [`on_control`] sends: set by [`spawn_input`], cleared when the
/// [`Tty`] is dropped.
static CONTROL: Mutex<Option<Terminate>> = Mutex::new(None);

/// The console's control events (on a thread Windows starts for them):
/// [`TermEvent::Terminate`] for each, so that the program ends as on
/// SIGTERM, putting the console back; without it Windows ends the process
/// at once, leaving the terminal in our modes and on our screen (seen with
/// Ctrl+Break in conhost). With nobody to tell, Windows' default (the end).
/// For a closing window, logoff and shutdown Windows ends the process
/// when this returns, so it waits here for the program to exit first
/// (Windows allows five seconds for a closing window).
unsafe extern "system" fn on_control(event: u32) -> BOOL {
    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;
    let told = CONTROL.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|terminate| terminate());
    if !told {
        return 0;
    }
    if event != CTRL_C_EVENT && event != CTRL_BREAK_EVENT {
        std::thread::sleep(Duration::from_secs(4));
    }
    1
}

/// Points the process's standard output and error handles at a file while
/// alive, and back when dropped (see the Unix version for why). Only what
/// goes through the standard handles moves: a C runtime's `stdout` that
/// was set up at start keeps the console's.
pub struct StdioRedirect {
    _file: File,
    saved_out: RawHandle,
    saved_err: RawHandle,
}

// SAFETY: the saved handles are only values passed back to SetStdHandle.
unsafe impl Send for StdioRedirect {}

impl StdioRedirect {
    /// Append stdout and stderr to `log` (created if missing).
    pub fn to_file(log: &std::path::Path) -> io::Result<Self> {
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(log)?;
        let get = |which: STD_HANDLE| {
            // SAFETY: a plain value.
            unsafe { GetStdHandle(which) }
        };
        let (saved_out, saved_err) = (get(STD_OUTPUT_HANDLE), get(STD_ERROR_HANDLE));
        let set = |which: STD_HANDLE, h: HANDLE| {
            // SAFETY: `h` is open (the file, kept in the guard) or was the handle.
            check(unsafe { SetStdHandle(which, h) })
        };
        set(STD_OUTPUT_HANDLE, handle(&file))?;
        if let Err(e) = set(STD_ERROR_HANDLE, handle(&file)) {
            let _ = set(STD_OUTPUT_HANDLE, saved_out);
            return Err(e);
        }
        Ok(Self { _file: file, saved_out: saved_out as RawHandle, saved_err: saved_err as RawHandle })
    }
}

impl Drop for StdioRedirect {
    fn drop(&mut self) {
        let _ = io::stdout().flush();
        // SAFETY: the handles that were there before.
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, self.saved_out as HANDLE);
            SetStdHandle(STD_ERROR_HANDLE, self.saved_err as HANDLE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_end_on_whole_characters() {
        assert_eq!(utf8_prefix(b"abc"), 3);
        assert_eq!(utf8_prefix("aé".as_bytes()), 3);
        assert_eq!(utf8_prefix(&"aé".as_bytes()[..2]), 1);
        assert_eq!(utf8_prefix(&"a€".as_bytes()[..3]), 1);
        assert_eq!(utf8_prefix(&"a😀".as_bytes()[..4]), 1);
        assert_eq!(utf8_prefix("a😀".as_bytes()), 5);
        assert_eq!(utf8_prefix(b"\xff"), 1, "not UTF-8: as it is");
    }

    #[test]
    fn utf16_becomes_utf8() {
        let mut buf = [0u8; 12];
        let units: Vec<u16> = "é😀a".encode_utf16().collect();
        let n = encode_utf8(&units, &mut buf);
        assert_eq!(&buf[..n], "é😀a".as_bytes());
        assert_eq!(encode_utf8(&[0xd800], &mut buf), 3, "a lone half: U+FFFD");
    }

    fn key(down: bool, vk: u16, ch: u16) -> INPUT_RECORD {
        let mut r = INPUT_RECORD { EventType: KEY_EVENT as u16, ..Default::default() };
        r.Event.KeyEvent.bKeyDown = down.into();
        r.Event.KeyEvent.wVirtualKeyCode = vk;
        r.Event.KeyEvent.uChar.UnicodeChar = ch;
        r
    }

    #[test]
    fn records_without_characters_are_skipped() {
        let size = INPUT_RECORD {
            EventType: windows_sys::Win32::System::Console::WINDOW_BUFFER_SIZE_EVENT as u16,
            ..Default::default()
        };
        let focus = INPUT_RECORD { EventType: 0x10, ..Default::default() };
        // A release, Shift alone, the size, then `a`.
        let records = [key(false, 0x41, 'a' as u16), key(true, 0x10, 0), size, key(true, 0x41, 'a' as u16), focus];
        assert_eq!(pending(&records), Pending { skip: 3, characters: true });
        // Up comes without a character: VT input encodes it as it is read.
        assert_eq!(pending(&[key(true, 0x26, 0)]), Pending { skip: 0, characters: true });
        assert_eq!(pending(&[focus, key(true, 0xa3, 0)]), Pending { skip: 2, characters: false });
        assert_eq!(pending(&[]), Pending::default());
    }
}
