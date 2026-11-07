//! Swap files (doc/design/text-editor.md section 13): a text buffer's text
//! kept beside the session while it is edited, so that what was not saved
//! survives a crash. Named as Neovim names them (the path with `/` as `%`,
//! `.swp`, then `.swo`, `.swn`, ... when taken), in the session's swap
//! directory; JSON with the owner (session, pid, host) and the whole text.
//! Written when the file is opened, `o.updatetime` after the last change,
//! and when the file is written; removed with the buffer and when the
//! session ends normally (a crash leaves it for recovering).

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use temzdf_protocol::script::ScriptValue;
use temzdf_texteditor::{StepKind, TextEdit};

use super::notices;
use super::{BufferId, Outcome, PaneId, Session};

const VERSION: u32 = 1;

/// What a swap file holds.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct SwapFile {
    pub version: u32,
    /// The file being edited.
    pub path: String,
    /// The session editing it, its server's pid and the host.
    pub session: String,
    pub pid: u32,
    pub host: String,
    /// When it was written (milliseconds since the Unix epoch).
    pub time: u64,
    /// The text differs from the file's.
    pub modified: bool,
    pub encoding: String,
    #[serde(rename = "lineEnding")]
    pub line_ending: String,
    pub text: String,
}

/// A buffer's own swap file, and when it is next due (changes not written
/// to it yet).
#[derive(Clone, Debug)]
pub(super) struct Swap {
    pub path: PathBuf,
    pub due: Option<Instant>,
}

/// A swap file found when the file was opened, of a session that is gone:
/// waiting for a decision (`SwapExists`; `useSwap`, `text.recover`,
/// `deleteSwap`).
#[derive(Clone, Debug)]
pub(super) struct Found {
    pub path: PathBuf,
    pub file: SwapFile,
    /// The file is newer than the swap file.
    pub newer: bool,
}

/// Where swap files go: `$XDG_STATE_HOME/temzdf/swap`, else
/// `~/.local/state/temzdf/swap` (the clients' state directory).
pub(super) fn swap_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("temzdf").join("swap"))
}

/// The names a swap file of `file` may have in `dir`, in the order taken:
/// `.swp`, `.swo`, `.swn`, ... `.swa` (as Vim's).
pub(super) fn candidates(dir: &Path, file: &Path) -> Vec<PathBuf> {
    let base = file.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "%");
    ('a'..='p').rev().map(|c| dir.join(format!("{base}.sw{c}"))).collect()
}

/// This host's name (an owner on another host cannot be checked).
pub(super) fn host() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_default()
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

pub(super) fn read(path: &Path) -> Result<SwapFile, String> {
    let json = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let f: SwapFile = serde_json::from_str(&json).map_err(|e| format!("{}: not a swap file: {e}", path.display()))?;
    if f.version != VERSION {
        return Err(format!("{}: swap file version {} (this reads {VERSION})", path.display(), f.version));
    }
    Ok(f)
}

/// Write `f` to `path`: readable by the user only (unsaved text is in it),
/// replaced whole.
pub(super) fn write(path: &Path, f: &SwapFile) -> Result<(), String> {
    use std::io::Write;
    let dir = path.parent().expect("in a directory");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let json = serde_json::to_string(f).map_err(|e| e.to_string())?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    let written = open.open(&tmp).and_then(|mut w| w.write_all(json.as_bytes()).and_then(|()| w.sync_all()));
    written.and_then(|()| std::fs::rename(&tmp, path)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// The commands of swap files.
pub(super) const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "swap-info",
        "(buffer?: number): SwapInfo | null",
        "The swap file found when the buffer's file was opened, of a session that is gone (SwapExists): null when none is waiting",
    ),
    (
        "use-swap",
        "(buffer?: number): string",
        "Start the buffer's own swap file (its first free name; after SwapExists was answered); returns its path",
    ),
    ("delete-swap", "(path: string): void", "Delete a swap file (one in the swap directory)"),
    (
        "text.recover",
        "(opts?: { swap?: string, pane?: number }): void",
        "Replace the text with a swap file's (the one found, by default), as one change that undo takes back",
    ),
];

