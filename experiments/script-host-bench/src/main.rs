//! Round trips between a mapped script function and the server, in three
//! forms (doc/survey/script-host-transport.md):
//!
//! - A: the real script host (`temzdf_script::run`, what `temzdf
//!   script-host` runs) in a child process over pipes, this program playing
//!   the server as crates/temzdf-server/src/session/script.rs does (a reader
//!   thread feeding a channel, replies written from the main thread);
//! - B: rquickjs in this process, `__host.request` a native function doing
//!   the JSON work;
//! - C: QuickJS compiled to WebAssembly (guest/) in wasmtime in this
//!   process, `__host.request` a host import doing the same work, epoch
//!   interruption on.
//!
//! One "key" is a call of the mapped function issuing `k` commands, timed
//! from the call to its end.
//!
//!   script-host-bench run --guest GUEST.wasm --label LABEL [--keys N]
//!   script-host-bench host          (form A's child: the script host)
//!
//! Output: CSV lines `label,form,what,k,n,p50_us,p90_us,p99_us,max_us,mean_us`.

use std::io::{BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use temzdf_protocol::script::{CommandSpec, FromHost, HostOp, MapRhs, ScriptValue, ToHost};

const KS: [u32; 3] = [1, 10, 50];
const WARMUP: usize = 200;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("host") => temzdf_script::run(std::io::stdin(), std::io::stdout()).unwrap(),
        Some("run") => {
            let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
            let guest = opt("--guest").expect("--guest GUEST.wasm");
            let label = opt("--label").unwrap_or_else(|| "none".into());
            let keys: usize = opt("--keys").map_or(2000, |s| s.parse().unwrap());
            let forms = opt("--forms").unwrap_or_else(|| "abc".into());
            if forms.contains('a') {
                form_a(&label, keys);
            }
            if forms.contains('b') {
                form_b(&label, keys);
            }
            if forms.contains('c') {
                form_c(&label, keys, &guest);
            }
        }
        _ => {
            eprintln!("usage: script-host-bench run --guest GUEST.wasm --label LABEL [--keys N] [--forms abc]");
            std::process::exit(2);
        }
    }
}

/// Print one CSV line for `samples` (microseconds).
fn report(label: &str, form: &str, what: &str, k: u32, samples: &mut [f64]) {
    samples.sort_by(f64::total_cmp);
    let q = |p: f64| samples[((samples.len() - 1) as f64 * p).round() as usize];
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!(
        "{label},{form},{what},{k},{},{:.1},{:.1},{:.1},{:.1},{:.1}",
        samples.len(),
        q(0.5),
        q(0.9),
        q(0.99),
        samples[samples.len() - 1],
        mean
    );
    std::io::stdout().flush().unwrap();
}

fn micros(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

/// Time `keys` calls of `one(k)` after a warm-up, for each k.
fn keys_loop(label: &str, form: &str, keys: usize, mut one: impl FnMut(u32)) {
    for k in KS {
        for _ in 0..WARMUP {
            one(k);
        }
        let mut samples: Vec<f64> = (0..keys)
            .map(|_| {
                let t = Instant::now();
                one(k);
                micros(t.elapsed())
            })
            .collect();
        report(label, form, "key", k, &mut samples);
    }
}

/// The server's side of a command: the JSON a host would hand over, parsed,
/// and the reply serialised (`current-mode` answers "normal").
fn serve_json(token: &str, op: &str, payload: &str) -> String {
    let token: serde_json::Value = serde_json::from_str(token).unwrap();
    let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert!(token.get("Call").is_some() && op == "command");
    assert_eq!(payload["name"], "current-mode");
    serde_json::to_string("normal").unwrap()
}

// --- A: the script host over pipes ---------------------------------------

/// Every command the shipped defaults use (as crates/temzdf-script/tests/host.rs
/// finds them), and `current-mode`.
fn command_specs() -> Vec<CommandSpec> {
    let mut names = vec!["current-mode".to_string()];
    let defaults = include_str!("../../../crates/temzdf-script/runtime/defaults.ts");
    for part in defaults.split("action.").skip(1) {
        let camel: String = part.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '.').collect();
        let kebab: String = camel
            .chars()
            .map(|c| if c.is_ascii_uppercase() { format!("-{}", c.to_ascii_lowercase()) } else { c.to_string() })
            .collect();
        if !names.contains(&kebab) {
            names.push(kebab);
        }
    }
    names.into_iter().map(|name| CommandSpec { name, doc: String::new(), ts: "(...args: unknown[]): unknown".into() }).collect()
}

