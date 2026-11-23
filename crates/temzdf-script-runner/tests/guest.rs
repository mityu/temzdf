//! The script host as the server runs it: the guest in wasmtime. The host's
//! cases (crates/temzdf-script/tests/host/cases.rs) run against it through a
//! [`ScriptHost`] bridged to a pair of sockets; then what only the runner does:
//! stopping a guest that ignores everything, and the memory cap.

use std::io::BufReader;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use temzdf_protocol::Stream;
use temzdf_protocol::script::{FromHost, HostOp, PathStyle, ScriptValue, ToHost};
use temzdf_script_runner::{Event, Options, ScriptHost};

#[path = "../../temzdf-script/tests/host/cases.rs"]
mod cases;

/// The cases' transport: messages from `host_in` go to the guest, the
/// guest's to `host_out`; when the guest ends, `host_out` closes.
fn spawn_host(host_in: Stream, mut host_out: Stream, limit: Duration) {
    let options = Options { unattended_limit: limit, ..Options::default() };
    let host = ScriptHost::start(options, move |e| match e {
        Event::Msg(msg) => {
            let _ = temzdf_protocol::send(&mut host_out, &msg);
        }
        Event::Exited(why) => eprintln!("the guest exited: {why}"),
    })
    .unwrap();
    std::thread::spawn(move || {
        let mut input = BufReader::new(host_in);
        while let Ok(Some(msg)) = temzdf_protocol::recv::<ToHost>(&mut input) {
            let last = msg == ToHost::Shutdown;
            if !host.send(msg) || last {
                break;
            }
        }
    });
}

/// A guest loading `config` (JavaScript, in a file of its own), and its
/// events.
fn start(config: &str, options: Options) -> (ScriptHost, mpsc::Receiver<Event>, std::path::PathBuf) {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("temzdf-guest-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.js");
    std::fs::write(&path, config).unwrap();
    let (tx, rx) = mpsc::channel();
    let host = ScriptHost::start(options, move |e| {
        let _ = tx.send(e);
    })
    .unwrap();
    let commands = cases::command_names().into_iter().collect();
    assert!(host.send(ToHost::Load { config: Some(path), commands, paths: PathStyle::native() }));
    (host, rx, dir)
}

/// The guest's last word, answering its requests with null meanwhile.
fn exited(host: &ScriptHost, rx: &mpsc::Receiver<Event>) -> String {
    loop {
        match rx.recv_timeout(Duration::from_secs(20)).expect("the guest did not end") {
            Event::Exited(why) => return why,
            Event::Msg(FromHost::Request { req, .. }) => {
                host.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
            }
            Event::Msg(_) => {}
        }
    }
}

#[test]
fn stop_ends_a_guest_that_ignores_interrupts() {
    // An endless loop while loading: attended, so only an interrupt would
    // stop it, and this one never reads the session.
    let (mut host, rx, dir) = start("for (;;) {}", Options::default());
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    host.stop();
    assert_eq!(exited(&host, &rx), "it was stopped");
    assert!(t.elapsed() < Duration::from_secs(2), "took {:?}", t.elapsed());
    assert!(!host.send(ToHost::Shutdown), "a stopped guest takes nothing");
    let _ = std::fs::remove_dir_all(dir);
}

