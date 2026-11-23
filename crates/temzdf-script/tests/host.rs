//! The script host's cases (tests/host/cases.rs), run natively: the host on a
//! thread of its own over a pair of sockets. Then what only the native host shows
//! a test: its log lines.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use temzdf_protocol::Stream;
use temzdf_protocol::script::{FromHost, ScriptValue, ToHost};

#[path = "host/cases.rs"]
mod cases;

fn spawn_host(host_in: Stream, host_out: Stream, limit: Duration) {
    std::thread::spawn(move || temzdf_script::run_with_limit(host_in, host_out, limit).unwrap());
}

/// A session whose host keeps its log lines (in the second value).
fn start_logged(files: &[(&str, &str)], limit: Duration) -> (cases::Session, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let kept = lines.clone();
    let session = cases::Session::start_with(files, move |host_in, host_out| {
        std::thread::spawn(move || {
            let log = move |line: &str| kept.lock().unwrap().push(line.to_owned());
            temzdf_script::run_with_log(host_in, host_out, limit, log).unwrap();
        });
    });
    (session, lines)
}

/// The log once a line contains each of `marks` (failing after 10 s).
fn log_with(lines: &Mutex<Vec<String>>, marks: &[&str]) -> Vec<String> {
    let start = Instant::now();
    loop {
        let log = lines.lock().unwrap().clone();
        if marks.iter().all(|m| log.iter().any(|l| l.contains(m))) {
            return log;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "no {marks:?} in {log:#?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn only_rejections_nobody_handles_are_logged() {
    // Handlers that come later, as long as they come before the jobs have
    // run out: right after the call (an async function throwing before its
    // first await), from an `await`, from a later job; while loading, in a
    // timer and in a call.
    let config = r#"import { keymap } from "temzdf";
function caught(where) {
  (async () => { throw new Error(`caught-at-once-${where}`); })().catch(() => {});
  const early = async () => { throw new Error(`caught-by-await-${where}`); };
  (async () => { try { await early(); } catch {} })();
  const p = Promise.reject(new Error(`caught-in-a-job-${where}`));
  Promise.resolve().then(() => p.catch(() => {}));
  Promise.reject(new Error(`unhandled-${where}`));
}
caught("load");
setTimeout(() => caught("timer"), 0);
keymap.add("normal", "x", () => caught("call"));
"#;
    let (mut s, lines) = start_logged(&[("config.js", config)], temzdf_script::UNATTENDED_LIMIT);
    s.load("config.js");
    cases::ready_ok(s.serve(&mut Vec::new()));
    s.send(ToHost::Call { id: 1, func: cases::user(0), args: vec![], nested: false });
    assert!(matches!(s.serve(&mut Vec::new()), FromHost::Done { id: 1, result: Ok(ScriptValue::Null) }));
    let log = log_with(&lines, &["unhandled-load", "unhandled-timer", "unhandled-call"]);
    let rejections: Vec<&String> = log.iter().filter(|l| l.contains("unhandled promise rejection")).collect();
    assert_eq!(rejections.len(), 3, "{log:#?}");
    assert!(
        rejections.iter().all(|l| l.starts_with("temzdf script: unhandled promise rejection: Error: unhandled-")),
        "{log:#?}"
    );
}

#[test]
fn a_job_the_limit_or_an_interrupt_stops_is_logged() {
    // Jobs that end with an error of their own (not a rejection): a
    // microtask that throws, and endless ones, stopped by the limit, then by
    // the session's interrupt. The jobs after them still run. And an endless
    // timer.
    let config = r#"setTimeout(() => {
  queueMicrotask(() => { throw new Error("thrown-by-a-microtask"); });
  Promise.resolve().then(() => { for (;;) {} });
  Promise.resolve().then(() => console.log("ran-after-the-endless-one"));
}, 0);
"#;
    let timer = format!("{config}setTimeout(() => {{ for (;;) {{}} }}, 0);\n");
    let (mut s, lines) = start_logged(&[("config.js", &timer)], Duration::from_millis(200));
    s.load("config.js");
    cases::ready_ok(s.serve(&mut Vec::new()));
    let log = log_with(&lines, &["ran-after-the-endless-one", "a timer"]);
    let line = |l: &str| log.iter().any(|x| x == l);
    let failed = "temzdf script: a job failed: Error: thrown-by-a-microtask\n    at ";
    assert!(log.iter().any(|l| l.starts_with(failed)), "{log:#?}");
    assert!(line("temzdf script: a job was stopped: it ran longer than the unattended limit (200 ms)"), "{log:#?}");
    assert!(line("temzdf script: a timer was stopped: it ran longer than the unattended limit (200 ms)"), "{log:#?}");
    // The user's <C-c> stops it before the limit.
    let (mut s, lines) = start_logged(&[("config.js", config)], Duration::from_secs(60));
    s.load("config.js");
    cases::ready_ok(s.serve(&mut Vec::new()));
    std::thread::sleep(Duration::from_millis(100));
    s.send(ToHost::Interrupt);
    let log = log_with(&lines, &["ran-after-the-endless-one"]);
    assert!(log.iter().any(|l| l == "temzdf script: a job was stopped: interrupted"), "{log:#?}");
}

#[test]
fn promise_jobs_a_call_queued_run_before_its_timers() {
    // A microtask checkpoint after the call, as in JavaScript: its promise
    // jobs, and those they queue, before a timer it set for 0 ms.
    let config = r#"import { keymap } from "temzdf";
keymap.add("normal", "x", () => {
  setTimeout(() => console.log("order: timer"), 0);
  Promise.resolve().then(() => {
    console.log("order: job");
    queueMicrotask(() => console.log("order: nested job"));
  });
});
"#;
    let (mut s, lines) = start_logged(&[("config.js", config)], temzdf_script::UNATTENDED_LIMIT);
    s.load("config.js");
    cases::ready_ok(s.serve(&mut Vec::new()));
    s.send(ToHost::Call { id: 1, func: cases::user(0), args: vec![], nested: false });
    assert!(matches!(s.serve(&mut Vec::new()), FromHost::Done { id: 1, result: Ok(ScriptValue::Null) }));
    let log = log_with(&lines, &["order: timer"]);
    let order: Vec<&str> = log.iter().filter_map(|l| l.split_once("order: ").map(|(_, o)| o)).collect();
    assert_eq!(order, ["job", "nested job", "timer"], "{log:#?}");
}
