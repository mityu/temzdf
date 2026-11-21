//! Sessions with the real script host (the WebAssembly guest, in process):
//! the helpers the tests of scripts share (vim_tests.rs uses them too), and
//! the shipped keys (crates/temzdf-script/runtime/, the defaults only)
//! typed into terminal, PDF and Markdown panes, checked by the state they
//! leave. Commands alone are tested with `run_command`, the matcher with
//! tests' mappings of commands (session.rs); see "Server tests" in
//! doc/dev/testing.md.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use temzdf_protocol::script::ScriptValue;
use termpix_core::geom::{CellSize, Geometry};

use super::*;
use crate::keymap::notation;

pub(super) const GEOMETRY: Geometry = Geometry { cols: 80, rows: 24, cell: CellSize { width: 10, height: 20 } };

/// The error messages shown now (a key dismisses them).
pub(super) fn errors(s: &Session) -> Vec<String> {
    s.notices.iter().filter(|n| n.kind == notices::Kind::Error).map(|n| n.text.clone()).collect()
}

/// Handle events until no script runs (no task either), the first pane
/// is there and nothing is loaded, rendered or painted; with `flush`, a key
/// waiting for a longer mapping times out too (after the last key). The
/// client's clipboard holds `clipboard`.
pub(super) fn settle(s: &mut Session, rx: &Receiver<SessionEvent>, flush: bool, clipboard: Option<&str>) {
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
        // As a client draws: what is in sight is asked for.
        let _ = s.frame();
        if !s.holds_input() && s.script.running() && !s.tasks_running() && !s.is_starting() && s.is_idle() {
            match rx.try_recv() {
                Ok(ev) => {
                    s.handle(ev, Instant::now());
                    continue;
                }
                // A key waiting for a longer mapping: as if its time ran out.
                Err(_) if flush && s.matcher.deadline(s.options.timeoutlen).is_some() => {
                    s.poll(Instant::now() + s.options.timeoutlen + Duration::from_millis(1));
                    continue;
                }
                Err(_) => return,
            }
        }
        assert!(Instant::now() < deadline, "the script host does not answer");
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(20)) {
            s.handle(ev, Instant::now());
        }
    }
}

/// A session showing `spec`, with the configuration file `config` (where
/// it is: its relative imports resolve from there; none: the defaults
/// alone). Splits start `/bin/sh`, not the user's shell.
pub(super) fn host_session(
    spec: PaneSpec,
    config: Option<PathBuf>,
) -> Result<(Session, Receiver<SessionEvent>), String> {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
    let settings = Settings { serve_forward: false, script: Some(ScriptSettings { config }), overrides: Vec::new() };
    let mut s = Session::new("host".into(), GEOMETRY, Hints::default(), settings, sink);
    s.shell = Some(vec!["/bin/sh".into()]);
    s.start(spec, None).map_err(|e| format!("{e:#}"))?;
    settle(&mut s, &rx, true, None);
    Ok((s, rx))
}

/// A session editing `text`, with `config` (none: the defaults alone).
pub(super) fn config_session(
    dir: &Path,
    text: &str,
    config: Option<&str>,
) -> Result<(Session, Receiver<SessionEvent>), String> {
    let config = config.map(|c| {
        let path = dir.join("config.ts");
        std::fs::write(&path, c).unwrap();
        path
    });
    config_file_session(dir, text, config)
}

/// A session editing `text` (in `dir`/case.txt), with the configuration
/// file `config`.
pub(super) fn config_file_session(
    dir: &Path,
    text: &str,
    config: Option<PathBuf>,
) -> Result<(Session, Receiver<SessionEvent>), String> {
    let file = dir.join("case.txt");
    std::fs::write(&file, text).unwrap();
    host_session(PaneSpec::File(file), config)
}

