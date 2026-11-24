//! temzdf in a terminal on Windows: the console frontend behind the
//! simulated terminal (`tuitest`, on the in-box ConPTY there). That ConPTY
//! drops kitty graphics and answers DA1 itself before the terminal's other
//! answers come (`doc/terminals/conpty.md`), so this is the frontend without
//! images: the message saying why, keys, resizes, detaching and quitting.
//! The Unix tests of the frontend are in `tui.rs`. See doc/dev/testing.md.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;

use tuitest::{Options, Screen, TempDir, Term};

const BIN: &str = env!("CARGO_BIN_EXE_temzdf");

/// A test's private environment (as in `tui.rs`).
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
        ]
    }

    /// A terminal whose cells are 9x18 px: the probe must have asked the
    /// terminal (past ConPTY's early DA1 answer) to know.
    fn options(&self) -> Options {
        Options { cell: (9, 18), env: self.vars(), ..Options::default() }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.envs(self.vars()).env_remove("TEMZDF_SESSION_NAME");
        cmd
    }

    fn run(&self, args: &[&str]) -> String {
        let out = self.command().args(args).output().unwrap();
        assert!(out.status.success(), "temzdf {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The client's log (`temzdf.log`: the frontend's stderr).
    fn client_log(&self) -> String {
        std::fs::read_to_string(self.path("log/temzdf.log")).unwrap_or_default()
    }

    /// Wait until the client's log satisfies `done`; returns it.
    fn wait_client_log(&self, what: &str, done: impl Fn(&str) -> bool) -> String {
        let deadline = std::time::Instant::now() + tuitest::TIMEOUT;
        loop {
            let log = self.client_log();
            if done(&log) {
                return log;
            }
            assert!(std::time::Instant::now() < deadline, "waiting for {what} in the client's log:\n{log}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // End whatever server a failed test left running.
        let _ = self.command().args(["kill-all-sessions", "--force"]).output();
    }
}

/// A one-page PDF written here (no TeX on Windows for the fixtures).
fn one_page_pdf(path: &std::path::Path) {
    let objects = [
        "<</Type/Catalog/Pages 2 0 R>>",
        "<</Type/Pages/Kids[3 0 R]/Count 1>>",
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>",
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let xref = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
    for o in offsets {
        pdf.push_str(&format!("{o:010} 00000 n \n"));
    }
    pdf.push_str(&format!("trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1));
    std::fs::write(path, pdf).unwrap();
}

/// The message saying why there are no images (until a key). (It wraps: its first words.)
fn no_images(s: &Screen) -> bool {
    s.text().contains("this terminal does not support the kitty")
}

#[test]
fn a_pdf_session_without_images_takes_keys_and_resizes() {
    let env = Env::new("win-attach");
    let pdf = env.path("one.pdf");
    one_page_pdf(&pdf);
    let mut t = Term::spawn(&[BIN, "-s", "w", pdf.to_str().unwrap()], &env.options());
    // The message saying why there are no images, on the screen.
    let s = t.wait_for("the message and the PDF pane's line", |s| {
        no_images(s) && s.text().contains("PDF viewer disabled (no kitty graphics protocol support)")
    });
    assert!(s.images.is_empty(), "the in-box ConPTY drops images: {:?}", s.images);
    let log = env.wait_client_log("the geometry", |l| l.contains("geometry "));
    assert!(log.contains("no images: this terminal does not support the kitty graphics protocol"), "{log}");
    // The cell size is the terminal's (`CSI 16 t`), whose answer comes after
    // ConPTY's DA1 answer.
    assert!(log.contains("geometry 100x30 cells of 9x18 px"), "{log}");

    // A key takes the message away (and does what it does).
    t.key("j");
    t.wait_for("the message gone", |s| !no_images(s));

    // A resize reaches the session.
    t.resize(80, 24);
    env.wait_client_log("the resize", |l| l.contains("resized to geometry 80x24 cells of 9x18 px"));
    let ls = env.run(&["ls"]);
    assert_eq!(ls, "w  (1 attached)\n");

    // Detach (the prefix, then d), and attach again.
    t.keys(&["C-b", "d"]);
    t.wait_text("[detached from session w]");
    assert_eq!(t.wait_exit(), 0);
    assert_eq!(env.run(&["ls"]), "w  (detached)\n");
    let mut t = Term::spawn(&[BIN, "attach", "w"], &env.options());
    t.wait_for("the message about images", no_images);
    // Ctrl+C is a key (no signal): the session takes it and goes on.
    t.key("C-c");
    t.wait_for("the message gone", |s| !no_images(s));
    assert_eq!(env.run(&["ls"]), "w  (1 attached)\n");
    // Quitting ends the session and the client.
    env.run(&["kill-session", "w"]);
    t.wait_text("[session w was killed]");
    assert_eq!(t.wait_exit(), 0);
}

#[test]
fn ctrl_break_detaches_and_puts_the_console_back() {
    let env = Env::new("win-break");
    let pdf = env.path("one.pdf");
    one_page_pdf(&pdf);
    let mut t = Term::spawn(&[BIN, "-s", "b", pdf.to_str().unwrap()], &env.options());
    t.wait_for("the message about images", no_images);
    // Ctrl+Break as Windows Terminal sends it to ConPTY (win32-input-mode:
    // VK_CANCEL, its scan code, no character, down, left Ctrl), which
    // makes it the console's control event. Without a handler Windows
    // ended the frontend at once, leaving its screen and modes behind.
    t.write(b"\x1b[3;70;0;1;8;1_");
    t.wait_text("[detached from session b]");
    assert_eq!(t.wait_exit(), 0);
    assert_eq!(env.run(&["ls"]), "b  (detached)\n");
}
