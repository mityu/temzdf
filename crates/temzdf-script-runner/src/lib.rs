//! The script host inside the server: the guest (crates/temzdf-script-guest,
//! QuickJS and the host's code built for WebAssembly), precompiled at build
//! time (build.rs) and embedded, runs in wasmtime on a thread of its own. The
//! session talks to it with [`ScriptHost::send`] and hears from it through
//! the callback given to [`ScriptHost::start`], the messages of
//! `temzdf_protocol::script`.
//!
//! The sandbox: the guest sees only its linear memory (capped,
//! [`Options::memory_limit`]), the imports below, stderr and clocks; no file
//! system (files are read here, natively: `fs_call`). A fault in it (QuickJS's C code included) is a trap that
//! ends the guest, not the server. See doc/design/script-host.md.

mod engine_config;

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use temzdf_protocol::script::{FromHost, ToHost};
use temzdf_protocol::script_fs::{FsOp, ToGuest};
use temzdf_script::native_fs::Workers;
use wasmtime::{Caller, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, Trap, UpdateDeadline};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::p1::WasiP1Ctx;

/// The guest, compiled for this target by build.rs.
static GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/guest.cwasm"));

/// The guest's memory cap by default.
pub const MEMORY_LIMIT: usize = 1 << 30;

/// How a [`ScriptHost`] runs.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// `temzdf_script::UNATTENDED_LIMIT` (shorter in tests).
    pub unattended_limit: Duration,
    /// Past this, growing the guest's memory traps and the guest ends.
    pub memory_limit: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { unattended_limit: temzdf_script::UNATTENDED_LIMIT, memory_limit: MEMORY_LIMIT }
    }
}

/// Where the guest's thread is ([`ScriptHost`]'s `at`): when a stop does
/// not end it, the log says where it stayed.
const AT_STARTING: u8 = 0;
const AT_GUEST: u8 = 1;
const AT_RECV: u8 = 2;
const AT_SEND: u8 = 3;
const AT_FS_CALL: u8 = 4;
const AT_RETURNED: u8 = 5;
const AT_EXITING: u8 = 6;
const AT_DROPPING: u8 = 7;
const AT_ENDED: u8 = 8;

fn at_name(at: u8) -> &'static str {
    match at {
        AT_STARTING => "starting",
        AT_GUEST => "in the guest's code or a WASI call",
        AT_RECV => "in temzdf.recv",
        AT_SEND => "in temzdf.send",
        AT_FS_CALL => "in temzdf.fs_call",
        AT_RETURNED => "back from the guest, dropping its store",
        AT_EXITING => "telling the session it ended",
        AT_DROPPING => "dropping its WASI context",
        AT_ENDED => "ended",
        _ => "?",
    }
}

/// How long [`ScriptHost`]'s drop waits for the stopped thread before it
/// says where the thread is (then it waits on).
const STOP_REPORT: Duration = Duration::from_secs(10);

/// What the guest tells the session.
#[derive(Debug)]
pub enum Event {
    Msg(FromHost),
    /// The guest ended (the last event): why, for the log.
    Exited(String),
}

/// A running guest. Dropping it stops it.
pub struct ScriptHost {
    tx: Option<Sender<ToGuest>>,
    interrupts: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Where the thread is (`AT_*`).
    at: Arc<AtomicU8>,
}

