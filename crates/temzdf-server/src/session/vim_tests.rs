//! The Vim mode (crates/temzdf-script/runtime/vim/, `import "temzdf/vim"`)
//! against hand-written cases (runtime/vim/test/*.cases): a session with
//! the real script host (the WebAssembly guest, in process), keys typed,
//! the text, cursor and mode compared.
//!
//! A case:
//!
//! ```text
//! === name
//! text:
//! hello |world
//! keys: dw
//! want:
//! hello |
//! mode: vim/normal        (optional)
//! ```
//!
//! Optional too: `copy: KIND TEXT` (a copy recorded before the keys, as a
//! terminal pane's: `\n` in TEXT is a line break), `clipboard: TEXT` (what
//! the client's clipboard holds, same escapes), `error: yes`.
//!
//! `|` is the cursor (Vim's cursor is on the character after it). Text and
//! expectation end with a line break (as files do), which is not written.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use temzdf_protocol::script::ScriptValue;
use termpix_core::geom::{CellSize, Geometry};

use super::*;
use crate::keymap::notation;

const GEOMETRY: Geometry = Geometry { cols: 80, rows: 24, cell: CellSize { width: 10, height: 20 } };

struct Case {
    name: String,
    file: String,
    text: String,
    keys: String,
    want: String,
    mode: Option<String>,
    /// An error message is expected (`error: yes`).
    want_error: bool,
    /// Copies recorded before the keys: (kind, text).
    copies: Vec<(String, String)>,
    /// What the client's clipboard holds.
    clipboard: Option<String>,
}

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../temzdf-script/runtime/vim/test")
}

fn parse(file: &str, src: &str) -> Vec<Case> {
    let mut out = Vec::new();
    for block in format!("\n{src}").split("\n=== ").skip(1) {
        let block = block.strip_prefix("=== ").unwrap_or(block);
        let Some((name, rest)) = block.split_once('\n') else { continue };
        if name.trim().is_empty() || name.starts_with('#') || !rest.contains("keys:") {
            continue;
        }
        let text = section(rest, "text:", "keys:");
        let text = text
            .lines()
            .filter(|l| !l.starts_with("copy:") && !l.starts_with("clipboard:"))
            .map(|l| format!("{l}\n"))
            .collect();
        let keys = rest.lines().find_map(|l| l.strip_prefix("keys:")).unwrap_or("").trim().to_owned();
        let after_keys = &rest[rest.find("keys:").unwrap()..];
        let want = section(after_keys, "want:", "mode:");
        let meta = ["error:", "copy:", "clipboard:"];
        let want = want
            .lines()
            .take_while(|l| !meta.iter().any(|m| l.starts_with(m)))
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        let mode = rest.lines().find_map(|l| l.strip_prefix("mode:")).map(|m| m.trim().to_owned());
        let want_error = rest.lines().any(|l| l.trim() == "error: yes");
        let unescape = |s: &str| s.replace("\\n", "\n");
        let copies = rest
            .lines()
            .filter_map(|l| l.strip_prefix("copy: "))
            .filter_map(|l| l.split_once(' '))
            .map(|(k, t)| (k.to_owned(), unescape(t)))
            .collect();
        let clipboard = rest.lines().find_map(|l| l.strip_prefix("clipboard: ")).map(unescape);
        out.push(Case {
            name: name.trim().to_owned(),
            file: file.to_owned(),
            text,
            keys,
            want,
            mode,
            want_error,
            copies,
            clipboard,
        });
    }
    out
}

