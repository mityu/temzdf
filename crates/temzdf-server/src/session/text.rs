//! Text editing panes (`edit`): buffers of `temzdf-texteditor`, the panes
//! showing them, and the `text.*` primitives scripts build editors from
//! (doc/design/text-editor.md). The pane kind and its default mode are
//! `edit`; typed text goes into the buffer in pass-through modes, as an
//! input pane's does.

use std::path::{Path, PathBuf};

use temzdf_protocol::script::ScriptValue;
use temzdf_scene::{CursorShape, Frame, TextGrid};
use temzdf_texteditor::file::{self, FileInfo};
use temzdf_texteditor::{
    Buffer, Cursor, Edited, Found, Highlights, LineEnding, Origin, Pattern, Position, Range, Reveal, SearchOptions,
    SelectionKind, StepKind, Text, TextEdit, View, ViewUnit,
};

use super::{BufferId, Kind, Outcome, PaneEntry, PaneId, Session, Spot};
use termpix_core::geom::Rect;

/// A text buffer and its file.
pub(super) struct TextEntry {
    pub buf: Buffer,
    pub file: Option<FileInfo>,
    /// What lists and the status line call it.
    pub name: String,
    /// The invocation its undo tree last heard of (a new one ends a step).
    invocation: u64,
    /// The base mode of panes showing it, chosen when it was made.
    pub basemode: Option<String>,
    /// Watches its file (kept alive here).
    watcher: Option<Box<dyn std::any::Any + Send>>,
    /// The modification time last told in FileChangedOnDisk (once per
    /// change, however many events it makes).
    disk_seen: Option<std::time::SystemTime>,
    /// Its attributes (Vim's buftype, several at once): [`BUFFER_ATTRS`].
    pub attrs: Vec<String>,
}

/// The attributes a text buffer may have, and what each does.
pub(super) const BUFFER_ATTRS: &[(&str, &str)] = &[
    ("nofile", "tied to no file: never modified (nothing to lose), written only to a path given"),
    ("unlisted", "for a script's own use: buffers() leaves it out (buffers({ all: true }) lists it)"),
];

/// Attributes from a script's list, each known.
pub(super) fn parse_attrs(v: Option<&ScriptValue>) -> Result<Vec<String>, String> {
    let Some(v) = v.filter(|v| **v != ScriptValue::Null) else { return Ok(Vec::new()) };
    let ScriptValue::List(l) = v else { return Err("attrs: a list of names".into()) };
    let mut out = Vec::new();
    for a in l {
        let a = a.as_str().ok_or("attrs: a list of names")?;
        if !BUFFER_ATTRS.iter().any(|(n, _)| *n == a) {
            let known: Vec<_> = BUFFER_ATTRS.iter().map(|(n, _)| *n).collect();
            return Err(format!("no buffer attribute {a:?} (known: {})", known.join(", ")));
        }
        if !out.iter().any(|o| o == a) {
            out.push(a.to_owned());
        }
    }
    Ok(out)
}

impl TextEntry {
    pub(super) fn has(&self, attr: &str) -> bool {
        self.attrs.iter().any(|a| a == attr)
    }

    /// Unsaved changes (a `nofile` buffer has none to lose).
    pub(super) fn modified(&self) -> bool {
        !self.has("nofile") && self.buf.modified()
    }

    pub(super) fn attrs_value(&self) -> ScriptValue {
        ScriptValue::List(self.attrs.iter().map(|a| ScriptValue::Str(a.clone())).collect())
    }
}

/// A text pane: its view of the buffer, and its cursor's shape.
pub(super) struct TextView {
    pub view: View,
    pub shape: CursorShape,
}

fn name_of(path: Option<&Path>) -> String {
    path.and_then(|p| p.file_name()).map_or_else(|| "[No Name]".into(), |n| n.to_string_lossy().into_owned())
}

/// `{ line, character }` from a script.
pub(super) fn position(v: &ScriptValue) -> Option<Position> {
    let n = |k: &str| v.get(k)?.as_num().map(|n| n.max(0.0) as usize);
    Some(Position { line: n("line")?, character: n("character")? })
}

fn range(v: &ScriptValue) -> Option<Range> {
    Some(Range { start: position(v.get("start")?)?, end: position(v.get("end")?)? })
}

pub(super) fn position_value(p: Position) -> ScriptValue {
    ScriptValue::map([("line", ScriptValue::Num(p.line as f64)), ("character", ScriptValue::Num(p.character as f64))])
}

fn range_value(text: &Text, a: usize, b: usize) -> ScriptValue {
    ScriptValue::map([("start", position_value(text.position(a))), ("end", position_value(text.position(b)))])
}

fn kind_name(k: SelectionKind) -> &'static str {
    match k {
        SelectionKind::Char => "char",
        SelectionKind::Line => "line",
        SelectionKind::Block => "block",
    }
}

fn selection_kind(name: Option<&str>) -> Result<SelectionKind, String> {
    Ok(match name {
        None | Some("char") => SelectionKind::Char,
        Some("line") => SelectionKind::Line,
        Some("block") => SelectionKind::Block,
        Some(other) => return Err(format!("no selection kind {other:?} (char, line, block)")),
    })
}

fn cursor_shape(name: &str) -> Result<CursorShape, String> {
    let blink = name.ends_with("-blink");
    Ok(match name.trim_end_matches("-blink") {
        "default" => CursorShape::Default,
        "block" => CursorShape::Block { blink },
        "bar" => CursorShape::Bar { blink },
        "underline" => CursorShape::Underline { blink },
        other => return Err(format!("no cursor style {other:?} (block, bar, underline, default; -blink)")),
    })
}

impl Session {
    /// The view a new pane on `buf` starts with: the options, and the
    /// cursor of another pane on it (a split shows the same place).
    pub(super) fn text_view_for(&self, buf: BufferId, pane: PaneId) -> TextView {
        let mut tv = self.text_view(pane);
        if let Some(other) = self.panes.values().find_map(|e| match &e.kind {
            Kind::Text { buf: b, view } if *b == buf => Some(view.view.cursor),
            _ => None,
        }) {
            tv.view.cursor = Cursor { anchor: None, ..other };
        }
        tv
    }

    /// The view a new text pane starts with, from the options.
    fn text_view(&self, pane: PaneId) -> TextView {
        let o = &self.options;
        let view = View {
            // Decorations shown only in it count there (inline virtual text).
            pane: Some(pane),
            layout: o.text.layout.clone(),
            number: o.text.number,
            cursorline: o.text.cursorline,
            scrolloff: o.text.scrolloff,
            ..View::default()
        };
        TextView { view, shape: CursorShape::Default }
    }

    /// Open `path` as text in a new pane at `spot`: the buffer already
    /// showing it if any. A missing file is a new one when `create`.
    pub(super) fn open_text(&mut self, path: &Path, spot: Spot, create: bool) -> anyhow::Result<PaneId> {
        self.open_text_as(path, spot, create, None)
    }

