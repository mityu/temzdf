//! The script host against a scripted fake session: configuration loading,
//! synchronous calls both ways, nested hooks, interrupts, dispatched tasks,
//! atomic sections and context tokens.

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
        std::thread::spawn(move || temzdf_script::run_with_limit(host_in, host_out, limit).unwrap());
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
/// `action.fooBar(` in runtime/defaults.ts, as `foo-bar`).
fn command_names() -> Vec<String> {
    let mut names: Vec<String> = ["split", "new-pane", "message"].iter().map(|s| s.to_string()).collect();
    let defaults = include_str!("../runtime/defaults.ts");
    for part in defaults.split("action.").skip(1) {
        let camel: String = part.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
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
        HostOp::On { event, func, nested } => format!("{t} on {event} f{func} nested={nested}"),
        HostOp::Map { modes, keys, rhs: MapRhs::Func(f), .. } => format!("{t} map {} {keys} f{f}", modes.join(",")),
        // The shipped defaults map actions; leave them out.
        HostOp::Map { rhs: MapRhs::Actions(_), .. } => String::new(),
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
    s.send(ToHost::Call { id: 5, func: 1, args: vec![] });
    match s.serve(&mut log) {
        FromHost::Done { id: 5, result: Ok(()) } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(log, ["call5 split(right)", "call5 new-pane()"]);

    // A hook gets its arguments.
    log.clear();
    s.send(ToHost::Call { id: 6, func: 0, args: vec![ScriptValue::Num(3.0)] });
    assert!(matches!(s.serve(&mut log), FromHost::Done { id: 6, result: Ok(()) }));
    assert_eq!(log, ["call6 message(opened 3)"]);
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
    s.send(ToHost::Call { id: 1, func: 1, args: vec![] });
    // The split: before answering it, the session runs the hook.
    let FromHost::Request { req: split, token: Token::Call(1), .. } = s.recv() else { panic!() };
    s.send(ToHost::Call { id: 2, func: 0, args: vec![] });
    let FromHost::Request { req, token: Token::Call(2), op } = s.recv() else { panic!() };
    assert_eq!(describe(Token::Call(2), &op), "call2 message(hook)");
    s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
    assert!(matches!(s.recv(), FromHost::Done { id: 2, result: Ok(()) }));
    s.send(ToHost::Reply { req: split, result: Ok(ScriptValue::Null) });
    let mut log = Vec::new();
    assert!(matches!(s.serve(&mut log), FromHost::Done { id: 1, result: Ok(()) }));
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
    s.send(ToHost::Call { id: 1, func: 0, args: vec![] });
    let FromHost::Done { result: Err(e), .. } = s.serve(&mut Vec::new()) else { panic!() };
    assert!(e.message.contains("synchronous"), "{e}");
    s.send(ToHost::Call { id: 2, func: 1, args: vec![] });
    let FromHost::Done { result: Err(e), .. } = s.serve(&mut Vec::new()) else { panic!() };
    assert_eq!(e.message, "Error: boom");
    assert!(e.stack.as_deref().unwrap_or_default().contains("config.ts:3"), "{e}");
    // A failed command (the session refused it) is an exception too.
    s.send(ToHost::Call { id: 3, func: 2, args: vec![] });
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
    s.send(ToHost::Call { id: 1, func: 0, args: vec![] });
    std::thread::sleep(Duration::from_millis(100));
    s.send(ToHost::Interrupt);
    assert!(matches!(s.recv(), FromHost::Done { id: 1, result: Err(_) }));
    // Blocked in a command the session does not answer: the interrupt ends
    // the wait, and the catch cannot keep it running.
    s.send(ToHost::Call { id: 2, func: 1, args: vec![] });
    let FromHost::Request { .. } = s.recv() else { panic!() };
    s.send(ToHost::Interrupt);
    assert!(matches!(s.recv(), FromHost::Done { id: 2, result: Err(_) }));
    // The host still works afterwards.
    s.send(ToHost::Call { id: 3, func: 99, args: vec![] });
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
    assert!(e.to_string().contains("only \"temzdf\" and relative paths"), "{e}");
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
    s.send(ToHost::Call { id: 1, func: 0, args: vec![] });
    s.send(ToHost::Interrupt);
    log.clear();
    match s.serve(&mut log) {
        FromHost::Done { id: 1, result: Ok(()) } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(log, ["call1 message(ran)"]);
    // Nobody pressing <C-c>: the timer is stopped at the limit.
    let mut s = Session::start_with_limit(&[("config.js", config)], Duration::from_millis(200));
    s.load("config.js");
    ready_ok(s.serve(&mut Vec::new()));
    s.send(ToHost::Call { id: 2, func: 0, args: vec![] });
    assert!(matches!(s.serve(&mut Vec::new()), FromHost::Done { id: 2, result: Ok(()) }));
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
import { action, cmd, getMappings, keymap } from "temzdf";
keymap.add("normal", "t", () => cmd.message("t"), { when: (ctx) => ctx.paneKind === "terminal", namespace: "mine" });
keymap.add("normal", "u", action.split("right"), { when: () => { throw new Error("bad"); } });
keymap.add("normal", "v", action.split("right"), { when: (async () => true) as any });
keymap.add("normal", "w", () => {
  for (const m of getMappings({ mode: "normal", namespace: "mine" })) keymap.add("copy", m.lhs, m.rhs, { when: m.when });
});
"#;
    let mut s = Session::start(&[("config.ts", config)]);
    s.load("config.ts");
    let mut maps = Vec::new();
    let ready = loop {
        match s.recv() {
            FromHost::Request { req, op, .. } => {
                if let HostOp::Map { keys, rhs, when, namespace, .. } = &op
                    && namespace.as_deref() != Some("default")
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
    assert_eq!(maps[0], ("t".into(), MapRhs::Func(1), Some(0), Some("mine".into())));
    let (when_u, when_v) = (maps[1].2.unwrap(), maps[2].2.unwrap());

    // Values in order: true, an exception, a Promise (refused).
    let ctx = ScriptValue::map([("paneKind", ScriptValue::Str("terminal".into()))]);
    s.send(ToHost::Eval { id: 9, funcs: vec![0, when_u, when_v], args: vec![ctx] });
    match s.recv() {
        FromHost::Values { id: 9, results } => {
            assert_eq!(results[0], Ok(true));
            assert!(results[1].as_ref().is_err_and(|e| e.message.contains("bad")), "{results:?}");
            assert!(results[2].as_ref().is_err_and(|e| e.message.contains("synchronous")), "{results:?}");
        }
        other => panic!("{other:?}"),
    }

    // getMappings gives the functions back: mapping them again reuses
    // their ids.
    let w = match &maps[3].1 {
        MapRhs::Func(f) => *f,
        other => panic!("{other:?}"),
    };
    s.send(ToHost::Call { id: 10, func: w, args: vec![] });
    let mut copied = None;
    loop {
        match s.recv() {
            FromHost::Request { req, op: HostOp::GetMappings { mode, namespace }, .. } => {
                assert_eq!((mode.as_deref(), namespace.as_deref()), (Some("normal"), Some("mine")));
                let func = |f: u32| ScriptValue::map([("func", ScriptValue::Num(f as f64))]);
                let m = ScriptValue::map([
                    ("mode", ScriptValue::Str("normal".into())),
                    ("lhs", ScriptValue::Str("t".into())),
                    ("rhs", func(1)),
                    ("when", func(0)),
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
    assert_eq!(copied, Some((vec!["copy".to_owned()], MapRhs::Func(1), Some(0))));
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
    s.send(ToHost::Call { id: 7, func: 0, args: vec![] });
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
    s.send(ToHost::Call { id: 9, func: 0, args: vec![] });
    assert!(matches!(s.recv(), FromHost::Done { id: 9, result: Ok(()) }));
    s.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
    assert!(matches!(s.serve(&mut Vec::new()), FromHost::TaskDone { task: 1, result: Ok(()) }));
}
