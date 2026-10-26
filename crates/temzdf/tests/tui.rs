//! temzdf end to end behind a simulated terminal (`tuitest`: Ghostty's
//! terminal emulation, kitty graphics included): sessions, terminal panes,
//! PDF panes, and the session commands. Each test has its own runtime,
//! config, state and log directories, so it never sees the user's sessions.
//! See doc/testing.md.

use std::path::PathBuf;
use std::process::Command;

use tuitest::{Options, Screen, TempDir, Term};

const BIN: &str = env!("CARGO_BIN_EXE_temzdf");

/// A test's private environment.
struct Env {
    dir: TempDir,
}

impl Env {
    fn new(label: &str) -> Self {
        let dir = TempDir::new(label);
        for d in ["run", "config", "state", "log"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        Self { dir }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.dir.path().join(sub)
    }

    fn vars(&self) -> Vec<(String, String)> {
        let d = |s: &str| self.path(s).display().to_string();
        vec![
            ("XDG_RUNTIME_DIR".into(), d("run")),
            ("XDG_CONFIG_HOME".into(), d("config")),
            ("XDG_STATE_HOME".into(), d("state")),
            ("TEMZDF_LOG_DIR".into(), d("log")),
            ("SHELL".into(), "/bin/sh".into()),
            ("PS1".into(), "$ ".into()),
            // POSIX sh reads this file at startup: no user rc files.
            ("ENV".into(), "/dev/null".into()),
        ]
    }

    fn options(&self) -> Options {
        Options { env: self.vars(), ..Options::default() }
    }

    /// Options whose shell is `script` (run with sh) instead of an interactive sh.
    fn with_shell_script(&self, name: &str, script: &str) -> Options {
        let path = self.path(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut o = self.options();
        o.env.retain(|(k, _)| k != "SHELL");
        o.env.push(("SHELL".into(), path.display().to_string()));
        o
    }

    /// Write the configuration script (`config.ts` or `config.js`).
    fn config(&self, name: &str, text: &str) {
        let dir = self.path("config/temzdf");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), text).unwrap();
    }

    /// The session server's log (where scripts' `console.log` goes).
    fn log(&self, session: &str) -> String {
        std::fs::read_to_string(self.path(&format!("log/{session}.log"))).unwrap_or_default()
    }

