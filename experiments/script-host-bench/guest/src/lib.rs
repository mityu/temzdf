//! Form C's guest: QuickJS inside WebAssembly. `__host.request` calls the
//! import `bench.host_request`, which the bench implements in wasmtime with
//! the same JSON work as form B; the reply comes back in a buffer
//! the guest passes.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

use rquickjs::{Context, Ctx, Function, Object, Persistent, Runtime};

#[link(wasm_import_module = "bench")]
unsafe extern "C" {
    /// Writes the reply into `out` (at most `cap` bytes) and returns its length.
    fn host_request(
        token: *const u8,
        token_len: usize,
        op: *const u8,
        op_len: usize,
        payload: *const u8,
        payload_len: usize,
        out: *mut u8,
        cap: usize,
    ) -> usize;
}

const CAP: usize = 64 * 1024;

struct State {
    // Field order: dropped before the context and runtime.
    mapped: Persistent<Function<'static>>,
    compute: Persistent<Function<'static>>,
    context: Context,
    _runtime: Runtime,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

static INTERRUPT: AtomicBool = AtomicBool::new(false);

fn request(token: String, op: String, payload: String) -> String {
    let mut out = vec![0u8; CAP];
    // SAFETY: the pointers and lengths describe live buffers of this module's
    // memory for the duration of the call; the host writes at most `cap` bytes.
    let n = unsafe {
        host_request(
            token.as_ptr(),
            token.len(),
            op.as_ptr(),
            op.len(),
            payload.as_ptr(),
            payload.len(),
            out.as_mut_ptr(),
            CAP,
        )
    };
    out.truncate(n);
    String::from_utf8(out).unwrap()
}

fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
    let host = Object::new(ctx.clone())?;
    host.set("current", Function::new(ctx.clone(), || r#"{"Call":1}"#.to_string())?)?;
    host.set("request", Function::new(ctx.clone(), request)?)?;
    ctx.globals().set("__host", host)?;
    ctx.eval::<(), _>(include_str!("../../bench.js"))
}

/// Set up the runtime and evaluate bench.js; 0 on success.
#[unsafe(no_mangle)]
pub extern "C" fn init() -> i32 {
    let runtime = Runtime::new().unwrap();
    runtime.set_interrupt_handler(Some(Box::new(|| INTERRUPT.load(Ordering::SeqCst))));
    let context = Context::full(&runtime).unwrap();
    let saved = context.with(|ctx| {
        install(&ctx)?;
        let g = ctx.globals();
        let mapped: Function = g.get("mapped")?;
        let compute: Function = g.get("compute")?;
        Ok::<_, rquickjs::Error>((Persistent::save(&ctx, mapped), Persistent::save(&ctx, compute)))
    });
    match saved {
        Ok((mapped, compute)) => {
            STATE.with(|s| {
                *s.borrow_mut() = Some(State { mapped, compute, context, _runtime: runtime })
            });
            0
        }
        Err(_) => 1,
    }
}

/// One key: the mapped function issuing `k` commands; 0 on success.
#[unsafe(no_mangle)]
pub extern "C" fn call(k: u32) -> i32 {
    STATE.with(|s| {
        let s = s.borrow();
        let s = s.as_ref().unwrap();
        s.context.with(|ctx| {
            let f = s.mapped.clone().restore(&ctx).unwrap();
            match f.call::<_, ()>((format!("[{{\"k\":{k}}}]"),)) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        })
    })
}

/// Plain JavaScript work (see bench.js).
#[unsafe(no_mangle)]
pub extern "C" fn compute(n: u32) -> f64 {
    STATE.with(|s| {
        let s = s.borrow();
        let s = s.as_ref().unwrap();
        s.context.with(|ctx| {
            let f = s.compute.clone().restore(&ctx).unwrap();
            f.call::<_, f64>((n,)).unwrap()
        })
    })
}
