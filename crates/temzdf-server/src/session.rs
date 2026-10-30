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
use termpix_core::input::{
    CoordUnit, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
};
use termpix_core::scroll::ScrollConfig;

use crate::action::{Action, PaneSpec};
use crate::clip;
use crate::forward::{self, ForwardRequest};
use crate::keymap::{History, Keepalive, Layers, Matcher, Modes, Step, Stroke, Target, Unmapped, notation};
use crate::layout::{BorderId, Layout, PaneId, Split};
use temzdf_protocol::script::{FromHost, ScriptValue};

mod commands;
mod floats;
mod notices;
mod script;
mod statusline;
mod tabs;

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

/// When the tab line shows (the option `showtabline`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowTabline {
    Never,
    /// With two tab pages or more.
    Auto,
    Always,
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
    /// When the tab line shows (Vim's `showtabline`).
    pub showtabline: ShowTabline,
    /// Colours for clients and terminal panes (`None`: the client's own),
    /// and the first palette entries.
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    pub palette: Vec<[u8; 3]>,
    /// The GUI fonts, first choice first, and those tried first for wide
    /// characters.
    pub guifont: Vec<temzdf_scene::FontEntry>,
    pub guifontwide: Vec<temzdf_scene::FontEntry>,
}

impl Options {
    /// The colours terminal panes report (OSC 10, 11, 4): the options', else
    /// what GUI clients draw (`temzdf_scene::theme`), so the two agree.
    fn foreground_or_default(&self) -> Option<[u8; 3]> {
        Some(self.foreground.unwrap_or(temzdf_scene::theme::FOREGROUND))
    }

    fn background_or_default(&self) -> Option<[u8; 3]> {
        Some(self.background.unwrap_or(temzdf_scene::theme::BACKGROUND))
    }

    fn full_palette(&self) -> Vec<[u8; 3]> {
        temzdf_scene::theme::palette(&self.palette)
    }
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
            showtabline: ShowTabline::Auto,
            foreground: None,
            background: None,
            palette: Vec::new(),
            guifont: vec![temzdf_scene::FontEntry { family: "JetBrains Mono".into(), size: Some(14.0) }],
            guifontwide: Vec::new(),
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
#[derive(Clone, Debug)]
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
    /// The first pane of a new tab after the current one (made then).
    NewTab,
}

struct DocEntry {
    path: PathBuf,
    name: String,
    host: Option<DocumentHost>,
    opening: Option<Opening>,
    /// Panes to create once it opened, and the tabs they were asked for.
    waiting: Vec<(Spot, tabs::TabId)>,
    _forward: Option<forward::Server>,
    /// A forward search that came while it opened (its socket listens from
    /// the start: `temzdf --forward` sends one to the session it started).
    forward_waiting: Option<ForwardRequest>,
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
    /// In cells: the content and, at the bottom, the status line.
    rect: Rect,
    /// The status line shows (its bottom row); on by default.
    status_line: bool,
}

/// Where a pane's content goes in `rect`: all of it, or the rows above its
/// status line (none in a pane one row high: then only the status line shows).
fn content_rect(rect: Rect, status_line: bool) -> Rect {
    if status_line { Rect { h: rect.h.saturating_sub(1), ..rect } } else { rect }
}

/// The size of content `r`: a pane's content gets no size of 0 rows (it
/// keeps the size it had, `old`, and is not drawn).
fn content_geometry(r: Rect, cell: termpix_core::geom::CellSize, old: Option<Geometry>) -> Geometry {
    match old {
        Some(old) if r.h == 0 => old,
        _ => Geometry { cols: r.w as u16, rows: r.h.max(1) as u16, cell },
    }
}

impl PaneEntry {
    fn new(kind: Kind, rect: Rect) -> Self {
        Self { kind, rect, status_line: true }
    }

