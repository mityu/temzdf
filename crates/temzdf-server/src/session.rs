//! A session: panes (PDF views and terminals), their layout and focus, the
//! prefix key, and the composed frame every attached client shows. No sockets
//! or terminals here: the standalone viewer, `--render-frame` and the session
//! server all drive a `Session` (see doc/session-server.md).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use temzdf_pdfcore::doc_cache::Need;
use temzdf_pdfcore::workers::DocEvent;
use temzdf_pdfview::document::Document;
use temzdf_pdfview::host::{DocumentHost, Forward, HostOptions, Opening};
use temzdf_pdfview::pane::{self, PdfPane};
use temzdf_pdfview::viewer::{StandIn, ViewState};
use temzdf_scene::{CursorShape, Effect, Frame, Hints, ImageKey, Images, Layer, Pointer, Rect, TextGrid};
use temzdf_term::{Spawn, TermBuffer, TermEvent, TermView};
use termpix_core::geom::Geometry;
use termpix_core::input::{CoordUnit, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use termpix_core::scroll::ScrollConfig;

use crate::action::{Action, PaneSpec};
use crate::clip;
use crate::forward::{self, ForwardRequest};
use crate::keymap::{History, Matcher, Modes, Step, Stroke, Target, Unmapped, notation};
use crate::layout::{BorderId, Layout, PaneId, Split};
use temzdf_protocol::script::{FromHost, ScriptValue};

mod commands;
mod floats;
mod notices;
mod script;

pub use commands::command_specs;

pub use script::{EVENTS as SCRIPT_EVENTS, ScriptSettings};

pub type DocId = u32;
/// A buffer is what a pane shows, as in Vim: a document (its id is the
/// [`DocId`]) or a terminal. Buffers outlive their panes: closing a pane
/// hides its buffer. Ids are unique in the session and never reused (panes
/// and buffers take them from one counter).
pub type BufferId = u32;

/// Events for the session from its documents' and terminals' threads and
/// from other processes (forward search).
pub enum SessionEvent {
    Doc(DocId, DocEvent),
    Forward(DocId, ForwardRequest),
    Term(BufferId, TermEvent),
    /// A message from the script host (of this generation: hosts are
    /// replaced on reload, and an old one's last words must not count).
    Script(u32, FromHost),
    /// The script host process of this generation ended.
    ScriptExited(u32),
}

/// Where a session's events go (returns false once nobody listens).
pub type EventSink = Arc<dyn Fn(SessionEvent) -> bool + Send + Sync>;

/// How new panes and documents are set up (from the configuration).
#[derive(Clone)]
pub struct Settings {
    /// Listen for `temzdf --forward` on each document's socket (not for
    /// headless rendering, which must not take a running viewer's socket).
    pub serve_forward: bool,
    /// The configuration script and how to run the script host; `None`
    /// runs no scripts.
    pub script: Option<ScriptSettings>,
    /// Options set on the command line (`--editor`): they win over the
    /// configuration's.
    pub overrides: Vec<(String, ScriptValue)>,
}

/// What the caller should do after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    Redraw,
    /// The client asked to detach.
    Detach,
    /// The last pane closed: the session is over.
    Quit,
}

impl Outcome {
    fn or(self, redraw: bool) -> Self {
        if self == Outcome::None && redraw { Outcome::Redraw } else { self }
    }

    /// The stronger of two outcomes (quit, detach, redraw, nothing).
    pub fn merge(self, other: Self) -> Self {
        let rank = |o: Self| match o {
            Outcome::None => 0,
            Outcome::Redraw => 1,
            Outcome::Detach => 2,
            Outcome::Quit => 3,
        };
        if rank(other) > rank(self) { other } else { self }
    }
}

/// Options scripts set with `o.<name> = ...` (global for now).
#[derive(Clone, Debug)]
pub struct Options {
    /// How long a key that starts a longer mapping waits (Vim's `timeoutlen`).
    pub timeoutlen: Duration,
    /// Where `split()` without a side puts the new pane: right (else left).
    pub splitright: bool,
    /// Below (else above), for splits towards the bottom.
    pub splitbelow: bool,
    /// How many modes the history keeps.
    pub modehistory: usize,
    /// Pixels per wheel step, and the easing's time constant.
    pub scrollstep: f64,
    pub scrollsmoothness: Duration,
    /// The command opening external links (the URI appended); the system
    /// handler when `None`.
    pub linkopener: Option<Vec<String>>,
    /// Preview internal links after the pointer rests on one this long.
    pub linkpreview: bool,
    pub linkpreviewdelay: Duration,
    /// Wait this long after the PDF changed before reloading.
    pub reloaddebounce: Duration,
    /// Keep the line being read across reloads (else the position).
    pub reloadanchor_text: bool,
    /// The inverse-search command (`%{input}`, `%{line}`, `%{column}`).
    pub synctexeditor: Option<String>,
    /// A shown terminal whose program ended keeps its panes (tmux's
    /// `remain-on-exit`).
    pub remainonexit: bool,
}

impl Default for Options {
    fn default() -> Self {
        let scroll = ScrollConfig::default();
        Self {
            timeoutlen: Duration::from_millis(1000),
            splitright: true,
            splitbelow: true,
            modehistory: 100,
            scrollstep: scroll.step,
            scrollsmoothness: scroll.tau,
            linkopener: None,
            linkpreview: true,
            linkpreviewdelay: temzdf_pdfview::pane::DEFAULT_PREVIEW_DELAY,
            reloaddebounce: Duration::from_millis(200),
            reloadanchor_text: false,
            synctexeditor: None,
            remainonexit: false,
        }
    }
}

impl Options {
    fn scroll(&self) -> ScrollConfig {
        ScrollConfig {
            step: self.scrollstep.max(1.0),
            tau: self.scrollsmoothness.max(Duration::from_millis(1)),
            ..Default::default()
        }
    }

    fn preview_delay(&self) -> Option<Duration> {
        self.linkpreview.then_some(self.linkpreviewdelay)
    }

    fn host(&self) -> HostOptions {
        HostOptions { debounce: self.reloaddebounce, editor: self.synctexeditor.clone() }
    }
}
/// Session layers: the background below every pane, borders above them.
const Z_SESSION_BACKGROUND: i32 = -1_073_741_950;
const Z_BORDER: i32 = -1_073_741_700;
const Z_BORDER_FOCUS: i32 = -1_073_741_699;
const BACKGROUND: [u8; 4] = [0x3a, 0x3a, 0x3a, 0xff];
const BORDER: [u8; 4] = [0x5a, 0x5a, 0x5a, 0xff];
/// Border line thickness in pixels (centred in the one-cell gap).
const BORDER_PX: u32 = 2;
/// The most mappings one typed key may run (Vim's `maxmapdepth`).
const MAX_MAP_DEPTH: usize = 1000;
/// Layer key owner for the session's own layers.
const OWNER_SESSION: u32 = u32::MAX;

/// Where a pane goes.
#[derive(Clone, Copy, Debug)]
enum Spot {
    /// The first pane of the session.
    First,
    /// Next to `target` (the whole layout for `None`), before (left/above)
    /// or after it.
    Split { target: Option<PaneId>, split: Split, before: bool },
    /// Instead of what this pane shows (Vim's `:edit`).
    Replace(PaneId),
    /// A floating pane, on top of the others.
    Float(floats::FloatSpec),
}

struct DocEntry {
    path: PathBuf,
    name: String,
    host: Option<DocumentHost>,
    opening: Option<Opening>,
    /// Panes to create once it opened.
    waiting: Vec<Spot>,
    _forward: Option<forward::Server>,
    /// How the last pane showed it, for the next one.
    view: Option<ViewState>,
}

struct TermEntry {
    term: Box<TermBuffer>,
    /// The program ended; the screen stays until the buffer is deleted.
    exited: bool,
}

enum Kind {
    Pdf { pane: Box<PdfPane>, doc: DocId },
    Term { buf: BufferId, view: TermView },
}

struct PaneEntry {
    kind: Kind,
    /// In cells.
    rect: Rect,
}

impl PaneEntry {
    fn pdf(&mut self) -> Option<&mut PdfPane> {
        match &mut self.kind {
            Kind::Pdf { pane, .. } => Some(pane),
            Kind::Term { .. } => None,
        }
    }

    fn doc(&self) -> Option<DocId> {
        match &self.kind {
            Kind::Pdf { doc, .. } => Some(*doc),
            Kind::Term { .. } => None,
        }
    }

    fn term(&self) -> Option<BufferId> {
        match &self.kind {
            Kind::Term { buf, .. } => Some(*buf),
            Kind::Pdf { .. } => None,
        }
    }

    fn buffer(&self) -> BufferId {
        match &self.kind {
            Kind::Pdf { doc, .. } => *doc,
            Kind::Term { buf, .. } => *buf,
        }
    }
}

/// Placement keys of the panes' layers, made unique across panes and kept
/// stable from frame to frame (so the compositor moves rather than re-creates).
/// A layer cut by floats gets a key per piece; the first piece keeps the key
/// the whole layer had.
#[derive(Default)]
struct Keys {
    map: HashMap<(u32, u32, u32), u32>,
    next: u32,
    /// The keys placed this frame; the others are forgotten after it.
    used: HashSet<(u32, u32, u32)>,
}

impl Keys {
    fn get(&mut self, k: (u32, u32, u32)) -> u32 {
        self.used.insert(k);
        let next = &mut self.next;
        *self.map.entry(k).or_insert_with(|| {
            *next = next.wrapping_add(1).max(1);
            *next
        })
    }

    /// Add `layer` (`key` of `owner`) to `out`, cut around `covers` (pixels).
    fn place(&mut self, out: &mut Vec<Layer>, owner: u32, key: u32, layer: Layer, covers: &[Rect]) {
        for (i, piece) in clip::layer_pieces(&layer, covers).into_iter().enumerate() {
            out.push(Layer { key: self.get((owner, key, i as u32)), ..piece });
        }
    }

    /// Forget the keys not placed since the last call.
    fn end_frame(&mut self) {
        let used = std::mem::take(&mut self.used);
        self.map.retain(|k, _| used.contains(k));
    }
}

pub struct Session {
    pub name: String,
    images: Images,
    geometry: Geometry,
    hints: Hints,
    settings: Settings,
    events: EventSink,
    docs: HashMap<DocId, DocEntry>,
    terms: HashMap<BufferId, TermEntry>,
    /// What a new shell pane runs; `None`: `$SHELL`.
    shell: Option<Vec<String>>,
    panes: HashMap<PaneId, PaneEntry>,
    layout: Option<Layout>,
    /// Floating panes, bottom to top, and whether they are hidden.
    floats: Vec<floats::Float>,
    floats_hidden: bool,
    focus: PaneId,
    /// The tiled pane focused last (where the focus goes when floats hide).
    tiled_focus: PaneId,
    next_id: u32,
    modes: Modes<Target>,
    matcher: Matcher,
    history: History,
    options: Options,
    notices: Vec<notices::Notice>,
    /// The fixed escape keymap (script host down) saw its prefix.
    escape_prefix: bool,
    /// Mappings run since the last typed key (Vim's `maxmapdepth`).
    map_depth: usize,
    keys: Keys,
    effects: Vec<Effect>,
    /// The pane under the pointer, and the one holding a pressed button.
    hovered: Option<PaneId>,
    captured: Option<PaneId>,
    /// The pressed button only switched the focus.
    focus_click: bool,
    /// The border being dragged, and the kind of border under the pointer.
    dragging: Option<BorderId>,
    over_border: Option<Split>,
    /// The float frame being dragged, and the part of a frame under the pointer.
    float_drag: Option<floats::FloatDrag>,
    over_frame: Option<floats::Grip>,
    /// Solid images (background, borders) by size and colour.
    solids: HashMap<(u32, u32, [u8; 4]), ImageKey>,
    complete: bool,
    /// Why the first pane could not be opened.
    error: Option<String>,
    script: script::Bridge,
    /// Events for script hooks, collected while an operation runs.
    script_events: Vec<(String, Vec<ScriptValue>)>,
}

impl Session {
    pub fn new(name: String, geometry: Geometry, hints: Hints, settings: Settings, events: EventSink) -> Self {
        let modes = builtin_modes();
        Self {
            name,
            images: Images::new(),
            geometry,
            hints,
            settings,
            events,
            docs: HashMap::new(),
            terms: HashMap::new(),
            shell: None,
            panes: HashMap::new(),
            layout: None,
            floats: Vec::new(),
            floats_hidden: false,
            focus: 0,
            tiled_focus: 0,
            next_id: 1,
            modes,
            matcher: Matcher::default(),
            history: History::new("normal", Options::default().modehistory),
            options: Options::default(),
            notices: Vec::new(),
            escape_prefix: false,
            map_depth: 0,
            keys: Keys::default(),
            effects: Vec::new(),
            hovered: None,
            captured: None,
            focus_click: false,
            dragging: None,
            float_drag: None,
            over_frame: None,
            over_border: None,
            solids: HashMap::new(),
            complete: false,
            error: None,
            script: script::Bridge::default(),
            script_events: Vec::new(),
        }
    }

