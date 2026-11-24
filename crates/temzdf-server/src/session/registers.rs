//! Registers and the copy history (doc/design/registers.md): what panes,
//! scripts and `temzdf copy` copy, newest first, named registers, putting an
//! entry into a pane, and reading the client's clipboard.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime};

use temzdf_protocol::script::ScriptValue;
use temzdf_scene::{CopyKind, Effect};

use super::{BufferId, Kind, Outcome, Session};
use crate::layout::PaneId;

/// How long a clipboard read (or a paste) waits for the client.
pub(super) const CLIPBOARD_WAIT: Duration = Duration::from_secs(10);

/// Whether an entry was copied or taken out of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum How {
    Copy,
    Delete,
}

/// Where an entry came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Source {
    /// The pane's kind it came from (`terminal`, `pdf`, `edit`, `markdown`),
    /// or `script` (no pane) or `cli`.
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
    /// Clipboard reads asked of the client, by ticket (the Promise's
    /// number, `Bridge::new_promise`): when they give up.
    reads: HashMap<u64, Instant>,
    /// Pastes asked of the client (`Effect::Paste`), by ticket: when they
    /// give up. Input waits for them, so that what follows a paste comes
    /// after it.
    pastes: HashMap<u64, Instant>,
    /// `copy` in a PDF pane: the Promise (ticket) waiting for each copy,
    /// by pane and the pane's copy id ([`PdfPane::copy`]).
    ///
    /// [`PdfPane::copy`]: temzdf_pdfview::pane::PdfPane::copy
    pub(super) pdf_copies: HashMap<(PaneId, u64), u64>,
}

impl Copies {
    pub fn register(&self, name: &str) -> Option<&Entry> {
        self.named.get(name)
    }

