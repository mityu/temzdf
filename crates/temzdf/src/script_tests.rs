//! `temzdf test FILE...`: tests of temzdf's behaviour written as scripts
//! (`temzdf/test`, crates/temzdf-script/runtime/test/), one session per
//! file.
//!
//! Each file runs in a child process (`temzdf test --one FILE`, hidden)
//! whose `XDG_*`, `TEMZDF_LOG_DIR` and `TMPDIR` point into a directory of
//! its own, made and removed here: nothing reaches the user's sessions,
//! swap files or configuration, nor another file's. The child runs a
//! session without clients in real time with the file as its
//! configuration and prints the TAP it reports. Here at most `-j` children
//! run at once, one printing no TAP line for `--timeout` is stopped (a
//! synchronous endless loop in a test), and each file's TAP is printed in
//! one piece, in the order the files were given, its stderr only when it
//! failed.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use temzdf_scene::{Effect, Hints};
use temzdf_server::action::PaneSpec;
use temzdf_server::session::{Outcome, ScriptSettings, Session, Settings, TestReport};
use temzdf_server::watchdog::Watchdog;
use termpix_core::geom::{CellSize, Geometry};

/// The child's private directory (set for `--one` by the parent).
const DIR_VAR: &str = "TEMZDF_TEST_DIR";
/// The sessions' screen: fixed, so that tests see the same layout anywhere.
const GEOMETRY: Geometry = Geometry { cols: 80, rows: 24, cell: CellSize { width: 10, height: 20 } };
/// How often the parent looks at its children.
const TICK: Duration = Duration::from_millis(20);

/// `temzdf test`: run FILES, at most `jobs` at once; the exit status (1
/// when a file failed).
pub fn run(files: &[PathBuf], jobs: usize, timeout: Duration) -> Result<i32> {
    for f in files {
        if !f.is_file() {
            bail!("{}: no such file", f.display());
        }
    }
    let exe = std::env::current_exe().context("cannot find the temzdf executable")?;
    let mut queue: VecDeque<(usize, &PathBuf)> = files.iter().enumerate().collect();
    let mut running: Vec<Job> = Vec::new();
    let mut done: Vec<Option<Done>> = files.iter().map(|_| None).collect();
    let mut shown = 0;
    let mut total = Total::default();
    let mut out = std::io::stdout();
    loop {
        while running.len() < jobs.max(1)
            && let Some((i, file)) = queue.pop_front()
        {
            running.push(Job::spawn(&exe, i, file)?);
        }
        if running.is_empty() {
            break;
        }
        let mut k = 0;
        while k < running.len() {
            let job = &mut running[k];
            if let Some(status) = job.child.try_wait()? {
                let job = running.swap_remove(k);
                let index = job.index;
                done[index] = Some(job.finish(status));
                continue;
            }
            let quiet = job.last.lock().map_or(Duration::ZERO, |t| t.elapsed());
            if job.stopped.is_none() && quiet > timeout {
                let _ = job.child.kill();
                job.stopped = Some(quiet);
            }
            k += 1;
        }
        while let Some(d) = done.get_mut(shown).and_then(Option::take) {
            d.print(&mut out, timeout)?;
            total.add(&d);
            shown += 1;
        }
        std::thread::sleep(TICK);
    }
    let failed_files = if total.failed_files > 0 { format!(" ({} failed)", total.failed_files) } else { String::new() };
    writeln!(
        out,
        "# {} files{failed_files}, {} tests: {} passed, {} failed",
        files.len(),
        total.passed + total.failed,
        total.passed,
        total.failed
    )?;
    out.flush()?;
    Ok(i32::from(total.failed_files > 0))
}

/// A child running one file.
struct Job {
    index: usize,
    file: PathBuf,
    dir: PathBuf,
    child: Child,
    /// When it printed its last line (or started).
    last: Arc<Mutex<Instant>>,
    stdout: JoinHandle<Vec<String>>,
    stderr: JoinHandle<Vec<u8>>,
    /// Stopped by the watchdog after this long without a line.
    stopped: Option<Duration>,
}

