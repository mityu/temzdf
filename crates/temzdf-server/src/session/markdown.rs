//! Markdown viewer panes (`doc/design/markdown-viewer.md`). A document is
//! read once per file and shared by the panes on it; its text mode is
//! drawn into `nomodifiable` text buffers, one per width the panes on it
//! have, so panes of one width share a buffer and a pane that changes
//! width moves to another (keeping its place by the source line).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use temzdf_markdown::{Deco, Document, Rendered};
use temzdf_mdview::{Strip, StripJob, StripKey};
use temzdf_protocol::script::ScriptValue;
use temzdf_scene::Effect;
use temzdf_scene::tiles::{Job, TileCache, Want, WorkPool};
use temzdf_texteditor::{MarkSpec, Text};

use super::text::TextView;
use super::{BufferId, Kind, Outcome, PaneId, Session, SessionEvent, Spot};

pub(super) type MdId = u32;

/// Bytes of painted strips a document keeps (32 strips of a 1800 pixel
/// wide page, about).
const STRIP_BUDGET: usize = 64 << 20;

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
    /// Remote images fetched (`markdown.loadRemoteImages`), for the
    /// graphics mode's pages.
    fetched: std::collections::HashMap<String, std::sync::Arc<Vec<u8>>>,
    /// The graphics mode's HTML (made when a page first needs it).
    body: Option<String>,
    /// The graphics mode laid out for each width and zoom panes have.
    pub pages: Vec<temzdf_mdview::Page>,
    /// The pages' painted strips (and those being painted), shared by the
    /// panes on them.
    pub strips: TileCache<StripKey>,
    _watcher: Option<Box<dyn std::any::Any + Send>>,
}

impl MdDoc {
    /// The page laid out at `key` (width, zoom), if made.
    pub fn page(&self, key: (u32, f32)) -> Option<&temzdf_mdview::Page> {
        self.pages.iter().find(|p| (p.width, p.zoom) == key)
    }

    /// The page at `key`, laid out now if it was not.
    pub fn page_mut(&mut self, key: (u32, f32)) -> Option<&mut temzdf_mdview::Page> {
        if let Some(i) = self.pages.iter().position(|p| (p.width, p.zoom) == key) {
            return self.pages.get_mut(i);
        }
        let body = self.body.get_or_insert_with(|| temzdf_markdown::html::body(&self.doc, &temzdf_mdview::math::svg));
        let dir = self.path.parent().unwrap_or(Path::new("/"));
        let fetched: temzdf_mdview::Fetched = std::sync::Arc::new(self.fetched.clone());
        self.pages.push(temzdf_mdview::Page::new(body, dir, key.0, 1000, key.1, fetched));
        self.pages.last_mut()
    }

    /// The page at `key` (laid out now if it was not) and the strips.
    pub fn page_and_strips(&mut self, key: (u32, f32)) -> Option<(&mut temzdf_mdview::Page, &mut TileCache<StripKey>)> {
        self.page_mut(key)?;
        let i = self.pages.iter().position(|p| (p.width, p.zoom) == key)?;
        Some((&mut self.pages[i], &mut self.strips))
    }

    /// These pages go, and their strips.
    fn drop_pages(&mut self, gone: Vec<temzdf_mdview::Page>, images: &mut temzdf_scene::Images) {
        let ids: Vec<_> = gone.iter().map(|p| p.id).collect();
        self.strips.drop_tiles(images, |k| ids.contains(&k.page));
    }