/// The engine and the guest, loaded once per process.
fn loaded() -> Result<&'static (Engine, Module), String> {
    static LOADED: OnceLock<Result<(Engine, Module), String>> = OnceLock::new();
    LOADED
        .get_or_init(|| {
            let mut config = wasmtime::Config::new();
            engine_config::configure(&mut config);
            let engine = Engine::new(&config).map_err(|e| format!("wasmtime: {e}"))?;
            // SAFETY: GUEST is what build.rs precompiled with the same
            // wasmtime and configuration, embedded in this binary: not data
            // from elsewhere (loading runs its machine code as it is).
            let module = unsafe { Module::deserialize(&engine, GUEST) }
                .map_err(|e| format!("cannot load the script host's guest: {e}"))?;
            Ok((engine, module))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The store's data: WASI, the memory cap, and the guest's side of the
/// conversation.
struct Data {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
    rx: Receiver<ToGuest>,
    /// The asynchronous file operations (`fs_start`): their results join
    /// the session's messages in `rx`.
    workers: Workers,
    /// The message `recv` announced, for `recv_into`.
    pending: Vec<u8>,
    /// The answer `fs_call` announced, for `fs_reply_into`.
    fs_reply: Vec<u8>,
    interrupts: Arc<AtomicU64>,
    events: Box<dyn FnMut(Event) + Send>,
    /// Where the thread is (`AT_*`), set around the imports.
    at: Arc<AtomicU8>,
}

impl ScriptHost {
    /// Start the guest on a thread of its own; `events` gets its messages
    /// (on that thread), then [`Event::Exited`].
    pub fn start(options: Options, events: impl FnMut(Event) + Send + 'static) -> Result<Self, String> {
        let (engine, module) = loaded()?;
        let (tx, rx) = mpsc::channel();
        let done = tx.clone();
        // Never joined: `stop` sends `Shutdown` itself, so a worker stuck on
        // a hung file system (keeping the channel open) holds nothing up.
        let workers = Workers::new(move |id, result| {
            let _ = done.send(ToGuest::FsDone { id, result });
        });
        let interrupts = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let at = Arc::new(AtomicU8::new(AT_STARTING));
        let mut wasi = WasiCtxBuilder::new();
        // No preopened directory: files are read natively (`fs_call`), as
        // WASI's sandboxing refuses Windows' drive paths and symbolic links
        // to absolute paths.
        wasi.inherit_stderr();
        let wasi = wasi.build_p1();
        let limits = StoreLimitsBuilder::new().memory_size(options.memory_limit).trap_on_grow_failure(true).build();
        let data = Data {
            wasi,
            limits,
            rx,
            workers,
            pending: Vec::new(),
            fs_reply: Vec::new(),
            interrupts: interrupts.clone(),
            events: Box::new(events),
            at: at.clone(),
        };
        let mut store = Store::new(engine, data);
        store.limiter(|d| &mut d.limits);
        let stopped = stop.clone();
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(move |_| {
            if stopped.load(Ordering::SeqCst) { Err(Trap::Interrupt.into()) } else { Ok(UpdateDeadline::Continue(1)) }
        });
        let linker = linker(engine).map_err(|e| format!("wasmtime: {e}"))?;
        let stopped = stop.clone();
        let here = at.clone();
        let thread = std::thread::Builder::new()
            .name("temzdf-script".into())
            .spawn(move || {
                here.store(AT_GUEST, Ordering::SeqCst);
                let ran = run(&linker, module, &mut store, options.unattended_limit);
                here.store(AT_RETURNED, Ordering::SeqCst);
                let why = match ran {
                    Ok(0) => "it ended".to_owned(),
                    Ok(_) => "it failed (see above)".to_owned(),
                    Err(_) if stopped.load(Ordering::SeqCst) => "it was stopped".to_owned(),
                    Err(e) => format!("{e:?}"),
                };
                let mut data = store.into_data();
                here.store(AT_EXITING, Ordering::SeqCst);
                (data.events)(Event::Exited(why));
                here.store(AT_DROPPING, Ordering::SeqCst);
                drop(data);
                here.store(AT_ENDED, Ordering::SeqCst);
            })
            .map_err(|e| format!("cannot start the script host's thread: {e}"))?;
        Ok(Self { tx: Some(tx), interrupts, stop, thread: Some(thread), at })
    }

    /// Send the guest a message; false when it is gone. An interrupt counts
    /// at once, so code running now notices it.
    pub fn send(&self, msg: ToHost) -> bool {
        let Some(tx) = &self.tx else { return false };
        if msg == ToHost::Interrupt {
            self.interrupts.fetch_add(1, Ordering::SeqCst);
        }
        tx.send(ToGuest::Msg(msg)).is_ok()
    }

    /// Stop the guest whatever it is doing (a wait ends, running code traps
    /// at its next function call or loop iteration); [`Event::Exited`]
    /// follows.
    pub fn stop(&mut self) {
        // The trap first: a guest woken by the Shutdown must not run on (a
        // stopping thread put off between the two let it read the Shutdown,
        // then wait in `recv` where no epoch reaches it).
        self.stop.store(true, Ordering::SeqCst);
        if let Ok((engine, _)) = loaded() {
            engine.increment_epoch();
        }
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(ToGuest::Msg(ToHost::Shutdown));
        }
    }
}

impl Drop for ScriptHost {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            // A stop ends the thread at once; one that does not (seen on CI
            // only) is said, with where the thread stayed, before waiting on.
            let since = Instant::now();
            while !thread.is_finished() && since.elapsed() < STOP_REPORT {
                std::thread::sleep(Duration::from_millis(1));
            }
            if !thread.is_finished() {
                let _ = writeln!(
                    std::io::stderr(),
                    "temzdf script: the script host's thread did not end {} s after it was stopped: it is {}",
                    STOP_REPORT.as_secs(),
                    at_name(self.at.load(Ordering::SeqCst))
                );
            }
            let _ = thread.join();
        }
    }
}

