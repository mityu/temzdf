//! The host's event loop and the native half of the API (`__host`, used by
//! `prelude.js`).
//!
//! One thread runs JavaScript; a second one only reads messages from the
//! session, so an interrupt can be noticed while JavaScript runs (rquickjs'
//! interrupt handler checks the flag it sets, and the interpreter then throws
//! an uncatchable error).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::promise::PromiseState;
use rquickjs::{Context, Ctx, Exception, Function, Module, Persistent, Runtime, Value};
use temzdf_protocol::script::{CommandSpec, FromHost, FuncId, HostOp, ScriptError, ScriptValue, TaskId, ToHost, Token};

const PRELUDE: &str = include_str!("prelude.js");
/// What `import ... from "temzdf"` gets.
const TEMZDF_MODULE: &str = "const t = globalThis.__temzdf;\n\
     export const cmd = t.cmd, on = t.on, prefixMap = t.prefixMap, dispatch = t.dispatch, atomic = t.atomic;\n\
     export default t;\n";

/// Run the host until the session goes away or says [`ToHost::Shutdown`].
pub fn run(input: impl Read + Send + 'static, output: impl Write + 'static) -> Result<(), String> {
    let interrupt = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    {
        let interrupt = interrupt.clone();
        let mut input = std::io::BufReader::new(input);
        std::thread::Builder::new()
            .name("script-host-in".into())
            .spawn(move || {
                loop {
                    let msg = match temzdf_protocol::recv::<ToHost>(&mut input) {
                        Ok(Some(m)) => m,
                        _ => ToHost::Shutdown,
                    };
                    if msg == ToHost::Interrupt {
                        interrupt.store(true, Ordering::SeqCst);
                    }
                    let last = msg == ToHost::Shutdown;
                    if tx.send(msg).is_err() || last {
                        return;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
    }
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    {
        let interrupt = interrupt.clone();
        rt.set_interrupt_handler(Some(Box::new(move || interrupt.load(Ordering::SeqCst))));
    }
    rt.set_loader(Resolve, Load);
    let context = Context::full(&rt).map_err(|e| e.to_string())?;
    let st = Rc::new(State {
        out: RefCell::new(Box::new(output)),
        rx,
        interrupt,
        next_req: Cell::new(1),
        next_task: Cell::new(1),
        next_timer: Cell::new(1),
        funcs: RefCell::new(Vec::new()),
        timers: RefCell::new(BTreeMap::new()),
        tokens: RefCell::new(Vec::new()),
        commands: RefCell::new(Vec::new()),
        gone: Cell::new(false),
    });
    context.with(|ctx| install(&ctx, &st)).map_err(|e| format!("cannot start the scripting API: {e}"))?;
    while !st.gone.get() {
        context.with(|ctx| while ctx.execute_pending_job() {});
        let next = st.timers.borrow().keys().next().map(|(at, _)| *at);
        let timeout = next.map_or(Duration::from_secs(3600), |at| at.saturating_duration_since(Instant::now()));
        match st.rx.recv_timeout(timeout) {
            Ok(ToHost::Load { config, commands }) => {
                *st.commands.borrow_mut() = commands;
                let result = context.with(|ctx| load_config(&ctx, &st, &config));
                st.send(&FromHost::Ready(result));
            }
            Ok(ToHost::Call { id, func, args }) => context.with(|ctx| run_call(&ctx, &st, id, func, args)),
            // Nothing runs: an interrupt or a late answer has nothing to stop.
            Ok(ToHost::Interrupt) => st.interrupt.store(false, Ordering::SeqCst),
            Ok(ToHost::Reply { .. }) => {}
            Ok(ToHost::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        context.with(|ctx| fire_timers(&ctx, &st));
    }
    // JavaScript values kept in Rust must go before the runtime does.
    st.funcs.borrow_mut().clear();
    st.timers.borrow_mut().clear();
    Ok(())
}

struct State {
    out: RefCell<Box<dyn Write>>,
    rx: Receiver<ToHost>,
    interrupt: Arc<AtomicBool>,
    next_req: Cell<u64>,
    next_task: Cell<TaskId>,
    next_timer: Cell<u64>,
    funcs: RefCell<Vec<Persistent<Function<'static>>>>,
    timers: RefCell<BTreeMap<(Instant, u64), Persistent<Function<'static>>>>,
    /// The synchronous calls running, innermost last.
    tokens: RefCell<Vec<Token>>,
    commands: RefCell<Vec<CommandSpec>>,
    /// The session went away.
    gone: Cell<bool>,
}

impl State {
    fn send(&self, msg: &FromHost) {
        if temzdf_protocol::send(&mut *self.out.borrow_mut(), msg).is_err() {
            self.gone.set(true);
        }
    }
}

/// The native functions `prelude.js` builds on, then the prelude itself.
fn install<'js>(ctx: &Ctx<'js>, st: &Rc<State>) -> rquickjs::Result<()> {
    let host = rquickjs::Object::new(ctx.clone())?;
    let s = st.clone();
    host.set("current", Function::new(ctx.clone(), move || json(&s.tokens.borrow().last().copied()))?)?;
    let s = st.clone();
    host.set(
        "commands",
        Function::new(ctx.clone(), move || {
            json(&s.commands.borrow().iter().map(|c| c.name.clone()).collect::<Vec<_>>())
        })?,
    )?;
    let s = st.clone();
    host.set(
        "request",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, token: String, op: String, payload: String| {
            request(&ctx, &s, &token, &op, &payload)
        })?,
    )?;
    let s = st.clone();
    host.set(
        "register",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, f: Function<'js>| {
            let mut funcs = s.funcs.borrow_mut();
            funcs.push(Persistent::save(&ctx, f));
            (funcs.len() - 1) as FuncId
        })?,
    )?;
    let s = st.clone();
    host.set(
        "taskStart",
        Function::new(ctx.clone(), move || {
            let task = s.next_task.get();
            s.next_task.set(task + 1);
            s.send(&FromHost::TaskStart(task));
            task as f64
        })?,
    )?;
    let s = st.clone();
    host.set(
        "taskDone",
        Function::new(ctx.clone(), move |task: f64, error: String| {
            let result = match serde_json::from_str::<Option<JsError>>(&error) {
                Ok(Some(e)) => Err(ScriptError { message: e.message, stack: e.stack }),
                _ => Ok(()),
            };
            if let Err(e) = &result {
                eprintln!("temzdf script: a dispatched task failed: {e}");
            }
            s.send(&FromHost::TaskDone { task: task as TaskId, result });
        })?,
    )?;
    for (name, begin) in [("atomicBegin", true), ("atomicEnd", false)] {
        let s = st.clone();
        host.set(
            name,
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, token: String| {
                let Ok(token) = serde_json::from_str::<Token>(&token) else {
                    return Err(Exception::throw_message(&ctx, "atomic: bad token"));
                };
                s.send(&if begin { FromHost::AtomicBegin(token) } else { FromHost::AtomicEnd(token) });
                Ok(())
            })?,
        )?;
    }
    let s = st.clone();
    host.set(
        "setTimer",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, ms: f64, f: Function<'js>| {
            let id = s.next_timer.get();
            s.next_timer.set(id + 1);
            let at = Instant::now() + Duration::from_secs_f64(ms.max(0.0) / 1000.0);
            s.timers.borrow_mut().insert((at, id), Persistent::save(&ctx, f));
            id as f64
        })?,
    )?;
    let s = st.clone();
    host.set(
        "clearTimer",
        Function::new(ctx.clone(), move |id: f64| {
            s.timers.borrow_mut().retain(|(_, t), _| *t != id as u64);
        })?,
    )?;
    host.set(
        "print",
        Function::new(ctx.clone(), |level: String, text: String| {
            // stderr is the session's log file.
            eprintln!("temzdf script [{level}]: {text}");
        })?,
    )?;
    ctx.globals().set("__host", host)?;
    ctx.eval::<(), _>(PRELUDE)
}

