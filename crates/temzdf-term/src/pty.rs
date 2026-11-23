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
//! threads read and write them blocking and end when ConPTY is closed. The
//! program runs in a Job Object that ends its process tree with the
//! terminal, and with the server (`doc/design/session-server.md`,
//! "Windows"; `doc/terminals/conpty.md`).

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
/// From the hang-up to the kill, for programs ignoring SIGHUP (on
/// Windows: from closing ConPTY to ending the Job Object).
pub(crate) const KILL_AFTER: Duration = Duration::from_secs(1);

pub(crate) struct Pty {
    /// `None` only while dropped (Windows closes it on a thread of its own).
    master: Option<Box<dyn MasterPty + Send>>,
    input: Arc<Queue>,
    backlog: Arc<Backlog>,
    /// The terminal is gone: threads stop.
    stop: Arc<AtomicBool>,
    /// The program was reaped.
    reaped: Arc<AtomicBool>,
    pid: Option<i32>,
    /// The program's process tree; `None` only while dropped.
    #[cfg(windows)]
    job: Option<job::Job>,
}

/// Whether terminal panes can start here.
#[cfg(unix)]
pub(crate) fn supported() -> io::Result<()> {
    Ok(())
}

/// Whether terminal panes can start here: on Windows they need ConPTY
/// (Windows 10 1809 and later), and portable-pty panics without it, so
/// this is asked before it opens one.
#[cfg(windows)]
pub(crate) fn supported() -> io::Result<()> {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
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

/// Ready this process to start a program in a pane. On Windows: a
/// program inherits whether its process ignores Ctrl+C, and the server
/// does (a client starts it with `CREATE_NEW_PROCESS_GROUP`, which sets
/// that; so does OpenSSH), so without this Ctrl+C typed in a pane would
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
        #[cfg(windows)]
        let job = match job::Job::holding(&*child) {
            Ok(job) => job,
            Err(e) => {
                let _ = child.kill();
                return Err(e);
            }
        };
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
        Ok(Self {
            master: Some(master),
            input,
            backlog,
            stop,
            reaped,
            pid,
            #[cfg(windows)]
            job: Some(job),
        })
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

    /// The processes of the program's tree (for tests).
    #[cfg(all(windows, test))]
    pub(crate) fn processes(&self) -> Vec<u32> {
        self.job.as_ref().map_or_else(Vec::new, job::Job::processes)
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
            // Ending the job ends the tree, programs off the console too;
            // the job's handle closing would do the same (as when the
            // server ends), so it is held until then. (Should the thread
            // not start, both go now: the tree ends at once.)
            let (master, job, reaped) = (self.master.take(), self.job.take(), self.reaped.clone());
            let _ = std::thread::Builder::new().name("temzdf-pty-kill".into()).spawn(move || {
                let since = std::time::Instant::now();
                drop(master);
                while !reaped.load(Ordering::SeqCst) && since.elapsed() < KILL_AFTER {
                    std::thread::sleep(Duration::from_millis(20));
                }
                if let Some(job) = job {
                    job.terminate();
                }
            });
        }
    }
}

/// The Job Object a program on Windows runs in: its process tree, ended
/// together. `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` ends it when the job's
/// last handle closes, so when the server ends, however it ends, its panes'
/// programs end too, as a pty's hang-up ends them on Unix.
#[cfg(windows)]
mod job {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use portable_pty::Child;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject,
    };

    pub(super) struct Job(OwnedHandle);

    impl Job {
        /// A new job holding `child`. Processes it started before this are
        /// outside the job: it was just started, so in practice none.
        pub(super) fn holding(child: &dyn Child) -> io::Result<Self> {
            let process = child.as_raw_handle().ok_or_else(|| io::Error::other("no process handle"))?;
            // SAFETY: a new job, no name or security attributes; on success
            // the handle is ours to own.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `handle` is a valid handle nothing else owns.
            let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: the job's handle is valid, and `limits` is the struct
            // the class names, its size given.
            let set = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if set == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: both handles are valid: the job's, and the child's
            // process handle, which `child` keeps open.
            if unsafe { AssignProcessToJobObject(handle, process) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        /// End every process left in the job, and close it.
        pub(super) fn terminate(self) {
            // SAFETY: the job's handle is valid.
            unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) };
        }

        /// The ids of the processes in the job (at most 64).
        #[cfg(test)]
        pub(super) fn processes(&self) -> Vec<u32> {
            use windows_sys::Win32::System::JobObjects::{JobObjectBasicProcessIdList, QueryInformationJobObject};
            /// `JOBOBJECT_BASIC_PROCESS_ID_LIST` with room for 64 ids.
            #[repr(C)]
            struct List {
                assigned: u32,
                listed: u32,
                ids: [usize; 64],
            }
            let mut list = List { assigned: 0, listed: 0, ids: [0; 64] };
            // SAFETY: the job's handle is valid, and `list` has the class's
            // layout with room for 64 ids, its size given.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.0.as_raw_handle(),
                    JobObjectBasicProcessIdList,
                    (&raw mut list).cast(),
                    size_of::<List>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Vec::new();
            }
            list.ids[..(list.listed as usize).min(64)].iter().map(|&id| id as u32).collect()
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
