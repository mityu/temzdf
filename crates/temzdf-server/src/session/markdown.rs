//! Markdown viewer panes (`doc/design/markdown-viewer.md`). A document is
//! read once per file and shared by the panes on it; its text mode is
//! drawn into `nomodifiable` text buffers, one per width the panes on it
//! have, so panes of one width share a buffer and a pane that changes
//! width moves to another (keeping its place by the source line).

use std::path::{Path, PathBuf};

use temzdf_markdown::{Deco, Document, Rendered};
use temzdf_protocol::script::ScriptValue;
use temzdf_scene::Effect;
use temzdf_texteditor::{MarkSpec, Text};

use super::text::TextView;
use super::{BufferId, Kind, Outcome, PaneId, Session, SessionEvent, Spot};

pub(super) type MdId = u32;

/// A Markdown file shown in panes.
pub(super) struct MdDoc {
    pub path: PathBuf,
    pub name: String,
    pub doc: Document,
    /// The text it was read from (lines map through a diff on a reload).
    source: String,
    /// The text mode drawn at each width a pane has.
    pub renders: Vec<MdRender>,
    /// The file's modification time when read.
    mtime: Option<std::time::SystemTime>,
    /// Its images read so far (by the URL or path written): their ids in
    /// the session's loaded images.
    images: std::collections::HashMap<String, u64>,
    _watcher: Option<Box<dyn std::any::Any + Send>>,
}

pub(super) struct MdRender {
    pub width: usize,
    pub buf: BufferId,
    pub drawn: Rendered,
}

/// The commands of Markdown viewers.
pub(super) const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "markdown.open",
        "(path: string, opts?: { pane?: number }): number",
        "Open a Markdown file in the viewer (its text mode), in a new pane or in `pane`; a file already open is shared. Returns the pane",
    ),
    (
        "markdown.follow",
        "(pane?: number): boolean",
        "Follow the link at the cursor of a Markdown viewer (the focused pane's): a URL to the link opener, `#anchor` to its heading, a .md or .pdf file in the same pane. Whether there was one",
    ),
    (
        "markdown.back",
        "(pane?: number): boolean",
        "Go back in a pane to the Markdown file it showed before following a link to another file. Whether there was one",
    ),
    (
        "markdown.goto",
        "(anchor: string, opts?: { pane?: number }): void",
        "Show the heading `anchor` names (as `#anchor` links do) at the top of a Markdown viewer",
    ),
    (
        "markdown.load-remote-images",
        "(pane?: number): number",
        "Fetch the http(s) images of the Markdown file a viewer shows (only local images are read otherwise), in the background; each shows as it arrives. Returns how many it fetches",
    ),
    (
        "markdown.outline",
        "(pane?: number): { level: number, text: string, anchor: string, line: number }[]",
        "The headings of the Markdown file a pane shows: level, text, anchor and source line (0-based)",
    ),
];

/// Math as text for the text mode (libtexprintf).
fn math(tex: &str) -> Option<Vec<String>> {
    texprintf::render(tex)
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown"))
}

