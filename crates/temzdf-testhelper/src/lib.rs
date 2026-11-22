//! A small program for tests to run in a pty (or as any child) in place of
//! `sh -c '...'`, which is not on every platform the tests run on. Its
//! arguments are a chain of commands, run in order:
//!
//! - `print TEXT`: write TEXT to stdout; the escapes `\e`, `\n`, `\r`,
//!   `\t`, `\\`, `\xNN` and `\NNN` (three octal digits) are decoded, and
//!   nothing is added.
//! - `raw`: switch the terminal on stdin to raw mode without echo (Unix).
//! - `hex`: read stdin until it ends, writing every byte as two hex digits
//!   followed by a space (`68 69 0d `), flushed at once.
//! - `sleep MS`: wait that many milliseconds.
//! - `exit CODE`: exit with that status.
//!
//! The library half is for tests: [`path`] builds the program (once) and
//! says where it is.

use std::io::{Read, Write};
use std::sync::OnceLock;
use std::time::Duration;

/// The helper's executable, built on first use (`cargo build` in this
/// workspace, `--release` when the caller is not a debug build) and found
/// through cargo's JSON messages. (Cargo builds binaries of other packages
/// for a package's tests only as dependencies of its integration tests, so
/// unit tests build it themselves; the lock on the target directory is free
/// while tests run.)
pub fn path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut build = std::process::Command::new(cargo);
        build
            .args(["build", "--quiet", "--message-format=json", "--manifest-path"])
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        if !cfg!(debug_assertions) {
            build.arg("--release");
        }
        build.stderr(std::process::Stdio::inherit());
        let out = build.output().expect("run cargo");
        assert!(out.status.success(), "building temzdf-testhelper failed");
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout.lines().find_map(executable).expect("cargo reported no executable")
    })
}

/// The `"executable":"..."` of one of cargo's JSON messages, unescaped.
fn executable(line: &str) -> Option<String> {
    let rest = &line[line.find("\"executable\":\"")? + "\"executable\":\"".len()..];
    let mut path = String::new();
    let mut chars = rest.chars();
    loop {
        match chars.next()? {
            '"' => return Some(path),
            '\\' => match chars.next()? {
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    path.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                c => path.push(c),
            },
            c => path.push(c),
        }
    }
}

/// Runs the commands of `args` (the program's arguments without its name).
pub fn run(args: &[String]) -> Result<(), String> {
    let mut args = args.iter();
    while let Some(cmd) = args.next() {
        let mut arg = |what: &str| args.next().ok_or_else(|| format!("{cmd}: missing {what}"));
        match cmd.as_str() {
            "print" => {
                let bytes = unescape(arg("TEXT")?)?;
                let mut out = std::io::stdout().lock();
                out.write_all(&bytes).and_then(|()| out.flush()).map_err(|e| e.to_string())?;
            }
            "raw" => raw()?,
            "hex" => hex()?,
            "sleep" => {
                let ms: u64 = arg("MS")?.parse().map_err(|e| format!("sleep: {e}"))?;
                std::thread::sleep(Duration::from_millis(ms));
            }
            "exit" => {
                let code: i32 = arg("CODE")?.parse().map_err(|e| format!("exit: {e}"))?;
                std::process::exit(code);
            }
            other => return Err(format!("unknown command {other:?}")),
        }
    }
    Ok(())
}

/// Decodes the escapes of `print`.
pub fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let digits = |from: usize, n: usize, radix: u32| -> Result<u8, String> {
            let s = text.get(from..from + n).ok_or("short escape")?;
            u8::from_str_radix(s, radix).map_err(|e| format!("bad escape {s:?}: {e}"))
        };
        match b.get(i + 1).copied().ok_or("trailing backslash")? {
            b'e' => (out.push(0x1b), i += 2),
            b'n' => (out.push(b'\n'), i += 2),
            b'r' => (out.push(b'\r'), i += 2),
            b't' => (out.push(b'\t'), i += 2),
            b'\\' => (out.push(b'\\'), i += 2),
            b'x' => (out.push(digits(i + 2, 2, 16)?), i += 4),
            b'0'..=b'3' => (out.push(digits(i + 1, 3, 8)?), i += 4),
            c => return Err(format!("unknown escape \\{}", c as char)),
        };
    }
    Ok(out)
}

fn hex() -> Result<(), String> {
    let mut stdin = std::io::stdin().lock();
    let mut out = std::io::stdout().lock();
    let mut buf = [0u8; 256];
    loop {
        let n = stdin.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        let text: String = buf[..n].iter().map(|b| format!("{b:02x} ")).collect();
        out.write_all(text.as_bytes()).and_then(|()| out.flush()).map_err(|e| e.to_string())?;
    }
}

#[cfg(unix)]
fn raw() -> Result<(), String> {
    use rustix::termios::{OptionalActions, tcgetattr, tcsetattr};
    let stdin = rustix::stdio::stdin();
    let mut t = tcgetattr(stdin).map_err(|e| format!("raw: {e}"))?;
    t.make_raw();
    tcsetattr(stdin, OptionalActions::Now, &t).map_err(|e| format!("raw: {e}"))
}

#[cfg(not(unix))]
fn raw() -> Result<(), String> {
    Err("raw: not implemented on this platform yet".into())
}

#[cfg(test)]
mod tests {
    use super::{executable, unescape};

    #[test]
    fn the_executable_is_read_from_cargos_message() {
        let line = r#"{"reason":"compiler-artifact","executable":"C:\\t\\h.exe","fresh":true}"#;
        assert_eq!(executable(line).as_deref(), Some(r"C:\t\h.exe"));
        assert_eq!(executable(r#"{"executable":null}"#), None);
    }

    #[test]
    fn escapes_are_decoded() {
        assert_eq!(unescape(r"a\e[31m\n\x41\346\225\260\\").unwrap(), b"a\x1b[31m\nA\xe6\x95\xb0\\");
        assert!(unescape(r"\q").is_err());
        assert!(unescape(r"\x4").is_err());
        assert!(unescape("\\").is_err());
    }
}