    /// Where its content goes (see [`content_rect`]).
    fn content(&self) -> Rect {
        content_rect(self.rect, self.status_line)
    }

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
    /// The tab pages and the current one's index (its state is in the
    /// fields above; see `tabs`).
    tabs: Vec<tabs::Tab>,
    tab: usize,
    next_tab: tabs::TabId,
    next_id: u32,
    modes: Modes<Target>,
    matcher: Matcher,
    history: History,
    /// The last key, or entering a mode: a submode's `timeout` counts from it.
    last_key: Instant,
    options: Options,
    /// `guifont` and `guifontwide` as configured, while `font-size` has
    /// changed them (what `font-size` with no size goes back to).
    configured_fonts: Option<(Vec<temzdf_scene::FontEntry>, Vec<temzdf_scene::FontEntry>)>,
    notices: Vec<notices::Notice>,
    /// The fixed escape keymap (script host down) saw its prefix.
    escape_prefix: bool,
    /// Mappings run since the matcher last used up its keys (Vim's
    /// `maxmapdepth`).
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
            tabs: vec![tabs::Tab::new(1)],
            tab: 0,
            next_tab: 2,
            next_id: 1,
            modes,
            matcher: Matcher::default(),
            history: History::new("normal", Options::default().modehistory),
            last_key: Instant::now(),
            options: Options::default(),
            configured_fonts: None,
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
                let spot = self.pin_spot(spot);
                self.docs.get_mut(&id).expect("found").waiting.push(spot);
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
        let spot = self.pin_spot(spot);
        self.docs.insert(
            id,
            DocEntry {
                path: canonical,
                name,
                host: None,
                opening: Some(opening),
                waiting: vec![spot],
                _forward: forward_server,
                forward_waiting: None,
                view: None,
            },
        );
        Ok(())
    }

    /// `spot` as asked for now, to wait for a document: a float goes to the
    /// current tab rather than the one current once the file opened, and a
    /// pane in the layout to this tab's layout.
    fn pin_spot(&self, spot: Spot) -> (Spot, tabs::TabId) {
        let spot = match spot {
            Spot::Float(spec) if spec.tab.is_none() => {
                Spot::Float(floats::FloatSpec { tab: Some(tabs::FloatTab::Only(self.tab_id())), ..spec })
            }
            spot => spot,
        };
        (spot, self.tab_id())
    }

    /// A pane that waited for document `doc` to open: in the tab it was
    /// asked for (see `pin_spot`), without taking the focus from this one.
    fn add_waiting_pane(&mut self, doc: DocId, spot: Spot, tab: tabs::TabId) {
        if !matches!(spot, Spot::Split { .. }) || tab == self.tab_id() {
            self.add_pdf_pane(doc, spot);
            return;
        }
        // Its tab closed meanwhile: so did the pane.
        let Some(i) = self.tab_index(tab) else { return };
        let here = self.tab;
        self.swap_tab(i);
        if let Some(p) = self.add_pdf_pane_focused(doc, spot, false) {
            // The pane that tab shows focused when entered.
            (self.focus, self.tiled_focus) = (p, p);
        }
        self.swap_tab(here);
        self.relayout();
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Where panes go: the screen, below the tab line when it shows.
    fn area(&self) -> Rect {
        let top = u32::from(self.tabline_shown());
        Rect::new(0, top as i32, self.geometry.cols as u32, (self.geometry.rows as u32).saturating_sub(top))
    }

    /// Put pane `id` into the layout at `spot`. Returns its rectangle.
    fn place(&mut self, id: PaneId, spot: Spot) -> Result<Rect> {
        self.dragging = None;
        self.float_drag = None;
        if let Spot::Float(spec) = spot {
            return Ok(self.place_float(id, spec));
        }
        if let Spot::NewTab = spot {
            self.open_tab();
        }
        let area = self.area();
        // A float has no place in the layout to split: the tiled pane
        // focused last stands in (rather than "no room").
        let tiled = |t: PaneId| if self.is_float(t) { self.tiled_focus } else { t };
        let (target, split, before) = match spot {
            // (A pane of another tab is no target here.)
            Spot::Split { target: Some(t), split, before } if self.tab_of(t) == Some(self.tab) => {
                (Some(tiled(t)), split, before)
            }
            Spot::Split { target: None, split, before } => (None, split, before),
            // The target went away meanwhile: split the focused pane instead.
            _ => (Some(tiled(self.focus)), Split::Horizontal, false),
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
            // A new tab that got no pane goes again, back to the tab it
            // was opened from (`open_tab` put it right after that one).
            if self.tabs.len() > 1 {
                let new = self.tab;
                self.enter_tab(new.saturating_sub(1));
                self.remove_tab(new);
            }
        }
    }

    /// Create a pane on an open document (or show it in a pane).
    fn add_pdf_pane(&mut self, doc: DocId, spot: Spot) -> Option<PaneId> {
        let focus = self.takes_focus(&spot);
        self.add_pdf_pane_focused(doc, spot, focus)
    }

    /// `add_pdf_pane`, the new pane focused if `focus`.
    fn add_pdf_pane_focused(&mut self, doc: DocId, spot: Spot, focus: bool) -> Option<PaneId> {
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
        self.panes.insert(id, PaneEntry::new(kind, self.area()));
        self.event("PaneOpened", vec![ScriptValue::Num(id as f64), ScriptValue::Str("pdf".into())]);
        if focus {
            self.set_focus(id);
        }
        self.relayout();
        if restore {
            self.restore_view(id);
        }
        Some(id)
    }

    /// A page view of `doc` for pane `id`.
    fn pdf_kind(&mut self, id: PaneId, doc: DocId) -> Option<Kind> {
        // The size its pane gives it (a status line shows by default); the
        // layout places it right after.
        let geometry = content_geometry(content_rect(self.area(), true), self.geometry.cell, None);
        let entry = self.docs.get_mut(&doc)?;
        let host = entry.host.as_mut()?;
        let mut pane = PdfPane::with_document(entry.name.clone(), host.info.clone(), host.doc.clone(), geometry);
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
        let g = content_geometry(e.content(), self.geometry.cell, None);
        let kind = if let Some(t) = self.terms.get_mut(&buf) {
            t.term.scroll_to_bottom();
            Kind::Term { buf, view: TermView::new(g) }
        } else if let Some(d) = self.docs.get_mut(&buf) {
            if d.host.is_none() {
                d.waiting.push((Spot::Replace(pane), self.tabs[self.tab].id));
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
    /// panes showing it (the last pane: the session ends). `PaneClosePre`
    /// hooks do not run (the buffer goes either way); `PaneClosed` does.
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
        let focus = self.takes_focus(&spot);
        let rect = self.place(id, spot)?;
        let buf = self.alloc_id();
        let events = self.events.clone();
        let sink: temzdf_term::Sink = Arc::new(move |e| events(SessionEvent::Term(buf, e)));
        let mut spawn = spawn.clone();
        spawn.argv = spawn.argv.or_else(|| self.shell.clone());
        spawn.env.push(("TEMZDF_SESSION_NAME".into(), self.name.clone()));
        // The size its pane will give it (status lines show by default).
        let geometry = content_geometry(content_rect(rect, true), self.geometry.cell, None);
        let term = match TermBuffer::spawn(&spawn, geometry, sink) {
            Ok(t) => t,
            Err(e) => {
                self.unplace(id);
                bail!("cannot start {}: {e}", spawn.argv.as_ref().map_or("the shell".into(), |a| a.join(" ")));
            }
        };
        let mut term = Box::new(term);
        term.set_colors(
            self.options.foreground_or_default(),
            self.options.background_or_default(),
            &self.options.full_palette(),
        );
        self.terms.insert(buf, TermEntry { term, exited: false });
        self.event("BufferCreated", vec![ScriptValue::Num(buf as f64), ScriptValue::Str("terminal".into())]);
        self.terminal_pane(id, buf, rect, focus);
        Ok(id)
    }

    /// Pane `id`, placed at `rect`, shows terminal `buf`.
    fn terminal_pane(&mut self, id: PaneId, buf: BufferId, rect: Rect, focus: bool) {
        let geometry = content_geometry(content_rect(rect, true), self.geometry.cell, None);
        self.panes.insert(id, PaneEntry::new(Kind::Term { buf, view: TermView::new(geometry) }, rect));
        self.event("PaneOpened", vec![ScriptValue::Num(id as f64), ScriptValue::Str("terminal".into())]);
        if focus {
            self.set_focus(id);
        }
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
        let area = self.area();
        let rects: Vec<(PaneId, Rect)> = self.layouts().flat_map(|(_, l)| l.rects(area)).collect();
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
            let content = e.content();
            match &mut e.kind {
                Kind::Pdf { pane, .. } => {
                    let g = content_geometry(content, cell, Some(pane.geometry()));
                    if g != pane.geometry() {
                        pane.resize(g);
                    }
                }
                Kind::Term { view, .. } => view.geometry = content_geometry(content, cell, Some(view.geometry)),
            }
        }
    }

    /// Show or hide pane `id`'s status line (`None`: the other way round);
    /// its content gets the row, or gives it back.
    fn set_status_line(&mut self, id: PaneId, show: Option<bool>) -> Result<(), String> {
        let e = self.panes.get_mut(&id).ok_or_else(|| format!("no pane {id}"))?;
        e.status_line = show.unwrap_or(!e.status_line);
        let rect = e.rect;
        self.set_rect(id, rect);
        self.fit_terms();
        Ok(())
    }

    /// A terminal is as large as the largest pane showing it in any tab (as
    /// in Neovim); a hidden one (or one in hidden floats only) keeps its size.
    fn fit_terms(&mut self) {
        let mut sizes: HashMap<BufferId, (u16, u16)> = HashMap::new();
        for (id, e) in &self.panes {
            if !self.shown_anywhere(*id) {
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

    /// The panes showing `doc`, the focused one first (it receives SyncTeX
    /// results), then those on screen (`here`).
    fn panes_of<'a>(
        panes: &'a mut HashMap<PaneId, PaneEntry>,
        doc: DocId,
        focus: PaneId,
        here: &[PaneId],
    ) -> Vec<&'a mut PdfPane> {
        let mut v: Vec<(PaneId, &mut PdfPane)> = panes
            .iter_mut()
            .filter_map(|(id, e)| match &mut e.kind {
                Kind::Pdf { pane, doc: d } if *d == doc => Some((*id, &mut **pane)),
                _ => None,
            })
            .collect();
        v.sort_by_key(|(id, _)| (*id != focus, !here.contains(id), *id));
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
                        let forward = entry.forward_waiting.take();
                        for (spot, tab) in std::mem::take(&mut entry.waiting) {
                            self.add_waiting_pane(id, spot, tab);
                        }
                        if let Some(req) = forward {
                            let _ = self.handle_inner(SessionEvent::Forward(id, req), now);
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
                        // The first pane failed but floats opened meanwhile
                        // (the configuration): the top one is tiled, or the
                        // session would wait for a first pane forever.
                        let opening = self.docs.values().any(|d| !d.waiting.is_empty());
                        if self.layout.is_none()
                            && !opening
                            && let Some(top) = self.floats.last().map(|f| f.pane)
                        {
                            self.floats.retain(|f| f.pane != top);
                            self.layout = Some(Layout::new(top));
                            self.tiled_focus = top;
                            self.set_focus(top);
                            self.relayout();
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
                let here = self.here_panes();
                let Some(host) = self.docs.get_mut(&id).and_then(|d| d.host.as_mut()) else { return Outcome::None };
                let mut panes = Self::panes_of(&mut self.panes, id, self.focus, &here);
                let (changed, effect) = host.handle(ev, &mut panes, &mut self.images, now);
                self.effects.extend(effect);
                Outcome::None.or(changed)
            }
            SessionEvent::Forward(id, req) => {
                let here = self.here_panes();
                let Some(entry) = self.docs.get_mut(&id) else { return Outcome::None };
                let Some(host) = entry.host.as_mut() else {
                    // Still opening: for when it has opened.
                    entry.forward_waiting = Some(req);
                    return Outcome::None;
                };
                let panes = Self::panes_of(&mut self.panes, id, self.focus, &here);
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
        self.last_key = now;
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
            let layers = self.layers();
            match self.matcher.step(&self.modes, &layers, self.options.timeoutlen, now) {
                Step::Wait(_) => {
                    // The keys and what they expanded to are used up: the
                    // next keys (typed or fed by a hook) count from zero,
                    // as Vim counts per typeahead expansion.
                    if !self.matcher.has_input() && self.matcher.pending().is_empty() {
                        self.map_depth = 0;
                    }
                    break;
                }
                Step::NeedConditions(funcs) => {
                    let ctx = self.key_context(None);
                    self.eval(funcs, vec![ctx]);
                }
                Step::Leave => {
                    let old = self.history.current().to_owned();
                    self.history.pop();
                    self.mode_changed(&old);
                    out = out.merge(Outcome::Redraw);
                }
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
                    if self.unmapped() == Unmapped::Passthrough {
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
                let ctx = self.key_context(Some(count));
                self.call(func, vec![ctx], false);
                Outcome::None
            }
        }
    }

    /// What mapped functions and conditions get: the focused pane, its
    /// kind, whether it floats, the mode, the keys pending, and (mapped
    /// functions) the count.
    fn key_context(&self, count: Option<Option<usize>>) -> ScriptValue {
        let kind = match self.panes.get(&self.focus).map(|e| &e.kind) {
            Some(Kind::Pdf { .. }) => ScriptValue::Str("pdf".into()),
            Some(Kind::Term { .. }) => ScriptValue::Str("terminal".into()),
            None => ScriptValue::Null,
        };
        let keys: String = self.matcher.pending().iter().map(|k| k.notation()).collect();
        let mut fields = vec![
            ("pane", ScriptValue::Num(self.focus as f64)),
            ("paneKind", kind),
            ("floating", ScriptValue::Bool(self.is_float(self.focus))),
            ("mode", ScriptValue::Str(self.history.current().to_owned())),
            ("keys", ScriptValue::Str(keys)),
        ];
        if let Some(count) = count {
            fields.push(("count", count.map_or(ScriptValue::Null, |c| ScriptValue::Num(c as f64))));
        }
        ScriptValue::map(fields)
    }

    /// Where keys are looked up now: the current mode; below a transient
    /// mode with `keepalive: "always"` the modes under it too, down to the
    /// first that is not transient.
    fn layers(&self) -> Layers {
        let mut modes = Vec::new();
        let mut keep = None;
        for (i, name) in self.history.modes().enumerate() {
            let def = self.modes.get(name);
            modes.push(name.to_owned());
            match def {
                Some(d) if d.transient => match d.keepalive {
                    Keepalive::Always => continue,
                    k if i == 0 => {
                        keep = Some(k);
                        break;
                    }
                    _ => break,
                },
                _ => break,
            }
        }
        let count = self.mode_property(|d| d.count).unwrap_or(false);
        Layers { modes, count, keep }
    }

    /// A property of the current mode, or of the modes below it where it
    /// is not given.
    fn mode_property<P>(&self, get: impl Fn(&crate::keymap::ModeDef<Target>) -> Option<P>) -> Option<P> {
        self.history.modes().find_map(|m| self.modes.get(m).and_then(&get))
    }

    /// What unmapped keys do now.
    fn unmapped(&self) -> Unmapped {
        self.mode_property(|d| d.unmapped).unwrap_or(Unmapped::Drop)
    }

    /// The current mode, or the first below it that is not transient.
    fn base_mode(&self) -> &str {
        self.history.modes().find(|m| !self.modes.get(m).is_some_and(|d| d.transient)).unwrap_or("normal")
    }

    /// When the transient mode on top leaves for lack of keys.
    fn submode_deadline(&self) -> Option<Instant> {
        let d = self.modes.get(self.history.current())?;
        Some(self.last_key + d.timeout.filter(|_| d.transient)?)
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
        let o = &self.options;
        for t in self.terms.values_mut() {
            t.term.set_colors(o.foreground_or_default(), o.background_or_default(), &o.full_palette());
        }
    }

    /// How GUI clients should look (see [`temzdf_scene::Appearance`]).
    pub fn appearance(&self) -> temzdf_scene::Appearance {
        let o = &self.options;
        temzdf_scene::Appearance {
            foreground: o.foreground,
            background: o.background,
            palette: o.palette.clone(),
            font: o.guifont.clone(),
            font_wide: o.guifontwide.clone(),
        }
    }

    /// The current mode's name.
    pub fn current_mode(&self) -> &str {
        self.history.current()
    }

    /// Text an input method committed: typed into the focused terminal in a
    /// pass-through mode (`terminal`), else each character as a key, so
    /// mappings stay reachable with an input method on (as Vim takes IME
    /// input in Normal mode).
    pub fn text(&mut self, text: &str, now: Instant) -> Outcome {
        if self.hold_text(text) {
            return Outcome::None;
        }
        let out = if self.unmapped() == Unmapped::Passthrough
            && let Some(t) = self.focused_term()
        {
            Outcome::None.or(t.send_text(text))
        } else {
            let mut out = Outcome::None;
            for c in text.chars() {
                let key = KeyEvent { text: Some(c.to_string()), ..KeyEvent::new(KeyCode::Char(c), Modifiers::NONE) };
                out = out.merge(self.key(&key, now));
            }
            out
        };
        out.merge(self.flush_events())
    }

    /// The client's window (or terminal) gained or lost the focus: the
    /// focused terminal pane tells its program, if it asked (Vim reloads
    /// files on focus).
    pub fn client_focus(&mut self, gained: bool) -> Outcome {
        if let Some(t) = self.focused_term() {
            t.focus(gained);
        }
        Outcome::None
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
        Outcome::None
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
    /// last focused tiled pane. A tab's last tiled pane leaves its place to
    /// the topmost shown float of that tab (not one on all tabs: it would
    /// leave the others); with none, the tab closes, and with the last tab
    /// the session ends.
    fn close(&mut self, id: PaneId) -> Outcome {
        if let Some(i) = self.tab_of(id).filter(|i| *i != self.tab) {
            return self.close_in_tab(i, id);
        }
        let next = if self.is_float(id) {
            self.floats.retain(|f| f.pane != id);
            self.shown_floats().last().map_or(self.tiled_focus, |f| f.pane)
        } else {
            let own = tabs::FloatTab::Only(self.tab_id());
            let top = self.shown_floats().into_iter().rev().find(|f| f.tab == own).map(|f| f.pane);
            // No pane tiled here (should not be): this tab page goes, the
            // session only with the last one.
            let Some(layout) = self.layout.as_mut() else { return self.remove_tab(self.tab) };
            if layout.panes().len() <= 1 {
                let Some(top) = top else { return self.remove_tab(self.tab) };
                layout.replace(id, top);
                self.floats.retain(|f| f.pane != top);
                top
            } else {
                let next = layout.next(id);
                layout.remove(id);
                next
            }
        };
        self.discard_pane(id);
        if self.focus == id {
            self.set_focus(next);
        }
        if self.tiled_focus == id
            && let Some(first) = self.layout.as_ref().and_then(|l| l.panes().first().copied())
        {
            self.tiled_focus = first;
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
        let (col, row) = ((px / cell.width as f32).floor() as i32, (py / cell.height as f32).floor() as i32);
        if let Some(tabs) = self.tabline_mouse(m, col, row) {
            return out.merge(tabs);
        }
        // Floats' frames lie over the borders (unless one is being dragged).
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
        let select = self.base_mode() == "normal";
        let Some(e) = self.panes.get_mut(&id) else { return out };
        let content = e.content();
        // A pane's status line is not its content: the pointer there goes to
        // the pane only to end what began in it (a drag, a selection).
        if self.captured.is_none() && row >= content.y + content.h as i32 {
            return out;
        }
        let (dx, dy) = match m.unit {
            CoordUnit::Pixel => ((content.x * cell.width as i32), (content.y * cell.height as i32)),
            CoordUnit::Cell => (content.x, content.y),
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
            let content = e.content();
            let (ox, oy) = (content.x * cell.width as i32, content.y * cell.height as i32);
            let clip = Rect::new(ox, oy, content.w * cell.width, content.h * cell.height);
            // A pane one row high shows its status line only.
            let frame = if content.h == 0 { Frame { title: frame.title, ..Frame::default() } } else { frame };
            for l in frame.layers {
                let moved = Layer { dst: Rect::new(l.dst.x + ox, l.dst.y + oy, l.dst.w, l.dst.h), ..l };
                if let Some(c) = moved.clipped(&clip) {
                    self.keys.place(&mut layers, id, l.key, c, &covers_px);
                }
            }
            for g in frame.text {
                let g = TextGrid { col: g.col + content.x as u16, row: g.row + content.y as u16, ..g };
                text.extend(clip::text_pieces(g, &covers));
            }
            if id == self.focus {
                cursor = frame
                    .cursor
                    .map(|(c, r)| (c + content.x as u16, r + content.y as u16))
                    .filter(|&(c, r)| !covers.iter().any(|k| k.contains(c as i32, r as i32)));
                cursor_shape = frame.cursor_shape;
                title = frame.title;
            }
        }
        self.complete = complete;
        // Status lines, under floats over them.
        for id in &order {
            let Some(e) = self.panes.get(id).filter(|e| e.status_line && e.rect.h > 0) else { continue };
            let bar = match &e.kind {
                Kind::Pdf { pane, .. } => pane.status(),
                Kind::Term { .. } => termpix_ui::StatusBar::default(),
            };
            let grid = statusline::grid(&bar, e.rect, *id == self.focus);
            text.extend(clip::text_pieces(grid, &self.covers(Some(*id))));
        }
        self.border_layers(&mut layers);
        self.float_frames(&mut layers, &mut text);
        text.extend(self.tabline().map(|(grid, _)| grid));
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
        let area = self.area();
        let accent = termpix_ui::Theme::dark().accent.0;
        let mut lines = Vec::new();
        let mut gaps = Vec::new();
        for (id, e) in tiled.iter().filter_map(|id| Some((*id, self.panes.get(id)?))) {
            let r = e.rect;
            let (x0, y0) = (r.x * cell.width as i32, r.y * cell.height as i32);
            let (w, h) = (r.w * cell.width, r.h * cell.height);
            let focus = id == self.focus;
            let (vx, vy) = (cell.width.saturating_sub(BORDER_PX) / 2, cell.height.saturating_sub(BORDER_PX) / 2);
            // The gap cells right of and below the pane (including the corner
            // where gaps cross).
            let (cw, ch) = (cell.width, cell.height);
            let right = r.right() < area.right();
            let below = r.bottom() < area.bottom();
            if right {
                gaps.push(Rect::new(x0 + w as i32, y0, cw, h + if below { ch } else { 0 }));
            }
            if below {
                gaps.push(Rect::new(x0, y0 + h as i32, w, ch));
            }
            if right {
                lines.push((focus, Rect::new(x0 + w as i32 + vx as i32, y0, BORDER_PX, h)));
            }
            if r.x > area.x {
                lines.push((focus, Rect::new(x0 - cell.width as i32 + vx as i32, y0, BORDER_PX, h)));
            }
            if below {
                lines.push((focus, Rect::new(x0, y0 + h as i32 + vy as i32, w, BORDER_PX)));
            }
            if r.y > area.y {
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

    /// Nothing is being opened, loaded, rendered or searched, and no script
    /// runs (keys wait for it).
    pub fn is_idle(&self) -> bool {
        !self.script.busy()
            && self.docs.values().all(|d| d.host.as_ref().is_some_and(|h| h.doc.borrow().cache.is_idle()))
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
        let here = self.here_panes();
        for (&id, d) in self.docs.iter_mut() {
            if let Some(host) = d.host.as_mut() {
                let mut panes = Self::panes_of(&mut self.panes, id, self.focus, &here);
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
        // A submode left alone for its timeout goes.
        if self.submode_deadline().is_some_and(|d| now >= d) && !self.script.busy() {
            let old = self.history.current().to_owned();
            self.history.pop();
            self.last_key = now;
            self.mode_changed(&old);
            out = out.merge(Outcome::Redraw);
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
            .chain(self.submode_deadline())
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
            ("=", acts(&[("equalize-panes", &[])])),
            ("<Bar>", acts(&[("maximize-pane", &[s("width")])])),
            ("_", acts(&[("maximize-pane", &[s("height")])])),
            ("H", acts(&[("move-pane", &[s("left")])])),
            ("J", acts(&[("move-pane", &[s("bottom")])])),
            ("K", acts(&[("move-pane", &[s("top")])])),
            ("L", acts(&[("move-pane", &[s("right")])])),
            ("r", acts(&[("rotate-panes", &[])])),
            ("R", acts(&[("rotate-panes", &[ScriptValue::Bool(true)])])),
            ("x", acts(&[("close-pane", &[])])),
            ("gt", acts(&[("next-tab", &[])])),
            ("gT", acts(&[("prev-tab", &[])])),
            ("T", acts(&[("break-pane", &[])])),
            ("d", acts(&[("detach", &[]), ("exit-mode", &[])])),
            ("q", acts(&[("kill-session", &[])])),
            ("<C-b>", acts(&[("exit-mode", &[]), ("feedkeys", &[s("<C-b>"), ScriptValue::Bool(false)])])),
            ("<Esc>", acts(&[("exit-mode", &[])])),
        ] {
            m.map("normal", keys(lhs), crate::keymap::Mapping::new(target));
        }
        for mode in ["terminal", "pdf"] {
            m.map(mode, keys("<C-b>"), crate::keymap::Mapping::new(acts(&[("switch-mode", &[s("normal")])])));
        }
        for lhs in ["<C-S-v>", "<D-v>"] {
            m.map("terminal", keys(lhs), crate::keymap::Mapping::new(acts(&[("paste", &[])])));
        }
        for (key, cmd) in Keymap::default().bindings() {
            let stroke = Stroke::of(&key.event());
            if stroke != keys("<C-c>")[0] {
                m.map(
                    "pdf",
                    vec![stroke],
                    crate::keymap::Mapping::new(Target::Actions(vec![Act::new(cmd.name(), vec![])])),
                );
            }
        }
    }
    use std::sync::mpsc::{self, Receiver};
    use temzdf_pdfcore::doc_cache::TileKey;
    use temzdf_pdfcore::pdf;
    use termpix_core::geom::CellSize;
    use termpix_core::input::Modifiers;

    const GEOMETRY: Geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };

    #[test]
    fn font_size_changes_guifont_and_goes_back_to_the_configured_one() {
        let sink: EventSink = Arc::new(|_| true);
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
        // Each list's sizes, `None` where a font names none.
        let sizes = |s: &Session| {
            let a = s.appearance();
            let of = |l: &[temzdf_scene::FontEntry]| l.iter().map(|f| f.size).collect::<Vec<_>>();
            (of(&a.font), of(&a.font_wide))
        };
        let run = |s: &mut Session, arg: Option<&str>, count| {
            let args: Vec<ScriptValue> = arg.map(|a| ScriptValue::Str(a.into())).into_iter().collect();
            s.run_command("font-size", &args, count, Instant::now()).map(|_| ())
        };
        let fonts = |list: &[(&str, Option<f64>)]| {
            ScriptValue::List(
                list.iter()
                    .map(|(f, size)| {
                        let mut m = vec![("font".to_owned(), ScriptValue::Str((*f).into()))];
                        m.extend(size.map(|n| ("size".to_owned(), ScriptValue::Num(n))));
                        ScriptValue::Map(m)
                    })
                    .collect(),
            )
        };
        s.set_option("guifont", &fonts(&[("Mono", Some(11.0)), ("Cjk", Some(12.0)), ("Emoji", None)])).unwrap();
        s.set_option("guifontwide", &fonts(&[("Wide", None)])).unwrap();
        assert_eq!(
            s.get_option("guifont").unwrap(),
            fonts(&[("Mono", Some(11.0)), ("Cjk", Some(12.0)), ("Emoji", None)])
        );
        run(&mut s, Some("+1"), None).unwrap();
        assert_eq!(
            sizes(&s),
            (vec![Some(12.0), Some(13.0), None], vec![None]),
            "each named size; the rest follow the first"
        );
        run(&mut s, Some("-2"), Some(3)).unwrap();
        assert_eq!(sizes(&s).0, [Some(6.0), Some(7.0), None], "times the count");
        run(&mut s, Some("-100"), None).unwrap();
        assert_eq!(sizes(&s).0, [Some(4.0), Some(5.0), None], "kept readable, by what the first could take");
        s.run_command("font-size", &[ScriptValue::Num(8.0)], None, Instant::now()).unwrap();
        assert_eq!(sizes(&s).0, [Some(8.0), Some(10.0), None], "a size scales them");
        assert!(run(&mut s, Some("big"), None).is_err());
        run(&mut s, None, None).unwrap();
        assert_eq!(sizes(&s), (vec![Some(11.0), Some(12.0), None], vec![None]), "back to the configuration");
        s.run_command("font-size", &[ScriptValue::Num(22.0)], None, Instant::now()).unwrap();
        assert_eq!(sizes(&s).0, [Some(22.0), Some(24.0), None], "in proportion");
        // A first font without a size starts from 14; setting the option
        // again makes that the configured font.
        s.set_option("guifont", &fonts(&[("Plain", None)])).unwrap();
        run(&mut s, Some("+2"), None).unwrap();
        assert_eq!(sizes(&s).0, [Some(16.0)], "from the default of 14");
        s.set_option("guifont", &fonts(&[("Other", Some(8.0))])).unwrap();
        run(&mut s, None, None).unwrap();
        assert_eq!(sizes(&s).0, [Some(8.0)]);
        // What is not a font list.
        for bad in [ScriptValue::Str("Mono:h11".into()), fonts(&[]), ScriptValue::List(vec![ScriptValue::Num(1.0)])] {
            assert!(s.set_option("guifont", &bad).is_err(), "{bad:?}");
        }
        let sized = |v: ScriptValue| {
            ScriptValue::List(vec![ScriptValue::Map(vec![
                ("font".into(), ScriptValue::Str("X".into())),
                ("size".into(), v),
            ])])
        };
        assert!(s.set_option("guifont", &sized(ScriptValue::Num(-1.0))).is_err());
        assert!(s.set_option("guifont", &sized(ScriptValue::Str("12".into()))).is_err());
        assert!(s.set_option("guifontwide", &fonts(&[])).is_ok(), "no wide fonts");
    }

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

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn search_for_searches_the_focused_pdf() {
        let (mut s, rx) = session();
        // As `temzdf action search-for ...`: words.
        s.run_cli(&["search-for".into(), "Back to Section".into()]).unwrap();
        settle(&mut s, &rx);
        // The jump to the first hit (on page 2) scrolls: let it end.
        s.tick(Instant::now() + Duration::from_secs(5));
        let frame = s.frame();
        assert!(frame.title.contains("2 / 3"), "{}", frame.title);
        let e = s.run_cli(&["search-for".into()]).unwrap_err();
        assert!(e.to_string().contains("what to search for"), "{e}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn search_for_needs_a_pdf() {
        let (mut s, _rx) = terminal_session(&["sh", "-c", "sleep 5"]);
        let e = s.run_cli(&["search-for".into(), "x".into()]).unwrap_err();
        assert!(e.to_string().contains("not a PDF"), "{e}");
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
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_float_for_another_tab_page_does_not_take_the_focus_here() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        let one = s.focus;
        new_tab(&mut s, &["sleep", "30"]);
        cmd(&mut s, "goto-tab", &["1"], None).unwrap();
        let f = float(&mut s, &["sleep", "30"], &[("tabpage", ScriptValue::Num(2.0))]);
        assert_eq!(s.focus, one, "keys must not go to a pane nobody sees");
        assert!(s.here_panes().contains(&s.focus));
        assert!(s.shown_floats().is_empty());
        cmd(&mut s, "goto-tab", &["2"], None).unwrap();
        assert!(s.shown_floats().iter().any(|x| x.pane == f), "it shows on its tab page");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_tab_opened_while_the_configuration_loads_leaves_no_tab_page_empty() {
        let (tx, _rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
        s.shell = Some(vec!["/bin/sh".into()]);
        // config.ts: cmd.newTab({ command: [...] }) at the top level.
        new_tab(&mut s, &["sleep", "30"]);
        // Then the session's first pane, once the configuration loaded.
        s.script.first = Some((PaneSpec::Shell, None));
        let _ = s.open_first();
        assert_eq!(s.layouts().count(), s.tabs.len(), "every tab page has a pane");
        assert_eq!(s.tab, 1, "the configuration's tab page stays current");
        for n in 1..=s.tabs.len() {
            cmd(&mut s, "goto-tab", &[&n.to_string()], None).unwrap();
            assert!(!s.is_starting(), "tab page {n} shows something");
            assert!(s.panes.contains_key(&s.focus), "tab page {n} has a focus");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn a_forward_search_sent_while_the_document_opens_is_kept() {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("test".into(), GEOMETRY, Hints::default(), settings, sink);
        s.open(&pdf::fixture("sample.pdf")).unwrap();
        // Before it has opened (line 44 is on the last page).
        let doc = *s.docs.keys().next().unwrap();
        let req = ForwardRequest { file: "sample.tex".into(), line: 44, column: 0 };
        s.handle(SessionEvent::Forward(doc, req), Instant::now());
        while s.is_starting() {
            let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            s.handle(ev, Instant::now());
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while s.focused_pane().unwrap().viewer.current_page() != 2 {
            assert!(Instant::now() < deadline, "the search was lost");
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
                s.handle(ev, Instant::now());
            }
            s.tick(Instant::now());
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn transient_modes_stay_leave_and_time_out() {
        use crate::keymap::Mapping;
        let (mut s, rx) = session();
        settle(&mut s, &rx);
        let keys = |x: &str| notation::parse(x).unwrap();
        let act = |n: &str, a: &[&str]| {
            Mapping::new(Target::Actions(vec![Act::new(n, a.iter().map(|x| ScriptValue::Str((*x).into())).collect())]))
        };
        // doc/keymaps.md's example: + repeats in a submode of normal.
        let d = s.modes.define("resize");
        d.transient = true;
        d.timeout = Some(Duration::from_millis(1500));
        s.modes.map("resize", keys("+"), act("resize-pane", &["taller"]));
        s.modes.map("normal", keys("s"), act("switch-mode", &["resize"]));
        s.modes.define("win").transient = true;
        s.modes.define("win").keepalive = Keepalive::Always;
        s.modes.map("normal", keys("w"), act("switch-mode", &["win"]));
        key(&mut s, "<C-b>\"");
        let (top, bottom) = {
            let p = s.layout.as_ref().unwrap().panes();
            (p[0], p[1])
        };
        assert_eq!(s.focus(), bottom);
        let height = |s: &Session| s.panes[&bottom].rect.h;
        let before = height(&s);
        key(&mut s, "<C-b>s++");
        assert_eq!((height(&s), s.current_mode()), (before + 2, "resize"), "its keys repeat");
        // keepalive "mapped": another key leaves, and normal handles it.
        key(&mut s, "k");
        assert_eq!(s.focus(), top, "k moved the focus up, as in normal");
        assert_eq!(s.current_mode(), "pdf", "the focus change left the modes");
        // A timeout leaves too.
        key(&mut s, "<C-b>s");
        assert_eq!(s.current_mode(), "resize");
        assert!(s.next_deadline().is_some_and(|d| d <= Instant::now() + Duration::from_millis(1500)));
        s.poll(Instant::now() + Duration::from_millis(1600));
        assert_eq!(s.current_mode(), "normal", "back to the mode below");
        // keepalive "always": the mode below takes what it does not map,
        // and it stays.
        key(&mut s, "w");
        key(&mut s, "=");
        assert_eq!(s.current_mode(), "win");
        // A focus change leaves it, as the normal mode below.
        key(&mut s, "j");
        assert_eq!((s.focus(), s.current_mode()), (bottom, "pdf"));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn equal_and_maximized_panes_by_the_default_keys() {
        let (mut s, rx) = session();
        settle(&mut s, &rx);
        for _ in 0..2 {
            key(&mut s, "<C-b>");
            key(&mut s, "%");
        }
        let widths =
            |s: &Session| s.layout.as_ref().unwrap().panes().iter().map(|p| s.panes[p].rect.w).collect::<Vec<_>>();
        assert_eq!(widths(&s), [44, 22, 22], "each split halved the focused pane");
        key(&mut s, "<C-b>");
        assert_eq!(key(&mut s, "="), Outcome::Redraw);
        assert_eq!(widths(&s), [29, 30, 29], "90 cells and two borders");
        assert_eq!(s.current_mode(), "normal", "the keys repeat, as resizing does");
        assert_eq!(key(&mut s, "<Bar>"), Outcome::Redraw);
        assert_eq!(widths(&s), [10, 10, 68], "the focused, last pane takes all it can");
        key(&mut s, "30<Bar>");
        assert_eq!(widths(&s)[2], 30, "a count is the width");
        assert_eq!(key(&mut s, "_"), Outcome::None, "nothing stacked with it");
        assert!(s.run_command("maximize-pane", &[], None, Instant::now()).is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
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
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
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
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
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
    #[cfg_attr(miri, ignore = "Miri has no inotify")]
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
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
        // (Its status line takes a row.)
        assert_eq!((frame.text[0].rows.len(), frame.text[0].rows[0].len()), (33, 100));
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_terminal_in_two_panes_is_as_large_as_the_larger() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; echo ready; while read l; do stty size; done"]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        let first = s.panes[&s.focus].buffer();
        key(&mut s, "<CR>");
        // (Rows less the status line.)
        wait_text(&mut s, &rx, |r| r.iter().any(|l| l == "29 90"));
        key(&mut s, "<C-b>%");
        let right = s.focus;
        s.show_in(right, first).unwrap();
        let widest = s.panes.values().map(|e| e.rect.w).max().unwrap();
        assert!(widest < 90);
        key(&mut s, "<CR>");
        let want = format!("29 {widest}");
        let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains(&want)));
        assert!(rows.iter().any(|l| l.contains(&want)), "{rows:?}");
        // One pane left: the whole width again.
        s.close(right);
        key(&mut s, "<CR>");
        let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().filter(|l| *l == "29 90").count() == 2);
        assert_eq!(rows.iter().filter(|l| *l == "29 90").count(), 2, "{rows:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_float_opened_while_the_first_document_fails_leaves_a_session_that_shows() {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
        // The configuration opens a float; then the first pane's PDF fails.
        let f = float(&mut s, &["sleep", "30"], &[]);
        let bad = std::env::temp_dir().join(format!("temzdf-bad-{}.pdf", std::process::id()));
        std::fs::write(&bad, "not a PDF").unwrap();
        s.open(&bad).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !s.docs.is_empty() {
            assert!(Instant::now() < deadline, "the document never failed");
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
                s.handle(ev, Instant::now());
            }
        }
        std::fs::remove_file(&bad).unwrap();
        assert!(!s.is_starting(), "frames are made again");
        assert!(s.layout.as_ref().is_some_and(|l| l.panes() == [f]), "the float is the tiled pane");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
        // Its terminal is as large as its content, less the status line.
        let buf = s.panes[&pane].buffer();
        assert_eq!((s.terms[&buf].term.geometry().cols, s.terms[&buf].term.geometry().rows), (20, 3));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn recursive_mappings_stop_with_an_error() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo ready; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        let feed = |keys: &str| Target::Actions(vec![Act::new("feedkeys", vec![ScriptValue::Str(keys.into())])]);
        let keys = |k: &str| notation::parse(k).unwrap();
        s.modes.map("normal", keys("a"), crate::keymap::Mapping::new(feed("a")));
        s.modes.map("normal", keys("b"), crate::keymap::Mapping::new(feed("bb")));
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn keys_fed_without_typing_do_not_add_up_to_a_recursive_mapping() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo ready; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        let act = Target::Actions(vec![Act::new("current-mode", vec![])]);
        s.modes.map("terminal", notation::parse("<F5>").unwrap(), crate::keymap::Mapping::new(act));
        // As a hook or a timer feeds keys, for hours, with nothing typed.
        for _ in 0..=MAX_MAP_DEPTH {
            let fed = ScriptValue::Str("<F5>".into());
            s.run_command("feedkeys", &[fed], None, Instant::now()).unwrap();
            let _ = s.drive(Instant::now());
        }
        assert!(!s.notices.iter().any(|n| n.text.contains("recursive mapping")));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
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

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn floats_on_tiny_screens_and_cells_do_not_overflow() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "tiled");
        let big = ScriptValue::Num(f64::from(u32::MAX));
        float(&mut s, &["sleep", "30"], &[("width", big.clone()), ("height", big)]);
        for g in [
            Geometry { cols: 3, rows: 2, cell: CellSize { width: 1, height: 1 } },
            Geometry { cols: 1, rows: 1, cell: CellSize { width: 10, height: 20 } },
        ] {
            s.resize(g);
            s.frame();
        }
        s.run_command("resize-pane", &[ScriptValue::map([("width", ScriptValue::Num(-5.0))])], None, Instant::now())
            .unwrap();
        s.frame();
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_float_waiting_for_its_file_keeps_its_title_and_focus() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "tiled");
        let tiled = s.focus;
        let what = ScriptValue::map([(
            "file",
            ScriptValue::Str(temzdf_pdfcore::pdf::fixture("sample.pdf").display().to_string()),
        )]);
        let opts = ScriptValue::map([("title", ScriptValue::Str("paper".into())), ("focus", ScriptValue::Bool(false))]);
        let (pane, _) = s.run_command("open-float", &[what, opts], None, Instant::now()).unwrap();
        assert_eq!(pane, ScriptValue::Null, "opening: no pane yet");
        pump(&mut s, &rx, |s| !s.floats.is_empty());
        assert_eq!(s.floats[0].title.as_deref(), Some("paper"));
        assert_eq!(s.focus, tiled, "focus: false");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn panes_waiting_for_a_document_go_to_the_tab_they_were_asked_for() {
        let sample = temzdf_pdfcore::pdf::fixture("sample.pdf");
        let opened = |s: &Session| s.docs.values().all(|d| d.host.is_some());
        // A split, then another tab entered while the file opens.
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "tiled");
        s.open(&sample).unwrap();
        let other = new_tab(&mut s, &["sleep", "30"]);
        pump(&mut s, &rx, opened);
        assert_eq!((s.tab, s.focus), (1, other), "the current tab stays");
        let first: Vec<_> = s.layouts().filter(|(i, _)| *i == 0).map(|(_, l)| l.panes().len()).collect();
        assert_eq!(first, [2], "the split went into the first tab");
        assert_eq!(s.layout.as_ref().unwrap().panes(), [other]);
        // A float, and a second one on the document still opening.
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo tiled; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "tiled");
        let file = ScriptValue::map([("file", ScriptValue::Str(sample.display().to_string()))]);
        s.run_command("open-float", &[file], None, Instant::now()).unwrap();
        let doc = *s.docs.keys().next().unwrap();
        let buffer = ScriptValue::map([("buffer", ScriptValue::Num(doc as f64))]);
        s.run_command("open-float", &[buffer], None, Instant::now()).unwrap();
        let first_tab = s.tab_id();
        let other = new_tab(&mut s, &["sleep", "30"]);
        pump(&mut s, &rx, |s| opened(s) && s.floats.len() == 2);
        assert_eq!(s.floats.len(), 2, "the buffer's float opened too");
        assert!(s.floats.iter().all(|f| f.tab == tabs::FloatTab::Only(first_tab)), "on the first tab");
        assert_eq!((s.tab, s.focus), (1, other));
    }

    #[test]
    fn tab_numbers_out_of_range_do_not_wrap_around() {
        let sink: EventSink = Arc::new(|_| true);
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("t".into(), GEOMETRY, Hints::default(), settings, sink);
        s.open_tab();
        let huge = (1u64 << 32) + 1;
        assert!(s.float_tab(&ScriptValue::Num(huge as f64)).is_err());
        assert!(s.float_tab(&ScriptValue::Str(huge.to_string())).is_err());
        assert!(s.run_command("next-tab", &[], Some(huge as usize), Instant::now()).is_err());
        assert_eq!(s.tab, 1);
    }

    /// `new-tab` with a command; returns its pane.
    fn new_tab(s: &mut Session, argv: &[&str]) -> PaneId {
        let what = ScriptValue::map([(
            "command",
            ScriptValue::List(argv.iter().map(|a| ScriptValue::Str((*a).into())).collect()),
        )]);
        let (pane, _) = s.run_command("new-tab", &[what], None, Instant::now()).unwrap();
        pane.as_num().expect("a pane") as PaneId
    }

    fn cmd(s: &mut Session, name: &str, args: &[&str], count: Option<usize>) -> Result<Outcome, String> {
        let args: Vec<ScriptValue> = args.iter().map(|a| ScriptValue::Str((*a).into())).collect();
        s.run_command(name, &args, count, Instant::now()).map(|(_, o)| o)
    }

    /// The tab line's labels, in order (`tabs()`).
    fn labels(s: &mut Session) -> Vec<String> {
        let (list, _) = s.run_command("tabs", &[], None, Instant::now()).unwrap();
        let ScriptValue::List(list) = list else { panic!() };
        list.iter().map(|t| t.get("label").and_then(ScriptValue::as_str).unwrap_or_default().to_owned()).collect()
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn tabs_open_switch_move_and_close() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        let one = s.focus;
        let two = new_tab(&mut s, &["sh", "-c", "echo two; sleep 30"]);
        assert_eq!((s.tabs.len(), s.tab, s.focus), (2, 1, two));
        let (_, rows) = wait_text(&mut s, &rx, |r| r[0] == "two");
        assert!(!rows.iter().any(|r| r.contains("one")), "only the current tab shows");
        // After the last, the first; the focus is the tab's own.
        cmd(&mut s, "next-tab", &[], None).unwrap();
        assert_eq!((s.tab, s.focus), (0, one));
        wait_text(&mut s, &rx, |r| r[0] == "one");
        // A count goes to that tab (Vim's {N}gt), back as many (gT).
        cmd(&mut s, "next-tab", &[], Some(2)).unwrap();
        assert_eq!((s.tab, s.focus), (1, two));
        cmd(&mut s, "prev-tab", &[], Some(3)).unwrap();
        assert_eq!(s.tab, 0);
        assert!(cmd(&mut s, "goto-tab", &["3"], None).unwrap_err().contains("no tab page 3"));
        // A new tab goes after the current one.
        let three = new_tab(&mut s, &["sh", "-c", "echo three; sleep 30"]);
        assert_eq!(s.tab, 1);
        for (i, name) in ["a", "c", "b"].iter().enumerate() {
            cmd(&mut s, "goto-tab", &[&(i + 1).to_string()], None).unwrap();
            cmd(&mut s, "rename-tab", &[name], None).unwrap();
        }
        assert_eq!(labels(&mut s), ["a", "c", "b"]);
        // :tabmove: after tab N (0: first), last, or by places.
        cmd(&mut s, "goto-tab", &["2"], None).unwrap();
        cmd(&mut s, "move-tab", &[], None).unwrap();
        assert_eq!((labels(&mut s), s.tab), (vec!["a".into(), "b".into(), "c".into()], 2));
        cmd(&mut s, "move-tab", &["0"], None).unwrap();
        assert_eq!((labels(&mut s), s.tab), (vec!["c".into(), "a".into(), "b".into()], 0));
        cmd(&mut s, "move-tab", &["+1"], None).unwrap();
        assert_eq!(labels(&mut s), ["a", "c", "b"]);
        // After itself (Vim: no effect), then after the last.
        cmd(&mut s, "move-tab", &["2"], None).unwrap();
        assert_eq!(labels(&mut s), ["a", "c", "b"]);
        cmd(&mut s, "move-tab", &["3"], None).unwrap();
        assert_eq!(labels(&mut s), ["a", "b", "c"]);
        // An empty name shows the focused buffer again.
        cmd(&mut s, "rename-tab", &[], None).unwrap();
        assert_eq!(labels(&mut s)[2], "shell");
        // Closing the current tab goes to the one after it, else before; the
        // buffers stay.
        assert_eq!(s.focus, three);
        let buffers = s.buffer_count();
        assert_eq!(cmd(&mut s, "close-tab", &[], None), Ok(Outcome::Redraw));
        assert_eq!((s.tabs.len(), s.tab, s.focus, s.buffer_count()), (2, 1, two, buffers));
        assert!(!s.panes.contains_key(&three));
        cmd(&mut s, "close-tab", &["1"], None).unwrap();
        assert_eq!((s.tabs.len(), s.tab, s.focus), (1, 0, two));
        assert!(cmd(&mut s, "close-tab", &[], None).unwrap_err().contains("last tab page"));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_tabs_last_pane_gives_way_to_its_own_float_else_the_tab_closes() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        let one = s.focus;
        let two = new_tab(&mut s, &["sleep", "30"]);
        let own = float(&mut s, &["sleep", "30"], &[]);
        let all = float(&mut s, &["sleep", "30"], &[("tabpage", ScriptValue::Num(-1.0))]);
        // The tab's own float takes the place, not the one on all tabs.
        assert_eq!(s.close(two), Outcome::Redraw);
        assert_eq!(s.layout.as_ref().unwrap().panes(), [own]);
        assert!(s.is_float(all));
        // Without one, the tab closes; the float on all tabs stays.
        assert_eq!(s.close(own), Outcome::Redraw);
        assert_eq!((s.tabs.len(), s.tab), (1, 0));
        assert!(s.shown_floats().iter().any(|f| f.pane == all));
        // The last tab's last pane: the session ends (the float does not
        // keep it).
        assert_eq!(s.close(one), Outcome::Quit);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn panes_of_other_tabs_close_there() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        let one = s.focus;
        // A tab whose only program ends while another tab is current goes.
        new_tab(&mut s, &["sh", "-c", "sleep 0.3"]);
        cmd(&mut s, "goto-tab", &["1"], None).unwrap();
        let out = pump(&mut s, &rx, |s| s.tabs.len() == 1);
        assert_ne!(out, Outcome::Quit);
        assert_eq!((s.tabs.len(), s.tab, s.focus), (1, 0, one));
        // One of two panes there: the tab stays, its focus moves on.
        let a = new_tab(&mut s, &["sleep", "30"]);
        s.action(Action::Split { split: Split::Horizontal, before: false, root: false }, Instant::now()).unwrap();
        let b = s.focus;
        assert_ne!(a, b);
        let buf = s.panes[&b].buffer();
        cmd(&mut s, "goto-tab", &["1"], None).unwrap();
        s.delete_buffer(buf).unwrap();
        assert_eq!((s.tabs.len(), s.tab, s.focus), (2, 0, one));
        cmd(&mut s, "goto-tab", &["2"], None).unwrap();
        assert_eq!((s.layout.as_ref().unwrap().panes(), s.focus), (vec![a], a));
        assert_eq!(s.panes[&a].rect, s.area(), "the whole screen again");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn toggle_floating_keeps_to_the_current_tab() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        let left = s.focus;
        key(&mut s, "<C-b>%");
        new_tab(&mut s, &["sleep", "30"]);
        key(&mut s, "<C-b>%");
        let e = cmd(&mut s, "toggle-floating", &[&left.to_string()], None).unwrap_err();
        assert!(e.contains("in this tab"), "{e}");
        let places = s.layouts().filter(|(_, l)| l.panes().contains(&left)).count()
            + s.floats.iter().filter(|f| f.pane == left).count();
        assert_eq!(places, 1, "a pane is in one place");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_tab_that_cannot_start_goes_back_where_it_was() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        let one = s.focus;
        new_tab(&mut s, &["sleep", "30"]);
        cmd(&mut s, "goto-tab", &["1"], None).unwrap();
        let what = ScriptValue::map([("command", ScriptValue::List(vec![ScriptValue::Str("/nonexistent/x".into())]))]);
        assert!(s.run_command("new-tab", &[what], None, Instant::now()).is_err());
        assert_eq!((s.tabs.len(), s.tab, s.focus), (2, 0, one));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn break_pane_moves_a_pane_to_a_new_tab() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        let left = s.focus;
        assert!(cmd(&mut s, "break-pane", &[], None).unwrap_err().contains("only tiled pane"));
        key(&mut s, "<C-b>%");
        let right = s.focus;
        cmd(&mut s, "break-pane", &[], None).unwrap();
        assert_eq!((s.tabs.len(), s.tab, s.focus), (2, 1, right));
        assert_eq!(s.layout.as_ref().unwrap().panes(), [right]);
        assert_eq!(s.panes[&right].rect, s.area());
        cmd(&mut s, "prev-tab", &[], None).unwrap();
        assert_eq!((s.layout.as_ref().unwrap().panes(), s.focus), (vec![left], left));
        assert_eq!(s.panes[&left].rect, s.area());
        // A float becomes the new tab's tiled pane.
        let f = float(&mut s, &["sleep", "30"], &[]);
        cmd(&mut s, "break-pane", &[], None).unwrap();
        assert!(!s.is_float(f));
        assert_eq!((s.tab, s.layout.as_ref().unwrap().panes()), (1, vec![f]));
        assert_eq!(s.tabs.len(), 3);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_terminal_in_two_tabs_is_as_large_as_the_larger() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        let one = s.focus;
        let buf = s.panes[&one].buffer();
        key(&mut s, "<C-b>%");
        let half = s.panes[&one].rect.w as u16;
        assert_eq!(s.terms[&buf].term.geometry().cols, half);
        let what = ScriptValue::map([("buffer", ScriptValue::Num(buf as f64))]);
        s.run_command("new-tab", &[what], None, Instant::now()).unwrap();
        assert_eq!(s.terms[&buf].term.geometry().cols, GEOMETRY.cols, "the whole screen in the new tab");
        cmd(&mut s, "goto-tab", &["1"], None).unwrap();
        assert_eq!(s.terms[&buf].term.geometry().cols, GEOMETRY.cols, "the other tab still counts");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn only_the_current_tabs_images_are_drawn() {
        let (mut s, rx) = session();
        settle(&mut s, &rx);
        assert!(!s.frame().layers.is_empty());
        new_tab(&mut s, &["sleep", "30"]);
        assert!(s.frame().layers.is_empty(), "the PDF is in the other tab");
        cmd(&mut s, "prev-tab", &[], None).unwrap();
        assert!(!s.frame().layers.is_empty());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn the_tab_line_takes_the_top_row_and_its_clicks_switch_and_close() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        let one = s.focus;
        let buf = s.panes[&one].buffer();
        // (Less the pane's status line.)
        assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 1, "one tab: no tab line");
        new_tab(&mut s, &["sh", "-c", "echo two; sleep 30"]);
        let (_, rows) = wait_text(&mut s, &rx, |r| r[1] == "two");
        assert_eq!(rows[0], " 1 shell  2 shell");
        assert_eq!(s.area(), Rect::new(0, 1, GEOMETRY.cols as u32, GEOMETRY.rows as u32 - 1));
        assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 2, "the other tab's terminal too");
        // A click on a tab goes there; the middle button closes it.
        let cell = GEOMETRY.cell;
        click(&mut s, 2 * cell.width as i32, cell.height as i32 / 2);
        assert_eq!((s.tab, s.focus), (0, one));
        wait_text(&mut s, &rx, |r| r[1] == "one");
        let middle = |kind| MouseEvent {
            kind,
            x: 12 * cell.width as i32,
            y: 0,
            unit: CoordUnit::Pixel,
            modifiers: Modifiers::NONE,
        };
        s.mouse(&middle(MouseEventKind::Press(MouseButton::Middle)), Instant::now());
        s.mouse(&middle(MouseEventKind::Release(MouseButton::Middle)), Instant::now());
        assert_eq!(s.tabs.len(), 1);
        // One tab: the line goes, the panes get the row back.
        assert_eq!(s.area().y, 0);
        assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 1);
        // showtabline: always, never.
        s.set_option("showtabline", &ScriptValue::Str("always".into())).unwrap();
        assert_eq!((s.area().y, screen_text(&s.frame(), GEOMETRY)[0].as_str()), (1, " 1 shell"));
        assert_eq!(s.get_option("showtabline"), Ok(ScriptValue::Str("always".into())));
        s.set_option("showtabline", &ScriptValue::Str("never".into())).unwrap();
        new_tab(&mut s, &["sleep", "30"]);
        assert_eq!(s.area().y, 0);
        assert!(s.set_option("showtabline", &ScriptValue::Str("sometimes".into())).is_err());
        assert!(s.set_option("showtabline", &ScriptValue::Num(2.0)).is_err());
    }

    /// The colour of a theme colour as a cell's.
    fn theme_rgb(c: termpix_ui::Color) -> temzdf_scene::Color {
        temzdf_scene::Color::Rgb([c.0[0], c.0[1], c.0[2]])
    }

    /// The composed cell at (`col`, `row`).
    fn cell_at(frame: &Frame, col: u16, row: u16) -> Option<temzdf_scene::Cell> {
        frame.text.iter().rev().find_map(|g| {
            let r = g.rows.get(row.checked_sub(g.row)? as usize)?;
            r.get(col.checked_sub(g.col)? as usize).cloned()
        })
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn pdf_panes_say_their_file_page_zoom_and_messages_in_their_status_line() {
        let (mut s, rx) = session();
        let (frame, _) = settle(&mut s, &rx);
        let last = GEOMETRY.rows - 1;
        let rows = screen_text(&frame, GEOMETRY);
        assert!(rows[last as usize].starts_with(" sample.pdf"), "{rows:?}");
        assert!(rows[last as usize].contains("1 / 3"), "{rows:?}");
        assert!(rows[last as usize].ends_with('%'), "the zoom: {rows:?}");
        let theme = termpix_ui::Theme::dark();
        let name = cell_at(&frame, 1, last).unwrap();
        assert_eq!((name.fg, name.bg), (theme_rgb(theme.bar_fg), theme_rgb(theme.bar_bg)), "focused");
        // No page lies on the status line.
        let row_px = s.pixels(&Rect::new(0, last as i32, GEOMETRY.cols as u32, 1));
        assert!(frame.layers.iter().all(|l| l.dst.intersect(&row_px).is_none()), "an image on the status line");
        // Messages take the left side.
        s.run_cli(&["search-for".into(), "zzzzqqq".into()]).unwrap();
        let (frame, _) = settle(&mut s, &rx);
        let rows = screen_text(&frame, GEOMETRY);
        assert!(rows[last as usize].starts_with(" Pattern not found: zzzzqqq"), "{rows:?}");
        assert_eq!(cell_at(&frame, 1, last).unwrap().fg, theme_rgb(theme.error));
        // Another pane: the unfocused one's name is dimmed.
        key(&mut s, "<C-b>%");
        let (frame, _) = settle(&mut s, &rx);
        let left = s.panes.iter().find(|(id, _)| **id != s.focus).map(|(_, e)| e.rect).unwrap();
        let right = s.panes[&s.focus].rect;
        let at = |r: Rect| ((r.x + 1) as u16, (r.y + r.h as i32 - 1) as u16);
        let dim = cell_at(&frame, at(left).0, at(left).1).unwrap();
        let lit = cell_at(&frame, at(right).0, at(right).1).unwrap();
        assert_eq!((dim.fg, lit.fg), (theme_rgb(theme.bar_dim), theme_rgb(theme.bar_fg)), "{left:?} {right:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn hiding_a_status_line_gives_its_row_to_the_pane() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; echo ready; while read l; do stty size; done"]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        let pane = s.focus;
        let buf = s.panes[&pane].buffer();
        // A terminal's status line is blank, in the bar's colour.
        let theme = termpix_ui::Theme::dark();
        let last = GEOMETRY.rows - 1;
        let bar = cell_at(&s.frame(), 0, last).unwrap();
        assert_eq!((bar.text.as_str(), bar.bg), (" ", theme_rgb(theme.bar_bg)));
        assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 1);
        cmd(&mut s, "status-line", &["off"], None).unwrap();
        assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows, "the row back");
        assert!(cell_at(&s.frame(), 0, last).is_none_or(|c| c.bg != theme_rgb(theme.bar_bg)), "no status line");
        key(&mut s, "<CR>");
        wait_text(&mut s, &rx, |r| r.iter().any(|l| l == "30 90"));
        // No argument: the other way round; a pane by its id.
        let id = ScriptValue::Num(pane as f64);
        s.run_command("status-line", &[ScriptValue::Null, id], None, Instant::now()).unwrap();
        assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 1);
        assert_eq!(cell_at(&s.frame(), 0, last).unwrap().bg, theme_rgb(theme.bar_bg));
        assert!(cmd(&mut s, "status-line", &["maybe"], None).is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_pane_one_row_high_shows_only_its_status_line() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; echo ready; while read l; do printf SAID; done"]);
        wait_text(&mut s, &rx, |r| r[0] == "ready");
        let pane = s.focus;
        let buf = s.panes[&pane].buffer();
        let one = Geometry { rows: 1, ..GEOMETRY };
        s.resize(one);
        // Its program keeps the size it had (never 0 rows).
        assert_eq!(s.terms[&buf].term.geometry().rows, GEOMETRY.rows - 1);
        key(&mut s, "<CR>");
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(50)) {
                s.handle(ev, Instant::now());
            }
        }
        let frame = s.frame();
        assert_eq!(screen_text(&frame, one), [""], "only the (blank) status line");
        assert_eq!(cell_at(&frame, 0, 0).unwrap().bg, theme_rgb(termpix_ui::Theme::dark().bar_bg));
        assert_eq!(frame.cursor, None, "no cursor in hidden content");
        // Hidden, the content gets its row back.
        cmd(&mut s, "status-line", &["off"], None).unwrap();
        assert_eq!(s.terms[&buf].term.geometry().rows, 1);
        // (No newline after it: one row would scroll it away.)
        let (_, rows) = wait_text(&mut s, &rx, |r| r[0] == "SAID");
        assert_eq!(rows[0], "SAID");
    }

    /// The panes() entry of `pane`.
    fn pane_info(s: &mut Session, pane: PaneId) -> ScriptValue {
        let (list, _) = s.run_command("panes", &[], None, Instant::now()).unwrap();
        let ScriptValue::List(list) = list else { panic!() };
        list.into_iter().find(|p| p.get("id").and_then(ScriptValue::as_num) == Some(pane as f64)).unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn floats_show_on_their_tab_or_on_all_and_hide_per_tab() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        let own = float(&mut s, &["sleep", "30"], &[]);
        let all = float(&mut s, &["sleep", "30"], &[("tabpage", ScriptValue::Num(-1.0))]);
        let shown = |s: &Session| s.shown_floats().iter().map(|f| f.pane).collect::<Vec<_>>();
        assert_eq!(shown(&s), [own, all]);
        new_tab(&mut s, &["sleep", "30"]);
        assert_eq!(shown(&s), [all], "only the float on all tabs");
        // The stack is one: raising here raises it there.
        let second = float(&mut s, &["sleep", "30"], &[]);
        assert_eq!(shown(&s), [all, second]);
        s.set_focus(all);
        assert_eq!(shown(&s), [second, all]);
        cmd(&mut s, "goto-tab", &["1"], None).unwrap();
        assert_eq!(shown(&s), [own, all]);
        // Hiding is per tab; the float on all tabs follows the current one.
        cmd(&mut s, "toggle-floats", &[], None).unwrap();
        assert!(shown(&s).is_empty());
        cmd(&mut s, "goto-tab", &["2"], None).unwrap();
        assert_eq!(shown(&s), [second, all]);
        // panes() says where each pane is.
        assert_eq!(pane_info(&mut s, own).get("tab"), Some(&ScriptValue::Num(1.0)));
        assert_eq!(pane_info(&mut s, second).get("tab"), Some(&ScriptValue::Num(2.0)));
        assert_eq!(pane_info(&mut s, all).get("tab"), Some(&ScriptValue::Null));
        // tabpage moves a float to another tab.
        let opts = ScriptValue::map([("tabpage", ScriptValue::Num(1.0))]);
        s.run_command("float-config", &[ScriptValue::Num(second as f64), opts], None, Instant::now()).unwrap();
        assert_eq!(shown(&s), [all]);
        assert_ne!(s.focus, second, "the focus stays on this tab");
        // A float on all tabs becomes a tiled pane of the current tab.
        s.set_focus(all);
        cmd(&mut s, "toggle-floating", &[], None).unwrap();
        assert!(!s.is_float(all));
        assert_eq!(s.tab_of(all), Some(1));
        cmd(&mut s, "goto-tab", &["1"], None).unwrap();
        assert!(!s.layout.as_ref().unwrap().panes().contains(&all));
        assert!(
            s.run_command(
                "open-float",
                &[ScriptValue::Null, ScriptValue::map([("tabpage", ScriptValue::Num(5.0))])],
                None,
                Instant::now()
            )
            .is_err()
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn the_shipped_keys_switch_and_break_out_tabs() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo one; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "one");
        key(&mut s, "<C-b>%");
        let right = s.focus;
        key(&mut s, "<C-b>T");
        assert_eq!((s.tabs.len(), s.tab, s.focus, s.current_mode()), (2, 1, right, "terminal"));
        new_tab(&mut s, &["sleep", "30"]);
        key(&mut s, "<C-b>gt");
        assert_eq!((s.tab, s.current_mode()), (0, "terminal"));
        key(&mut s, "<C-b>gT");
        assert_eq!(s.tab, 2);
        key(&mut s, "<C-b>2gt");
        assert_eq!((s.tab, s.focus), (1, right));
        key(&mut s, "<C-b>2gT");
        assert_eq!(s.tab, 2);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn committed_text_is_typed_in_terminals_and_keys_elsewhere() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "stty -echo; read l; echo \"got:$l\"; sleep 30"]);
        // In the terminal mode: typed as it is (not keys, not a paste).
        s.text("日本語", Instant::now());
        key(&mut s, "<CR>");
        // (Wide characters leave a blank spacer cell in the screen's text.)
        let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains("got:")));
        assert!(rows.iter().any(|l| l.replace(' ', "").contains("got:日本語")), "{rows:?}");
        // In the normal mode: keys, so mappings work with an input method on.
        key(&mut s, "<C-b>");
        s.text("%", Instant::now());
        assert_eq!(s.layout.as_ref().unwrap().panes().len(), 2, "split by the % mapping");
        // paste asks the client for its clipboard.
        s.take_effects();
        s.run_command("paste", &[], None, Instant::now()).unwrap();
        s.run_command("paste", &[ScriptValue::Str("primary".into())], None, Instant::now()).unwrap();
        assert_eq!(
            s.take_effects(),
            [temzdf_scene::Effect::Paste { primary: false }, temzdf_scene::Effect::Paste { primary: true }]
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn the_clients_focus_reaches_programs_that_asked() {
        let script = "printf '\\033[?1004h'; stty raw -echo; printf ready; v=$(dd bs=1 count=6 2>/dev/null | od -An -c | tr -s ' '); printf 'got:%s' \"$v\"; sleep 30";
        let (mut s, rx) = terminal_session(&["sh", "-c", script]);
        wait_text(&mut s, &rx, |r| r[0].contains("ready"));
        s.client_focus(false);
        s.client_focus(true);
        let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains("got:")));
        assert!(rows.iter().any(|l| l.contains("got: 033 [ O 033 [ I")), "{rows:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn unthemed_terminals_report_the_gui_defaults() {
        // One query at a time: under load two answers may come in two reads.
        let script = "stty raw -echo min 0 time 20; printf ready; \
            printf '\\033]4;1;?\\033\\\\'; a=$(dd bs=128 count=1 2>/dev/null | tr -d '\\033'); \
            printf '\\033]11;?\\033\\\\'; b=$(dd bs=128 count=1 2>/dev/null | tr -d '\\033'); \
            printf 'got:%s%s' \"$a\" \"$b\"; sleep 30";
        let (mut s, rx) = terminal_session(&["sh", "-c", script]);
        let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains("got:")));
        let all = rows.join("");
        // xterm's red and the GUI's default background (temzdf_scene::theme).
        assert!(all.contains("rgb:cdcd/0000/0000"), "{rows:?}");
        assert!(all.contains("rgb:1e1e/1e1e/1e1e"), "{rows:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn the_theme_answers_programs_colour_queries() {
        let script = "stty raw -echo min 0 time 20; printf ready; printf '\\033]11;?\\033\\\\'; v=$(dd bs=64 count=1 2>/dev/null | tr -d '\\033'); printf 'got:%s' \"$v\"; sleep 30";
        let (mut s, rx) = terminal_session(&["sh", "-c", "sleep 30"]);
        s.set_option("background", &ScriptValue::Str("#102030".into())).unwrap();
        s.set_option("palette", &ScriptValue::List(vec![ScriptValue::Str("#ff0000".into())])).unwrap();
        // A terminal opened after the theme was set gets it too.
        s.run_command(
            "new-pane",
            &[
                ScriptValue::Str("--".into()),
                ScriptValue::Str("sh".into()),
                ScriptValue::Str("-c".into()),
                ScriptValue::Str(script.into()),
            ],
            None,
            Instant::now(),
        )
        .unwrap();
        let (_, rows) = wait_text(&mut s, &rx, |r| r.iter().any(|l| l.contains("got:")));
        assert!(rows.iter().any(|l| l.contains("rgb:1010/2020/3030")), "{rows:?}");
        // Options read back; bad values are refused.
        assert_eq!(s.appearance().background, Some([0x10, 0x20, 0x30]));
        assert_eq!(s.appearance().palette, [[0xff, 0, 0]]);
        let jetbrains = temzdf_scene::FontEntry { family: "JetBrains Mono".into(), size: Some(14.0) };
        assert_eq!(s.appearance().font, [jetbrains], "the default");
        assert!(s.set_option("foreground", &ScriptValue::Str("#12345".into())).is_err());
        assert!(s.set_option("palette", &ScriptValue::List(vec![ScriptValue::Null])).is_err());
        let cjk = ScriptValue::Map(vec![("font".into(), ScriptValue::Str("Noto Sans CJK JP".into()))]);
        s.set_option("guifontwide", &ScriptValue::List(vec![cjk])).unwrap();
        assert_eq!(
            s.appearance().font_wide,
            [temzdf_scene::FontEntry { family: "Noto Sans CJK JP".into(), size: None }]
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn ctrl_shift_v_asks_the_client_to_paste() {
        let (mut s, _rx) = terminal_session(&["sleep", "30"]);
        s.take_effects();
        // As the GUI reports it: the base letter with Ctrl and Shift.
        let k = KeyEvent::new(KeyCode::Char('v'), Modifiers::CTRL | Modifiers::SHIFT);
        s.key(&k, Instant::now());
        assert_eq!(s.take_effects(), [temzdf_scene::Effect::Paste { primary: false }]);
    }
}
