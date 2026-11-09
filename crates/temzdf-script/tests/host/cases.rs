//! The script host against a scripted fake session: configuration loading,
//! synchronous calls both ways, nested hooks, interrupts, dispatched tasks,
//! atomic sections and context tokens. Shared by the native host's tests
//! (crates/temzdf-script/tests/host.rs) and the WebAssembly guest's
//! (crates/temzdf-script-runner/tests/guest.rs): each starts the host its own
//! way (`crate::spawn_host`), over a pair of Unix sockets.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use temzdf_protocol::script::{CommandSpec, FromHost, HostOp, MapRhs, ScriptValue, ToHost, Token};

struct Session {
    to_host: UnixStream,
    from_host: BufReader<UnixStream>,
    dir: PathBuf,
}

impl Session {
    fn start(files: &[(&str, &str)]) -> Self {
        Self::start_with_limit(files, temzdf_script::UNATTENDED_LIMIT)
    }

    /// With another limit for timers and promise jobs.
    fn start_with_limit(files: &[(&str, &str)], limit: Duration) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("temzdf-script-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        let (server_in, host_out) = UnixStream::pair().unwrap();
        let (host_in, server_out) = UnixStream::pair().unwrap();
        crate::spawn_host(host_in, host_out, limit);
        server_in.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Self { to_host: server_out, from_host: BufReader::new(server_in), dir }
    }

    fn send(&mut self, msg: ToHost) {
        temzdf_protocol::send(&mut self.to_host, &msg).unwrap();
    }

    fn recv(&mut self) -> FromHost {
        temzdf_protocol::recv(&mut self.from_host).unwrap().expect("the host hung up")
    }

    fn load(&mut self, file: &str) {
        let config = Some(self.dir.join(file));
        let commands = command_names()
            .into_iter()
            .map(|name| CommandSpec { name, doc: String::new(), ts: "(...args: unknown[]): void".into() })
            .collect();
        self.send(ToHost::Load { config, commands });
    }