impl Session {
    /// The buffer's file was opened: its own swap file, or the swap file of
    /// a session that is gone (`SwapExists`; nobody hearing it: edited
    /// anyway, as Neovim's E). One of a session still running is passed
    /// over (Neovim's W325).
    pub(super) fn swap_opened(&mut self, buf: BufferId) {
        let Some(t) = self.texts.get(&buf) else { return };
        let Some(dir) = self.swap_dir.clone().filter(|_| self.options.swapfile && !t.has("nofile")) else { return };
        let Some(file) = t.file.as_ref().map(|f| f.path.clone()) else { return };
        let mut stale = None;
        let mut running = None;
        for c in candidates(&dir, &file) {
            if !c.exists() {
                continue;
            }
            match read(&c) {
                Ok(f) if self.owner_running(&f) => {
                    running.get_or_insert(f);
                }
                Ok(f) => {
                    stale.get_or_insert((c, f));
                }
                Err(e) => eprintln!("temzdf: {e}"),
            }
        }
        if let Some((path, f)) = stale {
            let mtime = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
            let newer = mtime
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|m| m.as_millis() as u64 > f.time);
            let found = Found { path, file: f, newer };
            let info = info_value(&found);
            self.texts.get_mut(&buf).expect("found").found = Some(found);
            if self.script.hooks_on("SwapExists") {
                self.event("SwapExists", vec![ScriptValue::Num(buf as f64), info]);
                return;
            }
            let shown = info.get("swap").and_then(ScriptValue::as_str).unwrap_or_default().to_owned();
            self.notify(
                notices::Kind::Info,
                &format!(
                    "found the swap file {shown} of a session that is gone: editing anyway (text.recover() recovers it)"
                ),
            );
        } else if let Some(f) = running {
            self.notify(
                notices::Kind::Info,
                &format!("Ignoring the swap file of session {} (pid {})", f.session, f.pid),
            );
        }
        if let Err(e) = self.use_swap(buf) {
            self.message(&format!("swap file: {e}"));
        }
    }

    /// Whether the session that wrote `f` still runs (this one too).
    fn owner_running(&self, f: &SwapFile) -> bool {
        if f.session == self.name && f.pid == std::process::id() {
            return true;
        }
        if !f.host.is_empty() && f.host != host() {
            // Another host's: it cannot be told; asked about as gone.
            return false;
        }
        temzdf_protocol::socket_path(&f.session)
            .ok()
            .and_then(|s| temzdf_protocol::discovery::probe(&s))
            .is_some_and(|i| i.pid == f.pid)
    }

    /// Start `buf`'s own swap file at the first free name.
    pub(super) fn use_swap(&mut self, buf: BufferId) -> Result<PathBuf, String> {
        let dir = self.swap_dir.clone().ok_or("no state directory (HOME and XDG_STATE_HOME unset)")?;
        let t = self.texts.get_mut(&buf).ok_or_else(|| format!("no text buffer {buf}"))?;
        let file = t.file.as_ref().map(|f| f.path.clone()).ok_or("the buffer has no file")?;
        t.found = None;
        if let Some(s) = &t.swap {
            return Ok(s.path.clone());
        }
        let path = candidates(&dir, &file).into_iter().find(|c| !c.exists()).ok_or("every swap file name is taken")?;
        t.swap = Some(Swap { path: path.clone(), due: None });
        self.write_swap(buf)?;
        Ok(path)
    }

    /// Write `buf`'s swap file now.
    pub(super) fn write_swap(&mut self, buf: BufferId) -> Result<(), String> {
        let Some(t) = self.texts.get_mut(&buf) else { return Ok(()) };
        if t.swap.is_none() {
            return Ok(());
        }
        let f = SwapFile {
            version: VERSION,
            path: t.file.as_ref().map(|f| f.path.to_string_lossy().into_owned()).unwrap_or_default(),
            session: self.name.clone(),
            pid: std::process::id(),
            host: host(),
            time: now_ms(),
            modified: t.modified(),
            encoding: t.file.as_ref().map_or("utf-8".into(), |f| f.encoding.clone()),
            line_ending: if t.buf.line_ending == temzdf_texteditor::LineEnding::CrLf { "crlf" } else { "lf" }.into(),
            text: t.buf.text().to_string(),
        };
        let s = t.swap.as_mut().expect("checked");
        s.due = None;
        write(&s.path, &f)
    }

    /// `buf` changed: its swap file is due `o.updatetime` from now.
    pub(super) fn swap_changed(&mut self, buf: BufferId) {
        let due = Instant::now() + self.options.updatetime;
        if let Some(s) = self.texts.get_mut(&buf).and_then(|t| t.swap.as_mut()) {
            s.due = Some(due);
        }
    }

    pub(super) fn swap_deadline(&self) -> Option<Instant> {
        self.texts.values().filter_map(|t| t.swap.as_ref()?.due).min()
    }

    /// Write the swap files due by `now`.
    pub(super) fn write_due_swaps(&mut self, now: Instant) {
        let due: Vec<BufferId> = self
            .texts
            .iter()
            .filter(|(_, t)| t.swap.as_ref().and_then(|s| s.due).is_some_and(|d| d <= now))
            .map(|(b, _)| *b)
            .collect();
        for buf in due {
            if let Err(e) = self.write_swap(buf) {
                eprintln!("temzdf: swap file: {e}");
            }
        }
    }

    /// `buf`'s swap file goes (the buffer is deleted).
    pub(super) fn drop_swap(&mut self, buf: BufferId) {
        if let Some(s) = self.texts.get_mut(&buf).and_then(|t| t.swap.take()) {
            let _ = std::fs::remove_file(&s.path);
        }
    }

    /// The session ends normally: every swap file goes (a crash leaves them
    /// for recovering).
    pub fn end_swaps(&mut self) {
        let bufs: Vec<BufferId> = self.texts.keys().copied().collect();
        for buf in bufs {
            self.drop_swap(buf);
        }
    }

    /// `swap-info`, `use-swap`, `delete-swap`, `text.recover`.
    pub(super) fn swap_command(&mut self, name: &str, args: &[ScriptValue]) -> Result<(ScriptValue, Outcome), String> {
        let buffer = |s: &Session| -> Result<BufferId, String> {
            match args.first().and_then(ScriptValue::as_num) {
                Some(b) => Ok(b as BufferId),
                None => s.text_target(None).map(|(_, b)| b),
            }
        };
        match name {
            "swap-info" => {
                let buf = buffer(self)?;
                let t = self.texts.get(&buf).ok_or_else(|| format!("no text buffer {buf}"))?;
                Ok((t.found.as_ref().map_or(ScriptValue::Null, info_value), Outcome::None))
            }
            "use-swap" => {
                let buf = buffer(self)?;
                let path = self.use_swap(buf)?;
                Ok((ScriptValue::Str(path.to_string_lossy().into_owned()), Outcome::None))
            }
            "delete-swap" => {
                let path = PathBuf::from(args.first().and_then(ScriptValue::as_str).ok_or("which swap file?")?);
                let dir = self.swap_dir.clone().ok_or("no swap directory")?;
                let ours = path.parent() == Some(dir.as_path())
                    && path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.len() == 3 && e.starts_with("sw"));
                if !ours {
                    return Err(format!("{} is not a swap file in {}", path.display(), dir.display()));
                }
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                for t in self.texts.values_mut() {
                    if t.found.as_ref().is_some_and(|f| f.path == path) {
                        t.found = None;
                    }
                }
                Ok((ScriptValue::Null, Outcome::None))
            }
            _ => {
                // text.recover
                let opts = args.first();
                let pane = opts.and_then(|o| o.get("pane")).and_then(ScriptValue::as_num).map(|p| p as PaneId);
                let (pane, buf) = self.text_target(pane)?;
                let path = match opts.and_then(|o| o.get("swap")).and_then(ScriptValue::as_str) {
                    Some(p) => PathBuf::from(p),
                    None => {
                        self.texts[&buf].found.as_ref().map(|f| f.path.clone()).ok_or("no swap file found for it")?
                    }
                };
                let f = read(&path)?;
                let len = self.texts[&buf].buf.text().len_chars();
                self.text_apply(pane, buf, vec![TextEdit::new(0, len, &f.text)], StepKind::Other)?;
                Ok((ScriptValue::Null, Outcome::Redraw))
            }
        }
    }
}