    /// When the next clipboard read gives up.
    pub fn deadline(&self) -> Option<Instant> {
        self.reads.values().chain(self.pastes.values()).min().copied()
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

/// The names of the commands this module runs (declared with the others in
/// crates/temzdf-script/ts-types/temzdf-actions.d.ts).
const NAMES: &[&str] =
    &["copy-text", "history", "register", "set-register", "registers", "forget-copy", "put", "read-clipboard"];

/// What a pane copied, by which buffer: its text, kind and note, and the
/// PDF copy (pane, id) it was, if any.
type PaneCopy = (BufferId, String, CopyKind, Option<String>, Option<(PaneId, u64)>);

impl Session {
    /// The source of a copy made in `buf` (the focused pane's buffer for
    /// `None`).
    pub(super) fn copy_source(&self, buf: Option<BufferId>) -> Source {
        let buf = buf.or_else(|| self.pane_buffer(self.focus));
        let found = buf.and_then(|b| self.buffer_info(b));
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

    /// What panes copied since the last call goes into the history (with a
    /// notice, [`Session::pane_copied`]), and a PDF pane's search that found
    /// nothing gets its notice; their other effects stay for the client. A
    /// redraw when there was a notice.
    pub(super) fn collect_pane_requests(&mut self) -> Outcome {
        use temzdf_pdfview::pane::{CopyDropped, Request};
        let mut copied: Vec<PaneCopy> = Vec::new();
        let mut not_found = Vec::new();
        let mut notices = Vec::new();
        let mut dropped = Vec::new();
        let mut others = Vec::new();
        for (id, t) in &mut self.terms {
            for e in t.term.take_effects() {
                match e {
                    Effect::Copied { text, kind, note } => copied.push((*id, text, kind, note, None)),
                    e => others.push(e),
                }
            }
        }
        for (pid, e) in &mut self.panes {
            if let Kind::Pdf { pane, doc } = &mut e.kind {
                for req in pane.take_requests() {
                    match req {
                        Request::NotFound(query) => not_found.push(query),
                        Request::Copy { id, text, note } => {
                            copied.push((*doc, text, CopyKind::Char, note, Some((*pid, id))));
                        }
                        Request::Copying { pages, .. } => notices.push(format!("Copying: reading {pages} pages…")),
                        Request::CopyDropped { id, why } => {
                            if why == CopyDropped::Reloaded {
                                notices.push(format!("Copy dropped: {why}"));
                            }
                            dropped.push(((*pid, id), why));
                        }
                        req => {
                            if let Some(host) = self.docs.get_mut(doc).and_then(|d| d.host.as_mut())
                                && let Some(effect) = host.request(req)
                            {
                                others.push(effect);
                            }
                        }
                    }
                }
            }
        }
        self.effects.extend(others);
        let quiet = copied.is_empty() && not_found.is_empty() && notices.is_empty();
        let out = if quiet { Outcome::None } else { Outcome::Redraw };
        for (buf, text, kind, note, pdf) in copied {
            // Nothing selected had text: nothing goes into the history.
            let entry = if text.is_empty() && pdf.is_some() {
                ScriptValue::Null
            } else {
                self.pane_copied(buf, text, kind, note.as_deref())
            };
            if let Some(ticket) = pdf.and_then(|k| self.copies.pdf_copies.remove(&k)) {
                self.resolve_promise(ticket, Ok(entry));
            }
        }
        for (key, why) in dropped {
            if let Some(ticket) = self.copies.pdf_copies.remove(&key) {
                self.resolve_promise(ticket, Err(format!("copy: {why}")));
            }
        }
        // Copies of panes that closed meanwhile will not come.
        let gone: Vec<(PaneId, u64)> =
            self.copies.pdf_copies.keys().filter(|(p, _)| !self.panes.contains_key(p)).copied().collect();
        for key in gone {
            if let Some(ticket) = self.copies.pdf_copies.remove(&key) {
                self.resolve_promise(ticket, Err("copy: the pane closed".into()));
            }
        }
        for text in notices {
            self.notify(super::notices::Kind::Info, &text);
        }
        for query in not_found {
            self.not_found(&query);
        }
        out
    }

    /// A pane on `buf` copied `text` (a terminal's or a document's
    /// selection; a program in a terminal through OSC 52): into the
    /// history, and a notice says so, `note` after the count. Text panes'
    /// copies (yanks) go through `copy-text` instead, without a notice.
    /// Returns the entry, as `Copied` hooks get it.
    pub(super) fn pane_copied(
        &mut self,
        buf: BufferId,
        text: String,
        kind: CopyKind,
        note: Option<&str>,
    ) -> ScriptValue {
        let n = text.chars().count();
        let source = self.copy_source(Some(buf));
        let entry = self.record_copy(NewCopy {
            text,
            kind,
            how: How::Copy,
            register: None,
            append: false,
            clipboard: None,
            source,
        });
        let notice = match note {
            Some(note) => format!("Copied {n} characters; {note}"),
            None => format!("Copied {n} characters"),
        };
        self.notify(super::notices::Kind::Info, &notice);
        entry.value()
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

    /// The client answered a clipboard read or a paste.
    pub fn clipboard_answer(&mut self, ticket: u64, result: Result<String, String>) -> Outcome {
        if self.copies.pastes.remove(&ticket).is_some() {
            return self.paste_answered(result);
        }
        if self.copies.reads.remove(&ticket).is_some() {
            self.resolve_promise(ticket, result.map(ScriptValue::Str));
        }
        Outcome::None
    }

    /// Ask the client to paste its clipboard (`primary`: the primary
    /// selection); input waits for the answer ([`Session::awaits_paste`]).
    pub(super) fn ask_paste(&mut self, primary: bool) {
        let ticket = self.script.new_promise();
        self.copies.pastes.insert(ticket, Instant::now() + CLIPBOARD_WAIT);
        self.effects.push(Effect::Paste { ticket, primary });
    }

    /// A paste asked of the client has not been answered: input waits.
    pub(super) fn awaits_paste(&self) -> bool {
        !self.copies.pastes.is_empty()
    }

    /// `<C-c>` while input waits for a paste: it is given up.
    pub(super) fn cancel_pastes(&mut self) {
        self.copies.pastes.clear();
    }

    /// Clipboard reads and pastes the client did not answer in time fail.
    pub(super) fn expire_clipboard_reads(&mut self, now: Instant) -> Outcome {
        let late: Vec<u64> = self.copies.reads.iter().filter(|(_, at)| **at <= now).map(|(t, _)| *t).collect();
        for t in late {
            self.copies.reads.remove(&t);
            self.resolve_promise(t, Err("the clipboard could not be read: the client did not answer".into()));
        }
        let late: Vec<u64> = self.copies.pastes.iter().filter(|(_, at)| **at <= now).map(|(t, _)| *t).collect();
        let mut out = Outcome::None;
        for t in late {
            self.copies.pastes.remove(&t);
            out = out.merge(self.paste_answered(Err("the client did not answer".into())));
        }
        out
    }

    pub(super) fn copies_deadline(&self) -> Option<Instant> {
        self.copies.deadline()
    }

    /// The commands of [`NAMES`]; `None` for other names.
    pub(super) fn register_command(
        &mut self,
        name: &str,
        args: &[ScriptValue],
    ) -> Option<Result<(ScriptValue, Outcome), String>> {
        if !NAMES.contains(&name) {
            return None;
        }
        // What panes copied just now (a visual mode's yank) is in.
        let copied = self.collect_pane_requests();
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
        let result = match name {
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
                let ticket = self.script.new_promise();
                self.copies.reads.insert(ticket, Instant::now() + CLIPBOARD_WAIT);
                self.effects.push(Effect::ReadClipboard { ticket, primary: opt_bool("primary").unwrap_or(false) });
                Ok((ScriptValue::map([("$promise", ScriptValue::Num(ticket as f64))]), Outcome::None))
            }
            _ => return None,
        };
        Some(result.map(|(v, o)| (v, o.merge(copied))))
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
                let cur =
                    self.run_command("text.cursor", &[ScriptValue::map([("pane", num(pane))])], Instant::now())?.0;
                let line = cur.get("line").and_then(ScriptValue::as_num).unwrap_or(0.0);
                let at = ScriptValue::map([("line", ScriptValue::Num(line)), ("character", ScriptValue::Num(0.0))]);
                let args = [ScriptValue::Str(text.clone()), at, ScriptValue::map([("pane", num(pane))])];
                Ok(self.run_command("text.insert", &args, Instant::now())?.1)
            }
            Some(Kind::Text { .. }) => {
                let args = [ScriptValue::Str(text.clone()), ScriptValue::map([("pane", num(pane))])];
                Ok(self.run_command("text.type", &args, Instant::now())?.1)
            }
            Some(_) => Err(format!("put: pane {pane} takes no text")),
            None => Err(format!("put: no pane {pane}")),
        }
    }
}

fn num(p: PaneId) -> ScriptValue {
    ScriptValue::Num(p as f64)
}
