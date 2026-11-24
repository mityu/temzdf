//! A terminal's pty and program, driven off the session thread: nothing the
//! program does (not reading its input, flooding output, ignoring SIGHUP,
//! leaving jobs that hold the pty) can stop the session.
//!
//! - Input goes through a queue to a writer thread (a full pty holds up only
//!   that thread, and only until the terminal goes; past [`INPUT_LIMIT`]
//!   queued bytes, input is dropped).
//! - A reader thread sends output, and waits while more than
//!   [`OUTPUT_LIMIT`] bytes it sent are not consumed yet (backpressure: a
//!   flood cannot pile up in memory or ahead of the clients' input).
//! - A waiter thread reaps the program and reports its exit (not the pty's
//!   end-of-file: jobs left in the background may keep the pty open, and
//!   ConPTY keeps its output open until it is closed).
//! - Dropping [`Pty`] hangs the program's process group up and kills it a
//!   second later if it is still there; the threads end on their own.
//!
//! On Windows the pty is the in-box ConPTY (portable-pty's; terminal panes
//! need Windows 10 1809 or later, [`supported`]), whose pipes block: the
//! threads read and write them blocking and end when ConPTY is closed.
//! Dropping [`Pty`] closes ConPTY and nothing more, as Windows Terminal
//! does when a tab closes: the programs on its console get
//! `CTRL_CLOSE_EVENT`, and Windows ends those that stay after about five
//! seconds; programs off it (a GUI program, one with a console of its own)
//! go on. The server ending, however it ends, closes ConPTY too
//! (`doc/design/session-server.md`, "Windows"; `doc/terminals/conpty.md`).

use std::collections::VecDeque;
#[cfg(unix)]
use std::fs::File;
use std::io;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use portable_pty::{Child, MasterPty, PtySize};

use crate::{Sink, TermEvent};

/// Input queued for the program at most; more is dropped.
const INPUT_LIMIT: usize = 16 << 20;
/// Output sent to the session and not consumed yet at most; the reader
/// waits beyond.
const OUTPUT_LIMIT: usize = 4 << 20;
/// How often blocked threads look whether the terminal is gone.
const POLL: Duration = Duration::from_millis(200);
/// After the program exits, how long its last output may take to arrive.
const LAST_OUTPUT: Duration = Duration::from_millis(300);
/// From the hang-up to the kill, for programs ignoring SIGHUP.
#[cfg(unix)]
const KILL_AFTER: Duration = Duration::from_secs(1);

pub(crate) struct Pty {
    /// `None` only while dropped (Windows closes it on a thread of its own).
    master: Option<Box<dyn MasterPty + Send>>,
    input: Arc<Queue>,
    backlog: Arc<Backlog>,
    /// The terminal is gone: threads stop.
    stop: Arc<AtomicBool>,
    /// The program was reaped (for Unix's kill after the hang-up).
    #[cfg_attr(not(unix), allow(dead_code))]
    reaped: Arc<AtomicBool>,
    pid: Option<i32>,
}

/// Whether terminal panes can start here.
#[cfg(unix)]
pub(crate) fn supported() -> io::Result<()> {
    Ok(())
}

/// Whether terminal panes can start here: on Windows they need ConPTY
/// (Windows 10 1809 and later), and portable-pty panics without it, so
/// this is asked before it opens one. It also keeps portable-pty from
/// loading a `conpty.dll` from anywhere but beside the executable
/// ([`dll_search_restricted`]).
#[cfg(windows)]
pub(crate) fn supported() -> io::Result<()> {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    dll_search_restricted();
    let kernel32: Vec<u16> = "kernel32.dll".encode_utf16().chain([0]).collect();
    // SAFETY: both names are NUL-terminated and outlive the calls; kernel32
    // is loaded in every process, and its handle is not freed.
    let found = unsafe {
        let module = GetModuleHandleW(kernel32.as_ptr());
        !module.is_null() && GetProcAddress(module, c"CreatePseudoConsole".as_ptr().cast()).is_some()
    };
    if found {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Unsupported, "terminal panes need ConPTY (Windows 10 version 1809 or later)"))
    }
}