/// The lines after `start` up to `end` (or the end), each ending with a
/// line break; comment lines (`#`) at the end of a block left out.
fn section(s: &str, start: &str, end: &str) -> String {
    let Some(i) = s.find(start) else { return String::new() };
    let body = &s[i + start.len()..];
    let body = body.strip_prefix('\n').unwrap_or(body);
    let body = match body.find(&format!("\n{end}")) {
        Some(j) => &body[..j + 1],
        None => body,
    };
    let lines: Vec<&str> = body.lines().take_while(|l| !l.starts_with("===")).collect();
    let mut lines = lines;
    while lines.last().is_some_and(|l| l.trim().is_empty() || l.starts_with('#')) {
        lines.pop();
    }
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// The text without the cursor mark, and the cursor as (line, UTF-16
/// column).
fn split_cursor(marked: &str) -> (String, (usize, usize)) {
    let i = marked.find('|').expect("a cursor mark |");
    let (before, after) = marked.split_at(i);
    let line = before.matches('\n').count();
    let col = before.rsplit('\n').next().unwrap_or("").encode_utf16().count();
    (format!("{before}{}", &after[1..]), (line, col))
}

fn mark(text: &str, (line, col): (usize, usize)) -> String {
    let mut out = String::new();
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i == line {
            let units: Vec<u16> = l.encode_utf16().collect();
            let col = col.min(units.len());
            out.push_str(&String::from_utf16_lossy(&units[..col]));
            out.push('|');
            out.push_str(&String::from_utf16_lossy(&units[col..]));
        } else {
            out.push_str(l);
        }
    }
    if text.split_inclusive('\n').count() <= line {
        out.push('|');
    }
    out
}

/// The error messages shown now (a key dismisses them).
fn errors(s: &Session) -> Vec<String> {
    s.notices.iter().filter(|n| n.kind == notices::Kind::Error).map(|n| n.text.clone()).collect()
}

/// Handle events until no script runs (no task either); with `flush`, a key
/// waiting for a longer mapping times out too (after the last key). The
/// client's clipboard holds `clipboard`.
fn settle(s: &mut Session, rx: &Receiver<SessionEvent>, flush: bool, clipboard: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        while let Ok(ev) = rx.try_recv() {
            s.handle(ev, Instant::now());
        }
        for e in s.take_effects() {
            if let temzdf_scene::Effect::ReadClipboard { ticket, .. } = e {
                let answer = clipboard.map(str::to_owned).ok_or_else(|| "no clipboard".to_owned());
                s.clipboard_answer(ticket, answer);
            }
        }
        if !s.holds_input() && s.script.running() && !s.tasks_running() && rx.try_recv().is_err() {
            // A key waiting for a longer mapping: as if its time ran out.
            if flush && s.matcher.deadline(s.options.timeoutlen).is_some() {
                s.poll(Instant::now() + s.options.timeoutlen + Duration::from_millis(1));
                continue;
            }
            return;
        }
        assert!(Instant::now() < deadline, "the script host does not answer");
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(20)) {
            s.handle(ev, Instant::now());
        }
    }
}

/// A session editing `text` in the Vim mode.
fn vim_session(dir: &Path, text: &str) -> Result<(Session, Receiver<SessionEvent>), String> {
    let file = dir.join("case.txt");
    std::fs::write(&file, text).unwrap();
    let config = dir.join("config.ts");
    std::fs::write(&config, "import \"temzdf/vim\";\n").unwrap();
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings =
        Settings { serve_forward: false, script: Some(ScriptSettings { config: Some(config) }), overrides: Vec::new() };
    let mut s = Session::new("vim".into(), GEOMETRY, Hints::default(), settings, sink);
    s.start(PaneSpec::File(file), None).map_err(|e| format!("{e:#}"))?;
    settle(&mut s, &rx, true, None);
    if s.history.current() != "vim/normal" {
        return Err(format!("the Vim mode did not load: mode {}; {:?}", s.history.current(), errors(&s)));
    }
    Ok((s, rx))
}

/// Type `keys` one at a time, settling after each; the errors shown.
fn type_keys(
    s: &mut Session,
    rx: &Receiver<SessionEvent>,
    keys: &str,
    clipboard: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut seen = Vec::new();
    let keys = notation::parse(keys).map_err(|e| format!("keys: {e:#}"))?;
    for (i, k) in keys.iter().enumerate() {
        s.key(&k.event(), Instant::now());
        settle(s, rx, i + 1 == keys.len(), clipboard);
        seen.extend(errors(s));
    }
    Ok(seen)
}

