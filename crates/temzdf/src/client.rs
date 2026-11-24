//! Session commands: start a session server, list and end sessions, run
//! actions (see doc/design/session-server.md). Attaching this terminal is in
//! `attach.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use temzdf_protocol::{self as proto, ClientMsg, ServerMsg};
use temzdf_server::daemon;

use temzdf_client::{REQUEST_TIMEOUT, exit_message, utf8};
pub use temzdf_client::{first_pane, request};
// Starting sessions and attaching: the terminal frontend's.
pub use temzdf_client::{auto_name, name_for, resolve};

/// Start a server for session `name` (see [`temzdf_client::spawn_server`])
/// with this executable.
pub fn spawn_server(
    name: &str,
    pane: &[String],
    config_path: Option<&Path>,
    editor: Option<&str>,
    clean: bool,
) -> Result<()> {
    temzdf_client::spawn_server(&std::env::current_exe()?, name, pane, config_path, editor, clean)
}

pub fn list_sessions(verbose: bool) -> Result<()> {
    let daemon::Listing { sessions, other, silent } = daemon::list_all()?;
    if sessions.is_empty() && other.is_empty() && silent.is_empty() {
        println!("no sessions");
    }
    // The session whose pane this runs in, marked as zellij does.
    let current = temzdf_client::current_session();
    // Name and state only: what a session has open, `temzdf eval` asks.
    for s in sessions {
        let clients = if s.clients == 0 { "detached".to_owned() } else { format!("{} attached", s.clients) };
        let mut line = format!("{}  ({clients})", s.name);
        if current.as_deref() == Some(s.name.as_str()) {
            line.push_str(" (current)");
        }
        println!("{line}");
        if verbose {
            println!("    pid: {}", s.pid);
            if let Some(log) = &s.log {
                println!("    log: {}", log.display());
            }
        }
    }
    for name in other {
        println!("{name}  (another temzdf version; end it with that version's `temzdf kill-session {name}`)");
    }
    for name in silent {
        println!("{name}  (not answering: busy)");
    }
    Ok(())
}

/// End session `name`: as its `kill-session` command, which its
/// configuration's SessionEndPre hooks may refuse (unsaved changes), or
/// with `force` at once.
pub fn kill_session(name: &str, force: bool) -> Result<()> {
    if !force {
        return action(name, vec!["kill-session".into()], REQUEST_TIMEOUT)
            .map_err(|e| anyhow::anyhow!("{e:#} (kill-session --force ends it anyway)"));
    }
    match request(name, ClientMsg::Kill, REQUEST_TIMEOUT)? {
        ServerMsg::Done(Ok(())) => Ok(()),
        other => bail!("unexpected answer {other:?}"),
    }
}

/// End every session, those of other versions too when they still take the
/// request; one that fails does not stop the others.
pub fn kill_all_sessions(force: bool) -> Result<()> {
    let daemon::Listing { sessions, other, silent } = daemon::list_all()?;
    let mut failed = Vec::new();
    // Another version's sessions only take the plain request. One that did
    // not answer in time is busy, not another version: asked as asked (its
    // SessionEndPre hooks may keep unsaved text).
    let sessions = sessions
        .into_iter()
        .map(|s| (s.name, force))
        .chain(silent.into_iter().map(|n| (n, force)))
        .chain(other.into_iter().map(|n| (n, true)));
    for (name, force) in sessions {
        if let Err(e) = kill_session(&name, force) {
            failed.push(format!("{name}: {e:#}"));
        }
    }
    if !failed.is_empty() {
        bail!("some sessions did not end:\n{}", failed.join("\n"));
    }
    Ok(())
}

/// `temzdf copy`: record `text` in session `name`'s copy history.
pub fn copy(name: &str, text: String, register: Option<String>, line: bool) -> Result<()> {
    let kind = Some(if line { temzdf_scene::CopyKind::Line } else { temzdf_scene::CopyKind::Char });
    match request(name, ClientMsg::Copy { text, register, kind }, REQUEST_TIMEOUT)? {
        ServerMsg::Done(Ok(())) => Ok(()),
        ServerMsg::Done(Err(e)) => bail!("{e}"),
        other => bail!("unexpected answer {other:?}"),
    }
}

