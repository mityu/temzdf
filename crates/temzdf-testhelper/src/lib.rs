//! A small program for tests to run in a pty (or as any child) in place of
//! `sh -c '...'`, which is not on every platform the tests run on. Its
//! arguments are a chain of commands, run in order:
//!
//! - `print TEXT`: write TEXT to stdout; the escapes `\e`, `\n`, `\r`,
//!   `\t`, `\\`, `\xNN` and `\NNN` (three octal digits) are decoded, and
//!   nothing is added.
//! - `lines N`: write the numbers 1 to N, one a line (as `seq 1 N`).
//! - `size`: write the terminal's size, `ROWS COLS` (as `stty size`).
//! - `raw`: switch the terminal on stdin to raw mode without echo (on
//!   Windows: the console's input without line editing, echo and Ctrl+C
//!   handling, and with VT sequences for keys, as a terminal sends them).
//! - `read-line`: read a line from stdin (ended by CR, LF or CR LF).
//! - `said`: write the line read last as `said:LINE`, its control bytes as
//!   `\xNN`.
//! - `each-line ACTION`: until stdin ends, read a line and run ACTION after
//!   each, which is `size`, `said` or `print TEXT`.
//! - `hex`: read stdin until it ends, writing every byte as two hex digits
//!   followed by a space (`68 69 0d `), flushed at once.
//! - `spawn COMMANDS... ;`: start the program again with these commands,
//!   on the same stdin, stdout and stderr (or terminal), and go on without
//!   waiting for it (as `sh`'s `&`).
//! - `sleep MS`: wait that many milliseconds.
//! - `exit CODE`: exit with that status.
//!
//! The lines `lines`, `size` and `said` write end with CR LF, so that they
//! show as lines also after `raw`.
//!
//! On Windows the program turns on VT sequences for its console's output
//! (`ENABLE_VIRTUAL_TERMINAL_PROCESSING`) first, so that escapes printed
//! reach a pseudoconsole's terminal as sequences.
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
    vt_output();
    Helper::default().run(args)
}

/// The state commands share: the line read last.
#[derive(Default)]
struct Helper {
    line: Vec<u8>,
    /// The last byte read ended a line with CR (an LF right after it
    /// belongs to that line's end).
    after_cr: bool,
}

