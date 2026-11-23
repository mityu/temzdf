//! The host's event loop and the native half of the API (`__host`, used by
//! `prelude.ts`).
//!
//! JavaScript runs on one thread; the [`Link`] counts the session's
//! interrupts as they are sent, so one can be noticed while JavaScript runs
//! (rquickjs' interrupt handler checks the count, and the interpreter then
//! throws an uncatchable error).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rquickjs::context::EvalOptions;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::promise::PromiseState;
use rquickjs::{Context, Ctx, Exception, Function, Module, Persistent, Runtime, Value};
use temzdf_protocol::script::{
    Code, CodeResult, Fallback, FromHost, FuncId, HostOp, Keepalive, PathStyle, Printed, ScriptError, ScriptValue,
    TaskId, ToHost, Token,
};
use temzdf_protocol::script_fs::{FsOp, FsReply};

use crate::paths;

use crate::link::{Files, Link, Received, StreamLink};

const PRELUDE: &str = include_str!(concat!(env!("OUT_DIR"), "/prelude.js"));
/// The shipped default configuration, loaded before the user's.
const DEFAULTS: &str = include_str!(concat!(env!("OUT_DIR"), "/defaults.js"));
/// Globals QuickJS lacks (AbortController, ...), evaluated before any
/// configuration (bundled by build.rs from runtime/polyfills.ts).
const POLYFILLS: &str = include_str!(concat!(env!("OUT_DIR"), "/polyfills.js"));
/// The modules scripts import as `temzdf/NAME` (bundled by build.rs).
const BUNDLED: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/bundled.rs"));
const DEFAULTS_NAME: &str = "temzdf:defaults";
/// What `import ... from "temzdf"` gets (`src/temzdf.ts`, bundled by
/// `build.rs`).
const TEMZDF_MODULE: &str = include_str!(concat!(env!("OUT_DIR"), "/temzdf.js"));

/// Run the host over a byte stream ([`StreamLink`]) until the session goes
/// away or says [`ToHost::Shutdown`], with `limit` as the
/// [`UNATTENDED_LIMIT`](crate::UNATTENDED_LIMIT) (tests pass a shorter one). The server runs the
/// WebAssembly guest instead; the tests and experiments/script-host-bench
/// use this.
pub fn run_with_limit(
    input: impl Read + Send + 'static,
    output: impl Write + 'static,
    limit: Duration,
) -> Result<(), String> {
    let link = StreamLink::new(input, output).map_err(|e| e.to_string())?;
    run_link(Box::new(link), limit)
}

/// As [`run_with_limit`], with the host's log lines (stderr otherwise) going
/// to `log`: for tests.
#[doc(hidden)]
pub fn run_with_log(
    input: impl Read + Send + 'static,
    output: impl Write + 'static,
    limit: Duration,
    log: impl Fn(&str) + 'static,
) -> Result<(), String> {
    let link = StreamLink::new(input, output).map_err(|e| e.to_string())?;
    run(Box::new(link), limit, Box::new(log))
}

/// Run the host over `link` until the session goes away or says
/// [`ToHost::Shutdown`], with `limit` as the [`UNATTENDED_LIMIT`](crate::UNATTENDED_LIMIT).
pub fn run_link(link: Box<dyn Link>, limit: Duration) -> Result<(), String> {
    // stderr is the session's log file.
    run(link, limit, Box::new(|line| eprintln!("{line}")))
}

fn run(link: Box<dyn Link>, limit: Duration, log: Box<dyn Fn(&str)>) -> Result<(), String> {
    let interrupt = Interrupts { sent: link.interrupts().into(), cleared: Arc::default() };
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    let unattended = Unattended::new(limit);
    let stopped: Rc<Cell<Option<Stop>>> = Rc::default();
    {
        let (interrupt, unattended, stopped) = (interrupt.clone(), unattended.clone(), stopped.clone());
        rt.set_interrupt_handler(Some(Box::new(move || {
            let why = if interrupt.raised() {
                Stop::Interrupt
            } else if unattended.overdue() {
                Stop::Limit
            } else {
                return false;
            };
            stopped.set(Some(why));
            true
        })));
    }
    // A promise rejected with nobody to handle it (a timer's async code, a
    // top-level `Promise.reject`) would fail silently: say so in the log,
    // once the jobs have run and a handler can no longer come
    // (`report_rejections`). QuickJS reports a rejection without a handler
    // at once, and again when one is added.
    let rejections = Rc::new(Rejections::default());
    let tracker = rejections.clone();
    rt.set_host_promise_rejection_tracker(Some(Box::new(move |ctx, promise, reason, handled| {
        if handled {
            let mut unhandled = tracker.unhandled.borrow_mut();
            let i = unhandled.iter().rposition(|(p, _)| p.clone().restore(&ctx).is_ok_and(|p| p == promise));
            let gone = i.map(|i| unhandled.remove(i));
            drop(unhandled);
            drop(gone);
        } else {
            let entry = (Persistent::save(&ctx, promise), Persistent::save(&ctx, reason));
            tracker.unhandled.borrow_mut().push(entry);
        }
    })));
    let files = link.files();
    let paths = Rc::new(Cell::new(PathStyle::default()));
    rt.set_loader(Resolve { files: files.clone(), paths: paths.clone() }, Load { files: files.clone() });
    let context = Context::full(&rt).map_err(|e| e.to_string())?;
    let st = Rc::new(State {
        link: RefCell::new(link),
        interrupt,
        next_req: Cell::new(1),
        next_task: Cell::new(1),
        next_timer: Cell::new(1),
        unattended,
        interrupts_read: Cell::new(0),
        interrupts_used: Cell::new(0),
        funcs: RefCell::new(Vec::new()),
        timers: RefCell::new(BTreeMap::new()),
        tokens: RefCell::new(Vec::new()),
        open_atomic: RefCell::new(Vec::new()),
        running_tasks: RefCell::new(std::collections::BTreeSet::new()),
        commands: RefCell::new(Vec::new()),
        evals: RefCell::default(),
        queued_code: RefCell::default(),
        queued_calls: RefCell::default(),
        promises: RefCell::default(),
        settled: RefCell::default(),
        replies: RefCell::default(),
        gone: Cell::new(false),
        rejections,
        stopped,
        log,
        files,
        paths,
    });
    context.with(|ctx| install(&ctx, &st)).map_err(|e| format!("cannot start the scripting API: {e}"))?;
    while !st.gone.get() {
        // Each job on its own time: one that overran does not cut off the
        // next ones.
        // Answers that came while code ran (a nested wait took them) settle
        // before waiting: no message may come to wake the loop for them.
        loop {
            // Calls that crossed code waiting for an answer, in order.
            let call = st.queued_calls.borrow_mut().pop_front();
            if let Some(call) = call {
                context.with(|ctx| match call {
                    QueuedCall::Call { id, func, args } => run_call(&ctx, &st, id, func, args),
                    QueuedCall::Eval { id, funcs, args } => run_eval(&ctx, &st, id, &funcs, args),
                });
                continue;
            }
            // `temzdf eval`s that came while code waited.
            let queued = st.queued_code.borrow_mut().pop_front();
            if let Some((id, code)) = queued {
                context.with(|ctx| run_code(&ctx, &st, id, code));
                continue;
            }
            context.with(|ctx| run_jobs(&ctx, &st));
            if st.settled.borrow().is_empty()
                && st.queued_code.borrow().is_empty()
                && st.queued_calls.borrow().is_empty()
            {
                break;
            }
            context.with(|ctx| settle_promises(&ctx, &st));
        }
        let next = st.timers.borrow().keys().next().map(|(at, _)| *at);
        let timeout = next.map_or(Duration::from_secs(3600), |at| at.saturating_duration_since(Instant::now()));
        match st.recv(Some(timeout)) {
            Ok(ToHost::Load { config, commands, paths }) => {
                *st.commands.borrow_mut() = commands;
                st.paths.set(paths);
                // For temzdf/path, before any module loads (runtime/path.ts).
                let style = if paths == PathStyle::Windows { "windows" } else { "posix" };
                context.with(|ctx| ctx.globals().set("__temzdfPathStyle", style)).ok();
                let result = context.with(|ctx| load_config(&ctx, &st, config.as_deref()));
                st.send(&FromHost::Ready(result));
            }
            Ok(ToHost::Call { id, func, args, .. }) => context.with(|ctx| run_call(&ctx, &st, id, func, args)),
            Ok(ToHost::Eval { id, funcs, args, .. }) => context.with(|ctx| run_eval(&ctx, &st, id, &funcs, args)),
            Ok(ToHost::RunCode { id, code }) => context.with(|ctx| run_code(&ctx, &st, id, code)),
            // Nothing runs: an interrupt or a late answer has nothing to stop.
            Ok(ToHost::Interrupt) => {
                st.take_interrupt_message();
                st.interrupt.clear();
            }
            Ok(ToHost::Reply { .. }) => {}
            Ok(ToHost::Resolve { promise, result }) => st.settled.borrow_mut().push_back((promise, result)),
            Ok(ToHost::Shutdown) | Err(true) => break,
            Err(false) => {}
        }
        // A microtask checkpoint after the task (as in browsers and Node):
        // the promise jobs it queued, then the answers that came, each with
        // its jobs, all before a timer fires.
        context.with(|ctx| {
            run_jobs(&ctx, &st);
            settle_promises(&ctx, &st);
            fire_timers(&ctx, &st);
        });
    }
    // JavaScript values kept in Rust must go before the runtime does.
    st.funcs.borrow_mut().clear();
    st.timers.borrow_mut().clear();
    st.rejections.unhandled.borrow_mut().clear();
    Ok(())
}