/// Load DLLs named without a path only from the executable's directory and
/// System32, once per process, before portable-pty opens its first ConPTY.
///
/// portable-pty loads `conpty.dll` by name (`LoadLibraryW`) and prefers it
/// to the in-box ConPTY of kernel32. System32 has none, so by Windows'
/// default search order only someone else's copy is found: one on `PATH`
/// (WezTerm's installer puts its directory there, with a 2023 ConPTY whose
/// `ClosePseudoConsole` hangs and whose `OpenConsole.exe` spins forever
/// when a pane closes before ConPTY's cursor query is answered), or one in
/// the current directory, which the server inherits from the client (a
/// `conpty.dll` in a document's directory would run in the server). With
/// this, a name finds only a `conpty.dll` shipped beside `temzdf.exe`, and
/// otherwise portable-pty uses kernel32's. It holds for the rest of the
/// process; loads by full path, or with their own `LoadLibraryExW` flags,
/// are not affected ("Which ConPTY" in `doc/terminals/conpty.md`).
#[cfg(windows)]
pub(crate) fn dll_search_restricted() {
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
    };
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: flags only; it changes where later loads by name look.
    ONCE.call_once(|| unsafe {
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
    });
}

/// Ready this process to start a program in a pane. On Windows: a
/// program inherits whether its process ignores Ctrl+C, and the server
/// does (a client starts it with `CREATE_NEW_PROCESS_GROUP`, which sets
/// that; the tests' processes under OpenSSH in the VM did too), so without this Ctrl+C typed in a pane would
/// end nothing. The server has no console, so the change does not reach
/// it otherwise.
#[cfg(windows)]
pub(crate) fn before_spawn() {
    // SAFETY: no handler, `FALSE`: only the inherited flag is cleared.
    unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 0) };
}

#[cfg(unix)]
pub(crate) fn before_spawn() {}

impl Pty {
    /// Drive `child` on `master`; events go to `sink`.
    pub(crate) fn start(
        master: Box<dyn MasterPty + Send>,
        mut child: Box<dyn Child + Send + Sync>,
        sink: Sink,
    ) -> io::Result<Self> {
        let (reader, writer) = streams(&*master)?;
        let input = Arc::new(Queue::default());
        let backlog = Arc::new(Backlog::default());
        let stop = Arc::new(AtomicBool::new(false));
        let reaped = Arc::new(AtomicBool::new(false));
        let reader_done = Arc::new((Mutex::new(false), Condvar::new()));
        let pid = child.process_id().map(|p| p as i32);
        {
            let (input, stop) = (input.clone(), stop.clone());
            std::thread::Builder::new()
                .name("temzdf-pty-write".into())
                .spawn(move || write_loop(writer, &input, &stop))?;
        }
        {
            let (sink, backlog, stop, done) = (sink.clone(), backlog.clone(), stop.clone(), reader_done.clone());
            std::thread::Builder::new().name("temzdf-pty-read".into()).spawn(move || {
                read_loop(reader, &sink, &backlog, &stop);
                *done.0.lock().unwrap_or_else(|p| p.into_inner()) = true;
                done.1.notify_all();
            })?;
        }
        {
            let (stop, reaped) = (stop.clone(), reaped.clone());
            std::thread::Builder::new().name("temzdf-pty-wait".into()).spawn(move || {
                let _ = child.wait();
                reaped.store(true, Ordering::SeqCst);
                // Output written just before exiting is still on its way
                // (ConPTY's never ends before it is closed: there the wait
                // is the whole of LAST_OUTPUT).
                let (lock, cv) = &*reader_done;
                let done = lock.lock().unwrap_or_else(|p| p.into_inner());
                let _ = cv.wait_timeout_while(done, LAST_OUTPUT, |d| !*d);
                if !stop.load(Ordering::SeqCst) {
                    sink(TermEvent::Exited);
                }
            })?;
        }
        Ok(Self { master: Some(master), input, backlog, stop, reaped, pid })
    }

    /// Where input for the program goes (cloneable: the terminal's replies
    /// to the program's queries go there too).
    pub(crate) fn input(&self) -> Input {
        Input(self.input.clone())
    }

