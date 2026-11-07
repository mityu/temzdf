//! Registers and the copy history (doc/design/registers.md): what panes,
//! scripts and `temzdf copy` copy, newest first, named registers, putting an
//! entry into a pane, and reading the client's clipboard.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime};

use temzdf_protocol::script::ScriptValue;
use temzdf_scene::{CopyKind, Effect};

use super::{BufferId, Kind, Outcome, Session};
use crate::layout::PaneId;

/// How long a clipboard read waits for the client.
const CLIPBOARD_WAIT: Duration = Duration::from_secs(10);

/// Whether an entry was copied or taken out of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum How {
    Copy,
    Delete,
}

/// Where an entry came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Source {
    /// A buffer's kind (`terminal`, `pdf`, `edit`, `input`), or `cli`.
    pub kind: String,
    pub name: String,
    pub buffer: Option<BufferId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub id: u64,
    pub text: String,
    pub kind: CopyKind,
    pub how: How,
    pub register: Option<String>,
    pub source: Source,
    pub time: SystemTime,
}

/// A copy to record.
pub(super) struct NewCopy {
    pub text: String,
    pub kind: CopyKind,
    pub how: How,
    pub register: Option<String>,
    /// Add to the register's entry instead of replacing it (Vim's `"A`).
    pub append: bool,
    /// Put it on the client's clipboard; `None`: as `o.clipboard` says.
    pub clipboard: Option<bool>,
    pub source: Source,
}

/// What goes to the client's clipboard (`o.clipboard`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardSync {
    /// Copies and deletes (Vim's `clipboard=unnamedplus`).
    All,
    Copies,
    None,
}

impl ClipboardSync {
    pub fn name(self) -> &'static str {
        match self {
            ClipboardSync::All => "all",
            ClipboardSync::Copies => "copies",
            ClipboardSync::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "all" => ClipboardSync::All,
            "copies" => ClipboardSync::Copies,
            "none" => ClipboardSync::None,
            _ => return None,
        })
    }
}

#[derive(Default)]
pub(super) struct Copies {
    /// Newest first.
    history: VecDeque<Entry>,
    named: BTreeMap<String, Entry>,
    next_id: u64,
    /// Clipboard reads asked of the client, by ticket: when they give up.
    reads: HashMap<u64, Instant>,
    next_ticket: u64,
}

impl Copies {
    pub fn register(&self, name: &str) -> Option<&Entry> {
        self.named.get(name)
    }

    /// When the next clipboard read gives up.
    pub fn deadline(&self) -> Option<Instant> {
        self.reads.values().min().copied()
    }
}

/// Lines (`CopyKind::Line`) end with a line break.
fn normalized(text: String, kind: CopyKind) -> String {
    if kind == CopyKind::Line && !text.ends_with('\n') { text + "\n" } else { text }
}

impl Entry {
    pub fn value(&self) -> ScriptValue {
        let time = self.time.duration_since(SystemTime::UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64);
        ScriptValue::map([
            ("id", ScriptValue::Num(self.id as f64)),
            ("text", ScriptValue::Str(self.text.clone())),
            ("kind", ScriptValue::Str(self.kind.name().into())),
            (
                "how",
                ScriptValue::Str(
                    match self.how {
                        How::Copy => "copy",
                        How::Delete => "delete",
                    }
                    .into(),
                ),
            ),
            ("register", self.register.clone().map_or(ScriptValue::Null, ScriptValue::Str)),
            (
                "source",
                ScriptValue::map([
                    ("kind", ScriptValue::Str(self.source.kind.clone())),
                    ("name", ScriptValue::Str(self.source.name.clone())),
                    ("buffer", self.source.buffer.map_or(ScriptValue::Null, |b| ScriptValue::Num(b as f64))),
                ]),
            ),
            ("time", ScriptValue::Num(time)),
        ])
    }
}