/// The interrupt flag: raised while the session has sent interrupts since
/// it was last cleared (counted by the [`Link`] as they are sent).
#[derive(Clone)]
struct Interrupts {
    sent: Arc<dyn Fn() -> u64>,
    cleared: Arc<std::sync::atomic::AtomicU64>,
}

impl Interrupts {
    fn raised(&self) -> bool {
        (self.sent)() > self.cleared.load(Ordering::SeqCst)
    }

    fn clear(&self) {
        self.cleared.store((self.sent)(), Ordering::SeqCst);
    }

    /// Clear it: whether it was raised, and the interrupts sent until now.
    fn take(&self) -> (bool, u64) {
        let sent = (self.sent)();
        (self.cleared.swap(sent, Ordering::SeqCst) < sent, sent)
    }
}

/// A call or conditions waiting until nothing runs (`State::queued_calls`).
enum QueuedCall {
    Call { id: u64, func: FuncId, args: Vec<ScriptValue> },
    Eval { id: u64, funcs: Vec<FuncId>, args: Vec<ScriptValue> },
}

struct State {
    link: RefCell<Box<dyn Link>>,
    interrupt: Interrupts,
    next_req: Cell<u64>,
    next_task: Cell<TaskId>,
    next_timer: Cell<u64>,
    unattended: Unattended,
    /// Interrupts by number: those sent (`interrupt`, raising the flag),
    /// those whose message was read here, and the last one used up
    /// (by the flag stopping code nobody waited for, or by its message
    /// ending a wait). A message read later for an interrupt used up already
    /// must not interrupt the call that runs next; one whose message ended a
    /// wait must not count again when its flag is found.
    interrupts_read: Cell<u64>,
    interrupts_used: Cell<u64>,
    funcs: RefCell<Vec<Persistent<Function<'static>>>>,
    timers: RefCell<BTreeMap<(Instant, u64), Persistent<Function<'static>>>>,
    /// The synchronous calls running, innermost last.
    tokens: RefCell<Vec<Token>>,
    /// Atomic sections begun and not ended, innermost last, and the
    /// dispatched tasks not done: an interrupt skips `finally` and promise
    /// handlers (QuickJS: uncatchable), so they are closed from here.
    open_atomic: RefCell<Vec<Token>>,
    running_tasks: RefCell<std::collections::BTreeSet<TaskId>>,
    commands: RefCell<Vec<String>>,
    /// The `temzdf eval`s running, by their task.
    evals: RefCell<std::collections::HashMap<TaskId, EvalRun>>,
    /// `temzdf eval`s that came while code waited for an answer: started
    /// once nothing runs.
    queued_code: RefCell<std::collections::VecDeque<(u64, Code)>>,
    /// Calls and conditions the session sent while not answering a request
    /// (input, a scheduled function) that came while code the host started
    /// itself (a timer, a dispatched task) waited for an answer: run once
    /// nothing runs, never inside that code's command (or its atomic
    /// section).
    queued_calls: RefCell<std::collections::VecDeque<QueuedCall>>,
    /// Promises commands answered with, by number: the function settling
    /// each (`(ok, json)`), and answers that came while code ran.
    promises: RefCell<std::collections::HashMap<u64, Persistent<Function<'static>>>>,
    settled: RefCell<std::collections::VecDeque<(u64, Result<ScriptValue, String>)>>,
    /// Answers to requests waited for further out: a call run while one
    /// request waits (a hook) can be waiting for its own when the outer
    /// one's answer comes.
    replies: RefCell<std::collections::HashMap<u64, Result<ScriptValue, String>>>,
    /// The session went away.
    gone: Cell<bool>,
    rejections: Rc<Rejections>,
    /// Why the interrupt handler last stopped the interpreter.
    stopped: Rc<Cell<Option<Stop>>>,
    /// Where log lines go (stderr, the session's log file, but in tests).
    log: Box<dyn Fn(&str)>,
    /// File access ([`Link::files`]).
    files: Files,
    /// How the session's OS writes paths (`ToHost::Load`).
    paths: Rc<Cell<PathStyle>>,
}