    /// Answer requests with `null` until the host sends something else.
    fn serve(&mut self, log: &mut Vec<String>) -> FromHost {
        loop {
            match self.recv() {
                FromHost::Request { req, token, op } => {
                    let d = describe(token, &op);
                    if !d.is_empty() {
                        log.push(d);
                    }
                    self.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
                }
                other => return other,
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = temzdf_protocol::send(&mut self.to_host, &ToHost::Shutdown);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The commands the tests use, and those the shipped defaults use (every
/// `action.fooBar(` in runtime/defaults.ts as `foo-bar`, `action.ns.fooBar(`
/// as `ns.foo-bar`).
pub(crate) fn command_names() -> Vec<String> {
    let mut names: Vec<String> = ["split", "new-pane", "message", "text.erase", "text.get-text", "read-clipboard"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // The shipped modules the defaults enable (their mappings name actions).
    let shipped = [
        include_str!("../../runtime/defaults.ts"),
        include_str!("../../runtime/pane/pdf.ts"),
        include_str!("../../runtime/pane/terminal.ts"),
        include_str!("../../runtime/pane/markdown.ts"),
        include_str!("../../runtime/mode/normal.ts"),
        include_str!("../../runtime/mode/input.ts"),
        include_str!("../../runtime/edit/notepad.ts"),
        include_str!("../../runtime/buffers.ts"),
    ]
    .concat();
    for part in shipped.split("action.").skip(1) {
        let camel: String = part.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '.').collect();
        let kebab: String = camel
            .chars()
            .map(|c| if c.is_ascii_uppercase() { format!("-{}", c.to_ascii_lowercase()) } else { c.to_string() })
            .collect();
        if !names.contains(&kebab) {
            names.push(kebab);
        }
    }
    names
}

/// How many function ids the shipped defaults take (loaded before every
/// configuration): found by loading an empty one.
fn base() -> u32 {
    static BASE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| {
        let mut s = Session::start(&[("config.js", "")]);
        s.load("config.js");
        let mut most = None;
        loop {
            match s.recv() {
                FromHost::Request { req, op, .. } => {
                    let f = match op {
                        HostOp::On { func, .. } | HostOp::Map { rhs: MapRhs::Func(func), .. } => Some(func),
                        _ => None,
                    };
                    most = most.max(f);
                    s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
                }
                FromHost::Ready(r) => {
                    r.unwrap();
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        most.map_or(0, |m| m + 1)
    })
}

/// The id of the configuration's function number `n` (from 0, in the
/// order registered; the defaults' come first).
fn user(n: u32) -> u32 {
    base() + n
}

/// A request as a log line; the configuration's functions numbered from 0
/// (as [`user`]), the defaults' left out.
fn describe(token: Token, op: &HostOp) -> String {
    let t = match token {
        Token::Call(id) => format!("call{id}"),
        Token::Task(id) => format!("task{id}"),
        Token::Load => "load".into(),
    };
    match op {
        HostOp::Command { name, args } => {
            let args: Vec<String> = args
                .iter()
                .map(|a| match a {
                    ScriptValue::Str(s) => s.clone(),
                    other => format!("{other:?}"),
                })
                .collect();
            format!("{t} {name}({})", args.join(","))
        }
        HostOp::On { func, .. } | HostOp::Map { rhs: MapRhs::Func(func), .. } if *func < base() => String::new(),
        HostOp::On { event, func, nested } => format!("{t} on {event} f{} nested={nested}", func - base()),
        HostOp::Map { modes, keys, rhs: MapRhs::Func(f), .. } => {
            format!("{t} map {} {keys} f{}", modes.join(","), f - base())
        }
        // The shipped defaults map actions; leave them out.
        HostOp::Map { rhs: MapRhs::Actions(_), .. } => String::new(),
        // And they enable the Vim mode (its modes and base mode) and the
        // Markdown viewer's keys (its outline's mode).
        HostOp::DefineMode { name, .. } if name.starts_with("vim/") || name.starts_with("markdown/") => String::new(),
        // Modes the defaults define with their own functions (swap-choice).
        HostOp::DefineMode { fallback: Some(temzdf_protocol::script::Fallback::Func(f)), .. } if *f < base() => {
            String::new()
        }
        HostOp::GetOption { name } | HostOp::SetOption { name, .. } if name == "basemode" => String::new(),
        other => format!("{t} {other:?}"),
    }
}

fn ready_ok(msg: FromHost) {
    match msg {
        FromHost::Ready(Ok(())) => {}
        other => panic!("expected Ready(Ok), got {other:?}"),
    }
}

#[test]
fn a_typescript_configuration_registers_and_runs_commands() {
    let config = r#"
import { cmd, on, keymap } from "temzdf";
import { greeting } from "./lib.ts";
const n: number = 2;
on("PaneOpened", (pane: number) => cmd.message(`opened ${pane}`));
keymap.add("normal", "t", () => { cmd.split("right"); cmd.newPane(); });
cmd.message(greeting(n));
keymap.clear({ namespace: "default" });
keymap.clear();
"#;
    let lib = "export function greeting(n: number): string { return `hello ${n}`; }\n";
    let mut s = Session::start(&[("config.ts", config), ("lib.ts", lib)]);
    s.load("config.ts");
    let mut log = Vec::new();
    ready_ok(s.serve(&mut log));
    assert_eq!(
        log,
        [
            "load on PaneOpened f0 nested=false",
            "load map normal t f1",
            "load message(hello 2)",
            "load ClearMappings { mode: None, namespace: Some(\"default\") }",
            "load ClearMappings { mode: None, namespace: None }",
        ]
    );

    // The prefix mapping: its commands carry its call's token.
    log.clear();
    s.send(ToHost::Call { id: 5, func: user(1), args: vec![] });
    match s.serve(&mut log) {
        FromHost::Done { id: 5, result: Ok(ScriptValue::Null) } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(log, ["call5 split(right)", "call5 new-pane()"]);

    // A hook gets its arguments.
    log.clear();
    s.send(ToHost::Call { id: 6, func: user(0), args: vec![ScriptValue::Num(3.0)] });
    assert!(matches!(s.serve(&mut log), FromHost::Done { id: 6, result: Ok(ScriptValue::Null) }));
    assert_eq!(log, ["call6 message(opened 3)"]);
}

#[test]
fn namespaced_commands_are_objects_of_their_own() {
    let config = r#"
import { action, cmd, keymap } from "temzdf";
keymap.add("input", "<C-w>", () => cmd.text.getText());
keymap.add("input", "<BS>", action.text.erase());
cmd.message(typeof cmd.text + " " + Object.isFrozen(cmd.text) + " " + typeof action.text.getText);
"#;
    let mut s = Session::start(&[("config.ts", config)]);
    s.load("config.ts");
    let mut log = Vec::new();
    ready_ok(s.serve(&mut log));
    assert!(log.contains(&"load message(object true function)".to_owned()), "{log:?}");
    let f: u32 = log
        .iter()
        .find_map(|l| l.strip_prefix("load map input <C-w> f"))
        .expect("the function mapping")
        .parse()
        .unwrap();
    log.clear();
    s.send(ToHost::Call { id: 1, func: user(f), args: vec![] });
    assert!(matches!(s.serve(&mut log), FromHost::Done { id: 1, result: Ok(ScriptValue::Null) }));
    assert_eq!(log, ["call1 text.get-text()"], "the command's whole name");
}

#[test]
fn hooks_run_nested_inside_the_command_that_caused_them() {
    let config = r#"
import { cmd, on, keymap } from "temzdf";
on("PaneOpened", () => cmd.message("hook"));
keymap.add("normal", "s", () => { cmd.split("right"); cmd.message("after split"); });
"#;
    let mut s = Session::start(&[("config.js", config)]);
    s.load("config.js");
    ready_ok(s.serve(&mut Vec::new()));
    s.send(ToHost::Call { id: 1, func: user(1), args: vec![] });
    // The split: before answering it, the session runs the hook.
    let FromHost::Request { req: split, token: Token::Call(1), .. } = s.recv() else { panic!() };
    s.send(ToHost::Call { id: 2, func: user(0), args: vec![] });
    let FromHost::Request { req, token: Token::Call(2), op } = s.recv() else { panic!() };
    assert_eq!(describe(Token::Call(2), &op), "call2 message(hook)");
    s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
    assert!(matches!(s.recv(), FromHost::Done { id: 2, result: Ok(ScriptValue::Null) }));
    s.send(ToHost::Reply { req: split, result: Ok(ScriptValue::Null) });
    let mut log = Vec::new();
    assert!(matches!(s.serve(&mut log), FromHost::Done { id: 1, result: Ok(ScriptValue::Null) }));
    assert_eq!(log, ["call1 message(after split)"]);
}

#[test]
fn mapped_functions_must_be_synchronous_and_errors_carry_positions() {
    let config = r#"import { keymap, cmd } from "temzdf";
keymap.add("normal", "a", async () => {});
keymap.add("normal", "b", () => { throw new Error("boom"); });
keymap.add("normal", "c", () => cmd.message(String(1)));
"#;
    let mut s = Session::start(&[("config.ts", config)]);
    s.load("config.ts");
    ready_ok(s.serve(&mut Vec::new()));
    s.send(ToHost::Call { id: 1, func: user(0), args: vec![] });
    let FromHost::Done { result: Err(e), .. } = s.serve(&mut Vec::new()) else { panic!() };
    assert!(e.message.contains("synchronous"), "{e}");
    s.send(ToHost::Call { id: 2, func: user(1), args: vec![] });
    let FromHost::Done { result: Err(e), .. } = s.serve(&mut Vec::new()) else { panic!() };
    assert_eq!(e.message, "Error: boom");
    assert!(e.stack.as_deref().unwrap_or_default().contains("config.ts:3"), "{e}");
    // A failed command (the session refused it) is an exception too.
    s.send(ToHost::Call { id: 3, func: user(2), args: vec![] });
    let FromHost::Request { req, .. } = s.recv() else { panic!() };
    s.send(ToHost::Reply { req, result: Err("no such pane".into()) });
    let FromHost::Done { result: Err(e), .. } = s.recv() else { panic!() };
    assert!(e.message.contains("no such pane"), "{e}");
}

#[test]
fn an_endless_function_is_interrupted_even_if_it_catches() {
    let config = r#"import { keymap, cmd } from "temzdf";
keymap.add("normal", "l", () => { for (;;) { try { for (;;) {} } catch (e) {} } });
keymap.add("normal", "w", () => { try { cmd.message("waits"); } catch (e) {} for (;;) {} });
"#;
    let mut s = Session::start(&[("config.js", config)]);
    s.load("config.js");
    ready_ok(s.serve(&mut Vec::new()));
    s.send(ToHost::Call { id: 1, func: user(0), args: vec![] });
    std::thread::sleep(Duration::from_millis(100));
    s.send(ToHost::Interrupt);
    assert!(matches!(s.recv(), FromHost::Done { id: 1, result: Err(_) }));
    // Blocked in a command the session does not answer: the interrupt ends
    // the wait, and the catch cannot keep it running.
    s.send(ToHost::Call { id: 2, func: user(1), args: vec![] });
    let FromHost::Request { .. } = s.recv() else { panic!() };
    s.send(ToHost::Interrupt);
    assert!(matches!(s.recv(), FromHost::Done { id: 2, result: Err(_) }));
    // The host still works afterwards.
    s.send(ToHost::Call { id: 3, func: 100, args: vec![] });
    assert!(matches!(s.recv(), FromHost::Done { id: 3, result: Err(_) }));
}

#[test]
fn top_level_await_and_syntax_errors_fail_the_load() {
    let mut s = Session::start(&[
        ("wait.js", "await new Promise(() => {});\n"),
        ("quick.js", "await Promise.resolve(1);\n"),
        ("enum.ts", "const a = 1;\nenum E { A }\n"),
        ("bare.js", "import x from \"lodash\";\n"),
    ]);
    s.load("wait.js");
    let FromHost::Ready(Err(e)) = s.serve(&mut Vec::new()) else { panic!() };
    assert!(e.message.contains("dispatch"), "{e}");
    s.load("quick.js");
    ready_ok(s.serve(&mut Vec::new()));
    s.load("enum.ts");
    let FromHost::Ready(Err(e)) = s.serve(&mut Vec::new()) else { panic!() };
    assert!(e.message.contains("enum.ts:2:"), "{e}");
    s.load("bare.js");
    let FromHost::Ready(Err(e)) = s.serve(&mut Vec::new()) else { panic!() };
    let e = e.to_string();
    assert!(
        e.contains("cannot import \"lodash\"") && e.contains("\"temzdf/edit/vim\"") && e.contains("relative paths"),
        "{e}"
    );
}

#[test]
fn dispatched_tasks_use_their_ctx_and_stray_commands_are_refused() {
    let config = r#"import { cmd, dispatch } from "temzdf";
dispatch(async (ctx) => {
  await new Promise((r) => setTimeout(r, 30));
  ctx.cmd.message("late");
  try { cmd.message("global"); } catch (e) { ctx.cmd.message(String(e.message).split(":")[0]); }
  ctx.atomic(() => { ctx.cmd.message("in atomic"); });
});
"#;
    let mut s = Session::start(&[("config.js", config)]);
    s.load("config.js");
    let mut log = Vec::new();
    let mut events = Vec::new();
    loop {
        match s.serve(&mut log) {
            FromHost::Ready(r) => {
                assert!(r.is_ok());
                events.push("ready".to_owned());
            }
            FromHost::TaskStart(t) => events.push(format!("start{t}")),
            FromHost::AtomicBegin(t) => log.push(format!("begin {t:?}")),
            FromHost::AtomicEnd(t) => log.push(format!("end {t:?}")),
            FromHost::TaskDone { task, result } => {
                assert!(result.is_ok(), "{result:?}");
                events.push(format!("done{task}"));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(events, ["start1", "ready", "done1"]);
    assert_eq!(
        log,
        [
            "task1 message(late)",
            "task1 message(no context)",
            "begin Task(1)",
            "task1 message(in atomic)",
            "end Task(1)"
        ]
    );
}

#[test]
fn commands_answering_later_give_promises_and_atomic_lends_cmd_the_task() {
    // A command answering `{ $promise: n }` is a Promise, settled by
    // ToHost::Resolve; inside ctx.atomic the plain cmd issues as the task.
    let config = r#"import { cmd, dispatch } from "temzdf";
dispatch(async (ctx) => {
  const v = await ctx.cmd.readClipboard();
  ctx.atomic(() => cmd.message(`got ${v}`));
  try { await ctx.cmd.readClipboard(); } catch (e) { ctx.cmd.message(`failed ${e.message}`); }
});
"#;
    let mut s = Session::start(&[("config.js", config)]);
    s.load("config.js");
    let mut log = Vec::new();
    let mut promise = 6;
    loop {
        match s.recv() {
            FromHost::Request { req, token, op } => {
                let reading = matches!(&op, HostOp::Command { name, .. } if name == "read-clipboard");
                let d = describe(token, &op);
                if !d.is_empty() {
                    log.push(d);
                }
                if reading {
                    promise += 1;
                    let p = ScriptValue::map([("$promise", ScriptValue::Num(promise as f64))]);
                    s.send(ToHost::Reply { req, result: Ok(p) });
                    let result = if promise == 7 { Ok(ScriptValue::Str("clip".into())) } else { Err("nope".into()) };
                    s.send(ToHost::Resolve { promise, result });
                } else {
                    s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
                }
            }
            FromHost::Ready(r) => assert!(r.is_ok()),
            FromHost::TaskStart(_) | FromHost::AtomicBegin(_) | FromHost::AtomicEnd(_) => {}
            FromHost::TaskDone { result, .. } => {
                assert!(result.is_ok(), "{result:?}");
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(
        log,
        ["task1 read-clipboard()", "task1 message(got clip)", "task1 read-clipboard()", "task1 message(failed nope)"]
    );
}

#[test]
fn paths_resolve_relative_to_the_importing_module() {
    let mut s = Session::start(&[(
        "config.js",
        "import { v } from \"./sub/m.js\";\nimport { cmd } from \"temzdf\";\ncmd.message(v);\n",
    )]);
    std::fs::create_dir_all(s.dir.join("sub")).unwrap();
    std::fs::write(s.dir.join("sub/m.js"), "export { v } from \"../n.js\";\n").unwrap();
    std::fs::write(s.dir.join("n.js"), "export const v = \"from n\";\n").unwrap();
    s.load("config.js");
    let mut log = Vec::new();
    ready_ok(s.serve(&mut log));
    assert_eq!(log, ["load message(from n)"]);
}

#[test]
fn timers_take_any_delay() {
    // Infinity, huge and odd delays once panicked the host.
    let config = r#"import { cmd } from "temzdf";
const a = setTimeout(() => {}, Infinity);
const b = setTimeout(() => {}, 1e300);
setTimeout(() => {}, NaN);
setTimeout(() => {}, -5);
setTimeout(() => {}, "x");
clearTimeout(a);
clearTimeout(b);
clearTimeout(undefined);
clearTimeout(null);
clearTimeout("x");
cmd.message("ok");
"#;
    let mut s = Session::start(&[("config.js", config)]);
    s.load("config.js");
    let mut log = Vec::new();
    ready_ok(s.serve(&mut log));
    assert_eq!(log, ["load message(ok)"]);
}

#[test]
fn abort_controllers_come_from_the_polyfills() {
    // AbortController (an npm polyfill, runtime/polyfills.ts): abort with a
    // reason, its event, throwIfAborted, and AbortSignal.timeout firing
    // through the host's timers inside a dispatched task.
    let config = r#"import { cmd, dispatch } from "temzdf";
const c = new AbortController();
let heard = "";
c.signal.addEventListener("abort", () => { heard = String(c.signal.reason); });
c.abort("why");
let thrown = "";
try { c.signal.throwIfAborted(); } catch (e) { thrown = String(e); }
cmd.message(`${c.signal.aborted} ${heard} ${thrown}`);
dispatch(async (ctx) => {
  const t = AbortSignal.timeout(5);
  await new Promise<void>((resolve) => t.addEventListener("abort", () => resolve()));
  ctx.atomic(() => ctx.cmd.message(`timeout ${(t.reason as Error).name}`));
});
"#;
    let mut s = Session::start(&[("config.ts", config)]);
    s.load("config.ts");
    let mut log = Vec::new();
    let (mut ready, mut done) = (false, false);
    while !(ready && done) {
        match s.serve(&mut log) {
            FromHost::Ready(r) => {
                ready_ok(FromHost::Ready(r));
                ready = true;
            }
            FromHost::TaskDone { .. } => done = true,
            FromHost::TaskStart(_) | FromHost::AtomicBegin(_) | FromHost::AtomicEnd(_) => {}
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(log[0], "load message(true why why)");
    assert!(log.iter().any(|l| l.ends_with("message(timeout TimeoutError)")), "{log:?}");
}

#[test]
fn a_runaway_timer_is_stopped_and_the_call_behind_it_runs() {
    let config = r#"import { keymap, cmd } from "temzdf";
setTimeout(() => { for (;;) {} }, 0);
keymap.add("normal", "x", () => cmd.message("ran"));
"#;
    // The user's <C-c>: it stops the timer; the call waiting behind it then
    // runs (it used to fail as "interrupted").
    let mut s = Session::start(&[("config.js", config)]);
    s.load("config.js");
    let mut log = Vec::new();
    ready_ok(s.serve(&mut log));
    std::thread::sleep(Duration::from_millis(100));
    s.send(ToHost::Call { id: 1, func: user(0), args: vec![] });
    s.send(ToHost::Interrupt);
    log.clear();
    match s.serve(&mut log) {
        FromHost::Done { id: 1, result: Ok(ScriptValue::Null) } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(log, ["call1 message(ran)"]);
    // Nobody pressing <C-c>: the timer is stopped at the limit.
    let mut s = Session::start_with_limit(&[("config.js", config)], Duration::from_millis(200));
    s.load("config.js");
    ready_ok(s.serve(&mut Vec::new()));
    s.send(ToHost::Call { id: 2, func: user(0), args: vec![] });
    assert!(matches!(s.serve(&mut Vec::new()), FromHost::Done { id: 2, result: Ok(ScriptValue::Null) }));
}

#[test]
fn imports_find_typescript_files_as_tsc_does() {
    let config = r#"import { cmd } from "temzdf";
import { a } from "./lib/a";
import { b } from "./lib/b.js";
import { c } from "./lib";
cmd.message(a + b + c);
"#;
    let mut s = Session::start(&[("config.ts", config)]);
    std::fs::create_dir_all(s.dir.join("lib")).unwrap();
    std::fs::write(s.dir.join("lib/a.ts"), "export const a: string = \"a\";\n").unwrap();
    std::fs::write(s.dir.join("lib/b.ts"), "export const b: string = \"b\";\n").unwrap();
    std::fs::write(s.dir.join("lib/index.ts"), "export const c: string = \"c\";\n").unwrap();
    s.load("config.ts");
    let mut log = Vec::new();
    ready_ok(s.serve(&mut log));
    assert_eq!(log, ["load message(abc)"]);
}

#[test]
fn conditions_answer_booleans_and_get_mappings_gives_the_functions_back() {
    let config = r#"
import { action, cmd, keymap } from "temzdf";
keymap.add("normal", "t", () => cmd.message("t"), { when: (ctx) => ctx.paneKind === "terminal", namespace: "mine" });
keymap.add("normal", "u", action.split("right"), { when: () => { throw new Error("bad"); } });
keymap.add("normal", "v", action.split("right"), { when: (async () => true) as any });
keymap.add("normal", "w", () => {
  for (const m of keymap.get({ mode: "normal", namespace: "mine", lhs: "t" })) keymap.add("copy", m.lhs, m.rhs, { when: m.when });
});
"#;
    let mut s = Session::start(&[("config.ts", config)]);
    s.load("config.ts");
    let mut maps = Vec::new();
    let ready = loop {
        match s.recv() {
            FromHost::Request { req, op, .. } => {
                // (Not the shipped modules' mappings.)
                if let HostOp::Map { keys, rhs, when, namespace, .. } = &op
                    && !namespace.as_deref().is_some_and(|n| n.starts_with("builtin-"))
                {
                    maps.push((keys.clone(), rhs.clone(), *when, namespace.clone()));
                }
                s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
            }
            other => break other,
        }
    };
    ready_ok(ready);
    // Each condition is registered once, before the mapped function.
    assert_eq!(maps[0], ("t".into(), MapRhs::Func(user(1)), Some(user(0)), Some("mine".into())));
    let (when_u, when_v) = (maps[1].2.unwrap(), maps[2].2.unwrap());

    // Values in order: true, an exception, a Promise (refused).
    let ctx = ScriptValue::map([("paneKind", ScriptValue::Str("terminal".into()))]);
    s.send(ToHost::Eval { id: 9, funcs: vec![user(0), when_u, when_v], args: vec![ctx] });
    match s.recv() {
        FromHost::Values { id: 9, results } => {
            assert_eq!(results[0], Ok(true));
            assert!(results[1].as_ref().is_err_and(|e| e.message.contains("bad")), "{results:?}");
            assert!(results[2].as_ref().is_err_and(|e| e.message.contains("synchronous")), "{results:?}");
        }
        other => panic!("{other:?}"),
    }

    // keymap.get gives the functions back: mapping them again reuses
    // their ids.
    let w = match &maps[3].1 {
        MapRhs::Func(f) => *f,
        other => panic!("{other:?}"),
    };
    s.send(ToHost::Call { id: 10, func: w, args: vec![] });
    let mut copied = None;
    loop {
        match s.recv() {
            FromHost::Request { req, op: HostOp::GetMappings { mode, namespace, keys }, .. } => {
                assert_eq!(
                    (mode.as_deref(), namespace.as_deref(), keys.as_deref()),
                    (Some("normal"), Some("mine"), Some("t"))
                );
                let func = |f: u32| ScriptValue::map([("func", ScriptValue::Num(f as f64))]);
                let m = ScriptValue::map([
                    ("mode", ScriptValue::Str("normal".into())),
                    ("lhs", ScriptValue::Str("t".into())),
                    ("rhs", func(user(1))),
                    ("when", func(user(0))),
                    ("nowait", ScriptValue::Bool(false)),
                    ("desc", ScriptValue::Null),
                    ("namespace", ScriptValue::Str("mine".into())),
                    ("priority", ScriptValue::Num(0.0)),
                ]);
                s.send(ToHost::Reply { req, result: Ok(ScriptValue::List(vec![m])) });
            }
            FromHost::Request { req, op: HostOp::Map { modes, rhs, when, .. }, .. } => {
                copied = Some((modes, rhs, when));
                s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
            }
            FromHost::Done { id: 10, result } => {
                result.unwrap();
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(copied, Some((vec!["copy".to_owned()], MapRhs::Func(user(1)), Some(user(0)))));
}

/// What the host says about task 1 until it is done.
fn task_story(s: &mut Session) -> Vec<String> {
    let mut seen = Vec::new();
    loop {
        match s.serve(&mut Vec::new()) {
            FromHost::Ready(r) => assert!(r.is_ok(), "{r:?}"),
            FromHost::TaskStart(_) => {}
            FromHost::AtomicBegin(t) => seen.push(format!("begin {t:?}")),
            FromHost::AtomicEnd(t) => seen.push(format!("end {t:?}")),
            FromHost::TaskDone { task, result } => {
                seen.push(format!("done{task} failed={}", result.is_err()));
                return seen;
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn an_interrupted_atomic_section_still_ends() {
    // The interrupt skips `finally` (QuickJS: uncatchable): the session
    // would stay in the atomic section, holding all input, for good.
    let config = r#"import { dispatch } from "temzdf";
dispatch(async (ctx) => {
  await new Promise((r) => setTimeout(r, 30));
  ctx.atomic(() => { for (;;) {} });
});
"#;
    // At the limit, nobody pressing <C-c>.
    let mut s = Session::start_with_limit(&[("config.js", config)], Duration::from_millis(200));
    s.load("config.js");
    assert_eq!(task_story(&mut s), ["begin Task(1)", "end Task(1)", "done1 failed=true"]);
    // The user's <C-c> (the session sends it: an atomic section makes it busy).
    let mut s = Session::start(&[("config.js", config)]);
    s.load("config.js");
    let mut begun = false;
    while !begun {
        match s.serve(&mut Vec::new()) {
            FromHost::Ready(r) => assert!(r.is_ok(), "{r:?}"),
            FromHost::TaskStart(_) => {}
            FromHost::AtomicBegin(Token::Task(1)) => begun = true,
            other => panic!("{other:?}"),
        }
    }
    s.send(ToHost::Interrupt);
    assert_eq!(task_story(&mut s), ["end Task(1)", "done1 failed=true"]);
}

#[test]
fn an_interrupt_a_task_took_is_not_taken_again() {
    let config = r#"import { dispatch, keymap, cmd } from "temzdf";
keymap.add("normal", "x", () => { cmd.message("x"); });
dispatch(async (ctx) => {
  await new Promise((r) => setTimeout(r, 30));
  try { ctx.cmd.message("wait"); } catch (e) {}
});
"#;
    let mut s = Session::start(&[("config.js", config)]);
    s.load("config.js");
    // Up to the task's command, left unanswered; <C-c> ends its wait.
    loop {
        match s.recv() {
            FromHost::Request { token: Token::Task(1), .. } => break,
            FromHost::Request { req, .. } => s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) }),
            _ => {}
        }
    }
    s.send(ToHost::Interrupt);
    assert!(matches!(s.serve(&mut Vec::new()), FromHost::TaskDone { task: 1, .. }));
    // A call waiting in a command: the user's next <C-c> must end it.
    s.send(ToHost::Call { id: 7, func: user(0), args: vec![] });
    let FromHost::Request { token: Token::Call(7), .. } = s.recv() else { panic!() };
    s.send(ToHost::Interrupt);
    assert!(matches!(s.recv(), FromHost::Done { id: 7, result: Err(_) }));
}

#[test]
fn a_call_inside_a_tasks_wait_gets_its_own_time() {
    let config = r#"import { dispatch, keymap } from "temzdf";
keymap.add("normal", "x", () => { const t = Date.now(); while (Date.now() - t < 100) {} });
dispatch(async (ctx) => { await new Promise((r) => setTimeout(r, 30)); ctx.cmd.message("wait"); });
"#;
    let mut s = Session::start_with_limit(&[("config.js", config)], Duration::from_millis(300));
    s.load("config.js");
    let req = loop {
        match s.recv() {
            FromHost::Request { req, token: Token::Task(1), .. } => break req,
            FromHost::Request { req, .. } => s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) }),
            _ => {}
        }
    };
    // The task has waited past the limit when the user's mapping runs.
    std::thread::sleep(Duration::from_millis(400));
    s.send(ToHost::Call { id: 9, func: user(0), args: vec![] });
    assert!(matches!(s.recv(), FromHost::Done { id: 9, result: Ok(ScriptValue::Null) }));
    s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
    assert!(matches!(s.serve(&mut Vec::new()), FromHost::TaskDone { task: 1, result: Ok(()) }));
}

#[test]
fn answers_taken_by_a_nested_wait_reach_their_request_and_promise() {
    // A task's request waits; a call the session runs meanwhile (a
    // scheduled function) waits for its own, and the task's answer and the
    // Promise's settling come during that inner wait. Neither may be lost,
    // and the Promise settles without another message to wake the host.
    let config = r#"import { cmd, dispatch, keymap } from "temzdf";
keymap.add("normal", "a", () => dispatch(async (ctx) => {
  const v = await ctx.cmd.readClipboard();
  ctx.cmd.message(`got ${v}`);
}));
keymap.add("normal", "b", () => cmd.message("inner"));
"#;
    let mut s = Session::start(&[("config.js", config)]);
    s.load("config.js");
    ready_ok(s.serve(&mut Vec::new()));
    s.send(ToHost::Call { id: 1, func: user(0), args: vec![] });
    let task_req = loop {
        match s.recv() {
            FromHost::TaskStart(_) | FromHost::Done { id: 1, .. } => {}
            FromHost::Request { req, token: Token::Task(_), .. } => break req,
            other => panic!("{other:?}"),
        }
    };
    // Before answering, another call runs; while it waits, the answers come.
    s.send(ToHost::Call { id: 2, func: user(1), args: vec![] });
    let FromHost::Request { req: inner, token: Token::Call(2), .. } = s.recv() else { panic!() };
    let promise = ScriptValue::map([("$promise", ScriptValue::Num(9.0))]);
    s.send(ToHost::Reply { req: task_req, result: Ok(promise) });
    s.send(ToHost::Resolve { promise: 9, result: Ok(ScriptValue::Str("clip".into())) });
    s.send(ToHost::Reply { req: inner, result: Ok(ScriptValue::Null) });
    assert!(matches!(s.recv(), FromHost::Done { id: 2, result: Ok(_) }));
    let FromHost::Request { req, token, op } = s.recv() else { panic!() };
    assert_eq!(describe(token, &op), "task1 message(got clip)");
    s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
    assert!(matches!(s.recv(), FromHost::TaskDone { result: Ok(()), .. }));
}
