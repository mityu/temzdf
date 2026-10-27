//! `temzdf standalone pdf --render-frame` with `--events`: a recorded timeline replayed on a
//! simulated clock gives what the keys give, and timing matters where it
//! does in a terminal (wheel acceleration, link previews).

use std::path::Path;
use std::process::Command;

use tuitest::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_temzdf");

/// Render the fixture after `input` (`--keys` or `--events` with a file
/// holding the timeline); returns the PNG's bytes.
fn render(dir: &Path, name: &str, flag: &str, input: &str) -> Vec<u8> {
    let pdf = temzdf_pdfcore::pdf::fixture("sample.pdf");
    let out = dir.join(format!("{name}.png"));
    let arg = if flag == "--events" {
        let file = dir.join(format!("{name}.txt"));
        std::fs::write(&file, input).unwrap();
        file.display().to_string()
    } else {
        input.to_owned()
    };
    let status = Command::new(BIN)
        .args(["standalone", "pdf"])
        .arg(&pdf)
        .args(["--render-frame", &out.display().to_string(), "--size", "90x30", &format!("{flag}={arg}")])
        .env("TEMZDF_LOG_DIR", dir)
        .status()
        .unwrap();
    assert!(status.success(), "{name}");
    std::fs::read(out).unwrap()
}

#[test]
fn a_replayed_timeline_renders_as_its_input_did() {
    let dir = TempDir::new("render");
    let d = dir.path();
    // Keys one by one, a second apart: as --keys.
    assert!(
        render(d, "keys", "--keys", "jjj") == render(d, "timed-keys", "--events", "0 key j\n1000 key j\n2000 key j\n"),
        "the same screen"
    );
    // Wheel steps 30 ms apart accelerate; 200 ms apart they do not.
    let wheel = |gap: u32| (0..3).map(|i| format!("{} wheel down 300 300\n", i * gap)).collect::<String>();
    assert!(render(d, "flick", "--events", &wheel(30)) != render(d, "slow", "--events", &wheel(200)));
    // Resting on a link in the table of contents opens its preview after
    // the delay (300 ms): not yet at 100 ms.
    let hover = |end: u32| format!("0 move 150 412\n{end} end\n");
    let early = render(d, "early", "--events", &hover(100));
    assert!(early == render(d, "none", "--events", "100 end\n"), "no preview yet");
    assert!(early != render(d, "late", "--events", &hover(1000)), "the preview");
}

#[test]
fn bad_timelines_are_refused_with_the_line() {
    let dir = TempDir::new("render-bad");
    let file = dir.path().join("bad.txt");
    std::fs::write(&file, "0 move 1 2\n10 wiggle 3 4\n").unwrap();
    let out = Command::new(BIN)
        .args(["standalone", "pdf"])
        .arg(temzdf_pdfcore::pdf::fixture("sample.pdf"))
        .args(["--render-frame", &dir.path().join("x.png").display().to_string()])
        .arg(format!("--events={}", file.display()))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("line 2"), "{err}");
}