    /// [`open_text`](Self::open_text) in `encoding` (an Encoding Standard
    /// label), for a file that is not UTF-8.
    pub(super) fn open_text_as(
        &mut self,
        path: &Path,
        spot: Spot,
        create: bool,
        encoding: Option<&str>,
    ) -> anyhow::Result<PaneId> {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| std::path::absolute(path).unwrap_or_default());
        let existing = self.texts.iter().find(|(_, t)| t.file.as_ref().is_some_and(|f| f.path == canonical));
        let buf = match existing {
            Some((&id, _)) => id,
            None => {
                let (text, info) = match file::read_as(&canonical, encoding) {
                    Ok(read) => read,
                    Err(_) if create && !canonical.exists() => (
                        Text::default(),
                        FileInfo { path: canonical.clone(), encoding: "utf-8".into(), ..FileInfo::default() },
                    ),
                    Err(e) => anyhow::bail!(e),
                };
                self.add_text_buffer(text, Some(info))
            }
        };
        // Into an existing pane (`:e`): it shows the buffer instead.
        if let Spot::Replace(pane) = spot {
            self.show_in(pane, buf).map_err(anyhow::Error::msg)?;
            return Ok(pane);
        }
        let id = self.alloc_id();
        let focus = self.takes_focus(&spot);
        let rect = self.place(id, spot)?;
        self.text_pane(id, buf, rect, focus);
        Ok(id)
    }

    /// A new text buffer (no pane), with its file if any.
    pub(super) fn add_text_buffer(&mut self, text: Text, file: Option<FileInfo>) -> BufferId {
        let id = self.alloc_id();
        let buf = Buffer::new(text, file.as_ref().map_or(LineEnding::Lf, |f| f.line_ending));
        let name = name_of(file.as_ref().map(|f| f.path.as_path()));
        self.texts.insert(
            id,
            TextEntry {
                buf,
                file,
                name,
                invocation: 0,
                basemode: None,
                watcher: None,
                disk_seen: None,
                attrs: Vec::new(),
            },
        );
        self.watch_text(id);
        self.event("BufferCreated", vec![ScriptValue::Num(id as f64), ScriptValue::Str("edit".into())]);
        id
    }

    /// Pane `id`, placed at `rect`, shows text `buf`.
    pub(super) fn text_pane(&mut self, id: PaneId, buf: BufferId, rect: Rect, focus: bool) {
        let tv = self.text_view_for(buf, id);
        self.panes.insert(id, PaneEntry::new(Kind::Text { buf, view: Box::new(tv) }, rect));
        self.event("PaneOpened", vec![ScriptValue::Num(id as f64), ScriptValue::Str("edit".into())]);
        if focus {
            self.set_focus(id);
        }
        self.relayout();
        self.fit_text_view(id);
    }

    /// The focused pane's base mode changed: the history starts from it.
    fn refocus_mode(&mut self) {
        let old = self.history.current().to_owned();
        let mode = self.pane_mode(self.focus);
        self.history.reset(&mode);
        self.mode_changed(&old);
    }

    /// Pane `id`'s view follows its size.
    pub(super) fn fit_text_view(&mut self, id: PaneId) {
        let Some(e) = self.panes.get_mut(&id) else { return };
        let content = e.content();
        if let Kind::Text { buf, view } = &mut e.kind
            && let Some(t) = self.texts.get(buf)
        {
            let (w, h) = (content.w.max(1) as u16, content.h.max(1) as u16);
            if (view.view.width, view.view.height) != (w, h) {
                (view.view.width, view.view.height) = (w, h);
                view.view.relayout(&t.buf);
            }
        }
    }

    /// The text pane `pane` (the focused one for `None`) and its buffer.
    fn text_target(&self, pane: Option<PaneId>) -> Result<(PaneId, BufferId), String> {
        let pane = pane.unwrap_or(self.focus);
        match self.panes.get(&pane).map(|e| &e.kind) {
            Some(Kind::Text { buf, .. }) => Ok((pane, *buf)),
            Some(_) => Err(format!("pane {pane} is not a text pane")),
            None => Err(format!("no pane {pane}")),
        }
    }

    fn view_mut(&mut self, pane: PaneId) -> Option<&mut TextView> {
        match self.panes.get_mut(&pane).map(|e| &mut e.kind) {
            Some(Kind::Text { view, .. }) => Some(view),
            _ => None,
        }
    }

    /// Apply edits to `buf` from `pane`, as one batch: every pane on it
    /// follows, and `pane` keeps its cursor in sight.
    fn text_apply(&mut self, pane: PaneId, buf: BufferId, edits: Vec<TextEdit>, kind: StepKind) -> Result<(), String> {
        let cursor = self.view_mut(pane).map(|v| v.view.cursor).unwrap_or_default();
        let invocation = self.invocation;
        let t = self.texts.get_mut(&buf).ok_or("its buffer is gone")?;
        if t.invocation != invocation {
            t.invocation = invocation;
            t.buf.undo_tree_mut().invocation();
        }
        let edited = t.buf.edit(edits, Origin { kind, pane: Some(pane), cursor }).map_err(|e| e.to_string())?;
        self.text_follow(buf, &edited, pane);
        let cause = if kind == StepKind::Typing { "typing" } else { "script" };
        self.text_changed(buf, &edited, cause);
        Ok(())
    }

    /// `TextChanged(buffer, { changes, version, cause, invocation })`: the
    /// changes in positions before them, with the invocation they happened
    /// in (none from hooks, timers or tasks).
    pub(super) fn text_changed(&mut self, buf: BufferId, edited: &Edited, cause: &str) {
        if edited.reported.is_empty() || !self.script.hooks_on("TextChanged") {
            return;
        }
        let Some(t) = self.texts.get(&buf) else { return };
        let changes = edited
            .reported
            .iter()
            .map(|r| {
                ScriptValue::map([
                    (
                        "range",
                        ScriptValue::map([
                            ("start", position_value(r.range.start)),
                            ("end", position_value(r.range.end)),
                        ]),
                    ),
                    ("rangeLength", ScriptValue::Num(r.range_length as f64)),
                    ("text", ScriptValue::Str(r.text.clone())),
                ])
            })
            .collect();
        let invocation = match self.current_invocation {
            Some(n) if !self.script.in_hook() => ScriptValue::Num(n as f64),
            _ => ScriptValue::Null,
        };
        let info = ScriptValue::map([
            ("changes", ScriptValue::List(changes)),
            ("version", ScriptValue::Num(t.buf.version() as f64)),
            ("cause", ScriptValue::Str(cause.into())),
            ("invocation", invocation),
        ]);
        self.event("TextChanged", vec![ScriptValue::Num(buf as f64), info]);
    }

    /// The panes on `buf` follow what changed; the cursor undo or redo
    /// restores goes to its pane (or `by`'s when that one is gone).
    fn text_follow(&mut self, buf: BufferId, edited: &Edited, by: PaneId) {
        let Some(t) = self.texts.get(&buf) else { return };
        let restore = edited.cursor.map(|(p, c)| {
            let p = p
                .filter(|p| matches!(self.panes.get(p).map(|e| &e.kind), Some(Kind::Text { buf: b, .. }) if *b == buf));
            (p.unwrap_or(by), c)
        });
        for (id, e) in &mut self.panes {
            let Kind::Text { buf: b, view } = &mut e.kind else { continue };
            if *b != buf {
                continue;
            }
            for batch in &edited.batches {
                view.view.follow(&t.buf, batch);
            }
            if let Some((p, c)) = restore
                && p == *id
            {
                view.view.cursor = c;
            }
            if *id == by || restore.is_some_and(|(p, _)| p == *id) {
                let pos = view.view.cursor.pos;
                view.view.reveal(&t.buf, pos, Reveal::Minimal);
            }
        }
    }

    /// Typed text (keys no mapping took, input-method text, pastes) into
    /// the focused text pane, replacing its selection.
    pub(super) fn text_type(&mut self, s: &str) -> Result<Outcome, String> {
        let (pane, buf) = self.text_target(None)?;
        if s.is_empty() {
            return Ok(Outcome::None);
        }
        let c = self.view_mut(pane).map(|v| v.view.cursor).unwrap_or_default();
        let (a, b) = match c.selection() {
            Some((a, b, SelectionKind::Char)) => (a, b),
            _ => (c.pos, c.pos),
        };
        if let Some(v) = self.view_mut(pane) {
            v.view.cursor = Cursor::at(a);
        }
        self.text_apply(pane, buf, vec![TextEdit::new(a, b, s)], StepKind::Typing)?;
        // After what was typed (a replaced selection's start stays before
        // the replacement otherwise).
        let after = a + s.chars().count();
        if let Some(t) = self.texts.get(&buf)
            && let Some(Kind::Text { view, .. }) = self.panes.get_mut(&pane).map(|e| &mut e.kind)
        {
            view.view.set_cursor(&t.buf, after, false);
        }
        Ok(Outcome::Redraw)
    }

    /// Write `buf` (to `path`, which becomes its file, when given).
    fn text_write(&mut self, buf: BufferId, path: Option<PathBuf>, force: bool) -> Result<(), String> {
        let t = self.texts.get_mut(&buf).ok_or_else(|| format!("no text buffer {buf}"))?;
        if path.is_none() && t.has("nofile") {
            return Err("a nofile buffer is written only to a path given (write({ path }))".into());
        }
        let mut info = match (path, &t.file) {
            (Some(p), Some(f)) => FileInfo { path: std::path::absolute(&p).unwrap_or(p), mtime: None, ..f.clone() },
            (Some(p), None) => FileInfo {
                path: std::path::absolute(&p).unwrap_or(p),
                encoding: "utf-8".into(),
                line_ending: t.buf.line_ending,
                ..FileInfo::default()
            },
            (None, Some(f)) => f.clone(),
            (None, None) => return Err("no file name (write({ path }))".into()),
        };
        info.line_ending = t.buf.line_ending;
        info.mtime = file::write(t.buf.text(), &info, force)?;
        t.name = name_of(Some(&info.path));
        t.file = Some(info);
        t.buf.undo_tree_mut().mark_saved();
        // Its file may be another one now.
        self.watch_text(buf);
        Ok(())
    }

    /// Watch text buffer `buf`'s file (if it has one): changes made
    /// elsewhere come as `SessionEvent::TextFile`.
    fn watch_text(&mut self, buf: BufferId) {
        let events = self.events.clone();
        let Some(t) = self.texts.get_mut(&buf) else { return };
        t.watcher = t.file.as_ref().and_then(|f| {
            temzdf_pdfcore::workers::watch_file(&f.path, move || {
                events(super::SessionEvent::TextFile(buf));
            })
            .ok()
            .map(|w| Box::new(w) as Box<dyn std::any::Any + Send>)
        });
    }

    /// A watcher saw `buf`'s file change: when it differs from what was
    /// read or written (its modification time), `FileChangedOnDisk(buf)`,
    /// once per change. Reloading is the scripts' policy.
    pub(super) fn text_file_changed(&mut self, buf: BufferId) {
        let Some(t) = self.texts.get_mut(&buf) else { return };
        let Some(f) = &t.file else { return };
        let Ok(now) = std::fs::metadata(&f.path).and_then(|m| m.modified()) else { return };
        if Some(now) == f.mtime || Some(now) == t.disk_seen {
            return;
        }
        t.disk_seen = Some(now);
        self.event("FileChangedOnDisk", vec![ScriptValue::Num(buf as f64)]);
    }

    /// Read `buf`'s file again: the difference as one undoable edit (marks
    /// and cursors outside it stay), then it is the saved state.
    fn text_reload(&mut self, buf: BufferId) -> Result<(), String> {
        let t = self.texts.get(&buf).ok_or_else(|| format!("no text buffer {buf}"))?;
        let path = t.file.as_ref().map(|f| f.path.clone()).ok_or("the buffer has no file")?;
        let encoding = t.file.as_ref().map(|f| f.encoding.clone()).filter(|e| e != "utf-8");
        let (text, info) = file::read_as(&path, encoding.as_deref())?;
        let pane = self.panes.iter().find_map(|(id, e)| match &e.kind {
            Kind::Text { buf: b, .. } if *b == buf => Some(*id),
            _ => None,
        });
        let cursor = pane.and_then(|p| self.view_mut(p)).map(|v| v.view.cursor).unwrap_or_default();
        let t = self.texts.get_mut(&buf).expect("found");
        let edited = t
            .buf
            .reload(&text.to_string(), Origin { kind: StepKind::Other, pane, cursor })
            .map_err(|e| e.to_string())?;
        t.buf.line_ending = info.line_ending;
        (t.file, t.disk_seen) = (Some(info), None);
        if let Some(p) = pane {
            self.text_follow(buf, &edited, p);
        }
        self.text_changed(buf, &edited, "reload");
        Ok(())
    }

    /// A text pane's frame and status bar.
    pub(super) fn text_frame(
        t: Option<&TextEntry>,
        tv: &TextView,
        pane: PaneId,
        focused: bool,
        hl: &Highlights,
        patterns: &[(String, Pattern, String)],
    ) -> Frame {
        let Some(t) = t else { return Frame::default() };
        let painted: Vec<(&Pattern, &str)> = patterns.iter().map(|(_, p, s)| (p, s.as_str())).collect();
        let drawn = temzdf_texteditor::render(&t.buf, &tv.view, hl, Some(pane), focused, &painted);
        Frame {
            text: vec![TextGrid { col: 0, row: 0, rows: drawn.rows }],
            cursor: drawn.cursor,
            cursor_shape: tv.shape,
            title: format!("{}{}", t.name, if t.modified() { " [+]" } else { "" }),
            ..Frame::default()
        }
    }

    pub(super) fn text_status(t: Option<&TextEntry>, tv: &TextView, mode: &str) -> termpix_ui::StatusBar {
        let Some(t) = t else { return termpix_ui::StatusBar::default() };
        let text = t.buf.text();
        let pos = text.position(tv.view.cursor.pos);
        let lines = text.line_count();
        let pct = if lines <= 1 { 100 } else { (pos.line * 100) / (lines - 1) };
        let (enc, eol) = (
            t.file.as_ref().map_or("utf-8", |f| f.encoding.as_str()),
            match t.buf.line_ending {
                LineEnding::Lf => "lf",
                LineEnding::CrLf => "crlf",
            },
        );
        let bom = if t.file.as_ref().is_some_and(|f| f.bom) { "-bom" } else { "" };
        termpix_ui::StatusBar {
            left: format!("{}{}", t.name, if t.modified() { " [+]" } else { "" }),
            right: format!("{mode}  {enc}{bom} {eol}  {}:{}  {pct}%", pos.line + 1, pos.character + 1),
            ..Default::default()
        }
    }

    /// The `text.*` commands (and the file commands of text buffers);
    /// `None` for other names.
    pub(super) fn text_command(
        &mut self,
        name: &str,
        args: &[ScriptValue],
        count: Option<usize>,
    ) -> Option<Result<(ScriptValue, Outcome), String>> {
        let result = match name {
            "open-file" | "new-buffer" | "write" | "reload" => self.text_file_command(name, args),
            "set-buffer-attrs" => self.set_buffer_attrs(args),
            "highlight-pattern" => self.highlight_pattern(args),
            "highlight" => self.define_highlight(args),
            "get-var" | "set-var" => self.var_command(name, args),
            n if n.starts_with("text.") => self.text_primitive(&n["text.".len()..], args, count),
            _ => return None,
        };
        Some(result.map_err(|e| format!("{}: {e}", camel(name))))
    }

    /// `highlightPattern(ns, pattern, style)`: paint `pattern`'s matches in
    /// every text pane (null: no more); one pattern per namespace.
    fn highlight_pattern(&mut self, args: &[ScriptValue]) -> Result<(ScriptValue, Outcome), String> {
        let ns = args.first().and_then(ScriptValue::as_str).ok_or("which namespace?")?.to_owned();
        self.patterns.retain(|(n, _, _)| *n != ns);
        if let Some(p) = args.get(1).and_then(ScriptValue::as_str) {
            let style = args.get(2).and_then(ScriptValue::as_str).unwrap_or("Search").to_owned();
            self.patterns.push((ns, Pattern::new(p)?, style));
        }
        Ok((ScriptValue::Null, Outcome::Redraw))
    }

    /// `highlight(name, { fg, bg, bold, ... })`: define a highlight group
    /// (colours "#rrggbb", a palette index, or "default").
    fn define_highlight(&mut self, args: &[ScriptValue]) -> Result<(ScriptValue, Outcome), String> {
        use temzdf_scene::{Attrs, Color, Underline};
        let name = args.first().and_then(ScriptValue::as_str).ok_or("which group?")?;
        let spec = args.get(1).ok_or("which style?")?;
        let color = |k: &str| -> Result<Option<Color>, String> {
            Ok(match spec.get(k) {
                None | Some(ScriptValue::Null) => None,
                Some(ScriptValue::Num(n)) if (0.0..=255.0).contains(n) => Some(Color::Indexed(*n as u8)),
                Some(ScriptValue::Str(s)) if s == "default" => Some(Color::Default),
                Some(ScriptValue::Str(s)) if s.len() == 7 && s.starts_with('#') => {
                    let v = u32::from_str_radix(&s[1..], 16).map_err(|_| format!("{k}: {s:?} is no colour"))?;
                    Some(Color::Rgb([(v >> 16) as u8, (v >> 8) as u8, v as u8]))
                }
                Some(other) => return Err(format!("{k}: {other:?} is no colour (\"#rrggbb\", 0-255, \"default\")")),
            })
        };
        let flag = |k: &str| spec.get(k) == Some(&ScriptValue::Bool(true));
        let names = ["bold", "italic", "underline", "undercurl", "inverse", "strikethrough", "faint", "overline"];
        let any_attr = names.iter().any(|k| spec.get(k).is_some());
        let attrs = any_attr.then(|| Attrs {
            bold: flag("bold"),
            italic: flag("italic"),
            faint: flag("faint"),
            inverse: flag("inverse"),
            strikethrough: flag("strikethrough"),
            overline: flag("overline"),
            underline: if flag("undercurl") {
                Underline::Curly
            } else if flag("underline") {
                Underline::Single
            } else {
                Underline::None
            },
            ..Attrs::default()
        });
        let style = temzdf_texteditor::Style { fg: color("fg")?, bg: color("bg")?, attrs };
        self.highlights.set(name, style);
        Ok((ScriptValue::Null, Outcome::Redraw))
    }

    /// `getVar(scope, name, id?)`, `setVar(scope, name, value, id?)`: a
    /// buffer's, pane's or tab page's variable (the focused one's; tabs by
    /// number from 1), held by the server (it outlives script host
    /// restarts), gone with its owner.
    fn var_command(&mut self, name: &str, args: &[ScriptValue]) -> Result<(ScriptValue, Outcome), String> {
        let scope = args.first().and_then(ScriptValue::as_str).ok_or("which scope?")?;
        let var = args.get(1).and_then(ScriptValue::as_str).ok_or("which variable?")?.to_owned();
        let id_arg = if name == "set-var" { args.get(3) } else { args.get(2) };
        let id = id_arg.and_then(ScriptValue::as_num).map(|n| n as u32);
        let owner = match scope {
            "buffer" => {
                let b = id.unwrap_or_else(|| self.panes.get(&self.focus).map_or(0, |e| e.buffer()));
                self.has_buffer(b).then_some(super::VarOwner::Buffer(b))
            }
            "pane" => {
                let p = id.unwrap_or(self.focus);
                self.panes.contains_key(&p).then_some(super::VarOwner::Pane(p))
            }
            "tab" => {
                let i = id.map_or(self.tab, |n| (n as usize).wrapping_sub(1));
                self.tabs.get(i).map(|t| super::VarOwner::Tab(t.id))
            }
            other => return Err(format!("no scope {other:?} (buffer, pane, tab)")),
        }
        .ok_or_else(|| format!("no {scope} {}", id.map_or("focused".into(), |i| i.to_string())))?;
        // Owners are never reused: forget those that went.
        let live: Vec<super::VarOwner> =
            self.vars.keys().map(|(o, _)| *o).filter(|o| self.var_owner_lives(*o)).collect();
        self.vars.retain(|(o, _), _| live.contains(o));
        if name == "get-var" {
            return Ok((self.vars.get(&(owner, var)).cloned().unwrap_or(ScriptValue::Null), Outcome::None));
        }
        match args.get(2) {
            None | Some(ScriptValue::Null) => self.vars.remove(&(owner, var)),
            Some(v) => self.vars.insert((owner, var), v.clone()),
        };
        Ok((ScriptValue::Null, Outcome::None))
    }

    fn var_owner_lives(&self, o: super::VarOwner) -> bool {
        match o {
            super::VarOwner::Buffer(b) => self.has_buffer(b),
            super::VarOwner::Pane(p) => self.panes.contains_key(&p),
            super::VarOwner::Tab(t) => self.tab_index(t).is_some(),
        }
    }

    /// Drop decorations whose time is up; whether any were.
    pub(super) fn expire_decorations(&mut self, now: std::time::Instant) -> bool {
        let mut any = false;
        for t in self.texts.values_mut() {
            any |= t.buf.marks_mut().expire(now);
        }
        any
    }

    /// When the next decoration's time is up.
    pub(super) fn decorations_deadline(&self) -> Option<std::time::Instant> {
        self.texts.values().filter_map(|t| t.buf.marks().next_expiry()).min()
    }

    fn set_buffer_attrs(&mut self, args: &[ScriptValue]) -> Result<(ScriptValue, Outcome), String> {
        let buf = args.first().and_then(ScriptValue::as_num).ok_or("which buffer?")? as BufferId;
        let attrs = parse_attrs(args.get(1))?;
        let t = self.texts.get_mut(&buf).ok_or_else(|| format!("no text buffer {buf}"))?;
        t.attrs = attrs;
        Ok((ScriptValue::Null, Outcome::Redraw))
    }

    fn text_file_command(&mut self, name: &str, args: &[ScriptValue]) -> Result<(ScriptValue, Outcome), String> {
        let opts = args.get(1).or(args.first()).filter(|v| matches!(v, ScriptValue::Map(_)));
        let opt_str = |k: &str| opts.and_then(|o| o.get(k)).and_then(ScriptValue::as_str).map(str::to_owned);
        match name {
            "open-file" => {
                let path = args.first().and_then(ScriptValue::as_str).ok_or("which file?")?;
                let known: Vec<BufferId> = self.texts.keys().copied().collect();
                let encoding = opt_str("encoding");
                let into = opts.and_then(|o| o.get("pane")).and_then(ScriptValue::as_num);
                let spot = into.map_or_else(|| self.next_spot(), |p| Spot::Replace(p as PaneId));
                let id = self
                    .open_text_as(Path::new(path), spot, false, encoding.as_deref())
                    .map_err(|e| format!("{e:#}"))?;
                // A buffer it made takes the base mode asked for.
                if let Some(mode) = opt_str("basemode")
                    && let Some(Kind::Text { buf, .. }) = self.panes.get(&id).map(|e| &e.kind)
                    && !known.contains(buf)
                {
                    let buf = *buf;
                    self.texts.get_mut(&buf).expect("made").basemode = Some(mode);
                    self.refocus_mode();
                }
                Ok((ScriptValue::Num(id as f64), Outcome::Redraw))
            }
            "reload" => {
                let buf = match args.first().and_then(ScriptValue::as_num) {
                    Some(b) => b as BufferId,
                    None => self.text_target(None)?.1,
                };
                self.text_reload(buf)?;
                Ok((ScriptValue::Null, Outcome::Redraw))
            }
            "new-buffer" => {
                let text = opt_str("text").unwrap_or_default();
                let path = opt_str("path").map(PathBuf::from);
                let info = path.map(|p| FileInfo {
                    path: std::path::absolute(&p).unwrap_or(p),
                    encoding: "utf-8".into(),
                    ..FileInfo::default()
                });
                let attrs = parse_attrs(opts.and_then(|o| o.get("attrs")))?;
                let buf = self.add_text_buffer(Text::from(text.as_str()), info);
                let t = self.texts.get_mut(&buf).expect("made");
                t.basemode = opt_str("basemode");
                t.attrs = attrs;
                Ok((ScriptValue::Num(buf as f64), Outcome::None))
            }
            _ => {
                let buf = match opts.and_then(|o| o.get("buffer")).and_then(ScriptValue::as_num) {
                    Some(b) => b as BufferId,
                    None => self.text_target(None)?.1,
                };
                let force = opts.and_then(|o| o.get("force")).is_some_and(|v| *v == ScriptValue::Bool(true));
                self.text_write(buf, opt_str("path").map(PathBuf::from), force)?;
                Ok((ScriptValue::Null, Outcome::Redraw))
            }
        }
    }

    fn text_primitive(
        &mut self,
        name: &str,
        args: &[ScriptValue],
        count: Option<usize>,
    ) -> Result<(ScriptValue, Outcome), String> {
        // The options object, last, may name the pane (a position or range
        // is not one).
        let opts = args
            .last()
            .filter(|v| matches!(v, ScriptValue::Map(_)) && v.get("line").is_none() && v.get("start").is_none());
        let pane = opts.and_then(|o| o.get("pane")).and_then(ScriptValue::as_num).map(|p| p as PaneId);
        let (pane, buf) = self.text_target(pane)?;
        let flag = |k: &str| opts.and_then(|o| o.get(k)).is_some_and(|v| *v == ScriptValue::Bool(true));
        let arg_pos = |i: usize| args.get(i).and_then(position);
        let arg_range = |i: usize| args.get(i).and_then(range);
        let num = |i: usize| args.get(i).and_then(ScriptValue::as_num);
        // A move of the cursor ends a typing step.
        if matches!(name, "move" | "set-cursor" | "set-selection" | "clear-selection") {
            self.texts.get_mut(&buf).expect("found").buf.undo_tree_mut().cursor_moved();
        }
        let t = &self.texts[&buf];
        let text = t.buf.text();
        let tv = match self.panes.get(&pane).map(|e| &e.kind) {
            Some(Kind::Text { view, .. }) => view,
            _ => return Err("not a text pane".into()),
        };
        let cursor = tv.view.cursor;
        let none = |v: ScriptValue| Ok((v, Outcome::None));
        let redrawn = Ok((ScriptValue::Null, Outcome::Redraw));
        match name {
            // Queries.
            "line-count" => none(ScriptValue::Num(text.line_count() as f64)),
            "line" => none(ScriptValue::Str(text.line(num(0).ok_or("which line?")? as usize))),
            "get-text" => {
                let (a, b) = arg_range(0).map_or((0, text.len_chars()), |r| text.offsets(r));
                none(ScriptValue::Str(text.slice(a, b)))
            }
            "cursor" => none(position_value(text.position(cursor.pos))),
            "selection" => none(match cursor.anchor {
                Some((a, kind)) => ScriptValue::map([
                    ("anchor", position_value(text.position(a))),
                    ("active", position_value(text.position(cursor.pos))),
                    ("kind", ScriptValue::Str(kind_name(kind).into())),
                ]),
                None => ScriptValue::Null,
            }),
            "offset-at" => none(ScriptValue::Num(text.utf16_offset(text.offset(arg_pos(0).ok_or("where?")?)) as f64)),
            "position-at" => {
                let units = num(0).ok_or("which offset?")?.max(0.0) as usize;
                none(position_value(text.position(text.from_utf16_offset(units))))
            }
            "info" => none(ScriptValue::map([
                ("buffer", ScriptValue::Num(buf as f64)),
                ("path", t.file.as_ref().map_or(ScriptValue::Null, |f| ScriptValue::Str(f.path.display().to_string()))),
                ("name", ScriptValue::Str(t.name.clone())),
                ("modified", ScriptValue::Bool(t.modified())),
                ("attrs", t.attrs_value()),
                ("encoding", ScriptValue::Str(t.file.as_ref().map_or("utf-8".into(), |f| f.encoding.clone()))),
                (
                    "lineEnding",
                    ScriptValue::Str(if t.buf.line_ending == LineEnding::CrLf { "crlf" } else { "lf" }.into()),
                ),
                ("version", ScriptValue::Num(t.buf.version() as f64)),
            ])),
            "position-of" => {
                let from = text.offset(arg_pos(0).ok_or("from where?")?);
                let unit = args.get(1).and_then(ScriptValue::as_str).ok_or("which unit?")?;
                let unit = ViewUnit::from_name(unit).ok_or_else(|| format!("no unit {unit:?}"))?;
                let n = num(2).unwrap_or(1.0) as i64;
                none(position_value(text.position(tv.view.position_of(&t.buf, from, unit, n).0)))
            }
            "search" | "search-all" => {
                let pattern = Pattern::new(args.first().and_then(ScriptValue::as_str).ok_or("which pattern?")?)?;
                let get = |k: &str| opts.and_then(|o| o.get(k));
                let mut so = SearchOptions {
                    from: get("from").and_then(position).map_or(cursor.pos, |p| text.offset(p)),
                    backward: flag("backward"),
                    wrap: flag("wrap"),
                    group: get("group").and_then(ScriptValue::as_num).map_or(0, |g| g as usize),
                    word: flag("word"),
                    lines: None,
                };
                if let Some(ScriptValue::List(l)) = get("lines")
                    && let [a, b] = l.as_slice()
                {
                    so.lines = Some((a.as_num().unwrap_or(0.0) as usize, b.as_num().unwrap_or(0.0) as usize));
                }
                let found = |m: &Found| {
                    ScriptValue::map([
                        ("range", range_value(text, m.start, m.end)),
                        (
                            "groups",
                            ScriptValue::List(
                                m.groups
                                    .iter()
                                    .map(|g| g.map_or(ScriptValue::Null, |(a, b)| range_value(text, a, b)))
                                    .collect(),
                            ),
                        ),
                    ])
                };
                if name == "search" {
                    let m = temzdf_texteditor::search(text, &pattern, &so, &t.buf.wordchars);
                    return none(m.as_ref().map_or(ScriptValue::Null, found));
                }
                let (a, b) = get("range").and_then(range).map_or((0, text.len_chars()), |r| text.offsets(r));
                let all = temzdf_texteditor::find_all(text, &pattern, a, b, &so, &t.buf.wordchars);
                none(ScriptValue::List(all.iter().map(found).collect()))
            }
            // Decorations and marks (a mark: no width, no style).
            "add-decoration" => {
                let (a, b) = text.offsets(arg_range(0).ok_or("which range?")?);
                let get = |k: &str| opts.and_then(|o| o.get(k));
                let gravity = |k: &str| match get(k).and_then(ScriptValue::as_str) {
                    Some("after") => temzdf_texteditor::Gravity::After,
                    _ => temzdf_texteditor::Gravity::Before,
                };
                use temzdf_texteditor::{VirtChunk, VirtPos};
                let virt_text = match get("virtText") {
                    None | Some(ScriptValue::Null) => Vec::new(),
                    Some(ScriptValue::List(l)) => l
                        .iter()
                        .map(|c| match c {
                            ScriptValue::Str(s) => Ok(VirtChunk { text: s.clone(), style: None }),
                            c => Ok(VirtChunk {
                                text: c
                                    .get("text")
                                    .and_then(ScriptValue::as_str)
                                    .ok_or("virtText: { text, style? }")?
                                    .to_owned(),
                                style: c.get("style").and_then(ScriptValue::as_str).map(str::to_owned),
                            }),
                        })
                        .collect::<Result<_, String>>()?,
                    Some(_) => return Err("virtText: a list of { text, style? }".into()),
                };
                let virt_pos = match get("virtTextPos").and_then(ScriptValue::as_str) {
                    None | Some("inline") => VirtPos::Inline,
                    Some("eol") => VirtPos::Eol,
                    Some("overlay") => VirtPos::Overlay,
                    Some(other) => return Err(format!("virtTextPos: no {other:?} (inline, eol, overlay)")),
                };
                let spec = temzdf_texteditor::MarkSpec {
                    namespace: get("namespace").and_then(ScriptValue::as_str).unwrap_or("").to_owned(),
                    style: get("style").and_then(ScriptValue::as_str).map(str::to_owned),
                    priority: get("priority").and_then(ScriptValue::as_num).map_or(0, |p| p as i32),
                    pane: get("showIn").and_then(ScriptValue::as_num).map(|p| p as u32),
                    start_gravity: gravity("startGravity"),
                    end_gravity: gravity("endGravity"),
                    expires: get("ttl")
                        .and_then(ScriptValue::as_num)
                        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms.max(0.0) as u64)),
                    virt_text,
                    virt_pos,
                };
                let id = self.texts.get_mut(&buf).expect("found").buf.marks_mut().add(a, b, spec);
                Ok((ScriptValue::Num(id as f64), Outcome::Redraw))
            }
            "decoration" | "decorations" => {
                let value = |m: &temzdf_texteditor::Mark| {
                    ScriptValue::map([
                        ("id", ScriptValue::Num(m.id as f64)),
                        ("range", range_value(text, m.start, m.end)),
                        ("deleted", ScriptValue::Bool(m.deleted)),
                        ("namespace", ScriptValue::Str(m.spec.namespace.clone())),
                        ("style", m.spec.style.clone().map_or(ScriptValue::Null, ScriptValue::Str)),
                        (
                            "virtText",
                            ScriptValue::List(
                                m.spec
                                    .virt_text
                                    .iter()
                                    .map(|c| {
                                        ScriptValue::map([
                                            ("text", ScriptValue::Str(c.text.clone())),
                                            ("style", c.style.clone().map_or(ScriptValue::Null, ScriptValue::Str)),
                                        ])
                                    })
                                    .collect(),
                            ),
                        ),
                        (
                            "virtTextPos",
                            ScriptValue::Str(
                                match m.spec.virt_pos {
                                    temzdf_texteditor::VirtPos::Inline => "inline",
                                    temzdf_texteditor::VirtPos::Eol => "eol",
                                    temzdf_texteditor::VirtPos::Overlay => "overlay",
                                }
                                .into(),
                            ),
                        ),
                    ])
                };
                if name == "decoration" {
                    let id = num(0).ok_or("which decoration?")? as u64;
                    return none(t.buf.marks().get(id).map_or(ScriptValue::Null, value));
                }
                let ns = opts.and_then(|o| o.get("namespace")).and_then(ScriptValue::as_str);
                let (a, b) = opts
                    .and_then(|o| o.get("range"))
                    .and_then(range)
                    .map_or((0, text.len_chars()), |r| text.offsets(r));
                let mut list: Vec<&temzdf_texteditor::Mark> = t
                    .buf
                    .marks()
                    .iter()
                    .filter(|m| ns.is_none_or(|n| m.spec.namespace == n) && m.start <= b && m.end >= a)
                    .collect();
                list.sort_by_key(|m| (m.start, m.id));
                none(ScriptValue::List(list.into_iter().map(value).collect()))
            }
            "move-decoration" => {
                let id = num(0).ok_or("which decoration?")? as u64;
                let (a, b) = text.offsets(arg_range(1).ok_or("to which range?")?);
                if !self.texts.get_mut(&buf).expect("found").buf.marks_mut().set(id, a, b) {
                    return Err(format!("no decoration {id}"));
                }
                redrawn
            }
            "delete-decoration" => {
                let id = num(0).ok_or("which decoration?")? as u64;
                self.texts.get_mut(&buf).expect("found").buf.marks_mut().remove(id);
                redrawn
            }
            "clear-namespace" => {
                let ns = args.first().and_then(ScriptValue::as_str).ok_or("which namespace?")?.to_owned();
                self.texts.get_mut(&buf).expect("found").buf.marks_mut().clear(&ns);
                redrawn
            }
            "find-pair" => {
                let at = arg_pos(0).map_or(cursor.pos, |p| text.offset(p));
                let ch =
                    |k: &str| opts.and_then(|o| o.get(k)).and_then(ScriptValue::as_str).and_then(|s| s.chars().next());
                let (open, close) = (ch("open").unwrap_or('('), ch("close").unwrap_or(')'));
                let dir = match opts.and_then(|o| o.get("direction")).and_then(ScriptValue::as_str) {
                    Some("forward") => Some(true),
                    Some("backward") => Some(false),
                    _ => None,
                };
                let found = temzdf_texteditor::find_pair(text, at, open, close, dir);
                none(found.map_or(ScriptValue::Null, |o| position_value(text.position(o))))
            }
            "wordchars" => none(ScriptValue::Str(t.buf.wordchars.class().to_owned())),
            "set-wordchars" => {
                let class = args.first().and_then(ScriptValue::as_str).ok_or("which class?")?;
                let w = temzdf_texteditor::WordChars::new(class)?;
                self.texts.get_mut(&buf).expect("found").buf.wordchars = w;
                none(ScriptValue::Null)
            }
            "visible-range" => {
                let (a, b) = tv.view.visible_range(&t.buf);
                none(range_value(text, a, b))
            }
            // Edits.
            "edit" | "insert" | "delete" | "replace" => {
                let edits: Vec<TextEdit> = match name {
                    "edit" => match args.first() {
                        Some(ScriptValue::List(list)) => list
                            .iter()
                            .map(|e| {
                                let r = e.get("range").and_then(range).ok_or("an edit needs a range")?;
                                let (a, b) = text.offsets(r);
                                Ok(TextEdit::new(a, b, e.get("text").and_then(ScriptValue::as_str).unwrap_or("")))
                            })
                            .collect::<Result<_, String>>()?,
                        _ => return Err("edits: [{ range, text }, ...]".into()),
                    },
                    "insert" => {
                        let s = args.first().and_then(ScriptValue::as_str).ok_or("what text?")?;
                        let at = arg_pos(1).map_or(cursor.pos, |p| text.offset(p));
                        vec![TextEdit::new(at, at, s)]
                    }
                    "delete" => {
                        let (a, b) = text.offsets(arg_range(0).ok_or("which range?")?);
                        vec![TextEdit::new(a, b, "")]
                    }
                    _ => {
                        let (a, b) = text.offsets(arg_range(0).ok_or("which range?")?);
                        vec![TextEdit::new(a, b, args.get(1).and_then(ScriptValue::as_str).unwrap_or(""))]
                    }
                };
                self.text_apply(pane, buf, edits, StepKind::Other)?;
                redrawn
            }
            // What typing does: replace the selection, else insert.
            "type" => {
                let s = args.first().and_then(ScriptValue::as_str).ok_or("what text?")?.to_owned();
                let focus = self.focus;
                self.focus = pane;
                let out = self.text_type(&s);
                self.focus = focus;
                out.map(|o| (ScriptValue::Null, o))
            }
            // Delete the selection, else from the cursor `count` units.
            "erase" => {
                let (a, b) = match cursor.selection() {
                    Some((a, b, SelectionKind::Char)) => (a, b),
                    _ => {
                        let unit = args.first().and_then(ScriptValue::as_str).unwrap_or("grapheme");
                        let unit = ViewUnit::from_name(unit).ok_or_else(|| format!("no unit {unit:?}"))?;
                        let n = num(1).unwrap_or(1.0) as i64 * count.unwrap_or(1) as i64;
                        let to = tv.view.position_of(&t.buf, cursor.pos, unit, n).0;
                        (cursor.pos.min(to), cursor.pos.max(to))
                    }
                };
                if let Some(v) = self.view_mut(pane) {
                    v.view.cursor = Cursor::at(a);
                }
                self.text_apply(pane, buf, vec![TextEdit::new(a, b, "")], StepKind::Other)?;
                redrawn
            }
            // The cursor and the selection.
            "move" => {
                let unit = args.first().and_then(ScriptValue::as_str).ok_or("which unit?")?;
                let unit = ViewUnit::from_name(unit).ok_or_else(|| format!("no unit {unit:?}"))?;
                let n = num(1).unwrap_or(1.0) as i64 * count.unwrap_or(1) as i64;
                let select = flag("select");
                let b = &self.texts[&buf].buf;
                if let Some(Kind::Text { view, .. }) = self.panes.get_mut(&pane).map(|e| &mut e.kind) {
                    view.view.move_cursor(b, unit, n, select);
                }
                redrawn
            }
            "set-cursor" => {
                let at = text.offset(arg_pos(0).ok_or("where?")?);
                let (select, keep) = (flag("select"), flag("keepColumn"));
                let b = &self.texts[&buf].buf;
                if let Some(Kind::Text { view, .. }) = self.panes.get_mut(&pane).map(|e| &mut e.kind) {
                    let want = view.view.cursor.want_col;
                    view.view.set_cursor(b, at, select);
                    if keep {
                        view.view.cursor.want_col = want;
                    }
                }
                redrawn
            }
            "set-selection" => {
                let (a, b) = (text.offset(arg_pos(0).ok_or("anchor?")?), text.offset(arg_pos(1).ok_or("active?")?));
                let kind = selection_kind(args.get(2).and_then(ScriptValue::as_str))?;
                let tb = &self.texts[&buf].buf;
                if let Some(Kind::Text { view, .. }) = self.panes.get_mut(&pane).map(|e| &mut e.kind) {
                    view.view.set_cursor(tb, b, false);
                    view.view.cursor.anchor = Some((a, kind));
                }
                redrawn
            }
            "clear-selection" => {
                if let Some(v) = self.view_mut(pane) {
                    v.view.cursor.anchor = None;
                }
                redrawn
            }
            "set-cursor-style" => {
                let shape = cursor_shape(args.first().and_then(ScriptValue::as_str).ok_or("which style?")?)?;
                if let Some(v) = self.view_mut(pane) {
                    v.shape = shape;
                }
                redrawn
            }
            // The view.
            "scroll" => {
                let n = num(0).ok_or("how many rows?")? as i64 * count.unwrap_or(1) as i64;
                let b = &self.texts[&buf].buf;
                if let Some(Kind::Text { view, .. }) = self.panes.get_mut(&pane).map(|e| &mut e.kind) {
                    view.view.scroll(b, n);
                }
                redrawn
            }
            "reveal" => {
                let at = arg_pos(0).map_or(cursor.pos, |p| text.offset(p));
                let how = match args.get(1).and_then(ScriptValue::as_str) {
                    None | Some("minimal") => Reveal::Minimal,
                    Some("center") => Reveal::Center,
                    Some("top") => Reveal::Top,
                    Some(other) => return Err(format!("no way {other:?} (minimal, center, top)")),
                };
                let b = &self.texts[&buf].buf;
                if let Some(Kind::Text { view, .. }) = self.panes.get_mut(&pane).map(|e| &mut e.kind) {
                    view.view.reveal(b, at, how);
                }
                redrawn
            }
            // Undo.
            "undo" | "redo" => {
                let mut changed = Vec::new();
                for _ in 0..count.unwrap_or(1).max(1) {
                    let t = self.texts.get_mut(&buf).expect("found");
                    let Some(edited) = (if name == "undo" { t.buf.undo() } else { t.buf.redo() }) else { break };
                    // The ranges it changed, in the text now.
                    let text = t.buf.text();
                    if let Some(last) = edited.batches.last() {
                        let mut delta = 0isize;
                        for c in last {
                            let start = (c.start as isize + delta) as usize;
                            changed.push(range_value(text, start, start + c.inserted_chars()));
                            delta += c.inserted_chars() as isize - c.removed_chars() as isize;
                        }
                    }
                    self.text_follow(buf, &edited, pane);
                    self.text_changed(buf, &edited, name);
                }
                Ok((ScriptValue::List(changed), Outcome::Redraw))
            }
            "undo-begin" => {
                self.texts.get_mut(&buf).expect("found").buf.undo_tree_mut().begin();
                none(ScriptValue::Null)
            }
            "undo-end" => {
                self.texts.get_mut(&buf).expect("found").buf.undo_tree_mut().end();
                none(ScriptValue::Null)
            }
            "undo-boundary" => {
                self.texts.get_mut(&buf).expect("found").buf.undo_tree_mut().split();
                none(ScriptValue::Null)
            }
            other => Err(format!("no command text.{other}")),
        }
    }
}