/// What `SwapExists` and `swapInfo` tell of a swap file found.
fn info_value(f: &Found) -> ScriptValue {
    let n = |v: u64| ScriptValue::Num(v as f64);
    ScriptValue::map([
        ("swap", ScriptValue::Str(f.path.to_string_lossy().into_owned())),
        ("session", ScriptValue::Str(f.file.session.clone())),
        ("pid", n(u64::from(f.file.pid))),
        ("host", ScriptValue::Str(f.file.host.clone())),
        ("time", n(f.file.time)),
        ("modified", ScriptValue::Bool(f.file.modified)),
        ("newer", ScriptValue::Bool(f.newer)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_neovims_and_go_on_when_taken() {
        let c = candidates(Path::new("/s"), Path::new("/home/me/notes.txt"));
        assert_eq!(c[0], Path::new("/s/%home%me%notes.txt.swp"));
        assert_eq!(c[1], Path::new("/s/%home%me%notes.txt.swo"));
        assert_eq!(c.last().unwrap(), Path::new("/s/%home%me%notes.txt.swa"));
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn swap_files_round_trip_readable_by_the_user_only() {
        let dir = std::env::temp_dir().join(format!("temzdf-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = SwapFile {
            version: VERSION,
            path: "/x".into(),
            session: "s".into(),
            pid: 1,
            host: "h".into(),
            time: 2,
            modified: true,
            encoding: "utf-8".into(),
            line_ending: "lf".into(),
            text: "数式\n".into(),
        };
        let path = dir.join("a.swp");
        write(&path, &f).unwrap();
        assert_eq!(read(&path).unwrap(), f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(&path, "{").unwrap();
        assert!(read(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
