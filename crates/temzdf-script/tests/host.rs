//! The script host against a scripted fake session: configuration loading,
//! synchronous calls both ways, nested hooks, interrupts, dispatched tasks,
//! atomic sections and context tokens.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use temzdf_protocol::script::{CommandSpec, FromHost, HostOp, ScriptValue, ToHost, Token};

struct Session {
    to_host: UnixStream,
    from_host: BufReader<UnixStream>,
    dir: PathBuf,
}

impl Session {
    fn start(files: &[(&str, &str)]) -> Self {
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
        std::thread::spawn(move || temzdf_script::run(host_in, host_out).unwrap());
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
        let config = self.dir.join(file);
        let commands = ["split", "new-pane", "message"]
            .iter()
            .map(|n| CommandSpec { name: n.to_string(), doc: String::new() })
            .collect();
        self.send(ToHost::Load { config, commands });
    }

    /// Answer requests with `null` until the host sends something else.
    fn serve(&mut self, log: &mut Vec<String>) -> FromHost {
        loop {
            match self.recv() {
                FromHost::Request { req, token, op } => {
                    log.push(describe(token, &op));
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
        HostOp::PrefixMap { key, func } => format!("{t} prefix {key} f{func}"),
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
import { cmd, on, prefixMap } from "temzdf";
import { greeting } from "./lib.ts";
const n: number = 2;
on("PaneOpened", (pane: number) => cmd.message(`opened ${pane}`));
prefixMap("t", () => { cmd.split("right"); cmd.newPane(); });
cmd.message(greeting(n));
"#;
    let lib = "export function greeting(n: number): string { return `hello ${n}`; }\n";
    let mut s = Session::start(&[("config.ts", config), ("lib.ts", lib)]);
    s.load("config.ts");
    let mut log = Vec::new();
    ready_ok(s.serve(&mut log));
    assert_eq!(log, ["load on PaneOpened f0 nested=false", "load prefix t f1", "load message(hello 2)"]);

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
import { cmd, on, prefixMap } from "temzdf";
on("PaneOpened", () => cmd.message("hook"));
prefixMap("s", () => { cmd.split("right"); cmd.message("after split"); });
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
    let config = r#"import { prefixMap, cmd } from "temzdf";
prefixMap("a", async () => {});
prefixMap("b", () => { throw new Error("boom"); });
prefixMap("c", () => cmd.message(String(1)));
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
    let config = r#"import { prefixMap, cmd } from "temzdf";
prefixMap("l", () => { for (;;) { try { for (;;) {} } catch (e) {} } });
prefixMap("w", () => { try { cmd.message("waits"); } catch (e) {} for (;;) {} });
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
    let FromHost::Ready(Err(e)) = s.recv() else { panic!() };
    assert!(e.message.contains("dispatch"), "{e}");
    s.load("quick.js");
    ready_ok(s.recv());
    s.load("enum.ts");
    let FromHost::Ready(Err(e)) = s.recv() else { panic!() };
    assert!(e.message.contains("enum.ts:2:"), "{e}");
    s.load("bare.js");
    let FromHost::Ready(Err(e)) = s.recv() else { panic!() };
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
