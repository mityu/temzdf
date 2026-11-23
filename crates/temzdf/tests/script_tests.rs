//! `temzdf test`: test files run each in a process of its own, their TAP
//! printed in one piece, failures and stuck files counted.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_temzdf");

/// Run `temzdf test ARGS FILES` on `files` (name, source) written into a
/// temporary directory: its exit code and stdout.
fn run(args: &[&str], files: &[(&str, &str)]) -> (Option<i32>, String) {
    let dir = tuitest::TempDir::new("script-tests");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    let out = Command::new(BIN)
        .arg("test")
        .args(args)
        .args(files.iter().map(|(name, _)| name))
        .current_dir(dir.path())
        .output()
        .unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned())
}

const PASSES: &str = r#"
import { test } from "temzdf/test";
test("passes", (assert) => assert.equal(1 + 1, 2));
"#;

#[test]
#[cfg_attr(miri, ignore = "runs processes")]
#[cfg_attr(not(unix), ignore = "the script guest cannot read files on Windows yet")]
fn passing_files_exit_with_zero() {
    let (code, out) = run(&[], &[("a.test.ts", PASSES)]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with("# FILE: a.test.ts\nTAP version 13\nok 1 passes\n"), "{out}");
    assert!(out.ends_with("# 1 files, 1 tests: 1 passed, 0 failed\n"), "{out}");
}

#[test]
#[cfg_attr(miri, ignore = "runs processes")]
#[cfg_attr(not(unix), ignore = "the script guest cannot read files on Windows yet")]
fn a_stuck_file_is_stopped_and_the_others_run() {
    let fails = r#"
import { test } from "temzdf/test";
test("fails", (assert) => { console.log("said on stderr"); assert.equal(1, 2); });
"#;
    let stuck = r#"
import { test } from "temzdf/test";
test("loops", () => { for (;;) {} });
"#;
    // (Long enough for the others to start on a busy machine.)
    let (code, out) = run(
        &["-j", "3", "--timeout", "4"],
        &[("stuck.test.ts", stuck), ("fails.test.ts", fails), ("passes.test.ts", PASSES)],
    );
    assert_eq!(code, Some(1), "{out}");
    // Each file in one piece, in the order given.
    let blocks: Vec<&str> = out.split("# FILE: ").skip(1).collect();
    assert_eq!(blocks.len(), 3, "{out}");
    assert!(blocks[0].starts_with("stuck.test.ts\n"), "{out}");
    assert!(blocks[0].contains("Bail out! no TAP line for 4 s: stopped"), "{out}");
    assert!(blocks[1].starts_with("fails.test.ts\n") && blocks[1].contains("not ok 1 fails"), "{out}");
    // A failed file's stderr is shown, a passing one's not.
    assert!(blocks[1].contains("#   temzdf script [log]: said on stderr"), "{out}");
    assert!(blocks[2].starts_with("passes.test.ts\n") && blocks[2].contains("ok 1 passes"), "{out}");
    assert!(out.ends_with("# 3 files (2 failed), 2 tests: 1 passed, 1 failed\n"), "{out}");
}