    pub fn images(&mut self) -> &mut Images {
        &mut self.images
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Why the session's first pane failed to open, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// No pane yet (the first document is still opening).
    pub fn is_starting(&self) -> bool {
        self.layout.is_none() && self.error.is_none()
    }

    /// Where a new pane goes: the first one, or beside the focused pane.
    fn next_spot(&self) -> Spot {
        // The first tiled pane comes first, even after floats opened while
        // the configuration loaded.
        if self.layout.is_none() {
            Spot::First
        } else if self.is_float(self.focus) {
            // Beside a focused float: another float (zellij).
            Spot::Float(self.cascade())
        } else if self.layout.is_some() {
            Spot::Split { target: Some(self.focus), split: Split::Horizontal, before: false }
        } else {
            Spot::First
        }
    }

    /// Open a new pane: the first one, or beside the focused pane.
    pub fn new_pane(&mut self, spec: &PaneSpec, cwd: Option<&Path>) -> Result<()> {
        let cwd = cwd.map(Path::to_owned);
        match spec {
            PaneSpec::Pdf(path) => {
                let path = cwd.map_or_else(|| path.clone(), |d| d.join(path));
                self.open(&path)
            }
            PaneSpec::Shell => {
                self.open_terminal(&Spawn { argv: None, cwd, env: Vec::new() }, self.next_spot()).map(drop)
            }
            PaneSpec::Command(argv) => self
                .open_terminal(&Spawn { argv: Some(argv.clone()), cwd, env: Vec::new() }, self.next_spot())
                .map(drop),
        }
    }

    /// Open `path` in a new pane: the first one, or beside the focused pane.
    /// A file already open is shared, not opened again.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        self.open_at(path, self.next_spot())
    }

