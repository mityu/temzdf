//! temzdf/test (crates/temzdf-script/runtime/test/) and the actions it
//! drives the session with (`test.*`, test_actions.rs): test files run as
//! the configuration of a session with test reports on, in real time as
//! `temzdf test` will run them, their TAP checked line by line.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use temzdf_protocol::script::ScriptValue;

use super::host_tests::GEOMETRY;
use super::*;
use temzdf_scene::sink::channel_sink;

/// What a test file reported, and its counts once it ended.
struct Run {
    lines: Vec<String>,
    end: Option<(u64, u64)>,
}

impl Run {
    fn has(&self, line: &str) -> bool {
        self.lines.iter().any(|l| l == line)
    }

    /// The TAP, for assertion messages.
    fn tap(&self) -> String {
        self.lines.join("\n")
    }
}

/// Run `tests` (a test file's source) as the configuration of a session
/// editing `text`, with test reports on, until it ends.
fn run_tests(name: &str, text: &str, tests: &str) -> Run {
    let dir = std::env::temp_dir().join(format!("temzdf-js-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("case.test.ts");
    std::fs::write(&config, tests).unwrap();
    let file = dir.join("case.txt");
    std::fs::write(&file, text).unwrap();
    let (tx, rx) = mpsc::channel();
    let sink: EventSink = channel_sink(tx, |e| e);
    let settings =
        Settings { serve_forward: false, script: Some(ScriptSettings { config: Some(config) }), overrides: Vec::new() };
    let mut s = Session::new(format!("js-{name}"), GEOMETRY, Hints::default(), settings, sink);
    s.set_shell(Some(vec!["/bin/sh".into()]));
    s.enable_test_reports();
    s.start(PaneSpec::File(file), None).unwrap();
    let mut run = Run { lines: Vec::new(), end: None };
    let deadline = Instant::now() + Duration::from_secs(60);
    while run.end.is_none() {
        assert!(Instant::now() < deadline, "no end:\n{}", run.tap());
        if let Some(e) = s.config_error() {
            panic!("the test file did not load: {e}");
        }
        if let Some(e) = s.script_host_exit() {
            panic!("the script host ended: {e}\n{}", run.tap());
        }
        let wait = s
            .next_deadline()
            .map_or(Duration::from_millis(20), |d| d.saturating_duration_since(Instant::now()))
            .min(Duration::from_millis(20));
        if let Ok(ev) = rx.recv_timeout(wait) {
            s.handle(ev, Instant::now());
            while let Ok(ev) = rx.try_recv() {
                s.handle(ev, Instant::now());
            }
        }
        let _ = s.poll(Instant::now());
        for e in s.take_effects() {
            if let temzdf_scene::Effect::ReadClipboard { ticket, .. } = e {
                s.clipboard_answer(ticket, Err("no clipboard".into()));
            }
        }
        let _ = s.frame();
        for r in s.take_test_reports() {
            match r {
                TestReport::Line(l) => run.lines.push(l),
                TestReport::End { failed, total } => run.end = Some((failed, total)),
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    run
}

/// Whether the test numbered `n` named `name` passed (`ok`) or not.
fn result(run: &Run, name: &str) -> Option<bool> {
    run.lines.iter().find_map(|l| {
        let rest = l.strip_prefix("ok ").map(|r| (true, r)).or_else(|| l.strip_prefix("not ok ").map(|r| (false, r)));
        let (ok, rest) = rest?;
        let (_, n) = rest.split_once(' ')?;
        (n == name).then_some(ok)
    })
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn tests_report_tap_with_failures_shown_by_loupe() {
    let run = run_tests(
        "tap",
        "x\n",
        r#"
import { module, test } from "temzdf/test";
class Point { x: number; y: number; constructor(x: number, y: number) { this.x = x; this.y = y; } }
test("passes", (assert) => assert.equal(1 + 1, 2));
module("values");
test("a Map", (assert) => assert.deepEqual(new Map([["k", 1]]), new Map([["k", 2]])));
test("a class", (assert) => assert.deepEqual(new Point(1, 2), new Point(1, 3)));
test("ok", (assert) => assert.ok(0));
test("throws", () => { throw new Error("boom"); });
"#,
    );
    assert_eq!(run.end, Some((4, 5)), "{}", run.tap());
    assert_eq!(run.lines.first().map(String::as_str), Some("TAP version 13"));
    assert_eq!(result(&run, "passes"), Some(true), "{}", run.tap());
    for name in ["values > a Map", "values > a class", "values > ok", "values > throws"] {
        assert_eq!(result(&run, name), Some(false), "{name}:\n{}", run.tap());
    }
    for line in [
        "  actual: Map{ 'k' => 1 }",
        "  expected: Map{ 'k' => 2 }",
        "  actual: Point{ x: 1, y: 2 }",
        "  expected: Point{ x: 1, y: 3 }",
        "1..5",
        "# fail 4",
    ] {
        assert!(run.has(line), "{line:?} in:\n{}", run.tap());
    }
    assert!(run.lines.iter().any(|l| l.contains("boom")), "{}", run.tap());
    assert!(!run.tap().contains('\x1b'), "no colours");
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn keys_drive_the_vim_mode_one_at_a_time() {
    let run = run_tests(
        "vim",
        "x\n",
        r#"
import { test } from "temzdf/test";
const text = (t: any, pane: number) => t.action.text.getText(undefined, { pane });
test("dd and u", async (assert, t) => {
  const pane = t.openText("one\ntwo\nthree\n");
  const r = await t.keys("dd");
  assert.deepEqual(r, { errors: [], messages: [], pending: "", mode: "vim/normal" });
  assert.equal(text(t, pane), "two\nthree\n");
  await t.keys("u");
  assert.equal(text(t, pane), "one\ntwo\nthree\n");
});
test("a macro records the keys typed", async (assert, t) => {
  const pane = t.openText("1\n2\n3\n4\n");
  await t.keys("qaddq");
  assert.equal(t.action.register("a")?.text, "dd");
  await t.keys("@a");
  assert.equal(text(t, pane), "3\n4\n");
});
test("insert, and each test starts in the normal mode", async (assert, t) => {
  t.openText("");
  const r = await t.keys("ihello");
  assert.equal(r.mode, "vim/insert");
});
test("the mode was reset", (assert, t) => {
  assert.equal(t.action.currentMode(), "vim/normal");
});
test("test.reset goes back to the base mode", async (assert, t) => {
  t.openText("");
  await t.keys("i");
  t.action.test.reset();
  assert.equal(t.action.currentMode(), "vim/normal");
});
"#,
    );
    assert_eq!(run.end, Some((0, 5)), "{}", run.tap());
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn keys_flush_a_prefix_unless_told_not_to() {
    let run = run_tests(
        "flush",
        "x\n",
        r#"
import { keymap, action } from "temzdf";
import { test } from "temzdf/test";
let ran: string[] = [];
keymap.add("vim/normal", "<F3>", () => { ran.push("short"); });
keymap.add("vim/normal", "<F3><F4>", () => { ran.push("long"); });
test("flushed", async (assert, t) => {
  ran = [];
  const r = await t.keys("<F3>");
  assert.equal(r.pending, "");
  assert.deepEqual(ran, ["short"]);
});
test("kept pending", async (assert, t) => {
  ran = [];
  const r = await t.keys("<F3>", { flush: false });
  assert.equal(r.pending, "<F3>");
  assert.deepEqual(ran, []);
  await t.keys("<F4>");
  assert.deepEqual(ran, ["long"]);
});
test("settle flushes too", async (assert, t) => {
  ran = [];
  await t.keys("<F3>", { flush: false });
  const r = await t.settle();
  assert.equal(r.pending, "");
  assert.deepEqual(ran, ["short"]);
});
"#,
    );
    assert_eq!(run.end, Some((0, 3)), "{}", run.tap());
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn keys_wait_for_each_other_however_long_the_session_takes() {
    // A slow KeyTyped hook (as the Vim mode's while recording a macro)
    // takes longer than timeoutlen: the prefix still waits for its next
    // key. And `<C-c>` after a slow mapping comes after it (an ordinary
    // key), not while it runs (an interrupt).
    let run = run_tests(
        "slow",
        "x\n",
        r#"
import { dispatch, keymap, o, on } from "temzdf";
import { test } from "temzdf/test";
let ran: string[] = [];
const busy = (ms: number) => { const s = Date.now(); while (Date.now() - s < ms) {} };
keymap.add("vim/normal", "<F3>", () => { ran.push("short"); });
keymap.add("vim/normal", "<F3><F4>", () => { ran.push("long"); });
keymap.add("vim/normal", "<F7>", () => { busy(100); ran.push("slow"); });
keymap.add("vim/normal", "<F8>", () => dispatch(async () => {
  await new Promise((resolve) => setTimeout(resolve, 100));
  ran.push("task");
}));
test("a prefix outlasts timeoutlen", async (assert, t) => {
  ran = [];
  const [off, before] = t.run(() => {
    const before = o.timeoutlen;
    o.timeoutlen = 20;
    return [on("KeyTyped", () => busy(100)), before] as const;
  });
  t.after(() => t.run(() => { off(); o.timeoutlen = before; }));
  await t.keys("<F3><F4>");
  assert.deepEqual(ran, ["long"]);
});
test("<C-c> after a slow mapping", async (assert, t) => {
  ran = [];
  await t.keys("<F7><C-c>");
  assert.deepEqual(ran, ["slow"]);
});
test("a task a mapping dispatched is waited for", async (assert, t) => {
  ran = [];
  await t.keys("<F8>");
  assert.deepEqual(ran, ["task"]);
});
"#,
    );
    assert_eq!(run.end, Some((0, 3)), "{}", run.tap());
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn errors_of_mapped_functions_come_back_and_fail_the_test() {
    let run = run_tests(
        "errors",
        "x\n",
        r#"
import { action, keymap } from "temzdf";
import { test } from "temzdf/test";
keymap.add("vim/normal", "<F5>", () => { throw new Error("boom"); });
keymap.add("vim/normal", "<F6>", () => action.message("hello"));
test("returned", async (assert, t) => {
  const r = await t.keys("<F5>", { allowErrors: true });
  assert.equal(r.errors.length, 1);
  assert.ok(r.errors[0].includes("boom"), r.errors[0]);
  assert.deepEqual(r.messages, [], "errors are not messages");
  const again = await t.keys("<F6>", { allowErrors: true });
  assert.deepEqual(again.errors, [], "only those since the keys");
  assert.deepEqual(again.messages, ["hello"], "information comes as messages");
});
test("failing", async (assert, t) => {
  await t.keys("<F5>");
});
"#,
    );
    assert_eq!(run.end, Some((1, 2)), "{}", run.tap());
    assert_eq!(result(&run, "returned"), Some(true), "{}", run.tap());
    assert!(run.has("  message: \"errors after keys <F5>\""), "{}", run.tap());
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn after_runs_last_registered_first_even_on_failure() {
    let run = run_tests(
        "after",
        "x\n",
        r#"
import { test } from "temzdf/test";
const steps: string[] = [];
test("fails", (assert, t) => {
  t.after(() => { steps.push("first"); });
  t.after(() => { steps.push("second"); });
  assert.ok(false, "on purpose");
});
test("throws", (assert, t) => {
  t.after(() => { steps.push("third"); });
  throw new Error("on purpose");
});
test("saw them", (assert) => assert.deepEqual(steps, ["second", "first", "third"]));
"#,
    );
    assert_eq!(run.end, Some((2, 3)), "{}", run.tap());
    assert_eq!(result(&run, "saw them"), Some(true), "{}", run.tap());
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn a_timed_out_test_is_stopped_before_its_next_keys() {
    let run = run_tests(
        "timeout",
        "x\n",
        r#"
import { test } from "temzdf/test";
let late = "";
let pane = 0;
test("slow", async (assert, t) => {
  pane = t.openText("text\n");
  t.after(() => { late += "after;"; });
  await new Promise((resolve) => setTimeout(resolve, 600));
  try {
    await t.keys("dd");
    late += "typed;";
  } catch (e) {
    late += "stopped;";
  }
}, { timeout: 200 });
test("then", async (assert, t) => {
  await new Promise((resolve) => setTimeout(resolve, 800));
  assert.equal(late, "after;stopped;");
});
"#,
    );
    assert_eq!(run.end, Some((1, 2)), "{}", run.tap());
    assert!(run.has("  message: \"timed out after 200 ms\""), "{}", run.tap());
    assert_eq!(result(&run, "then"), Some(true), "{}", run.tap());
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn waiting_actions_are_refused_while_loading_and_from_the_command_line() {
    // `temzdf action` runs commands through run_command.
    let sink: EventSink = std::sync::Arc::new(|_| true);
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("cli".into(), GEOMETRY, Hints::default(), settings, sink);
    for name in test_actions::WAITING {
        let e = s.run_command(name, &[ScriptValue::Str("x".into())], Instant::now()).unwrap_err();
        assert!(e.contains("only a script"), "{name}: {e}");
    }
    // The configuration's top level.
    let dir = std::env::temp_dir().join(format!("temzdf-js-load-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = "import { action } from \"temzdf\";\naction.test.keys(\"x\");\n";
    let (s, _rx) = super::host_tests::config_session(&dir, "x\n", Some(config)).unwrap();
    let e = s.config_error().unwrap_or_default();
    assert!(e.contains("not while the configuration loads"), "{e:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reports_go_to_the_log_unless_collected() {
    let sink: EventSink = std::sync::Arc::new(|_| true);
    let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
    let mut s = Session::new("reports".into(), GEOMETRY, Hints::default(), settings, sink);
    let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, Instant::now()).unwrap().0;
    let summary = ScriptValue::map([("failed", ScriptValue::Num(1.0)), ("total", ScriptValue::Num(3.0))]);
    assert_eq!(run(&mut s, "test.reporting", &[]), ScriptValue::Bool(false));
    // Logged (stderr), not kept.
    run(&mut s, "test.report", &[ScriptValue::Str("ok 1 logged".into())]);
    run(&mut s, "test.end", std::slice::from_ref(&summary));
    assert!(s.take_test_reports().is_empty());
    s.enable_test_reports();
    assert_eq!(run(&mut s, "test.reporting", &[]), ScriptValue::Bool(true));
    run(&mut s, "test.report", &[ScriptValue::Str("ok 1 kept".into())]);
    run(&mut s, "test.end", &[summary]);
    assert_eq!(s.take_test_reports(), [TestReport::Line("ok 1 kept".into()), TestReport::End { failed: 1, total: 3 }]);
    assert!(s.take_test_reports().is_empty(), "taken");
}