    /// Pages drawn again from the document (it or its images changed).
    fn relayout(&mut self, images: &mut temzdf_scene::Images) {
        let gone = std::mem::take(&mut self.pages);
        self.drop_pages(gone, images);
        self.body = None;
    }
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
        "markdown.set-mode",
        "(mode: \"text\" | \"graphics\", opts?: { pane?: number }): void",
        "Show a Markdown viewer (the focused pane) as text (a text pane: Vim's keys, selecting and copying) or in pixels like a PDF page, at the same place",
    ),
    (
        "markdown.mode",
        "(pane?: number): \"text\" | \"graphics\" | null",
        "The mode a pane shows its Markdown file in (null: not a Markdown viewer)",
    ),
    (
        "markdown.scroll",
        "(rows: number, opts?: { pane?: number }): void",
        "Graphics mode: scroll by rows of text (negative: up; a mapping's count multiplies)",
    ),
    (
        "markdown.page",
        "(pages: number, opts?: { pane?: number }): void",
        "Graphics mode: scroll by screenfuls (negative: up; a mapping's count multiplies)",
    ),
    ("markdown.top", "(pane?: number): void", "Graphics mode: to the top of the page"),
    ("markdown.bottom", "(pane?: number): void", "Graphics mode: to the bottom of the page"),
    (
        "markdown.zoom",
        "(how: \"in\" | \"out\" | \"reset\", opts?: { pane?: number }): void",
        "Graphics mode: zoom a step in or out, or back to 100%, at the same place",
    ),
    (
        "markdown.search-next",
        "(dir?: number, opts?: { pane?: number }): boolean",
        "Graphics mode: the next match of the search (dir < 0: the one before), around the ends. Whether there was one",
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
        count: Option<usize>,
        now: Instant,
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
            "markdown.set-mode" => {
                let pane = pane_opt(1);
                (|| {
                    let mode = args.first().and_then(ScriptValue::as_str).ok_or("markdown.setMode: which mode?")?;
                    self.md_set_mode(pane, mode)?;
                    Ok((ScriptValue::Null, Outcome::Redraw))
                })()
            }
            "markdown.mode" => {
                let pane = pane_opt(0);
                Ok((self.md_mode(pane).map_or(ScriptValue::Null, |m| ScriptValue::Str(m.into())), Outcome::None))
            }
            "markdown.scroll"
            | "markdown.page"
            | "markdown.top"
            | "markdown.bottom"
            | "markdown.zoom"
            | "markdown.search-next" => {
                let numeric = matches!(name, "markdown.scroll" | "markdown.page" | "markdown.search-next");
                let pane = pane_opt(if numeric || name == "markdown.zoom" { 1 } else { 0 });
                let n = args.first().and_then(ScriptValue::as_num).unwrap_or(1.0) * count.unwrap_or(1) as f64;
                let row = f64::from(self.geometry.cell.height);
                if name == "markdown.zoom" {
                    let dir = match args.first().and_then(ScriptValue::as_str) {
                        Some("in") => 1,
                        Some("out") => -1,
                        Some("reset") => 0,
                        _ => return Some(Err("markdown.zoom: in, out or reset?".into())),
                    };
                    return Some(self.md_zoom(pane, dir).map(|()| (ScriptValue::Null, Outcome::Redraw)));
                }
                let Some((p, page)) = self.md_graphics(pane) else {
                    return Some(Err(format!("{name}: not a Markdown viewer in its graphics mode")));
                };
                let result = match name {
                    "markdown.scroll" => {
                        p.scroll_by(page, n * row, now);
                        ScriptValue::Null
                    }
                    "markdown.page" => {
                        p.page_by(page, n, now);
                        ScriptValue::Null
                    }
                    "markdown.top" => {
                        p.glide_to(page, 0.0, now);
                        ScriptValue::Null
                    }
                    "markdown.bottom" => {
                        p.glide_to(page, f64::MAX, now);
                        ScriptValue::Null
                    }
                    _ => ScriptValue::Bool(p.search_next(page, if n < 0.0 { -1 } else { 1 })),
                };
                Ok((result, Outcome::Redraw))
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

    /// Hand a frame's strips to paint (from every viewer, most urgent
    /// first) to the threads; strips asked for twice are painted once.
    /// Strips over each document's budget go.
    pub(super) fn md_dispatch(&mut self, wants: Vec<Want<StripJob>>) {
        if wants.is_empty() && self.md_pool.is_none() {
            return;
        }
        if self.md_pool.is_none() {
            let events = self.events.clone();
            let threads = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4));
            match WorkPool::new("temzdf-markdown", threads, Arc::new(move |s| events(SessionEvent::MarkdownStrip(s)))) {
                Ok(pool) => self.md_pool = Some(pool),
                Err(e) => {
                    self.message(&format!("markdown: no threads to paint with: {e}"));
                    return;
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        let wants: Vec<Want<StripJob>> = wants
            .into_iter()
            .filter(|w| {
                seen.insert(match w {
                    Want::New(j) => j.key(),
                    Want::Queued(k) => *k,
                })
            })
            .collect();
        for w in &wants {
            if let Want::New(j) = w
                && let Some(d) = self.md_docs.get_mut(&j.tag)
            {
                d.strips.mark_in_flight(j.key);
            }
        }
        let dropped = self.md_pool.as_ref().map(|p| p.replace_wanted(wants)).unwrap_or_default();
        for (md, key) in dropped {
            if let Some(d) = self.md_docs.get_mut(&md) {
                d.strips.cancelled(key);
            }
        }
        for d in self.md_docs.values_mut() {
            d.strips.evict(&mut self.images);
        }
    }

    /// A strip was painted: shown, unless its page went meanwhile.
    pub(super) fn md_strip_painted(&mut self, s: Strip) -> Outcome {
        let Some(d) = self.md_docs.get_mut(&s.tag) else { return Outcome::None };
        if !d.pages.iter().any(|p| p.id == s.key.page) {
            d.strips.cancelled(s.key);
            return Outcome::None;
        }
        if d.strips.rendered(&mut self.images, s.key, s.width, s.rows, s.pixels) {
            Outcome::Redraw
        } else {
            Outcome::None
        }
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
        // A graphics-mode viewer goes on in it; others as the option says.
        let graphics = match spot {
            Spot::Replace(p) => self.panes.get(&p).is_some_and(|e| matches!(e.kind, Kind::Markdown { .. })),
            _ => self.options.markdownmode == "graphics",
        };
        let pane = if let Spot::Replace(pane) = spot {
            self.show_in(pane, buf)?;
            self.md_fit(pane);
            pane
        } else {
            let id = self.alloc_id();
            let focus = self.takes_focus(&spot);
            let rect = self.place(id, spot).map_err(|e| format!("{e:#}"))?;
            self.text_pane(id, buf, rect, focus);
            id
        };
        if graphics {
            self.md_set_mode(pane, "graphics")?;
        }
        Ok(pane)
    }

    /// Lay `md`'s pages out again after `change`, its graphics panes at the
    /// same source lines.
    fn md_relayout_keeping_place(&mut self, md: MdId, change: impl FnOnce(&mut MdDoc)) {
        let tops: Vec<(PaneId, usize)> = self
            .panes
            .iter()
            .filter_map(|(&id, e)| match &e.kind {
                Kind::Markdown { pane, doc } if *doc == md => {
                    self.md_docs.get(&md)?.page(pane.page_key()).map(|p| (id, pane.top_line(p)))
                }
                _ => None,
            })
            .collect();
        let Some(d) = self.md_docs.get_mut(&md) else { return };
        change(d);
        d.relayout(&mut self.images);
        for (id, line) in tops {
            if let Some((pane, page)) = self.md_graphics(id) {
                pane.show_line(page, line);
            }
        }
    }

    /// Graphics pane `id` and its page (laid out if it was not).
    pub(super) fn md_graphics(&mut self, id: PaneId) -> Option<(&mut temzdf_mdview::MdPane, &mut temzdf_mdview::Page)> {
        let Kind::Markdown { pane, doc } = &mut self.panes.get_mut(&id)?.kind else { return None };
        let page = self.md_docs.get_mut(doc)?.page_mut(pane.page_key())?;
        Some((pane, page))
    }

    /// Pane `id`'s size in device pixels.
    fn pane_pixels(&self, id: PaneId) -> Option<(u32, u32)> {
        let c = self.panes.get(&id)?.content();
        let cell = self.geometry.cell;
        Some((c.w * cell.width, c.h * cell.height))
    }

    /// A graphics pane follows its size: laid out again for a new width,
    /// at the same source line.
    pub(super) fn md_fit_graphics(&mut self, id: PaneId) {
        let Some((w, h)) = self.pane_pixels(id) else { return };
        let Some(Kind::Markdown { pane, doc }) = self.panes.get(&id).map(|e| &e.kind) else { return };
        if (pane.width, pane.height) == (w, h) {
            return;
        }
        let doc = *doc;
        let line = self.md_docs.get(&doc).and_then(|d| d.page(pane.page_key())).map(|p| pane.top_line(p));
        if let Some(Kind::Markdown { pane, .. }) = self.panes.get_mut(&id).map(|e| &mut e.kind) {
            pane.resize(w, h);
        }
        if let (Some(line), Some((pane, page))) = (line, self.md_graphics(id)) {
            pane.show_line(page, line);
        }
        self.md_collect(doc);
    }

    /// Show viewer `pane` in `mode` ("text" or "graphics"), at the same
    /// source line.
    pub(super) fn md_set_mode(&mut self, pane: PaneId, mode: &str) -> Result<(), String> {
        let md = match self.panes.get(&pane).map(|e| &e.kind) {
            Some(Kind::Markdown { doc, .. }) => *doc,
            Some(Kind::Text { buf, .. }) => {
                self.texts.get(buf).and_then(|t| t.markdown).ok_or("not a Markdown viewer")?
            }
            _ => return Err("not a Markdown viewer".into()),
        };
        match (mode, self.md_mode(pane)) {
            ("text", Some("text")) | ("graphics", Some("graphics")) => return Ok(()),
            ("text" | "graphics", _) => {}
            (other, _) => return Err(format!("no mode {other:?} (text, graphics)")),
        }
        let line = self.md_top_line(pane).unwrap_or(0);
        if mode == "graphics" {
            let (w, h) = self.pane_pixels(pane).ok_or("no pane")?;
            let mut view = temzdf_mdview::MdPane::new(w, h);
            view.set_scroll_config(self.options.scroll());
            let kind = Kind::Markdown { pane: Box::new(view), doc: md };
            let e = self.panes.get_mut(&pane).ok_or("no pane")?;
            let old = std::mem::replace(&mut e.kind, kind);
            if let Some((p, page)) = self.md_graphics(pane) {
                p.show_line(page, line);
            }
            self.hide(old);
            if pane == self.focus {
                self.refocus_mode();
            }
        } else {
            let width = self.panes.get(&pane).map_or(80, |e| e.content().w as usize);
            let buf = self.md_buffer(md, width.max(1));
            self.show_in(pane, buf)?;
            self.md_fit(pane);
            if let Some(Kind::Text { buf, view }) = self.panes.get_mut(&pane).map(|e| &mut e.kind)
                && let Some(t) = self.texts.get(buf)
                && let Some(r) = self.md_docs.get(&md).and_then(|d| d.renders.iter().find(|r| r.buf == *buf))
            {
                place_view(view, &t.buf, &r.drawn, line, line);
            }
        }
        Ok(())
    }

    /// Zoom graphics pane `pane` a step (`dir` > 0 in, < 0 out, 0 back to
    /// 100%), at the same place.
    pub(super) fn md_zoom(&mut self, pane: PaneId, dir: i32) -> Result<(), String> {
        let line = self.md_top_line(pane);
        let Some(Kind::Markdown { pane: p, doc }) = self.panes.get_mut(&pane).map(|e| &mut e.kind) else {
            return Err("markdown.zoom: not a Markdown viewer in its graphics mode".into());
        };
        p.zoom_step(dir);
        let doc = *doc;
        if let (Some(line), Some((p, page))) = (line, self.md_graphics(pane)) {
            p.show_line(page, line);
        }
        self.md_collect(doc);
        Ok(())
    }

    /// Search in graphics pane `pane` (`searchFor`, `searchPreview`,
    /// `searchCancel` there): `None` when it is not one.
    pub(super) fn md_search(&mut self, pane: PaneId, how: &str, query: &str) -> Option<bool> {
        let (p, page) = self.md_graphics(pane)?;
        Some(match how {
            "search-for" => p.search_for(page, query),
            "search-preview" => p.preview(page, query),
            _ => {
                p.search_cancel(page);
                true
            }
        })
    }

    /// The mode viewer `pane` is in.
    pub(super) fn md_mode(&self, pane: PaneId) -> Option<&'static str> {
        match &self.panes.get(&pane)?.kind {
            Kind::Markdown { .. } => Some("graphics"),
            Kind::Text { buf, .. } if self.texts.get(buf)?.markdown.is_some() => Some("text"),
            _ => None,
        }
    }

    /// The source line at the top of viewer `pane`, in either mode.
    pub(super) fn md_top_line(&self, pane: PaneId) -> Option<usize> {
        match &self.panes.get(&pane)?.kind {
            Kind::Markdown { pane: p, doc } => self.md_docs.get(doc)?.page(p.page_key()).map(|page| p.top_line(page)),
            Kind::Text { .. } => self.md_source_line(pane),
            _ => None,
        }
    }

    /// The status line of a graphics pane: its file (or the link under the
    /// pointer), how far down it is and its zoom.
    pub(super) fn md_status(&self, pane: &temzdf_mdview::MdPane, doc: MdId) -> termpix_ui::StatusBar {
        let Some(d) = self.md_docs.get(&doc) else { return termpix_ui::StatusBar::default() };
        let percent = d.page(pane.page_key()).map_or(0, |p| pane.percent(p));
        let zoom = if (pane.zoom - 1.0).abs() > 1e-3 { format!("  {:.0}%z", pane.zoom * 100.0) } else { String::new() };
        let search = match (&pane.search.current, pane.search.hits.len()) {
            (Some(i), n) if n > 0 => format!("  [{}/{n}]", i + 1),
            (_, 0) if !pane.search.query.is_empty() => "  [no match]".to_owned(),
            _ => String::new(),
        };
        termpix_ui::StatusBar {
            left: pane.hovered().map_or_else(|| format!(" {}", d.name), |l| format!(" {l}")),
            right: format!("markdown{search}{zoom}  {percent}% "),
            ..Default::default()
        }
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
                fetched: Default::default(),
                body: None,
                pages: Vec::new(),
                strips: TileCache::new(STRIP_BUDGET, temzdf_mdview::pane::STRIP_LAYERS),
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
        let mut fetched_bytes = got.as_ref().ok().cloned();
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
                self.md_docs.get_mut(&md).expect("open").images.insert(url.clone(), id);
                self.md_redecorate(md);
                self.md_relayout_keeping_place(md, |d| {
                    if let Some(bytes) = fetched_bytes.take() {
                        d.fetched.insert(url, std::sync::Arc::new(bytes));
                    }
                });
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
    pub(super) fn md_buffer(&mut self, md: MdId, width: usize) -> BufferId {
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
        let keys: Vec<(u32, f32)> = self
            .panes
            .values()
            .filter_map(|e| match &e.kind {
                Kind::Markdown { pane, doc } if *doc == md => Some(pane.page_key()),
                _ => None,
            })
            .collect();
        let Some(d) = self.md_docs.get_mut(&md) else { return };
        let (keep, gone): (Vec<MdRender>, Vec<MdRender>) = d.renders.drain(..).partition(|r| shown.contains(&r.buf));
        d.renders = keep;
        let (pages, unused): (Vec<_>, Vec<_>) = d.pages.drain(..).partition(|p| keys.contains(&(p.width, p.zoom)));
        d.pages = pages;
        d.drop_pages(unused, &mut self.images);
        for r in gone {
            self.remove_buffer(r.buf);
        }
        if self.md_docs.get(&md).is_some_and(|d| d.renders.is_empty() && keys.is_empty())
            && let Some(mut d) = self.md_docs.remove(&md)
        {
            d.strips.clear(&mut self.images);
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
        // Graphics panes: where they are, then laid out again.
        let tops: Vec<(PaneId, usize)> = self
            .panes
            .iter()
            .filter_map(|(&id, e)| match &e.kind {
                Kind::Markdown { pane, doc } if *doc == md => {
                    self.md_docs[&md].page(pane.page_key()).map(|p| (id, moved(pane.top_line(p))))
                }
                _ => None,
            })
            .collect();
        self.md_docs.get_mut(&md).expect("open").relayout(&mut self.images);
        for (id, line) in tops {
            if let Some((pane, page)) = self.md_graphics(id) {
                pane.show_line(page, line);
            }
        }
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
    pub(super) fn md_follow(&mut self, pane: PaneId, target: &str) -> Result<(), String> {
        if let Some(anchor) = target.strip_prefix('#') {
            return self.md_goto(pane, anchor);
        }
        let is_uri = target.contains("://") || target.starts_with("mailto:");
        let md = self
            .md_of_pane(pane)
            .or_else(|| match self.panes.get(&pane).map(|e| &e.kind) {
                Some(Kind::Markdown { doc, .. }) => Some(*doc),
                _ => None,
            })
            .ok_or("not a Markdown viewer")?;
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
        let line = self.md_top_line(pane).unwrap_or(0);
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

    /// The source line at the top of a text-mode viewer `pane`.
    fn md_source_line(&self, pane: PaneId) -> Option<usize> {
        let Kind::Text { buf, view } = &self.panes.get(&pane)?.kind else { return None };
        let r = self.md_render_of(*buf)?;
        let t = self.texts.get(buf)?;
        r.drawn.sources.get(t.buf.text().line_of(view.view.top)).copied()
    }

    /// Back to the file pane `pane` showed before following a link.
    fn md_back(&mut self, pane: PaneId) -> Result<bool, String> {
        let Some((path, line)) = self.md_history.get_mut(&pane).and_then(Vec::pop) else { return Ok(false) };
        let before = self.md_of_pane(pane).or_else(|| match self.panes.get(&pane).map(|e| &e.kind) {
            Some(Kind::Markdown { doc, .. }) => Some(*doc),
            _ => None,
        });
        self.md_open(&path, Spot::Replace(pane))?;
        if let Some((p, page)) = self.md_graphics(pane) {
            p.show_line(page, line);
        }
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
        if matches!(self.panes.get(&pane).map(|e| &e.kind), Some(Kind::Markdown { .. })) {
            let (p, page) = self.md_graphics(pane).ok_or("not a Markdown viewer")?;
            return if p.show_anchor(page, anchor) { Ok(()) } else { Err(format!("no heading #{anchor}")) };
        }
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