    fn open_at(&mut self, path: &Path, spot: Spot) -> Result<()> {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        if let Some((&id, entry)) = self.docs.iter_mut().find(|(_, d)| d.path == canonical) {
            if entry.host.is_some() {
                self.add_pdf_pane(id, spot);
            } else {
                entry.waiting.push(spot);
            }
            return Ok(());
        }
        let id = self.alloc_id();
        let events = self.events.clone();
        let sink: temzdf_pdfcore::workers::Sink = Arc::new(move |e| events(SessionEvent::Doc(id, e)));
        let opening = Opening::start(path, sink)?;
        let forward_server = if self.settings.serve_forward {
            let events = self.events.clone();
            // A second viewer of the same file just goes without.
            forward::serve(path, move |req| events(SessionEvent::Forward(id, req))).ok()
        } else {
            None
        };
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        self.docs.insert(
            id,
            DocEntry {
                path: canonical,
                name,
                host: None,
                opening: Some(opening),
                waiting: vec![spot],
                _forward: forward_server,
                view: None,
            },
        );
        Ok(())
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn area(&self) -> Rect {
        Rect::new(0, 0, self.geometry.cols as u32, self.geometry.rows as u32)
    }

    /// Put pane `id` into the layout at `spot`. Returns its rectangle.
    fn place(&mut self, id: PaneId, spot: Spot) -> Result<Rect> {
        self.dragging = None;
        self.float_drag = None;
        if let Spot::Float(spec) = spot {
            return Ok(self.place_float(id, spec));
        }
        let area = self.area();
        let (target, split, before) = match spot {
            Spot::Split { target: Some(t), split, before } if self.panes.contains_key(&t) => (Some(t), split, before),
            Spot::Split { target: None, split, before } => (None, split, before),
            // The target went away meanwhile: split the focused pane instead.
            _ => (Some(self.focus), Split::Horizontal, false),
        };
        match self.layout.as_mut() {
            None => self.layout = Some(Layout::new(id)),
            Some(layout) => {
                if !layout.split_at(target, id, split, before, area) {
                    bail!("no room for another pane");
                }
            }
        }
        let layout = self.layout.as_ref().expect("placed");
        Ok(layout.rects(area).into_iter().find(|(p, _)| *p == id).map_or(area, |(_, r)| r))
    }

    /// Take pane `id` out of the layout again (its creation failed).
    fn unplace(&mut self, id: PaneId) {
        if self.is_float(id) {
            self.floats.retain(|f| f.pane != id);
            return;
        }
        if let Some(layout) = self.layout.as_mut()
            && !layout.remove(id)
        {
            self.layout = None;
        }
    }

    /// Create a pane on an open document (or show it in a pane).
    fn add_pdf_pane(&mut self, doc: DocId, spot: Spot) -> Option<PaneId> {
        self.docs.get(&doc)?.host.as_ref()?;
        if let Spot::Replace(p) = spot {
            if let Err(e) = self.show_in(p, doc) {
                self.message(&e);
            }
            return Some(p);
        }
        let id = self.alloc_id();
        if let Err(e) = self.place(id, spot) {
            self.message(&e.to_string());
            return None;
        }
        let restore = !self.shown(doc);
        let kind = self.pdf_kind(id, doc)?;
        self.panes.insert(id, PaneEntry { kind, rect: self.area() });
        self.event("PaneOpened", vec![ScriptValue::Num(id as f64), ScriptValue::Str("pdf".into())]);
        self.set_focus(id);
        self.relayout();
        if restore {
            self.restore_view(id);
        }
        Some(id)
    }

    /// A page view of `doc` for pane `id`.
    fn pdf_kind(&mut self, id: PaneId, doc: DocId) -> Option<Kind> {
        let entry = self.docs.get_mut(&doc)?;
        let host = entry.host.as_mut()?;
        let mut pane = PdfPane::with_document(entry.name.clone(), host.info.clone(), host.doc.clone(), self.geometry);
        pane.preview_delay = self.options.preview_delay();
        pane.anchor_by_text = self.options.reloadanchor_text;
        pane.viewer.scroll_x.config = self.options.scroll();
        pane.viewer.scroll_y.config = self.options.scroll();
        pane.viewer.stand_in_mode = StandIn::for_hints(self.hints);
        pane.set_search_base((id as u64) << 32);
        if let Some(e) = host.watcher_error.take() {
            pane.message = Some(e);
        }
        Some(Kind::Pdf { pane: Box::new(pane), doc })
    }

    /// Show the document the way its last pane did (it was hidden).
    fn restore_view(&mut self, id: PaneId) {
        let Some(e) = self.panes.get_mut(&id) else { return };
        if let Kind::Pdf { pane, doc } = &mut e.kind
            && let Some(state) = self.docs.get(doc).and_then(|d| d.view)
        {
            pane.viewer.restore(&state);
        }
    }

    /// Whether a pane shows buffer `buf`.
    fn shown(&self, buf: BufferId) -> bool {
        self.panes.values().any(|e| e.buffer() == buf)
    }

    /// The panes showing `buf`.
    fn panes_showing(&self, buf: BufferId) -> Vec<PaneId> {
        let mut v: Vec<PaneId> = self.panes.iter().filter(|(_, e)| e.buffer() == buf).map(|(id, _)| *id).collect();
        v.sort();
        v
    }

    /// Show buffer `buf` in pane `pane` instead of what it shows (Vim's
    /// `:buffer`). A document still opening shows once it opened.
    fn show_in(&mut self, pane: PaneId, buf: BufferId) -> Result<Outcome, String> {
        let Some(e) = self.panes.get(&pane) else { return Err(format!("no pane {pane}")) };
        if e.buffer() == buf {
            return Ok(Outcome::None);
        }
        let g = Geometry { cols: e.rect.w as u16, rows: e.rect.h as u16, cell: self.geometry.cell };
        let kind = if let Some(t) = self.terms.get_mut(&buf) {
            t.term.scroll_to_bottom();
            Kind::Term { buf, view: TermView::new(g) }
        } else if let Some(d) = self.docs.get_mut(&buf) {
            if d.host.is_none() {
                d.waiting.push(Spot::Replace(pane));
                return Ok(Outcome::None);
            }
            self.pdf_kind(pane, buf).ok_or("the document is not open")?
        } else {
            return Err(format!("no buffer {buf}"));
        };
        let restore = matches!(kind, Kind::Pdf { .. }) && !self.shown(buf);
        let focused = pane == self.focus;
        if focused && let Some(t) = self.panes.get(&pane).and_then(PaneEntry::term).and_then(|b| self.terms.get_mut(&b))
        {
            t.term.focus(false);
        }
        let old = std::mem::replace(&mut self.panes.get_mut(&pane).expect("checked").kind, kind);
        self.hide(old);
        self.relayout();
        if restore {
            self.restore_view(pane);
        }
        if focused {
            if let Some(t) = self.terms.get_mut(&buf) {
                t.term.focus(true);
            }
            let old = self.history.current().to_owned();
            let mode = self.pane_mode(pane);
            self.history.reset(mode);
            self.mode_changed(&old);
        }
        Ok(Outcome::Redraw)
    }

    /// What a pane showed is no longer shown there. A buffer no pane shows
    /// stays, hidden: a document keeps its position (and drops its tiles), a
    /// terminal keeps running; an ended terminal goes.
    fn hide(&mut self, kind: Kind) {
        match kind {
            Kind::Pdf { mut pane, doc } => {
                let state = pane.viewer.view_state();
                pane.release(&mut self.images);
                if !self.shown(doc)
                    && let Some(d) = self.docs.get_mut(&doc)
                {
                    d.view = Some(state);
                    if let Some(h) = &d.host {
                        h.doc.borrow_mut().forget(&mut self.images);
                    }
                }
            }
            Kind::Term { buf, .. } => {
                if !self.shown(buf)
                    && let Some(t) = self.terms.get_mut(&buf)
                {
                    t.term.release(&mut self.images);
                    if t.exited {
                        self.remove_buffer(buf);
                    }
                }
            }
        }
    }

    /// A terminal's program ended. A shown one closes its panes (unless
    /// `remainonexit`); a hidden one stays to be looked at, as ended.
    fn term_exited(&mut self, buf: BufferId) -> Outcome {
        let Some(t) = self.terms.get_mut(&buf) else { return Outcome::None };
        t.term.set_exited();
        t.exited = true;
        let panes = self.panes_showing(buf);
        if panes.is_empty() || self.options.remainonexit {
            return Outcome::Redraw;
        }
        let mut out = Outcome::None;
        for p in panes {
            out = out.merge(self.request_close(p));
        }
        out
    }

    /// Delete a buffer: end its program or close its document, and close the
    /// panes showing it (the last pane shows another buffer instead).
    fn delete_buffer(&mut self, buf: BufferId) -> Result<Outcome, String> {
        if !self.terms.contains_key(&buf) && !self.docs.contains_key(&buf) {
            return Err(format!("no buffer {buf}"));
        }
        for p in self.panes_showing(buf) {
            if self.close(p) == Outcome::Quit {
                return Ok(Outcome::Quit);
            }
        }
        self.remove_buffer(buf);
        Ok(Outcome::Redraw)
    }

    /// Forget a buffer no pane shows.
    fn remove_buffer(&mut self, buf: BufferId) {
        if let Some(d) = self.docs.remove(&buf) {
            if let Some(host) = d.host {
                host.doc.borrow_mut().release(&mut self.images);
            }
        } else if let Some(mut t) = self.terms.remove(&buf) {
            t.term.release(&mut self.images);
        } else {
            return;
        }
        self.event("BufferDeleted", vec![ScriptValue::Num(buf as f64)]);
    }

    /// The buffers, for `buffers()`.
    fn buffer_list(&self) -> ScriptValue {
        let mut list: Vec<(BufferId, &str, String, bool)> = self
            .docs
            .iter()
            .filter(|(_, d)| d.host.is_some())
            .map(|(id, d)| (*id, "pdf", d.name.clone(), false))
            .chain(
                self.terms
                    .iter()
                    .map(|(id, t)| (*id, "terminal", t.term.title().unwrap_or_else(|| "shell".into()), t.exited)),
            )
            .collect();
        list.sort_by_key(|b| b.0);
        ScriptValue::List(
            list.into_iter()
                .map(|(id, kind, name, exited)| {
                    let panes = self.panes_showing(id).into_iter().map(|p| ScriptValue::Num(p as f64)).collect();
                    ScriptValue::map([
                        ("id", ScriptValue::Num(id as f64)),
                        ("kind", ScriptValue::Str(kind.into())),
                        ("name", ScriptValue::Str(name)),
                        ("panes", ScriptValue::List(panes)),
                        ("exited", ScriptValue::Bool(exited)),
                    ])
                })
                .collect(),
        )
    }

    /// The number of buffers (for `temzdf ls`).
    pub fn buffer_count(&self) -> usize {
        self.docs.values().filter(|d| d.host.is_some()).count() + self.terms.len()
    }

    /// The focused pane's terminal.
    fn focused_term(&mut self) -> Option<&mut TermBuffer> {
        let buf = self.panes.get(&self.focus)?.term()?;
        self.terms.get_mut(&buf).map(|t| &mut *t.term)
    }

    /// Start a program in a new terminal pane.
    fn open_terminal(&mut self, spawn: &Spawn, spot: Spot) -> Result<PaneId> {
        let id = self.alloc_id();
        let rect = self.place(id, spot)?;
        let buf = self.alloc_id();
        let events = self.events.clone();
        let sink: temzdf_term::Sink = Arc::new(move |e| events(SessionEvent::Term(buf, e)));
        let mut spawn = spawn.clone();
        spawn.argv = spawn.argv.or_else(|| self.shell.clone());
        spawn.env.push(("TEMZDF_SESSION_NAME".into(), self.name.clone()));
        let geometry = Geometry { cols: rect.w as u16, rows: rect.h as u16, cell: self.geometry.cell };
        let term = match TermBuffer::spawn(&spawn, geometry, sink) {
            Ok(t) => t,
            Err(e) => {
                self.unplace(id);
                bail!("cannot start {}: {e}", spawn.argv.as_ref().map_or("the shell".into(), |a| a.join(" ")));
            }
        };
        self.terms.insert(buf, TermEntry { term: Box::new(term), exited: false });
        self.event("BufferCreated", vec![ScriptValue::Num(buf as f64), ScriptValue::Str("terminal".into())]);
        self.terminal_pane(id, buf, rect);
        Ok(id)
    }

    /// Pane `id`, placed at `rect`, shows terminal `buf`.
    fn terminal_pane(&mut self, id: PaneId, buf: BufferId, rect: Rect) {
        let geometry = Geometry { cols: rect.w as u16, rows: rect.h as u16, cell: self.geometry.cell };
        self.panes.insert(id, PaneEntry { kind: Kind::Term { buf, view: TermView::new(geometry) }, rect });
        self.event("PaneOpened", vec![ScriptValue::Num(id as f64), ScriptValue::Str("terminal".into())]);
        self.set_focus(id);
        self.relayout();
    }

    /// Move the focus, telling terminals that asked for focus reports.
    fn set_focus(&mut self, id: PaneId) {
        // A focused float comes to the top.
        self.raise(id);
        if !self.is_float(id) {
            self.tiled_focus = id;
        }
        if id == self.focus {
            return;
        }
        for (p, gained) in [(self.focus, false), (id, true)] {
            if let Some(t) = self.panes.get(&p).and_then(PaneEntry::term).and_then(|b| self.terms.get_mut(&b)) {
                t.term.focus(gained);
            }
        }
        self.focus = id;
        self.event("PaneFocused", vec![ScriptValue::Num(id as f64)]);
        // The history starts again from the pane's own mode.
        let old = self.history.current().to_owned();
        let mode = self.pane_mode(id);
        self.history.reset(mode);
        self.mode_changed(&old);
    }

    /// Show a message in the focused pane's status bar (or another PDF pane's:
    /// terminal panes have none).
    /// An error message, in a floating window until the next key.
    fn message(&mut self, m: &str) {
        self.notify(notices::Kind::Error, m);
    }

    /// Give every pane its rectangle (after the layout or the screen changed).
    fn relayout(&mut self) {
        let rects = self.layout.as_ref().map(|l| l.rects(self.area())).unwrap_or_default();
        for (id, r) in rects {
            self.set_rect(id, r);
        }
        self.relayout_floats();
        // Programs get their new size when the drag ends.
        if self.dragging.is_none() && self.float_drag.is_none() {
            self.fit_terms();
        }
    }

    /// Give pane `id` its rectangle (in cells).
    fn set_rect(&mut self, id: PaneId, r: Rect) {
        let cell = self.geometry.cell;
        if let Some(e) = self.panes.get_mut(&id) {
            e.rect = r;
            let g = Geometry { cols: r.w as u16, rows: r.h as u16, cell };
            match &mut e.kind {
                Kind::Pdf { pane, .. } if g != pane.geometry() => pane.resize(g),
                Kind::Pdf { .. } => {}
                Kind::Term { view, .. } => view.geometry = g,
            }
        }
    }

    /// A terminal is as large as the largest pane on screen showing it (as
    /// in Neovim); a hidden one (or one in hidden floats only) keeps its size.
    fn fit_terms(&mut self) {
        let mut sizes: HashMap<BufferId, (u16, u16)> = HashMap::new();
        for (id, e) in &self.panes {
            if !self.on_screen(*id) {
                continue;
            }
            if let Kind::Term { buf, view } = &e.kind {
                let s = sizes.entry(*buf).or_default();
                *s = (s.0.max(view.geometry.cols), s.1.max(view.geometry.rows));
            }
        }
        for (buf, (cols, rows)) in sizes {
            if let Some(t) = self.terms.get_mut(&buf) {
                t.term.resize(Geometry { cols, rows, cell: self.geometry.cell });
            }
        }
    }

    pub fn resize(&mut self, geometry: Geometry) {
        self.geometry = geometry;
        self.relayout();
    }

    /// The frontend changed (another client became the latest).
    pub fn set_hints(&mut self, hints: Hints) {
        self.hints = hints;
        for p in self.panes.values_mut().filter_map(PaneEntry::pdf) {
            p.viewer.stand_in_mode = StandIn::for_hints(hints);
        }
    }

    /// The panes showing `doc`, the focused one first (it receives SyncTeX results).
    fn panes_of(panes: &mut HashMap<PaneId, PaneEntry>, doc: DocId, focus: PaneId) -> Vec<&mut PdfPane> {
        let mut v: Vec<(PaneId, &mut PdfPane)> = panes
            .iter_mut()
            .filter_map(|(id, e)| match &mut e.kind {
                Kind::Pdf { pane, doc: d } if *d == doc => Some((*id, &mut **pane)),
                _ => None,
            })
            .collect();
        v.sort_by_key(|(id, _)| (*id != focus, *id));
        v.into_iter().map(|(_, p)| p).collect()
    }

    pub fn handle(&mut self, ev: SessionEvent, now: Instant) -> Outcome {
        let out = match ev {
            SessionEvent::Script(generation, msg) => {
                return if generation == self.script.generation { self.handle_script(msg) } else { Outcome::None };
            }
            SessionEvent::ScriptExited(generation) => {
                return if generation == self.script.generation { self.script_exited() } else { Outcome::None };
            }
            ev => self.handle_inner(ev, now),
        };
        out.merge(self.flush_events())
    }

    fn handle_inner(&mut self, ev: SessionEvent, now: Instant) -> Outcome {
        match ev {
            SessionEvent::Script(..) | SessionEvent::ScriptExited(_) => Outcome::None,
            SessionEvent::Doc(id, DocEvent::Opened(result)) => {
                let Some(entry) = self.docs.get_mut(&id) else { return Outcome::None };
                let Some(opening) = entry.opening.take() else { return Outcome::None };
                match result {
                    Ok(info) if !info.bounds.is_empty() => {
                        let doc = Document::shared(info.bounds.clone());
                        entry.host = Some(opening.finish(info, doc, self.options.host()));
                        self.event("BufferCreated", vec![ScriptValue::Num(id as f64), ScriptValue::Str("pdf".into())]);
                        let Some(entry) = self.docs.get_mut(&id) else { return Outcome::Redraw };
                        for spot in std::mem::take(&mut entry.waiting) {
                            self.add_pdf_pane(id, spot);
                        }
                    }
                    other => {
                        let e = match other {
                            Err(e) => e,
                            Ok(_) => "the document has no pages".into(),
                        };
                        let name = entry.name.clone();
                        self.docs.remove(&id);
                        if self.layout.is_none() && self.floats.is_empty() {
                            self.error = Some(e);
                            return Outcome::Quit;
                        }
                        self.message(&format!("{name}: {e}"));
                    }
                }
                Outcome::Redraw
            }
            SessionEvent::Doc(id, ev) => {
                if matches!(ev, DocEvent::Reloaded(_))
                    && let Some(d) = self.docs.get(&id)
                {
                    let path = d.path.display().to_string();
                    self.event("DocumentReloaded", vec![ScriptValue::Str(path)]);
                }
                let Some(host) = self.docs.get_mut(&id).and_then(|d| d.host.as_mut()) else { return Outcome::None };
                let mut panes = Self::panes_of(&mut self.panes, id, self.focus);
                let (changed, effect) = host.handle(ev, &mut panes, &mut self.images, now);
                self.effects.extend(effect);
                Outcome::None.or(changed)
            }
            SessionEvent::Forward(id, req) => {
                let Some(host) = self.docs.get_mut(&id).and_then(|d| d.host.as_mut()) else { return Outcome::None };
                let panes = Self::panes_of(&mut self.panes, id, self.focus);
                if let Some(pane) = panes.first() {
                    host.forward(Forward { file: req.file, line: req.line, column: req.column }, pane);
                }
                Outcome::None
            }
            SessionEvent::Term(buf, TermEvent::Output(bytes)) => match self.terms.get_mut(&buf) {
                Some(t) => {
                    t.term.feed(&bytes);
                    Outcome::None.or(self.shown(buf))
                }
                None => Outcome::None,
            },
            SessionEvent::Term(buf, TermEvent::Exited) => self.term_exited(buf),
        }
    }

    pub fn key(&mut self, k: &KeyEvent, now: Instant) -> Outcome {
        if k.kind == KeyEventKind::Release || matches!(k.code, KeyCode::Modifier(_)) {
            return Outcome::None;
        }
        // A key dismisses error messages (and is handled as usual).
        let dismissed = Outcome::None.or(self.dismiss_errors());
        self.map_depth = 0;
        if self.hold_key(k) {
            return dismissed;
        }
        let out = self.key_inner(k, now);
        dismissed.merge(out).merge(self.flush_events())
    }

    fn key_inner(&mut self, k: &KeyEvent, now: Instant) -> Outcome {
        // The script host should run but does not: the fixed escape keys.
        if self.settings.script.is_some() && !self.script.running() {
            return self.escape_key(k, now);
        }
        // <C-c> cancels pending keys and the count; otherwise it is an
        // ordinary (unmappable) key.
        let ctrl_c = script::is_interrupt(k);
        if ctrl_c && (!self.matcher.pending().is_empty() || self.matcher.count().is_some()) {
            self.matcher.cancel();
            return Outcome::None;
        }
        let mut out = Outcome::None;
        // A PDF pane's own keys (its search prompt, outline, link preview)
        // come before mappings, unless a key sequence is under way.
        let focus = self.focus;
        if self.matcher.pending().is_empty()
            && !self.matcher.has_input()
            && let Some(Kind::Pdf { pane, .. }) = self.panes.get_mut(&focus).map(|e| &mut e.kind)
        {
            let mut redraw = false;
            if let Some(action) = pane.key_first(k, now, &mut redraw) {
                return match action {
                    pane::Action::Quit => self.request_close(focus),
                    pane::Action::Redraw => Outcome::Redraw,
                    pane::Action::None => Outcome::None,
                };
            }
            out = out.or(redraw);
        }
        self.matcher.push(k);
        out.merge(self.drive(now))
    }

    /// Keys while the script host is down (no configured mappings work):
    /// `<C-b>d` detaches, `<C-b>q` ends the session, `<C-b>r` restarts the
    /// host; everything else goes to the focused pane as typed.
    fn escape_key(&mut self, k: &KeyEvent, now: Instant) -> Outcome {
        let stroke = Stroke::of(k);
        let is = |s: &str| notation::parse(s).is_ok_and(|v| v[0] == stroke);
        if std::mem::take(&mut self.escape_prefix) {
            return if is("d") {
                Outcome::Detach
            } else if is("q") {
                Outcome::Quit
            } else if is("r") {
                match self.reload_scripts() {
                    Ok(o) => o,
                    Err(e) => {
                        self.message(&format!("{e:#}"));
                        Outcome::Redraw
                    }
                }
            } else {
                Outcome::None
            };
        }
        if is("<C-b>") {
            self.escape_prefix = true;
            return Outcome::None;
        }
        if let Some(t) = self.focused_term() {
            return Outcome::None.or(t.key(k));
        }
        let focus = self.focus;
        match self.panes.get_mut(&focus).map(|e| &mut e.kind) {
            Some(Kind::Term { .. }) => Outcome::None,
            // The pane's own page-view keys still work.
            Some(Kind::Pdf { pane, .. }) => match pane.key(k, now) {
                pane::Action::Quit => self.close(focus),
                pane::Action::Redraw => Outcome::Redraw,
                pane::Action::None => Outcome::None,
            },
            None => Outcome::None,
        }
    }

    /// Let the matcher decide what queued keys do, until it waits (for more
    /// keys, or while a script function runs).
    fn drive(&mut self, now: Instant) -> Outcome {
        let mut out = Outcome::None;
        while !self.script.busy() {
            let mode = self.history.current().to_owned();
            match self.matcher.step(&self.modes, &mode, self.options.timeoutlen, now) {
                Step::Wait(_) => break,
                Step::Run { target, count, .. } => {
                    // Mappings that feed keys mapping to themselves (`a` to
                    // `a`, or to `aa`) would run forever, as in Vim.
                    self.map_depth += 1;
                    if self.map_depth > MAX_MAP_DEPTH {
                        self.matcher.cancel();
                        self.message("recursive mapping (more than 1000 mappings for one key)");
                        out = out.merge(Outcome::Redraw);
                        break;
                    }
                    out = out.merge(self.run_target(target, count, now));
                }
                Step::Unmapped(stroke, ev) => {
                    if self.modes.unmapped(&mode) == Unmapped::Passthrough {
                        let ev = ev.unwrap_or_else(|| stroke.event());
                        out = out.merge(self.send_to_pane(&ev));
                    }
                }
            }
        }
        out
    }

    /// What a mapping runs: commands right here, or a script function.
    fn run_target(&mut self, target: Target, count: Option<usize>, now: Instant) -> Outcome {
        match target {
            Target::Actions(acts) => {
                let mut out = Outcome::None;
                for a in acts {
                    match self.run_command(&a.name, &a.args, count, now) {
                        Ok((_, o)) => out = out.merge(o),
                        Err(e) => {
                            self.message(&e);
                            out = out.merge(Outcome::Redraw);
                            break;
                        }
                    }
                }
                out
            }
            Target::Func(func) => {
                let count = count.map_or(ScriptValue::Null, |c| ScriptValue::Num(c as f64));
                let ctx = ScriptValue::map([("count", count), ("pane", ScriptValue::Num(self.focus as f64))]);
                self.call(func, vec![ctx], false);
                Outcome::None
            }
        }
    }

    /// An unmapped key in a pass-through mode goes to the focused pane.
    fn send_to_pane(&mut self, k: &KeyEvent) -> Outcome {
        match self.focused_term() {
            Some(t) => Outcome::None.or(t.key(k)),
            None => Outcome::None,
        }
    }

    /// The mode of a pane kind (its default mode).
    fn pane_mode(&self, id: PaneId) -> &'static str {
        match self.panes.get(&id).map(|e| &e.kind) {
            Some(Kind::Pdf { .. }) => "pdf",
            Some(Kind::Term { .. }) => "terminal",
            None => "normal",
        }
    }