impl Session {
    pub(super) fn markdown_command(
        &mut self,
        name: &str,
        args: &[ScriptValue],
    ) -> Option<Result<(ScriptValue, Outcome), String>> {
        let opts = args.iter().find(|v| matches!(v, ScriptValue::Map(_)));
        let pane_opt = |i: usize| -> PaneId {
            args.get(i)
                .and_then(ScriptValue::as_num)
                .or_else(|| opts.and_then(|o| o.get("pane")).and_then(ScriptValue::as_num))
                .map_or(self.focus, |p| p as PaneId)
        };
        Some(match name {
            "markdown.open" => (|| {
                let path = args.first().and_then(ScriptValue::as_str).ok_or("markdown.open: which file?")?;
                let spot = match opts.and_then(|o| o.get("pane")).and_then(ScriptValue::as_num) {
                    Some(p) => Spot::Replace(p as PaneId),
                    None => self.next_spot(),
                };
                let pane = self.md_open(Path::new(path), spot)?;
                Ok((ScriptValue::Num(pane as f64), Outcome::Redraw))
            })(),
            "markdown.follow" => {
                let pane = pane_opt(0);
                (|| {
                    let at = self.md_cursor(pane).ok_or("markdown.follow: not a Markdown viewer")?;
                    let followed = self.md_follow_at(pane, at)?;
                    Ok((ScriptValue::Bool(followed), Outcome::Redraw))
                })()
            }
            "markdown.back" => {
                let pane = pane_opt(0);
                self.md_back(pane).map(|b| (ScriptValue::Bool(b), Outcome::Redraw))
            }
            "markdown.goto" => {
                let pane = pane_opt(1);
                (|| {
                    let anchor = args.first().and_then(ScriptValue::as_str).ok_or("markdown.goto: which anchor?")?;
                    self.md_goto(pane, anchor)?;
                    Ok((ScriptValue::Null, Outcome::Redraw))
                })()
            }
            "markdown.load-remote-images" => {
                let pane = pane_opt(0);
                self.md_load_remote(pane).map(|n| (ScriptValue::Num(n as f64), Outcome::None))
            }
            "markdown.outline" => {
                let pane = pane_opt(0);
                (|| {
                    let md = self.md_of_pane(pane).ok_or("markdown.outline: not a Markdown viewer")?;
                    let list = self.md_docs[&md]
                        .doc
                        .headings
                        .iter()
                        .map(|h| {
                            ScriptValue::map([
                                ("level", ScriptValue::Num(f64::from(h.level))),
                                ("text", ScriptValue::Str(h.text.clone())),
                                ("anchor", ScriptValue::Str(h.anchor.clone())),
                                ("line", ScriptValue::Num(h.lines.start as f64)),
                            ])
                        })
                        .collect();
                    Ok((ScriptValue::List(list), Outcome::None))
                })()
            }
            _ => return None,
        })
    }

    /// Open `path` in the viewer at `spot`; returns the pane.
    pub(super) fn md_open(&mut self, path: &Path, spot: Spot) -> Result<PaneId, String> {
        let md = self.md_doc(path)?;
        // A width to start with; the pane's own once it is laid out
        // (`md_fit`).
        let width = match spot {
            Spot::Replace(p) => self.panes.get(&p).map_or(80, |e| e.content().w as usize),
            _ => self.geometry.cols as usize,
        };
        let buf = self.md_buffer(md, width.max(1));
        if let Spot::Replace(pane) = spot {
            self.show_in(pane, buf)?;
            self.md_fit(pane);
            return Ok(pane);
        }
        let id = self.alloc_id();
        let focus = self.takes_focus(&spot);
        let rect = self.place(id, spot).map_err(|e| format!("{e:#}"))?;
        self.text_pane(id, buf, rect, focus);
        Ok(id)
    }

    /// The document of `path`: the one open, or read now.
    fn md_doc(&mut self, path: &Path) -> Result<MdId, String> {
        let canonical = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some((&id, _)) = self.md_docs.iter().find(|(_, d)| d.path == canonical) {
            return Ok(id);
        }
        let source = std::fs::read_to_string(&canonical).map_err(|e| format!("{}: {e}", path.display()))?;
        let mtime = std::fs::metadata(&canonical).and_then(|m| m.modified()).ok();
        let id = self.alloc_id();
        let events = self.events.clone();
        let watcher = temzdf_pdfcore::workers::watch_file(&canonical, move || {
            events(SessionEvent::Markdown(id));
        })
        .ok()
        .map(|w| Box::new(w) as Box<dyn std::any::Any + Send>);
        let name =
            canonical.file_name().map_or_else(|| canonical.display().to_string(), |n| n.to_string_lossy().into_owned());
        self.md_docs.insert(
            id,
            MdDoc {
                path: canonical,
                name,
                doc: Document::parse(&source),
                source,
                renders: Vec::new(),
                mtime,
                images: Default::default(),
                _watcher: watcher,
            },
        );
        self.md_load_local(id);
        Ok(id)
    }

    /// Read the local images of `md` not read yet (those that fail show
    /// their alt text).
    fn md_load_local(&mut self, md: MdId) {
        let Some(d) = self.md_docs.get(&md) else { return };
        let dir = d.path.parent().unwrap_or(Path::new("/")).to_owned();
        let urls: Vec<String> =
            d.doc.image_urls().into_iter().filter(|u| !d.images.contains_key(u) && !is_remote(u)).collect();
        for url in urls {
            let path = dir.join(percent_decode(url.strip_prefix("file://").unwrap_or(&url)));
            if let Ok(id) = self.load_image(&path) {
                self.md_docs.get_mut(&md).expect("open").images.insert(url, id);
            }
        }
    }