impl Job {
    fn spawn(exe: &Path, index: usize, file: &Path) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("temzdf-test-{}-{index}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        private_dir(&dir)?;
        let sub = |name: &str| -> Result<PathBuf> {
            let d = dir.join(name);
            private_dir(&d)?;
            Ok(d)
        };
        let mut cmd = Command::new(exe);
        cmd.arg("test")
            .arg("--one")
            .arg(std::path::absolute(file)?)
            .env(DIR_VAR, &dir)
            .env("XDG_RUNTIME_DIR", sub("run")?)
            .env("XDG_STATE_HOME", sub("state")?)
            .env("XDG_CONFIG_HOME", sub("config")?)
            .env("TEMZDF_LOG_DIR", sub("log")?)
            .env("TMPDIR", sub("tmp")?)
            .env_remove("TEMZDF_SESSION_NAME")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().with_context(|| format!("cannot run {}", exe.display()))?;
        let last = Arc::new(Mutex::new(Instant::now()));
        let stdout = {
            let pipe = child.stdout.take().expect("piped");
            let last = last.clone();
            std::thread::spawn(move || {
                let mut lines = Vec::new();
                for line in BufReader::new(pipe).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(mut t) = last.lock() {
                        *t = Instant::now();
                    }
                    lines.push(line);
                }
                lines
            })
        };
        let stderr = {
            let mut pipe = child.stderr.take().expect("piped");
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                buf
            })
        };
        Ok(Job { index, file: file.to_owned(), dir, child, last, stdout, stderr, stopped: None })
    }

    fn finish(self, status: ExitStatus) -> Done {
        let lines = self.stdout.join().unwrap_or_default();
        let stderr = self.stderr.join().unwrap_or_default();
        let _ = std::fs::remove_dir_all(&self.dir);
        Done { file: self.file, lines, stderr, status, stopped: self.stopped }
    }
}

/// `dir`, made readable by us alone (XDG_RUNTIME_DIR must be).
fn private_dir(dir: &Path) -> Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut b = std::fs::DirBuilder::new();
    // (Windows: the profile's ACL, as for the session directories.)
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir).with_context(|| format!("cannot create {}", dir.display()))
}

/// A child that ended.
struct Done {
    file: PathBuf,
    lines: Vec<String>,
    stderr: Vec<u8>,
    status: ExitStatus,
    stopped: Option<Duration>,
}

impl Done {
    fn failed(&self) -> bool {
        self.stopped.is_some() || !self.status.success()
    }

    /// The tests that passed and failed (TAP's `ok` and `not ok` lines).
    fn counts(&self) -> (u64, u64) {
        let passed = self.lines.iter().filter(|l| l.starts_with("ok ")).count() as u64;
        let failed = self.lines.iter().filter(|l| l.starts_with("not ok ")).count() as u64;
        (passed, failed)
    }

    fn print(&self, out: &mut impl Write, timeout: Duration) -> Result<()> {
        writeln!(out, "# FILE: {}", self.file.display())?;
        for l in &self.lines {
            writeln!(out, "{l}")?;
        }
        if self.stopped.is_some() {
            writeln!(out, "Bail out! no TAP line for {} s: stopped", timeout.as_secs_f64())?;
        } else if !self.status.success() && self.status.code() != Some(1) {
            writeln!(out, "Bail out! the test process ended: {}", self.status)?;
        }
        if self.failed() && !self.stderr.is_empty() {
            writeln!(out, "# stderr:")?;
            for l in String::from_utf8_lossy(&self.stderr).lines() {
                writeln!(out, "#   {l}")?;
            }
        }
        out.flush()?;
        Ok(())
    }
}

#[derive(Default)]
struct Total {
    passed: u64,
    failed: u64,
    failed_files: u64,
}

impl Total {
    fn add(&mut self, d: &Done) {
        let (passed, failed) = d.counts();
        self.passed += passed;
        self.failed += failed;
        self.failed_files += u64::from(d.failed());
    }
}