/// The mouse in a text pane (`m` in its content's coordinates): a click
/// puts the cursor there, dragging selects, the wheel scrolls three rows.
pub(super) fn mouse(
    t: &mut TextEntry,
    tv: &mut TextView,
    m: &termpix_core::input::MouseEvent,
    cell: termpix_core::geom::CellSize,
) -> Outcome {
    use termpix_core::input::{CoordUnit, MouseButton, MouseEventKind, ScrollDirection, ScrollUnit};
    let (col, row) = match m.unit {
        CoordUnit::Pixel => (m.x.div_euclid(cell.width.max(1) as i32), m.y.div_euclid(cell.height.max(1) as i32)),
        CoordUnit::Cell => (m.x, m.y),
    };
    let (col, row) = (col.max(0) as usize, row.max(0) as usize);
    match m.kind {
        MouseEventKind::Press(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
            let select = matches!(m.kind, MouseEventKind::Drag(_));
            let at = tv.view.offset_at_cell(&t.buf, col, row);
            tv.view.set_cursor(&t.buf, at, select);
            t.buf.undo_tree_mut().cursor_moved();
            Outcome::Redraw
        }
        MouseEventKind::Scroll(d @ (ScrollDirection::Up | ScrollDirection::Down)) => {
            tv.view.scroll(&t.buf, if d == ScrollDirection::Up { -3 } else { 3 });
            Outcome::Redraw
        }
        MouseEventKind::ScrollBy { dy, unit, .. } if dy != 0 => {
            let rows = match unit {
                ScrollUnit::Line => dy as f64 / 100.0 * 3.0,
                ScrollUnit::Pixel => dy as f64 / 100.0 / cell.height.max(1) as f64,
            };
            let rows = if rows.abs() < 1.0 { rows.signum() } else { rows.round() };
            tv.view.scroll(&t.buf, rows as i64);
            Outcome::Redraw
        }
        _ => Outcome::None,
    }
}