/// The commands of this module: (name, TypeScript signature, documentation).
pub(super) const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "copy-text",
        "(text: string, opts?: { kind?: CopyKind; how?: \"copy\" | \"delete\"; register?: string; append?: boolean; clipboard?: boolean }): Copied",
        "Record `text` in the copy history (doc/design/registers.md): as characters, lines or a block, copied or deleted, to a named `register` (`append` adds to its text); it goes to the client's clipboard as o.clipboard says (`clipboard` overrides; never for a named register unless true); returns the entry",
    ),
    (
        "history",
        "(opts?: { limit?: number }): Copied[]",
        "The copy history, newest first (at most o.copyhistory entries)",
    ),
    ("register", "(name: string): Copied | null", "The entry of a named register"),
    (
        "set-register",
        "(name: string, entry: Copied | { text: string; kind?: CopyKind } | null): void",
        "Set a named register to an entry (of the history, or text) without recording a copy; null empties it",
    ),
    ("registers", "(): Record<string, Copied>", "The named registers"),
    ("forget-copy", "(id?: number): void", "Drop an entry from the history (and registers); no id: everything"),
    (
        "put",
        "(opts?: { register?: string; index?: number; pane?: number }): void",
        "Put an entry (a named register's, else the history's `index`, 0 the newest) into a pane (the focused one): pasted into a terminal, typed into a text or input pane (lines go in before the cursor's line)",
    ),
    (
        "read-clipboard",
        "(opts?: { primary?: boolean }): Promise<string>",
        "Read the client's clipboard (the primary selection): a terminal client asks the terminal (OSC 52), which may refuse or not answer (an error after 10 s); await it in a dispatched task",
    ),
];

impl Session {
    /// The source of a copy made in `buf` (the focused pane's buffer for
    /// `None`).
    pub(super) fn copy_source(&self, buf: Option<BufferId>) -> Source {
        let buf = buf.or_else(|| self.panes.get(&self.focus).map(super::PaneEntry::buffer));
        let found = buf.and_then(|b| {
            if let Some(t) = self.terms.get(&b) {
                return Some(("terminal", t.term.title().unwrap_or_else(|| "shell".into())));
            }
            if let Some(t) = self.texts.get(&b) {
                return Some(("edit", t.name.clone()));
            }
            if let Some(i) = self.inputs.get(&b) {
                return Some(("input", i.prompt.clone()));
            }
            self.docs.get(&b).map(|d| ("pdf", d.name.clone()))
        });
        match found {
            Some((kind, name)) => Source { kind: kind.into(), name, buffer: buf },
            None => Source { kind: "script".into(), name: String::new(), buffer: None },
        }
    }

    /// Record a copy: in the history, in its register, on the client's
    /// clipboard as asked; hooks hear `Copied`.
    pub(super) fn record_copy(&mut self, c: NewCopy) -> Entry {
        let mut text = normalized(c.text, c.kind);
        let mut kind = c.kind;
        if c.append
            && let Some(old) = c.register.as_ref().and_then(|r| self.copies.named.get(r))
        {
            if old.kind == CopyKind::Line || kind == CopyKind::Line {
                text = normalized(old.text.clone(), CopyKind::Line) + &text;
                kind = CopyKind::Line;
            } else {
                text = old.text.clone() + &text;
                kind = old.kind;
            }
        }
        self.copies.next_id += 1;
        let entry = Entry {
            id: self.copies.next_id,
            text,
            kind,
            how: c.how,
            register: c.register.clone(),
            source: c.source,
            time: SystemTime::now(),
        };
        if let Some(r) = &c.register {
            self.copies.named.insert(r.clone(), entry.clone());
        }
        self.copies.history.push_front(entry.clone());
        self.copies.history.truncate(self.options.copyhistory);
        let to_clipboard = c.clipboard.unwrap_or_else(|| {
            c.register.is_none()
                && match self.options.clipboard {
                    ClipboardSync::All => true,
                    ClipboardSync::Copies => c.how == How::Copy,
                    ClipboardSync::None => false,
                }
        });
        if to_clipboard {
            self.effects.push(Effect::Copy(entry.text.clone()));
        }
        self.event("Copied", vec![entry.value()]);
        entry
    }

    /// What panes copied since the last call goes into the history; their
    /// other effects stay for the client.
    pub(super) fn collect_copies(&mut self) {
        let mut copied: Vec<(BufferId, String, CopyKind)> = Vec::new();
        let mut others = Vec::new();
        for (id, t) in &mut self.terms {
            for e in t.term.take_effects() {
                match e {
                    Effect::Copied { text, kind } => copied.push((*id, text, kind)),
                    e => others.push(e),
                }
            }
        }
        for e in self.panes.values_mut() {
            if let Kind::Pdf { pane, doc } = &mut e.kind {
                for req in pane.take_requests() {
                    if let Some(host) = self.docs.get_mut(doc).and_then(|d| d.host.as_mut())
                        && let Some(effect) = host.request(req)
                    {
                        match effect {
                            Effect::Copied { text, kind } => copied.push((*doc, text, kind)),
                            e => others.push(e),
                        }
                    }
                }
            }
        }
        self.effects.extend(others);
        for (buf, text, kind) in copied {
            let source = self.copy_source(Some(buf));
            self.record_copy(NewCopy {
                text,
                kind,
                how: How::Copy,
                register: None,
                append: false,
                clipboard: None,
                source,
            });
        }
    }