/// `temzdf test --one FILE`: run one file in a session of this process,
/// its TAP on stdout; the exit status (1 when a test failed or the run
/// broke off: `Bail out!`).
pub fn run_one(file: &Path) -> Result<i32> {
    let Some(dir) = std::env::var_os(DIR_VAR).map(PathBuf::from) else {
        bail!("--one is for `temzdf test` (it gives the run a directory of its own)");
    };
    let file = std::path::absolute(file)?;
    let scratch = dir.join("scratch.txt");
    std::fs::write(&scratch, "").with_context(|| format!("cannot create {}", scratch.display()))?;
    let mut out = std::io::stdout();
    let mut say = |line: &str| -> Result<()> {
        writeln!(out, "{line}")?;
        out.flush()?;
        Ok(())
    };
    // Each step returns at once; one that does not (CI saw a child that had
    // reported every test hang) is named on stderr, which the parent shows,
    // before it stops the child for printing nothing.
    let watchdog = Watchdog::new(format!("temzdf test {}", file.display()), STEPS, Duration::from_secs(20));
    let (tx, rx) = mpsc::channel();
    let sink = temzdf_scene::sink::channel_sink(tx, |e| e);
    let settings =
        Settings { serve_forward: false, script: Some(ScriptSettings { config: Some(file) }), overrides: Vec::new() };
    let name = format!("test-{}", std::process::id());
    let mut s = Session::new(name, GEOMETRY, Hints::default(), settings, sink);
    // Terminal panes run a known shell, not the user's $SHELL.
    s.set_shell(Some(vec!["/bin/sh".into()]));
    s.enable_test_reports();
    s.start(PaneSpec::File(scratch), Some(dir))?;
    let mut clipboard = String::new();
    loop {
        if let Some(e) = s.config_error() {
            say(&format!("Bail out! the test file did not load: {}", one_line(e)))?;
            return Ok(1);
        }
        if let Some(e) = s.script_host_exit() {
            say(&format!("Bail out! the script host ended: {}", one_line(e)))?;
            return Ok(1);
        }
        let wait = s.next_deadline().map_or(TICK, |d| d.saturating_duration_since(Instant::now())).min(TICK);
        let mut outcomes = Vec::new();
        watchdog.step(1);
        if let Ok(ev) = rx.recv_timeout(wait) {
            watchdog.step(2);
            outcomes.push(s.handle(ev, Instant::now()));
            while let Ok(ev) = rx.try_recv() {
                outcomes.push(s.handle(ev, Instant::now()));
            }
        }
        watchdog.step(3);
        outcomes.push(s.poll(Instant::now()));
        watchdog.step(4);
        // A clipboard of our own: what was copied is what is read.
        for e in s.take_effects() {
            match e {
                Effect::Copy(text) => clipboard = text,
                Effect::ReadClipboard { ticket, .. } => {
                    outcomes.push(s.clipboard_answer(ticket, Ok(clipboard.clone())))
                }
                Effect::Paste { ticket, .. } => outcomes.push(s.clipboard_answer(ticket, Ok(clipboard.clone()))),
                // Links and inverse search are not followed in tests.
                _ => {}
            }
        }
        watchdog.step(5);
        let _ = s.frame();
        watchdog.step(6);
        for r in s.take_test_reports() {
            match r {
                TestReport::Line(l) => say(&l)?,
                TestReport::End { failed, .. } => {
                    watchdog.step(7);
                    drop(s);
                    return Ok(i32::from(failed > 0));
                }
            }
        }
        if outcomes.contains(&Outcome::Quit) {
            say("Bail out! the session ended (its last pane closed) before the tests did")?;
            return Ok(1);
        }
    }
}

/// `run_one`'s steps, for its watchdog.
const STEPS: &[&str] = &[
    "starting",
    "waiting for an event",
    "handling events",
    "poll",
    "answering effects",
    "frame",
    "test reports",
    "ending the session",
];

/// An error message on one line (TAP's `Bail out!` takes one).
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
