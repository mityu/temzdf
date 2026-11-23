//! The script host (temzdf-script) as a WebAssembly guest: the export `run`
//! runs it over a [`Link`] made of the imports `temzdf.*`, which
//! crates/temzdf-script-runner implements in the server. Messages cross as
//! `temzdf_protocol`'s frames (a length, then postcard). WASI gives it
//! stderr (the session's log) and clocks; files are read on the server's
//! side (`temzdf.fs_call`, crates/temzdf-script/src/native_fs.rs). See
//! doc/design/script-host.md.
#![cfg(target_family = "wasm")]

use std::io;
use std::rc::Rc;
use std::time::Duration;

use temzdf_protocol::script::{FromHost, ToHost};
use temzdf_protocol::script_fs::{FsError, FsErrorKind, FsOp, FsResult};
use temzdf_script::link::{Files, Link, Received};

#[link(wasm_import_module = "temzdf")]
unsafe extern "C" {
    /// Hand the session one framed message; 0 when it took it.
    #[link_name = "send"]
    fn host_send(ptr: *const u8, len: usize) -> i32;
    /// Wait at most `timeout_ms` (negative: as long as it takes) for the
    /// next message: its length (then `recv_into` copies it), -1 when none
    /// came in time, -2 when the session is gone.
    #[link_name = "recv"]
    fn host_recv(timeout_ms: i64) -> i64;
    /// Copy the message `recv` announced (exactly its length).
    #[link_name = "recv_into"]
    fn host_recv_into(ptr: *mut u8, len: usize);
    /// The interrupts sent so far ([`Link::interrupts`]).
    #[link_name = "interrupts"]
    fn host_interrupts() -> u64;
    /// Do one file operation (a framed `FsOp`) and wait for it: the length
    /// of its framed `FsResult` (then `fs_reply_into` copies it), negative
    /// when the operation was not understood.
    #[link_name = "fs_call"]
    fn host_fs_call(ptr: *const u8, len: usize) -> i64;
    /// Copy the answer `fs_call` announced (exactly its length).
    #[link_name = "fs_reply_into"]
    fn host_fs_reply_into(ptr: *mut u8, len: usize);
}

/// [`Link::files`] over `fs_call`.
fn fs_call(op: &FsOp) -> FsResult {
    let broken = || FsError { kind: FsErrorKind::Other, message: "the file access failed".into() };
    let mut frame = Vec::new();
    temzdf_protocol::send(&mut frame, op).map_err(|_| broken())?;
    // SAFETY: the pointer and length describe `frame`, alive for the call.
    let len = unsafe { host_fs_call(frame.as_ptr(), frame.len()) };
    let len = usize::try_from(len).map_err(|_| broken())?;
    let mut reply = vec![0u8; len];
    // SAFETY: `reply` has exactly the announced length, alive for the call.
    unsafe { host_fs_reply_into(reply.as_mut_ptr(), len) };
    match temzdf_protocol::recv::<FsResult>(&mut reply.as_slice()) {
        Ok(Some(result)) => result,
        _ => Err(broken()),
    }
}

struct GuestLink;

impl Link for GuestLink {
    fn send(&mut self, msg: &FromHost) -> io::Result<()> {
        let mut frame = Vec::new();
        temzdf_protocol::send(&mut frame, msg)?;
        // SAFETY: the pointer and length describe `frame`, alive for the call.
        match unsafe { host_send(frame.as_ptr(), frame.len()) } {
            0 => Ok(()),
            _ => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn recv(&mut self, timeout: Option<Duration>) -> Received {
        // Rounded up: a timer due in 0.4 ms must not spin on 0 ms waits.
        let ms = timeout.map_or(-1, |t| i64::try_from(t.as_micros().div_ceil(1000)).unwrap_or(i64::MAX));
        // SAFETY: an import without pointers.
        let len = unsafe { host_recv(ms) };
        let Ok(len) = usize::try_from(len) else {
            return if len == -1 { Received::Timeout } else { Received::Closed };
        };
        let mut frame = vec![0u8; len];
        // SAFETY: `frame` has exactly the announced length, alive for the call.
        unsafe { host_recv_into(frame.as_mut_ptr(), len) };
        match temzdf_protocol::recv::<ToHost>(&mut frame.as_slice()) {
            Ok(Some(msg)) => Received::Msg(msg),
            _ => Received::Closed,
        }
    }

    fn interrupts(&self) -> Box<dyn Fn() -> u64> {
        // SAFETY: an import without pointers.
        Box::new(|| unsafe { host_interrupts() })
    }

    fn files(&self) -> Files {
        Rc::new(fs_call)
    }
}

/// Run the host until the session goes away or says `Shutdown`, with
/// `unattended_ms` as `temzdf_script::UNATTENDED_LIMIT`. 0 when it ended
/// normally; otherwise why it could not run is in stderr.
#[unsafe(no_mangle)]
pub extern "C" fn run(unattended_ms: u64) -> i32 {
    match temzdf_script::run_link(Box::new(GuestLink), Duration::from_millis(unattended_ms)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("temzdf script: {e}");
            1
        }
    }
}