/// A `temzdf eval` running as a task: its request's number, what it
/// printed so far, and how its steps' positions moved (`eval::Positions`).
struct EvalRun {
    id: u64,
    printed: Vec<Printed>,
    positions: Vec<(String, crate::eval::Positions)>,
}

thread_local! {
    /// The directories relative imports of `temzdf eval`'s steps start
    /// from, by step name (`-c 1`, `[stdin]`; a file's is its own).
    static EVAL_BASES: RefCell<std::collections::HashMap<String, String>> = RefCell::default();
}

/// Why the interrupt handler stopped the interpreter.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stop {
    /// The session's interrupt (the user's <C-c>).
    Interrupt,
    /// Code nobody waits for ran past the [`UNATTENDED_LIMIT`](crate::UNATTENDED_LIMIT).
    Limit,
}

/// Promises rejected with no handler: the promise and its reason, in the
/// order they were rejected.
#[derive(Default)]
struct Rejections {
    unhandled: RefCell<Vec<(Persistent<Value<'static>>, Persistent<Value<'static>>)>>,
    /// Held back while a configuration loads (see `load_config`).
    held: Cell<bool>,
}

impl State {
    fn log(&self, line: &str) {
        (self.log)(line);
    }

    fn send(&self, msg: &FromHost) {
        if self.link.borrow_mut().send(msg).is_err() {
            self.gone.set(true);
        }
    }

    /// The next message; `Err(true)` when the session went away,
    /// `Err(false)` when nothing came in time. (A function, not a borrow
    /// kept over the caller's `match`: handling a message sends.)
    fn recv(&self, timeout: Option<Duration>) -> Result<ToHost, bool> {
        match self.link.borrow_mut().recv(timeout) {
            Received::Msg(m) => Ok(m),
            Received::Timeout => Err(false),
            Received::Closed => Err(true),
        }
    }
}

/// The native functions `prelude.ts` builds on, then the prelude itself.
fn install<'js>(ctx: &Ctx<'js>, st: &Rc<State>) -> rquickjs::Result<()> {
    let host = rquickjs::Object::new(ctx.clone())?;
    let s = st.clone();
    host.set("current", Function::new(ctx.clone(), move || json(&s.tokens.borrow().last().copied()))?)?;
    let s = st.clone();
    host.set("commands", Function::new(ctx.clone(), move || json(&*s.commands.borrow()))?)?;
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
            s.running_tasks.borrow_mut().insert(task);
            s.send(&FromHost::TaskStart(task));
            task as f64
        })?,
    )?;
    let s = st.clone();
    host.set(
        "taskDone",
        Function::new(ctx.clone(), move |task: f64, error: String| {
            let result = match serde_json::from_str::<Option<JsError>>(&error) {
                Ok(Some(e)) => Err(ScriptError { message: e.message, stack: e.stack, command: e.command }),
                _ => Ok(()),
            };
            if let Err(e) = &result {
                s.log(&format!("temzdf script: a dispatched task failed: {e}"));
            }
            // Reported already if an interrupt cut it off (close_cut_off).
            if s.running_tasks.borrow_mut().remove(&(task as TaskId)) {
                s.send(&FromHost::TaskDone { task: task as TaskId, result });
            }
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
                let mut open = s.open_atomic.borrow_mut();
                if begin {
                    open.push(token);
                } else if let Some(i) = open.iter().rposition(|t| *t == token) {
                    open.remove(i);
                } else {
                    // Ended already (close_cut_off).
                    return Ok(());
                }
                drop(open);
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
            // The prelude clamps delays already; NaN and infinities stop here too.
            let ms = if ms.is_nan() { 0.0 } else { ms.clamp(0.0, 2_147_483_647.0) };
            let at = Instant::now() + Duration::from_secs_f64(ms / 1000.0);
            s.timers.borrow_mut().insert((at, id), Persistent::save(&ctx, f));
            id as f64
        })?,
    )?;
    let s = st.clone();
    host.set(
        "awaitPromise",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: f64, settle: Function<'js>| {
            s.promises.borrow_mut().insert(id as u64, Persistent::save(&ctx, settle));
        })?,
    )?;
    let s = st.clone();
    host.set(
        "clearTimer",
        Function::new(ctx.clone(), move |id: f64| {
            s.timers.borrow_mut().retain(|(_, t), _| *t != id as u64);
        })?,
    )?;
    let s = st.clone();
    host.set(
        "print",
        Function::new(ctx.clone(), move |level: String, text: String| {
            s.log(&format!("temzdf script [{level}]: {text}"));
        })?,
    )?;
    let s = st.clone();
    host.set(
        "evalPrint",
        Function::new(ctx.clone(), move |task: f64, err: bool, text: String| {
            if let Some(run) = s.evals.borrow_mut().get_mut(&(task as TaskId)) {
                run.printed.push(if err { Printed::Err(text) } else { Printed::Out(text) });
            }
        })?,
    )?;
    let s = st.clone();
    host.set(
        "compile",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, task: f64, name: String, code: String| {
            let compiled = crate::eval::compile(&name, &code).map_err(|e| Exception::throw_syntax(&ctx, &e))?;
            if let Some(run) = s.evals.borrow_mut().get_mut(&(task as TaskId)) {
                run.positions.push((name.clone(), compiled.positions));
            }
            let mut options = EvalOptions::default();
            options.strict = false;
            options.filename = Some(name);
            ctx.eval_with_options::<Function, _>(compiled.source, options)
        })?,
    )?;
    let s = st.clone();
    host.set(
        "evalDone",
        Function::new(ctx.clone(), move |task: f64, outcome: String| {
            let Some(run) = s.evals.borrow_mut().remove(&(task as TaskId)) else { return };
            let result = match serde_json::from_str::<EvalOutcome>(&outcome) {
                Ok(EvalOutcome { error: Some(e), .. }) => Err(eval_error(&run, &e)),
                Ok(EvalOutcome { value, .. }) => Ok(value),
                Err(e) => Err(format!("internal: evalDone: {e}")),
            };
            s.send(&FromHost::CodeDone { id: run.id, result: CodeResult { printed: run.printed, result } });
        })?,
    )?;
    ctx.globals().set("__host", host)?;
    ctx.eval::<(), _>(PRELUDE)?;
    ctx.eval::<(), _>(POLYFILLS)
}

/// How a `temzdf eval` ended, as the prelude says (`evalDone`): its value
/// as JSON (with `-p`), or its error.
#[derive(serde::Deserialize)]
struct EvalOutcome {
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    error: Option<EvalError>,
}