/// `text.set-cursor` as scripts write it (`text.setCursor`), for errors.
fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in name.chars() {
        match c {
            '-' => up = true,
            c if up => {
                out.extend(c.to_uppercase());
                up = false;
            }
            c => out.push(c),
        }
    }
    out
}

/// The `text.*` commands for the registry: (name, TypeScript signature,
/// documentation).
pub(super) const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "open-file",
        "(path: string, opts?: { basemode?: string, encoding?: string, pane?: number }): number",
        "Open a file as text in a new pane (its buffer if it is open); returns the pane. `basemode`: the base mode of panes on a buffer made now; `encoding`: an Encoding Standard label (shift_jis, euc-jp, ...) for a file that is not UTF-8 (nothing is guessed), written back in it; `pane`: show it in that pane instead (Vim's :e)",
    ),
    (
        "new-buffer",
        "(opts?: { text?: string, path?: string, basemode?: string, attrs?: BufferAttr[] }): number",
        "A new text buffer (no pane); with only `path`, a new file. `basemode`: the base mode of panes on it (never changed after); `attrs` its attributes (Vim's buftype, several at once): `nofile` never modified and written only to a path given, `unlisted` left out of buffers(). Returns the buffer",
    ),
    (
        "set-buffer-attrs",
        "(buffer: number, attrs: BufferAttr[]): void",
        "Set a text buffer's attributes (see newBuffer)",
    ),
    (
        "write",
        "(opts?: { path?: string, force?: boolean, buffer?: number }): void",
        "Write a text buffer (the focused pane's) to its file, or to `path`; a file changed on disk since it was read needs `force`",
    ),
    (
        "reload",
        "(buffer?: number): void",
        "Read a text buffer (the focused pane's) from its file again: the difference is one undoable edit, and the result the saved state",
    ),
    ("text.line-count", "(opts?: { pane?: number }): number", "Lines of the text pane's buffer"),
    ("text.line", "(line: number, opts?: { pane?: number }): string", "A line, without its line break"),
    (
        "text.get-text",
        "(range?: Range, opts?: { pane?: number }): string",
        "The text of a range (all of it by default)",
    ),
    ("text.cursor", "(opts?: { pane?: number }): Position", "Where the cursor is"),
    (
        "text.selection",
        "(opts?: { pane?: number }): { anchor: Position, active: Position, kind: \"char\" | \"line\" | \"block\" } | null",
        "The selection, if any (its active end is the cursor)",
    ),
    (
        "text.offset-at",
        "(pos: Position, opts?: { pane?: number }): number",
        "A position's UTF-16 offset from the start",
    ),
    ("text.position-at", "(offset: number, opts?: { pane?: number }): Position", "The position at a UTF-16 offset"),
    (
        "text.info",
        "(opts?: { pane?: number }): { buffer: number, path: string | null, name: string, modified: boolean, encoding: string, lineEnding: \"lf\" | \"crlf\", version: number }",
        "About the buffer",
    ),
    (
        "text.position-of",
        "(from: Position, unit: TextUnit, count?: number, opts?: { pane?: number }): Position",
        "Where `count` units from `from` is, without moving (negative counts go backwards)",
    ),
    ("text.visible-range", "(opts?: { pane?: number }): Range", "The text the pane shows"),
    (
        "text.search",
        "(pattern: string, opts?: { from?: Position, backward?: boolean, wrap?: boolean, lines?: [number, number], group?: number, word?: boolean, pane?: number }): { range: Range, groups: (Range | null)[] } | null",
        "Search with a Rust regex (^ and $ at lines): forward the first match starting at or after `from` (the cursor), backward the last one starting before it; `wrap` goes on from the other end; `lines` (zero based, inclusive) narrows it; `group` is the capture that counts as the match; `word` wants word boundaries of the buffer's word characters",
    ),
    (
        "text.search-all",
        "(pattern: string, opts?: { range?: Range, group?: number, word?: boolean, pane?: number }): { range: Range, groups: (Range | null)[] }[]",
        "Every match in a range (the whole text)",
    ),
    (
        "text.find-pair",
        "(at?: Position, opts?: { open?: string, close?: string, direction?: \"forward\" | \"backward\", pane?: number }): Position | null",
        "The matching bracket (Vim's %; ( and ) by default): from one on `open` forward, on `close` backward; with `direction`, the enclosing one that way. Nesting counts; strings and comments do not",
    ),
    ("text.wordchars", "(opts?: { pane?: number }): string", "The buffer's word characters, a regex character class"),
    (
        "text.set-wordchars",
        "(chars: string, opts?: { pane?: number }): void",
        "Set the buffer's word characters (Vim's iskeyword): one regex character class, e.g. [\\p{L}\\p{N}_]",
    ),
    (
        "get-var",
        "(scope: \"buffer\" | \"pane\" | \"tab\", name: string, id?: number): unknown",
        "A buffer's, pane's or tab page's variable (Vim's b:, w:, t:; the focused one's; tabs by number from 1); null when unset",
    ),
    (
        "set-var",
        "(scope: \"buffer\" | \"pane\" | \"tab\", name: string, value: unknown, id?: number): void",
        "Set one (JSON values; null unsets it). Held by the server: it outlives script host restarts and goes with its owner",
    ),
    (
        "text.add-decoration",
        "(range: Range, opts?: { style?: string, namespace?: string, priority?: number, showIn?: number, startGravity?: \"before\" | \"after\", endGravity?: \"before\" | \"after\", ttl?: number, virtText?: (VirtChunk | string)[], virtTextPos?: \"inline\" | \"eol\" | \"overlay\", pane?: number }): number",
        "Decorate a range (an empty one without a style is a mark): it follows edits, collapses and is flagged deleted when its text goes (undo restores it). `style` a highlight group, `showIn` a pane only, gravity where each end goes on an insertion exactly there, `ttl` milliseconds until it goes. `virtText` is text shown that is not in the buffer, at the range's start: `inline` between the characters (it takes columns; the cursor never goes in it), `eol` after the line's end, `overlay` over the text. Returns its id",
    ),
    ("text.decoration", "(id: number, opts?: { pane?: number }): Decoration | null", "A decoration or mark by id"),
    (
        "text.decorations",
        "(opts?: { namespace?: string, range?: Range, pane?: number }): Decoration[]",
        "The decorations of a namespace and range (all of them), in order",
    ),
    (
        "text.move-decoration",
        "(id: number, range: Range, opts?: { pane?: number }): void",
        "Move a decoration (no longer deleted)",
    ),
    ("text.delete-decoration", "(id: number, opts?: { pane?: number }): void", "Delete a decoration"),
    ("text.clear-namespace", "(namespace: string, opts?: { pane?: number }): void", "Delete a namespace's decorations"),
    (
        "highlight",
        "(name: string, style: { fg?: string | number, bg?: string | number, bold?: boolean, italic?: boolean, underline?: boolean, undercurl?: boolean, inverse?: boolean, strikethrough?: boolean, faint?: boolean, overline?: boolean }): void",
        "Define a highlight group for text panes: colours \"#rrggbb\", a palette index or \"default\" (left out: unchanged under it)",
    ),
    (
        "highlight-pattern",
        "(namespace: string, pattern: string | null, style?: string): void",
        "Paint a pattern's matches in every text pane with a highlight group (Search by default), one pattern per namespace; null removes it (hlsearch)",
    ),
    (
        "text.edit",
        "(edits: { range: Range, text: string }[], opts?: { pane?: number }): void",
        "Replace ranges (given in the text before the edit, not overlapping) at once",
    ),
    (
        "text.insert",
        "(text: string, at?: Position, opts?: { pane?: number }): void",
        "Insert text at a position (the cursor)",
    ),
    ("text.delete", "(range: Range, opts?: { pane?: number }): void", "Delete a range"),
    ("text.replace", "(range: Range, text: string, opts?: { pane?: number }): void", "Replace a range"),
    (
        "text.type",
        "(text: string, opts?: { pane?: number }): void",
        "Insert text as typed: it replaces the selection, and consecutive typing is one undo step",
    ),
    (
        "text.erase",
        "(unit?: TextUnit, count?: number, opts?: { pane?: number }): void",
        "Delete the selection, else from the cursor `count` units (negative: backwards)",
    ),
    (
        "text.move",
        "(unit: TextUnit, count?: number, opts?: { select?: boolean, pane?: number }): void",
        "Move the cursor `count` units (times the mapping's count); `select` extends the selection",
    ),
    (
        "text.set-cursor",
        "(pos: Position, opts?: { select?: boolean, keepColumn?: boolean, pane?: number }): void",
        "Put the cursor at a position; `keepColumn` keeps the column vertical moves aim for (Vim's cursor kept off a line's end)",
    ),
    (
        "text.set-selection",
        "(anchor: Position, active: Position, kind?: \"char\" | \"line\" | \"block\", opts?: { pane?: number }): void",
        "Select from `anchor` to `active` (the cursor)",
    ),
    ("text.clear-selection", "(opts?: { pane?: number }): void", "Drop the selection"),
    (
        "text.set-cursor-style",
        "(style: \"block\" | \"bar\" | \"underline\" | \"default\" | \"block-blink\" | \"bar-blink\" | \"underline-blink\", opts?: { pane?: number }): void",
        "The cursor's shape in this pane",
    ),
    ("text.scroll", "(rows: number, opts?: { pane?: number }): void", "Scroll by display rows (negative: up)"),
    (
        "text.reveal",
        "(pos?: Position, how?: \"minimal\" | \"center\" | \"top\", opts?: { pane?: number }): void",
        "Scroll so a position (the cursor) shows",
    ),
    ("text.undo", "(opts?: { pane?: number }): Range[]", "Undo a step; returns the ranges it changed"),
    ("text.redo", "(opts?: { pane?: number }): Range[]", "Redo a step along the current branch"),
    ("text.undo-begin", "(opts?: { pane?: number }): void", "Group what follows into one undo step (nestable)"),
    ("text.undo-end", "(opts?: { pane?: number }): void", "End a group begun with undoBegin"),
    ("text.undo-boundary", "(opts?: { pane?: number }): void", "End the undo step here, even inside a group"),
];
