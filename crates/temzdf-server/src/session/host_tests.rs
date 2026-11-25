//! Sessions with the real script host (the WebAssembly guest, in process):
//! the helpers the tests of scripts share (vim_tests.rs uses them too), and
//! the shipped keys (crates/temzdf-script/runtime/, the defaults only)
//! typed into terminal, PDF and Markdown panes, checked by the state they
//! leave. Commands alone are tested with `run_command`, the matcher with
//! tests' mappings of commands (session.rs); see "Server tests" in
//! doc/dev/testing.md.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use temzdf_protocol::script::ScriptValue;
use termpix_core::geom::{CellSize, Geometry};
use termpix_core::input::{CoordUnit, Modifiers, MouseEvent, MouseEventKind, ScrollDirection, ScrollPhase, ScrollUnit};

use super::tests::helper;
use super::*;
use crate::keymap::notation;
use temzdf_scene::sink::channel_sink;

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
/// alone). Splits start the tests' shell (`tests::test_shell`), not the
/// user's.
pub(super) fn host_session(
    spec: PaneSpec,
    config: Option<PathBuf>,
) -> Result<(Session, Receiver<SessionEvent>), String> {
    let (tx, rx) = mpsc::channel();
    let sink: EventSink = channel_sink(tx, |e| e);
    let settings = Settings { serve_forward: false, script: Some(ScriptSettings { config }) };
    let mut s = Session::new("host".into(), GEOMETRY, Hints::default(), settings, sink);
    s.shell = Some(super::tests::test_shell());
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
fn config_file_session(
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
        Open::Terminal => PaneSpec::Command(helper(&["sleep", "30000"]).into_iter().map(String::from).collect()),
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
#[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
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
#[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
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

/// `q` closes a PDF pane and a Markdown viewer's graphics mode alike (in a
/// split beside a terminal), with no error shown.
#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
#[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
fn q_closes_pdf_and_markdown_graphics_panes() {
    let dir = TempDir::new("q-closes");
    for what in [Open::Pdf, Open::MarkdownGraphics] {
        let (mut s, rx) = open(what, &dir.0);
        // A second pane keeps the session alive to look at.
        type_keys(&mut s, &rx, "<C-b>%<C-b>h", None).unwrap();
        let before = s.panes.len();
        let mode = s.current_mode().to_owned();
        let errors = type_keys(&mut s, &rx, "q", None).unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(s.panes.len(), before - 1, "the pane is closed (mode {mode})");
    }
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
#[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
fn counts_choose_the_tab_page_and_the_pdf_page() {
    let dir = TempDir::new("count-places");
    // {N}gt: tab page N.
    let (mut s, rx) = open(Open::Terminal, &dir.0);
    for _ in 0..2 {
        let what = ScriptValue::map([(
            "command",
            ScriptValue::List(helper(&["sleep", "30000"]).into_iter().map(|a| ScriptValue::Str(a.into())).collect()),
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
#[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
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
#[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
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
#[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
fn preview_opens_a_pdf_split_off_to_the_right() {
    // From a terminal: the split shows the PDF, and no second shell starts.
    let dir = TempDir::new("preview");
    let (mut s, rx) = open(Open::Terminal, &dir.0);
    let first = s.focus;
    let pdf = temzdf_pdfcore::pdf::fixture("sample.pdf");
    let keys = format!("<C-b>:preview {}<CR>", pdf.display());
    assert_eq!(type_keys(&mut s, &rx, &keys, None).unwrap(), Vec::<String>::new());
    settle(&mut s, &rx, true, None);
    assert_eq!(s.pane_count(), 2);
    assert_ne!(s.focus, first, "the new pane has the focus");
    assert!(matches!(s.panes[&s.focus].kind, Kind::Pdf { .. }), "a PDF pane");
    assert_eq!(s.current_mode(), "pdf");
    assert_eq!(s.terms.len(), 1, "the one shell");
    let (left, right) = (s.panes[&first].rect, s.panes[&s.focus].rect);
    assert!(right.x > left.x && right.y == left.y, "to the right: {left:?} {right:?}");
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
#[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
fn inverse_search_goes_to_the_synctex_inverse_hooks() {
    let dir = TempDir::new("inverse");
    let (mut s, rx) = open(Open::Pdf, &dir.0);
    let pane = s.focus;
    // A Ctrl+click on the title (page 1).
    let click = |s: &mut Session| {
        let press = MouseEvent {
            kind: MouseEventKind::Press(termpix_core::input::MouseButton::Left),
            x: 300,
            y: 167,
            unit: CoordUnit::Pixel,
            modifiers: Modifiers::CTRL,
        };
        s.mouse(&press, Instant::now());
        settle(s, &rx, true, None);
    };
    // Nothing hooks it: the pane says where it is.
    click(&mut s);
    let said = pdf_message(&mut s, pane);
    assert!(said.contains("sample.tex:") && said.contains("SynctexInverse"), "{said:?}");
    // A hook gets the pane and the location, and may act itself.
    // (`on` under the eval's task: `atomic` lends it the task's context.)
    let hook = r#"temzdf.atomic(() => temzdf.on("SynctexInverse", (pane, loc) => {
        globalThis.got = { pane, file: loc.file, line: loc.line, column: loc.column ?? null };
        // Changing something (not only a message): the configuration's
        // `action` (the eval's `temzdf.action` is its task's).
        globalThis.__temzdf.action.setVar("pane", "inverse", loc.line);
    })); null"#;
    assert_eq!(eval(&mut s, &rx, hook), Ok(Some("null".into())));
    click(&mut s);
    let got = eval(&mut s, &rx, "JSON.stringify(globalThis.got ?? null)").unwrap().unwrap();
    let got: serde_json::Value = serde_json::from_str(&serde_json::from_str::<String>(&got).unwrap()).unwrap();
    assert_eq!(got["pane"], pane as u64, "{got}");
    assert!(got["file"].as_str().unwrap().ends_with("sample.tex"), "{got}");
    assert!(got["line"].as_u64().unwrap() >= 1, "{got}");
    assert!(got["column"].is_null(), "TeX recorded no column: {got}");
    let args = [ScriptValue::Str("pane".into()), ScriptValue::Str("inverse".into())];
    let var = s.run_command("get-var", &args, Instant::now()).unwrap().0;
    assert_eq!(var, ScriptValue::Num(got["line"].as_f64().unwrap()), "the hook acted");
}

/// What PDF pane `pane` says (its message), empty when nothing.
fn pdf_message(s: &mut Session, pane: PaneId) -> String {
    match &s.panes[&pane].kind {
        Kind::Pdf { pane, .. } => pane.message.clone().unwrap_or_default(),
        _ => panic!("not a PDF pane"),
    }
}

/// Run `code` as `temzdf eval -p` does: its value as JSON, or its error.
fn eval(s: &mut Session, rx: &Receiver<SessionEvent>, code: &str) -> Result<Option<String>, String> {
    use temzdf_protocol::script::{Code, CodeStep};
    let step = CodeStep { name: "-c 1".into(), code: code.into() };
    let id = s.start_eval(Code { steps: vec![step], base: PathBuf::new(), print: true })?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(20)) {
            s.handle(ev, Instant::now());
        }
        let _ = s.poll(Instant::now());
        let _ = s.take_effects();
        let _ = s.frame();
        if let Some((_, r)) = s.take_evaluated().into_iter().find(|(i, _)| *i == id) {
            return r.result;
        }
        assert!(Instant::now() < deadline, "{code}: no answer");
    }
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
#[cfg_attr(not(unix), ignore = "the fixtures need TeX (none on Windows yet)")]
fn copy_resolves_with_the_entry_once_pages_never_shown_are_read() {
    let dir = TempDir::new("copy");
    let (mut s, rx) = open(Open::Pdf, &dir.0);
    let copy = "(await temzdf.action.copy())?.text ?? null";
    assert_eq!(eval(&mut s, &rx, copy), Ok(Some("null".into())), "nothing selected");
    // From page 1 to deep into page 3, zoomed in: page 2 is never shown.
    let at = |kind, x, y| MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE };
    let left = termpix_core::input::MouseButton::Left;
    // On the title (a press off the text pans instead), moved a little so
    // that the selection starts.
    s.mouse(&at(MouseEventKind::Press(left), 300, 167), Instant::now());
    s.mouse(&at(MouseEventKind::Drag(left), 330, 167), Instant::now());
    for cmd in ["zoom-in", "zoom-in", "zoom-in", "last-page", "page-down"] {
        s.run_command(cmd, &[], Instant::now()).unwrap();
        finish_animations(&mut s);
        settle(&mut s, &rx, true, None);
    }
    s.mouse(&at(MouseEventKind::Drag(left), 700, 300), Instant::now());
    settle(&mut s, &rx, true, None);
    // Copied while the drag goes on: the Promise waits for page 2's text.
    let json = eval(&mut s, &rx, copy).unwrap().unwrap();
    let text: String = serde_json::from_str(&json).unwrap();
    let notices: Vec<&str> = s.notices.iter().map(|n| n.text.as_str()).collect();
    assert!(notices.contains(&"Copying: reading 1 pages…"), "{notices:?}");
    for part in ["sample document", "Paragraph 1: Lorem ipsum", "Back to Section", "Conclusion"] {
        assert!(text.contains(part), "{part:?} in {text}");
    }
    // It is the copy history's newest entry.
    let newest = match s.run_command("history", &[], Instant::now()).unwrap().0 {
        ScriptValue::List(l) => l[0].get("text").and_then(ScriptValue::as_str).map(str::to_owned),
        v => panic!("{v:?}"),
    };
    assert_eq!(newest.as_deref(), Some(text.as_str()));
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn the_prefix_twice_reaches_the_program() {
    let argv = helper(&["raw", "print", "ready\\r\\n", "read-line", "said", "sleep", "30000"]);
    let (mut s, rx) = host_session(PaneSpec::Command(argv.into_iter().map(String::from).collect()), None).unwrap();
    let rows = |s: &mut Session| super::tests::screen_text(&s.frame(), GEOMETRY);
    let wait = |s: &mut Session, what: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !rows(s).iter().any(|l| l.starts_with(what)) {
            assert!(Instant::now() < deadline, "no {what}: {:?}", rows(s));
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(20)) {
                s.handle(ev, Instant::now());
            }
        }
    };
    wait(&mut s, "ready");
    type_keys(&mut s, &rx, "<C-b><C-b>a<CR>", None).unwrap();
    assert_eq!(s.current_mode(), "terminal");
    wait(&mut s, "said:");
    let shown = rows(&mut s);
    assert!(shown.iter().any(|l| l == "said:\\x02a"), "C-b (0x02), then a: {shown:?}");
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

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn conditions_may_read_and_hooks_may_also_show_messages() {
    // Conditions (when) may call every command that only reads; message
    // shows something, which only hooks may (session/script.rs: read_only,
    // hook_may).
    let config = r#"import { action, keymap, on } from "temzdf";
const tried = (f: () => unknown) => { try { f(); return "ok"; } catch (e) { return (e as Error).message; } };
let cond = "";
let hook = "";
keymap.add("vim/normal", "<F6>", () => { action.setVar("pane", "cond", cond); action.setVar("pane", "hook", hook); }, {
  when: () => {
    cond = [tried(() => action.buffers()), tried(() => action.text.line(0)), tried(() => action.message("c"))].join(" | ");
    return true;
  },
});
on("ModeChanged", () => { hook = [tried(() => action.buffers()), tried(() => action.message("h"))].join(" | "); });
keymap.add("vim/normal", "<F7>", () => action.switchMode("normal"));
"#;
    let dir = TempDir::new("cond-reads");
    let (mut s, rx) = config_session(&dir.0, "x\n", Some(config)).unwrap();
    type_keys(&mut s, &rx, "<F7><Esc><F6>", None).unwrap();
    let var = |s: &mut Session, name: &str| {
        let args = [ScriptValue::Str("pane".into()), ScriptValue::Str(name.into())];
        s.run_command("get-var", &args, Instant::now()).unwrap().0
    };
    assert_eq!(
        var(&mut s, "cond"),
        ScriptValue::Str("ok | ok | a condition (when, keepalive) cannot change anything; it may only read".into())
    );
    assert_eq!(var(&mut s, "hook"), ScriptValue::Str("ok | ok".into()));
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn the_mouse_wheel_is_heard_as_pane_scrolled() {
    // Script tests cannot turn the wheel (runtime/scrolled.test.ts has the
    // rest): a notch scrolls three rows, heard once; a touchpad's fraction
    // of a row scrolls nothing and is not heard.
    let config = r#"import { action, keymap, on } from "temzdf";
const heard: string[] = [];
on("PaneScrolled", (pane, i) => { heard.push(`${pane}:${i.first}-${i.last}<${i.previous?.first}-${i.previous?.last}`); });
keymap.add("vim/normal", "<F6>", () => { action.setVar("pane", "heard", heard.join(" ")); heard.length = 0; });
keymap.add("vim/normal", "<F7>", () => action.text.scroll(1));
"#;
    let dir = TempDir::new("pane-scrolled");
    let text: String = (0..100).map(|i| format!("{i}\n")).collect();
    let (mut s, rx) = config_session(&dir.0, &text, Some(config)).unwrap();
    let pane = s.focus;
    let rows = s.panes[&pane].content().h as usize;
    let heard = |s: &mut Session| {
        // As the server's loop polls after each event.
        let _ = s.poll(Instant::now());
        type_keys(s, &rx, "<F6>", None).unwrap();
        let args = [ScriptValue::Str("pane".into()), ScriptValue::Str("heard".into())];
        s.run_command("get-var", &args, Instant::now()).unwrap().0
    };
    let last = rows - 1;
    assert_eq!(heard(&mut s), ScriptValue::Str(format!("{pane}:0-{last}<undefined-undefined")), "the first pane");
    let wheel = |s: &mut Session, kind| {
        s.mouse(
            &MouseEvent { kind, x: 100, y: 100, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE },
            Instant::now(),
        );
    };
    wheel(&mut s, MouseEventKind::Scroll(ScrollDirection::Down));
    assert_eq!(heard(&mut s), ScriptValue::Str(format!("{pane}:3-{}<0-{last}", last + 3)));
    let kind = MouseEventKind::ScrollBy { dx: 0, dy: 5, unit: ScrollUnit::Pixel, phase: ScrollPhase::Touch };
    wheel(&mut s, kind);
    assert_eq!(heard(&mut s), ScriptValue::Str(String::new()), "a fraction of a row");
    // A key typed while a mapping runs waits; the mapping's scrolling is
    // heard before it (at the safe point), not together with its own.
    let f7 = notation::parse("<F7>").unwrap()[0].event();
    s.key(&f7, Instant::now());
    s.key(&f7, Instant::now());
    settle(&mut s, &rx, true, None);
    assert_eq!(
        heard(&mut s),
        ScriptValue::Str(format!("{pane}:4-{}<3-{} {pane}:5-{}<4-{}", last + 4, last + 3, last + 5, last + 4))
    );
}

/// Press `keys` without waiting for anything in between.
fn press(s: &mut Session, keys: &str) {
    for k in notation::parse(keys).unwrap() {
        s.key(&k.event(), Instant::now());
    }
}

/// `waitResolved`'s configuration: <F6> waits, <F9> waits until <C-c>;
/// what ran, in order, is in the pane variable `log`.
const WAITING: &str = r#"import { action, dispatch, keymap, waitResolved } from "temzdf";
import type { Actions } from "temzdf";
const log: string[] = [];
const note = (a: Actions, what: string) => { log.push(what); a.setVar("pane", "log", log.join(" ")); };
keymap.add("vim/normal", "<F6>", () => waitResolved(async (ctx) => {
  dispatch(async (other) => { await new Promise((r) => setTimeout(r, 10)); note(other.action, "other"); });
  await new Promise((r) => setTimeout(r, 200));
  ctx.action.feedkeys("<F8>");
  note(ctx.action, "waited");
}));
keymap.add("vim/normal", "<F7>", () => note(action, "typed"));
keymap.add("vim/normal", "<F8>", () => note(action, "fed"));
keymap.add("vim/normal", "<F9>", () => waitResolved(async (ctx) => {
  // (<C-c> may come before this runs.)
  if (!ctx.signal.aborted) await new Promise((r) => ctx.signal.addEventListener("abort", () => r(null)));
  let refused = "no";
  try { ctx.action.message("after"); } catch { refused = "yes"; }
  dispatch((c) => note(c.action, `aborted refused=${refused}`));
}));
"#;

fn pane_log(s: &mut Session) -> ScriptValue {
    let args = [ScriptValue::Str("pane".into()), ScriptValue::Str("log".into())];
    s.run_command("get-var", &args, Instant::now()).unwrap().0
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn keys_typed_while_a_task_is_waited_for_come_after_it() {
    let dir = TempDir::new("wait-resolved");
    let (mut s, rx) = config_session(&dir.0, "x\n", Some(WAITING)).unwrap();
    // Typed at once: held until the task is done, though other tasks go
    // on meanwhile; the keys it fed come first.
    press(&mut s, "<F6><F7><F7>");
    settle(&mut s, &rx, true, None);
    assert_eq!(pane_log(&mut s), ScriptValue::Str("other waited fed typed typed".into()));
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn ctrl_c_gives_up_the_task_the_input_waits_for_and_its_typeahead() {
    let dir = TempDir::new("wait-interrupted");
    let (mut s, rx) = config_session(&dir.0, "x\n", Some(WAITING)).unwrap();
    press(&mut s, "<F9><F7>");
    // Until the task waits and the call of <F9> is done.
    let deadline = Instant::now() + Duration::from_secs(20);
    while s.script.waiting.is_empty() || s.script.busy() {
        assert!(Instant::now() < deadline, "the task never waited");
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(20)) {
            s.handle(ev, Instant::now());
        }
    }
    press(&mut s, "<C-c>");
    assert!(errors(&s).contains(&"Interrupted".to_owned()), "{:?}", errors(&s));
    assert!(!s.holds_input(), "the input goes on at once");
    press(&mut s, "<F7>");
    settle(&mut s, &rx, true, None);
    // The <F7> typed before <C-c> was dropped; its signal aborted, the
    // task's commands are refused.
    assert_eq!(pane_log(&mut s), ScriptValue::Str("aborted refused=yes typed".into()));
}

#[test]
#[cfg_attr(miri, ignore = "the script host (WebAssembly)")]
fn hooks_go_after_one_run_or_when_their_signal_is_aborted() {
    let config = r#"import { action, keymap, on, waitResolved } from "temzdf";
const n = { once: 0, signalled: 0, early: 0, waited: 0 };
on("ModeChanged", () => { n.once++; }, { once: true });
const c = new AbortController();
on("ModeChanged", () => { n.signalled++; }, { signal: c.signal });
const aborted = new AbortController();
aborted.abort();
on("ModeChanged", () => { n.early++; }, { signal: aborted.signal });
// Two changes in one mapping: two events.
keymap.add("vim/normal", "<F6>", () => { action.switchMode("normal"); action.switchMode("vim/normal"); });
keymap.add("vim/normal", "<F7>", () => c.abort());
keymap.add("vim/normal", "<F8>", () => waitResolved(async (ctx) => {
  ctx.atomic(() => on("ModeChanged", () => { n.waited++; }, { signal: ctx.signal }));
  ctx.action.switchMode("normal");
  ctx.action.switchMode("vim/normal");
  ctx.action.setVar("pane", "ready", true);
  await new Promise((r) => ctx.signal.addEventListener("abort", () => r(null)));
}));
keymap.add("vim/normal", "<F5>", () => action.setVar("pane", "n", `${n.once} ${n.signalled} ${n.early} ${n.waited}`));
"#;
    let dir = TempDir::new("hook-options");
    let (mut s, rx) = config_session(&dir.0, "x\n", Some(config)).unwrap();
    let n = |s: &mut Session| {
        let args = [ScriptValue::Str("pane".into()), ScriptValue::Str("n".into())];
        s.run_command("get-var", &args, Instant::now()).unwrap().0
    };
    // The first pane's mode at startup, then the two of <F6>.
    type_keys(&mut s, &rx, "<F6><F5>", None).unwrap();
    assert_eq!(n(&mut s), ScriptValue::Str("1 3 0 0".into()), "once ran once, of three");
    press(&mut s, "<F8>");
    let deadline = Instant::now() + Duration::from_secs(20);
    let ready = |s: &mut Session| {
        let args = [ScriptValue::Str("pane".into()), ScriptValue::Str("ready".into())];
        s.run_command("get-var", &args, Instant::now()).unwrap().0 == ScriptValue::Bool(true)
    };
    while !ready(&mut s) || s.script.busy() {
        assert!(Instant::now() < deadline, "the task never waited");
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(20)) {
            s.handle(ev, Instant::now());
        }
    }
    // The task's signal is aborted where no command works (waitResolved's
    // <C-c>); c's in a mapping.
    press(&mut s, "<C-c>");
    type_keys(&mut s, &rx, "<F7>", None).unwrap();
    let before = s.script.hook_count("ModeChanged");
    type_keys(&mut s, &rx, "<F6><F5>", None).unwrap();
    // The task's two changes reached c's hook too, none came after.
    assert_eq!(n(&mut s), ScriptValue::Str("1 5 0 2".into()), "none ran after its signal was aborted");
    // The one aborted where no command worked (c's went at once) took
    // itself off as the event came.
    assert_eq!(s.script.hook_count("ModeChanged"), before - 1);
}
