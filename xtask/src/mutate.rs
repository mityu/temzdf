//! `cargo xtask mutate`: a mutation check. Puts a bug back into a file
//! (OLD replaced by NEW, exactly one occurrence), runs a command and always
//! restores the file, so a test written for a fix is proven to fail without
//! it. The command's output goes to a log file, never through argv.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use anyhow::{Context, Result};

pub struct Options {
    first_run: bool,
    file: String,
    old: String,
    new: String,
    command: Vec<String>,
}

impl Options {
    /// `None`: a usage error.
    pub fn parse(args: impl Iterator<Item = String>) -> Option<Options> {
        let mut args: Vec<String> = args.collect();
        let first_run = if args.first().map(String::as_str) == Some("-n") {
            args.remove(0);
            false
        } else {
            true
        };
        if args.len() < 5 || args[3] != "--" {
            return None;
        }
        let command = args.split_off(4);
        let mut it = args.into_iter();
        let (file, old, new) = (it.next()?, it.next()?, it.next()?);
        if old.is_empty() {
            return None;
        }
        Some(Options { first_run, file, old, new, command })
    }
}

/// Occurrences of `old` in `s`, overlapping ones too (`aa` is twice in `aaa`).
fn count_overlapping(s: &str, old: &str) -> usize {
    let mut count = 0;
    let mut rest = s;
    while let Some(i) = rest.find(old) {
        count += 1;
        let step = rest[i..].chars().next().map_or(1, char::len_utf8);
        rest = &rest[i + step..];
    }
    count
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Ran,
    DidNotCompile,
    NoTestRan,
}

/// What cargo's output says: no compile error, and some test counted.
fn judge(out: &str) -> Verdict {
    if out.contains("error: could not compile") {
        return Verdict::DidNotCompile;
    }
    let ran = out.lines().any(|l| {
        l.find("running ").is_some_and(|i| l[i + 8..].chars().next().is_some_and(|c| c.is_ascii_digit() && c != '0'))
    });
    if out.lines().any(|l| l.starts_with("test result")) && !ran {
        return Verdict::NoTestRan;
    }
    Verdict::Ran
}

struct Pending {
    id: usize,
    file: PathBuf,
    backup: PathBuf,
}

/// The files to put back: the guards' and, on Ctrl-C, the handler's business.
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn restore_one(p: &Pending) {
    if fs::copy(&p.backup, &p.file).is_ok() {
        // A fresh mtime: the old one is older than the mutated build, and
        // cargo would keep running that.
        if let Ok(f) = File::options().write(true).open(&p.file) {
            let _ = f.set_modified(SystemTime::now());
        }
        let _ = fs::remove_file(&p.backup);
    } else {
        eprintln!("COULD NOT RESTORE {}: the original is in {}", p.file.display(), p.backup.display());
    }
}

/// Restores its file when dropped, on every return path (and a panic).
struct Guard(usize);

impl Drop for Guard {
    fn drop(&mut self) {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = pending.iter().position(|p| p.id == self.0) {
            restore_one(&pending.remove(i));
        }
    }
}

fn unique_temp(prefix: &str) -> PathBuf {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}.{}.{n}", std::process::id()))
}

