//! The actions scripts' tests drive the session with (`test.*`,
//! test_actions.rs).

use std::time::Instant;

use temzdf_protocol::script::ScriptValue;

use super::host_tests::GEOMETRY;
use super::*;

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