fn run(linker: &Linker<Data>, module: &Module, store: &mut Store<Data>, limit: Duration) -> wasmtime::Result<i32> {
    let instance = linker.instantiate(&mut *store, module)?;
    // A reactor (cdylib): its constructors first.
    if let Ok(init) = instance.get_typed_func::<(), ()>(&mut *store, "_initialize") {
        init.call(&mut *store, ())?;
    }
    let run = instance.get_typed_func::<u64, i32>(&mut *store, "run")?;
    run.call(&mut *store, limit.as_millis().try_into().unwrap_or(u64::MAX))
}

/// WASI, and the imports of crates/temzdf-script-guest.
fn linker(engine: &Engine) -> wasmtime::Result<Linker<Data>> {
    let mut linker: Linker<Data> = Linker::new(engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |d| &mut d.wasi)?;
    linker.func_wrap(
        "temzdf",
        "send",
        |mut caller: Caller<'_, Data>, ptr: u32, len: u32| -> wasmtime::Result<i32> {
            // Read in place: a bogus length is a trap, not an allocation of
            // whatever size the guest names.
            let _at = At::enter(&caller.data().at, AT_SEND);
            let mem = memory(&mut caller)?;
            let frame = frame_at(mem.data(&caller), ptr, len)
                .ok_or_else(|| wasmtime::Error::msg("send: a frame outside the guest's memory"))?;
            let msg = temzdf_protocol::recv::<FromHost>(&mut &frame[..]);
            Ok(match msg {
                Ok(Some(msg)) => {
                    (caller.data_mut().events)(Event::Msg(msg));
                    0
                }
                _ => 1,
            })
        },
    )?;
    linker.func_wrap("temzdf", "recv", |mut caller: Caller<'_, Data>, timeout_ms: i64| -> i64 {
        let data = caller.data_mut();
        let _at = At::enter(&data.at.clone(), AT_RECV);
        let got = match u64::try_from(timeout_ms) {
            Ok(ms) => data.rx.recv_timeout(Duration::from_millis(ms)),
            Err(_) => data.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match got {
            Ok(msg) => {
                data.pending.clear();
                match temzdf_protocol::send(&mut data.pending, &msg) {
                    Ok(()) => data.pending.len() as i64,
                    Err(_) => -2,
                }
            }
            Err(RecvTimeoutError::Timeout) => -1,
            Err(RecvTimeoutError::Disconnected) => -2,
        }
    })?;
    linker.func_wrap(
        "temzdf",
        "recv_into",
        |mut caller: Caller<'_, Data>, ptr: u32, len: u32| -> wasmtime::Result<()> {
            let frame = std::mem::take(&mut caller.data_mut().pending);
            if frame.len() != len as usize {
                return Err(wasmtime::Error::msg("recv_into: not the announced length"));
            }
            memory(&mut caller)?.write(&mut caller, ptr as usize, &frame)?;
            Ok(())
        },
    )?;
    linker.func_wrap("temzdf", "interrupts", |caller: Caller<'_, Data>| -> u64 {
        caller.data().interrupts.load(Ordering::SeqCst)
    })?;
    linker.func_wrap(
        "temzdf",
        "fs_call",
        |mut caller: Caller<'_, Data>, ptr: u32, len: u32| -> wasmtime::Result<i64> {
            let _at = At::enter(&caller.data().at, AT_FS_CALL);
            let mem = memory(&mut caller)?;
            let frame = frame_at(mem.data(&caller), ptr, len)
                .ok_or_else(|| wasmtime::Error::msg("fs_call: a frame outside the guest's memory"))?;
            let Ok(Some(op)) = temzdf_protocol::recv::<FsOp>(&mut &frame[..]) else { return Ok(-1) };
            let result = temzdf_script::native_fs::run(&op);
            let data = caller.data_mut();
            data.fs_reply.clear();
            Ok(match temzdf_protocol::send(&mut data.fs_reply, &result) {
                Ok(()) => data.fs_reply.len() as i64,
                Err(_) => -1,
            })
        },
    )?;
    linker.func_wrap(
        "temzdf",
        "fs_start",
        |mut caller: Caller<'_, Data>, id: u64, ptr: u32, len: u32| -> wasmtime::Result<i32> {
            let mem = memory(&mut caller)?;
            let frame = frame_at(mem.data(&caller), ptr, len)
                .ok_or_else(|| wasmtime::Error::msg("fs_start: a frame outside the guest's memory"))?;
            let Ok(Some(op)) = temzdf_protocol::recv::<FsOp>(&mut &frame[..]) else { return Ok(1) };
            caller.data().workers.start(id, op);
            Ok(0)
        },
    )?;
    linker.func_wrap(
        "temzdf",
        "fs_reply_into",
        |mut caller: Caller<'_, Data>, ptr: u32, len: u32| -> wasmtime::Result<()> {
            let reply = std::mem::take(&mut caller.data_mut().fs_reply);
            if reply.len() != len as usize {
                return Err(wasmtime::Error::msg("fs_reply_into: not the announced length"));
            }
            memory(&mut caller)?.write(&mut caller, ptr as usize, &reply)?;
            Ok(())
        },
    )?;
    Ok(linker)
}