    /// The session handled `n` bytes of output.
    pub(crate) fn consumed(&self, n: usize) {
        self.backlog.consumed(n);
    }

    pub(crate) fn resize(&self, size: PtySize) {
        if let Some(master) = &self.master {
            let _ = master.resize(size);
        }
    }

    pub(crate) fn pid(&self) -> Option<i32> {
        self.pid
    }
}

/// The pty's output and input, each for a thread of its own.
#[cfg(unix)]
fn streams(master: &dyn MasterPty) -> io::Result<(File, File)> {
    let fd = master.as_raw_fd().ok_or_else(|| io::Error::other("no pty file descriptor"))?;
    // SAFETY: `fd` is the master's open descriptor, borrowed only to be
    // duplicated while `master` is alive.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    Ok((File::from(borrowed.try_clone_to_owned()?), File::from(borrowed.try_clone_to_owned()?)))
}

/// The pty's output and input: ConPTY's pipes.
#[cfg(windows)]
#[allow(clippy::type_complexity)]
fn streams(master: &dyn MasterPty) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    Ok((master.try_clone_reader().map_err(io::Error::other)?, master.take_writer().map_err(io::Error::other)?))
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Deleting the terminal ends its program, as closing a terminal
        // window does: hang its process group up, kill it if it stays.
        self.stop.store(true, Ordering::SeqCst);
        self.input.close();
        self.backlog.wake();
        #[cfg(unix)]
        {
            let Some(pid) = self.pid.filter(|_| !self.reaped.load(Ordering::SeqCst)) else { return };
            signal(pid, rustix::process::Signal::HUP);
            let reaped = self.reaped.clone();
            let _ = std::thread::Builder::new().name("temzdf-pty-kill".into()).spawn(move || {
                std::thread::sleep(KILL_AFTER);
                if !reaped.load(Ordering::SeqCst) {
                    signal(pid, rustix::process::Signal::KILL);
                }
            });
        }
        #[cfg(windows)]
        {
            // Closing ConPTY is the hang-up: the console's programs get
            // CTRL_CLOSE_EVENT, and its pipes end, which ends the reader
            // and the writer. It can wait until the reader has read what
            // ConPTY still had to write (older Windows), so not here.
            // (Should the thread not start, it closes here.)
            let master = self.master.take();
            let _ = std::thread::Builder::new().name("temzdf-pty-close".into()).spawn(move || drop(master));
        }
    }
}

/// Send `sig` to the program's process group (the pty's session leader
/// leads it), and to the program itself.
#[cfg(unix)]
fn signal(pid: i32, sig: rustix::process::Signal) {
    if let Some(pid) = rustix::process::Pid::from_raw(pid) {
        let _ = rustix::process::kill_process_group(pid, sig);
        let _ = rustix::process::kill_process(pid, sig);
    }
}

/// The program's input.
#[derive(Clone)]
pub(crate) struct Input(Arc<Queue>);

impl Input {
    /// Queue `bytes` for the program. False when they were dropped (it has
    /// not read [`INPUT_LIMIT`] bytes of input).
    pub(crate) fn write(&self, bytes: &[u8]) -> bool {
        self.0.push(bytes)
    }
}

/// Bytes for the program, in order.
#[derive(Default)]
struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct QueueState {
    chunks: VecDeque<Vec<u8>>,
    len: usize,
    closed: bool,
}

impl Queue {
    fn push(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.closed || s.len + bytes.len() > INPUT_LIMIT {
            return false;
        }
        s.len += bytes.len();
        s.chunks.push_back(bytes.to_vec());
        self.ready.notify_one();
        true
    }

    fn close(&self) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).closed = true;
        self.ready.notify_all();
    }

    /// The next chunk, waiting for one; `None` once closed.
    fn pop(&self) -> Option<Vec<u8>> {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if s.closed {
                return None;
            }
            if let Some(chunk) = s.chunks.pop_front() {
                s.len -= chunk.len();
                return Some(chunk);
            }
            s = self.ready.wait(s).unwrap_or_else(|p| p.into_inner());
        }
    }
}