#[derive(serde::Deserialize)]
struct EvalError {
    /// The step that failed (`-c 2`, the file).
    step: String,
    message: String,
    #[serde(default)]
    stack: Option<String>,
}

/// A failed `temzdf eval` as text: the step, the message, where (columns
/// on a step's first line its own, not its wrapper's).
fn eval_error(run: &EvalRun, e: &EvalError) -> String {
    let mut text = e.message.clone();
    // A syntax error says where already.
    if !text.contains(&format!("{}:", e.step)) {
        text = format!("{}: {text}", e.step);
    }
    // Not the prelude's frames (evaluated as `eval_script`): the code's.
    let frames: Vec<&str> = e.stack.as_deref().unwrap_or("").lines().filter(|l| !l.contains("(eval_script:")).collect();
    let stack = frames.join("\n");
    if !stack.trim().is_empty() {
        text = format!("{text}\n{}", stack.trim_end());
    }
    run.positions.iter().fold(text, |t, (name, positions)| positions.fix(&t, name))
}

/// Start `temzdf eval`'s code as a dispatched task (`__temzdfEval` in
/// `prelude.ts`); it answers with `CodeDone` when it ends.
fn run_code(ctx: &Ctx<'_>, st: &Rc<State>, id: u64, code: Code) {
    EVAL_BASES.with(|bases| {
        let mut bases = bases.borrow_mut();
        for s in code.steps.iter().filter(|s| !paths::is_absolute(st.paths.get(), &s.name)) {
            bases.insert(s.name.clone(), code.base.display().to_string());
        }
    });
    let steps = json(&code.steps);
    st.stopped.set(None);
    let started = st.unattended(|| {
        ctx.globals().get::<_, Function>("__temzdfEval").and_then(|f| f.call::<_, f64>((steps, code.print)))
    });
    match started {
        Ok(task) => {
            st.evals.borrow_mut().insert(task as TaskId, EvalRun { id, printed: Vec::new(), positions: Vec::new() });
        }
        Err(e) => {
            let result = Err(error_of(ctx, e).to_string());
            st.send(&FromHost::CodeDone { id, result: CodeResult { printed: Vec::new(), result } });
        }
    }
}

#[derive(serde::Deserialize)]
struct JsError {
    message: String,
    stack: Option<String>,
    #[serde(default)]
    command: bool,
}

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

/// The operation `op` with its JSON `payload`, as the prelude sends them.
/// A payload that breaks that protocol (a field missing, a wrong type) is
/// the prelude's bug, refused with an `internal:` error the call throws:
/// never read as a default (function 0, no mode), and never a panic, which
/// would stop every script. What scripts get wrong the prelude refuses
/// before, with messages of its own.
fn host_op(op: &str, payload: &str) -> Result<HostOp, String> {
    fn parse<T: serde::de::DeserializeOwned>(op: &str, payload: &str) -> Result<T, String> {
        serde_json::from_str(payload).map_err(|e| format!("internal: {op}: {e}"))
    }
    Ok(match op {
        "command" => {
            let a: CommandArgs = parse(op, payload)?;
            HostOp::Command { name: a.name, args: a.args.iter().map(to_script).collect() }
        }
        "schedule" => HostOp::Schedule { func: parse::<ScheduleArgs>(op, payload)?.func },
        "off" => {
            let a: HookArgs = parse(op, payload)?;
            HostOp::Off { event: a.event, func: a.func }
        }
        "on" => {
            let a: HookArgs = parse(op, payload)?;
            HostOp::On { event: a.event, func: a.func, nested: a.nested }
        }
        "defineMode" => {
            let a: DefineModeArgs = parse(op, payload)?;
            HostOp::DefineMode {
                name: a.name,
                fallback: a.fallback.map(|f| match f {
                    FallbackArg::Name(FallbackName::Drop) => Fallback::Drop,
                    FallbackArg::Name(FallbackName::Passthrough) => Fallback::Passthrough,
                    FallbackArg::Func(f) => Fallback::Func(f),
                }),
                count: a.count,
                transient: a.transient,
                keepalive: a.keepalive.map(|k| match k {
                    KeepaliveArg::Name(KeepaliveName::Mapped) => Keepalive::Mapped,
                    KeepaliveArg::Name(KeepaliveName::Always) => Keepalive::Always,
                    KeepaliveArg::Func(f) => Keepalive::Func(f),
                }),
                timeout: a.timeout,
            }
        }
        "map" => {
            let a: MapArgs = parse(op, payload)?;
            HostOp::Map {
                modes: a.modes.into_vec(),
                keys: a.keys,
                func: a.func,
                nowait: a.nowait,
                desc: a.desc,
                when: a.when,
                namespace: a.namespace,
                priority: a.priority,
            }
        }
        "unmap" => {
            let a: UnmapArgs = parse(op, payload)?;
            HostOp::Unmap { modes: a.modes.into_vec(), keys: a.keys, namespace: a.namespace }
        }
        "clearMappings" => {
            let a: MappingFilter = parse(op, payload)?;
            HostOp::ClearMappings { mode: a.mode, namespace: a.namespace }
        }
        "getMappings" => {
            let a: MappingFilter = parse(op, payload)?;
            HostOp::GetMappings { mode: a.mode, namespace: a.namespace, keys: a.keys }
        }
        "setOption" => {
            let a: SetOptionArgs = parse(op, payload)?;
            HostOp::SetOption { name: a.name, value: to_script(&a.value) }
        }
        "getOption" => HostOp::GetOption { name: parse::<GetOptionArgs>(op, payload)?.name },
        other => return Err(format!("internal: unknown operation {other}")),
    })
}

// The payloads of `host_op`: optional fields are `Option`s (absent or
// null); the rest must be there.