    /// Announce a mode change (if `old` differs from the current mode).
    fn mode_changed(&mut self, old: &str) {
        let new = self.history.current().to_owned();
        if new != old {
            self.event("ModeChanged", vec![ScriptValue::Str(old.to_owned()), ScriptValue::Str(new)]);
        }
    }

    /// Options changed: panes and documents follow.
    fn apply_options(&mut self) {
        let (scroll, preview, anchor, host) =
            (self.options.scroll(), self.options.preview_delay(), self.options.reloadanchor_text, self.options.host());
        for p in self.panes.values_mut().filter_map(PaneEntry::pdf) {
            p.viewer.scroll_x.config = scroll;
            p.viewer.scroll_y.config = scroll;
            p.preview_delay = preview;
            p.anchor_by_text = anchor;
        }
        for h in self.docs.values_mut().filter_map(|d| d.host.as_mut()) {
            h.set_options(host.clone());
        }
        self.history.cap = self.options.modehistory;
    }

    /// The current mode's name.
    pub fn current_mode(&self) -> &str {
        self.history.current()
    }

    pub fn paste(&mut self, text: &str) -> Outcome {
        if self.hold_paste(text) {
            return Outcome::None;
        }
        let out = self.paste_inner(text);
        out.merge(self.flush_events())
    }

    fn paste_inner(&mut self, text: &str) -> Outcome {
        if let Some(t) = self.focused_term() {
            return Outcome::None.or(t.paste(text));
        }
        match self.panes.get_mut(&self.focus).and_then(PaneEntry::pdf) {
            Some(pane) => Outcome::None.or(pane.paste(text) == pane::Action::Redraw),
            None => Outcome::None,
        }
    }

    /// Run an action. Errors (a file that cannot be opened, no room for a
    /// pane) are for whoever asked.
    pub fn action(&mut self, action: Action, now: Instant) -> Result<Outcome> {
        let out = self.action_inner(action, now)?;
        Ok(out.merge(self.flush_events()))
    }

    fn action_inner(&mut self, action: Action, _now: Instant) -> Result<Outcome> {
        match action {
            Action::Split { split, before, root } => {
                let spot = if self.is_float(self.focus) {
                    Spot::Float(self.cascade())
                } else {
                    Spot::Split { target: if root { None } else { Some(self.focus) }, split, before }
                };
                match self.panes.get(&self.focus).map(|e| &e.kind) {
                    Some(Kind::Pdf { doc, .. }) => {
                        let doc = *doc;
                        self.add_pdf_pane(doc, spot);
                    }
                    // A new shell where the focused one is.
                    Some(Kind::Term { buf, .. }) => {
                        let cwd = self.terms.get(buf).and_then(|t| t.term.pwd()).filter(|d| d.is_dir());
                        self.open_terminal(&Spawn { argv: None, cwd, env: Vec::new() }, spot)?;
                    }
                    None => {}
                }
            }
            Action::NewPane { spec, cwd } => self.new_pane(&spec, cwd.as_deref())?,
            Action::MoveFocus(dir) => {
                // Within the floats or within the tiled panes.
                let next = if self.is_float(self.focus) {
                    self.float_neighbor(dir)
                } else {
                    self.layout.as_ref().and_then(|l| l.neighbor(self.focus, dir, self.area()))
                };
                if let Some(p) = next {
                    self.set_focus(p);
                }
            }
            Action::FocusNextPane => {
                let order = self.focus_order();
                if let Some(i) = order.iter().position(|p| *p == self.focus) {
                    self.set_focus(order[(i + 1) % order.len()]);
                }
            }
            Action::ClosePane => return Ok(self.request_close(self.focus)),
            Action::Detach => return Ok(Outcome::Detach),
            Action::ReloadConfig => return self.reload_scripts(),
        }
        Ok(Outcome::Redraw)
    }

    /// Close a pane; its buffer stays, hidden, if no other pane shows it.
    /// Closing the last pane ends the session, hidden buffers or not (as if
    /// every buffer were Vim's `buftype=nofile`).
    /// A closing float leaves the focus to the float below it, else to the
    /// last focused tiled pane. The last tiled pane leaves its place to the
    /// topmost shown float; with none shown, the session ends.
    fn close(&mut self, id: PaneId) -> Outcome {
        let next = if self.is_float(id) {
            self.floats.retain(|f| f.pane != id);
            self.shown_floats().last().map_or(self.tiled_focus, |f| f.pane)
        } else {
            let top = self.shown_floats().last().map(|f| f.pane);
            let Some(layout) = self.layout.as_mut() else { return Outcome::Quit };
            if layout.panes().len() <= 1 {
                let Some(top) = top else { return Outcome::Quit };
                layout.replace(id, top);
                self.floats.retain(|f| f.pane != top);
                top
            } else {
                let next = layout.next(id);
                layout.remove(id);
                next
            }
        };
        if let Some(e) = self.panes.remove(&id) {
            if id == self.focus
                && let Some(t) = e.term().and_then(|b| self.terms.get_mut(&b))
            {
                t.term.focus(false);
            }
            self.hide(e.kind);
        }
        self.event("PaneClosed", vec![ScriptValue::Num(id as f64)]);
        if self.focus == id {
            self.set_focus(next);
        }
        if self.tiled_focus == id
            && let Some(first) = self.layout.as_ref().and_then(|l| l.panes().first().copied())
        {
            self.tiled_focus = first;
        }
        for p in [&mut self.hovered, &mut self.captured] {
            if *p == Some(id) {
                *p = None;
            }
        }
        // The layout changed under it.
        self.dragging = None;
        self.float_drag = None;
        self.relayout();
        Outcome::Redraw
    }

    /// The pane at a screen pixel.
    /// The shown floats come first, top down, with their frames.
    fn pane_at(&self, x: f32, y: f32) -> Option<PaneId> {
        let cell = self.geometry.cell;
        let inside = |r: &Rect| {
            let (x0, y0) = (r.x as f32 * cell.width as f32, r.y as f32 * cell.height as f32);
            x >= x0 && y >= y0 && x < x0 + (r.w * cell.width) as f32 && y < y0 + (r.h * cell.height) as f32
        };
        if let Some(f) = self.shown_floats().iter().rev().find(|f| inside(&f.frame)) {
            return Some(f.pane);
        }
        let tiled = self.layout.as_ref().map(|l| l.panes()).unwrap_or_default();
        tiled.into_iter().find(|id| self.panes.get(id).is_some_and(|e| inside(&e.rect)))
    }

    pub fn mouse(&mut self, m: &MouseEvent, now: Instant) -> Outcome {
        if self.hold_mouse(m) {
            return Outcome::None;
        }
        let out = self.mouse_inner(m, now);
        out.merge(self.flush_events())
    }

    fn mouse_inner(&mut self, m: &MouseEvent, now: Instant) -> Outcome {
        let cell = self.geometry.cell;
        let (px, py) = match m.unit {
            CoordUnit::Pixel => (m.x as f32, m.y as f32),
            CoordUnit::Cell => ((m.x as f32 + 0.5) * cell.width as f32, (m.y as f32 + 0.5) * cell.height as f32),
        };
        let under = self.pane_at(px, py);
        let target = self.captured.or(under);
        let mut out = Outcome::None;
        // The pointer left a pane: let it forget hover state (link previews).
        if self.hovered != under {
            if let Some(old) = self.hovered.take()
                && let Some(p) = self.panes.get_mut(&old).and_then(PaneEntry::pdf)
            {
                let away = MouseEvent { kind: MouseEventKind::Move, x: -100_000, y: -100_000, ..*m };
                out = out.or(p.mouse(&away, now));
            }
            self.hovered = under;
        }
        // Floats' frames lie over the borders (unless one is being dragged).
        let (col, row) = ((px / cell.width as f32).floor() as i32, (py / cell.height as f32).floor() as i32);
        if self.dragging.is_none()
            && let Some(frame) = self.float_mouse(m, col, row)
        {
            return out.merge(frame);
        }
        if let Some(border) = self.border_mouse(m, under, px, py) {
            return out.merge(border);
        }
        match m.kind {
            MouseEventKind::Press(_) => {
                self.captured = under;
                if let Some(p) = under
                    && p != self.focus
                {
                    // A click on another pane only focuses it: the press, its
                    // drags and its release stay out of the pane (a program
                    // there would act on a click meant to switch panes).
                    self.set_focus(p);
                    self.focus_click = true;
                    return Outcome::Redraw;
                }
            }
            MouseEventKind::Release(_) => {
                self.captured = None;
                if std::mem::take(&mut self.focus_click) {
                    return out;
                }
            }
            MouseEventKind::Drag(_) if self.focus_click => return out,
            _ => {}
        }
        let Some(id) = target else { return out };
        // In the operating mode, the left button selects in terminals even
        // when their program tracks the mouse.
        let select = self.history.current() == "normal";
        let Some(e) = self.panes.get_mut(&id) else { return out };
        let (dx, dy) = match m.unit {
            CoordUnit::Pixel => ((e.rect.x * cell.width as i32), (e.rect.y * cell.height as i32)),
            CoordUnit::Cell => (e.rect.x, e.rect.y),
        };
        let local = MouseEvent { x: m.x - dx, y: m.y - dy, ..*m };
        match &mut e.kind {
            Kind::Pdf { pane, .. } => out.or(pane.mouse(&local, now)),
            Kind::Term { buf, view } => match self.terms.get_mut(buf) {
                Some(t) => out.or(view.mouse(&mut t.term, &local, select)),
                None => out,
            },
        }
    }

    /// Borders: the pointer shape over them, and dragging them with the left
    /// button. `Some` when the event was the border's.
    fn border_mouse(&mut self, m: &MouseEvent, under: Option<PaneId>, px: f32, py: f32) -> Option<Outcome> {
        let cell = self.geometry.cell;
        let (col, row) = ((px / cell.width as f32).floor() as i32, (py / cell.height as f32).floor() as i32);
        let area = self.area();
        let layout = self.layout.as_mut()?;
        if let Some(id) = &self.dragging {
            return Some(match m.kind {
                MouseEventKind::Release(_) => {
                    self.dragging = None;
                    self.over_border = layout.border_at(area, col, row).map(|(_, s)| s);
                    self.fit_terms();
                    Outcome::Redraw
                }
                MouseEventKind::Drag(_) if layout.drag_border(id, col, row, area) => {
                    self.relayout();
                    Outcome::Redraw
                }
                _ => Outcome::None,
            });
        }
        // Over a pane, or a button held for one: not a border's.
        let border = if under.is_none() && self.captured.is_none() { layout.border_at(area, col, row) } else { None };
        self.over_border = border.as_ref().map(|(_, s)| *s);
        let (id, _) = border?;
        Some(match m.kind {
            MouseEventKind::Press(MouseButton::Left) => {
                self.dragging = Some(id);
                Outcome::Redraw
            }
            _ => Outcome::None,
        })
    }

    /// Effects for the frontend, including requests panes made since the last
    /// call (call every loop iteration: inverse search does not redraw).
    pub fn take_effects(&mut self) -> Vec<Effect> {
        for e in self.panes.values_mut() {
            match &mut e.kind {
                Kind::Pdf { pane, doc } => {
                    for req in pane.take_requests() {
                        if let Some(host) = self.docs.get_mut(doc).and_then(|d| d.host.as_mut())
                            && let Some(effect) = host.request(req)
                        {
                            self.effects.push(effect);
                        }
                    }
                }
                Kind::Term { .. } => {}
            }
        }
        for t in self.terms.values_mut() {
            self.effects.extend(t.term.take_effects());
        }
        // Links open with the configured command, in the client.
        for e in &mut self.effects {
            if let Effect::OpenUri { command, .. } = e
                && command.is_none()
            {
                command.clone_from(&self.options.linkopener);
            }
        }
        std::mem::take(&mut self.effects)
    }