    /// `temzdf copy`.
    pub fn cli_copy(&mut self, text: String, register: Option<String>, kind: Option<CopyKind>) -> Outcome {
        let kind = kind.unwrap_or(CopyKind::Char);
        let source = Source { kind: "cli".into(), name: String::new(), buffer: None };
        self.record_copy(NewCopy { text, kind, how: How::Copy, register, append: false, clipboard: None, source });
        self.flush_events()
    }

    /// `temzdf paste`: a register's text, else the history's entry `index`.
    pub fn cli_get_copy(&self, register: Option<&str>, index: usize) -> Result<String, String> {
        match register {
            Some(r) => {
                self.copies.register(r).map(|e| e.text.clone()).ok_or_else(|| format!("register {r:?} is empty"))
            }
            None => self
                .copies
                .history
                .get(index)
                .map(|e| e.text.clone())
                .ok_or_else(|| if index == 0 { "nothing copied yet".into() } else { format!("no entry {index}") }),
        }
    }

    /// The client answered a clipboard read.
    pub fn clipboard_answer(&mut self, ticket: u64, result: Result<String, String>) -> Outcome {
        if self.copies.reads.remove(&ticket).is_some() {
            self.resolve_promise(ticket, result.map(ScriptValue::Str));
        }
        Outcome::None
    }

    /// Clipboard reads the client did not answer in time fail.
    pub(super) fn expire_clipboard_reads(&mut self, now: Instant) {
        let late: Vec<u64> = self.copies.reads.iter().filter(|(_, at)| **at <= now).map(|(t, _)| *t).collect();
        for t in late {
            self.copies.reads.remove(&t);
            self.resolve_promise(t, Err("the clipboard could not be read: the client did not answer".into()));
        }
    }

    pub(super) fn copies_deadline(&self) -> Option<Instant> {
        self.copies.deadline()
    }

    /// The commands of [`COMMANDS`]; `None` for other names.
    pub(super) fn register_command(
        &mut self,
        name: &str,
        args: &[ScriptValue],
    ) -> Option<Result<(ScriptValue, Outcome), String>> {
        if !COMMANDS.iter().any(|(n, _, _)| *n == name) {
            return None;
        }
        // What panes copied just now (a visual mode's yank) is in.
        self.collect_copies();
        let opts = args.iter().find(|a| matches!(a, ScriptValue::Map(_)));
        let opt_str = |k: &str| opts.and_then(|o| o.get(k)).and_then(ScriptValue::as_str).map(str::to_owned);
        let opt_bool = |k: &str| match opts.and_then(|o| o.get(k)) {
            Some(ScriptValue::Bool(b)) => Some(*b),
            _ => None,
        };
        let opt_num = |k: &str| opts.and_then(|o| o.get(k)).and_then(ScriptValue::as_num);
        let kind_of = |v: Option<String>| match v {
            None => Ok(CopyKind::Char),
            Some(s) => CopyKind::parse(&s).ok_or(format!("{s:?} is not char, line or block")),
        };
        Some(match name {
            "copy-text" => (|| {
                let text = args.first().and_then(ScriptValue::as_str).ok_or("copyText: what text?")?.to_owned();
                let kind = kind_of(opt_str("kind")).map_err(|e| format!("copyText: kind {e}"))?;
                let how = match opt_str("how").as_deref() {
                    None | Some("copy") => How::Copy,
                    Some("delete") => How::Delete,
                    Some(o) => return Err(format!("copyText: how {o:?} is not copy or delete")),
                };
                let source = self.copy_source(None);
                let entry = self.record_copy(NewCopy {
                    text,
                    kind,
                    how,
                    register: opt_str("register"),
                    append: opt_bool("append").unwrap_or(false),
                    clipboard: opt_bool("clipboard"),
                    source,
                });
                Ok((entry.value(), Outcome::None))
            })(),
            "history" => {
                let limit = opt_num("limit").map_or(usize::MAX, |n| n.max(0.0) as usize);
                let list = self.copies.history.iter().take(limit).map(Entry::value).collect();
                Ok((ScriptValue::List(list), Outcome::None))
            }
            "register" => match args.first().and_then(ScriptValue::as_str) {
                Some(r) => Ok((self.copies.register(r).map_or(ScriptValue::Null, Entry::value), Outcome::None)),
                None => Err("register: which name?".into()),
            },
            "registers" => {
                let map = self.copies.named.iter().map(|(k, e)| (k.clone(), e.value())).collect();
                Ok((ScriptValue::Map(map), Outcome::None))
            }
            "set-register" => (|| {
                let r = args.first().and_then(ScriptValue::as_str).ok_or("setRegister: which name?")?.to_owned();
                match args.get(1) {
                    None | Some(ScriptValue::Null) => {
                        self.copies.named.remove(&r);
                    }
                    Some(v) => {
                        let found = v
                            .get("id")
                            .and_then(ScriptValue::as_num)
                            .and_then(|id| self.copies.history.iter().find(|e| e.id == id as u64).cloned());
                        let entry = match found {
                            Some(e) => e,
                            None => {
                                let text =
                                    v.get("text").and_then(ScriptValue::as_str).ok_or("setRegister: what text?")?;
                                let kind = kind_of(v.get("kind").and_then(ScriptValue::as_str).map(str::to_owned))
                                    .map_err(|e| format!("setRegister: kind {e}"))?;
                                self.copies.next_id += 1;
                                Entry {
                                    id: self.copies.next_id,
                                    text: normalized(text.to_owned(), kind),
                                    kind,
                                    how: How::Copy,
                                    register: Some(r.clone()),
                                    source: self.copy_source(None),
                                    time: SystemTime::now(),
                                }
                            }
                        };
                        self.copies.named.insert(r, entry);
                    }
                }
                Ok((ScriptValue::Null, Outcome::None))
            })(),
            "forget-copy" => {
                match args.first().and_then(ScriptValue::as_num) {
                    Some(id) => {
                        let id = id as u64;
                        self.copies.history.retain(|e| e.id != id);
                        self.copies.named.retain(|_, e| e.id != id);
                    }
                    None => {
                        self.copies.history.clear();
                        self.copies.named.clear();
                    }
                }
                Ok((ScriptValue::Null, Outcome::None))
            }
            "put" => (|| {
                let entry = match opt_str("register") {
                    Some(r) => self.copies.register(&r).cloned().ok_or(format!("put: register {r:?} is empty"))?,
                    None => {
                        let i = opt_num("index").map_or(0, |n| n.max(0.0) as usize);
                        self.copies.history.get(i).cloned().ok_or("put: nothing copied")?
                    }
                };
                let pane = opt_num("pane").map_or(self.focus, |p| p as PaneId);
                self.put_entry(pane, &entry).map(|o| (ScriptValue::Null, o))
            })(),
            "read-clipboard" => {
                self.copies.next_ticket += 1;
                let ticket = self.copies.next_ticket;
                self.copies.reads.insert(ticket, Instant::now() + CLIPBOARD_WAIT);
                self.effects.push(Effect::ReadClipboard { ticket, primary: opt_bool("primary").unwrap_or(false) });
                Ok((ScriptValue::map([("$promise", ScriptValue::Num(ticket as f64))]), Outcome::None))
            }
            _ => return None,
        })
    }