impl Helper {
    fn run(&mut self, args: &[String]) -> Result<(), String> {
        let mut args = args.iter();
        while let Some(cmd) = args.next() {
            let mut arg = |what: &str| args.next().ok_or_else(|| format!("{cmd}: missing {what}"));
            match cmd.as_str() {
                "print" => write(&unescape(arg("TEXT")?)?)?,
                "lines" => {
                    let n: u64 = arg("N")?.parse().map_err(|e| format!("lines: {e}"))?;
                    write((1..=n).map(|i| format!("{i}\r\n")).collect::<String>().as_bytes())?;
                }
                "size" => {
                    let (rows, cols) = size()?;
                    write(format!("{rows} {cols}\r\n").as_bytes())?;
                }
                "raw" => raw()?,
                "read-line" => {
                    self.read_line()?;
                }
                "said" => write(&said(&self.line))?,
                "each-line" => {
                    let action: Vec<String> = match arg("ACTION")?.as_str() {
                        "print" => vec!["print".into(), arg("TEXT")?.clone()],
                        a @ ("size" | "said") => vec![a.into()],
                        other => return Err(format!("each-line: unknown action {other:?}")),
                    };
                    while self.read_line()? {
                        self.run(&action)?;
                    }
                }
                "hex" => hex()?,
                "spawn" => {
                    let mut commands = Vec::new();
                    loop {
                        match arg("`;`")? {
                            end if end == ";" => break,
                            c => commands.push(c.clone()),
                        }
                    }
                    let exe = std::env::current_exe().map_err(|e| format!("spawn: {e}"))?;
                    std::process::Command::new(exe).args(commands).spawn().map_err(|e| format!("spawn: {e}"))?;
                }
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

    /// Reads a line into `line`; false when stdin ended before one began.
    fn read_line(&mut self) -> Result<bool, String> {
        self.line.clear();
        let mut stdin = std::io::stdin().lock();
        let mut b = [0u8; 1];
        loop {
            if stdin.read(&mut b).map_err(|e| format!("read-line: {e}"))? == 0 {
                return Ok(!self.line.is_empty());
            }
            match b[0] {
                b'\n' if std::mem::take(&mut self.after_cr) => {}
                end @ (b'\r' | b'\n') => {
                    self.after_cr = end == b'\r';
                    return Ok(true);
                }
                c => {
                    self.after_cr = false;
                    self.line.push(c);
                }
            }
        }
    }
}

/// Writes and flushes `bytes` to stdout.
fn write(bytes: &[u8]) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    out.write_all(bytes).and_then(|()| out.flush()).map_err(|e| e.to_string())
}

/// `said:LINE` and CR LF, with LINE's control bytes as `\xNN`.
fn said(line: &[u8]) -> Vec<u8> {
    let mut out = b"said:".to_vec();
    for &b in line {
        if b < 0x20 || b == 0x7f {
            out.extend_from_slice(format!("\\x{b:02x}").as_bytes());
        } else {
            out.push(b);
        }
    }
    out.extend_from_slice(b"\r\n");
    out
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
    let mut buf = [0u8; 256];
    loop {
        let n = stdin.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        let text: String = buf[..n].iter().map(|b| format!("{b:02x} ")).collect();
        write(text.as_bytes())?;
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

#[cfg(unix)]
fn size() -> Result<(u16, u16), String> {
    let w = rustix::termios::tcgetwinsize(std::io::stdout()).map_err(|e| format!("size: {e}"))?;
    Ok((w.ws_row, w.ws_col))
}

#[cfg(unix)]
fn vt_output() {}

#[cfg(windows)]
fn raw() -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode,
        SetConsoleMode,
    };
    let stdin = std::io::stdin().as_raw_handle();
    let mut mode = 0;
    // SAFETY: stdin's handle, valid while the program runs, and a mode to
    // write into.
    if unsafe { GetConsoleMode(stdin, &mut mode) } == 0 {
        return Err(format!("raw: {}", std::io::Error::last_os_error()));
    }
    mode &= !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
    mode |= ENABLE_VIRTUAL_TERMINAL_INPUT;
    // SAFETY: as above.
    if unsafe { SetConsoleMode(stdin, mode) } == 0 {
        return Err(format!("raw: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(windows)]
fn size() -> Result<(u16, u16), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo};
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: stdout's handle, valid while the program runs, and a
    // structure to write into.
    if unsafe { GetConsoleScreenBufferInfo(std::io::stdout().as_raw_handle(), &mut info) } == 0 {
        return Err(format!("size: {}", std::io::Error::last_os_error()));
    }
    let w = info.srWindow;
    Ok(((w.Bottom - w.Top + 1) as u16, (w.Right - w.Left + 1) as u16))
}

/// Turns VT sequences on for stdout when it is a console (nothing when it
/// is not).
#[cfg(windows)]
fn vt_output() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, SetConsoleMode};
    let stdout = std::io::stdout().as_raw_handle();
    let mut mode = 0;
    // SAFETY: stdout's handle, valid while the program runs, and a mode to
    // write into.
    if unsafe { GetConsoleMode(stdout, &mut mode) } != 0 {
        // SAFETY: as above.
        unsafe { SetConsoleMode(stdout, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) };
    }
}

#[cfg(test)]
mod tests {
    use super::{executable, path, said, unescape};
    use std::io::Write;
    use std::process::{Command, Stdio};

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

    #[test]
    fn said_shows_control_bytes_as_escapes() {
        assert_eq!(said(b"\x02a\x7f\xc3\xa9"), b"said:\\x02a\\x7f\xc3\xa9\r\n");
    }

    /// The helper's output and exit status with `input` on its stdin.
    fn helper(args: &[&str], input: &[u8]) -> (String, Option<i32>) {
        let mut child = Command::new(path())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start the helper");
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code())
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start processes")]
    fn lines_print_and_exit() {
        assert_eq!(helper(&["lines", "3", "print", "x\\n", "exit", "3"], b""), ("1\r\n2\r\n3\r\nx\n".into(), Some(3)));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start processes")]
    fn lines_read_end_with_cr_lf_or_both() {
        let input = b"one\r\ntwo\rthree\n\nfour";
        assert_eq!(
            helper(&["each-line", "said"], input).0,
            "said:one\r\nsaid:two\r\nsaid:three\r\nsaid:\r\nsaid:four\r\n"
        );
        assert_eq!(helper(&["each-line", "print", "-"], b"a\nb\n").0, "--");
        assert_eq!(helper(&["read-line", "read-line", "said", "print", "!"], b"a\nb\nc\n").0, "said:b\r\n!");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start processes")]
    fn spawned_commands_run_without_being_waited_for() {
        // The child writes after the parent ended, into the same pipe (read
        // until every writer closed it).
        let (out, code) = helper(&["spawn", "sleep", "300", "print", "child", ";", "print", "parent"], b"");
        assert_eq!((out.as_str(), code), ("parentchild", Some(0)));
        assert_eq!(helper(&["spawn", "print", "x"], b"").1, Some(2), "no `;`");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot start processes")]
    fn size_needs_a_terminal() {
        assert_eq!(helper(&["size"], b"").1, Some(2));
    }
}