#[derive(serde::Deserialize)]
struct JsError {
    message: String,
    stack: Option<String>,
}

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

/// A command from JavaScript: send it, then wait for the answer. A hook the
/// command causes arrives meanwhile and runs right here, nested (Vim's
/// autocmds run inside the command that triggered them).
fn request<'js>(ctx: &Ctx<'js>, st: &Rc<State>, token: &str, op: &str, payload: &str) -> rquickjs::Result<String> {
    let throw = |m: &str| Err(Exception::throw_message(ctx, m));
    let Ok(token) = serde_json::from_str::<Token>(token) else { return throw("internal: bad context token") };
    let payload: serde_json::Value = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
    let text = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
    let func = || payload.get("func").and_then(|v| v.as_u64()).unwrap_or(0) as FuncId;
    let op = match op {
        "command" => HostOp::Command {
            name: text("name"),
            args: payload
                .get("args")
                .and_then(|a| a.as_array())
                .map_or(Vec::new(), |a| a.iter().map(to_script).collect()),
        },
        "on" => HostOp::On {
            event: text("event"),
            func: func(),
            nested: payload.get("nested").and_then(|v| v.as_bool()).unwrap_or(false),
        },
        "prefixMap" => HostOp::PrefixMap { key: text("key"), func: func() },
        other => return throw(&format!("internal: unknown operation {other}")),
    };
    let req = st.next_req.get();
    st.next_req.set(req + 1);
    st.send(&FromHost::Request { req, token, op });
    loop {
        if st.gone.get() {
            return throw("the session is gone");
        }
        match st.rx.recv() {
            Ok(ToHost::Reply { req: r, result }) if r == req => {
                return match result {
                    Ok(v) => Ok(serde_json::to_string(&from_script(&v)).unwrap_or_else(|_| "null".into())),
                    Err(e) => throw(&e),
                };
            }
            Ok(ToHost::Call { id, func, args }) => run_call(ctx, st, id, func, args),
            // The flag is set too: if the script catches this error, the
            // interrupt handler stops it at its next step anyway.
            Ok(ToHost::Interrupt) => return throw("interrupted"),
            Ok(ToHost::Shutdown) | Err(_) => {
                st.gone.set(true);
                return throw("the session is gone");
            }
            Ok(other) => eprintln!("temzdf script: unexpected {other:?} while waiting for an answer"),
        }
    }
}