    /// Put `entry` into `pane`.
    fn put_entry(&mut self, pane: PaneId, entry: &Entry) -> Result<Outcome, String> {
        // A block is its lines.
        let text = &entry.text;
        match self.panes.get(&pane).map(|e| &e.kind) {
            Some(Kind::Term { buf, .. }) => {
                let buf = *buf;
                // Lines without their last break: the shell does not run
                // them yet.
                let text = if entry.kind == CopyKind::Line { text.strip_suffix('\n').unwrap_or(text) } else { text };
                let t = self.terms.get_mut(&buf).ok_or("put: no terminal")?;
                Ok(Outcome::None.or(t.term.paste(text)))
            }
            Some(Kind::Text { .. }) if entry.kind == CopyKind::Line => {
                let cur = self
                    .run_command("text.cursor", &[ScriptValue::map([("pane", num(pane))])], None, Instant::now())?
                    .0;
                let line = cur.get("line").and_then(ScriptValue::as_num).unwrap_or(0.0);
                let at = ScriptValue::map([("line", ScriptValue::Num(line)), ("character", ScriptValue::Num(0.0))]);
                let args = [ScriptValue::Str(text.clone()), at, ScriptValue::map([("pane", num(pane))])];
                Ok(self.run_command("text.insert", &args, None, Instant::now())?.1)
            }
            Some(Kind::Text { .. }) => {
                let args = [ScriptValue::Str(text.clone()), ScriptValue::map([("pane", num(pane))])];
                Ok(self.run_command("text.type", &args, None, Instant::now())?.1)
            }
            Some(Kind::Input { .. }) if pane == self.focus => {
                let one_line = text.trim_end_matches('\n').replace('\n', " ");
                Ok(self.run_command("input.insert", &[ScriptValue::Str(one_line)], None, Instant::now())?.1)
            }
            Some(_) => Err(format!("put: pane {pane} takes no text")),
            None => Err(format!("put: no pane {pane}")),
        }
    }
}

fn num(p: PaneId) -> ScriptValue {
    ScriptValue::Num(p as f64)
}