fn form_a(label: &str, keys: usize) {
    let dir = std::env::temp_dir().join(format!("script-host-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.js");
    std::fs::write(
        &config,
        r#"import { keymap, cmd } from "temzdf";
keymap.add("normal", "<F35>", (ctx) => {
  for (let i = 0; i < ctx.k; i++) {
    if (cmd.currentMode() !== "normal") throw new Error("unexpected reply");
  }
});
"#,
    )
    .unwrap();

    let start = Instant::now();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("host")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut to_host = child.stdin.take().unwrap();
    let from_host = child.stdout.take().unwrap();
    // As the server: a thread reads the host's messages into a channel.
    let (tx, rx) = mpsc::channel::<FromHost>();
    std::thread::spawn(move || {
        let mut r = BufReader::new(from_host);
        while let Ok(Some(msg)) = temzdf_protocol::recv(&mut r) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    let mut send = |msg: ToHost| temzdf_protocol::send(&mut to_host, &msg).unwrap();

    send(ToHost::Load { config: Some(config), commands: command_specs() });
    let mut func = None;
    loop {
        match rx.recv().unwrap() {
            FromHost::Request { req, op, .. } => {
                if let HostOp::Map { keys, rhs: MapRhs::Func(f), .. } = &op
                    && keys == "<F35>"
                {
                    func = Some(*f);
                }
                send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
            }
            FromHost::Ready(r) => {
                r.unwrap();
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    report(label, "A", "startup", 0, &mut [micros(start.elapsed())]);
    let func = func.expect("the bench mapping");

    let mut id = 0;
    keys_loop(label, "A", keys, |k| {
        id += 1;
        let args = vec![ScriptValue::map([("k", ScriptValue::Num(k as f64))])];
        send(ToHost::Call { id, func, args });
        let mut served = 0;
        loop {
            match rx.recv().unwrap() {
                FromHost::Request { req, op: HostOp::Command { name, .. }, .. } => {
                    assert_eq!(name, "current-mode");
                    served += 1;
                    send(ToHost::Reply { req, result: Ok(ScriptValue::Str("normal".into())) });
                }
                FromHost::Done { id: done, result } => {
                    assert_eq!(done, id);
                    result.unwrap();
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(served, k);
    });
    send(ToHost::Shutdown);
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

// --- B: rquickjs in process ----------------------------------------------

static SERVED: AtomicU64 = AtomicU64::new(0);

fn form_b(label: &str, keys: usize) {
    use rquickjs::{Context, Function, Object, Persistent, Runtime};
    let interrupt = std::sync::Arc::new(AtomicBool::new(false));
    let start = Instant::now();
    let rt = Runtime::new().unwrap();
    let flag = interrupt.clone();
    rt.set_interrupt_handler(Some(Box::new(move || flag.load(Ordering::SeqCst))));
    let context = Context::full(&rt).unwrap();
    let (mapped, compute) = context.with(|ctx| {
        let host = Object::new(ctx.clone()).unwrap();
        host.set("current", Function::new(ctx.clone(), || r#"{"Call":1}"#.to_string()).unwrap()).unwrap();
        host.set(
            "request",
            Function::new(ctx.clone(), |t: String, o: String, p: String| {
                SERVED.fetch_add(1, Ordering::Relaxed);
                serve_json(&t, &o, &p)
            })
            .unwrap(),
        )
        .unwrap();
        ctx.globals().set("__host", host).unwrap();
        ctx.eval::<(), _>(include_str!("../bench.js")).unwrap();
        let g = ctx.globals();
        let mapped: Function = g.get("mapped").unwrap();
        let compute: Function = g.get("compute").unwrap();
        (Persistent::save(&ctx, mapped), Persistent::save(&ctx, compute))
    });
    report(label, "B", "startup", 0, &mut [micros(start.elapsed())]);

    keys_loop(label, "B", keys, |k| {
        let before = SERVED.load(Ordering::Relaxed);
        context.with(|ctx| {
            let f = mapped.clone().restore(&ctx).unwrap();
            f.call::<_, ()>((format!("[{{\"k\":{k}}}]"),)).unwrap();
        });
        assert_eq!(SERVED.load(Ordering::Relaxed) - before, k as u64);
    });

    let mut samples: Vec<f64> = (0..5)
        .map(|_| {
            let t = Instant::now();
            context.with(|ctx| {
                let f = compute.clone().restore(&ctx).unwrap();
                f.call::<_, f64>((1_000_000u32,)).unwrap();
            });
            micros(t.elapsed())
        })
        .collect();
    report(label, "B", "compute1e6", 0, &mut samples);
    drop((mapped, compute));
}

// --- C: QuickJS in wasmtime ----------------------------------------------

fn form_c(label: &str, keys: usize, guest: &str) {
    use wasmtime::{Caller, Config, Engine, Linker, Module, Store, UpdateDeadline};
    use wasmtime_wasi::WasiCtxBuilder;
    use wasmtime_wasi::p1::WasiP1Ctx;

    let mut config = Config::new();
    config.epoch_interruption(true);
    let engine = Engine::new(&config).unwrap();
    let t = Instant::now();
    let module = Module::from_file(&engine, guest).unwrap();
    report(label, "C", "compile", 0, &mut [micros(t.elapsed())]);

    let start = Instant::now();
    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s| s).unwrap();
    linker
        .func_wrap(
            "bench",
            "host_request",
            |mut caller: Caller<'_, WasiP1Ctx>,
             tp: u32,
             tl: u32,
             op: u32,
             ol: u32,
             pp: u32,
             pl: u32,
             out: u32,
             cap: u32|
             -> u32 {
                let mem = caller.get_export("memory").unwrap().into_memory().unwrap();
                let data = mem.data(&caller);
                let s = |p: u32, l: u32| std::str::from_utf8(&data[p as usize..(p + l) as usize]).unwrap();
                let reply = serve_json(s(tp, tl), s(op, ol), s(pp, pl));
                assert!(reply.len() <= cap as usize);
                mem.data_mut(&mut caller)[out as usize..out as usize + reply.len()].copy_from_slice(reply.as_bytes());
                SERVED.fetch_add(1, Ordering::Relaxed);
                reply.len() as u32
            },
        )
        .unwrap();
    let wasi = WasiCtxBuilder::new().inherit_stderr().build_p1();
    let mut store = Store::new(&engine, wasi);
    // As a host would: a ticker advances the epoch, and each deadline lets
    // the guest go on (a real host checks for an interrupt here).
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(|_| Ok(UpdateDeadline::Continue(1)));
    let ticker = engine.clone();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    std::thread::spawn(move || {
        while !stopped.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
            ticker.increment_epoch();
        }
    });
    let instance = linker.instantiate(&mut store, &module).unwrap();
    if let Ok(init) = instance.get_typed_func::<(), ()>(&mut store, "_initialize") {
        init.call(&mut store, ()).unwrap();
    }
    let init = instance.get_typed_func::<(), i32>(&mut store, "init").unwrap();
    assert_eq!(init.call(&mut store, ()).unwrap(), 0);
    report(label, "C", "startup", 0, &mut [micros(start.elapsed())]);

    let call = instance.get_typed_func::<u32, i32>(&mut store, "call").unwrap();
    keys_loop(label, "C", keys, |k| {
        let before = SERVED.load(Ordering::Relaxed);
        assert_eq!(call.call(&mut store, k).unwrap(), 0);
        assert_eq!(SERVED.load(Ordering::Relaxed) - before, k as u64);
    });

    let compute = instance.get_typed_func::<u32, f64>(&mut store, "compute").unwrap();
    let mut samples: Vec<f64> = (0..5)
        .map(|_| {
            let t = Instant::now();
            compute.call(&mut store, 1_000_000).unwrap();
            micros(t.elapsed())
        })
        .collect();
    report(label, "C", "compute1e6", 0, &mut samples);
    stop.store(true, Ordering::Relaxed);
}
