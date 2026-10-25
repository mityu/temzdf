//! Terminal output written by a background thread.
//!
//! Writing a frame can block for a long time when the terminal reads slowly
//! (over SSH, through zellij): a whole-screen image is megabytes. Done on the
//! main thread, that also delays input handling, so even `q` waits until the
//! frame is out. Here callers only queue bytes; a writer thread sends them with
//! non-blocking writes, so it never sits in a write the terminal does not
//! drain and exiting can always take over: it drops the rest, terminates a
//! string sequence (APC, DCS, OSC) cut in the middle, so the bytes that follow
//! are not swallowed as its payload, and writes the terminal reset.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Bytes per write call.
const CHUNK: usize = 16 * 1024;
/// Longest wait for the terminal to accept more bytes before looking whether
/// output was closed (the writer holds the sink this long at most).
const WAIT_SLICE: Duration = Duration::from_millis(20);

/// Where output goes. Writes may fail with `WouldBlock` (a non-blocking file
/// descriptor); [`OutputSink::wait_writable`] then waits until they can go on.
pub trait OutputSink: Write + Send + 'static {
    /// Wait at most `timeout` until a write would make progress.
    fn wait_writable(&self, timeout: Duration) {
        std::thread::sleep(timeout.min(Duration::from_millis(1)));
    }
}

impl OutputSink for std::fs::File {
    fn wait_writable(&self, timeout: Duration) {
        use std::os::fd::AsFd;
        let _ = crate::tty::wait_fd(self.as_fd(), true, Some(timeout));
    }
}

/// Where escape-sequence parsing stands after the bytes written so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Seq {
    #[default]
    Ground,
    Esc,
    /// Inside a string sequence; `nested` is an APC inside a tmux passthrough
    /// DCS (written with doubled escapes).
    Str {
        osc: bool,
        nested: bool,
    },
    StrEsc {
        nested: bool,
    },
    /// `ESC ESC` inside a DCS: an escaped ESC of the wrapped sequence.
    StrEscEsc {
        nested: bool,
    },
}

impl Seq {
    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            *self = match (*self, b) {
                (Seq::Ground, 0x1b) => Seq::Esc,
                (Seq::Ground, _) => Seq::Ground,
                (Seq::Esc, b'_' | b'P' | b'^' | b'X') => Seq::Str { osc: false, nested: false },
                (Seq::Esc, b']') => Seq::Str { osc: true, nested: false },
                (Seq::Esc, 0x1b) => Seq::Esc,
                (Seq::Esc, _) => Seq::Ground,
                (Seq::Str { osc: true, .. }, 0x07) => Seq::Ground,
                (Seq::Str { nested, .. }, 0x1b) => Seq::StrEsc { nested },
                (s @ Seq::Str { .. }, _) => s,
                (Seq::StrEsc { nested }, 0x1b) => Seq::StrEscEsc { nested },
                (Seq::StrEsc { .. }, b'\\') => Seq::Ground,
                (Seq::StrEsc { nested }, _) => Seq::Str { osc: false, nested },
                (Seq::StrEscEsc { .. }, b'_') => Seq::Str { osc: false, nested: true },
                (Seq::StrEscEsc { .. }, b'\\') => Seq::Str { osc: false, nested: false },
                (Seq::StrEscEsc { nested }, _) => Seq::Str { osc: false, nested },
            };
        }
    }

    /// Bytes that end whatever sequence was cut off.
    fn terminator(self) -> &'static [u8] {
        match self {
            Seq::Ground => b"",
            // A lone ESC: complete it harmlessly (ESC \ is a no-op ST).
            Seq::Esc => b"\\",
            Seq::Str { nested: false, .. } => b"\x1b\\",
            Seq::Str { nested: true, .. } => b"\x1b\x1b\\\x1b\\",
            Seq::StrEsc { nested: false } => b"\\",
            Seq::StrEsc { nested: true } => b"\x1b\\\x1b\\",
            Seq::StrEscEsc { nested: false } => b"\\\x1b\\",
            Seq::StrEscEsc { nested: true } => b"\\\x1b\\",
        }
    }
}

struct Queue {
    bufs: VecDeque<Vec<u8>>,
    /// Bytes queued or being written.
    pending: usize,
    /// Drop everything queued and stop.
    closed: bool,
    error: Option<io::ErrorKind>,
}

struct Sink<W> {
    out: W,
    seq: Seq,
    trace: Option<std::fs::File>,
}

struct Shared<W> {
    queue: Mutex<Queue>,
    /// Signalled when bytes are queued and when the queue drains.
    changed: Condvar,
    sink: Mutex<Sink<W>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A queue of bytes for `W` and the thread writing them.
pub struct Output<W> {
    shared: Arc<Shared<W>>,
}

impl<W> Clone for Output<W> {
    fn clone(&self) -> Self {
        Self { shared: self.shared.clone() }
    }
}

impl<W: OutputSink> Output<W> {
    /// Start the writer thread. `trace` gets a copy of every byte written.
    pub fn spawn(out: W, trace: Option<std::fs::File>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue { bufs: VecDeque::new(), pending: 0, closed: false, error: None }),
            changed: Condvar::new(),
            sink: Mutex::new(Sink { out, seq: Seq::Ground, trace }),
        });
        let s = shared.clone();
        std::thread::Builder::new().name("termpix-output".into()).spawn(move || writer(&s))?;
        Ok(Self { shared })
    }
}