#[derive(serde::Deserialize)]
struct CommandArgs {
    name: String,
    args: Vec<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct ScheduleArgs {
    func: FuncId,
}

#[derive(serde::Deserialize)]
struct HookArgs {
    event: String,
    func: FuncId,
    #[serde(default)]
    nested: bool,
}

#[derive(serde::Deserialize)]
struct DefineModeArgs {
    name: String,
    #[serde(default)]
    fallback: Option<FallbackArg>,
    #[serde(default)]
    count: Option<bool>,
    #[serde(default)]
    transient: Option<bool>,
    #[serde(default)]
    keepalive: Option<KeepaliveArg>,
    #[serde(default)]
    timeout: Option<f64>,
}

#[derive(serde::Deserialize)]
#[serde(untagged, expecting = "fallback: \"drop\", \"passthrough\" or a function")]
enum FallbackArg {
    Name(FallbackName),
    Func(FuncId),
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum FallbackName {
    Drop,
    Passthrough,
}

#[derive(serde::Deserialize)]
#[serde(untagged, expecting = "keepalive: \"mapped\", \"always\" or a function")]
enum KeepaliveArg {
    Name(KeepaliveName),
    Func(FuncId),
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum KeepaliveName {
    Mapped,
    Always,
}

/// A mode or a list of modes.
#[derive(serde::Deserialize)]
#[serde(untagged, expecting = "modes: a mode or a list of modes")]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

#[derive(serde::Deserialize)]
struct MapArgs {
    modes: OneOrMany,
    keys: String,
    func: FuncId,
    #[serde(default)]
    nowait: bool,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    when: Option<FuncId>,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    priority: i32,
}

#[derive(serde::Deserialize)]
struct UnmapArgs {
    modes: OneOrMany,
    keys: String,
    #[serde(default)]
    namespace: Option<String>,
}

/// What `clearMappings` and `getMappings` filter by (`None`: any);
/// `clearMappings` has no `keys`.
#[derive(serde::Deserialize)]
struct MappingFilter {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    keys: Option<String>,
}

#[derive(serde::Deserialize)]
struct SetOptionArgs {
    name: String,
    /// Left out (undefined, which JSON drops): null, unsetting it.
    #[serde(default)]
    value: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct GetOptionArgs {
    name: String,
}

/// A command from JavaScript: send it, then wait for the answer. A hook the
/// command causes arrives meanwhile and runs right here, nested (Vim's
/// autocmds run inside the command that triggered them). A call the session
/// sent before it saw this command (a key typed while a timer or a task ran)
/// waits until nothing runs: run here, it would change what the code sees
/// between two of its commands, inside an atomic section too.
fn request<'js>(ctx: &Ctx<'js>, st: &Rc<State>, token: &str, op: &str, payload: &str) -> rquickjs::Result<String> {
    let throw = |m: &str| Err(Exception::throw_message(ctx, m));
    let Ok(token) = serde_json::from_str::<Token>(token) else { return throw("internal: bad context token") };
    let op = match host_op(op, payload) {
        Ok(op) => op,
        Err(e) => return throw(&e),
    };
    // Waiting for the session is not running code: no deadline meanwhile
    // (calls run here, nested, are attended).
    let unattended = st.unattended.clone();
    unattended.paused(|| {
        let req = st.next_req.get();
        st.next_req.set(req + 1);
        st.send(&FromHost::Request { req, token, op });
        let answer = |result: Result<ScriptValue, String>| match result {
            Ok(v) => Ok(json(&from_script(&v))),
            // Marked, so that one nothing catches shows as the command's.
            Err(e) => Err(Exception::from_message(ctx.clone(), &e).and_then(|x| {
                x.as_object().set(COMMAND_FAILED, true)?;
                Ok(x.throw())
            })?),
        };
        loop {
            if st.gone.get() {
                return throw("the session is gone");
            }
            // Answered while a nested call waited for its own.
            if let Some(result) = st.replies.borrow_mut().remove(&req) {
                return answer(result);
            }
            match st.recv(None) {
                Ok(ToHost::Reply { req: r, result }) if r == req => return answer(result),
                // An outer request's answer: kept for it.
                Ok(ToHost::Reply { req: r, result }) if r < req => {
                    st.replies.borrow_mut().insert(r, result);
                }
                Ok(ToHost::Call { id, func, args, nested: true }) => run_call(ctx, st, id, func, args),
                Ok(ToHost::Eval { id, funcs, args, nested: true }) => run_eval(ctx, st, id, &funcs, args),
                Ok(ToHost::Call { id, func, args, nested: false }) => {
                    st.queued_calls.borrow_mut().push_back(QueuedCall::Call { id, func, args });
                }
                Ok(ToHost::Eval { id, funcs, args, nested: false }) => {
                    st.queued_calls.borrow_mut().push_back(QueuedCall::Eval { id, funcs, args });
                }
                // Started once nothing runs.
                Ok(ToHost::RunCode { id, code }) => st.queued_code.borrow_mut().push_back((id, code)),
                // Settled once nothing runs (as timers fire).
                Ok(ToHost::Resolve { promise, result }) => st.settled.borrow_mut().push_back((promise, result)),
                // One that already stopped a timer is not for this call.
                Ok(ToHost::Interrupt) if !st.take_interrupt_message() => {}
                // The flag is set too: if the script catches this error, the
                // interrupt handler stops it at its next step anyway.
                Ok(ToHost::Interrupt) => return throw("interrupted"),
                Ok(ToHost::Shutdown) | Err(_) => {
                    st.gone.set(true);
                    return throw("the session is gone");
                }
                Ok(other) => st.log(&format!("temzdf script: unexpected {other:?} while waiting for an answer")),
            }
        }
    })
}

/// Run a registered function as call `id` and report it done.
fn run_call(ctx: &Ctx<'_>, st: &Rc<State>, id: u64, func: FuncId, args: Vec<ScriptValue>) {
    // Someone waits for it (<C-c> interrupts it), even nested in a task's
    // wait: not on that task's deadline.
    st.unattended.clone().paused(|| run_call_now(ctx, st, id, func, args));
}

fn run_call_now(ctx: &Ctx<'_>, st: &Rc<State>, id: u64, func: FuncId, args: Vec<ScriptValue>) {
    let f = st.funcs.borrow().get(func as usize).cloned();
    let result = as_call(st, id, || match f {
        None => Err(ScriptError::new(format!("no function {func}"))),
        Some(f) => f
            .restore(ctx)
            .and_then(|f| f.call::<_, Option<String>>((args_json(&args),)))
            .map(|r| {
                r.and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
                    .map_or(ScriptValue::Null, |v| to_script(&v))
            })
            .map_err(|e| error_of(ctx, e)),
    });
    st.send(&FromHost::Done { id, result });
}

/// Evaluate conditions (registered with `register(fn, what, true)`, which
/// return booleans) as call `id`, and report their values.
fn run_eval(ctx: &Ctx<'_>, st: &Rc<State>, id: u64, funcs: &[FuncId], args: Vec<ScriptValue>) {
    st.unattended.clone().paused(|| run_eval_now(ctx, st, id, funcs, args));
}

fn run_eval_now(ctx: &Ctx<'_>, st: &Rc<State>, id: u64, funcs: &[FuncId], args: Vec<ScriptValue>) {
    let args = args_json(&args);
    let results = as_call(st, id, || {
        funcs
            .iter()
            .map(|&func| {
                let f = st.funcs.borrow().get(func as usize).cloned();
                match f {
                    None => Err(ScriptError::new(format!("no function {func}"))),
                    Some(f) => {
                        f.restore(ctx).and_then(|f| f.call::<_, bool>((args.clone(),))).map_err(|e| error_of(ctx, e))
                    }
                }
            })
            .collect()
    });
    st.send(&FromHost::Values { id, results });
}

/// Run `f` as call `id`: its token on the stack meanwhile, and the
/// interrupt flag cleared once no call runs any more.
fn as_call<R>(st: &State, id: u64, f: impl FnOnce() -> R) -> R {
    st.tokens.borrow_mut().push(Token::Call(id));
    let result = f();
    st.tokens.borrow_mut().pop();
    if st.tokens.borrow().is_empty() {
        st.interrupt.clear();
    }
    result
}

/// A call's arguments as the JSON array the prelude's wrappers take.
fn args_json(args: &[ScriptValue]) -> String {
    json(&args.iter().map(from_script).collect::<Vec<_>>())
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
        st.stopped.set(None);
        if let Err(e) = st.unattended(|| f.restore(ctx).and_then(|f| f.call::<_, ()>(()))) {
            log_failure(ctx, st, "a timer", e);
        }
        run_jobs(ctx, st);
    }
}

/// Settle the Promises whose answers came (in the order they came); an
/// answer for no Promise (one nobody awaits any more) is dropped.
fn settle_promises(ctx: &Ctx<'_>, st: &Rc<State>) {
    loop {
        let Some((id, result)) = st.settled.borrow_mut().pop_front() else { return };
        let Some(f) = st.promises.borrow_mut().remove(&id) else { continue };
        let (ok, json) = match result {
            Ok(v) => (true, json(&from_script(&v))),
            Err(e) => (false, e),
        };
        st.unattended(|| {
            if let Err(e) = f.restore(ctx).and_then(|f| f.call::<_, ()>((ok, json))) {
                st.log(&format!("temzdf script: settling a promise failed: {}", error_of(ctx, e)));
            }
        });
        run_jobs(ctx, st);
    }
}

/// Run the pending promise jobs, each on its own time, then report the
/// rejections still without a handler: with no job left, none can come any
/// more but from code that runs later (a timer, a call).
fn run_jobs(ctx: &Ctx<'_>, st: &Rc<State>) {
    loop {
        st.stopped.set(None);
        match st.unattended(|| run_job(ctx)) {
            Ok(true) => {}
            Ok(false) => break,
            // A job's own error rejects its promise; this one ended the job
            // (an interrupt, uncatchable, or a microtask that threw), and
            // whatever awaited it never goes on.
            Err(e) => log_failure(ctx, st, "a job", e),
        }
    }
    report_rejections(ctx, st);
}

/// Run one pending promise job: whether there was one, or the error that
/// ended it (pending on `ctx`, as [`rquickjs::Error::Exception`]).
/// (`Ctx::execute_pending_job` drops that error.)
fn run_job(ctx: &Ctx<'_>) -> rquickjs::Result<bool> {
    let mut job_ctx = std::mem::MaybeUninit::<*mut rquickjs::qjs::JSContext>::uninit();
    // SAFETY: `ctx` is alive and its runtime locked meanwhile (as in
    // `Ctx::execute_pending_job`); QuickJS writes `job_ctx` (unread here).
    let ran = unsafe {
        let rt = rquickjs::qjs::JS_GetRuntime(ctx.as_raw().as_ptr());
        rquickjs::qjs::JS_ExecutePendingJob(rt, job_ctx.as_mut_ptr())
    };
    // The runtime has this one context, which a failed job leaves its
    // exception on.
    if ran < 0 { Err(rquickjs::Error::Exception) } else { Ok(ran > 0) }
}

/// Log that `what` (a timer, a job) ended with `e`: stopped by the
/// interrupt handler, or failed.
fn log_failure(ctx: &Ctx<'_>, st: &State, what: &str, e: rquickjs::Error) {
    let error = error_of(ctx, e);
    let line = match st.stopped.take() {
        Some(Stop::Limit) => format!(
            "temzdf script: {what} was stopped: it ran longer than the unattended limit ({})",
            duration_text(st.unattended.limit)
        ),
        Some(Stop::Interrupt) => format!("temzdf script: {what} was stopped: interrupted"),
        None => format!("temzdf script: {what} failed: {error}"),
    };
    st.log(&line);
}

/// "10 s", "200 ms".
fn duration_text(d: Duration) -> String {
    let ms = d.as_millis();
    if ms.is_multiple_of(1000) { format!("{} s", ms / 1000) } else { format!("{ms} ms") }
}

/// Log the rejections nobody handled, unless a configuration loads.
fn report_rejections(ctx: &Ctx<'_>, st: &State) {
    if st.rejections.held.get() {
        return;
    }
    for text in take_rejections(ctx, st) {
        log_rejection(st, &text);
    }
}

/// The rejections without a handler until now, as text, forgotten.
fn take_rejections(ctx: &Ctx<'_>, st: &State) -> Vec<String> {
    // Taken first: reading a reason can run code (a getter).
    let unhandled = std::mem::take(&mut *st.rejections.unhandled.borrow_mut());
    unhandled.into_iter().filter_map(|(_, reason)| reason.restore(ctx).ok()).map(|r| rejection_text(&r)).collect()
}

/// A rejection's reason as text. An Error: "Name: message" and where (its
/// stack has frames only).
fn rejection_text(reason: &Value<'_>) -> String {
    let error = reason.as_object().and_then(|o| {
        let message: String = o.get("message").ok()?;
        let name: String = o.get("name").unwrap_or_else(|_| "Error".into());
        let stack: String = o.get("stack").unwrap_or_default();
        Some(format!("{name}: {message}\n{stack}").trim_end().to_owned())
    });
    error.or_else(|| reason.get::<rquickjs::Coerced<String>>().ok().map(|c| c.0)).unwrap_or_default()
}

/// When code nobody waits for (timers, promise jobs) must stop: see
/// [`UNATTENDED_LIMIT`](crate::UNATTENDED_LIMIT).
#[derive(Clone)]
struct Unattended {
    start: Instant,
    limit: Duration,
    /// Milliseconds since `start` it must stop at; 0 while none runs.
    deadline: Arc<std::sync::atomic::AtomicU64>,
}

impl Unattended {
    fn new(limit: Duration) -> Self {
        Self { start: Instant::now(), limit, deadline: Arc::default() }
    }

