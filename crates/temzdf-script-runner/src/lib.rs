//! The script host inside the server: the guest (crates/temzdf-script-guest,
//! QuickJS and the host's code built for WebAssembly), precompiled at build
//! time (build.rs) and embedded, runs in wasmtime on a thread of its own. The
//! session talks to it with [`ScriptHost::send`] and hears from it through
//! the callback given to [`ScriptHost::start`], the messages of
//! `temzdf_protocol::script`.
//!
//! The sandbox: the guest sees only its linear memory (capped,
//! [`Options::memory_limit`]), the imports below, stderr, clocks and the file
//! system read-only. A fault in it (QuickJS's C code included) is a trap that
//! ends the guest, not the server. See doc/design/script-host.md.

mod engine_config;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use temzdf_protocol::script::{FromHost, ToHost};
use wasmtime::{Caller, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, Trap, UpdateDeadline};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{FsPerms, WasiCtxBuilder};

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

/// What the guest tells the session.
#[derive(Debug)]
pub enum Event {
    Msg(FromHost),
    /// The guest ended (the last event): why, for the log.
    Exited(String),
}

/// A running guest. Dropping it stops it.
pub struct ScriptHost {
    tx: Option<Sender<ToHost>>,
    interrupts: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
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
    rx: Receiver<ToHost>,
    /// The message `recv` announced, for `recv_into`.
    pending: Vec<u8>,
    interrupts: Arc<AtomicU64>,
    events: Box<dyn FnMut(Event) + Send>,
}

impl ScriptHost {
    /// Start the guest on a thread of its own; `events` gets its messages
    /// (on that thread), then [`Event::Exited`].
    pub fn start(options: Options, events: impl FnMut(Event) + Send + 'static) -> Result<Self, String> {
        let (engine, module) = loaded()?;
        let (tx, rx) = mpsc::channel();
        let interrupts = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let mut wasi = WasiCtxBuilder::new();
        wasi.inherit_stderr()
            .preopened_dir("/", "/", FsPerms::ReadOnly)
            .map_err(|e| format!("cannot open / for the script host: {e}"))?;
        let wasi = wasi.build_p1();
        let limits = StoreLimitsBuilder::new().memory_size(options.memory_limit).trap_on_grow_failure(true).build();
        let data =
            Data { wasi, limits, rx, pending: Vec::new(), interrupts: interrupts.clone(), events: Box::new(events) };
        let mut store = Store::new(engine, data);
        store.limiter(|d| &mut d.limits);
        let stopped = stop.clone();
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(move |_| {
            if stopped.load(Ordering::SeqCst) { Err(Trap::Interrupt.into()) } else { Ok(UpdateDeadline::Continue(1)) }
        });
        let linker = linker(engine).map_err(|e| format!("wasmtime: {e}"))?;
        let stopped = stop.clone();
        let thread = std::thread::Builder::new()
            .name("temzdf-script".into())
            .spawn(move || {
                let why = match run(&linker, module, &mut store, options.unattended_limit) {
                    Ok(0) => "it ended".to_owned(),
                    Ok(_) => "it failed (see above)".to_owned(),
                    Err(_) if stopped.load(Ordering::SeqCst) => "it was stopped".to_owned(),
                    Err(e) => format!("{e:?}"),
                };
                (store.into_data().events)(Event::Exited(why));
            })
            .map_err(|e| format!("cannot start the script host's thread: {e}"))?;
        Ok(Self { tx: Some(tx), interrupts, stop, thread: Some(thread) })
    }

    /// Send the guest a message; false when it is gone. An interrupt counts
    /// at once, so code running now notices it.
    pub fn send(&self, msg: ToHost) -> bool {
        let Some(tx) = &self.tx else { return false };
        if msg == ToHost::Interrupt {
            self.interrupts.fetch_add(1, Ordering::SeqCst);
        }
        tx.send(msg).is_ok()
    }

    /// Stop the guest whatever it is doing (a wait ends, running code traps
    /// at its next function call or loop iteration); [`Event::Exited`]
    /// follows.
    pub fn stop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(ToHost::Shutdown);
        }
        self.stop.store(true, Ordering::SeqCst);
        if let Ok((engine, _)) = loaded() {
            engine.increment_epoch();
        }
    }
}

impl Drop for ScriptHost {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
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
            let mut frame = vec![0u8; len as usize];
            memory(&mut caller)?.read(&caller, ptr as usize, &mut frame)?;
            Ok(match temzdf_protocol::recv::<FromHost>(&mut frame.as_slice()) {
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
    Ok(linker)
}

fn memory(caller: &mut Caller<'_, Data>) -> wasmtime::Result<wasmtime::Memory> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("the guest has no memory"))
}