    /// A frontend could not do what was asked (e.g. open a link).
    pub fn report(&mut self, message: String) {
        self.message(&message);
    }

    /// The pointer shape the frontend should show.
    pub fn pointer(&self) -> Pointer {
        if let Some(grip) = self.float_drag.map(|d| d.pointer()).or(self.over_frame.map(|g| g.pointer())) {
            return grip;
        }
        let border = self.dragging.as_ref().and_then(|id| self.layout.as_ref()?.split_of(id)).or(self.over_border);
        match border {
            Some(Split::Horizontal) => return Pointer::ResizeColumns,
            Some(Split::Vertical) => return Pointer::ResizeRows,
            None => {}
        }
        match self.hovered.and_then(|p| self.panes.get(&p)).map(|e| &e.kind) {
            Some(Kind::Pdf { pane, .. }) => pane.pointer(),
            Some(Kind::Term { buf, .. }) if self.terms.get(buf).is_some_and(|t| t.term.hovering_link()) => {
                Pointer::Link
            }
            _ => Pointer::Default,
        }
    }

    /// Compose this frame from every pane and hand their work to the documents.
    pub fn frame(&mut self) -> Frame {
        let cell = self.geometry.cell;
        // No screen-wide background: terminal panes show the terminal's own
        // background, so only the gaps between panes get one (border_layers).
        let mut layers = Vec::new();
        let mut text = Vec::new();
        let mut order = self.layout.as_ref().map(|l| l.panes()).unwrap_or_default();
        order.extend(self.shown_floats().iter().map(|f| f.pane));
        let mut needs: HashMap<DocId, Vec<Need>> = HashMap::new();
        let mut cursor = None;
        let mut cursor_shape = CursorShape::Default;
        let mut title = self.name.clone();
        let mut complete = true;
        // The focused pane's work first.
        let mut ids = order.clone();
        ids.sort_by_key(|id| *id != self.focus);
        for id in ids {
            // What lies above this pane, in cells and in pixels.
            let covers = self.covers(Some(id));
            let covers_px: Vec<Rect> = covers.iter().map(|r| self.pixels(r)).collect();
            let Some(e) = self.panes.get_mut(&id) else { continue };
            let frame = match &mut e.kind {
                Kind::Pdf { pane, doc } => {
                    let (frame, pane_needs) = pane.frame(&mut self.images);
                    needs.entry(*doc).or_default().extend(pane_needs);
                    complete &= pane.viewer.is_complete();
                    frame
                }
                Kind::Term { buf, view } => match self.terms.get_mut(buf) {
                    Some(t) => {
                        let mut frame = view.frame(&mut t.term, &mut self.images);
                        // Programs' images stay above the session's own layers.
                        for l in &mut frame.layers {
                            l.z = l.z.max(Z_BORDER_FOCUS + 1);
                        }
                        frame
                    }
                    None => Frame::default(),
                },
            };
            let (ox, oy) = (e.rect.x * cell.width as i32, e.rect.y * cell.height as i32);
            let clip = Rect::new(ox, oy, e.rect.w * cell.width, e.rect.h * cell.height);
            for l in frame.layers {
                let moved = Layer { dst: Rect::new(l.dst.x + ox, l.dst.y + oy, l.dst.w, l.dst.h), ..l };
                if let Some(c) = moved.clipped(&clip) {
                    self.keys.place(&mut layers, id, l.key, c, &covers_px);
                }
            }
            for g in frame.text {
                let g = TextGrid { col: g.col + e.rect.x as u16, row: g.row + e.rect.y as u16, ..g };
                text.extend(clip::text_pieces(g, &covers));
            }
            if id == self.focus {
                cursor = frame
                    .cursor
                    .map(|(c, r)| (c + e.rect.x as u16, r + e.rect.y as u16))
                    .filter(|&(c, r)| !covers.iter().any(|k| k.contains(c as i32, r as i32)));
                cursor_shape = frame.cursor_shape;
                title = frame.title;
            }
        }
        self.complete = complete;
        self.border_layers(&mut layers);
        self.float_frames(&mut layers, &mut text);
        self.keys.end_frame();
        for (doc, needs) in needs {
            if let Some(host) = self.docs.get_mut(&doc).and_then(|d| d.host.as_mut()) {
                host.dispatch(needs);
            }
        }
        // Floating messages over everything.
        text.extend(self.notice_grids());
        Frame { layers, text, cursor, cursor_shape, pointer: self.pointer(), title }
    }

    /// Lines in the one-cell gaps between panes; the focused pane's in the accent colour.
    fn border_layers(&mut self, layers: &mut Vec<Layer>) {
        let tiled = self.layout.as_ref().map(|l| l.panes()).unwrap_or_default();
        if tiled.len() < 2 {
            return;
        }
        let covers: Vec<Rect> = self.covers(None).iter().map(|r| self.pixels(r)).collect();
        let cell = self.geometry.cell;
        let (cols, rows) = (self.geometry.cols as i32, self.geometry.rows as i32);
        let accent = termpix_ui::Theme::dark().accent.0;
        let mut lines = Vec::new();
        let mut gaps = Vec::new();
        for (id, e) in tiled.iter().filter_map(|id| Some((*id, self.panes.get(id)?))) {
            let r = e.rect;
            let (x0, y0) = (r.x * cell.width as i32, r.y * cell.height as i32);
            let (w, h) = (r.w * cell.width, r.h * cell.height);
            let focus = id == self.focus;
            let (vx, vy) = ((cell.width - BORDER_PX) / 2, (cell.height - BORDER_PX) / 2);
            // The gap cells right of and below the pane (including the corner
            // where gaps cross).
            let (cw, ch) = (cell.width, cell.height);
            let right = r.right() < cols as i64;
            let below = r.bottom() < rows as i64;
            if right {
                gaps.push(Rect::new(x0 + w as i32, y0, cw, h + if below { ch } else { 0 }));
            }
            if below {
                gaps.push(Rect::new(x0, y0 + h as i32, w, ch));
            }
            if r.right() < cols as i64 {
                lines.push((focus, Rect::new(x0 + w as i32 + vx as i32, y0, BORDER_PX, h)));
            }
            if r.x > 0 {
                lines.push((focus, Rect::new(x0 - cell.width as i32 + vx as i32, y0, BORDER_PX, h)));
            }
            if r.bottom() < rows as i64 {
                lines.push((focus, Rect::new(x0, y0 + h as i32 + vy as i32, w, BORDER_PX)));
            }
            if r.y > 0 {
                lines.push((focus, Rect::new(x0, y0 - cell.height as i32 + vy as i32, w, BORDER_PX)));
            }
        }
        for (i, r) in gaps.into_iter().enumerate() {
            let image = self.solid(r.w, r.h, BACKGROUND);
            let layer = Layer { key: 0, image, src: Rect::new(0, 0, r.w, r.h), dst: r, z: Z_SESSION_BACKGROUND };
            self.keys.place(layers, OWNER_SESSION, 0x1000 + i as u32, layer, &covers);
        }
        for (i, (focus, r)) in lines.into_iter().enumerate() {
            let image = self.solid(r.w, r.h, if focus { accent } else { BORDER });
            let z = if focus { Z_BORDER_FOCUS } else { Z_BORDER };
            let layer = Layer { key: 0, image, src: Rect::new(0, 0, r.w, r.h), dst: r, z };
            self.keys.place(layers, OWNER_SESSION, 1 + i as u32, layer, &covers);
        }
    }

    /// A rectangle of cells in screen pixels.
    fn pixels(&self, r: &Rect) -> Rect {
        let cell = self.geometry.cell;
        Rect::new(r.x * cell.width as i32, r.y * cell.height as i32, r.w * cell.width, r.h * cell.height)
    }

    fn solid(&mut self, w: u32, h: u32, color: [u8; 4]) -> ImageKey {
        if self.solids.len() > 64 {
            for (_, id) in self.solids.drain() {
                self.images.remove(id);
            }
        }
        let images = &mut self.images;
        *self.solids.entry((w, h, color)).or_insert_with(|| images.add(w, h, color.repeat(w as usize * h as usize)))
    }

    /// Whether every pane showed its visible pages fully rendered last frame.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Nothing is being opened, loaded, rendered or searched.
    pub fn is_idle(&self) -> bool {
        self.docs.values().all(|d| d.host.as_ref().is_some_and(|h| h.doc.borrow().cache.is_idle()))
            && self.panes.values().all(|e| match &e.kind {
                Kind::Pdf { pane, .. } => !pane.search.is_active() || pane.search.is_complete(),
                Kind::Term { .. } => true,
            })
    }

    pub fn is_animating(&self) -> bool {
        self.panes.values().any(|e| matches!(&e.kind, Kind::Pdf { pane, .. } if pane.is_animating()))
    }

    /// Advance animations and timers. Returns whether to redraw.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for p in self.panes.values_mut().filter_map(PaneEntry::pdf) {
            changed |= p.tick(now);
            // A highlight expired.
            changed |= p.next_deadline().is_some_and(|d| now >= d);
        }
        for (&id, d) in self.docs.iter_mut() {
            if let Some(host) = d.host.as_mut() {
                let mut panes = Self::panes_of(&mut self.panes, id, self.focus);
                changed |= host.poll(&mut panes, now);
            }
        }
        for t in self.terms.values_mut() {
            changed |= t.term.autoscroll(now);
        }
        changed
    }

    /// [`Session::tick`], plus scripting's timers (the configuration's load
    /// timeout, the "running a script" notice). For event loops.
    pub fn poll(&mut self, now: Instant) -> Outcome {
        let mut out = self.script_tick(now).or(self.tick(now)).or(self.expire_notices(now));
        if self.matcher.deadline(self.options.timeoutlen).is_some_and(|d| now >= d) {
            out = out.merge(self.drive(now));
        }
        out.merge(self.flush_events())
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let panes = self.panes.values().filter_map(|e| match &e.kind {
            Kind::Pdf { pane, .. } => pane.next_deadline(),
            Kind::Term { .. } => None,
        });
        let docs = self.docs.values().filter_map(|d| d.host.as_ref()?.next_deadline());
        let terms = self.terms.values().filter_map(|t| t.term.autoscroll_deadline());
        panes
            .chain(docs)
            .chain(terms)
            .chain(self.script_deadline())
            .chain(self.matcher.deadline(self.options.timeoutlen))
            .chain(self.notice_deadline())
            .min()
    }

    pub fn focus(&self) -> PaneId {
        self.focus
    }

    /// The files open in the session.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut files: Vec<_> = self.docs.values().map(|d| d.path.clone()).collect();
        files.sort();
        files
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// The focused pane when it shows a PDF (for options applied after
    /// start-up, e.g. `--page`).
    pub fn focused_pane(&mut self) -> Option<&mut PdfPane> {
        self.panes.get_mut(&self.focus).and_then(PaneEntry::pdf)
    }

    /// Ask for a forward search in the focused pane's document.
    pub fn forward(&mut self, req: ForwardRequest) {
        if let Some(doc) = self.panes.get(&self.focus).and_then(PaneEntry::doc) {
            self.handle(SessionEvent::Forward(doc, req), Instant::now());
        }
    }
}