fn run(case: &Case, dir: &Path) -> Result<(), String> {
    let (text, cursor) = split_cursor(&case.text);
    let (mut s, rx) = vim_session(dir, &text)?;
    let pos = ScriptValue::map([
        ("line", ScriptValue::Num(cursor.0 as f64)),
        ("character", ScriptValue::Num(cursor.1 as f64)),
    ]);
    s.run_command("text.set-cursor", &[pos], None, Instant::now())?;
    for (kind, text) in &case.copies {
        let opts = ScriptValue::map([("kind", ScriptValue::Str(kind.clone()))]);
        s.run_command("copy-text", &[ScriptValue::Str(text.clone()), opts], None, Instant::now())?;
    }
    let mut seen = errors(&s);
    seen.extend(type_keys(&mut s, &rx, &case.keys, case.clipboard.as_deref())?);
    let got_text = s.run_command("text.get-text", &[], None, Instant::now())?.0;
    let c = s.run_command("text.cursor", &[], None, Instant::now())?.0;
    let n = |k: &str| c.get(k).and_then(ScriptValue::as_num).unwrap_or(0.0) as usize;
    let got = mark(got_text.as_str().unwrap_or(""), (n("line"), n("character")));
    let mut errors = Vec::new();
    if got != case.want {
        errors.push(format!("want:\n{}got:\n{}", case.want, got));
    }
    if let Some(m) = &case.mode
        && s.history.current() != m
    {
        errors.push(format!("mode {} (want {m})", s.history.current()));
    }
    seen.dedup();
    if !seen.is_empty() && !case.want_error {
        errors.push(format!("errors: {seen:?}"));
    }
    if seen.is_empty() && case.want_error {
        errors.push("no error shown (want one)".to_owned());
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("\n")) }
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn vim_cases() {
    let mut files: Vec<PathBuf> = std::fs::read_dir(cases_dir())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "cases"))
        .collect();
    files.sort();
    let only = std::env::var("VIM_CASE").ok();
    let dir = std::env::temp_dir().join(format!("temzdf-vim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failed = Vec::new();
    let mut count = 0;
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        for case in parse(&name, &std::fs::read_to_string(f).unwrap()) {
            if only.as_ref().is_some_and(|o| !case.name.contains(o.as_str())) {
                continue;
            }
            count += 1;
            if let Err(e) = run(&case, &dir) {
                failed.push(format!("--- {}: {} (keys {})\n{e}", case.file, case.name, case.keys));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(count > 0, "no cases");
    assert!(failed.is_empty(), "{} of {count} cases failed:\n{}", failed.len(), failed.join("\n"));
}

#[test]
fn cases_parse_and_cursors_mark() {
    let src = "# a comment\n=== one\ntext:\nab|c\nkeys: x\nwant:\nab|\nmode: vim/normal\n=== two\ntext:\n|x\ny\nkeys: dd\nwant:\n|y\n";
    let cases = parse("t.cases", src);
    assert_eq!(cases.len(), 2);
    let with = parse("t.cases", "=== c\ntext:\n|x\ncopy: line a\\nb\nkeys: p\nwant:\nx\n|a\nclipboard: c\\n\n");
    assert_eq!((with[0].text.as_str(), with[0].want.as_str()), ("|x\n", "x\n|a\n"));
    assert_eq!(with[0].copies, [("line".to_owned(), "a\nb".to_owned())]);
    assert_eq!(with[0].clipboard.as_deref(), Some("c\n"));
    assert_eq!(
        (cases[0].text.as_str(), cases[0].want.as_str(), cases[0].mode.as_deref()),
        ("ab|c\n", "ab|\n", Some("vim/normal"))
    );
    assert_eq!((cases[1].text.as_str(), cases[1].keys.as_str(), cases[1].want.as_str()), ("|x\ny\n", "dd", "|y\n"));
    assert_eq!(split_cursor("ab\nc|d\n"), ("ab\ncd\n".to_owned(), (1, 1)));
    assert_eq!(mark("ab\ncd\n", (1, 1)), "ab\nc|d\n");
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn recording_a_macro_hooks_keys_only_while_it_records() {
    let dir = std::env::temp_dir().join(format!("temzdf-vim-rec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut s, rx) = vim_session(&dir, "x\n").unwrap();
    assert!(!s.script.hooks_on("KeyTyped"));
    type_keys(&mut s, &rx, "qa", None).unwrap();
    assert!(s.script.hooks_on("KeyTyped"), "recording");
    type_keys(&mut s, &rx, "lq", None).unwrap();
    assert!(!s.script.hooks_on("KeyTyped"), "keys are not held after it");
    let _ = std::fs::remove_dir_all(&dir);
}