/// Type `keys` one at a time, settling after each; the errors shown.
pub(super) fn type_keys(
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

/// A fresh directory for one test's files (removed when dropped).
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("temzdf-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What a test opens with the shipped defaults.
#[derive(Clone, Copy, Debug)]
enum Open {
    /// A terminal running `sleep`.
    Terminal,
    /// The fixture sample.pdf (three pages).
    Pdf,
    /// A long Markdown file in the viewer's graphics mode.
    MarkdownGraphics,
}

fn open(what: Open, dir: &Path) -> (Session, Receiver<SessionEvent>) {
    let spec = match what {
        Open::Terminal => PaneSpec::Command(vec!["sleep".into(), "30".into()]),
        Open::Pdf => PaneSpec::File(temzdf_pdfcore::pdf::fixture("sample.pdf")),
        Open::MarkdownGraphics => {
            let words = "word ".repeat(30);
            let src: String = (0..20).map(|i| format!("## Section {i}\n\n{words}\n\n")).collect();
            let path = dir.join("long.md");
            std::fs::write(&path, src).unwrap();
            PaneSpec::File(path)
        }
    };
    let path = match &spec {
        PaneSpec::File(p) => Some(p.clone()),
        _ => None,
    };
    let (mut s, rx) = host_session(spec, None).unwrap();
    assert!(errors(&s).is_empty(), "{:?}", errors(&s));
    if let Open::MarkdownGraphics = what {
        let p = ScriptValue::Str(path.unwrap().to_str().unwrap().into());
        s.run_command("markdown.open", &[p], Instant::now()).unwrap();
        s.run_command("markdown.set-mode", &[ScriptValue::Str("graphics".into())], Instant::now()).unwrap();
        settle(&mut s, &rx, true, None);
        assert_eq!(s.current_mode(), "markdown");
    }
    (s, rx)
}

/// Animations (eased scrolling) at their end. (They start at the
/// session's clock, which an earlier call put ahead.)
fn finish_animations(s: &mut Session) {
    s.tick(s.now().max(Instant::now()) + Duration::from_secs(5));
    assert!(!s.is_animating());
}

/// The tiled panes' places and sizes.
fn layout_state(s: &mut Session) -> String {
    let panes = s.layout.as_ref().unwrap().panes();
    format!("{:?}", panes.iter().map(|p| (*p, s.panes[p].rect)).collect::<Vec<_>>())
}

/// Where the focused PDF pane shows its document.
fn pdf_state(s: &mut Session) -> String {
    finish_animations(s);
    format!("{:?}", s.focused_pane().unwrap().viewer.top_position())
}

/// How far the focused Markdown viewer is scrolled.
fn markdown_state(s: &mut Session) -> String {
    finish_animations(s);
    format!("{}", s.md_graphics(s.focus).unwrap().0.scroll())
}

/// A key pressed `n` times does what it does once with the count `n`.
struct CountCase {
    open: Open,
    /// Typed first (the panes a case needs).
    prep: &'static str,
    /// Typed before the presses (the prefix: its mode stays meanwhile).
    before: &'static str,
    key: &'static str,
    n: usize,
    state: fn(&mut Session) -> String,
}

const COUNT_CASES: &[CountCase] = &[
    // runtime/mode/normal.ts
    CountCase { open: Open::Terminal, prep: "<C-b>%", before: "<C-b>", key: ">", n: 3, state: layout_state },
    CountCase { open: Open::Terminal, prep: "<C-b>\"", before: "<C-b>", key: "-", n: 2, state: layout_state },
    CountCase { open: Open::Terminal, prep: "<C-b>%<C-b>%", before: "<C-b>", key: "r", n: 2, state: layout_state },
    // runtime/pane/pdf.ts
    CountCase { open: Open::Pdf, prep: "", before: "", key: "j", n: 3, state: pdf_state },
    CountCase { open: Open::Pdf, prep: "", before: "", key: "<C-f>", n: 2, state: pdf_state },
    // runtime/pane/markdown.ts (the text mode's keys are the Vim mode's:
    // runtime/vim/test/)
    CountCase { open: Open::MarkdownGraphics, prep: "", before: "", key: "j", n: 3, state: markdown_state },
];

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn the_shipped_keys_take_counts_as_presses() {
    let dir = TempDir::new("counts");
    let mut failed = Vec::new();
    for case in COUNT_CASES {
        let presses = format!("{}{}", case.before, case.key.repeat(case.n));
        let counted = format!("{}{}{}", case.before, case.n, case.key);
        let mut states = Vec::new();
        for keys in [presses.as_str(), counted.as_str()] {
            let (mut s, rx) = open(case.open, &dir.0);
            if !case.prep.is_empty() {
                type_keys(&mut s, &rx, case.prep, None).unwrap();
            }
            let start = (case.state)(&mut s);
            let seen = type_keys(&mut s, &rx, keys, None).unwrap();
            let state = (case.state)(&mut s);
            if !seen.is_empty() {
                failed.push(format!("{:?} {keys}: errors {seen:?}", case.open));
            }
            if state == start {
                failed.push(format!("{:?} {keys}: nothing changed ({state})", case.open));
            }
            states.push(state);
        }
        if states[0] != states[1] {
            failed.push(format!("{:?}: {presses} gave {}, {counted} gave {}", case.open, states[0], states[1]));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// While the script host is down a PDF pane's keys do nothing (there is
/// no fallback keymap, decided with the user on 2026-11-22), and the
/// escape keys still work.
#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn with_the_host_down_pdf_keys_do_nothing_but_the_escape_keys_work() {
    let dir = TempDir::new("host-down-pdf");
    let (mut s, rx) = open(Open::Pdf, &dir.0);
    s.run_command("debug-stop-script-host", &[], Instant::now()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while s.script.running() {
        assert!(Instant::now() < deadline, "the host did not stop");
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(20)) {
            s.handle(ev, Instant::now());
        }
    }
    let press = |s: &mut Session, keys: &str| {
        let mut out = Outcome::None;
        for k in notation::parse(keys).unwrap() {
            out = out.merge(s.key(&k.event(), Instant::now()));
        }
        out
    };
    let start = pdf_state(&mut s);
    assert_eq!(press(&mut s, "jjj<C-f>"), Outcome::None);
    assert_eq!(pdf_state(&mut s), start, "the PDF pane did not move");
    assert_eq!(press(&mut s, "<C-b>q"), Outcome::Quit);
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn counts_choose_the_tab_page_and_the_pdf_page() {
    let dir = TempDir::new("count-places");
    // {N}gt: tab page N.
    let (mut s, rx) = open(Open::Terminal, &dir.0);
    for _ in 0..2 {
        let what = ScriptValue::map([(
            "command",
            ScriptValue::List(vec![ScriptValue::Str("sleep".into()), ScriptValue::Str("30".into())]),
        )]);
        s.run_command("new-tab", &[what], Instant::now()).unwrap();
    }
    assert_eq!(s.tab, 2);
    type_keys(&mut s, &rx, "<C-b>2gt", None).unwrap();
    assert_eq!((s.tab, s.current_mode()), (1, "terminal"));
    // {N}G: page N.
    let (mut s, rx) = open(Open::Pdf, &dir.0);
    type_keys(&mut s, &rx, "2G", None).unwrap();
    finish_animations(&mut s);
    assert_eq!(s.focused_pane().unwrap().viewer.current_page(), 1);
    type_keys(&mut s, &rx, "G", None).unwrap();
    finish_animations(&mut s);
    assert_eq!(s.focused_pane().unwrap().viewer.current_page(), 2, "the last page without a count");
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn the_prefix_splits_terminal_and_pdf_panes() {
    let dir = TempDir::new("prefix");
    for (what, mode) in [(Open::Terminal, "terminal"), (Open::Pdf, "pdf")] {
        let (mut s, rx) = open(what, &dir.0);
        assert_eq!(s.current_mode(), mode);
        type_keys(&mut s, &rx, "<C-b>", None).unwrap();
        assert_eq!(s.current_mode(), "normal", "{what:?}");
        let first = s.focus;
        type_keys(&mut s, &rx, "%", None).unwrap();
        assert_eq!(s.pane_count(), 2, "{what:?}");
        assert_ne!(s.focus, first, "{what:?}: the new pane has the focus");
        assert_eq!(s.current_mode(), mode, "{what:?}: the new pane's mode");
    }
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn the_prefix_and_a_colon_run_ex_commands_in_any_pane() {
    let dir = TempDir::new("colon");
    for (what, mode) in [(Open::Pdf, "pdf"), (Open::Terminal, "terminal")] {
        let (mut s, rx) = open(what, &dir.0);
        // The commands on panes run here, back in the pane's mode.
        assert_eq!(type_keys(&mut s, &rx, "<C-b>:vsplit<CR>", None).unwrap(), Vec::<String>::new(), "{what:?}");
        assert_eq!(s.pane_count(), 2, "{what:?}");
        assert_eq!(s.current_mode(), mode, "{what:?}");
        assert_eq!(type_keys(&mut s, &rx, "<C-b>:close | tabnew<CR>", None).unwrap(), Vec::<String>::new(), "{what:?}");
        assert_eq!((s.pane_count(), s.tabs.len()), (2, 2), "{what:?}: one pane on each tab page");
        // Those on text are not commands here.
        let seen = type_keys(&mut s, &rx, "<C-b>:tabprevious | s/a/b/<CR>", None).unwrap();
        assert!(seen.iter().any(|e| e.contains("E492")), "{what:?}: {seen:?}");
        assert_eq!(s.current_mode(), mode, "{what:?}");
    }
    // :e without a file reloads text panes only.
    let (mut s, rx) = open(Open::Pdf, &dir.0);
    let seen = type_keys(&mut s, &rx, "<C-b>:e<CR>", None).unwrap();
    assert!(seen.iter().any(|e| e.contains("E32")), "{seen:?}");
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn the_prefix_twice_reaches_the_program() {
    let dir = TempDir::new("prefix-twice");
    let out = dir.0.join("got");
    let script = format!("stty -echo; read l; printf %s \"$l\" | od -An -tx1 | tr -d ' \\n' > '{}'", out.display());
    let spec = PaneSpec::Command(vec!["sh".into(), "-c".into(), script]);
    let (mut s, rx) = host_session(spec, None).unwrap();
    type_keys(&mut s, &rx, "<C-b><C-b>a<CR>", None).unwrap();
    assert_eq!(s.current_mode(), "terminal");
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::read_to_string(&out).map_or(true, |t| t.is_empty()) {
        assert!(Instant::now() < deadline, "the program read nothing");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "0261", "C-b (0x02), then a");
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn the_normal_mode_stays_for_sizes_and_goes_with_the_focus() {
    let dir = TempDir::new("normal-mode");
    let (mut s, rx) = open(Open::Terminal, &dir.0);
    type_keys(&mut s, &rx, "<C-b>%<C-b>", None).unwrap();
    // Sizes repeat (Vim's <C-w>+ - < > = | _).
    for k in ["=", "<Bar>", "_", ">", "<lt>", "+", "-"] {
        type_keys(&mut s, &rx, k, None).unwrap();
        assert_eq!(s.current_mode(), "normal", "after {k}");
    }
    type_keys(&mut s, &rx, "<Esc>", None).unwrap();
    assert_eq!(s.current_mode(), "terminal");
    // A floating shell: its own mode; tiled again: back to the pane's mode.
    type_keys(&mut s, &rx, "<C-b>W", None).unwrap();
    let f = s.focus;
    assert!(s.is_float(f));
    assert_eq!(s.current_mode(), "terminal", "in its own mode");
    type_keys(&mut s, &rx, "<C-b>e", None).unwrap();
    assert!(!s.is_float(f));
    assert_eq!(s.current_mode(), "terminal", "back to the pane's mode");
    // A pane out to a tab page of its own; tab pages in turn.
    type_keys(&mut s, &rx, "<C-b>T", None).unwrap();
    assert_eq!((s.tabs.len(), s.tab, s.focus, s.current_mode()), (2, 1, f, "terminal"));
    type_keys(&mut s, &rx, "<C-b>gt", None).unwrap();
    assert_eq!((s.tab, s.current_mode()), (0, "terminal"));
}