/// An import running: the thread's `at` while it runs, back in the
/// guest's code after.
struct At(Arc<AtomicU8>);

impl At {
    fn enter(at: &Arc<AtomicU8>, here: u8) -> Self {
        at.store(here, Ordering::SeqCst);
        Self(at.clone())
    }
}

impl Drop for At {
    fn drop(&mut self) {
        self.0.store(AT_GUEST, Ordering::SeqCst);
    }
}

/// The `len` bytes at `ptr` of the guest's memory, if they are inside it.
fn frame_at(mem: &[u8], ptr: u32, len: u32) -> Option<&[u8]> {
    let start = ptr as usize;
    mem.get(start..start.checked_add(len as usize)?)
}

fn memory(caller: &mut Caller<'_, Data>) -> wasmtime::Result<wasmtime::Memory> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("the guest has no memory"))
}

#[cfg(test)]
mod tests {
    use super::frame_at;

    #[test]
    fn frames_outside_the_guests_memory_are_refused() {
        let mem = [1u8, 2, 3, 4];
        assert_eq!(frame_at(&mem, 1, 2), Some(&[2u8, 3][..]));
        assert_eq!(frame_at(&mem, 0, 4), Some(&mem[..]));
        assert_eq!(frame_at(&mem, 4, 0), Some(&[][..]));
        assert_eq!(frame_at(&mem, 3, 2), None);
        assert_eq!(frame_at(&mem, 0, u32::MAX), None, "nothing allocated for a bogus length");
        assert_eq!(frame_at(&mem, u32::MAX, u32::MAX), None);
    }
}