    /// No deadline while `f` runs (a call someone waits for, or a wait for
    /// the session's answer); after it, the time that was left.
    fn paused<R>(&self, f: impl FnOnce() -> R) -> R {
        let now = || self.start.elapsed().as_millis() as u64;
        let before = self.deadline.swap(0, Ordering::SeqCst);
        let left = before.saturating_sub(now());
        let r = f();
        if before != 0 {
            self.deadline.store((now() + left).max(1), Ordering::SeqCst);
        }
        r
    }

    fn overdue(&self) -> bool {
        let d = self.deadline.load(Ordering::SeqCst);
        d != 0 && self.start.elapsed().as_millis() as u64 > d
    }
}

impl State {
    /// Run code nobody waits for: interrupted past the limit, and an
    /// interrupt that reached it (the user's <C-c> while a call waited
    /// behind it) is used up, not left for the next call.
    fn unattended<R>(&self, f: impl FnOnce() -> R) -> R {
        let u = &self.unattended;
        let deadline = (u.start.elapsed() + u.limit).as_millis() as u64;
        u.deadline.store(deadline.max(1), Ordering::SeqCst);
        let r = f();
        u.deadline.store(0, Ordering::SeqCst);
        if self.tokens.borrow().is_empty()
            && let (true, sent) = self.interrupt.take()
        {
            self.interrupts_used.set(self.interrupts_used.get().max(sent));
        }
        self.close_cut_off();
        r
    }