/// `temzdf paste`: an entry of session `name`'s copy history.
pub fn paste(name: &str, register: Option<String>, index: usize) -> Result<String> {
    match request(name, ClientMsg::GetCopy { register, index }, REQUEST_TIMEOUT)? {
        ServerMsg::Copied(Ok(text)) => Ok(text),
        ServerMsg::Copied(Err(e)) => bail!("{e}"),
        other => bail!("unexpected answer {other:?}"),
    }
}

/// Run an action in session `name`. The server runs elsewhere: a new pane's
/// file is made absolute, and its terminal starts in this directory.
pub fn action(name: &str, mut words: Vec<String>, timeout: Duration) -> Result<()> {
    resolve_paths(&mut words, &std::env::current_dir()?)?;
    match request(name, ClientMsg::Action(words), timeout)? {
        ServerMsg::Done(Ok(())) => Ok(()),
        ServerMsg::Done(Err(e)) => bail!("{e}"),
        other => bail!("unexpected answer {other:?}"),
    }
}

/// `temzdf eval`: run `code` in session `name`'s script host, then print
/// what it printed (`console.log` to stdout, `console.error` to stderr)
/// and its value (with `-p`) or its error. Whether it succeeded.
pub fn eval(name: &str, code: proto::script::Code, timeout: Duration) -> Result<bool> {
    use std::io::Write;
    use temzdf_protocol::script::{CodeResult, Printed};
    let CodeResult { printed, result } = match request(name, ClientMsg::Eval(code), timeout)? {
        ServerMsg::Evaluated(r) => r,
        ServerMsg::Exit(reason) => bail!("{}", exit_message(&reason, name)),
        other => bail!("unexpected answer {other:?}"),
    };
    let (mut out, mut err) = (std::io::stdout().lock(), std::io::stderr());
    for p in printed {
        match p {
            Printed::Out(text) => writeln!(out, "{text}")?,
            Printed::Err(text) => {
                out.flush()?;
                writeln!(err, "{text}")?;
            }
        }
    }
    match result {
        Ok(value) => {
            if let Some(v) = value {
                writeln!(out, "{v}")?;
            }
            out.flush()?;
            Ok(true)
        }
        Err(e) => {
            out.flush()?;
            writeln!(err, "{e}")?;
            Ok(false)
        }
    }
}

/// What `temzdf eval` runs: each `-c` a step (`-c 1`, ...), or `file` (`-`:
/// standard input), read here; relative imports start from this
/// directory (a file's from its own).
pub fn eval_code(code: Vec<String>, file: Option<&Path>, print: bool) -> Result<proto::script::Code> {
    use temzdf_protocol::script::{Code, CodeStep};
    let cwd = std::env::current_dir()?;
    let steps = match file {
        Some(f) if f == Path::new("-") => {
            let mut code = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut code).context("reading standard input")?;
            vec![CodeStep { name: "[stdin]".into(), code }]
        }
        Some(f) => {
            let path = std::path::absolute(cwd.join(f))?;
            let code = std::fs::read_to_string(&path).with_context(|| format!("cannot read {}", f.display()))?;
            vec![CodeStep { name: path.display().to_string(), code }]
        }
        None => {
            code.into_iter().enumerate().map(|(i, code)| CodeStep { name: format!("-c {}", i + 1), code }).collect()
        }
    };
    Ok(Code { steps, base: cwd, print })
}