/// Runs the command with both streams into `log`; whether it succeeded.
fn run_logged(command: &[String], dir: &Path, log: &Path) -> Result<bool> {
    let out = File::create(log)?;
    let err = out.try_clone()?;
    let status = Command::new(&command[0])
        .args(&command[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .status()
        .with_context(|| format!("cannot run {}", command[0]))?;
    Ok(status.success())
}

fn read_log(log: &Path) -> String {
    String::from_utf8_lossy(&fs::read(log).unwrap_or_default()).into_owned()
}

pub fn mutate(args: impl Iterator<Item = String>) -> Result<i32> {
    let Some(opts) = Options::parse(args) else {
        eprint!("{}", crate::USAGE);
        return Ok(2);
    };
    // Ctrl-C reaches the child too; the file is put back before exiting.
    let _ = ctrlc::set_handler(|| {
        let pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        for p in pending.iter() {
            restore_one(p);
        }
        std::process::exit(130);
    });
    run(&crate::root(), &opts)
}

fn run(root: &Path, opts: &Options) -> Result<i32> {
    let file = root.join(&opts.file);
    if !file.is_file() {
        eprintln!("no such file: {}", opts.file);
        return Ok(2);
    }
    let content = fs::read_to_string(&file).with_context(|| format!("cannot read {}", opts.file))?;
    let count = count_overlapping(&content, &opts.old);
    if count != 1 {
        eprintln!("OLD occurs {count} times in {} (needs exactly 1)", opts.file);
        return Ok(2);
    }

    let log = unique_temp("mutate");
    if opts.first_run {
        if !run_logged(&opts.command, root, &log)? {
            eprintln!("COMMAND fails before the mutation (see {})", log.display());
            return Ok(2);
        }
        if judge(&read_log(&log)) != Verdict::Ran {
            eprintln!("no test ran before the mutation (see {})", log.display());
            return Ok(4);
        }
    }

    let backup = unique_temp("mutate-orig");
    fs::copy(&file, &backup)?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).push(Pending { id, file: file.clone(), backup });
    let _guard = Guard(id);
    let mutated = content.replacen(&opts.old, &opts.new, 1);
    File::create(&file)?.write_all(mutated.as_bytes())?;

    let passed = run_logged(&opts.command, root, &log)?;
    let out = read_log(&log);
    match judge(&out) {
        Verdict::DidNotCompile => {
            println!("DID NOT COMPILE: the mutation proves nothing (see {})", log.display());
            return Ok(3);
        }
        Verdict::NoTestRan => {
            println!("NO TEST RAN (see {})", log.display());
            return Ok(4);
        }
        Verdict::Ran => {}
    }
    if passed {
        println!("SURVIVED: COMMAND passes with the mutation (see {})", log.display());
        return Ok(1);
    }
    println!("CAUGHT:");
    let failed = |l: &&str| (l.starts_with("test ") && l.ends_with(" FAILED")) || l.contains("panicked at");
    for l in out.lines().filter(failed).take(5) {
        println!("{l}");
    }
    let _ = fs::remove_file(&log);
    Ok(0)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(name: &str, content: &str) -> (PathBuf, PathBuf) {
        let dir = unique_temp(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("f.txt"), content).unwrap();
        (dir.clone(), dir.join("f.txt"))
    }

    fn opts(first: bool, old: &str, new: &str, script: &str) -> Options {
        let command = ["sh", "-c", script].map(String::from).to_vec();
        Options { first_run: first, file: "f.txt".into(), old: old.into(), new: new.into(), command }
    }

    #[test]
    fn counts_overlapping_occurrences() {
        assert_eq!(count_overlapping("aaa", "aa"), 2);
        assert_eq!(count_overlapping("abc", "x"), 0);
        assert_eq!(count_overlapping("あああ", "ああ"), 2);
    }

    #[test]
    fn judges_cargo_output() {
        assert_eq!(judge("error: could not compile `x`"), Verdict::DidNotCompile);
        assert_eq!(judge("running 0 tests\ntest result: ok. 0 passed"), Verdict::NoTestRan);
        assert_eq!(judge("running 3 tests\ntest result: ok. 3 passed"), Verdict::Ran);
        assert_eq!(judge("some other tool"), Verdict::Ran);
    }

    #[test]
    fn parses_arguments() {
        let a = |v: &[&str]| Options::parse(v.iter().map(|s| s.to_string()));
        assert!(a(&["f", "a", "b", "--", "true"]).is_some());
        assert!(a(&["-n", "f", "a", "b", "--", "true"]).is_some_and(|o| !o.first_run));
        assert!(a(&["f", "a", "b", "true"]).is_none());
        assert!(a(&["f", "", "b", "--", "true"]).is_none());
    }

    #[test]
    fn refuses_a_pattern_not_found_once() {
        let (dir, file) = scratch("mut-once", "aaa");
        assert_eq!(run(&dir, &opts(false, "aa", "b", "true")).unwrap(), 2);
        assert_eq!(run(&dir, &opts(false, "x", "b", "true")).unwrap(), 2);
        assert_eq!(fs::read_to_string(file).unwrap(), "aaa");
    }

    #[test]
    #[cfg_attr(miri, ignore = "runs processes")]
    fn caught_and_survived_restore_the_file() {
        let (dir, file) = scratch("mut-verdict", "hello world\n");
        // Fails with the mutation.
        assert_eq!(run(&dir, &opts(false, "hello", "bye", "grep -q hello f.txt")).unwrap(), 0);
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello world\n");
        // Passes with it.
        assert_eq!(run(&dir, &opts(false, "hello", "bye", "true")).unwrap(), 1);
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello world\n");
        // Fails before the mutation.
        assert_eq!(run(&dir, &opts(true, "hello", "bye", "false")).unwrap(), 2);
    }

    #[test]
    #[cfg_attr(miri, ignore = "runs processes")]
    fn huge_output_is_fine() {
        let (dir, file) = scratch("mut-huge", "hello\n");
        let script = "head -c 300000 /dev/zero | tr '\\0' x; exit 1";
        assert_eq!(run(&dir, &opts(false, "hello", "bye", script)).unwrap(), 0);
        assert_eq!(fs::read_to_string(file).unwrap(), "hello\n");
    }

    #[test]
    #[cfg_attr(miri, ignore = "runs processes")]
    fn restores_when_the_command_cannot_run() {
        let (dir, file) = scratch("mut-fail", "hello\n");
        let mut o = opts(false, "hello", "bye", "");
        o.command = vec!["/nonexistent/command".into()];
        assert!(run(&dir, &o).is_err());
        assert_eq!(fs::read_to_string(file).unwrap(), "hello\n");
    }

    #[test]
    #[cfg_attr(miri, ignore = "runs processes")]
    fn compile_errors_and_no_tests_are_told_apart() {
        let (dir, file) = scratch("mut-cargo", "hello\n");
        let o = opts(false, "hello", "bye", "echo 'error: could not compile x'; exit 1");
        assert_eq!(run(&dir, &o).unwrap(), 3);
        let o = opts(false, "hello", "bye", "echo 'running 0 tests'; echo 'test result: ok.'");
        assert_eq!(run(&dir, &o).unwrap(), 4);
        assert_eq!(fs::read_to_string(file).unwrap(), "hello\n");
    }
}
