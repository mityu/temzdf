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
    t.keys(&["C-s", "d"]);
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
    t.keys(&["C-s", "%"]);
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
    t.keys(&["C-s", "%"]);
    t.wait_for("a second pane", |s| is_prompt(&s.row_from(0, 50)));
    let pdf = fixture("sample.pdf");
    env.run(&["action", "-s", "x", "new-pane", &pdf.display().to_string()]);
    // tmux shows images only through Unicode placeholder cells.
    let s = t.wait_for("pages (through Unicode placeholders)", |s| s.placeholder_cells().iter().any(|(x, _)| *x >= 76));
    assert!(s.images.is_empty(), "no direct placements inside tmux: {:?}", s.images);
}