    /// The images of `md` as decorations need them: URL, id and size.
    fn md_image_sizes(&self, md: MdId) -> std::collections::HashMap<String, (u64, u32, u32)> {
        self.md_docs[&md]
            .images
            .iter()
            .filter_map(|(url, id)| self.loaded_images.get(id).map(|l| (url.clone(), (*id, l.width, l.height))))
            .collect()
    }

    /// Every drawing of `md` decorated again (its images changed).
    fn md_redecorate(&mut self, md: MdId) {
        let sizes = self.md_image_sizes(md);
        let Some(d) = self.md_docs.get(&md) else { return };
        for r in &d.renders {
            if let Some(t) = self.texts.get_mut(&r.buf) {
                t.buf.marks_mut().clear("markdown");
                decorate(&mut t.buf, &r.drawn, &sizes);
            }
        }
    }

    /// Fetch the remote images of the document `pane` shows, in the
    /// background (`markdown.loadRemoteImages`): each shows as it arrives.
    fn md_load_remote(&mut self, pane: PaneId) -> Result<usize, String> {
        let md = self.md_of_pane(pane).ok_or("not a Markdown viewer")?;
        let d = &self.md_docs[&md];
        let urls: Vec<String> =
            d.doc.image_urls().into_iter().filter(|u| is_remote(u) && !d.images.contains_key(u)).collect();
        let n = urls.len();
        if n > 0 {
            let events = self.events.clone();
            std::thread::Builder::new()
                .name("markdown-images".into())
                .spawn(move || {
                    for url in urls {
                        let got = fetch(&url);
                        if !events(SessionEvent::MarkdownImage(md, url, got)) {
                            break;
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(n)
    }

    /// A remote image of `md` arrived (or failed to).
    pub(super) fn md_image_fetched(&mut self, md: MdId, url: String, got: Result<Vec<u8>, String>) -> Outcome {
        if !self.md_docs.contains_key(&md) {
            return Outcome::None;
        }
        let decoded = got.and_then(|bytes| {
            super::images::decode(
                &bytes,
                url.split(['?', '#']).next().is_some_and(|p| p.to_ascii_lowercase().ends_with(".svg")),
            )
            .ok_or_else(|| "not an image".to_owned())
        });
        match decoded {
            Ok((w, h, rgba)) => {
                let id = self.add_loaded(PathBuf::from(&url), w, h, rgba);
                self.md_docs.get_mut(&md).expect("open").images.insert(url, id);
                self.md_redecorate(md);
                Outcome::Redraw
            }
            Err(e) => {
                self.message(&format!("{url}: {e}"));
                Outcome::Redraw
            }
        }
    }

    /// The buffer showing document `md` at `width`: the one drawn, or drawn
    /// now.
    fn md_buffer(&mut self, md: MdId, width: usize) -> BufferId {
        if let Some(r) = self.md_docs[&md].renders.iter().find(|r| r.width == width) {
            return r.buf;
        }
        let d = &self.md_docs[&md];
        let drawn = temzdf_markdown::render(&d.doc, width, &math);
        let name = d.name.clone();
        let sizes = self.md_image_sizes(md);
        let buf = self.add_text_buffer(Text::from(drawn.text.as_str()), None);
        let t = self.texts.get_mut(&buf).expect("made");
        t.attrs = ["nofile", "unlisted", "nomodifiable"].map(str::to_owned).to_vec();
        t.name = name;
        t.markdown = Some(md);
        decorate(&mut t.buf, &drawn, &sizes);
        self.md_docs.get_mut(&md).expect("open").renders.push(MdRender { width, buf, drawn });
        buf
    }

    /// The document pane `pane` shows, if a viewer.
    pub(super) fn md_of_pane(&self, pane: PaneId) -> Option<MdId> {
        match &self.panes.get(&pane)?.kind {
            Kind::Text { buf, .. } => self.texts.get(buf)?.markdown,
            _ => None,
        }
    }

    fn md_render_of(&self, buf: BufferId) -> Option<&MdRender> {
        let md = self.texts.get(&buf)?.markdown?;
        self.md_docs.get(&md)?.renders.iter().find(|r| r.buf == buf)
    }

    /// A viewer pane follows its width: drawn again at it when it changed.
    pub(super) fn md_fit(&mut self, pane: PaneId) {
        let Some(e) = self.panes.get(&pane) else { return };
        let Kind::Text { buf, view } = &e.kind else { return };
        let Some(t) = self.texts.get(buf) else { return };
        let Some(md) = t.markdown else { return };
        let width = view.view.text_width(t.buf.text());
        let old = *buf;
        if self.md_render_of(old).is_some_and(|r| r.width == width) {
            return;
        }
        let new = self.md_buffer(md, width);
        self.md_move(pane, old, new);
        self.md_collect(md);
    }

    /// Pane `pane` shows buffer `new` (another drawing of the same
    /// document) in place of `old`, at the same source lines.
    fn md_move(&mut self, pane: PaneId, old: BufferId, new: BufferId) {
        let (Some(from), Some(to)) = (self.md_render_of(old), self.md_render_of(new)) else { return };
        let (from, to) = (from.drawn.sources.clone(), to.drawn.clone());
        let Some(t_old) = self.texts.get(&old) else { return };
        let old_text = t_old.buf.text().clone();
        let Some(e) = self.panes.get_mut(&pane) else { return };
        let Kind::Text { buf, view } = &mut e.kind else { return };
        let src = |offset: usize| from.get(old_text.line_of(offset)).copied().unwrap_or(0);
        let (cursor_src, top_src) = (src(view.view.cursor.pos), src(view.view.top));
        *buf = new;
        let Some(t) = self.texts.get(&new) else { return };
        let text = t.buf.text();
        view.view.cursor =
            temzdf_texteditor::Cursor::at(text.line_start(to.line_of_source(cursor_src).min(text.line_count() - 1)));
        view.view.top = text.line_start(to.line_of_source(top_src).min(text.line_count() - 1));
        view.view.top_skip = 0;
        view.view.relayout(&t.buf);
    }

    /// Forget the drawings of `md` no pane shows (one stays while any pane
    /// shows the document... or none, once no pane does).
    pub(super) fn md_collect(&mut self, md: MdId) {
        let shown: Vec<BufferId> = self
            .panes
            .values()
            .filter_map(|e| match &e.kind {
                Kind::Text { buf, .. } => Some(*buf),
                _ => None,
            })
            .collect();
        let Some(d) = self.md_docs.get_mut(&md) else { return };
        let (keep, gone): (Vec<MdRender>, Vec<MdRender>) = d.renders.drain(..).partition(|r| shown.contains(&r.buf));
        d.renders = keep;
        for r in gone {
            self.remove_buffer(r.buf);
        }
        if self.md_docs.get(&md).is_some_and(|d| d.renders.is_empty())
            && let Some(d) = self.md_docs.remove(&md)
        {
            // Its images, unless another document shows them.
            for id in d.images.into_values() {
                if !self.md_docs.values().any(|o| o.images.values().any(|i| *i == id))
                    && let Some(l) = self.loaded_images.remove(&id)
                {
                    self.images.remove(l.key);
                }
            }
        }
    }

    /// The watcher saw `md`'s file change: read and drawn again, each pane
    /// at the same source lines.
    pub(super) fn md_file_changed(&mut self, md: MdId) -> Outcome {
        let Some(d) = self.md_docs.get(&md) else { return Outcome::None };
        let mtime = std::fs::metadata(&d.path).and_then(|m| m.modified()).ok();
        if mtime.is_none() || mtime == d.mtime {
            return Outcome::None;
        }
        let Ok(source) = std::fs::read_to_string(&d.path) else { return Outcome::None };
        let doc = Document::parse(&source);
        let d = self.md_docs.get_mut(&md).expect("open");
        let new_source = source.clone();
        let old_source = std::mem::replace(&mut d.source, source);
        // Source lines of the old text in the new one.
        let moved = |line: usize| temzdf_markdown::map_line(&old_source, &new_source, line);
        d.mtime = mtime;
        d.doc = doc;
        let renders: Vec<(usize, BufferId, Vec<usize>)> =
            d.renders.iter().map(|r| (r.width, r.buf, r.drawn.sources.clone())).collect();
        self.md_load_local(md);
        let sizes = self.md_image_sizes(md);
        for (width, buf, old_sources) in renders {
            // Where each pane on it is, as source lines.
            let mut places = Vec::new();
            for (&pane, e) in &self.panes {
                if let Kind::Text { buf: b, view } = &e.kind
                    && *b == buf
                    && let Some(t) = self.texts.get(&buf)
                {
                    let text = t.buf.text();
                    let src = |o: usize| old_sources.get(text.line_of(o)).copied().unwrap_or(0);
                    places.push((pane, moved(src(view.view.cursor.pos)), moved(src(view.view.top))));
                }
            }
            let drawn = temzdf_markdown::render(&self.md_docs[&md].doc, width, &math);
            let Some(t) = self.texts.get_mut(&buf) else { continue };
            t.buf = temzdf_texteditor::Buffer::new(Text::from(drawn.text.as_str()), temzdf_texteditor::LineEnding::Lf);
            decorate(&mut t.buf, &drawn, &sizes);
            for (pane, cursor_src, top_src) in places {
                let Some(Kind::Text { view, .. }) = self.panes.get_mut(&pane).map(|e| &mut e.kind) else { continue };
                place_view(view, &t.buf, &drawn, cursor_src, top_src);
            }
            if let Some(r) = self.md_docs.get_mut(&md).and_then(|d| d.renders.iter_mut().find(|r| r.buf == buf)) {
                r.drawn = drawn;
            }
        }
        Outcome::Redraw
    }

    /// The cursor's offset in viewer `pane`.
    fn md_cursor(&self, pane: PaneId) -> Option<usize> {
        self.md_of_pane(pane)?;
        match &self.panes.get(&pane)?.kind {
            Kind::Text { view, .. } => Some(view.view.cursor.pos),
            _ => None,
        }
    }

    /// Follow the link at offset `at` of viewer `pane`, if there is one.
    pub(super) fn md_follow_at(&mut self, pane: PaneId, at: usize) -> Result<bool, String> {
        let Some(Kind::Text { buf, .. }) = self.panes.get(&pane).map(|e| &e.kind) else { return Ok(false) };
        let Some(r) = self.md_render_of(*buf) else { return Ok(false) };
        let target = r.drawn.decos.iter().find_map(|d| match d {
            Deco::Link { start, end, target } if (*start..*end).contains(&at) => Some(target.clone()),
            _ => None,
        });
        let Some(target) = target else { return Ok(false) };
        self.md_follow(pane, &target)?;
        Ok(true)
    }

    /// Follow link `target` from viewer `pane`.
    fn md_follow(&mut self, pane: PaneId, target: &str) -> Result<(), String> {
        if let Some(anchor) = target.strip_prefix('#') {
            return self.md_goto(pane, anchor);
        }
        let is_uri = target.contains("://") || target.starts_with("mailto:");
        let md = self.md_of_pane(pane).ok_or("not a Markdown viewer")?;
        if is_uri {
            self.effects.push(Effect::OpenUri { uri: target.to_owned(), command: None });
            return Ok(());
        }
        // A file beside this one (and maybe an anchor in it).
        let (file, anchor) = target.split_once('#').map_or((target, None), |(f, a)| (f, Some(a)));
        let file = percent_decode(file);
        let here = self.md_docs[&md].path.clone();
        let path = here.parent().unwrap_or(Path::new("/")).join(&file);
        let is_pdf = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
        if !is_markdown(&path) && !is_pdf {
            let uri = format!("file://{}", path.display());
            self.effects.push(Effect::OpenUri { uri, command: None });
            return Ok(());
        }
        let line = self.md_source_line(pane).unwrap_or(0);
        if is_pdf {
            self.open_at(&path, Spot::Replace(pane)).map_err(|e| format!("{e:#}"))?;
        } else {
            self.md_open(&path, Spot::Replace(pane))?;
            if let Some(a) = anchor {
                let _ = self.md_goto(pane, a);
            }
        }
        self.md_history.entry(pane).or_default().push((here, line));
        self.md_collect(md);
        Ok(())
    }

    /// The source line at the top of viewer `pane`.
    fn md_source_line(&self, pane: PaneId) -> Option<usize> {
        let Kind::Text { buf, view } = &self.panes.get(&pane)?.kind else { return None };
        let r = self.md_render_of(*buf)?;
        let t = self.texts.get(buf)?;
        r.drawn.sources.get(t.buf.text().line_of(view.view.top)).copied()
    }

    /// Back to the file pane `pane` showed before following a link.
    fn md_back(&mut self, pane: PaneId) -> Result<bool, String> {
        let Some((path, line)) = self.md_history.get_mut(&pane).and_then(Vec::pop) else { return Ok(false) };
        let before = self.md_of_pane(pane);
        self.md_open(&path, Spot::Replace(pane))?;
        if let Some(Kind::Text { buf, view }) = self.panes.get_mut(&pane).map(|e| &mut e.kind)
            && let Some(t) = self.texts.get(buf)
        {
            let md = t.markdown.expect("a viewer");
            if let Some(r) = self.md_docs[&md].renders.iter().find(|r| r.buf == *buf) {
                place_view(view, &t.buf, &r.drawn, line, line);
            }
        }
        if let Some(md) = before {
            self.md_collect(md);
        }
        Ok(true)
    }

    /// Show the heading `anchor` names at the top of viewer `pane`.
    fn md_goto(&mut self, pane: PaneId, anchor: &str) -> Result<(), String> {
        let Some(Kind::Text { buf, view }) = self.panes.get_mut(&pane).map(|e| &mut e.kind) else {
            return Err("not a Markdown viewer".into());
        };
        let t = self.texts.get(buf).ok_or("not a Markdown viewer")?;
        let md = t.markdown.ok_or("not a Markdown viewer")?;
        let r = self.md_docs[&md].renders.iter().find(|r| r.buf == *buf).ok_or("not a Markdown viewer")?;
        let line = r.drawn.anchor_line(anchor).ok_or_else(|| format!("no heading #{anchor}"))?;
        let at = t.buf.text().line_start(line);
        view.view.set_cursor(&t.buf, at, false);
        view.view.reveal(&t.buf, at, temzdf_texteditor::Reveal::Top);
        Ok(())
    }
}

/// The view at source lines `cursor` and `top` of a drawing.
fn place_view(view: &mut TextView, buf: &temzdf_texteditor::Buffer, drawn: &Rendered, cursor: usize, top: usize) {
    let text = buf.text();
    let last = text.line_count() - 1;
    view.view.cursor = temzdf_texteditor::Cursor::at(text.line_start(drawn.line_of_source(cursor).min(last)));
    view.view.top = text.line_start(drawn.line_of_source(top).min(last));
    view.view.top_skip = 0;
    view.view.relayout(buf);
}

/// The drawing's styles and images as decorations of `buf`; `images`: the
/// images read, by URL (id, width, height).
fn decorate(
    buf: &mut temzdf_texteditor::Buffer,
    drawn: &Rendered,
    images: &std::collections::HashMap<String, (u64, u32, u32)>,
) {
    use temzdf_texteditor::{Conceal, VirtImage, VirtLines};
    let marks = buf.marks_mut();
    let spec = || MarkSpec { namespace: "markdown".into(), ..Default::default() };
    for d in &drawn.decos {
        match d {
            Deco::Style { start, end, group } => {
                marks.add(*start, *end, MarkSpec { style: Some((*group).to_owned()), ..spec() });
            }
            Deco::Image { start, end, url, alt, block } => {
                let Some(&(id, width, height)) = images.get(url) else { continue };
                let image = VirtImage { id, width, height, alt: alt.clone(), ..Default::default() };
                let s = if *block {
                    // Below its line, the alt text there hidden (shown on
                    // the cursor's line).
                    MarkSpec {
                        conceal: Some(Conceal::default()),
                        virt_lines: Some(VirtLines { text: Vec::new(), image: Some(image), above: false }),
                        ..spec()
                    }
                } else {
                    MarkSpec { conceal: Some(Conceal { image: Some(image), ..Conceal::default() }), ..spec() }
                };
                marks.add(*start, *end, s);
            }
            Deco::Link { .. } => {}
        }
    }
}

fn is_remote(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Images fetched over the network are at most this large.
const MAX_FETCH: u64 = 32 << 20;

/// Fetch `url` (an image), within a time and size limit.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(20))).build().into();
    let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
    response.body_mut().with_config().limit(MAX_FETCH).read_to_vec().map_err(|e| e.to_string())
}

/// `%xx` escapes in a link's path decoded.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(h) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
