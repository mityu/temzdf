//! The script host as the server runs it: the guest in wasmtime. The host's
//! cases (crates/temzdf-script/tests/host/cases.rs) run against it through a
//! [`ScriptHost`] bridged to Unix sockets; then what only the runner does:
//! stopping a guest that ignores everything, and the memory cap.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use temzdf_protocol::script::{CommandSpec, FromHost, ScriptValue, ToHost};
use temzdf_script_runner::{Event, Options, ScriptHost};

#[path = "../../temzdf-script/tests/host/cases.rs"]
mod cases;

/// The cases' transport: messages from `host_in` go to the guest, the
/// guest's to `host_out`; when the guest ends, `host_out` closes.
fn spawn_host(host_in: UnixStream, mut host_out: UnixStream, limit: Duration) {
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
    let commands = cases::command_names()
        .into_iter()
        .map(|name| CommandSpec { name, doc: String::new(), ts: "(...args: unknown[]): void".into() })
        .collect();
    assert!(host.send(ToHost::Load { config: Some(path), commands }));
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
