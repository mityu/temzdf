//! A terminal's pty and program, driven off the session thread: nothing the
//! program does (not reading its input, flooding output, ignoring SIGHUP,
//! leaving jobs that hold the pty) can stop the session.
//!
//! - Input goes through a queue to a writer thread (a full pty blocks only
//!   that thread; past [`INPUT_LIMIT`] queued bytes, input is dropped).
//! - A reader thread sends output, and waits while more than
//!   [`OUTPUT_LIMIT`] bytes it sent are not consumed yet (backpressure: a
//!   flood cannot pile up in memory or ahead of the clients' input).
//! - A waiter thread reaps the program and reports its exit (not the pty's
//!   end-of-file: jobs left in the background may keep the pty open).
//! - Dropping [`Pty`] hangs the program's process group up and kills it a
//!   second later if it is still there; the threads end on their own.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::BorrowedFd;
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
const KILL_AFTER: Duration = Duration::from_secs(1);

pub(crate) struct Pty {
    master: Box<dyn MasterPty + Send>,
    input: Arc<Queue>,
    backlog: Arc<Backlog>,
    /// The terminal is gone: threads stop.
    stop: Arc<AtomicBool>,
    /// The program was reaped.
    reaped: Arc<AtomicBool>,
    pid: Option<i32>,
}

impl Pty {
    /// Drive `child` on `master`; events go to `sink`.
    pub(crate) fn start(
        master: Box<dyn MasterPty + Send>,
        child: Box<dyn Child + Send + Sync>,
        sink: Sink,
    ) -> io::Result<Self> {
        let fd = master.as_raw_fd().ok_or_else(|| io::Error::other("no pty file descriptor"))?;
        // SAFETY: `fd` is the master's open descriptor, borrowed only to be
        // duplicated while `master` is alive.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let reader = File::from(borrowed.try_clone_to_owned()?);
        let writer = File::from(borrowed.try_clone_to_owned()?);
        let input = Arc::new(Queue::default());
        let backlog = Arc::new(Backlog::default());
        let stop = Arc::new(AtomicBool::new(false));
        let reaped = Arc::new(AtomicBool::new(false));
        let reader_done = Arc::new((Mutex::new(false), Condvar::new()));
        let pid = child.process_id().map(|p| p as i32);
        {
            let input = input.clone();
            std::thread::Builder::new().name("temzdf-pty-write".into()).spawn(move || write_loop(writer, &input))?;
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
                let mut child = child;
                let _ = child.wait();
                reaped.store(true, Ordering::SeqCst);
                // Output written just before exiting is still on its way.
                let (lock, cv) = &*reader_done;
                let done = lock.lock().unwrap_or_else(|p| p.into_inner());
                let _ = cv.wait_timeout_while(done, LAST_OUTPUT, |d| !*d);
                if !stop.load(Ordering::SeqCst) {
                    sink(TermEvent::Exited);
                }
            })?;
        }
        Ok(Self { master, input, backlog, stop, reaped, pid })
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
        let _ = self.master.resize(size);
    }

    pub(crate) fn pid(&self) -> Option<i32> {
        self.pid
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Deleting the terminal ends its program, as closing a terminal
        // window does: hang its process group up, kill it if it stays.
        self.stop.store(true, Ordering::SeqCst);
        self.input.close();
        self.backlog.wake();
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
}

/// Send `sig` to the program's process group (the pty's session leader
/// leads it), and to the program itself.
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

fn write_loop(mut pty: File, input: &Queue) {
    while let Some(chunk) = input.pop() {
        // Blocks while the program does not read; only this thread waits.
        if pty.write_all(&chunk).is_err() {
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