    /// Atomic sections are synchronous: one still open when no code runs was
    /// cut off by an interrupt, which skipped its `finally` and its task's
    /// rejection handler. End it, and its task.
    fn close_cut_off(&self) {
        if !self.tokens.borrow().is_empty() {
            return;
        }
        let open: Vec<Token> = self.open_atomic.borrow_mut().drain(..).rev().collect();
        for token in open {
            self.send(&FromHost::AtomicEnd(token));
            if let Token::Task(t) = token
                && self.running_tasks.borrow_mut().remove(&t)
            {
                // An eval's error goes to whoever ran it.
                let eval = self.evals.borrow_mut().remove(&t);
                let result = match eval {
                    Some(run) => {
                        let result = CodeResult { printed: run.printed, result: Err("interrupted".into()) };
                        self.send(&FromHost::CodeDone { id: run.id, result });
                        Ok(())
                    }
                    None => Err(ScriptError::new("interrupted".to_owned())),
                };
                self.send(&FromHost::TaskDone { task: t, result });
            }
        }
    }

    /// An interrupt's message was read: whether it is still to be acted on
    /// (and now is), rather than used up already (its flag stopped code
    /// nobody waited for).
    fn take_interrupt_message(&self) -> bool {
        let n = self.interrupts_read.get() + 1;
        self.interrupts_read.set(n);
        if n <= self.interrupts_used.get() {
            return false;
        }
        self.interrupts_used.set(n);
        true
    }
}

/// Evaluate the shipped defaults, then the configuration at `path`.
fn load_config(ctx: &Ctx<'_>, st: &Rc<State>, path: Option<&Path>) -> Result<(), ScriptError> {
    st.tokens.borrow_mut().push(Token::Load);
    st.rejections.held.set(true);
    let result = (|| {
        let fill: Function = ctx.globals().get("__temzdfLoad").map_err(|e| error_of(ctx, e))?;
        fill.call::<_, ()>(()).map_err(|e| error_of(ctx, e))?;
        evaluate(ctx, DEFAULTS_NAME, DEFAULTS.to_owned())?;
        let Some(path) = path else { return Ok(()) };
        let path = path.display().to_string();
        let source = read_module(&st.files, &path).map_err(ScriptError::new)?;
        evaluate(ctx, &path, source)
    })();
    st.tokens.borrow_mut().pop();
    st.interrupt.clear();
    st.rejections.held.set(false);
    for text in rejections_to_log(take_rejections(ctx, st), &result) {
        log_rejection(st, &text);
    }
    result
}

/// Which rejections seen while a configuration loaded go to the log: none
/// when the load failed, as a module that throws (or is interrupted)
/// rejects its evaluation's promises, its own and those of the modules
/// importing it, with what the load reports as its error already.
fn rejections_to_log(held: Vec<String>, result: &Result<(), ScriptError>) -> Vec<String> {
    if result.is_err() { Vec::new() } else { held }
}

fn log_rejection(st: &State, text: &str) {
    st.log(&format!("temzdf script: unhandled promise rejection: {text}"));
}

/// Evaluate one module. Its top level only registers things; a top-level
/// `await` that does not settle at once is an error.
fn evaluate(ctx: &Ctx<'_>, name: &str, source: String) -> Result<(), ScriptError> {
    let promise = Module::evaluate(ctx.clone(), name.to_owned(), source).map_err(|e| error_of(ctx, e))?;
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
}

/// A module's JavaScript: TypeScript is stripped of its types.
fn read_module(files: &Files, path: &str) -> Result<String, String> {
    let text = match files(&FsOp::ReadFile(path.to_owned())) {
        Ok(FsReply::Bytes(b)) => String::from_utf8(b).map_err(|_| format!("cannot read {path}: not UTF-8"))?,
        Ok(_) => return Err(format!("cannot read {path}")),
        Err(e) => return Err(format!("cannot read {}", e.message)),
    };
    if path.ends_with(".ts") || path.ends_with(".mts") { crate::strip::strip(path, &text) } else { Ok(text) }
}

/// The property marking an error a command answered with (`prelude.ts`
/// reads it too).
const COMMAND_FAILED: &str = "temzdfCommandFailed";

/// The error a failed call left, as message and stack.
fn error_of(ctx: &Ctx<'_>, e: rquickjs::Error) -> ScriptError {
    if !matches!(e, rquickjs::Error::Exception) {
        return ScriptError::new(e.to_string());
    }
    let caught = ctx.catch();
    if let Some(obj) = caught.as_object() {
        let message: Option<String> = obj.get("message").ok();
        if obj.get::<_, bool>(COMMAND_FAILED).unwrap_or(false) {
            return ScriptError { message: message.unwrap_or_default(), stack: None, command: true };
        }
        let name: Option<String> = obj.get("name").ok();
        let stack: Option<String> = obj.get("stack").ok();
        let message = match (name, message) {
            (Some(n), Some(m)) if !n.is_empty() => format!("{n}: {m}"),
            (_, Some(m)) => m,
            _ => "error".into(),
        };
        return ScriptError { message, stack: stack.filter(|s| !s.trim().is_empty()), command: false };
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
struct Resolve {
    files: Files,
    paths: Rc<Cell<PathStyle>>,
}

impl Resolver for Resolve {
    fn resolve<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if name == "temzdf" || BUNDLED.iter().any(|(n, _)| *n == name) {
            return Ok(name.into());
        }
        if name.starts_with("./") || name.starts_with("../") || name.starts_with('/') {
            // A `temzdf eval` step not named by a path: the caller's directory.
            let style = self.paths.get();
            let dir = EVAL_BASES.with(|b| b.borrow().get(base).cloned()).unwrap_or_else(|| paths::parent(style, base));
            return Ok(module_file(&self.files, style, &paths::join(style, &dir, name)));
        }
        Err(Exception::throw_message(
            ctx,
            &format!(
                "cannot import {name:?} from {base}: only \"temzdf\", {} and relative paths can be imported",
                BUNDLED.iter().map(|(n, _)| format!("{n:?}")).collect::<Vec<_>>().join(", ")
            ),
        ))
    }
}

/// The file a relative import names, as TypeScript finds it: the path
/// itself, `x.ts` for `x.js` (TypeScript sources import each other by their
/// output's name), `x.ts` or `x.js` for `x`, or a directory's `index.ts` or
/// `index.js`. Nothing found: the path, for the loader to report.
fn module_file(files: &Files, style: PathStyle, path: &str) -> String {
    let is_file = |p: &str| matches!(files(&FsOp::Stat(p.to_owned())), Ok(FsReply::Info(i)) if i.is_file);
    paths::module_candidates(style, path).into_iter().find(|p| is_file(p)).unwrap_or_else(|| path.to_owned())
}

struct Load {
    files: Files,
}

impl Loader for Load {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js>> {
        let source = if name == "temzdf" {
            TEMZDF_MODULE.to_owned()
        } else if let Some((_, source)) = BUNDLED.iter().find(|(n, _)| *n == name) {
            (*source).to_owned()
        } else {
            read_module(&self.files, name).map_err(|e| Exception::throw_message(ctx, &e))?
        };
        Module::declare(ctx.clone(), name, source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_load_error_is_not_logged_again_as_a_rejection() {
        let held = || vec!["Error: bad config\n    at <anonymous> (config.js:1:7)".to_owned(); 2];
        let failed = Err(ScriptError::new("Error: bad config"));
        assert!(rejections_to_log(held(), &failed).is_empty());
        // A load that went fine: they are rejections nobody handles.
        assert_eq!(rejections_to_log(held(), &Ok(())).len(), 2);
    }

    #[test]
    fn a_payload_without_its_function_is_refused() {
        // Not function 0: an internal error the call throws.
        for (op, payload) in [("on", r#"{"event":"KeyTyped"}"#), ("off", r#"{"event":"KeyTyped"}"#), ("schedule", "{}")]
        {
            let e = host_op(op, payload).unwrap_err();
            assert!(e.starts_with(&format!("internal: {op}: ")), "{e}");
            assert!(e.contains("func"), "{e}");
        }
        let e = host_op("map", r#"{"modes":"normal","keys":"a"}"#).unwrap_err();
        assert!(e.starts_with("internal: map: ") && e.contains("func"), "{e}");
    }

    #[test]
    fn payloads_of_the_wrong_shape_are_refused() {
        for (op, payload) in [
            ("defineMode", r#"{"name":"m","fallback":-1}"#),
            ("defineMode", r#"{"name":"m","fallback":1.5}"#),
            ("defineMode", r#"{"name":"m","fallback":"eat"}"#),
            ("defineMode", r#"{"name":"m","keepalive":-2}"#),
            ("defineMode", r#"{"name":"m","keepalive":0.5}"#),
            ("defineMode", r#"{"name":"m","keepalive":"drop"}"#),
            ("map", r#"{"modes":42,"keys":"a","func":1}"#),
            ("map", r#"{"modes":["normal",1],"keys":"a","func":1}"#),
            ("map", r#"{"modes":"normal","keys":"a","func":1,"priority":1.5}"#),
            ("on", r#"{"event":"KeyTyped","func":-1}"#),
            ("command", r#"{"name":"message"}"#),
            ("getOption", "null"),
            ("setOption", "not json"),
        ] {
            let e = host_op(op, payload).map(|_| ()).unwrap_err();
            assert!(e.starts_with(&format!("internal: {op}: ")), "{op} {payload}: {e}");
        }
        assert_eq!(host_op("nope", "{}").unwrap_err(), "internal: unknown operation nope");
    }

    #[test]
    fn payloads_the_prelude_sends_are_read() {
        let op = |op: &str, payload: &str| host_op(op, payload).unwrap();
        assert_eq!(
            op("on", r#"{"event":"KeyTyped","func":3,"nested":true}"#),
            HostOp::On { event: "KeyTyped".into(), func: 3, nested: true }
        );
        assert_eq!(op("off", r#"{"event":"KeyTyped","func":3}"#), HostOp::Off { event: "KeyTyped".into(), func: 3 });
        assert_eq!(op("schedule", r#"{"func":7}"#), HostOp::Schedule { func: 7 });
        assert_eq!(
            op("command", r#"{"name":"message","args":["hi",1]}"#),
            HostOp::Command {
                name: "message".into(),
                args: vec![ScriptValue::Str("hi".into()), ScriptValue::Num(1.0)]
            }
        );
        assert_eq!(
            op(
                "defineMode",
                r#"{"name":"m","fallback":"passthrough","count":null,"transient":true,"keepalive":4,"timeout":null}"#
            ),
            HostOp::DefineMode {
                name: "m".into(),
                fallback: Some(Fallback::Passthrough),
                count: None,
                transient: Some(true),
                keepalive: Some(Keepalive::Func(4)),
                timeout: None,
            }
        );
        assert_eq!(
            op("defineMode", r#"{"name":"m","fallback":2,"keepalive":"always"}"#),
            HostOp::DefineMode {
                name: "m".into(),
                fallback: Some(Fallback::Func(2)),
                count: None,
                transient: None,
                keepalive: Some(Keepalive::Always),
                timeout: None,
            }
        );
        assert_eq!(
            op(
                "map",
                r#"{"modes":["normal","copy"],"keys":"a","nowait":true,"desc":"d","when":null,"namespace":"ns","priority":-2,"func":5}"#
            ),
            HostOp::Map {
                modes: vec!["normal".into(), "copy".into()],
                keys: "a".into(),
                func: 5,
                nowait: true,
                desc: Some("d".into()),
                when: None,
                namespace: Some("ns".into()),
                priority: -2,
            }
        );
        assert_eq!(
            op("unmap", r#"{"modes":"normal","keys":"a","namespace":null}"#),
            HostOp::Unmap { modes: vec!["normal".into()], keys: "a".into(), namespace: None }
        );
        assert_eq!(
            op("getMappings", r#"{"mode":"normal","namespace":null,"keys":"t"}"#),
            HostOp::GetMappings { mode: Some("normal".into()), namespace: None, keys: Some("t".into()) }
        );
        assert_eq!(
            op("clearMappings", r#"{"mode":null,"namespace":"ns"}"#),
            HostOp::ClearMappings { mode: None, namespace: Some("ns".into()) }
        );
        // An option set to undefined: JSON leaves the value out.
        assert_eq!(
            op("setOption", r#"{"name":"timeoutlen"}"#),
            HostOp::SetOption { name: "timeoutlen".into(), value: ScriptValue::Null }
        );
        assert_eq!(op("getOption", r#"{"name":"timeoutlen"}"#), HostOp::GetOption { name: "timeoutlen".into() });
    }
}