    /// Wait until the session's log satisfies `done`; returns the log.
    fn wait_log(&self, session: &str, what: &str, done: impl Fn(&str) -> bool) -> String {
        let deadline = std::time::Instant::now() + tuitest::TIMEOUT;
        loop {
            let log = self.log(session);
            if done(&log) {
                return log;
            }
            assert!(std::time::Instant::now() < deadline, "waiting for {what} in the log:\n{log}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Script output lines (`console.log`) in the session's log.
    fn script_lines(&self, session: &str) -> Vec<String> {
        self.log(session).lines().filter_map(|l| l.strip_prefix("temzdf script [log]: ")).map(str::to_owned).collect()
    }

    /// Run a temzdf command (not attached to a terminal) and return its output.
    fn run(&self, args: &[&str]) -> String {
        let out = Command::new(BIN).args(args).envs(self.vars()).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "temzdf {args:?} failed: {text}");
        text
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // End whatever server a failed test left running.
        let _ = Command::new(BIN).arg("kill-all-sessions").envs(self.vars()).output();
    }
}

fn fixture(name: &str) -> PathBuf {
    temzdf_pdfcore::pdf::fixture(name)
}

/// A shell prompt (`$ `, or `sh-5.3$ ` where PS1 does not get through).
fn is_prompt(line: &str) -> bool {
    // A floating message may cover the rest of the line.
    line.trim_end().ends_with('$') || line.starts_with("$ ")
}

fn prompt(s: &Screen) -> bool {
    s.lines().iter().any(|l| is_prompt(l))
}

#[test]
fn a_shell_session_runs_commands_and_ends_when_the_shell_exits() {
    let env = Env::new("shell");
    let mut t = Term::spawn(&[BIN, "-s", "t"], &env.options());
    t.wait_for("the prompt", prompt);
    t.type_text("echo hi-$((6*7))\n");
    t.wait_text("hi-42");
    let s = t.screen();
    assert!(s.cursor.is_some(), "the shell's cursor is shown");
    t.type_text("exit\n");
    t.wait_text("[session t ended]");
    assert_eq!(t.wait_exit(), 0);
}

#[test]
fn ctrl_keys_reach_programs_in_both_keyboard_modes() {
    let env = Env::new("keys");
    // The pane's program shows the bytes it gets: first in legacy mode, then
    // after turning the kitty keyboard protocol on (as fish 4 and neovim do).
    let script = "stty raw -echo; printf 'legacy\\r\\n'; head -c 1 | od -An -c; printf '\\033[>1u'; \
                  printf 'kitty\\r\\n'; head -c 8 | od -An -c; sleep 30";
    let mut t = Term::spawn(&[BIN, "-s", "k"], &env.with_shell_script("keys.sh", script));
    t.wait_text("legacy");
    t.key("C-h");
    t.wait_text("kitty");
    t.key("C-h");
    let s = t.wait_for("the kitty encoding", |s| s.contains("u"));
    let text = s.text();
    assert!(text.contains("\\b"), "legacy C-h is 0x08:\n{text}");
    assert!(text.contains("033   [   1   0   4   ;   5   u"), "kitty C-h is CSI 104;5u:\n{text}");
}

#[test]
fn detaching_and_attaching_again_keeps_the_shell() {
    let env = Env::new("detach");
    let mut t = Term::spawn(&[BIN, "-s", "d"], &env.options());
    t.wait_for("the prompt", prompt);
    t.type_text("echo marker-1\n");
    t.wait_text("marker-1");
    t.keys(&["C-b", "d"]);
    t.wait_text("[detached from session d]");
    assert_eq!(t.wait_exit(), 0);

    let ls = env.run(&["ls"]);
    assert!(ls.contains("d  (1 panes, 1 buffers, detached)"), "{ls}");
    assert!(!ls.contains("log:"), "the log only with -v: {ls}");
    let ls = env.run(&["ls", "-v"]);
    assert!(ls.contains(&format!("log: {}", env.path("log/d.log").display())), "{ls}");

    let mut t = Term::spawn(&[BIN, "attach", "d"], &env.options());
    t.wait_text("marker-1");
    env.run(&["kill-session", "d"]);
    t.wait_text("[session d was killed]");
    assert_eq!(t.wait_exit(), 0);
}

#[test]
fn splitting_a_terminal_and_opening_a_pdf_beside_it() {
    let env = Env::new("split");
    let mut t = Term::spawn(&[BIN, "-s", "p"], &env.options());
    t.wait_for("the prompt", prompt);
    t.type_text("echo left-pane\n");
    t.wait_text("left-pane");
    // A new shell on the right half (100 columns: 49 + border + 50).
    t.keys(&["C-b", "%"]);
    t.wait_for("a prompt in the right pane", |s| is_prompt(&s.row_from(0, 50)));
    t.type_text("echo right-$((1+1))\n");
    let s = t.wait_for("output in the right pane", |s| s.row_from(1, 50) == "right-2");
    let left: String = s.cells[1][..49].iter().map(|c| if c.text.is_empty() { " " } else { c.text.as_str() }).collect();
    assert_eq!(left.trim_end(), "left-pane", "the left pane keeps its text, and nothing spills into it");

    // A PDF opened from another process goes beside the focused (right) pane.
    let pdf = fixture("sample.pdf");
    env.run(&["action", "-s", "p", "new-pane", &pdf.display().to_string()]);
    let s = t.wait_for("pages of the PDF", |s| s.images.iter().any(|p| p.col >= 75));
    // Elsewhere only the one-cell gaps and borders between the panes.
    let stray: Vec<_> = s.images.iter().filter(|p| p.col < 75 && p.cols > 1).collect();
    assert!(stray.is_empty(), "the PDF is in the rightmost pane: {stray:?}");
    let ls = env.run(&["ls"]);
    assert!(ls.contains("p  (3 panes, 3 buffers, 1 attached)") && ls.contains("sample.pdf"), "{ls}");
}

#[test]
fn the_shell_follows_the_window_size() {
    let env = Env::new("resize");
    let mut t = Term::spawn(&[BIN, "-s", "r"], &env.options());
    t.wait_for("the prompt", prompt);
    t.type_text("stty size\n");
    t.wait_text("30 100");
    t.resize(120, 40);
    t.type_text("clear; stty size\n");
    t.wait_text("40 120");
}

#[test]
fn typed_keys_echo_at_once_with_composited_frames() {
    // Composited frames sent inline over SSH, whose rate is capped (200 ms;
    // WezTerm on Linux sends inline too, locally). The cap held text too:
    // every key showed up to 200 ms late.
    let env = Env::new("echo");
    let mut opts = env.options();
    opts.env.push(("TERMPIX_BACKEND".into(), "composited".into()));
    opts.env.push(("TERMPIX_MEDIUM".into(), "direct".into()));
    opts.env.push(("SSH_CONNECTION".into(), "192.0.2.1 50000 192.0.2.2 22".into()));
    let mut t = Term::spawn(&[BIN, "-s", "e"], &opts);
    t.wait_for("the prompt", prompt);
    let mut typed = String::from("$ ");
    let mut times = Vec::new();
    for c in "abcdefghij".chars() {
        typed.push(c);
        let start = std::time::Instant::now();
        t.key(&c.to_string());
        while !t.screen().lines().iter().any(|l| l.starts_with(&typed)) {
            assert!(start.elapsed() < tuitest::TIMEOUT, "{c:?} never echoed");
            t.pump(std::time::Duration::from_millis(1));
        }
        times.push(start.elapsed());
    }
    times.sort();
    assert!(times[times.len() / 2] < std::time::Duration::from_millis(100), "echo times: {times:?}");
}

#[test]
fn typed_keys_echo_at_once_while_a_pdf_scrolls() {
    // Composited frames sent inline over SSH, as above, and a PDF beside the
    // shell scrolled by the wheel: its frames send pixels and are capped; the
    // shell's text must not wait for them.
    let env = Env::new("echo-scroll");
    let mut opts = env.options();
    opts.env.push(("TERMPIX_BACKEND".into(), "composited".into()));
    opts.env.push(("TERMPIX_MEDIUM".into(), "direct".into()));
    opts.env.push(("SSH_CONNECTION".into(), "192.0.2.1 50000 192.0.2.2 22".into()));
    let mut t = Term::spawn(&[BIN, "-s", "e"], &opts);
    t.wait_for("the prompt", prompt);
    env.run(&["action", "-s", "e", "new-pane", &fixture("sample.pdf").display().to_string()]);
    // One composited image covers the screen: see the split in the shell.
    let deadline = std::time::Instant::now() + tuitest::TIMEOUT;
    while !env.run(&["ls"]).contains("2 panes") {
        assert!(std::time::Instant::now() < deadline, "the PDF pane never opened");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    t.click(10, 10);
    t.type_text("clear; stty size\n");
    t.wait_text("30 49");
    t.settle(std::time::Duration::from_millis(500));
    t.type_text("clear\n");
    t.wait_for("a clear screen", |s| s.lines().first().is_some_and(|l| l.trim_end() == "$"));
    let mut typed = String::from("$ ");
    let mut times = Vec::new();
    let mut down = true;
    for (i, c) in "abcdefghij".chars().enumerate() {
        typed.push(c);
        let start = std::time::Instant::now();
        t.key(&c.to_string());
        while !t.screen().lines().iter().any(|l| l.starts_with(&typed)) {
            assert!(start.elapsed() < tuitest::TIMEOUT, "{c:?} never echoed");
            t.wheel(75, 10, down);
            t.pump(std::time::Duration::from_millis(5));
        }
        times.push(start.elapsed());
        if i % 3 == 2 {
            down = !down;
        }
        // Keep scrolling between keys.
        for _ in 0..4 {
            t.wheel(75, 10, down);
            t.pump(std::time::Duration::from_millis(10));
        }
    }
    times.sort();
    assert!(times[times.len() / 2] < std::time::Duration::from_millis(100), "echo times: {times:?}");
}

#[test]
fn a_pdf_shows_its_pages() {
    let env = Env::new("pdf");
    let pdf = fixture("sample.pdf");
    let mut t = Term::spawn(&[BIN, &pdf.display().to_string()], &env.options());
    let s = t.wait_for("pages", |s| !s.images.is_empty());
    assert!(s.images.iter().all(|p| p.col >= 0 && p.row >= 0), "{:?}", s.images);
    t.key("q");
    assert_eq!(t.wait_exit(), 0);
}

#[test]
fn inside_tmux() {
    if !tuitest::have_tmux() {
        eprintln!("SKIPPED: tmux not found");
        return;
    }
    let env = Env::new("tmux");
    let mut t = Term::inside_tmux(&["env", &format!("SHELL={}", "/bin/sh"), BIN, "-s", "x"], &env.options());
    t.wait_for("the prompt", prompt);
    t.type_text("echo in-$((2*3))\n");
    t.wait_text("in-6");
    assert!(t.capture_pane().contains("in-6"), "tmux sees the same:\n{}", t.capture_pane());
    // The prefix reaches temzdf through tmux.
    t.keys(&["C-b", "%"]);
    t.wait_for("a second pane", |s| is_prompt(&s.row_from(0, 50)));
    let pdf = fixture("sample.pdf");
    env.run(&["action", "-s", "x", "new-pane", &pdf.display().to_string()]);
    // tmux shows images only through Unicode placeholder cells.
    let s = t.wait_for("pages (through Unicode placeholders)", |s| s.placeholder_cells().iter().any(|(x, _)| *x >= 76));
    assert!(s.images.is_empty(), "no direct placements inside tmux: {:?}", s.images);
}

#[test]
fn a_mapping_from_a_typescript_configuration() {
    let env = Env::new("script-map");
    env.config(
        "config.ts",
        r#"import { cmd, map } from "temzdf";
const label: string = "mapped";
map("normal", "t", (): void => {
  cmd.split("right");
  cmd.sendText(`echo ${label}-$((40+2))\n`);
});
"#,
    );
    let mut t = Term::spawn(&[BIN, "-s", "m"], &env.options());
    t.wait_for("the prompt", prompt);
    t.keys(&["C-b", "t"]);
    t.wait_for("the mapping's command in the new pane", |s| {
        (0..s.cells.len() as u16).any(|r| s.row_from(r, 50) == "mapped-42")
    });
}

#[test]
fn hooks_run_like_vim_autocmds() {
    let env = Env::new("script-hooks");
    env.config(
        "config.js",
        r#"import { cmd, on } from "temzdf";
on("SessionReady", () => console.log("ready"));
on("PaneOpened", (id, kind) => console.log(`opened ${id} ${kind}`));
on("PaneClosePre", (id) => { console.log(`pre ${id}`); cmd.split("down"); });
on("PaneClosed", (id) => console.log(`closed ${id}`));
"#,
    );
    let mut t = Term::spawn(&[BIN, "-s", "h"], &env.options());
    t.wait_for("the prompt", prompt);
    env.wait_log("h", "ready", |l| l.contains("[log]: ready"));
    t.keys(&["C-b", "%"]);
    // Pane and buffer ids share one counter: the shell's buffer is 2.
    env.wait_log("h", "the second pane", |l| l.contains("opened 3"));
    // Closing it: the "before" hook runs first; the pane its split creates
    // fires no PaneOpened (events inside hooks only reach nested hooks).
    t.keys(&["C-b", "x"]);
    env.wait_log("h", "the close", |l| l.contains("closed 3"));
    let lines = env.script_lines("h");
    assert_eq!(lines, ["opened 1 terminal", "ready", "opened 3 terminal", "pre 3", "closed 3"], "{lines:?}");
    let ls = env.run(&["ls"]);
    assert!(ls.contains("h  (2 panes"), "the hook's split happened: {ls}");
}

#[test]
fn ctrl_c_interrupts_a_runaway_mapping() {
    let env = Env::new("script-interrupt");
    env.config("config.js", "import { map } from \"temzdf\";\nmap(\"normal\", \"l\", () => { for (;;) {} });\n");
    let mut t = Term::spawn(&[BIN, "-s", "i"], &env.options());
    t.wait_for("the prompt", prompt);
    t.keys(&["C-b", "l"]);
    std::thread::sleep(std::time::Duration::from_millis(300));
    // While it runs, input is held; `temzdf action` waits too and times out.
    t.type_text("echo lost\n");
    let out = Command::new(BIN)
        .args(["action", "-s", "i", "--timeout", "1", "focus-next-pane"])
        .envs(env.vars())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("within 1.0 s"), "{}", String::from_utf8_lossy(&out.stderr));
    t.key("C-c");
    env.wait_log("i", "the interrupt", |l| l.contains("script error"));
    // The mapping ran in the normal mode (the prefix enters it); back to the shell's.
    t.key("Esc");
    // Typeahead was dropped with the interrupt (as Vim's CTRL-C); input works again.
    t.type_text("echo after-$((1+1))\n");
    let s = t.wait_text("after-2");
    assert!(!s.contains("lost"), "{}", s.text());
}

#[test]
fn a_broken_configuration_still_starts_the_session() {
    for (label, file, text, expect) in [
        ("tla", "config.js", "await new Promise(() => {});\n", "dispatch"),
        ("enum", "config.ts", "const a = 1;\nenum E { A }\n", "config.ts:2:"),
    ] {
        let env = Env::new(label);
        env.config(file, text);
        let mut t = Term::spawn(&[BIN, "-s", "b"], &env.options());
        t.wait_for("the prompt", prompt);
        let log = env.wait_log("b", "the error", |l| l.contains("configuration:"));
        assert!(log.contains(expect), "{log}");
        t.type_text("echo works\n");
        t.wait_text("works");
    }
}

#[test]
fn dispatched_tasks_act_later_through_their_ctx() {
    let env = Env::new("script-task");
    env.config(
        "config.js",
        r#"import { cmd, dispatch } from "temzdf";
dispatch(async (ctx) => {
  await new Promise((r) => setTimeout(r, 200));
  ctx.cmd.sendText("echo from-task\n");
  try { cmd.sendText("x"); } catch (e) { console.log(`refused: ${e.message.split(":")[0]}`); }
});
"#,
    );
    let mut t = Term::spawn(&[BIN, "-s", "d"], &env.options());
    env.wait_log("d", "the refusal", |l| l.contains("refused: no context"));
    t.wait_text("from-task");
}

#[test]
fn reload_config_registers_again_and_the_session_survives_the_host() {
    let env = Env::new("script-reload");
    env.config(
        "config.js",
        "import { cmd, map } from \"temzdf\";\nconsole.log(\"loaded\");\nmap(\"normal\", \"e\", () => cmd.sendText(\"echo mapped\\n\"));\n",
    );
    let mut t = Term::spawn(&[BIN, "-s", "r"], &env.options());
    t.wait_for("the prompt", prompt);
    env.run(&["action", "-s", "r", "reload-config"]);
    env.wait_log("r", "the second load", |l| l.matches("[log]: loaded").count() == 2);
    t.keys(&["C-b", "e"]);
    t.wait_text("mapped");
    t.key("Esc");
    // Kill the host: the session goes on without scripts.
    let ls = env.run(&["ls", "-v"]);
    let pid = ls.lines().find_map(|l| l.trim().strip_prefix("pid: ")).unwrap().trim().to_owned();
    let killed = Command::new("pkill").args(["-P", &pid, "-f", "script-host"]).status().unwrap();
    assert!(killed.success());
    env.wait_log("r", "the host's exit", |l| l.contains("the script host exited"));
    // The notice floats at the top right with the keys that still work.
    t.wait_text("<C-b>r restarts");
    t.type_text("clear; echo still-$((2+2))\n");
    t.wait_text("still-4");
    // <C-b>r brings the host (and the mappings) back.
    t.keys(&["C-b", "r"]);
    env.wait_log("r", "the third load", |l| l.matches("[log]: loaded").count() == 3);
    t.wait_for("the notice gone", |s| !s.contains("<C-b>r restarts"));
    t.keys(&["C-b", "e"]);
    t.key("Esc");
    t.wait_for("the mapping again", |s| s.lines().iter().any(|l| l.trim() == "mapped"));
}

#[test]
fn a_click_on_another_pane_only_focuses_it() {
    let env = Env::new("click");
    // The right pane's program tracks the mouse and shows what it gets.
    let script = "printf '\\033[?1000h\\033[?1006h'; stty raw -echo; printf 'tracking\\r\\n'; \
                  head -c 12 | od -An -c; sleep 30";
    let mut t = Term::spawn(&[BIN, "-s", "c"], &env.options());
    t.wait_for("the prompt", prompt);
    let words = vec![
        "action".to_owned(),
        "-s".into(),
        "c".into(),
        "new-pane".into(),
        "--".into(),
        "sh".into(),
        "-c".into(),
        script.into(),
    ];
    let out = Command::new(BIN).args(&words).envs(env.vars()).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    t.wait_for("the tracking program", |s| s.row_from(0, 50).starts_with("tracking"));
    // Focus back to the shell, then click the program's pane: only focus.
    t.keys(&["C-b", "h"]);
    t.click(70, 10);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!t.screen().row_from(1, 50).contains('['), "the focusing click reached the program");
    // A second click, now that it has the focus, reaches it.
    t.click(70, 10);
    t.wait_for("the program's view of the click", |s| (1..6).any(|r| s.row_from(r, 50).contains('[')));
}

#[test]
fn modes_mappings_and_options_from_the_configuration() {
    let env = Env::new("script-modes");
    env.config(
        "config.ts",
        r#"import { action, cmd, defineMode, feedkeys, map, o, on, unmap } from "temzdf";
o.timeoutlen = 300;
defineMode("win", { inherit: "normal", unmapped: "drop" });
map("terminal", "<C-x>w", action.switchMode("win"));
map("win", "v", [action.split("right"), action.sendText("echo in-new\n")]);
map("terminal", "<C-x>e", () => cmd.sendText(`echo mode-${cmd.currentMode()}\n`));
map("terminal", "<C-x>f", () => feedkeys("echo fed<CR>"));
unmap("normal", "x");
on("ModeChanged", (a: string, b: string) => console.log(`mode ${a} ${b}`));
try { map("terminal", "<C-c>x", action.exitMode()); } catch (e) { console.log(`refused: ${(e as Error).message}`); }
o.scrollstep = 90;
o.linkopener = ["true"];
o.reloadanchor = "text";
console.log(`options ${o.scrollstep} ${JSON.stringify(o.linkopener)} ${o.reloadanchor} ${o.linkpreviewdelay}`);
try { (o as any).nope = 1; } catch (e) { console.log(`option refused: ${(e as Error).message}`); }
"#,
    );
    let mut t = Term::spawn(&[BIN, "-s", "md"], &env.options());
    t.wait_for("the prompt", prompt);
    env.wait_log("md", "the refusal", |l| l.contains("refused:") && l.contains("reserved"));
    env.wait_log("md", "the options", |l| l.contains(r#"options 90 ["true"] text 300"#));
    env.wait_log("md", "the unknown option", |l| l.contains("option refused: no option \"nope\""));
    t.keys(&["C-x", "e"]);
    t.wait_text("mode-terminal");
    t.keys(&["C-x", "f"]);
    t.wait_text("fed");
    // <C-x> alone times out and goes to the shell; the next key types on.
    t.key("C-x");
    std::thread::sleep(std::time::Duration::from_millis(500));
    t.type_text("echo after-timeout\n");
    t.wait_text("after-timeout");
    // A user mode inheriting normal: x is unmapped there too (unmap in normal).
    t.keys(&["C-x", "w"]);
    env.wait_log("md", "the mode change", |l| l.contains("mode terminal win"));
    t.key("x");
    t.key("v");
    t.wait_for("the new pane's command", |s| (0..s.cells.len() as u16).any(|r| s.row_from(r, 50) == "in-new"));
    let lines = env.script_lines("md");
    assert!(lines.iter().any(|l| l == "mode win terminal"), "the new pane's mode after the split: {lines:?}");
    let ls = env.run(&["ls"]);
    assert!(ls.contains("md  (2 panes"), "x did not close a pane: {ls}");
}

#[test]
fn errors_float_until_the_next_key() {
    let env = Env::new("notice");
    env.config(
        "config.js",
        "import { cmd, map } from \"temzdf\";\nmap(\"terminal\", \"<C-x>\", () => cmd.moveFocus(\"sideways\"));\n",
    );
    let mut t = Term::spawn(&[BIN, "-s", "n"], &env.options());
    t.wait_for("the prompt", prompt);
    t.key("C-x");
    let s = t.wait_text("sideways");
    let row = s.find("sideways").unwrap();
    assert!(s.lines()[row].find("sideways").unwrap() > 50, "at the right: {}", s.text());
    assert!(s.text().contains('╭'), "in a box: {}", s.text());
    t.type_text("echo gone\n");
    t.wait_for("the message gone and the key handled", |s| !s.contains("sideways") && s.contains("gone"));
}

#[test]
fn a_closed_shell_stays_hidden_and_shows_again() {
    let env = Env::new("buffers");
    let mut t = Term::spawn(&[BIN, "-s", "b"], &env.options());
    t.wait_for("the prompt", prompt);
    t.type_text("echo marker-left\n");
    t.wait_text("marker-left");
    t.keys(&["C-b", "%"]);
    t.wait_for("a prompt in the right pane", |s| is_prompt(&s.row_from(0, 50)));
    // Close the left pane: its shell keeps running, hidden.
    t.keys(&["C-b", "h", "C-b", "x"]);
    t.wait_for("one pane", |s| !s.contains("marker-left"));
    let ls = env.run(&["ls"]);
    assert!(ls.contains("b  (1 panes, 2 buffers, 1 attached)"), "{ls}");
    // The first shell's buffer is 2 (the first pane took 1).
    env.run(&["action", "-s", "b", "show-buffer", "2"]);
    t.wait_text("marker-left");
    t.type_text("echo still-$((2+3))\n");
    t.wait_text("still-5");
    // Closing the last pane ends the session, although a shell runs hidden.
    t.keys(&["C-b", "x"]);
    t.wait_text("[session b ended]");
    assert_eq!(t.wait_exit(), 0);
}

#[test]
fn dragging_in_a_shell_copies_to_the_outer_terminal() {
    let env = Env::new("select");
    let mut t = Term::spawn(&[BIN, "-s", "c"], &env.options());
    t.wait_for("the prompt", prompt);
    t.type_text("clear; echo copy-me please\n");
    let s = t.wait_for("the output", |s| s.lines().iter().any(|l| l == "copy-me please"));
    let row = s.find("copy-me please").unwrap() as u16;
    t.drag((0, row), (6, row));
    let s = t.wait_for("the selection", |s| s.cell(3, row).inverse);
    assert!(!s.cell(8, row).inverse, "only the dragged cells");
    let deadline = std::time::Instant::now() + tuitest::TIMEOUT;
    while t.copied().is_empty() && std::time::Instant::now() < deadline {
        t.pump(std::time::Duration::from_millis(50));
    }
    assert_eq!(t.copied(), ["copy-me"]);
    // Typing clears the selection.
    t.type_text("x");
    t.wait_for("no selection", |s| !s.cell(3, row).inverse);
}

#[test]
fn borders_drag_and_the_shipped_keys_resize_panes() {
    let env = Env::new("resize");
    let mut t = Term::spawn(&[BIN, "-s", "r"], &env.options());
    t.wait_for("the prompt", prompt);
    // 100 columns: 49 + border + 50.
    t.keys(&["C-b", "%"]);
    t.wait_for("a prompt in the right pane", |s| is_prompt(&s.row_from(0, 50)));
    t.drag((49, 5), (70, 5));
    t.type_text("clear; stty size\n");
    // `stty size` prints "ROWS COLS".
    let widths = |s: &tuitest::Screen| -> Vec<u16> {
        let lines = s.lines();
        let pairs = lines.iter().flat_map(|l| {
            let words: Vec<_> = l.split_whitespace().map(|w| w.parse::<u16>().ok()).collect();
            words.windows(2).filter_map(|w| Some((w[0]?, w[1]?))).collect::<Vec<_>>()
        });
        pairs.map(|(_, cols)| cols).collect()
    };
    t.wait_for("the new size", |s| widths(s).contains(&29));
    // The shipped mappings: the right pane five columns wider (Vim's <C-w>>).
    t.keys(&["C-b", "5", ">", "Esc"]);
    t.type_text("clear; stty size\n");
    t.wait_for("five columns wider", |s| widths(s).contains(&34));
}

#[test]
fn images_from_a_program_show_inside_its_pane() {
    let env = Env::new("graphics");
    let mut t = Term::spawn(&[BIN, "-s", "g"], &env.options());
    t.wait_for("the prompt", prompt);
    // The right half (100 columns: 49 + border + 50).
    t.keys(&["C-b", "%"]);
    t.wait_for("a prompt in the right pane", |s| is_prompt(&s.row_from(0, 50)));
    // A 2x2 RGBA image over 4x2 cells, by the kitty graphics protocol.
    t.type_text("clear; printf '\\033_Ga=T,f=32,s=2,v=2,c=4,r=2,q=2;/wAA/wD/AP8AAP///////w==\\033\\\\'; echo\n");
    let s = t.wait_for("the image", |s| s.images.iter().any(|p| p.width == 2 && p.height == 2));
    let p = s.images.iter().find(|p| p.width == 2 && p.height == 2).unwrap();
    assert!(p.col >= 50, "inside the right pane: {p:?}");
    assert_eq!((p.cols, p.rows), (4, 2), "{p:?}");
}

/// Cells of the default float on a 100x30 screen: its frame (centred, 80% by
/// 80%) and its content inside the frame.
const FLOAT_FRAME: (u16, u16, u16, u16) = (10, 3, 80, 24);
const FLOAT_CONTENT: (u16, u16, u16, u16) = (11, 4, 78, 22);

/// Whether anything of a lower pane shows inside the default float: a `0`
/// from the shell below.
/// A shell prompt at the top left of the default float's content (the
/// frame's right edge ends the row).
fn float_prompt(s: &Screen) -> bool {
    let row = s.row_from(FLOAT_CONTENT.1, FLOAT_CONTENT.0);
    row.starts_with('$') || row.starts_with("sh-")
}

fn shows_through(s: &Screen) -> Option<(u16, u16)> {
    let (x0, y0, w, h) = FLOAT_FRAME;
    (y0..y0 + h).flat_map(|y| (x0..x0 + w).map(move |x| (x, y))).find(|&(x, y)| s.cell(x, y).text == "0")
}

#[test]
fn a_floating_shell_covers_a_shell_and_a_pdf() {
    let env = Env::new("floats");
    let mut t = Term::spawn(&[BIN, "-s", "f"], &env.options());
    t.wait_for("the prompt", prompt);
    // Zeros all over the left half.
    t.type_text("clear; for i in $(seq 28); do printf '%049d\\n' 0; done\n");
    t.wait_for("zeros", |s| s.lines().iter().filter(|l| l.starts_with("0000")).count() >= 25);
    let pdf = fixture("sample.pdf");
    env.run(&["action", "-s", "f", "new-pane", &pdf.display().to_string()]);
    t.wait_for("pages of the PDF", |s| s.images.iter().any(|p| p.col >= 50 && p.cols > 1));
    env.run(&["action", "-s", "f", "open-float"]);
    t.wait_for("a prompt in the float", float_prompt);
    let (cx, cy, cw, ch) = FLOAT_CONTENT;
    t.type_text("echo inside-$((3*4))\n");
    let s = t.wait_text("inside-12");
    assert_eq!(shows_through(&s), None, "the shell below shows through");
    // No image of the PDF inside the float: only its frame's lines are
    // images there, and they stay in the frame's cells.
    let content = |p: &tuitest::Placement| {
        p.col < (cx + cw) as i32
            && p.col + p.cols as i32 > cx as i32
            && p.row < (cy + ch) as i32
            && p.row + p.rows as i32 > cy as i32
    };
    let inside: Vec<_> = s.images.iter().filter(|p| content(p)).collect();
    assert!(inside.is_empty(), "images inside the float: {inside:?}");
    assert!(s.images.iter().any(|p| p.col >= 90 && p.cols > 1), "the PDF beside the float still shows");
}

#[test]
fn a_floating_shell_inside_tmux() {
    if !tuitest::have_tmux() {
        eprintln!("SKIPPED: tmux not found");
        return;
    }
    let env = Env::new("floats-tmux");
    let mut t = Term::inside_tmux(&["env", "SHELL=/bin/sh", BIN, "-s", "ft"], &env.options());
    t.wait_for("the prompt", prompt);
    t.type_text("clear; for i in $(seq 28); do printf '%098d\\n' 0; done\n");
    t.wait_for("zeros", |s| s.lines().iter().filter(|l| l.starts_with("0000")).count() >= 25);
    env.run(&["action", "-s", "ft", "open-float"]);
    t.wait_for("a prompt in the float", float_prompt);
    let (cx, cy, cw, ch) = FLOAT_CONTENT;
    t.type_text("clear\n");
    let s = t.wait_for("the float cleared", |s| shows_through(s).is_none());
    // Its frame is drawn through placeholders; its content is text only.
    let cells = s.placeholder_cells();
    assert!(cells.iter().any(|&(x, y)| (x, y) == (FLOAT_FRAME.0, FLOAT_FRAME.1 + 5)), "the frame's left edge");
    assert!(!cells.iter().any(|&(x, y)| x >= cx && x < cx + cw && y >= cy && y < cy + ch), "nothing over the content");
}

#[test]
fn a_float_moves_by_its_title_edge_and_a_click_raises_it() {
    let env = Env::new("float-drag");
    let mut t = Term::spawn(&[BIN, "-s", "fd"], &env.options());
    t.wait_for("the prompt", prompt);
    env.run(&["action", "-s", "fd", "open-float"]);
    t.wait_for("a prompt in the float", float_prompt);
    // The title edge from column 30 to 35, row 3 to 5: the float moves 5
    // right and 2 down.
    t.drag((30, FLOAT_FRAME.1), (35, FLOAT_FRAME.1 + 2));
    let (cx, cy) = (FLOAT_CONTENT.0 + 5, FLOAT_CONTENT.1 + 2);
    t.wait_for("the float moved", |s| s.row_from(cy, cx).starts_with('$'));
    t.type_text("stty size\n");
    t.wait_for("its shell's size", |s| s.row_from(cy + 1, cx).starts_with("22 78"));
    // A click on the tiled shell focuses it; typing goes there.
    t.click(2, 1);
    t.type_text("echo tiled-$((1+1))\n");
    t.wait_text("tiled-2");
    // A second float (centred: the focus is not in a float) over the first;
    // a click on the first's right edge, beside the second, raises it.
    env.run(&["action", "-s", "fd", "open-float", "--", "sh", "-c", "echo second; sleep 30"]);
    t.wait_text("second");
    t.click(FLOAT_FRAME.0 + 5 + FLOAT_FRAME.2 - 1, cy + 10);
    t.type_text("echo first-$((2+3))\n");
    t.wait_text("first-5");
}