/// Run a registered function as call `id` and report it done.
fn run_call(ctx: &Ctx<'_>, st: &Rc<State>, id: u64, func: FuncId, args: Vec<ScriptValue>) {
    let f = st.funcs.borrow().get(func as usize).cloned();
    st.tokens.borrow_mut().push(Token::Call(id));
    let result = match f {
        None => Err(ScriptError::new(format!("no function {func}"))),
        Some(f) => {
            let args = serde_json::to_string(&args.iter().map(from_script).collect::<Vec<_>>()).unwrap_or_default();
            f.restore(ctx).and_then(|f| f.call::<_, ()>((args,))).map_err(|e| error_of(ctx, e))
        }
    };
    st.tokens.borrow_mut().pop();
    if st.tokens.borrow().is_empty() {
        st.interrupt.store(false, Ordering::SeqCst);
    }
    st.send(&FromHost::Done { id, result });
}

fn fire_timers(ctx: &Ctx<'_>, st: &Rc<State>) {
    let now = Instant::now();
    loop {
        let due = {
            let mut timers = st.timers.borrow_mut();
            let key = timers.keys().next().copied();
            match key {
                Some(k) if k.0 <= now => timers.remove(&k),
                _ => None,
            }
        };
        let Some(f) = due else { return };
        if let Err(e) = f.restore(ctx).and_then(|f| f.call::<_, ()>(())) {
            eprintln!("temzdf script: a timer failed: {}", error_of(ctx, e));
        }
        while ctx.execute_pending_job() {}
    }
}