/// Paths in `temzdf action` words are the caller's: resolve them against
/// `cwd` here (the server has its own working directory) and check that
/// PDFs exist (a missing text file is a new one; opening happens later in
/// the server, too late to fail the
/// command). `new-pane`, `open-float` and `new-tab` `[--cwd DIR] [FILE | -- COMMAND...]`
/// get `--cwd` (the caller's directory unless given; relative to it), and
/// FILE is relative to that directory; `edit FILE` is relative to `cwd`.
fn resolve_paths(words: &mut Vec<String>, cwd: &Path) -> Result<()> {
    let existing = |p: PathBuf| -> Result<String> {
        if temzdf_client::must_exist(&p) {
            bail!("{}: no such file", p.display());
        }
        utf8(&p)
    };
    match words.first().map(String::as_str) {
        Some("new-pane" | "open-float" | "new-tab") => {
            if words.get(1).is_none_or(|w| w != "--cwd") {
                words.splice(1..1, ["--cwd".to_owned(), utf8(cwd)?]);
            }
            let Some(dir) = words.get(2).map(|d| cwd.join(d)) else { bail!("--cwd: which directory?") };
            words[2] = utf8(&dir)?;
            if let Some(file) = words.get_mut(3)
                && !file.starts_with('-')
            {
                *file = existing(dir.join(&*file))?;
            }
        }
        Some("edit") => {
            if let Some(file) = words.get_mut(1) {
                *file = existing(cwd.join(&*file))?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_paths_are_the_callers() {
        let tmp = std::env::temp_dir().join(format!("temzdf-paths-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a.pdf"), "").unwrap();
        std::fs::write(tmp.join("sub/b.pdf"), "").unwrap();
        let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let t = |p: &str| tmp.join(p).display().to_string();
        let mut words = w(&["new-pane", "a.pdf"]);
        resolve_paths(&mut words, &tmp).unwrap();
        let here = tmp.display().to_string();
        assert_eq!(words, [w(&["new-pane", "--cwd"]), vec![here.clone(), t("a.pdf")]].concat());
        // FILE is relative to --cwd DIR, itself relative to the caller.
        let mut words = w(&["open-float", "--cwd", "sub", "b.pdf"]);
        resolve_paths(&mut words, &tmp).unwrap();
        let b = tmp.join("sub").join("b.pdf").display().to_string();
        assert_eq!(words, [w(&["open-float", "--cwd"]), vec![t("sub"), b]].concat());
        let mut words = w(&["open-float", "--", "htop"]);
        resolve_paths(&mut words, &tmp).unwrap();
        assert_eq!(words, [w(&["open-float", "--cwd"]), vec![here], w(&["--", "htop"])].concat());
        let mut words = w(&["edit", "sub/b.pdf"]);
        resolve_paths(&mut words, &tmp).unwrap();
        assert_eq!(words, ["edit".to_owned(), t("sub/b.pdf")]);
        // A missing file fails here, not later in the server.
        let e = resolve_paths(&mut w(&["edit", "missing.pdf"]), &tmp).unwrap_err();
        assert!(e.to_string().contains("no such file"), "{e}");
        assert!(resolve_paths(&mut w(&["new-pane", "missing.pdf"]), &tmp).is_err());
        // A missing text file is a new one.
        let mut words = w(&["new-pane", "new.txt"]);
        resolve_paths(&mut words, &tmp).unwrap();
        assert_eq!(words[3], t("new.txt"));
        let mut words = w(&["split", "right"]);
        resolve_paths(&mut words, &tmp).unwrap();
        assert_eq!(words, w(&["split", "right"]));
        // A path that is not Unicode is refused, not mangled.
        assert!(utf8(Path::new(&not_unicode())).is_err());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A path that is not Unicode: a byte that is not UTF-8.
    #[cfg(unix)]
    fn not_unicode() -> std::ffi::OsString {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(b"/tmp/\xff.pdf".to_vec())
    }

    /// A path that is not Unicode: a lone surrogate (Windows' paths are UTF-16).
    #[cfg(windows)]
    fn not_unicode() -> std::ffi::OsString {
        use std::os::windows::ffi::OsStringExt;
        let wide: Vec<u16> = "C:\\a".encode_utf16().chain([0xD800]).chain(".pdf".encode_utf16()).collect();
        std::ffi::OsString::from_wide(&wide)
    }
}