/// File operations (temzdf/fs) run on threads of their own, which keep a
/// sender of the guest's queue: stopping still ends the guest at once, and
/// dropping it waits for no worker.
#[test]
fn stop_ends_a_guest_with_file_operations_running() {
    let config = r#"import { dispatch } from "temzdf";
import { readDir } from "temzdf/fs";
dispatch(async () => { for (;;) { for await (const _ of readDir("/")) {} } });
"#;
    let (mut host, rx, dir) = start(config, Options::default());
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    host.stop();
    let why = exited(&host, &rx);
    assert!(why == "it was stopped" || why == "it ended", "{why}");
    drop(host);
    assert!(t.elapsed() < Duration::from_secs(2), "took {:?}", t.elapsed());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_memory_cap_ends_the_guest() {
    // Low, so the test is quick: the guest starts in a few MiB.
    let options = Options { memory_limit: 32 << 20, ..Options::default() };
    let (host, rx, dir) = start("const a = []; for (;;) a.push(new Array(100000).fill(1));", options);
    let why = exited(&host, &rx);
    assert!(why.contains("memory"), "{why}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_interrupt_stops_running_code_through_the_count() {
    let (host, rx, dir) = start("for (;;) {}", Options::default());
    std::thread::sleep(Duration::from_millis(200));
    assert!(host.send(ToHost::Interrupt));
    let ready = loop {
        match rx.recv_timeout(Duration::from_secs(10)).expect("no answer to the interrupt") {
            Event::Msg(FromHost::Ready(r)) => break r,
            Event::Msg(FromHost::Request { req, .. }) => {
                host.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
            }
            other => panic!("{other:?}"),
        }
    };
    let e = ready.expect_err("the load was cut off");
    assert!(e.message.contains("interrupted"), "{e:?}");
    let _ = std::fs::remove_dir_all(dir);
}

// How long the guest takes, as the server runs it (ignored by default;
// doc/survey/script-host-transport.md has the figures):
//
//     cargo test --release -p temzdf-script-runner --test guest keys_and_startup -- --ignored --nocapture
//
// Startup (`ScriptHost::start` to `Ready`, loading the shipped defaults and
// a small configuration), then keys: a mapped function issuing `k` commands
// (`message`), each answered at once, timed from `Call` to `Done` on this
// side of the channel. The figures include the guest's thread and channels,
// which experiments/script-host-bench's form C left out.

const KS: [usize; 3] = [1, 10, 50];
const KEYS: usize = 2000;

/// The next message that is not a request, answering requests (with null)
/// meanwhile; `maps` gets the functions mapped to keys `k1`, `k10`, ...
fn until_answer(host: &ScriptHost, rx: &mpsc::Receiver<Event>, maps: &mut Vec<(usize, u32)>) -> FromHost {
    loop {
        match rx.recv_timeout(Duration::from_secs(10)).expect("no answer") {
            Event::Msg(FromHost::Request { req, op, .. }) => {
                if let HostOp::Map { keys, func: f, .. } = op
                    && let Some(k) = keys.strip_prefix('k').and_then(|k| k.parse().ok())
                {
                    maps.push((k, f));
                }
                host.send(ToHost::Reply { req, result: Ok(ScriptValue::Null) });
            }
            Event::Msg(other) => return other,
            Event::Exited(why) => panic!("the guest exited: {why}"),
        }
    }
}

#[test]
#[ignore = "timing, by hand"]
fn keys_and_startup() {
    let dir = std::env::temp_dir().join(format!("temzdf-guest-timing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = String::from("import { action, keymap } from \"temzdf\";\n");
    for k in KS {
        config += &format!(
            "keymap.add(\"normal\", \"k{k}\", () => {{ for (let i = 0; i < {k}; i++) action.message(\"\"); }});\n"
        );
    }
    std::fs::write(dir.join("config.js"), config).unwrap();
    let commands: Vec<String> = cases::command_names().into_iter().collect();

    // Startup: the first in the process (it also loads the engine and the
    // module), then a few more.
    let mut startups = Vec::new();
    let mut maps = Vec::new();
    let mut kept = None;
    for _ in 0..6 {
        let (tx, rx) = mpsc::channel();
        let t = Instant::now();
        let host = ScriptHost::start(Options::default(), move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        host.send(ToHost::Load {
            config: Some(dir.join("config.js")),
            commands: commands.clone(),
            paths: PathStyle::native(),
        });
        maps.clear();
        match until_answer(&host, &rx, &mut maps) {
            FromHost::Ready(Ok(())) => {}
            other => panic!("{other:?}"),
        }
        startups.push(t.elapsed());
        kept = Some((host, rx));
    }
    println!("startup: first {:?}, then {:?}", startups[0], &startups[1..]);

    let (host, rx) = kept.unwrap();
    for k in KS {
        let func = maps.iter().find(|(kk, _)| *kk == k).expect("mapped").1;
        let mut samples = Vec::new();
        for i in 0..(KEYS + 200) {
            let t = Instant::now();
            host.send(ToHost::Call { id: i as u64, func, args: Vec::new(), nested: false });
            match until_answer(&host, &rx, &mut Vec::new()) {
                FromHost::Done { result: Ok(ScriptValue::Null), .. } => {}
                other => panic!("{other:?}"),
            }
            if i >= 200 {
                samples.push(t.elapsed().as_secs_f64() * 1e6);
            }
        }
        samples.sort_by(f64::total_cmp);
        let at = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
        println!("k={k:2}: p50 {:7.1} µs, p90 {:7.1} µs, p99 {:7.1} µs", at(0.5), at(0.9), at(0.99));
    }
    let _ = std::fs::remove_dir_all(dir);
}