/// Evaluate the configuration. Its top level only registers things; a
/// top-level `await` that does not settle at once is an error.
fn load_config(ctx: &Ctx<'_>, st: &Rc<State>, path: &Path) -> Result<(), ScriptError> {
    let name = path.display().to_string();
    let source = read_module(path).map_err(ScriptError::new)?;
    st.tokens.borrow_mut().push(Token::Load);
    let result = (|| {
        let fill: Function = ctx.globals().get("__temzdfLoad").map_err(|e| error_of(ctx, e))?;
        fill.call::<_, ()>(()).map_err(|e| error_of(ctx, e))?;
        let promise = Module::evaluate(ctx.clone(), name.clone(), source).map_err(|e| error_of(ctx, e))?;
        while ctx.execute_pending_job() {}
        match promise.state() {
            PromiseState::Resolved => Ok(()),
            PromiseState::Rejected => Err(promise
                .result::<Value>()
                .and_then(|r| r.err())
                .map_or_else(|| ScriptError::new("failed"), |e| error_of(ctx, e))),
            PromiseState::Pending => Err(ScriptError::new(format!(
                "{name}: the configuration waits at its top level (`await`); \
                 start asynchronous work with dispatch() instead"
            ))),
        }
    })();
    st.tokens.borrow_mut().pop();
    st.interrupt.store(false, Ordering::SeqCst);
    result
}

/// A module's JavaScript: TypeScript is stripped of its types.
fn read_module(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if path.extension().is_some_and(|e| e == "ts" || e == "mts") {
        crate::strip::strip(&path.display().to_string(), &text)
    } else {
        Ok(text)
    }
}

/// The error a failed call left, as message and stack.
fn error_of(ctx: &Ctx<'_>, e: rquickjs::Error) -> ScriptError {
    if !matches!(e, rquickjs::Error::Exception) {
        return ScriptError::new(e.to_string());
    }
    let caught = ctx.catch();
    if let Some(obj) = caught.as_object() {
        let message: Option<String> = obj.get("message").ok();
        let name: Option<String> = obj.get("name").ok();
        let stack: Option<String> = obj.get("stack").ok();
        let message = match (name, message) {
            (Some(n), Some(m)) if !n.is_empty() => format!("{n}: {m}"),
            (_, Some(m)) => m,
            _ => "error".into(),
        };
        return ScriptError { message, stack: stack.filter(|s| !s.trim().is_empty()) };
    }
    let text: Option<String> = caught.as_string().and_then(|s| s.to_string().ok());
    ScriptError::new(text.unwrap_or_else(|| "uncatchable error (interrupted?)".into()))
}

fn to_script(v: &serde_json::Value) -> ScriptValue {
    match v {
        serde_json::Value::Null => ScriptValue::Null,
        serde_json::Value::Bool(b) => ScriptValue::Bool(*b),
        serde_json::Value::Number(n) => ScriptValue::Num(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => ScriptValue::Str(s.clone()),
        serde_json::Value::Array(a) => ScriptValue::List(a.iter().map(to_script).collect()),
        serde_json::Value::Object(o) => ScriptValue::Map(o.iter().map(|(k, v)| (k.clone(), to_script(v))).collect()),
    }
}

fn from_script(v: &ScriptValue) -> serde_json::Value {
    match v {
        ScriptValue::Null => serde_json::Value::Null,
        ScriptValue::Bool(b) => (*b).into(),
        ScriptValue::Num(n) => {
            serde_json::Number::from_f64(*n).map_or(serde_json::Value::Null, serde_json::Value::Number)
        }
        ScriptValue::Str(s) => s.clone().into(),
        ScriptValue::List(l) => l.iter().map(from_script).collect(),
        ScriptValue::Map(m) => serde_json::Value::Object(m.iter().map(|(k, v)| (k.clone(), from_script(v))).collect()),
    }
}

/// Imports: `"temzdf"`, and paths relative to the importing module.
struct Resolve;

impl Resolver for Resolve {
    fn resolve<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if name == "temzdf" {
            return Ok(name.into());
        }
        if name.starts_with("./") || name.starts_with("../") || name.starts_with('/') {
            let dir = Path::new(base).parent().unwrap_or(Path::new("."));
            return Ok(normalize(&dir.join(name)).display().to_string());
        }
        Err(Exception::throw_message(
            ctx,
            &format!("cannot import {name:?} from {base}: only \"temzdf\" and relative paths can be imported"),
        ))
    }
}

struct Load;

impl Loader for Load {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js>> {
        let source = if name == "temzdf" {
            TEMZDF_MODULE.to_owned()
        } else {
            read_module(Path::new(name)).map_err(|e| Exception::throw_message(ctx, &e))?
        };
        Module::declare(ctx.clone(), name, source)
    }
}

/// `a/./b/../c` as `a/c`, without touching the file system.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}