impl<W> Output<W> {
    /// Queue `bytes`; returns at once. Fails once a write has failed.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut q = lock(&self.shared.queue);
        if let Some(kind) = q.error {
            return Err(io::Error::new(kind, "terminal write failed"));
        }
        if q.closed || bytes.is_empty() {
            return Ok(());
        }
        q.pending += bytes.len();
        q.bufs.push_back(bytes.to_vec());
        self.shared.changed.notify_all();
        Ok(())
    }

    /// Bytes not yet written to the terminal.
    pub fn pending(&self) -> usize {
        lock(&self.shared.queue).pending
    }

    /// Wait until everything queued was written (or will never be: closed or
    /// failed), at most `timeout`. Returns whether the wait ended that way.
    pub fn drain(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut q = lock(&self.shared.queue);
        while q.pending > 0 && q.error.is_none() && !q.closed {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            q = self.shared.changed.wait_timeout(q, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        q.pending == 0 || q.error.is_some() || q.closed
    }
}

impl<W: OutputSink> Output<W> {
    /// Drop whatever is still queued, end a sequence cut off mid-way, then
    /// write `last` (the terminal reset), waiting at most `timeout` for the
    /// terminal to take it. Later writes are ignored.
    pub fn close(&self, last: &[u8], timeout: Duration) {
        {
            let mut q = lock(&self.shared.queue);
            q.closed = true;
            q.bufs.clear();
            self.shared.changed.notify_all();
        }
        // The writer holds the sink for one write or one WAIT_SLICE at most.
        let mut sink = lock(&self.shared.sink);
        let deadline = Instant::now() + timeout;
        let end = sink.seq.terminator();
        let mut bytes = end.to_vec();
        bytes.extend_from_slice(last);
        let mut off = 0;
        while off < bytes.len() && Instant::now() < deadline {
            match sink.out.write(&bytes[off..]) {
                Ok(0) => break,
                Ok(n) => off += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    sink.out.wait_writable(deadline.saturating_duration_since(Instant::now()).min(WAIT_SLICE));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = sink.out.flush();
        sink.seq = Seq::Ground;
    }
}

fn writer<W: OutputSink>(s: &Shared<W>) {
    loop {
        let buf = {
            let mut q = lock(&s.queue);
            loop {
                if q.closed {
                    q.pending = 0;
                    s.changed.notify_all();
                    return;
                }
                if let Some(b) = q.bufs.pop_front() {
                    break b;
                }
                q = s.changed.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let mut off = 0;
        while off < buf.len() {
            if lock(&s.queue).closed {
                break;
            }
            let chunk = &buf[off..(off + CHUNK).min(buf.len())];
            let mut sink = lock(&s.sink);
            let res = sink.out.write(chunk);
            let written = match res {
                Ok(n) => n,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    sink.out.wait_writable(WAIT_SLICE);
                    0
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => 0,
                Err(_) => 0,
            };
            // Only what the terminal actually took counts.
            sink.seq.feed(&chunk[..written]);
            if let Some(t) = &mut sink.trace {
                let _ = t.write_all(&chunk[..written]);
            }
            drop(sink);
            off += written;
            let mut q = lock(&s.queue);
            match res {
                Ok(0) => {
                    q.error = Some(io::ErrorKind::WriteZero);
                    q.closed = true;
                }
                Err(e) if !matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                    q.error = Some(e.kind());
                    q.closed = true;
                }
                _ => {}
            }
            q.pending = q.pending.saturating_sub(written);
            s.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            lock(&self.0).extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl OutputSink for Buf {}

    /// A non-blocking terminal that stopped reading: takes `room` more bytes
    /// (possibly part of a write), then answers `WouldBlock` until `room` grows.
    struct Stalled {
        inner: Buf,
        room: Arc<Mutex<usize>>,
    }

    impl Write for Stalled {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            let mut room = lock(&self.room);
            if *room == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = b.len().min(*room);
            *room -= n;
            self.inner.write(&b[..n])
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl OutputSink for Stalled {}

    /// Writes stall while the gate holds no more writes (a terminal that
    /// stopped reading).
    struct Gated {
        inner: Buf,
        gate: Arc<(Mutex<usize>, Condvar)>,
    }

    impl OutputSink for Gated {}

    impl Write for Gated {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            let (m, cv) = &*self.gate;
            let mut left = lock(m);
            while *left == 0 {
                left = cv.wait(left).unwrap();
            }
            *left -= 1;
            self.inner.write(b)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn after(seq: &[u8]) -> Seq {
        let mut s = Seq::Ground;
        s.feed(seq);
        s
    }

    #[test]
    fn tracks_string_sequences() {
        assert_eq!(after(b"\x1b[2J\x1b_Ga=T;AAAA\x1b\\x"), Seq::Ground);
        assert_eq!(after(b"\x1b_Ga=T;AA"), Seq::Str { osc: false, nested: false });
        assert_eq!(after(b"\x1b]2;title\x07"), Seq::Ground);
        assert_eq!(after(b"\x1bPtmux;\x1b\x1b_Ga=T;AA"), Seq::Str { osc: false, nested: true });
        assert_eq!(after(b"\x1bPtmux;\x1b\x1b_Ga=T;AA\x1b\x1b\\"), Seq::Str { osc: false, nested: false });
        assert_eq!(after(b"\x1bPtmux;\x1b\x1b_Ga=T;AA\x1b\x1b\\\x1b\\"), Seq::Ground);
    }

    #[test]
    fn terminators_return_to_ground() {
        for cut in [
            &b"\x1b"[..],
            b"\x1b_Ga=T;AA",
            b"\x1b_Ga=T;AA\x1b",
            b"\x1bPtmux;\x1b\x1b_Ga=T;AA",
            b"\x1bPtmux;\x1b\x1b_Ga=T;AA\x1b",
            b"\x1bPtmux;\x1b\x1b_Ga=T;AA\x1b\x1b",
            b"\x1bPtmux;\x1b\x1b",
        ] {
            let mut s = after(cut);
            s.feed(s.terminator());
            assert_eq!(s, Seq::Ground, "{cut:?}");
        }
    }

    #[test]
    fn writes_in_order_and_drains() {
        let sink = Buf::default();
        let out = Output::spawn(sink.clone(), None).unwrap();
        out.write(b"abc").unwrap();
        out.write(&vec![b'x'; 3 * CHUNK]).unwrap();
        assert!(out.drain(Duration::from_secs(5)));
        assert_eq!(out.pending(), 0);
        let got = lock(&sink.0).clone();
        assert_eq!(&got[..3], b"abc");
        assert_eq!(got.len(), 3 + 3 * CHUNK);
    }

    #[test]
    fn close_drops_the_backlog_and_terminates_the_cut_sequence() {
        let inner = Buf::default();
        // The first chunk goes out, then the terminal stops reading.
        let gate = Arc::new((Mutex::new(1), Condvar::new()));
        let out = Output::spawn(Gated { inner: inner.clone(), gate: gate.clone() }, None).unwrap();
        let mut frame = b"\x1b_Ga=T;".to_vec();
        frame.resize(10 * CHUNK, b'A');
        frame.extend_from_slice(b"\x1b\\");
        out.write(&frame).unwrap();
        while lock(&inner.0).len() < CHUNK {
            std::thread::yield_now();
        }
        // Let the writer block on its next chunk.
        std::thread::sleep(Duration::from_millis(20));
        let start = Instant::now();
        // The terminal resumes reading shortly after the user quit.
        let g = gate.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            *lock(&g.0) = usize::MAX;
            g.1.notify_all();
        });
        out.close(b"RESET", Duration::from_secs(2));
        assert!(start.elapsed() < Duration::from_secs(1));
        let got = lock(&inner.0).clone();
        assert!(got.len() <= 2 * CHUNK + 16, "the backlog was dropped: {}", got.len());
        assert!(got.ends_with(b"\x1b\\RESET"), "{:?}", &got[got.len() - 12..]);
        assert_eq!(after(&got[..got.len() - 5]), Seq::Ground);
        out.write(b"late").unwrap();
        assert!(!lock(&inner.0).ends_with(b"late"));
    }

    #[test]
    fn a_stalled_terminal_never_keeps_the_reset_back() {
        let inner = Buf::default();
        let room = Arc::new(Mutex::new(CHUNK + 100));
        let out = Output::spawn(Stalled { inner: inner.clone(), room: room.clone() }, None).unwrap();
        // The terminal takes a chunk and a bit of the next, then stops reading
        // in the middle of an image transfer.
        let mut frame = b"\x1b_Ga=T;".to_vec();
        frame.resize(10 * CHUNK, b'A');
        frame.extend_from_slice(b"\x1b\\");
        out.write(&frame).unwrap();
        while lock(&inner.0).len() < CHUNK + 100 {
            std::thread::yield_now();
        }
        // It reads again a little later; quitting must not have given up.
        let r = room.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            *lock(&r) = usize::MAX;
        });
        let start = Instant::now();
        out.close(b"RESET", Duration::from_secs(2));
        assert!(start.elapsed() < Duration::from_secs(1));
        let got = lock(&inner.0).clone();
        assert_eq!(got.len(), CHUNK + 100 + 2 + 5, "exactly what was taken, the terminator and the reset");
        assert!(got.ends_with(b"\x1b\\RESET"));
    }

    #[test]
    fn closing_gives_up_on_a_terminal_that_never_reads() {
        let room = Arc::new(Mutex::new(0));
        let out = Output::spawn(Stalled { inner: Buf::default(), room }, None).unwrap();
        out.write(b"stuck").unwrap();
        let start = Instant::now();
        out.close(b"RESET", Duration::from_millis(200));
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(150) && took < Duration::from_secs(1), "{took:?}");
    }
}