/// The built-in modes, without mappings (the shipped JavaScript defaults,
/// `crates/temzdf-script/runtime/defaults.ts`, map them): `normal`
/// (operating), `terminal` (pass-through) and `pdf`.
fn builtin_modes() -> Modes<Target> {
    let mut m = Modes::default();
    for (name, unmapped, count) in
        [("normal", Unmapped::Drop, true), ("terminal", Unmapped::Passthrough, false), ("pdf", Unmapped::Drop, true)]
    {
        let d = m.define(name);
        d.unmapped = Some(unmapped);
        d.count = Some(count);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Act, Stroke, notation};
    use crate::layout::Direction;
    use temzdf_pdfview::keys::Keymap;

    /// What the shipped defaults (runtime/defaults.ts) map, for sessions
    /// without a script host.
    fn default_mappings(m: &mut Modes<Target>) {
        let keys = |s: &str| notation::parse(s).expect("keys");
        let acts =
            |v: &[(&str, &[ScriptValue])]| Target::Actions(v.iter().map(|(n, a)| Act::new(n, a.to_vec())).collect());
        let s = |x: &str| ScriptValue::Str(x.into());
        for (lhs, target) in [
            ("%", acts(&[("split", &[s("right")])])),
            ("\"", acts(&[("split", &[s("bottom")])])),
            ("h", acts(&[("move-focus", &[s("left")])])),
            ("j", acts(&[("move-focus", &[s("down")])])),
            ("k", acts(&[("move-focus", &[s("up")])])),
            ("l", acts(&[("move-focus", &[s("right")])])),
            ("o", acts(&[("focus-next-pane", &[])])),
            ("w", acts(&[("toggle-floats", &[])])),
            ("W", acts(&[("open-float", &[])])),
            ("e", acts(&[("toggle-floating", &[]), ("exit-mode", &[])])),
            ("+", acts(&[("resize-pane", &[s("taller")])])),
            ("-", acts(&[("resize-pane", &[s("shorter")])])),
            ("<lt>", acts(&[("resize-pane", &[s("narrower")])])),
            (">", acts(&[("resize-pane", &[s("wider")])])),
            ("H", acts(&[("move-pane", &[s("left")])])),
            ("J", acts(&[("move-pane", &[s("bottom")])])),
            ("K", acts(&[("move-pane", &[s("top")])])),
            ("L", acts(&[("move-pane", &[s("right")])])),
            ("r", acts(&[("rotate-panes", &[])])),
            ("R", acts(&[("rotate-panes", &[ScriptValue::Bool(true)])])),
            ("x", acts(&[("close-pane", &[])])),
            ("d", acts(&[("detach", &[]), ("exit-mode", &[])])),
            ("q", acts(&[("kill-session", &[])])),
            ("<C-b>", acts(&[("exit-mode", &[]), ("feedkeys", &[s("<C-b>"), ScriptValue::Bool(false)])])),
            ("<Esc>", acts(&[("exit-mode", &[])])),
        ] {
            m.map("normal", keys(lhs), target, false, None);
        }
        for mode in ["terminal", "pdf"] {
            m.map(mode, keys("<C-b>"), acts(&[("switch-mode", &[s("normal")])]), false, None);
        }
        for (key, cmd) in Keymap::default().bindings() {
            let stroke = Stroke::of(&key.event());
            if stroke != keys("<C-c>")[0] {
                m.map("pdf", vec![stroke], Target::Actions(vec![Act::new(cmd.name(), vec![])]), false, None);
            }
        }
    }
    use std::sync::mpsc::{self, Receiver};
    use temzdf_pdfcore::doc_cache::TileKey;
    use temzdf_pdfcore::pdf;
    use termpix_core::geom::CellSize;
    use termpix_core::input::Modifiers;

    const GEOMETRY: Geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };

    fn session() -> (Session, Receiver<SessionEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("test".into(), GEOMETRY, Hints::default(), settings, sink);
        default_mappings(&mut s.modes);
        s.open(&pdf::fixture("sample.pdf")).unwrap();
        while s.is_starting() {
            let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            s.handle(ev, Instant::now());
        }
        (s, rx)
    }

    /// Run frames and the workers' results until idle; returns the last frame
    /// and the keys of every tile rendered meanwhile.
    fn settle(s: &mut Session, rx: &Receiver<SessionEvent>) -> (Frame, Vec<TileKey>) {
        let mut rendered = Vec::new();
        loop {
            s.take_effects();
            let frame = s.frame();
            if s.is_idle() {
                return (frame, rendered);
            }
            let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            if let SessionEvent::Doc(_, DocEvent::Rendered(r)) = &ev {
                rendered.push(r.key);
            }
            s.handle(ev, Instant::now());
        }
    }

    fn key(s: &mut Session, spec: &str) -> Outcome {
        let mut out = Outcome::None;
        for k in notation::parse(spec).unwrap() {
            out = out.merge(s.key(&k.event(), Instant::now()));
        }
        out
    }

    fn click(s: &mut Session, x: i32, y: i32) {
        for kind in [MouseEventKind::Press(MouseButton::Left), MouseEventKind::Release(MouseButton::Left)] {
            let m = MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE };
            s.mouse(&m, Instant::now());
        }
    }

    #[test]
    fn prefix_keys_split_focus_and_close() {
        let (mut s, rx) = session();
        settle(&mut s, &rx);
        let first = s.focus();
        assert_eq!(key(&mut s, "<C-b>"), Outcome::Redraw, "the normal mode");
        assert_eq!(s.current_mode(), "normal");
        assert_eq!(key(&mut s, "%"), Outcome::Redraw);
        assert_eq!(s.pane_count(), 2);
        let second = s.focus();
        assert_ne!(first, second, "the new pane has the focus");
        key(&mut s, "<C-b>");
        key(&mut s, "h");
        assert_eq!(s.focus(), first);
        // A click on the right half focuses the pane there.
        click(&mut s, 700, 200);
        assert_eq!(s.focus(), second);
        key(&mut s, "<C-b>");
        assert_eq!(key(&mut s, "x"), Outcome::Redraw);
        assert_eq!((s.pane_count(), s.focus()), (1, first));
        key(&mut s, "<C-b>");
        assert_eq!(key(&mut s, "d"), Outcome::Detach);
        assert_eq!(key(&mut s, "q"), Outcome::Quit, "quitting the last pane ends the session");
    }

    #[test]
    fn panes_are_composed_inside_their_rectangles_with_unique_keys() {
        let (mut s, rx) = session();
        s.action(Action::Split { split: Split::Horizontal, before: false, root: false }, Instant::now()).unwrap();
        let (frame, _) = settle(&mut s, &rx);
        let mut keys = HashSet::new();
        for l in &frame.layers {
            assert!(keys.insert(l.key), "duplicate key {}", l.key);
            let in_left = l.dst.right() <= 440;
            let in_right = l.dst.x >= 450;
            let screen = l.dst == Rect::new(0, 0, 900, 600);
            let border = l.dst.w == BORDER_PX || l.dst.h == BORDER_PX;
            let gap = l.dst.x == 440 && l.dst.w == 10;
            assert!(in_left || in_right || screen || border || gap, "{:?} crosses the panes", l.dst);
        }
        // The focused pane's title is the window title.
        assert!(frame.title.starts_with("sample.pdf"), "{}", frame.title);
    }

    #[test]
    fn panes_on_one_document_render_each_tile_once() {
        let (mut s, rx) = session();
        // Stacked panes have the same width, so the same zoom and tiles; both
        // ask for them in the first frame. (The deduplication itself is
        // tested deterministically in temzdf-pdfview's host.rs.)
        s.action(Action::Split { split: Split::Vertical, before: false, root: false }, Instant::now()).unwrap();
        let (_, rendered) = settle(&mut s, &rx);
        assert!(!rendered.is_empty());
        let unique: HashSet<_> = rendered.iter().collect();
        assert_eq!(unique.len(), rendered.len(), "no tile rendered twice: {rendered:?}");
    }

    #[test]
    fn closing_a_pane_hides_its_document_until_it_is_shown_again() {
        let (mut s, rx) = session();
        settle(&mut s, &rx);
        let first = s.panes[&s.focus].buffer();
        s.action(Action::NewPane { spec: PaneSpec::Pdf(pdf::fixture("sample-short.pdf")), cwd: None }, Instant::now())
            .unwrap();
        while s.pane_count() < 2 {
            let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            s.handle(ev, Instant::now());
        }
        settle(&mut s, &rx);
        let pane = s.focus;
        let doc = s.panes[&pane].buffer();
        s.panes.get_mut(&pane).unwrap().pdf().unwrap().viewer.jump_to(0.0, 700.0);
        let before = s.panes.get_mut(&pane).unwrap().pdf().unwrap().viewer.view_state();
        s.action(Action::ClosePane, Instant::now()).unwrap();
        assert_eq!((s.pane_count(), s.buffer_count()), (1, 2), "the document stays, hidden");
        {
            let host = s.docs[&doc].host.as_ref().unwrap();
            let d = host.doc.borrow();
            assert!((0..d.cache.page_count()).all(|p| d.cache.display_list(p).is_none()), "its display lists went");
        }
        // Shown again (in the other pane), where it was.
        assert_eq!(s.show_in(s.focus, doc), Ok(Outcome::Redraw));
        settle(&mut s, &rx);
        let focus = s.focus;
        let after = s.panes.get_mut(&focus).unwrap().pdf().unwrap().viewer.view_state();
        assert_eq!(after.top.page, before.top.page);
        assert!((after.top.y.unwrap() - before.top.y.unwrap()).abs() < 1.0, "{after:?} {before:?}");
        // Closing the last pane ends the session, even with a buffer hidden.
        assert_eq!((s.pane_count(), s.buffer_count()), (1, 2));
        assert_eq!(s.close(focus), Outcome::Quit);
        // Deleting the hidden buffer leaves the pane alone; deleting the last
        // pane's ends the session.
        let hidden = [ScriptValue::Num(first as f64)];
        assert_eq!(s.run_command("delete-buffer", &hidden, None, Instant::now()).map(|r| r.1), Ok(Outcome::Redraw));
        assert_eq!((s.buffer_count(), s.panes[&focus].buffer()), (1, doc));
        assert_eq!(s.run_command("delete-buffer", &[], None, Instant::now()).map(|r| r.1), Ok(Outcome::Quit));
    }

    #[test]
    fn opening_a_missing_file_fails_the_session() {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
        s.open(Path::new("/nonexistent/x.pdf")).unwrap();
        let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(s.handle(ev, Instant::now()), Outcome::Quit);
        assert!(s.error().is_some());
    }

    /// Present `frame` through `comp`, appending the terminal bytes to `out`.
    fn present(
        s: &mut Session,
        frame: &Frame,
        comp: &mut termpix::compositor::Compositor,
        presenter: &mut temzdf_tty::Presenter,
        out: &mut Vec<u8>,
    ) {
        presenter.sync(comp, s.images());
        comp.present(&presenter.layers(&frame.layers), out).unwrap();
    }

    #[test]
    fn resizing_split_panes_redraws_like_a_fresh_start() {
        use termpix::compositor::{Backend, Compositor};
        use termpix::graphics::Medium;
        use termpix::mock::MockTerminal;
        let big = Geometry { cols: 100, rows: 34, ..GEOMETRY };
        let (mut s, rx) = session();
        s.action(Action::Split { split: Split::Horizontal, before: false, root: false }, Instant::now()).unwrap();
        s.action(Action::Split { split: Split::Vertical, before: false, root: false }, Instant::now()).unwrap();
        let mut comp = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, GEOMETRY);
        let mut presenter = temzdf_tty::Presenter::new();
        let mut out = Vec::new();
        let (frame, _) = settle(&mut s, &rx);
        present(&mut s, &frame, &mut comp, &mut presenter, &mut out);
        // The window grows (same cells, more of them): replay everything sent
        // since the start into a terminal of the new size.
        s.resize(big);
        comp.set_geometry(big);
        let (frame, _) = settle(&mut s, &rx);
        present(&mut s, &frame, &mut comp, &mut presenter, &mut out);
        let mut term = MockTerminal::new(big, [0, 0, 0, 255]);
        term.feed(&out);
        let resized = term.render();

        // The same layout (panes keep their sizes when the screen changes),
        // drawn only at the new size.
        let (mut fresh, rx2) = session();
        fresh.action(Action::Split { split: Split::Horizontal, before: false, root: false }, Instant::now()).unwrap();
        fresh.action(Action::Split { split: Split::Vertical, before: false, root: false }, Instant::now()).unwrap();
        fresh.resize(big);
        let mut comp2 = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, big);
        let (frame2, _) = settle(&mut fresh, &rx2);
        let mut out2 = Vec::new();
        present(&mut fresh, &frame2, &mut comp2, &mut temzdf_tty::Presenter::new(), &mut out2);
        let mut term2 = MockTerminal::new(big, [0, 0, 0, 255]);
        term2.feed(&out2);
        let expected = term2.render();
        if let Ok(dir) = std::env::var("TEMZDF_TEST_PNG") {
            resized.save_png(&Path::new(&dir).join("resized.png")).unwrap();
            expected.save_png(&Path::new(&dir).join("expected.png")).unwrap();
        }
        let differing = resized.data.chunks(4).zip(expected.data.chunks(4)).filter(|(a, b)| a != b).count();
        assert_eq!(differing, 0, "{differing} pixels differ");
    }

    fn terminal_session(argv: &[&str]) -> (Session, Receiver<SessionEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("term".into(), GEOMETRY, Hints::default(), settings, sink);
        // Splits start this instead of the user's $SHELL.
        s.shell = Some(vec!["/bin/sh".into()]);
        default_mappings(&mut s.modes);
        let spec = PaneSpec::Command(argv.iter().map(|a| a.to_string()).collect());
        s.new_pane(&spec, None).unwrap();
        (s, rx)
    }

    /// Handle events until the composed text satisfies `done`; returns the
    /// last outcome and the screen's rows.
    fn wait_text(
        s: &mut Session,
        rx: &Receiver<SessionEvent>,
        done: impl Fn(&[String]) -> bool,
    ) -> (Outcome, Vec<String>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut outcome = Outcome::None;
        loop {
            let rows = screen_text(&s.frame(), GEOMETRY);
            if done(&rows) || outcome == Outcome::Quit || Instant::now() > deadline {
                return (outcome, rows);
            }
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
                outcome = s.handle(ev, Instant::now());
            }
        }
    }

    /// The frame's text as screen rows (trailing blanks trimmed).
    fn screen_text(frame: &Frame, g: Geometry) -> Vec<String> {
        let mut screen = vec![vec![' '; g.cols as usize]; g.rows as usize];
        for grid in &frame.text {
            for (dy, row) in grid.rows.iter().enumerate() {
                for (dx, c) in row.iter().enumerate() {
                    let (x, y) = (grid.col as usize + dx, grid.row as usize + dy);
                    if let (Some(ch), true) = (c.text.chars().next(), y < screen.len() && x < screen[0].len()) {
                        screen[y][x] = ch;
                    }
                }
            }
        }
        screen.into_iter().map(|r| r.into_iter().collect::<String>().trim_end().to_owned()).collect()
    }

    #[test]
    fn terminal_panes_show_output_take_keys_and_close_when_the_program_exits() {
        let (mut s, rx) = terminal_session(&[
            "sh",
            "-c",
            "stty -echo -ixon; echo ready; read l; echo \"got:$(printf %s \"$l\" | od -An -tx1 | tr -d ' ')\"; read l",
        ]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        // The prefix twice sends it itself.
        key(&mut s, "<C-b>");
        key(&mut s, "<C-b>");
        key(&mut s, "a<CR>");
        let (_, rows) = wait_text(&mut s, &rx, |r| r[1].starts_with("got:"));
        assert_eq!(rows[1], "got:0261", "C-b (0x02), then a");
        assert!(s.frame().cursor.is_some());
        key(&mut s, "<CR>");
        let (outcome, _) = wait_text(&mut s, &rx, |_| false);
        assert_eq!(outcome, Outcome::Quit, "the last pane's program exited");
    }

    #[test]
    fn splitting_a_terminal_starts_another_shell_beside_it() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo left; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "left");
        key(&mut s, "<C-b>");
        assert_eq!(key(&mut s, "%"), Outcome::Redraw);
        assert_eq!(s.pane_count(), 2);
        for c in "echo right-$((40+2))".chars() {
            s.key(&KeyEvent::new(KeyCode::Char(c), Modifiers::NONE), Instant::now());
        }
        key(&mut s, "<CR>");
        let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains("right-42")));
        // The left pane keeps its text in its half; the new shell is in the right one.
        assert!(rows[0].starts_with("left"), "{rows:?}");
        let line = rows.iter().find(|l| l.contains("right-42")).unwrap();
        assert!(line.find("right-42").unwrap() >= 45, "{line:?}");
        // Closing it gives the first pane the whole screen again.
        key(&mut s, "<C-b>");
        key(&mut s, "x");
        assert_eq!(s.pane_count(), 1);
        // A bigger window: the pane (and its program's pty) grows with it.
        s.resize(Geometry { cols: 100, rows: 34, ..GEOMETRY });
        let frame = s.frame();
        assert_eq!((frame.text[0].rows.len(), frame.text[0].rows[0].len()), (34, 100));
    }

    /// Handle events until `done` holds (or ten seconds pass); returns the
    /// strongest outcome meanwhile.
    fn pump(s: &mut Session, rx: &Receiver<SessionEvent>, done: impl Fn(&Session) -> bool) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut out = Outcome::None;
        while !done(s) && Instant::now() < deadline {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
                out = out.merge(s.handle(ev, Instant::now()));
            }
        }
        out
    }

    #[test]
    fn closed_terminals_keep_running_hidden() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; echo first; read l; echo \"got:$l\"; read l"]);
        wait_text(&mut s, &rx, |r| r[0] == "first");
        let first = s.panes[&s.focus].buffer();
        key(&mut s, "<C-b>%");
        let second = s.panes[&s.focus].buffer();
        key(&mut s, "<C-b>h<C-b>x");
        assert_eq!((s.pane_count(), s.buffer_count()), (1, 2));
        assert_eq!(s.panes[&s.focus].buffer(), second);
        // Shown again, it takes input.
        assert_eq!(s.show_in(s.focus, first), Ok(Outcome::Redraw));
        assert_eq!(s.current_mode(), "terminal");
        key(&mut s, "hi<CR>");
        let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l == "got:hi"));
        assert_eq!(rows[0], "first", "{rows:?}");
        // Its program ends: the last pane closes, and the session ends
        // although the other shell still runs hidden.
        key(&mut s, "<CR>");
        let out = pump(&mut s, &rx, |s| s.terms[&first].exited);
        assert_eq!(out, Outcome::Quit);
        assert!(!s.terms[&second].exited);
    }

    #[test]
    fn a_hidden_terminal_that_ended_stays_until_it_was_shown() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo started; sleep 1; echo bye"]);
        wait_text(&mut s, &rx, |r| r[0] == "started");
        let first = s.panes[&s.focus].buffer();
        key(&mut s, "<C-b>%<C-b>h<C-b>x");
        pump(&mut s, &rx, |s| s.terms[&first].exited);
        assert_eq!((s.pane_count(), s.buffer_count()), (1, 2));
        let list = s.buffer_list();
        let ScriptValue::List(items) = &list else { panic!() };
        let entry = items.iter().find(|b| b.get("id") == Some(&ScriptValue::Num(first as f64))).unwrap();
        assert_eq!(entry.get("exited"), Some(&ScriptValue::Bool(true)));
        s.show_in(s.focus, first).unwrap();
        let (_, rows) = wait_text(&mut s, &rx, |r| r.get(1).is_some_and(|l| l == "bye"));
        assert_eq!(&rows[..2], ["started", "bye"]);
        // Hidden again, it goes.
        let shown = s.focus;
        key(&mut s, "<C-b>%");
        assert_eq!(s.close(shown), Outcome::Redraw);
        assert!(!s.terms.contains_key(&first));
        assert_eq!(s.buffer_count(), 2);
    }

    #[test]
    fn a_terminal_in_two_panes_is_as_large_as_the_larger() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; echo ready; while read l; do stty size; done"]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        let first = s.panes[&s.focus].buffer();
        key(&mut s, "<CR>");
        wait_text(&mut s, &rx, |r| r.iter().any(|l| l == "30 90"));
        key(&mut s, "<C-b>%");
        let right = s.focus;
        s.show_in(right, first).unwrap();
        let widest = s.panes.values().map(|e| e.rect.w).max().unwrap();
        assert!(widest < 90);
        key(&mut s, "<CR>");
        let want = format!("30 {widest}");
        let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains(&want)));
        assert!(rows.iter().any(|l| l.contains(&want)), "{rows:?}");
        // One pane left: the whole width again.
        s.close(right);
        key(&mut s, "<CR>");
        let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().filter(|l| *l == "30 90").count() == 2);
        assert_eq!(rows.iter().filter(|l| *l == "30 90").count(), 2, "{rows:?}");
    }

    #[test]
    fn dragging_a_border_resizes_panes_and_their_programs_on_release() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo ready; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        let left = s.focus;
        key(&mut s, "<C-b>%");
        let right = s.focus;
        let (term, width) = (s.panes[&left].buffer(), |s: &Session, p| s.panes[&p].rect.w);
        let cols = |s: &Session| s.terms[&term].term.geometry().cols;
        let border = width(&s, left) as i32;
        let at = |kind, col: i32| MouseEvent {
            kind,
            x: col * 10 + 5,
            y: 105,
            unit: CoordUnit::Pixel,
            modifiers: Modifiers::NONE,
        };
        let now = Instant::now();
        s.mouse(&at(MouseEventKind::Move, border - 3), now);
        assert_eq!(s.pointer(), Pointer::Default);
        s.mouse(&at(MouseEventKind::Move, border), now);
        assert_eq!(s.pointer(), Pointer::ResizeColumns, "over the border");
        s.mouse(&at(MouseEventKind::Press(MouseButton::Left), border), now);
        assert_eq!(s.mouse(&at(MouseEventKind::Drag(MouseButton::Left), 60), now), Outcome::Redraw);
        assert_eq!((width(&s, left), width(&s, right)), (60, 29));
        assert_eq!(cols(&s), border as u16, "the program keeps its size while dragging");
        assert_eq!(s.pointer(), Pointer::ResizeColumns, "anywhere while dragging");
        s.mouse(&at(MouseEventKind::Drag(MouseButton::Left), 70), now);
        s.mouse(&at(MouseEventKind::Release(MouseButton::Left), 70), now);
        assert_eq!((width(&s, left), cols(&s)), (70, 70), "and gets the new size on release");
        assert_eq!(s.focus, right, "the border took the press");
        // The keys size the focused pane (as Vim's <C-w>> and <C-w><).
        assert_eq!(key(&mut s, "<C-b>3>"), Outcome::Redraw);
        assert_eq!((width(&s, left), cols(&s)), (67, 67));
        key(&mut s, "<lt>");
        assert_eq!(width(&s, left), 68, "the normal mode stays");
        assert_eq!(s.current_mode(), "normal");
    }

    #[test]
    fn the_shipped_keys_move_and_rotate_panes() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo ready; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        let one = s.focus;
        key(&mut s, "<C-b>%");
        let two = s.focus;
        key(&mut s, "<C-b>\"");
        let three = s.focus;
        let panes = |s: &Session| s.layout.as_ref().unwrap().panes();
        assert_eq!(panes(&s), [one, two, three]);
        // H: the full height at the left; K: the full width at the top.
        key(&mut s, "<C-b>H");
        assert_eq!(panes(&s), [three, one, two]);
        assert_eq!(s.panes[&three].rect.h, s.area().h);
        key(&mut s, "K");
        assert_eq!((s.panes[&three].rect.y, s.panes[&three].rect.w), (0, s.area().w));
        // r: the row of the others turns; R back.
        s.set_focus(one);
        key(&mut s, "<C-b>r");
        assert_eq!(panes(&s), [three, two, one]);
        key(&mut s, "R");
        assert_eq!(panes(&s), [three, one, two]);
        assert_eq!(s.focus, one, "the focus stays with its pane");
        // Sizes: + and - (one pane in a column: its border moves).
        let h = s.panes[&one].rect.h;
        key(&mut s, "2-");
        assert_eq!(s.panes[&one].rect.h, h - 2);
        key(&mut s, "+");
        assert_eq!(s.panes[&one].rect.h, h - 1);
    }

    /// Open a floating pane running `argv` with `opts`; returns it.
    fn float(s: &mut Session, argv: &[&str], opts: &[(&str, ScriptValue)]) -> PaneId {
        let what = ScriptValue::map([(
            "command",
            ScriptValue::List(argv.iter().map(|a| ScriptValue::Str((*a).into())).collect()),
        )]);
        let opts = ScriptValue::map(opts.iter().cloned());
        let (pane, _) = s.run_command("open-float", &[what, opts], None, Instant::now()).unwrap();
        pane.as_num().expect("a pane") as PaneId
    }

    #[test]
    fn a_float_covers_the_panes_below_it() {
        let (mut s, rx) =
            terminal_session(&["sh", "-c", "for i in 1 2 3 4 5 6 7 8; do printf '%080d\\n' 0; done; sleep 30"]);
        let zeros = "0".repeat(80);
        wait_text(&mut s, &rx, |r| r[7] == zeros);
        let n = |v: f64| ScriptValue::Num(v);
        let opts = [
            ("width", n(20.0)),
            ("height", n(4.0)),
            ("col", n(5.0)),
            ("row", n(2.0)),
            ("title", ScriptValue::Str("pop".into())),
        ];
        let pane = float(&mut s, &["sh", "-c", "echo FLOAT; sleep 30"], &opts);
        assert_eq!((s.focus, s.current_mode()), (pane, "terminal"));
        // The frame is 22 x 6 cells at (5, 2); the content inside it.
        assert_eq!(s.panes[&pane].rect, Rect::new(6, 3, 20, 4));
        let (_, rows) = wait_text(&mut s, &rx, |r| r[3].get(6..11) == Some("FLOAT"));
        assert_eq!(rows[1], zeros, "above the float");
        assert_eq!(&rows[2][7..12], " pop ", "the title on the frame");
        for (y, row) in rows.iter().enumerate().take(8).skip(2) {
            let inside: String = row.chars().skip(5).take(22).collect();
            assert!(!inside.contains('0'), "row {y}: the pane below shows through: {row:?}");
            assert_eq!(row.chars().nth(4), Some('0'), "row {y}: left of the float");
            assert_eq!(row.chars().nth(27), Some('0'), "row {y}: right of the float");
        }
        // The frame is drawn: its lines lie within the float's cells.
        let frame = s.frame();
        let outer = s.pixels(&Rect::new(5, 2, 22, 6));
        let lines = frame.layers.iter().filter(|l| l.z == Z_BORDER_FOCUS).count();
        assert_eq!(lines, 5, "the focused float's four lines, the top one in two around the title");
        assert!(frame.layers.iter().filter(|l| l.z == Z_BORDER_FOCUS).all(|l| outer.intersect(&l.dst) == Some(l.dst)));
        // Its terminal is as large as its content.
        let buf = s.panes[&pane].buffer();
        assert_eq!((s.terms[&buf].term.geometry().cols, s.terms[&buf].term.geometry().rows), (20, 4));
    }

    #[test]
    fn floats_cascade_raise_take_turns_and_hide_together() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "tiled");
        let tiled = s.focus;
        let a = float(&mut s, &["sleep", "30"], &[]);
        // Splitting a float opens another float beside it.
        s.action(Action::Split { split: Split::Horizontal, before: false, root: false }, Instant::now()).unwrap();
        let b = s.focus;
        assert_ne!(b, a);
        assert_eq!(s.pane_count(), 3);
        let (fa, fb) = (s.floats[0].frame, s.floats[1].frame);
        assert_eq!((fb.x, fb.y, fb.w, fb.h), (fa.x + 2, fa.y + 2, fa.w, fa.h), "two cells right and down");
        // Focus moves among the floats, raising them.
        s.action(Action::MoveFocus(Direction::Left), Instant::now()).unwrap();
        assert_eq!((s.focus, s.floats.last().unwrap().pane), (a, a));
        // The cycle: floats from the top, then the tiled panes.
        assert_eq!(s.focus_order(), [a, b, tiled]);
        s.action(Action::FocusNextPane, Instant::now()).unwrap();
        assert_eq!(s.focus, b);
        s.action(Action::FocusNextPane, Instant::now()).unwrap();
        assert_eq!(s.focus, tiled, "a tiled pane takes the focus; the floats stay");
        assert_eq!(s.covers(Some(tiled)).len(), 2);
        // Hidden: the focus goes to the tiled pane, nothing covers it.
        s.action(Action::FocusNextPane, Instant::now()).unwrap();
        s.run_command("toggle-floats", &[], None, Instant::now()).unwrap();
        assert_eq!((s.focus, s.covers(Some(tiled)).len()), (tiled, 0));
        assert!(s.pane_at(10.0, 10.0) == Some(tiled), "hidden floats take no clicks");
        // Shown again: the top float has the focus.
        s.run_command("toggle-floats", &[], None, Instant::now()).unwrap();
        assert_eq!(s.focus, s.floats.last().unwrap().pane);
        // Moving and resizing by commands.
        s.run_command(
            "float-config",
            &[
                ScriptValue::Num(a as f64),
                ScriptValue::map([("width", ScriptValue::Num(30.0)), ("col", ScriptValue::Num(0.0))]),
            ],
            None,
            Instant::now(),
        )
        .unwrap();
        let fa = s.floats.iter().find(|f| f.pane == a).unwrap().frame;
        assert_eq!((fa.x, fa.w), (0, 32));
        s.set_focus(a);
        s.run_command("resize-pane", &[ScriptValue::Str("left".into()), ScriptValue::Num(5.0)], None, Instant::now())
            .unwrap();
        assert_eq!(s.panes[&a].rect.w, 25, "the right edge moved left");
        let ScriptValue::List(panes) = s.run_command("panes", &[], None, Instant::now()).unwrap().0 else { panic!() };
        let floating: Vec<bool> = panes.iter().map(|p| p.get("floating") == Some(&ScriptValue::Bool(true))).collect();
        assert_eq!(floating.iter().filter(|f| **f).count(), 2);
    }

    #[test]
    fn the_last_tiled_pane_gives_way_to_a_shown_float() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "tiled");
        let tiled = s.focus;
        // The last tiled pane does not float.
        let err = s.run_command("toggle-floating", &[], None, Instant::now()).unwrap_err();
        assert!(err.contains("last tiled pane"), "{err}");
        let f = float(&mut s, &["sh", "-c", "echo floating; sleep 30"], &[]);
        // Hidden floats do not keep the session.
        s.run_command("toggle-floats", &[], None, Instant::now()).unwrap();
        assert_eq!(s.close(tiled), Outcome::Quit);
        // Shown, the top float takes the tiled pane's place.
        s.run_command("toggle-floats", &[], None, Instant::now()).unwrap();
        assert_eq!(s.close(tiled), Outcome::Redraw);
        assert_eq!((s.layout.as_ref().unwrap().panes(), s.floats.len(), s.focus), (vec![f], 0, f));
        assert_eq!(s.panes[&f].rect, s.area(), "the whole screen");
        // A tiled pane floats and comes back beside the focused tiled pane.
        key(&mut s, "<C-b>%");
        let right = s.focus;
        s.run_command("toggle-floating", &[], None, Instant::now()).unwrap();
        assert!(s.is_float(right));
        assert_eq!(s.layout.as_ref().unwrap().panes(), [f]);
        s.run_command("toggle-floating", &[], None, Instant::now()).unwrap();
        assert!(!s.is_float(right));
        assert_eq!(s.layout.as_ref().unwrap().panes(), [f, right]);
    }

    #[test]
    fn a_floating_terminal_closes_when_its_program_exits() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "tiled");
        let tiled = s.focus;
        float(&mut s, &["sh", "-c", "sleep 0.2"], &[]);
        let out = pump(&mut s, &rx, |s| s.floats.is_empty());
        assert_ne!(out, Outcome::Quit);
        assert_eq!((s.floats.len(), s.focus), (0, tiled));
        // The tiled pane to return to closes while a float has the focus.
        key(&mut s, "<C-b>%");
        let right = s.focus;
        float(&mut s, &["sleep", "30"], &[]);
        assert_eq!(s.close(right), Outcome::Redraw);
        s.run_command("toggle-floats", &[], None, Instant::now()).unwrap();
        assert_eq!(s.focus, tiled, "hiding the floats focuses a tiled pane that is there");
        s.run_command("toggle-floats", &[], None, Instant::now()).unwrap();
        assert_eq!(s.floats.len(), 1);
        assert_eq!(s.focus, s.floats[0].pane);
        // With no float, toggling floats opens a floating shell.
        let only = s.focus;
        s.close(only);
        s.run_command("toggle-floats", &[], None, Instant::now()).unwrap();
        assert_eq!(s.floats.len(), 1);
        assert_eq!(s.focus, s.floats[0].pane);
    }

    #[test]
    fn dragging_a_float_moves_and_resizes_it_and_its_program_on_release() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "tiled");
        let tiled = s.focus;
        let n = |v: f64| ScriptValue::Num(v);
        let f = float(
            &mut s,
            &["sleep", "30"],
            &[("width", n(20.0)), ("height", n(6.0)), ("col", n(10.0)), ("row", n(5.0))],
        );
        let frame = |s: &Session| s.floats.iter().find(|x| x.pane == f).unwrap().frame;
        let cols = |s: &Session| s.terms[&s.panes[&f].buffer()].term.geometry().cols;
        assert_eq!(frame(&s), Rect::new(10, 5, 22, 8));
        // Cells to pixels (10 x 20 cells), at the cell's centre.
        let at = |kind, col: i32, row: i32| MouseEvent {
            kind,
            x: col * 10 + 5,
            y: row * 20 + 10,
            unit: CoordUnit::Pixel,
            modifiers: Modifiers::NONE,
        };
        let now = Instant::now();
        let left = MouseButton::Left;
        // A click in the tiled pane focuses it; the float stays.
        s.mouse(&at(MouseEventKind::Press(left), 50, 20), now);
        s.mouse(&at(MouseEventKind::Release(left), 50, 20), now);
        assert_eq!(s.focus, tiled);
        // The pointer over the frame's parts.
        for (col, row, pointer) in [
            (15, 5, Pointer::Move),
            (10, 5, Pointer::ResizeNwse),
            (31, 12, Pointer::ResizeNwse),
            (31, 5, Pointer::ResizeNesw),
            (31, 8, Pointer::ResizeColumns),
            (15, 12, Pointer::ResizeRows),
            (15, 8, Pointer::Default),
        ] {
            s.mouse(&at(MouseEventKind::Move, col, row), now);
            assert_eq!(s.pointer(), pointer, "at ({col}, {row})");
        }
        // Moving by the title edge focuses the float.
        s.mouse(&at(MouseEventKind::Press(left), 15, 5), now);
        assert_eq!(s.focus, f);
        s.mouse(&at(MouseEventKind::Drag(left), 25, 2), now);
        assert_eq!(frame(&s), Rect::new(20, 2, 22, 8));
        s.mouse(&at(MouseEventKind::Release(left), 25, 2), now);
        // Resizing by the bottom right corner: the program gets the size on release.
        s.mouse(&at(MouseEventKind::Press(left), 41, 9), now);
        s.mouse(&at(MouseEventKind::Drag(left), 51, 11), now);
        assert_eq!(frame(&s), Rect::new(20, 2, 32, 10));
        assert_eq!((s.panes[&f].rect.w, cols(&s)), (30, 20), "the program keeps its size while dragging");
        assert_eq!(s.pointer(), Pointer::ResizeNwse);
        s.mouse(&at(MouseEventKind::Release(left), 51, 11), now);
        assert_eq!(cols(&s), 30);
        // Not below the smallest float.
        s.mouse(&at(MouseEventKind::Press(left), 20, 6), now);
        s.mouse(&at(MouseEventKind::Drag(left), 60, 6), now);
        s.mouse(&at(MouseEventKind::Release(left), 60, 6), now);
        assert_eq!((frame(&s).x, frame(&s).w), (40, 12), "the left edge stops at the smallest width");
    }

    #[test]
    fn the_shipped_keys_open_hide_and_embed_floats() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "tiled");
        let tiled = s.focus;
        key(&mut s, "<C-b>W");
        let f = s.focus;
        assert!(s.is_float(f), "W: a floating shell");
        assert_eq!(s.current_mode(), "terminal", "in its own mode");
        key(&mut s, "<C-b>w");
        assert_eq!((s.focus, s.floats_hidden), (tiled, true), "w: hidden");
        key(&mut s, "<C-b>w");
        assert_eq!((s.focus, s.floats_hidden), (f, false), "w: shown");
        key(&mut s, "<C-b>e");
        assert!(!s.is_float(f), "e: tiled");
        assert_eq!(s.current_mode(), "terminal", "back to the pane's mode");
        assert_eq!(s.layout.as_ref().unwrap().panes(), [tiled, f]);
        key(&mut s, "<C-b>e");
        assert!(s.is_float(f), "e: floating again");
    }

    #[test]
    fn no_image_lies_under_text_drawn_over_it() {
        // WezTerm draws images over text whatever their z: text over images
        // (messages, a float's title) needs the images cut out below it.
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "tiled");
        let pct = |p: &str| ScriptValue::Str(p.into());
        let f = float(
            &mut s,
            &["sleep", "30"],
            &[("width", pct("100%")), ("height", pct("100%")), ("title", pct("title"))],
        );
        assert_eq!(s.floats[0].frame, s.area());
        s.message("a message");
        let frame = s.frame();
        let notice = s.notice_rects()[0];
        let title = Rect::new(2, 0, 7, 1);
        let rows = screen_text(&frame, GEOMETRY);
        assert_eq!(&rows[0][2..9], " title ");
        for r in [notice, title] {
            let px = s.pixels(&r);
            let under: Vec<_> = frame.layers.iter().filter(|l| l.dst.intersect(&px).is_some()).collect();
            assert!(under.is_empty(), "images under the text at {r:?}: {under:?}");
        }
        // Around them the frame is drawn.
        assert!(frame.layers.iter().any(|l| l.dst.y == 0 && l.dst.x == 0), "the frame of float {f}");
    }

    #[test]
    fn recursive_mappings_stop_with_an_error() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo ready; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        let feed = |keys: &str| Target::Actions(vec![Act::new("feedkeys", vec![ScriptValue::Str(keys.into())])]);
        let keys = |k: &str| notation::parse(k).unwrap();
        s.modes.map("normal", keys("a"), feed("a"), false, None);
        s.modes.map("normal", keys("b"), feed("bb"), false, None);
        for k in ["a", "b"] {
            let start = Instant::now();
            key(&mut s, &format!("<C-b>{k}"));
            assert!(start.elapsed() < Duration::from_secs(5), "{k} ran too long");
            let shown = s.notices.iter().any(|n| n.text.contains("recursive mapping"));
            assert!(shown, "{k}: an error says why");
            assert!(s.matcher.pending().is_empty());
            key(&mut s, "<Esc>");
        }
        // Ordinary keys work again afterwards.
        key(&mut s, "<C-b>%");
        assert_eq!(s.pane_count(), 2);
    }

    #[test]
    fn a_terminal_closes_when_its_program_exits_although_jobs_hold_the_pty() {
        // The shell leaves a job behind that keeps the pty open. (Linux hangs
        // the pty up when its session leader exits, so its end of file came
        // anyway there; the exit is now taken from the process itself.)
        let (mut s, rx) = terminal_session(&["sh", "-c", "sleep 1000 & echo started"]);
        let (outcome, rows) = wait_text(&mut s, &rx, |_| false);
        assert_eq!(outcome, Outcome::Quit, "{rows:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn deleting_a_terminal_ends_a_program_that_ignores_hangups_without_waiting() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "trap '' HUP; echo ready; exec sleep 1000"]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        let buf = s.panes[&s.focus].buffer();
        let pid = s.terms[&buf].term.pid().unwrap();
        key(&mut s, "<C-b>%");
        let start = Instant::now();
        s.run_command("delete-buffer", &[ScriptValue::Num(buf as f64)], None, Instant::now()).unwrap();
        assert!(start.elapsed() < Duration::from_millis(100), "the session waited {:?}", start.elapsed());
        // Hung up, then killed a second later, and reaped (no zombie).
        let proc = std::path::PathBuf::from(format!("/proc/{pid}"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while proc.exists() {
            assert!(Instant::now() < deadline, "{pid} is still there");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_float_opened_while_the_configuration_loads_leaves_the_first_pane_tiled() {
        let (tx, _rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
        s.shell = Some(vec!["/bin/sh".into()]);
        float(&mut s, &["sleep", "30"], &[]);
        assert!(s.is_starting(), "no tiled pane yet");
        s.new_pane(&PaneSpec::Shell, None).unwrap();
        assert!(!s.is_starting());
        assert_eq!(s.layout.as_ref().map(|l| l.panes().len()), Some(1));
        assert_eq!(s.floats.len(), 1);
    }
}