/// Write the queued input to `pty` until the terminal goes. The writes do
/// not block: a program that stops reading (or a job that holds the pty and
/// never reads) would keep this thread, and with it the master, forever,
/// and the master open keeps that job from seeing the terminal hang up.
/// (Non-blocking holds for the reader's descriptor too: they share it.)
#[cfg(unix)]
fn write_loop(mut pty: impl Write + AsFd, input: &Queue, stop: &AtomicBool) {
    if rustix::io::ioctl_fionbio(&pty, true).is_err() {
        return;
    }
    while let Some(chunk) = input.pop() {
        let mut rest = &chunk[..];
        while !rest.is_empty() {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            match pty.write(rest) {
                Ok(0) => return,
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // Full: wait for room in slices, as the reader waits.
                    let mut fds = [rustix::event::PollFd::new(&pty, rustix::event::PollFlags::OUT)];
                    let timeout = rustix::event::Timespec { tv_sec: 0, tv_nsec: POLL.as_nanos() as _ };
                    match rustix::event::poll(&mut fds, Some(&timeout)) {
                        Ok(_) | Err(rustix::io::Errno::INTR) => {}
                        Err(_) => return,
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }
}

/// Write the queued input to ConPTY until the terminal goes. Its pipe
/// blocks: a program that stops reading holds this thread up, until
/// closing ConPTY (Pty's drop) makes the write fail.
#[cfg(windows)]
fn write_loop(mut pty: impl Write, input: &Queue, stop: &AtomicBool) {
    while let Some(chunk) = input.pop() {
        if stop.load(Ordering::SeqCst) || pty.write_all(&chunk).is_err() {
            return;
        }
    }
}

/// Output sent and not consumed yet.
#[derive(Default)]
struct Backlog {
    pending: Mutex<usize>,
    room: Condvar,
}

impl Backlog {
    /// Count `n` more bytes sent, after waiting for room (or `stop`).
    fn reserve(&self, n: usize, stop: &AtomicBool) {
        let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while *p > OUTPUT_LIMIT && !stop.load(Ordering::SeqCst) {
            p = self.room.wait_timeout(p, POLL).unwrap_or_else(|e| e.into_inner()).0;
        }
        *p += n;
    }

    fn consumed(&self, n: usize) {
        let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        *p = p.saturating_sub(n);
        self.room.notify_all();
    }

    fn wake(&self) {
        self.room.notify_all();
    }
}

#[cfg(unix)]
fn read_loop(mut pty: File, sink: &Sink, backlog: &Backlog, stop: &AtomicBool) {
    let mut buf = vec![0u8; 64 << 10];
    while !stop.load(Ordering::SeqCst) {
        // Wait in slices, to notice the terminal going away even while jobs
        // keep the pty open and quiet.
        let mut fds = [rustix::event::PollFd::new(&pty, rustix::event::PollFlags::IN)];
        let timeout = rustix::event::Timespec { tv_sec: 0, tv_nsec: POLL.as_nanos() as _ };
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return,
        }
        match pty.read(&mut buf) {
            // End of file, or EIO once no process has the pty open.
            Ok(0) => return,
            Ok(n) => {
                backlog.reserve(n, stop);
                if !sink(TermEvent::Output(buf[..n].to_vec())) {
                    return;
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) => {}
            Err(_) => return,
        }
    }
}

/// Read ConPTY's output until it is closed. Its pipe blocks and has no
/// timeout, so the terminal going away is noticed only when the read
/// ends; until then output is read and dropped: closing ConPTY can wait
/// until what it still had to write was read.
#[cfg(windows)]
fn read_loop(mut pty: impl Read, sink: &Sink, backlog: &Backlog, stop: &AtomicBool) {
    let mut buf = vec![0u8; 64 << 10];
    let mut sending = true;
    loop {
        match pty.read(&mut buf) {
            // End of file, or a broken pipe once ConPTY is closed.
            Ok(0) => return,
            Ok(n) if sending && !stop.load(Ordering::SeqCst) => {
                backlog.reserve(n, stop);
                sending = sink(TermEvent::Output(buf[..n].to_vec()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// After `supported`, a DLL named without a path is not found on
    /// `PATH` (where WezTerm's `conpty.dll` was picked up). A copy of a
    /// System32 DLL under a name of its own stands in for it: loaded by
    /// its full path it loads, so only the search keeps it out.
    #[test]
    #[cfg(windows)]
    fn dlls_by_name_are_not_looked_for_on_path() {
        use windows_sys::Win32::Foundation::FreeLibrary;
        use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
        let wide = |s: &std::ffi::OsStr| {
            use std::os::windows::ffi::OsStrExt;
            s.encode_wide().chain([0]).collect::<Vec<u16>>()
        };
        let dir = std::env::temp_dir().join(format!("temzdf-dll-search-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = "temzdf-dll-search-probe.dll";
        let system = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap().join("System32");
        std::fs::copy(system.join("version.dll"), dir.join(name)).unwrap();
        let mut path = std::ffi::OsString::from(&dir);
        path.push(";");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        // SAFETY: on Windows std serialises its environment accesses, and
        // the directory only adds to PATH for the other tests.
        unsafe { std::env::set_var("PATH", &path) };
        supported().unwrap();
        // SAFETY: NUL-terminated names that outlive the calls; a module
        // loaded is freed again.
        unsafe {
            let by_name = LoadLibraryW(wide(name.as_ref()).as_ptr());
            if !by_name.is_null() {
                FreeLibrary(by_name);
            }
            assert!(by_name.is_null(), "{name} was found on PATH");
            let by_path = LoadLibraryW(wide(dir.join(name).as_os_str()).as_ptr());
            assert!(!by_path.is_null(), "the copy loads by its full path");
            FreeLibrary(by_path);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn input_beyond_the_limit_is_dropped_not_waited_for() {
        let q = Queue::default();
        assert!(q.push(&vec![b'x'; INPUT_LIMIT - 1]));
        assert!(!q.push(b"yz"), "full: dropped");
        assert!(q.push(b"z"));
        assert_eq!(q.pop().map(|c| c.len()), Some(INPUT_LIMIT - 1));
        assert!(q.push(b"room again"));
        q.close();
        assert_eq!(q.pop(), None, "closed: the writer ends");
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore = "Miri cannot poll file descriptors")]
    fn a_writer_held_up_by_a_program_not_reading_ends_with_the_terminal() {
        // A pipe nobody reads stands in for the pty of such a program.
        let (_unread, pipe) = std::io::pipe().unwrap();
        let q = Arc::new(Queue::default());
        assert!(q.push(&vec![b'x'; 1 << 20]), "more than the pipe holds");
        let stop = Arc::new(AtomicBool::new(false));
        let (done_tx, done) = std::sync::mpsc::channel();
        let (wq, ws) = (q.clone(), stop.clone());
        std::thread::spawn(move || {
            write_loop(pipe, &wq, &ws);
            let _ = done_tx.send(());
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(done.try_recv().is_err(), "still writing");
        // The terminal goes (Pty's drop).
        stop.store(true, Ordering::SeqCst);
        q.close();
        assert!(done.recv_timeout(Duration::from_secs(1)).is_ok(), "the writer ended, its descriptor closed");
    }

    #[test]
    fn the_reader_waits_while_the_session_is_behind() {
        let backlog = Arc::new(Backlog::default());
        let stop = Arc::new(AtomicBool::new(false));
        backlog.reserve(OUTPUT_LIMIT + 1, &stop);
        // Whether the session caught up, set just before it does: the reader
        // must see it once let go (an order, not a time, so a slow thread
        // start under Miri cannot fail it).
        let caught_up = Arc::new(AtomicBool::new(false));
        let (b, s, c) = (backlog.clone(), stop.clone(), caught_up.clone());
        let waiting = std::thread::spawn(move || {
            b.reserve(10, &s);
            c.load(Ordering::SeqCst)
        });
        std::thread::sleep(Duration::from_millis(100));
        caught_up.store(true, Ordering::SeqCst);
        backlog.consumed(OUTPUT_LIMIT);
        assert!(waiting.join().unwrap(), "the reader went on while the session was behind");
        // Stopping lets a waiting reader go at once.
        backlog.reserve(OUTPUT_LIMIT, &stop);
        stop.store(true, Ordering::SeqCst);
        backlog.reserve(1, &stop);
    }
}
