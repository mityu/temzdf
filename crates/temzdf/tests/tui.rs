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
    line.trim_end().ends_with('$')
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
    assert!(ls.contains("d  (1 panes, detached)"), "{ls}");
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
    assert!(ls.contains("p  (3 panes, 1 attached)") && ls.contains("sample.pdf"), "{ls}");
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
    env.wait_log("h", "the second pane", |l| l.contains("opened 2"));
    // Closing it: the "before" hook runs first; the pane its split creates
    // fires no PaneOpened (events inside hooks only reach nested hooks).
    t.keys(&["C-b", "x"]);
    env.wait_log("h", "the close", |l| l.contains("closed 2"));
    let lines = env.script_lines("h");
    assert_eq!(lines, ["opened 1 terminal", "ready", "opened 2 terminal", "pre 2", "closed 2"], "{lines:?}");
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
    t.type_text("clear; echo still-$((2+2))\n");
    t.wait_text("still-4");
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
"#,
    );
    let mut t = Term::spawn(&[BIN, "-s", "md"], &env.options());
    t.wait_for("the prompt", prompt);
    env.wait_log("md", "the refusal", |l| l.contains("refused:") && l.contains("reserved"));
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
