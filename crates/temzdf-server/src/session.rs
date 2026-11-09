//! A session: panes (PDF views and terminals), their layout and focus, the
//! prefix key, and the composed frame every attached client shows. No sockets
//! or terminals here: the standalone viewer, `--render-frame` and the session
//! server all drive a `Session` (see doc/design/session-server.md).

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
use temzdf_scene::{CursorShape, Effect, Frame, Hints, Images, Layer, Pointer, Rect, Region, RegionKind, TextGrid};
use temzdf_term::{Spawn, TermBuffer, TermEvent, TermView};
use termpix_core::geom::Geometry;
use termpix_core::input::{
    CoordUnit, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
};
use termpix_core::scroll::ScrollConfig;

use crate::action::{Action, PaneSpec};
use crate::clip;
use crate::forward::{self, ForwardRequest};
use crate::keymap::{Fallback, History, Keepalive, Layers, Matcher, Modes, Step, Stroke, Target, notation};
use crate::layout::{BorderId, Layout, PaneId, Split};
use temzdf_protocol::script::{FromHost, ScriptValue};

mod borders;
mod commands;
mod floats;
mod images;
mod markdown;
mod notices;
mod registers;
mod script;
mod statusline;
mod swap;
mod tabs;
mod text;
#[cfg(test)]
mod vim_tests;

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
    /// The script host of this generation ended.
    ScriptExited(u32),
    /// A text buffer's file changed (or may have: a watcher's event).
    TextFile(BufferId),
    /// A Markdown viewer's file changed (or may have).
    Markdown(u32),
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
    /// Keep text buffers' undo trees in files (Vim's undofile): written
    /// with the file, read back when it is opened.
    pub undofile: bool,
    /// Keep text buffers' text in swap files while they are edited.
    pub swapfile: bool,
    /// How long after the last change a swap file is written.
    pub updatetime: Duration,
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
    /// The window's status line shows (its bottom row; Vim's `laststatus`
    /// 3, Neovim's global status line).
    pub showstatusline: bool,
    /// Colours for clients and terminal panes (`None`: the client's own),
    /// and the first palette entries.
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    pub palette: Vec<[u8; 3]>,
    /// The GUI fonts, first choice first, and those tried first for wide
    /// characters.
    pub guifont: Vec<temzdf_scene::FontEntry>,
    pub guifontwide: Vec<temzdf_scene::FontEntry>,
    /// Text panes: wrapping, tabs, line numbers, the cursor line,
    /// `scrolloff`.
    pub text: TextOptions,
    /// The mode at the bottom of the history in a pane of each kind (by
    /// the kind's name; its own mode when left out). Set only while the
    /// configuration loads.
    pub basemode: std::collections::BTreeMap<String, String>,
    /// How many entries the copy history keeps.
    pub copyhistory: usize,
    /// What goes to the client's clipboard (doc/design/registers.md).
    pub clipboard: registers::ClipboardSync,
}

#[derive(Clone, Debug, Default)]
pub struct TextOptions {
    pub layout: temzdf_texteditor::LayoutOptions,
    pub number: temzdf_texteditor::NumberMode,
    pub cursorline: bool,
    pub scrolloff: usize,
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
            swapfile: true,
            updatetime: Duration::from_millis(4000),
            undofile: false,
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
            // Off in this crate's tests, which count panes' rows; the window
            // status line's own tests turn it on.
            showstatusline: !cfg!(test),
            foreground: None,
            background: None,
            palette: Vec::new(),
            guifont: vec![temzdf_scene::FontEntry { family: "JetBrains Mono".into(), size: Some(14.0) }],
            guifontwide: Vec::new(),
            text: TextOptions::default(),
            basemode: std::collections::BTreeMap::new(),
            copyhistory: 30,
            clipboard: registers::ClipboardSync::All,
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
/// The lowest z of programs' images in terminal panes: never under the cell
/// backgrounds (the GUI draws layers below -2^30 under them).
const Z_PROGRAMS_MIN: i32 = -1_073_741_698;
/// What a PDF pane says to frontends without images.
const NO_GRAPHICS_PDF: &str = "PDF viewer disabled (no kitty graphics protocol support)";
/// The session's own cells: borders, frames, the tab line.
const BACKGROUND: [u8; 4] = [0x3a, 0x3a, 0x3a, 0xff];
const BORDER: [u8; 4] = [0x5a, 0x5a, 0x5a, 0xff];
/// The most mappings one typed key may run (Vim's `maxmapdepth`).
const MAX_MAP_DEPTH: usize = 1000;

/// Where a pane goes.
#[derive(Clone, Debug)]
enum Spot {
    /// The first pane of the session.
    First,
    /// Next to `target` (the whole layout for `None`), before (left/above)
    /// or after it; `size` cells wide (or high) and fixed when given.
    Split { target: Option<PaneId>, split: Split, before: bool, size: Option<u32> },
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
    Text { buf: BufferId, view: Box<text::TextView> },
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
            Kind::Term { .. } | Kind::Text { .. } => None,
        }
    }

    fn doc(&self) -> Option<DocId> {
        match &self.kind {
            Kind::Pdf { doc, .. } => Some(*doc),
            Kind::Term { .. } | Kind::Text { .. } => None,
        }
    }

    fn term(&self) -> Option<BufferId> {
        match &self.kind {
            Kind::Term { buf, .. } => Some(*buf),
            Kind::Pdf { .. } | Kind::Text { .. } => None,
        }
    }

    fn buffer(&self) -> BufferId {
        match &self.kind {
            Kind::Pdf { doc, .. } => *doc,
            Kind::Term { buf, .. } | Kind::Text { buf, .. } => *buf,
        }
    }

    /// The kind's name, as scripts see it.
    fn kind_name(&self) -> &'static str {
        match &self.kind {
            Kind::Pdf { .. } => "pdf",
            Kind::Term { .. } => "terminal",
            Kind::Text { .. } => "edit",
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
    texts: HashMap<BufferId, text::TextEntry>,
    /// Where undo files go (o.undofile; tests set their own).
    undo_dir: Option<PathBuf>,
    /// Images read for text panes' decorations (loadImage), by id.
    loaded_images: images::LoadedImages,
    /// Markdown files shown in viewers, by id.
    md_docs: HashMap<markdown::MdId, markdown::MdDoc>,
    /// Where each viewer pane was before it followed a link to another
    /// file (`markdown.back`): the file and the source line at the top.
    md_history: HashMap<PaneId, Vec<(PathBuf, usize)>>,
    /// Where swap files go (o.swapfile; tests set their own).
    swap_dir: Option<PathBuf>,
    /// Highlight groups for text panes (scripts define them).
    highlights: temzdf_texteditor::Highlights,
    /// Buffer, pane and tab page variables (`getVar`, `setVar`).
    vars: HashMap<(VarOwner, String), ScriptValue>,
    /// `highlightPattern`: (namespace, pattern, style), painted in text panes.
    patterns: Vec<(String, temzdf_texteditor::Pattern, String)>,
    /// While a `temzdf action` request waits for the scripts it started:
    /// why a "before" hook cancelled what it asked for, if one did.
    cli_cancel: Option<Option<String>>,
    /// The session ends because it was killed (`kill-session`).
    killed: bool,
    /// Invocations (doc/design/text-editor.md section 9): the last one's
    /// number, and the one under way (its edits name it in `TextChanged`).
    inv_seq: u64,
    pub(super) current_invocation: Option<u64>,
    /// Counts invocations (keys, text, pastes, CLI requests): a text
    /// buffer's undo step ends when a new one edits it.
    invocation: u64,
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
    /// The terminal buffer with a visual mode, on or suspended (one at a
    /// time; see [`Session::mode_changed`]).
    visual: Option<BufferId>,
    /// Mappings run since the matcher last used up its keys (Vim's
    /// `maxmapdepth`).
    map_depth: usize,
    keys: Keys,
    effects: Vec<Effect>,
    /// The copy history and named registers.
    copies: registers::Copies,
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
            texts: HashMap::new(),
            undo_dir: text::undo_dir(),
            loaded_images: HashMap::new(),
            md_docs: HashMap::new(),
            md_history: HashMap::new(),
            // (Unit tests set theirs: none in the user's state directory.)
            swap_dir: if cfg!(test) { None } else { swap::swap_dir() },
            highlights: temzdf_texteditor::Highlights::default(),
            patterns: Vec::new(),
            vars: HashMap::new(),
            invocation: 0,
            cli_cancel: None,
            killed: false,
            inv_seq: 0,
            current_invocation: None,
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
            visual: None,
            map_depth: 0,
            keys: Keys::default(),
            effects: Vec::new(),
            copies: registers::Copies::default(),
            hovered: None,
            captured: None,
            focus_click: false,
            dragging: None,
            float_drag: None,
            over_frame: None,
            over_border: None,
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
            Spot::Split { target: Some(self.focus), split: Split::Horizontal, before: false, size: None }
        } else {
            Spot::First
        }
    }

    /// Open a new pane: the first one, or beside the focused pane.
    pub fn new_pane(&mut self, spec: &PaneSpec, cwd: Option<&Path>) -> Result<()> {
        let cwd = cwd.map(Path::to_owned);
        match spec {
            PaneSpec::File(path) => {
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
        if !is_pdf(path) {
            return self.open_text(path, spot, true).map(drop);
        }
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

    /// Where panes go: the screen, below the tab line and above the
    /// window's status line when they show.
    fn area(&self) -> Rect {
        let top = u32::from(self.tabline_shown());
        let bottom = u32::from(self.window_statusline_shown());
        Rect::new(0, top as i32, self.geometry.cols as u32, (self.geometry.rows as u32).saturating_sub(top + bottom))
    }

    /// Whether the window's status line shows (`showstatusline`, and room
    /// for it: three rows at least).
    fn window_statusline_shown(&self) -> bool {
        self.options.showstatusline && self.geometry.rows > 2
    }

    /// The window's status line: the mode, and the session (and the tab
    /// page, with several).
    fn window_statusline(&self) -> Option<TextGrid> {
        if !self.window_statusline_shown() {
            return None;
        }
        let mode = match self.history.current() {
            "visual" => self
                .panes
                .get(&self.focus)
                .and_then(PaneEntry::term)
                .and_then(|b| self.terms.get(&b))
                .and_then(|t| t.term.visual_label())
                .unwrap_or("VISUAL")
                .to_owned(),
            m => m.to_uppercase(),
        };
        let right = if self.tabs.len() > 1 {
            format!("{}/{}  {}", self.tab + 1, self.tabs.len(), self.name)
        } else {
            self.name.clone()
        };
        let row = self.geometry.rows - 1;
        Some(TextGrid { col: 0, row, rows: vec![statusline::window(&mode, &right, self.geometry.cols as usize)] })
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
        let (target, split, before, size) = match spot {
            // (A pane of another tab is no target here.)
            Spot::Split { target: Some(t), split, before, size } if self.tab_of(t) == Some(self.tab) => {
                (Some(tiled(t)), split, before, size)
            }
            Spot::Split { target: None, split, before, size } => (None, split, before, size),
            // The target went away meanwhile: split the focused pane instead.
            _ => (Some(tiled(self.focus)), Split::Horizontal, false, None),
        };
        match self.layout.as_mut() {
            None => self.layout = Some(Layout::new(id)),
            Some(layout) => {
                if !layout.split_sized(target, id, split, before, size, area) {
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
        } else if self.texts.contains_key(&buf) {
            let mut tv = self.text_view_for(buf, pane);
            (tv.view.width, tv.view.height) = (g.cols, g.rows);
            Kind::Text { buf, view: Box::new(tv) }
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
            self.history.reset(&mode);
            self.mode_changed(&old);
        }
        Ok(Outcome::Redraw)
    }

    /// What a pane showed is no longer shown there. A buffer no pane shows
    /// stays, hidden (`BufferHidden`): a document keeps its position (and
    /// drops its tiles), a terminal keeps running; an ended terminal goes.
    fn hide(&mut self, kind: Kind) {
        let buf = match &kind {
            Kind::Pdf { doc, .. } => *doc,
            Kind::Term { buf, .. } | Kind::Text { buf, .. } => *buf,
        };
        let name = match &kind {
            Kind::Pdf { .. } => "pdf",
            Kind::Term { .. } => "terminal",
            Kind::Text { .. } => "edit",
        };
        if !self.shown(buf) {
            self.event("BufferHidden", vec![ScriptValue::Num(buf as f64), ScriptValue::Str(name.into())]);
        }
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
            Kind::Text { buf, .. } => {
                // A viewer's drawing no pane shows any more goes.
                if let Some(md) = self.texts.get(&buf).and_then(|t| t.markdown) {
                    self.md_collect(md);
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

    /// The answer to the last `temzdf action` request, once the scripts it
    /// started are done: an error when a "before" hook cancelled it.
    pub fn take_cli_answer(&mut self) -> Result<(), String> {
        match self.cli_cancel.take() {
            Some(Some(reason)) => Err(reason),
            _ => Ok(()),
        }
    }

    /// It ends because `kill-session` asked (clients say it was killed).
    pub fn was_killed(&self) -> bool {
        self.killed
    }

    fn has_buffer(&self, buf: BufferId) -> bool {
        self.terms.contains_key(&buf) || self.docs.contains_key(&buf) || self.texts.contains_key(&buf)
    }

    /// Delete a buffer: end its program or close its document, and close the
    /// panes showing it (the last pane: the session ends). `PaneClosePre`
    /// hooks do not run (the buffer goes either way); `PaneClosed` does.
    fn delete_buffer(&mut self, buf: BufferId) -> Result<Outcome, String> {
        if !self.has_buffer(buf) {
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
            self.drop_swap(buf);
            if self.texts.remove(&buf).is_none() {
                return;
            }
        }
        self.event("BufferDeleted", vec![ScriptValue::Num(buf as f64)]);
    }

    /// The buffers, for `buffers()`.
    /// The buffers, `unlisted` ones only with `all`.
    fn buffer_list(&self, all: bool) -> ScriptValue {
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
            .chain(
                self.texts
                    .iter()
                    .filter(|(_, t)| all || !t.has("unlisted"))
                    .map(|(id, t)| (*id, "edit", t.name.clone(), false)),
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
                        ("modified", ScriptValue::Bool(self.texts.get(&id).is_some_and(|t| t.modified()))),
                        ("attrs", self.texts.get(&id).map_or(ScriptValue::List(Vec::new()), |t| t.attrs_value())),
                    ])
                })
                .collect(),
        )
    }

    /// The number of buffers (for `temzdf ls`).
    pub fn buffer_count(&self) -> usize {
        self.docs.values().filter(|d| d.host.is_some()).count()
            + self.terms.len()
            + self.texts.values().filter(|t| !t.has("unlisted")).count()
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
        self.history.reset(&mode);
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
                Kind::Text { .. } => {}
            }
        }
        self.fit_text_view(id);
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
            SessionEvent::TextFile(buf) => {
                self.text_file_changed(buf);
                Outcome::None
            }
            SessionEvent::Markdown(md) => self.md_file_changed(md),
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
        self.invocation += 1;
        // A cancelled operation a key asked for is told in a message.
        self.cli_cancel = None;
        // A key dismisses error messages (and is handled as usual).
        let dismissed = Outcome::None.or(self.dismiss_errors());
        self.map_depth = 0;
        if self.hold_key(k) {
            return dismissed;
        }
        // KeyTyped hooks hear it before it is matched: while they run, it
        // waits (first in line).
        if self.script.hooks_on("KeyTyped") {
            let typed = self.typed_text(k);
            self.key_typed(Stroke::of(k).notation(), typed, "typed");
            let heard = self.flush_events();
            if self.script.busy() {
                self.script.hold_announced(k.clone());
                return dismissed.merge(heard);
            }
        }
        dismissed.merge(self.key_announced(k, now))
    }

    /// A key whose KeyTyped hooks ran: matched and handled.
    pub(super) fn key_announced(&mut self, k: &KeyEvent, now: Instant) -> Outcome {
        self.last_key = now;
        let out = self.key_inner(k, now);
        out.merge(self.flush_events())
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
            Some(Kind::Term { .. } | Kind::Text { .. }) => Outcome::None,
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
                Step::Run { target, count, keys } => {
                    // Mappings that feed keys mapping to themselves (`a` to
                    // `a`, or to `aa`) would run forever, as in Vim.
                    self.map_depth += 1;
                    if self.map_depth > MAX_MAP_DEPTH {
                        self.matcher.cancel();
                        self.message("recursive mapping (more than 1000 mappings for one key)");
                        out = out.merge(Outcome::Redraw);
                        break;
                    }
                    let keys = keys.iter().map(|k| k.notation()).collect();
                    out = out.merge(self.invoke(Invoked::Target { target, count, at: now }, "key", keys));
                }
                Step::Unmapped(stroke, ev) => {
                    let ev = ev.unwrap_or_else(|| stroke.event());
                    match self.fallback() {
                        Fallback::Drop => {}
                        // Typing into a text or input pane is an
                        // invocation; a key for a terminal's program is not.
                        Fallback::Passthrough => match self.typed_text(&ev) {
                            Some(t) if self.focused_takes_text() => {
                                out = out.merge(self.invoke(Invoked::Text(t), "key", stroke.notation()));
                            }
                            _ => out = out.merge(self.send_to_pane(&ev)),
                        },
                        Fallback::Func(func) => {
                            let text = ev.text.clone().or_else(|| match ev.code {
                                KeyCode::Char(c) => Some(c.to_string()),
                                _ => None,
                            });
                            let key = ScriptValue::map([
                                ("keys", ScriptValue::Str(stroke.notation())),
                                ("text", text.map_or(ScriptValue::Null, ScriptValue::Str)),
                            ]);
                            out = out.merge(self.invoke(Invoked::Fallback { func, key }, "key", stroke.notation()));
                        }
                    }
                }
            }
        }
        out
    }

    /// Start an invocation (a mapping, typed text, a fallback function, a
    /// CLI request): `InvokePre` hooks first (they may cancel it), then it
    /// runs; `InvokePost` comes at the safe point after it and its hooks.
    pub(super) fn invoke(&mut self, what: Invoked, source: &str, keys: String) -> Outcome {
        self.inv_seq += 1;
        let count = match &what {
            Invoked::Target { count: Some(c), .. } => ScriptValue::Num(*c as f64),
            _ => ScriptValue::Null,
        };
        let info = ScriptValue::map([
            ("seq", ScriptValue::Num(self.inv_seq as f64)),
            ("target", what.value()),
            ("keys", ScriptValue::Str(keys)),
            ("count", count),
            ("mode", ScriptValue::Str(self.history.current().to_owned())),
            ("pane", ScriptValue::Num(self.focus as f64)),
            ("source", ScriptValue::Str(source.to_owned())),
        ]);
        self.current_invocation = Some(self.inv_seq);
        if self.script.hooks_on("InvokePost") {
            self.script.pending_post = Some(info.clone());
        }
        self.request_op(vec![("InvokePre".to_owned(), vec![info])], script::Deferred::Run(Box::new(what)))
    }

    /// Run what an invocation invokes (also `invoke`, again).
    pub(super) fn run_invoked(&mut self, what: Invoked) -> Outcome {
        match what {
            Invoked::Target { target, count, at } => self.run_target(target, count, at),
            Invoked::Text(t) => self.text_type(&t).unwrap_or(Outcome::None),
            Invoked::Fallback { func, key } => {
                let ctx = self.key_context(None);
                self.call(func, vec![ctx, key], false);
                Outcome::None
            }
            Invoked::Cli { name, args } => match self.run_command(&name, &args, None, Instant::now()) {
                Ok((_, o)) => o,
                Err(e) => {
                    match &mut self.cli_cancel {
                        Some(slot) => {
                            slot.get_or_insert(e);
                        }
                        None => self.message(&e),
                    }
                    Outcome::Redraw
                }
            },
        }
    }

    /// `KeyTyped({ keys, text, source })`: a key arrived (typed, or fed by
    /// `feedkeys`), before it is matched.
    pub(super) fn key_typed(&mut self, keys: String, text: Option<String>, source: &str) {
        let info = ScriptValue::map([
            ("keys", ScriptValue::Str(keys)),
            ("text", text.map_or(ScriptValue::Null, ScriptValue::Str)),
            ("source", ScriptValue::Str(source.into())),
        ]);
        self.event("KeyTyped", vec![info]);
    }

    /// The text a key types (no modifier but Shift), if any.
    fn typed_text(&self, k: &KeyEvent) -> Option<String> {
        let typed = k.text.clone().or_else(|| match k.code {
            KeyCode::Char(c) => Some(c.to_string()),
            _ => None,
        });
        let held = [Modifiers::ALT, Modifiers::CTRL, Modifiers::SUPER, Modifiers::HYPER, Modifiers::META];
        let plain = !held.iter().any(|m| k.modifiers.contains(*m));
        typed.filter(|t| plain && !t.chars().any(char::is_control))
    }

    /// The focused pane takes typed text (a text or input pane).
    fn focused_takes_text(&self) -> bool {
        matches!(self.panes.get(&self.focus).map(|e| &e.kind), Some(Kind::Text { .. }))
    }

    /// An `InvokePost` argument with the cursor and selection after it (the
    /// focused text pane's; null elsewhere).
    pub(super) fn with_cursor(&self, info: ScriptValue) -> ScriptValue {
        let ScriptValue::Map(mut fields) = info else { return info };
        let (cursor, selection) = match self.panes.get(&self.focus).map(|e| &e.kind) {
            Some(Kind::Text { buf, view }) if let Some(t) = self.texts.get(buf) => {
                let text = t.buf.text();
                let c = view.view.cursor;
                let sel = c.selection().map_or(ScriptValue::Null, |(a, b, _)| {
                    ScriptValue::map([
                        ("start", text::position_value(text.position(a))),
                        ("end", text::position_value(text.position(b))),
                    ])
                });
                (text::position_value(text.position(c.pos)), sel)
            }
            _ => (ScriptValue::Null, ScriptValue::Null),
        };
        fields.push(("cursor".into(), cursor));
        fields.push(("selection".into(), selection));
        ScriptValue::Map(fields)
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
        let kind = match self.panes.get(&self.focus) {
            Some(e) => ScriptValue::Str(e.kind_name().into()),
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
                    // Its own fallback function takes the keys it does not
                    // map, rather than leaving (reading one key).
                    _ if i == 0 && matches!(d.fallback, Some(Fallback::Func(_))) => break,
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
    fn fallback(&self) -> Fallback {
        self.mode_property(|d| d.fallback).unwrap_or(Fallback::Drop)
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

    /// An unmapped key in a pass-through mode goes to the focused pane: a
    /// terminal's program, or an input pane, which takes the characters it
    /// types (with no modifier but Shift) and drops the rest.
    fn send_to_pane(&mut self, k: &KeyEvent) -> Outcome {
        if let Some(t) = self.focused_term() {
            return Outcome::None.or(t.key(k));
        }
        let typed = k.text.clone().or_else(|| match k.code {
            KeyCode::Char(c) => Some(c.to_string()),
            _ => None,
        });
        let held = [Modifiers::ALT, Modifiers::CTRL, Modifiers::SUPER, Modifiers::HYPER, Modifiers::META];
        let plain = !held.iter().any(|m| k.modifiers.contains(*m));
        match typed.filter(|t| plain && !t.chars().any(char::is_control)) {
            Some(t) if self.text_type(&t).is_ok() => Outcome::Redraw,
            _ => Outcome::None,
        }
    }

    /// A pane's base mode, the bottom of the history while it has the
    /// focus: its buffer's (text buffers made with one), else its kind's
    /// (`o.basemode`), else the kind's own mode; one not defined falls back
    /// to the kind's.
    fn pane_mode(&self, id: PaneId) -> String {
        let Some(e) = self.panes.get(&id) else { return "normal".into() };
        let kind = e.kind_name();
        let chosen = match &e.kind {
            Kind::Text { buf, .. } => self.texts.get(buf).and_then(|t| t.basemode.clone()),
            _ => None,
        };
        chosen
            .or_else(|| self.options.basemode.get(kind).cloned())
            .filter(|m| self.modes.get(m).is_some())
            .unwrap_or_else(|| kind.into())
    }

    /// Announce a mode change (if `old` differs from the current mode).
    ///
    /// The `visual` mode drives the focused terminal's visual mode: entering
    /// it begins one (or goes on with a suspended one); leaving it ends it,
    /// unless the focus went to another pane (the search's input line, a
    /// click) or `visual` is still deeper in the history: then it is
    /// suspended, to go on when the mode comes back.
    fn mode_changed(&mut self, old: &str) {
        let new = self.history.current().to_owned();
        if new == "visual" && old != "visual" {
            let buf = self.panes.get(&self.focus).and_then(PaneEntry::term);
            if let Some(other) = self.visual.filter(|b| Some(*b) != buf)
                && let Some(t) = self.terms.get_mut(&other)
            {
                t.term.visual_end();
            }
            self.visual = None;
            if let Some(b) = buf
                && let Some(t) = self.terms.get_mut(&b)
                && t.term.visual_begin()
            {
                self.visual = Some(b);
            }
        } else if old == "visual"
            && new != "visual"
            && let Some(b) = self.visual
        {
            let here = self.panes.get(&self.focus).and_then(PaneEntry::term) == Some(b);
            let deeper = self.history.modes().any(|m| m == "visual");
            match self.terms.get_mut(&b) {
                Some(t) if here && !deeper => {
                    t.term.visual_end();
                    self.visual = None;
                }
                Some(t) => t.term.visual_suspend(),
                None => self.visual = None,
            }
        }
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
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in ids {
            let o = &self.options.text;
            let Some(Kind::Text { buf, view }) = self.panes.get_mut(&id).map(|e| &mut e.kind) else {
                continue;
            };
            view.apply(o);
            if let Some(t) = self.texts.get(buf) {
                view.view.relayout(&t.buf);
            }
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
        self.invocation += 1;
        if self.hold_text(text) {
            return Outcome::None;
        }
        let out = if self.fallback() == Fallback::Passthrough
            && let Some(t) = self.focused_term()
        {
            Outcome::None.or(t.send_text(text))
        } else if self.fallback() == Fallback::Passthrough && self.focused_takes_text() {
            self.invoke(Invoked::Text(text.to_owned()), "key", String::new())
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
        self.invocation += 1;
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
        // A text or input pane takes it (an invocation); elsewhere it goes
        // nowhere.
        if self.focused_takes_text() {
            return self.invoke(Invoked::Text(text.to_owned()), "key", String::new());
        }
        Outcome::None
    }

    /// Run an action. Errors (a file that cannot be opened, no room for a
    /// pane) are for whoever asked.
    pub fn action(&mut self, action: Action, now: Instant) -> Result<Outcome> {
        let out = self.action_inner(action, now)?;
        Ok(out.merge(self.flush_events()))
    }

    /// Split `target` (the whole layout for `None`): the new pane shows
    /// `buf`, or what `target` shows (the same document again, a new shell
    /// where it is); `size` cells wide (or high) and fixed when given. From
    /// a float: a new float instead. Returns the new pane (`None` for a
    /// document still opening).
    pub(super) fn split_pane(
        &mut self,
        target: Option<PaneId>,
        split: Split,
        before: bool,
        size: Option<u32>,
        buf: Option<BufferId>,
    ) -> Result<Option<PaneId>, String> {
        let from = target.unwrap_or(self.focus);
        let spot =
            if self.is_float(from) { Spot::Float(self.cascade()) } else { Spot::Split { target, split, before, size } };
        let what = match (buf, self.panes.get(&from).map(|e| &e.kind)) {
            (Some(b), _) => floats::FloatWhat::Buffer(b),
            // The same document or input again.
            (None, Some(Kind::Pdf { doc: b, .. } | Kind::Text { buf: b, .. })) => floats::FloatWhat::Buffer(*b),
            // A new shell where the focused one is.
            (None, Some(Kind::Term { buf, .. })) => {
                let cwd = self.terms.get(buf).and_then(|t| t.term.pwd()).filter(|d| d.is_dir());
                floats::FloatWhat::Spawn(Spawn { argv: None, cwd, env: Vec::new() })
            }
            (None, None) => return Err(format!("no pane {from}")),
        };
        self.open_what(what, spot)
    }

    fn action_inner(&mut self, action: Action, _now: Instant) -> Result<Outcome> {
        match action {
            Action::Split { split, before, root } => {
                let target = if root { None } else { Some(self.focus) };
                self.split_pane(target, split, before, None, None).map_err(|e| anyhow::anyhow!(e))?;
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
        // (A release ends the capture, and still belongs to it.)
        let captured = self.captured.is_some();
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
        // when their program tracks the mouse; in the visual mode the mouse
        // is the visual mode's.
        let select = self.base_mode() == "normal";
        let visual = self.history.current() == "visual";
        let Some(e) = self.panes.get_mut(&id) else { return out };
        let content = e.content();
        // A pane's status line is not its content: the pointer there goes to
        // the pane only to end what began in it (a drag, a selection).
        if !captured && row >= content.y + content.h as i32 {
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
                Some(t) if visual && t.term.visual_on() => out.or(view.visual_mouse(&mut t.term, &local)),
                Some(t) => out.or(view.mouse(&mut t.term, &local, select)),
                None => out,
            },
            Kind::Text { buf, view } => match self.texts.get_mut(buf) {
                Some(t) => {
                    // A click (press and release on one character) on a
                    // viewer's link follows it.
                    let click = t.markdown.is_some().then(|| text::click(t, view, &local, cell)).flatten();
                    let out = out.merge(text::mouse(t, view, &local, cell));
                    match click {
                        Some(at) => match self.md_follow_at(id, at) {
                            Ok(_) => out.merge(Outcome::Redraw),
                            Err(e) => {
                                self.message(&e);
                                out.merge(Outcome::Redraw)
                            }
                        },
                        None => out,
                    }
                }
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
        self.collect_copies();
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
        let mut regions = Vec::new();
        let mut fallback_text = Vec::new();
        // The focused pane's work first.
        let mut ids = order.clone();
        ids.sort_by_key(|id| *id != self.focus);
        for id in ids {
            // What lies above this pane, in cells and in pixels.
            let covers = self.covers(Some(id));
            let covers_px: Vec<Rect> = covers.iter().map(|r| self.pixels(r)).collect();
            let Some(e) = self.panes.get_mut(&id) else { continue };
            let frame = match &mut e.kind {
                Kind::Text { buf, view } => Self::text_frame(
                    self.texts.get(buf),
                    view,
                    id,
                    id == self.focus,
                    &self.highlights,
                    &self.patterns,
                    &self.loaded_images,
                    cell,
                ),
                Kind::Pdf { pane, doc } => {
                    let (frame, pane_needs) = pane.frame(&mut self.images);
                    needs.entry(*doc).or_default().extend(pane_needs);
                    complete &= pane.viewer.is_complete();
                    frame
                }
                Kind::Term { buf, view } => match self.terms.get_mut(buf) {
                    Some(t) => {
                        let mut frame = view.frame(&mut t.term, &mut self.images);
                        for l in &mut frame.layers {
                            l.z = l.z.max(Z_PROGRAMS_MIN);
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
            // For frontends without images: where it is, and a PDF pane's
            // line saying it cannot show.
            if content.h > 0 {
                let kind = match &e.kind {
                    Kind::Pdf { .. } => RegionKind::Pdf,
                    Kind::Term { .. } => RegionKind::Terminal,
                    Kind::Text { .. } => RegionKind::Input,
                };
                let images = kind == RegionKind::Pdf || !frame.layers.is_empty();
                regions.push(Region { pane: id, rect: content, kind, images });
                if kind == RegionKind::Pdf {
                    let line = statusline::centred(NO_GRAPHICS_PDF, content);
                    fallback_text.extend(clip::text_pieces(line, &covers));
                }
            }
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
            // A text pane's images' alt text, where images cannot be shown.
            for g in frame.fallback_text {
                let g = TextGrid { col: g.col + content.x as u16, row: g.row + content.y as u16, ..g };
                fallback_text.extend(clip::text_pieces(g, &covers));
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
                Kind::Text { buf, view } => Self::text_status(self.texts.get(buf), view, self.history.current()),
                Kind::Term { .. } => termpix_ui::StatusBar::default(),
            };
            let grid = statusline::grid(&bar, e.rect, *id == self.focus);
            text.extend(clip::text_pieces(grid, &self.covers(Some(*id))));
        }
        text.extend(self.border_grids());
        self.float_frames(&mut text);
        text.extend(self.tabline().map(|(grid, _)| grid));
        text.extend(self.window_statusline());
        self.keys.end_frame();
        for (doc, needs) in needs {
            if let Some(host) = self.docs.get_mut(&doc).and_then(|d| d.host.as_mut()) {
                host.dispatch(needs);
            }
        }
        // Floating messages over everything.
        text.extend(self.notice_grids());
        Frame { layers, text, cursor, cursor_shape, pointer: self.pointer(), title, regions, fallback_text }
    }

    /// The borders between the tiled panes, as text (see `borders`), cut by
    /// what lies above them.
    fn border_grids(&self) -> Vec<TextGrid> {
        let Some(layout) = self.layout.as_ref() else { return Vec::new() };
        let grids = borders::gaps(self.area(), &layout.rects(self.area()), self.focus);
        let covers = self.covers(None);
        grids.into_iter().flat_map(|g| clip::text_pieces(g, &covers)).collect()
    }

    /// A rectangle of cells in screen pixels.
    fn pixels(&self, r: &Rect) -> Rect {
        let cell = self.geometry.cell;
        Rect::new(r.x * cell.width as i32, r.y * cell.height as i32, r.w * cell.width, r.h * cell.height)
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
                Kind::Term { .. } | Kind::Text { .. } => true,
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
        self.expire_clipboard_reads(now);
        self.write_due_swaps(now);
        let mut out =
            self.script_tick(now).or(self.tick(now)).or(self.expire_notices(now)).or(self.expire_decorations(now));
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
            Kind::Term { .. } | Kind::Text { .. } => None,
        });
        let docs = self.docs.values().filter_map(|d| d.host.as_ref()?.next_deadline());
        let terms = self.terms.values().filter_map(|t| t.term.autoscroll_deadline());
        panes
            .chain(docs)
            .chain(terms)
            .chain(self.script_deadline())
            .chain(self.decorations_deadline())
            .chain(self.copies_deadline())
            .chain(self.swap_deadline())
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
/// (operating), `terminal` (pass-through), `pdf`, `input` and `edit`
/// (pass-through: typed characters go in) and `visual` (a terminal's visual mode, entered
/// from the others; see [`Session::mode_changed`]).
/// Who a variable belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum VarOwner {
    Buffer(BufferId),
    Pane(PaneId),
    Tab(tabs::TabId),
}

/// What an invocation runs.
pub(super) enum Invoked {
    /// A mapping's right-hand side (or `invoke`'s target), and when its
    /// key came (animations start from it, not from when it runs).
    Target { target: Target, count: Option<usize>, at: Instant },
    /// Text typed into a text or input pane (keys no mapping took, input
    /// methods, pastes).
    Text(String),
    /// A mode's fallback function, given the key.
    Fallback { func: crate::keymap::Cond, key: ScriptValue },
    /// A `temzdf action` request.
    Cli { name: String, args: Vec<ScriptValue> },
}

impl Invoked {
    /// What `InvokePre`/`InvokePost` call the target: actions as
    /// `{ name, args }` (what `invoke` takes back), a function's id, or
    /// `{ textInput }`.
    fn value(&self) -> ScriptValue {
        let act = |name: &str, args: &[ScriptValue]| {
            ScriptValue::map([("name", ScriptValue::Str(name.into())), ("args", ScriptValue::List(args.to_vec()))])
        };
        match self {
            Invoked::Target { target: Target::Actions(acts), .. } => {
                ScriptValue::List(acts.iter().map(|a| act(&a.name, &a.args)).collect())
            }
            Invoked::Target { target: Target::Func(f), .. } | Invoked::Fallback { func: f, .. } => {
                ScriptValue::Num(*f as f64)
            }
            Invoked::Text(t) => ScriptValue::map([("textInput", ScriptValue::Str(t.clone()))]),
            Invoked::Cli { name, args } => ScriptValue::List(vec![act(name, args)]),
        }
    }
}

/// A file opens as a PDF by its extension, otherwise as text.
fn is_pdf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

fn builtin_modes() -> Modes<Target> {
    let mut m = Modes::default();
    for (name, fallback, count) in [
        ("normal", Fallback::Drop, true),
        ("terminal", Fallback::Passthrough, false),
        ("pdf", Fallback::Drop, true),
        ("input", Fallback::Passthrough, false),
        ("edit", Fallback::Passthrough, false),
        ("visual", Fallback::Drop, true),
    ] {
        let d = m.define(name);
        d.fallback = Some(fallback);
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
        for lhs in ["[", "v"] {
            let target = acts(&[("exit-mode", &[]), ("switch-mode", &[s("visual")])]);
            m.map("normal", keys(lhs), crate::keymap::Mapping::new(target));
        }
        for (lhs, name) in [
            ("h", "visual.left"),
            ("l", "visual.right"),
            ("k", "visual.up"),
            ("j", "visual.down"),
            ("0", "visual.line-start"),
            ("$", "visual.line-end"),
            ("w", "visual.word-forward"),
            ("e", "visual.word-end"),
            ("gg", "visual.top"),
            ("G", "visual.bottom"),
            ("v", "visual.charwise"),
            ("V", "visual.linewise"),
            ("<C-v>", "visual.blockwise"),
            ("o", "visual.swap-ends"),
            ("y", "visual.yank"),
            ("Y", "visual.yank-lines"),
            ("<Esc>", "visual.escape"),
            ("n", "visual.next-match"),
            ("N", "visual.prev-match"),
            ("q", "exit-mode"),
        ] {
            m.map("visual", keys(lhs), crate::keymap::Mapping::new(acts(&[(name, &[])])));
        }
        m.map("visual", keys("<C-b>"), crate::keymap::Mapping::new(acts(&[("switch-mode", &[s("normal")])])));
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
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn searches_go_to_the_pdf_pane_given() {
        let (mut s, rx) = session();
        settle(&mut s, &rx);
        let pdf = s.focus;
        // Typed into an input pane below it: the PDF pane is not focused.
        let prompt = ScriptValue::map([("basemode", ScriptValue::Str("input".into()))]);
        let (buf, _) = s.run_command("new-buffer", &[prompt], None, Instant::now()).unwrap();
        let o = ScriptValue::map([
            ("side", ScriptValue::Str("bottom".into())),
            ("buffer", buf),
            ("height", ScriptValue::Num(1.0)),
        ]);
        let (input, _) = s.run_command("split", &[o], None, Instant::now()).unwrap();
        assert_eq!(s.focus, input.as_num().unwrap() as PaneId);
        let search = |s: &mut Session, name: &str, args: &[&str]| cmd(s, name, args, None);
        search(&mut s, "search-preview", &["needle", &pdf.to_string()]).unwrap();
        settle(&mut s, &rx);
        assert!(s.panes.get_mut(&pdf).and_then(PaneEntry::pdf).unwrap().search.is_active());
        search(&mut s, "search-cancel", &[&pdf.to_string()]).unwrap();
        assert!(!s.panes.get_mut(&pdf).and_then(PaneEntry::pdf).unwrap().search.is_active());
        // The focused input pane is no PDF.
        let e = search(&mut s, "search-for", &["needle"]).unwrap_err();
        assert!(e.contains("not a PDF"), "{e}");
        let focus = s.focus.to_string();
        let e = search(&mut s, "search-cancel", &[&focus]).unwrap_err();
        assert!(e.contains("is not a PDF"), "{e}");
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
        // doc/design/keymaps.md's example: + repeats in a submode of normal.
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
            assert!(in_left || in_right || screen, "{:?} crosses the panes", l.dst);
        }
        // The border between them is text: a line down the gap's column,
        // lit beside the focused (new, right) pane.
        let rows = screen_text(&frame, GEOMETRY);
        assert!(rows.iter().take(29).all(|r| r.chars().nth(44) == Some('│')), "{rows:?}");
        let border = frame.text.iter().find(|g| g.col == 44 && g.row == 0).expect("the border's top cell");
        let accent = termpix_ui::Theme::dark().accent.0;
        assert_eq!(border.rows[0][0].fg, temzdf_scene::Color::Rgb([accent[0], accent[1], accent[2]]));
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
        s.action(Action::NewPane { spec: PaneSpec::File(pdf::fixture("sample-short.pdf")), cwd: None }, Instant::now())
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

    /// Columns `a..b` of a screen row (characters, not bytes).
    fn cols(row: &str, a: usize, b: usize) -> String {
        row.chars().skip(a).take(b - a).collect()
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
        let list = s.buffer_list(false);
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
        let (_, rows) = wait_text(&mut s, &rx, |r| cols(&r[3], 6, 11) == "FLOAT");
        assert_eq!(rows[1], zeros, "above the float");
        assert_eq!(cols(&rows[2], 7, 12), " pop ", "the title on the frame");
        for (y, row) in rows.iter().enumerate().take(8).skip(2) {
            let inside: String = row.chars().skip(5).take(22).collect();
            assert!(!inside.contains('0'), "row {y}: the pane below shows through: {row:?}");
            assert_eq!(row.chars().nth(4), Some('0'), "row {y}: left of the float");
            assert_eq!(row.chars().nth(27), Some('0'), "row {y}: right of the float");
        }
        // The frame is a box of text around the title.
        assert_eq!(cols(&rows[2], 5, 27), "╭─ pop ──────────────╮");
        assert_eq!(cols(&rows[4], 5, 6), "│");
        assert_eq!(cols(&rows[7], 5, 27), "╰────────────────────╯");
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
        assert_eq!(cols(&rows[0], 2, 9), " title ");
        for r in [notice, title] {
            let px = s.pixels(&r);
            let under: Vec<_> = frame.layers.iter().filter(|l| l.dst.intersect(&px).is_some()).collect();
            assert!(under.is_empty(), "images under the text at {r:?}: {under:?}");
        }
        // Around them the frame is drawn.
        assert_eq!(cols(&rows[0], 0, 2), "╭─", "the frame of float {f}");
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
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn a_selection_released_over_the_status_line_is_copied() {
        let (mut s, rx) = session();
        settle(&mut s, &rx);
        let at = |kind, x, y| MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE };
        let left = MouseButton::Left;
        // From the title down onto the status line (the bottom row, 580..600).
        s.mouse(&at(MouseEventKind::Press(left), 318, 188), Instant::now());
        s.mouse(&at(MouseEventKind::Drag(left), 450, 590), Instant::now());
        s.mouse(&at(MouseEventKind::Release(left), 450, 590), Instant::now());
        let copied = s.take_effects().into_iter().find_map(|e| match e {
            temzdf_scene::Effect::Copy(t) => Some(t),
            _ => None,
        });
        assert!(copied.as_deref().is_some_and(|t| t.starts_with("temzdf sample document")), "{copied:?}");
        // It is the copy history's newest entry, from the PDF, as characters.
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| {
            s.run_command(name, args, None, Instant::now()).unwrap().0
        };
        let newest = match run(&mut s, "history", &[]) {
            ScriptValue::List(l) => l[0].clone(),
            v => panic!("{v:?}"),
        };
        assert_eq!(newest.get("text").and_then(ScriptValue::as_str), copied.as_deref());
        assert_eq!(newest.get("kind"), Some(&ScriptValue::Str("char".into())));
        assert_eq!(newest.get("how"), Some(&ScriptValue::Str("copy".into())));
        let source = newest.get("source").and_then(|x| x.get("kind"));
        assert_eq!(source, Some(&ScriptValue::Str("pdf".into())));
        // A text pane puts it.
        let buf = run(&mut s, "new-buffer", &[]);
        let o = ScriptValue::map([("side", ScriptValue::Str("right".into())), ("buffer", buf)]);
        run(&mut s, "split", &[o]);
        run(&mut s, "put", &[]);
        assert_eq!(Some(text_of(&mut s)), copied);
    }

    /// What the session copied (OSC 52 in a terminal client).
    fn copied(s: &mut Session) -> Vec<String> {
        s.take_effects()
            .into_iter()
            .filter_map(|e| match e {
                temzdf_scene::Effect::Copy(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn the_visual_mode_moves_selects_and_copies_by_keys() {
        // cat echoes what reaches it: keys in the visual mode must not.
        let (mut s, rx) = terminal_session(&["sh", "-c", "printf 'hello world\\nsecond line\\n'; exec cat"]);
        wait_text(&mut s, &rx, |r| r[1] == "second line");
        key(&mut s, "<C-b>[");
        assert_eq!(s.current_mode(), "visual");
        key(&mut s, "x");
        key(&mut s, "ggwve");
        assert!(rows_after(&mut s, &rx, 300)[2].is_empty(), "x went nowhere");
        key(&mut s, "y");
        assert_eq!(copied(&mut s), ["world"]);
        assert_eq!(s.current_mode(), "terminal", "y leaves");
        assert!(!s.focused_term().unwrap().visual_kept());
        // Counts, rows, <Esc> twice.
        key(&mut s, "<C-b>v");
        key(&mut s, "ggVjy");
        assert_eq!(copied(&mut s), ["hello world\nsecond line\n"], "lines, with their line break");
        let (history, _) = s.run_command("history", &[], None, Instant::now()).unwrap();
        let ScriptValue::List(history) = history else { panic!() };
        let newest = &history[0];
        assert_eq!(newest.get("kind").and_then(ScriptValue::as_str), Some("line"));
        assert_eq!(newest.get("source").and_then(|x| x.get("kind")).and_then(ScriptValue::as_str), Some("terminal"));
        key(&mut s, "<C-b>[");
        key(&mut s, "v<Esc>");
        assert_eq!(s.current_mode(), "visual", "the first <Esc> drops the selection");
        key(&mut s, "<Esc>");
        assert_eq!(s.current_mode(), "terminal");
        assert!(cmd(&mut s, "visual.left", &[], None).is_err(), "no visual mode now");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn the_window_status_line_names_the_mode_and_the_session() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "stty size; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == format!("{} {}", GEOMETRY.rows, GEOMETRY.cols));
        s.set_option("showstatusline", &ScriptValue::Str("always".into())).unwrap();
        assert_eq!(s.get_option("showstatusline"), Ok(ScriptValue::Str("always".into())));
        let last = GEOMETRY.rows as usize - 1;
        let bottom = |s: &mut Session| screen_text(&s.frame(), GEOMETRY)[last].clone();
        let row = bottom(&mut s);
        assert!(row.starts_with(" TERMINAL ") && row.ends_with("term"), "{row:?}");
        // The pane is a row shorter.
        assert_eq!(s.panes[&s.focus].rect.h, GEOMETRY.rows as u32 - 1);
        key(&mut s, "<C-b>");
        assert!(bottom(&mut s).starts_with(" NORMAL "));
        key(&mut s, "[");
        assert!(bottom(&mut s).starts_with(" VISUAL "));
        key(&mut s, "V");
        assert!(bottom(&mut s).starts_with(" VISUAL LINE "), "the kind of selection");
        let frame = s.frame();
        let cell = &frame.text.last().unwrap().rows[0][1];
        assert!(cell.attrs.bold && cell.bg != frame.text.last().unwrap().rows[0][40].bg, "the mode stands out");
        key(&mut s, "<Esc><Esc>");
        // With tab pages: which one, of how many.
        new_tab(&mut s, &["sh", "-c", "sleep 30"]);
        assert!(bottom(&mut s).ends_with("2/2  term"), "{:?}", bottom(&mut s));
        // never: the row goes back to the panes.
        s.set_option("showstatusline", &ScriptValue::Str("never".into())).unwrap();
        assert_eq!(s.panes[&s.focus].rect.h, GEOMETRY.rows as u32 - 1, "below the tab line only");
        assert!(!bottom(&mut s).contains("TERMINAL"));
        assert!(s.set_option("showstatusline", &ScriptValue::Str("auto".into())).is_err());
    }

    /// The screen after handling events for `ms` milliseconds.
    fn rows_after(s: &mut Session, rx: &Receiver<SessionEvent>, ms: u64) -> Vec<String> {
        let until = Instant::now() + Duration::from_millis(ms);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if let Ok(ev) = rx.recv_timeout(left) {
                s.handle(ev, Instant::now());
            }
        }
        screen_text(&s.frame(), GEOMETRY)
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn the_visual_mode_is_for_terminals_and_waits_while_another_pane_has_the_focus() {
        let (mut s, rx) = session();
        settle(&mut s, &rx);
        assert!(cmd(&mut s, "switch-mode", &["visual"], None).is_err(), "not on a PDF pane");
        assert_eq!(s.current_mode(), "pdf");

        let (mut s, rx) = terminal_session(&["sh", "-c", "echo first; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "first");
        let term = s.focus;
        key(&mut s, "<C-b>[");
        key(&mut s, "ggv");
        // The search's input line takes the focus: the visual mode waits.
        let prompt = ScriptValue::map([("basemode", ScriptValue::Str("input".into()))]);
        let (buf, _) = s.run_command("new-buffer", &[prompt], None, Instant::now()).unwrap();
        let spec = ScriptValue::map([
            ("side", ScriptValue::Str("bottom".into())),
            ("buffer", buf.clone()),
            ("height", ScriptValue::Num(1.0)),
        ]);
        s.run_command("split", &[spec], None, Instant::now()).unwrap();
        assert_eq!(s.current_mode(), "input");
        let t = s.terms.values().next().unwrap();
        assert!(t.term.visual_kept() && !t.term.visual_on(), "suspended");
        // Searching from there, then back.
        cmd(&mut s, "visual.search-preview", &["fir", &term.to_string()], None).unwrap();
        cmd(&mut s, "focus-pane", &[&term.to_string()], None).unwrap();
        assert_eq!(s.current_mode(), "terminal");
        cmd(&mut s, "switch-mode", &["visual"], None).unwrap();
        assert_eq!(s.focused_term().unwrap().visual_label(), Some("VISUAL CHAR"), "it goes on");
        assert_eq!(cmd(&mut s, "switch-mode", &["visual"], None), Ok(Outcome::None), "once only");
        // The prefix's normal mode over it, and back.
        key(&mut s, "<C-b>");
        assert!(!s.focused_term().unwrap().visual_on());
        key(&mut s, "<Esc>");
        assert_eq!(s.current_mode(), "visual");
        assert_eq!(s.focused_term().unwrap().visual_label(), Some("VISUAL CHAR"));
        key(&mut s, "q");
        assert!(!s.focused_term().unwrap().visual_kept(), "leaving in its own pane ends it");
    }

    /// A terminal session with a prompt below it, as openPrompt makes one
    /// (a nofile, unlisted text buffer in the mode `input`, one row high,
    /// without wrapping, `/` inline virtual text at its start), focused;
    /// returns the buffer and the pane.
    fn prompt_session() -> (Session, Receiver<SessionEvent>, BufferId, PaneId) {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo first; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "first");
        let str = |v: &str| ScriptValue::Str(v.into());
        let o = ScriptValue::map([
            ("basemode", str("input")),
            ("attrs", ScriptValue::List(vec![str("nofile"), str("unlisted")])),
        ]);
        let (buf, _) = s.run_command("new-buffer", &[o], None, Instant::now()).unwrap();
        let o = ScriptValue::map([("side", str("bottom")), ("buffer", buf.clone()), ("height", ScriptValue::Num(1.0))]);
        let (pane, _) = s.run_command("split", &[o], None, Instant::now()).unwrap();
        let pane = pane.as_num().unwrap() as PaneId;
        cmd(&mut s, "status-line", &["off"], None).unwrap();
        let o = ScriptValue::map([("wrap", ScriptValue::Bool(false)), ("number", str("off"))]);
        s.run_command("text.set-options", &[o], None, Instant::now()).unwrap();
        let at = ScriptValue::map([("start", pos(0, 0)), ("end", pos(0, 0))]);
        let virt = ScriptValue::map([
            ("virtText", ScriptValue::List(vec![ScriptValue::map([("text", str("/"))])])),
            ("style", ScriptValue::Null),
        ]);
        s.run_command("text.add-decoration", &[at, virt], None, Instant::now()).unwrap();
        (s, rx, buf.as_num().unwrap() as BufferId, pane)
    }

    /// The prompt pane's row (29) as text, and the cursor.
    fn prompt_row(s: &mut Session) -> (String, Option<(u16, u16)>) {
        let frame = s.frame();
        (screen_text(&frame, GEOMETRY)[29].clone(), frame.cursor)
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn prompts_are_text_panes_taking_typed_text() {
        let (mut s, _rx, buf, pane) = prompt_session();
        assert_eq!((s.current_mode(), s.panes[&pane].kind_name()), ("input", "edit"));
        assert_eq!(prompt_row(&mut s), ("/".into(), Some((1, 29))));
        // Characters typed go in after the prompt; keys with Ctrl or Alt,
        // and Enter, do not (unmapped here).
        key(&mut s, "ab<C-x><M-c><CR><Tab>C");
        assert_eq!(prompt_row(&mut s), ("/abC".into(), Some((4, 29))));
        // Input-method text, and pastes (lines and all: the cursor's line
        // is what a prompt runs).
        s.text("数式", Instant::now());
        assert_eq!(prompt_row(&mut s), ("/abC数 式".into(), Some((8, 29))));
        s.paste("x\ny");
        assert_eq!(text_of(&mut s), "abC数式x\ny");
        // A script's own buffer: unlisted, never modified.
        let (list, _) = s.run_command("buffers", &[], None, Instant::now()).unwrap();
        let ScriptValue::List(list) = list else { panic!() };
        assert!(list.iter().all(|b| b.get("id") != Some(&ScriptValue::Num(buf as f64))));
        let info = s.run_command("text.info", &[], None, Instant::now()).unwrap().0;
        assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(false)));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_long_prompt_scrolls_sideways_to_keep_the_cursor_in_sight() {
        let (mut s, _rx, _buf, _pane) = prompt_session();
        let long: String = ('a'..='z').cycle().take(120).collect();
        s.run_command("text.insert", &[ScriptValue::Str(long.clone())], None, Instant::now()).unwrap();
        let end = ScriptValue::map([("line", ScriptValue::Num(0.0)), ("character", ScriptValue::Num(120.0))]);
        s.run_command("text.set-cursor", &[end], None, Instant::now()).unwrap();
        let (row, cursor) = prompt_row(&mut s);
        // 121 columns and the cursor's: the last 89 characters and the cursor.
        assert_eq!((row.as_str(), cursor), (&long[31..], Some((89, 29))));
        s.run_command("text.set-cursor", &[pos(0, 0)], None, Instant::now()).unwrap();
        let (row, cursor) = prompt_row(&mut s);
        assert_eq!((row.as_str(), cursor), (format!("/{}", &long[..89]).as_str(), Some((1, 29))), "the prompt again");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn hidden_buffers_reach_hooks() {
        let (mut s, _rx, buf, pane) = prompt_session();
        s.listen(&["BufferHidden", "BufferDeleted"]);
        let b = ScriptValue::Num(buf as f64);
        // Shown in a second pane: closing one does not hide it.
        let term_pane = *s.panes.iter().find(|(_, e)| e.kind_name() == "terminal").unwrap().0;
        let o = ScriptValue::map([("buffer", b.clone()), ("pane", ScriptValue::Num(term_pane as f64))]);
        let (second, _) = s.run_command("split", &[o], None, Instant::now()).unwrap();
        s.script_events.clear();
        s.close(second.as_num().unwrap() as PaneId);
        assert!(s.script_events.iter().all(|(e, _)| e != "BufferHidden"), "{:?}", s.script_events);
        // Deleting it: hidden first, then deleted.
        s.script_events.clear();
        cmd(&mut s, "delete-buffer", &[&buf.to_string()], None).unwrap();
        let names: Vec<&str> = s.script_events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(names, ["BufferHidden", "BufferDeleted"]);
        assert_eq!(s.script_events[0].1, [b, ScriptValue::Str("edit".into())]);
        assert!(!s.panes.contains_key(&pane));
        // A terminal hidden by closing its pane too.
        let first = s.focus;
        let term = s.panes[&first].buffer();
        key(&mut s, "<C-b>%");
        s.script_events.clear();
        s.close(first);
        assert_eq!(
            s.script_events.iter().find(|(e, _)| e == "BufferHidden").map(|(_, a)| a.clone()),
            Some(vec![ScriptValue::Num(term as f64), ScriptValue::Str("terminal".into())])
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn split_shows_a_buffer_at_a_fixed_size_and_returns_the_pane() {
        let (mut s, rx) = terminal_session(&["sh", "-c", "echo first; sleep 30"]);
        wait_text(&mut s, &rx, |r| r[0] == "first");
        let first = s.focus;
        let buf = s.panes[&first].buffer();
        let opts = |kv: &[(&str, ScriptValue)]| ScriptValue::map(kv.iter().cloned());
        let n = |v: f64| ScriptValue::Num(v);
        let side = |v: &str| ScriptValue::Str(v.into());
        let split = |s: &mut Session, o: ScriptValue| s.run_command("split", &[o], None, Instant::now());
        // The first terminal again, one row high, below it.
        let (pane, _) =
            split(&mut s, opts(&[("side", side("bottom")), ("buffer", n(buf as f64)), ("height", n(1.0))])).unwrap();
        let pane = pane.as_num().expect("the new pane") as PaneId;
        assert_eq!((s.focus, s.panes[&pane].buffer()), (pane, buf));
        assert_eq!(s.panes[&pane].rect, Rect::new(0, 29, 90, 1));
        assert_eq!(s.panes[&first].rect, Rect::new(0, 0, 90, 28), "less the row and the border");
        // Fixed: a taller screen and equalizing leave it one row.
        s.resize(Geometry { rows: 40, ..GEOMETRY });
        assert_eq!(s.panes[&pane].rect, Rect::new(0, 39, 90, 1));
        cmd(&mut s, "equalize-panes", &[], None).unwrap();
        assert_eq!(s.panes[&pane].rect.h, 1);
        // Back to the first pane by its id; a pane of another one to split.
        cmd(&mut s, "focus-pane", &[&first.to_string()], None).unwrap();
        assert_eq!(s.focus, first);
        let (right, _) =
            split(&mut s, opts(&[("side", side("right")), ("pane", n(first as f64)), ("width", side("50%"))])).unwrap();
        let right = right.as_num().unwrap() as PaneId;
        assert_eq!((s.panes[&right].rect.w, s.panes[&right].rect.h), (45, 38));
        // Beside the one-row pane there is no room (4 rows at least).
        let beside = opts(&[("side", side("right")), ("pane", n(pane as f64)), ("width", n(20.0))]);
        assert!(split(&mut s, beside).is_err());
        // Wrong combinations are refused.
        assert!(split(&mut s, opts(&[("side", side("bottom")), ("width", n(3.0))])).is_err());
        assert!(split(&mut s, opts(&[("buffer", n(9999.0))])).is_err());
        assert!(cmd(&mut s, "focus-pane", &["9999"], None).is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot spawn processes (fixtures, programs)")]
    fn frames_say_where_panes_and_their_images_are_for_frontends_without_images() {
        let (mut s, rx) = session();
        let (frame, _) = settle(&mut s, &rx);
        let pdf = s.focus;
        // The PDF pane: images, and its line for frontends without them in
        // the middle of its content (29 rows: the status line below).
        assert_eq!(
            frame.regions,
            [Region { pane: pdf, rect: Rect::new(0, 0, 90, 29), kind: RegionKind::Pdf, images: true }]
        );
        let rows = screen_text(&Frame { text: frame.fallback_text.clone(), ..Frame::default() }, GEOMETRY);
        assert_eq!(rows[14].trim(), NO_GRAPHICS_PDF);
        assert!(screen_text(&frame, GEOMETRY).iter().all(|r| !r.contains("disabled")), "not in the text");
        // A float over it cuts the line.
        let n = |v: f64| ScriptValue::Num(v);
        let opts = [("width", n(20.0)), ("height", n(5.0)), ("col", n(30.0)), ("row", n(11.0))];
        let f = float(&mut s, &["sleep", "30"], &opts);
        let frame = s.frame();
        let rows = screen_text(&Frame { text: frame.fallback_text.clone(), ..Frame::default() }, GEOMETRY);
        assert!(!rows[14].contains("graphics protocol"), "{:?}", rows[14]);
        let kinds: Vec<_> = frame.regions.iter().map(|r| (r.pane, r.kind, r.images)).collect();
        assert!(kinds.contains(&(f, RegionKind::Terminal, false)), "{kinds:?}");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn a_terminal_region_has_images_once_a_program_shows_one() {
        let (mut s, rx) = terminal_session(&[
            "sh",
            "-c",
            "read l; printf '\\033_Ga=T,f=32,s=2,v=2,c=4,r=2,q=2;/wAA/wD/AP8AAP///////w==\\033\\\\'; echo; sleep 30",
        ]);
        let pane = s.focus;
        let images = |s: &mut Session| s.frame().regions.iter().find(|r| r.pane == pane).map(|r| r.images);
        assert_eq!(images(&mut s), Some(false));
        key(&mut s, "<CR>");
        let deadline = Instant::now() + Duration::from_secs(10);
        while images(&mut s) != Some(true) {
            assert!(Instant::now() < deadline, "the image never came");
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(50)) {
                s.handle(ev, Instant::now());
            }
        }
        assert!(s.frame().fallback_text.is_empty(), "terminal panes have no line");
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

    /// A session editing `name` (written with `content` first) in a
    /// temporary directory.
    fn text_session(name: &str, content: &str) -> (Session, PathBuf) {
        let dir = std::env::temp_dir().join(format!("temzdf-text-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        let sink: EventSink = Arc::new(|_| true);
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("test".into(), GEOMETRY, Hints::default(), settings, sink);
        default_mappings(&mut s.modes);
        s.start(PaneSpec::File(path.clone()), None).unwrap();
        (s, path)
    }

    /// A session whose pane shows `name` (Markdown, in a fresh directory
    /// with `others` beside it) in the viewer.
    fn markdown_session(name: &str, content: &str, others: &[(&str, &str)]) -> (Session, PathBuf) {
        let (mut s, path) = text_session(name, content);
        for (n, c) in others {
            std::fs::write(path.parent().unwrap().join(n), c).unwrap();
        }
        let p = ScriptValue::Str(path.to_str().unwrap().into());
        let pane = ScriptValue::map([("pane", ScriptValue::Num(f64::from(s.focus)))]);
        s.run_command("markdown.open", &[p, pane], None, Instant::now()).unwrap();
        (s, path)
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system, libtexprintf")]
    fn the_markdown_viewer_draws_text_that_cannot_be_edited() {
        let src = "# Title\n\nSome *text* and $\\alpha^2$.\n";
        let (mut s, path) = markdown_session("view.md", src, &[]);
        let rows = screen_text(&s.frame(), GEOMETRY);
        assert_eq!(rows[0].trim_end(), "Title");
        assert_eq!(rows[1].trim_end(), "═════");
        assert_eq!(rows[3].trim_end(), "Some text and α².", "math by libtexprintf");
        // The buffer is not modifiable, by edits, typing or undo.
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, None, Instant::now());
        let err = run(&mut s, "text.insert", &[ScriptValue::Str("x".into()), pos(0, 0)]).unwrap_err();
        assert!(err.contains("not modifiable"), "{err}");
        assert!(run(&mut s, "text.undo", &[]).unwrap_err().contains("not modifiable"));
        assert!(s.text_type("x").unwrap_err().contains("not modifiable"));
        assert_eq!(rows[0].trim_end(), screen_text(&s.frame(), GEOMETRY)[0].trim_end());
        // Its styles are decorations.
        let decos = run(&mut s, "text.decorations", &[]).unwrap().0;
        let ScriptValue::List(decos) = decos else { panic!() };
        assert!(decos.iter().any(|d| d.get("style") == Some(&ScriptValue::Str("MarkdownItalic".into()))));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn markdown_links_go_to_anchors_urls_and_files_with_a_way_back() {
        let src = "[down](#far) [web](https://example.com) [other](other.md#end)\n\n# Far\n";
        let (mut s, path) = markdown_session("links.md", src, &[("other.md", "# Other\n\n# End\n")]);
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, None, Instant::now());
        let follow = |s: &mut Session, col: usize| {
            run(s, "text.set-cursor", &[pos(0, col)]).unwrap();
            run(s, "markdown.follow", &[]).unwrap().0
        };
        assert_eq!(follow(&mut s, 1), ScriptValue::Bool(true));
        assert_eq!(cursor_of(&mut s), (2, 0), "at the heading");
        assert_eq!(follow(&mut s, 7), ScriptValue::Bool(true));
        assert!(
            s.take_effects().iter().any(|e| matches!(e, Effect::OpenUri { uri, .. } if uri == "https://example.com"))
        );
        assert_eq!(follow(&mut s, 4), ScriptValue::Bool(false), "a space: no link");
        let pane = s.focus;
        assert_eq!(follow(&mut s, 11), ScriptValue::Bool(true));
        assert_eq!(s.focus, pane, "the same pane");
        let rows = screen_text(&s.frame(), GEOMETRY);
        assert_eq!(rows[0].trim_end(), "End", "the other file, at its anchor");
        assert_eq!(run(&mut s, "markdown.back", &[]).unwrap().0, ScriptValue::Bool(true));
        assert!(screen_text(&s.frame(), GEOMETRY)[0].starts_with("down web other"));
        assert_eq!(run(&mut s, "markdown.back", &[]).unwrap().0, ScriptValue::Bool(false));
        // Only one document stays open: the other went with its pane.
        assert_eq!(s.md_docs.len(), 1);
        let outline = run(&mut s, "markdown.outline", &[]).unwrap().0;
        assert_eq!(
            outline,
            ScriptValue::List(vec![ScriptValue::map([
                ("level", ScriptValue::Num(1.0)),
                ("text", ScriptValue::Str("Far".into())),
                ("anchor", ScriptValue::Str("far".into())),
                ("line", ScriptValue::Num(2.0)),
            ])])
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn markdown_is_drawn_again_for_a_new_width_and_a_changed_file_at_the_same_place() {
        let words = "word ".repeat(40);
        let src: String = (0..30).map(|i| format!("## Section {i}\n\n{words}\n\n")).collect();
        let (mut s, path) = markdown_session("long.md", &src, &[]);
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, None, Instant::now());
        run(&mut s, "markdown.goto", &[ScriptValue::Str("section-10".into())]).unwrap();
        assert_eq!(screen_text(&s.frame(), GEOMETRY)[0].trim_end(), "Section 10");
        let widths =
            |s: &Session| s.md_docs.values().next().unwrap().renders.iter().map(|r| r.width).collect::<Vec<_>>();
        assert_eq!(widths(&s), [90]);
        // Narrower: drawn again (paragraphs wrap at the new width), the old
        // drawing gone, the same heading on top.
        s.resize(Geometry { cols: 50, ..GEOMETRY });
        let rows = screen_text(&s.frame(), Geometry { cols: 50, ..GEOMETRY });
        assert_eq!(rows[0].trim_end(), "Section 10");
        assert!(rows[3].trim_end().len() <= 50 && rows[4].starts_with("word"), "{rows:?}");
        assert_eq!(widths(&s), [50], "drawn for the new width, the old drawing gone");
        // The file changes above: still at Section 10.
        let src2 = format!("# New top\n\nMore.\n\n{src}");
        std::fs::write(&path, &src2).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        let md = *s.md_docs.keys().next().unwrap();
        assert_eq!(s.md_file_changed(md), Outcome::Redraw);
        assert_eq!(screen_text(&s.frame(), Geometry { cols: 50, ..GEOMETRY })[0].trim_end(), "Section 10");
        assert_eq!(s.md_file_changed(md), Outcome::None, "unchanged since");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    fn text_of(s: &mut Session) -> String {
        s.run_command("text.get-text", &[], None, Instant::now()).unwrap().0.as_str().unwrap().to_owned()
    }

    fn cursor_of(s: &mut Session) -> (usize, usize) {
        let (c, _) = s.run_command("text.cursor", &[], None, Instant::now()).unwrap();
        let n = |k| c.get(k).unwrap().as_num().unwrap() as usize;
        (n("line"), n("character"))
    }

    fn pos(line: usize, character: usize) -> ScriptValue {
        ScriptValue::map([("line", ScriptValue::Num(line as f64)), ("character", ScriptValue::Num(character as f64))])
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn a_text_file_opens_takes_typing_and_is_written_back() {
        let (mut s, path) = text_session("notes.txt", "one\r\ntwo\r\n");
        assert_eq!(s.panes[&s.focus].kind_name(), "edit");
        assert_eq!(s.history.current(), "edit", "the pane's mode");
        assert_eq!(text_of(&mut s), "one\ntwo\n");
        key(&mut s, "hi");
        assert_eq!(text_of(&mut s), "hione\ntwo\n");
        assert_eq!(cursor_of(&mut s), (0, 2));
        // Moves by the mapping's count; typing goes on from there.
        s.run_command("text.move", &[ScriptValue::Str("line".into())], Some(1), Instant::now()).unwrap();
        assert_eq!(cursor_of(&mut s), (1, 2));
        key(&mut s, "!");
        assert_eq!(text_of(&mut s), "hione\ntw!o\n");
        // Each typing run is one step; undo puts the cursor back.
        cmd(&mut s, "text.undo", &[], None).unwrap();
        assert_eq!((text_of(&mut s).as_str(), cursor_of(&mut s)), ("hione\ntwo\n", (1, 2)));
        cmd(&mut s, "text.undo", &[], None).unwrap();
        assert_eq!(text_of(&mut s), "one\ntwo\n");
        cmd(&mut s, "text.redo", &[], None).unwrap();
        cmd(&mut s, "write", &[], None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hione\r\ntwo\r\n", "CRLF as it was read");
        let (info, _) = s.run_command("text.info", &[], None, Instant::now()).unwrap();
        assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(false)));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn panes_on_one_text_buffer_follow_each_others_edits() {
        let (mut s, path) = text_session("shared.txt", "abc\n");
        let first = s.focus;
        s.run_command("text.set-cursor", &[pos(0, 3)], None, Instant::now()).unwrap();
        let (second, _) = s.run_command("split", &[], None, Instant::now()).unwrap();
        let second = second.as_num().unwrap() as PaneId;
        assert_eq!(s.focus, second);
        assert_eq!(cursor_of(&mut s), (0, 3), "the split starts where the other pane was");
        let at0 = ScriptValue::map([("pane", ScriptValue::Num(first as f64))]);
        s.run_command("text.insert", &[ScriptValue::Str(">> ".into()), pos(0, 0), at0], None, Instant::now()).unwrap();
        assert_eq!(cursor_of(&mut s), (0, 6), "moved along by the other pane's edit");
        // The selection is replaced by typing.
        s.run_command("text.set-selection", &[pos(0, 3), pos(0, 6)], None, Instant::now()).unwrap();
        key(&mut s, "X");
        assert_eq!(text_of(&mut s), ">> X\n");
        assert_eq!(cursor_of(&mut s), (0, 4), "after the replacement");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn a_text_pane_draws_its_text_cursor_and_status_line() {
        let (mut s, path) = text_session("draw.txt", "hello\nあい\n");
        s.run_command("text.set-cursor", &[pos(1, 1)], None, Instant::now()).unwrap();
        let frame = s.frame();
        let row = |y: usize| -> String {
            let g = frame.text.iter().find(|g| g.row as usize <= y && y < g.row as usize + g.rows.len()).unwrap();
            g.rows[y - g.row as usize]
                .iter()
                .map(|c| if c.text.is_empty() && c.width != 0 { " " } else { &c.text })
                .collect()
        };
        assert!(row(0).starts_with("hello "), "{:?}", row(0));
        assert!(row(1).starts_with("あい "), "{:?}", row(1));
        assert!(row(2).trim().is_empty(), "the empty line after the last line break");
        assert!(row(3).starts_with('~'), "past the text");
        assert_eq!(frame.cursor, Some((2, 1)), "after あ");
        assert_eq!(frame.title, "draw.txt");
        key(&mut s, "x");
        let bar = Session::text_status(
            s.texts.values().next(),
            match &s.panes[&s.focus].kind {
                Kind::Text { view, .. } => view,
                _ => unreachable!(),
            },
            "edit",
        );
        assert_eq!(bar.left, "draw.txt [+]");
        assert!(bar.right.contains("2:3"), "{}", bar.right);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn text_files_are_refused_when_binary_and_new_when_missing() {
        let (mut s, path) = text_session("a.txt", "x");
        let dir = path.parent().unwrap().to_owned();
        std::fs::write(dir.join("bin.dat"), b"\0\0").unwrap();
        let e = s.run_command(
            "open-file",
            &[ScriptValue::Str(dir.join("bin.dat").display().to_string())],
            None,
            Instant::now(),
        );
        assert!(e.unwrap_err().contains("binary"));
        // A file that does not exist yet opens empty (new-pane FILE).
        s.new_pane(&PaneSpec::File(dir.join("new.txt")), None).unwrap();
        key(&mut s, "made");
        cmd(&mut s, "write", &[], None).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "made");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn base_modes_come_from_the_buffer_then_the_kind() {
        let (mut s, path) = text_session("base.txt", "x");
        s.modes.define("kindmode");
        s.modes.define("bufmode");
        s.options.basemode.insert("edit".into(), "kindmode".into());
        let first = s.focus;
        // A buffer made with its own base mode, in a split.
        let o = ScriptValue::map([("basemode", ScriptValue::Str("bufmode".into()))]);
        let (buf, _) = s.run_command("new-buffer", &[o], None, Instant::now()).unwrap();
        let o = ScriptValue::map([("buffer", buf)]);
        s.run_command("split", &[o], None, Instant::now()).unwrap();
        assert_eq!(s.history.current(), "bufmode");
        s.set_focus(first);
        assert_eq!(s.history.current(), "kindmode");
        // A mode nobody defined: the kind's own.
        s.options.basemode.insert("edit".into(), "nowhere".into());
        let (second, _) = s.run_command("split", &[], None, Instant::now()).unwrap();
        assert_eq!(s.focus, second.as_num().unwrap() as PaneId);
        assert_eq!(s.history.current(), "edit");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn text_panes_search_and_paint_patterns() {
        let (mut s, path) = text_session("search.txt", "one two\ntwo three two\n");
        let o = |pairs: Vec<(&str, ScriptValue)>| ScriptValue::map(pairs);
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| {
            s.run_command(name, args, None, Instant::now()).unwrap().0
        };
        let start = |v: &ScriptValue| {
            let p = v.get("range").unwrap().get("start").unwrap();
            (p.get("line").unwrap().as_num().unwrap() as usize, p.get("character").unwrap().as_num().unwrap() as usize)
        };
        let two = ScriptValue::Str("two".into());
        assert_eq!(start(&run(&mut s, "text.search", std::slice::from_ref(&two))), (0, 4), "from the cursor");
        let back = o(vec![("from", pos(1, 0)), ("backward", ScriptValue::Bool(true))]);
        assert_eq!(start(&run(&mut s, "text.search", &[two.clone(), back])), (0, 4));
        let lines = o(vec![("lines", ScriptValue::List(vec![ScriptValue::Num(1.0), ScriptValue::Num(1.0)]))]);
        assert_eq!(start(&run(&mut s, "text.search", &[two.clone(), lines])), (1, 0));
        let ScriptValue::List(all) = run(&mut s, "text.search-all", std::slice::from_ref(&two)) else { panic!() };
        assert_eq!(all.len(), 3);
        assert_eq!(run(&mut s, "text.search", &[ScriptValue::Str("nothing".into())]), ScriptValue::Null);
        assert!(s.run_command("text.search", &[ScriptValue::Str("(".into())], None, Instant::now()).is_err());
        // hlsearch: the matches painted in Search's colours.
        let hl = temzdf_texteditor::Style { bg: Some(temzdf_scene::Color::Indexed(3)), ..Default::default() };
        s.highlights.set("Search", hl);
        run(&mut s, "highlight-pattern", &[ScriptValue::Str("hl".into()), two]);
        let frame = s.frame();
        let row = &frame.text[0].rows[0];
        assert_eq!(
            (row[3].bg, row[4].bg, row[6].bg, row[7].bg),
            (temzdf_scene::Color::Default, hl.bg.unwrap(), hl.bg.unwrap(), temzdf_scene::Color::Default)
        );
        run(&mut s, "highlight-pattern", &[ScriptValue::Str("hl".into()), ScriptValue::Null]);
        assert_eq!(s.frame().text[0].rows[0][4].bg, temzdf_scene::Color::Default, "removed");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn scripts_decorate_text_and_define_highlight_groups() {
        let (mut s, path) = text_session("deco.txt", "keep gone keep\n");
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| {
            s.run_command(name, args, None, Instant::now()).unwrap().0
        };
        let range = |a: usize, b: usize| ScriptValue::map([("start", pos(0, a)), ("end", pos(0, b))]);
        let o = |pairs: Vec<(&str, ScriptValue)>| ScriptValue::map(pairs);
        let red = o(vec![("bg", ScriptValue::Str("#ff0000".into())), ("bold", ScriptValue::Bool(true))]);
        run(&mut s, "highlight", &[ScriptValue::Str("Red".into()), red]);
        let style = o(vec![("style", ScriptValue::Str("Red".into())), ("namespace", ScriptValue::Str("ns".into()))]);
        let id = run(&mut s, "text.add-decoration", &[range(5, 9), style]);
        let cell = s.frame().text[0].rows[0][5].clone();
        assert_eq!((cell.bg, cell.attrs.bold), (temzdf_scene::Color::Rgb([255, 0, 0]), true));
        // Its text deleted: flagged, and undo restores it.
        run(&mut s, "text.delete", &[range(5, 10)]);
        let d = run(&mut s, "text.decoration", std::slice::from_ref(&id));
        assert_eq!(d.get("deleted"), Some(&ScriptValue::Bool(true)));
        run(&mut s, "text.undo", &[]);
        let d = run(&mut s, "text.decoration", std::slice::from_ref(&id));
        assert_eq!((d.get("deleted"), d.get("range")), (Some(&ScriptValue::Bool(false)), Some(&range(5, 9))));
        let ScriptValue::List(list) =
            run(&mut s, "text.decorations", &[o(vec![("namespace", ScriptValue::Str("ns".into()))])])
        else {
            panic!()
        };
        assert_eq!(list.len(), 1);
        run(&mut s, "text.clear-namespace", &[ScriptValue::Str("ns".into())]);
        assert_eq!(run(&mut s, "text.decoration", &[id]), ScriptValue::Null);
        // A flash: gone once its time is up.
        let flash = o(vec![("style", ScriptValue::Str("Red".into())), ("ttl", ScriptValue::Num(1.0))]);
        run(&mut s, "text.add-decoration", &[range(0, 4), flash]);
        let due = s.next_deadline().unwrap();
        assert_eq!(s.poll(due + Duration::from_millis(1)), Outcome::Redraw);
        assert_eq!(s.frame().text[0].rows[0][0].bg, temzdf_scene::Color::Default);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn swap_files_follow_the_buffer_and_a_gone_sessions_is_recovered() {
        let (s0, path) = text_session("swap.txt", "one\n");
        drop(s0);
        let dir = path.parent().unwrap().to_owned();
        let swaps = dir.join("state-swap");
        // A session `name` on `path`, swap files in `swaps`, SwapExists heard
        // (`listen`) or not.
        let session = |name: &str, listen: bool| {
            let sink: EventSink = Arc::new(|_| true);
            let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
            let mut s = Session::new(name.into(), GEOMETRY, Hints::default(), settings, sink);
            s.swap_dir = Some(swaps.clone());
            if listen {
                s.listen(&["SwapExists"]);
            }
            s.start(PaneSpec::File(path.clone()), None).unwrap();
            s
        };
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, None, Instant::now());
        let str = |v: &str| ScriptValue::Str(v.into());
        let canonical = std::fs::canonicalize(&path).unwrap();
        let names = swap::candidates(&swaps, &canonical);
        let swapped = |p: &std::path::Path| swap::read(p).unwrap();
        // Opened: its swap file, written as edits settle (o.updatetime).
        let mut a = session("first", true);
        assert_eq!((swapped(&names[0]).text.as_str(), swapped(&names[0]).modified), ("one\n", false));
        run(&mut a, "text.insert", &[str("x"), pos(0, 0)]).unwrap();
        assert_eq!(swapped(&names[0]).text, "one\n", "not before o.updatetime");
        assert_eq!(a.swap_deadline(), a.next_deadline(), "its deadline is the session's");
        a.poll(Instant::now() + a.options.updatetime + Duration::from_millis(1));
        assert_eq!((swapped(&names[0]).text.as_str(), swapped(&names[0]).modified), ("xone\n", true));
        run(&mut a, "write", &[]).unwrap();
        assert!(!swapped(&names[0]).modified, "written with the file");
        // (In the same invocation as the write: still a change of its own.)
        run(&mut a, "text.insert", &[str("y"), pos(0, 0)]).unwrap();
        a.poll(Instant::now() + a.options.updatetime + Duration::from_millis(1));
        // `first` is gone (as after a crash: its swap file stays). Another
        // session opening the file hears of it and waits.
        // (Starting runs the events: SwapExists said what swapInfo says.)
        let mut b = session("second", true);
        let info = run(&mut b, "swap-info", &[]).unwrap().0;
        assert_eq!(info.get("session"), Some(&str("first")));
        assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(true)));
        assert!(!names[1].exists(), "no swap file of its own until answered");
        // Recover: the text that was not saved, as one change.
        run(&mut b, "text.recover", &[]).unwrap();
        assert_eq!(text_of(&mut b), "yxone\n");
        assert_eq!(run(&mut b, "text.info", &[]).unwrap().0.get("modified"), Some(&ScriptValue::Bool(true)));
        run(&mut b, "text.undo", &[]).unwrap();
        assert_eq!(text_of(&mut b), "xone\n");
        // Its own swap file next to that one; the old one deleted.
        let own = run(&mut b, "use-swap", &[]).unwrap().0;
        assert_eq!(own, str(names[1].to_str().unwrap()));
        run(&mut b, "delete-swap", &[str(names[0].to_str().unwrap())]).unwrap();
        assert!(!names[0].exists());
        assert_eq!(run(&mut b, "swap-info", &[]).unwrap().0, ScriptValue::Null);
        assert!(run(&mut b, "delete-swap", &[str("/etc/passwd")]).is_err(), "only swap files");
        // A normal end: its swap files go.
        b.end_swaps();
        assert!(!names[1].exists());
        // Nobody hearing SwapExists: edited anyway, with its own swap file.
        swap::write(&names[0], &swapped_of("first", "lost\n")).unwrap();
        let mut c = session("third", false);
        assert!(names[1].exists());
        assert!(c.notices.iter().any(|n| n.text.contains("found the swap file")));
        c.end_swaps();
        // o.swapfile off: none.
        std::fs::remove_file(&names[0]).unwrap();
        let sink: EventSink = Arc::new(|_| true);
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut d = Session::new("fourth".into(), GEOMETRY, Hints::default(), settings, sink);
        d.swap_dir = Some(swaps.clone());
        d.options.swapfile = false;
        d.start(PaneSpec::File(path.clone()), None).unwrap();
        assert!(!names[0].exists());
        // Read-only: written with force only.
        let attrs = ScriptValue::List(vec![str("readonly")]);
        let buf = run(&mut d, "text.info", &[]).unwrap().0.get("buffer").cloned().unwrap();
        run(&mut d, "set-buffer-attrs", &[buf, attrs]).unwrap();
        assert!(run(&mut d, "write", &[]).unwrap_err().contains("read-only"));
        run(&mut d, "write", &[ScriptValue::map([("force", ScriptValue::Bool(true))])]).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn options_choose_where_undo_and_swap_files_go() {
        let (s0, path) = text_session("dirs.txt", "one\n");
        drop(s0);
        let dir = path.parent().unwrap().to_owned();
        let str = |v: &str| ScriptValue::Str(v.into());
        let sink: EventSink = Arc::new(|_| true);
        let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
        let mut s = Session::new("dirs".into(), GEOMETRY, Hints::default(), settings, sink);
        let (undo, swaps) = (dir.join("my-undo"), dir.join("my-swap"));
        s.set_option("undodir", &str(undo.to_str().unwrap())).unwrap();
        s.set_option("directory", &str(swaps.to_str().unwrap())).unwrap();
        s.set_option("undofile", &ScriptValue::Bool(true)).unwrap();
        assert_eq!(s.get_option("directory").unwrap(), str(swaps.to_str().unwrap()));
        s.start(PaneSpec::File(path.clone()), None).unwrap();
        let canonical = std::fs::canonicalize(&path).unwrap();
        assert!(swap::candidates(&swaps, &canonical)[0].exists(), "the swap file there");
        s.run_command("text.insert", &[str("x"), pos(0, 0)], None, Instant::now()).unwrap();
        s.run_command("write", &[], None, Instant::now()).unwrap();
        assert!(text::undo_file_of(&undo, &canonical).exists(), "the undo file there");
        s.end_swaps();
        // "~/" is the home directory; null or "" the default.
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(text::dir_option(Some("~/u".into()), text::undo_dir).unwrap(), Some(home.join("u")));
        assert_eq!(text::dir_option(None, text::undo_dir).unwrap(), text::undo_dir());
        assert_eq!(text::dir_option(Some(String::new()), swap::swap_dir).unwrap(), swap::swap_dir());
        assert!(text::dir_option(Some("rel".into()), text::undo_dir).unwrap().unwrap().is_absolute());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A swap file of session `session` (gone) holding `text`.
    fn swapped_of(session: &str, text: &str) -> swap::SwapFile {
        swap::SwapFile {
            version: 1,
            path: String::new(),
            session: session.into(),
            pid: 1,
            host: String::new(),
            time: 0,
            modified: true,
            encoding: "utf-8".into(),
            line_ending: "lf".into(),
            text: text.into(),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn undo_trees_are_kept_in_files_when_written_and_read_back() {
        let (mut s, path) = text_session("undo.txt", "one\n");
        let dir = path.parent().unwrap().to_owned();
        let undo_dir = dir.join("state-undo");
        // A session on `path` with undo files in `undo_dir` (o.undofile).
        let session = |undofile: bool| {
            let sink: EventSink = Arc::new(|_| true);
            let settings = Settings { serve_forward: false, script: None, overrides: Vec::new() };
            let mut s = Session::new("test".into(), GEOMETRY, Hints::default(), settings, sink);
            s.undo_dir = Some(undo_dir.clone());
            s.options.undofile = undofile;
            s.start(PaneSpec::File(path.clone()), None).unwrap();
            s
        };
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, None, Instant::now());
        let str = |v: &str| ScriptValue::Str(v.into());
        // Off (the default): nothing written.
        run(&mut s, "text.insert", &[str("zero "), pos(0, 0)]).unwrap();
        run(&mut s, "write", &[]).unwrap();
        assert!(!undo_dir.exists());
        // On: written with the file, 0600 in a 0700 directory.
        s.undo_dir = Some(undo_dir.clone());
        s.options.undofile = true;
        // (Commands run here are one invocation: a boundary between the
        // two edits.)
        run(&mut s, "text.undo-boundary", &[]).unwrap();
        run(&mut s, "text.insert", &[str("two "), pos(0, 0)]).unwrap();
        run(&mut s, "write", &[]).unwrap();
        let canonical = std::fs::canonicalize(&path).unwrap();
        let file = text::undo_file_of(&undo_dir, &canonical);
        assert!(file.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(&undo_dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        // Opened again: undo goes back through both edits.
        let mut again = session(true);
        assert_eq!(text_of(&mut again), "two zero one\n");
        let info = run(&mut again, "text.info", &[]).unwrap().0;
        assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(false)));
        run(&mut again, "text.undo", &[]).unwrap();
        assert_eq!(text_of(&mut again), "zero one\n");
        run(&mut again, "text.undo", &[]).unwrap();
        assert_eq!(text_of(&mut again), "one\n");
        // Without o.undofile it is not read.
        let mut off = session(false);
        run(&mut off, "text.undo", &[]).unwrap();
        assert_eq!(text_of(&mut off), "two zero one\n");
        // Changed elsewhere: the old tree is not put on other text.
        std::fs::write(&path, "changed\n").unwrap();
        let mut changed = session(true);
        run(&mut changed, "text.undo", &[]).unwrap();
        assert_eq!(text_of(&mut changed), "changed\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn buffer_attributes_nofile_and_unlisted() {
        let (mut s, path) = text_session("attrs.txt", "x\n");
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, None, Instant::now());
        let str = |v: &str| ScriptValue::Str(v.into());
        let names = |s: &mut Session, all: bool| -> Vec<String> {
            let o = ScriptValue::map([("all", ScriptValue::Bool(all))]);
            match run(s, "buffers", &[o]).unwrap().0 {
                ScriptValue::List(l) => l.iter().map(|b| b.get("name").unwrap().as_str().unwrap().to_owned()).collect(),
                _ => Vec::new(),
            }
        };
        let o = ScriptValue::map([("attrs", ScriptValue::List(vec![str("nofile"), str("unlisted")]))]);
        let buf = run(&mut s, "new-buffer", &[o]).unwrap().0;
        assert_eq!(names(&mut s, false), ["attrs.txt"], "unlisted: left out");
        assert_eq!(names(&mut s, true).len(), 2, "all: listed");
        let count = s.buffer_count();
        // nofile: typed into, never modified; written only to a path given.
        let split = ScriptValue::map([("side", str("right")), ("buffer", buf.clone())]);
        run(&mut s, "split", &[split]).unwrap();
        run(&mut s, "text.type", &[str("typed")]).unwrap();
        let info = run(&mut s, "text.info", &[]).unwrap().0;
        assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(false)));
        assert_eq!(info.get("attrs"), Some(&ScriptValue::List(vec![str("nofile"), str("unlisted")])));
        assert!(run(&mut s, "write", &[]).is_err());
        // Changed: listed and modified like any.
        run(&mut s, "set-buffer-attrs", &[buf.clone(), ScriptValue::List(vec![])]).unwrap();
        assert_eq!(names(&mut s, false).len(), 2);
        assert_eq!(s.buffer_count(), count + 1);
        let info = run(&mut s, "text.info", &[]).unwrap().0;
        assert_eq!(info.get("modified"), Some(&ScriptValue::Bool(true)));
        assert!(run(&mut s, "set-buffer-attrs", &[buf, ScriptValue::List(vec![str("bogus")])]).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Write a `w` x `h` PNG of one colour at `path`.
    fn write_png(path: &std::path::Path, w: u32, h: u32) {
        let file = std::fs::File::create(path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(&vec![200; (w * h * 4) as usize]).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn inline_images_are_layers_over_holes_in_the_text() {
        let (mut s, path) = text_session("img.txt", "abc\n");
        let dir = path.parent().unwrap().to_owned();
        let png = dir.join("pic.png");
        // 10x20-pixel cells (GEOMETRY): 40x20 is four columns one row high.
        write_png(&png, 40, 20);
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, None, Instant::now());
        let str = |v: &str| ScriptValue::Str(v.into());
        let id = run(&mut s, "load-image", &[str(png.to_str().unwrap())]).unwrap().0;
        let at = ScriptValue::map([("start", pos(0, 1)), ("end", pos(0, 1))]);
        let o = ScriptValue::map([
            ("virtImage", ScriptValue::map([("image", id.clone()), ("alt", str("pic"))])),
            ("virtTextPos", str("inline")),
        ]);
        run(&mut s, "text.add-decoration", &[at, o]).unwrap();
        let frame = s.frame();
        assert_eq!(frame.layers.len(), 1, "{:?}", frame.layers);
        let l = frame.layers[0];
        assert_eq!((l.dst.x, l.dst.y, l.dst.w, l.dst.h), (10, 0, 40, 20), "after the a, its pixels");
        assert_eq!(s.frame().cursor, Some((0, 0)));
        // A hole in the text there; the alt text for frontends without images.
        let row = screen_text(&frame, GEOMETRY)[0].clone();
        assert!(row.starts_with("a    bc"), "{row:?}");
        let fallback = screen_text(&Frame { text: frame.fallback_text.clone(), ..Frame::default() }, GEOMETRY);
        assert!(fallback[0].starts_with(" pic"), "{:?}", fallback[0]);
        // Read again at another size: the same id, redrawn.
        write_png(&png, 20, 20);
        assert_eq!(run(&mut s, "load-image", &[str(png.to_str().unwrap())]).unwrap().0, id);
        assert_eq!(s.frame().layers[0].dst.w, 20);
        // Unloaded: its alt text in the text, no layer.
        run(&mut s, "unload-image", &[id]).unwrap();
        let frame = s.frame();
        assert!(frame.layers.is_empty());
        assert!(screen_text(&frame, GEOMETRY)[0].starts_with("apicbc"));
        assert!(run(&mut s, "load-image", &[str(path.to_str().unwrap())]).unwrap_err().contains("not a PNG"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn concealed_ranges_show_text_or_an_image_off_the_cursor_line() {
        let (mut s, path) = text_session("conceal.txt", "a\\alpha b\n\\pic.\nx\n");
        let dir = path.parent().unwrap().to_owned();
        let png = dir.join("pic.png");
        write_png(&png, 20, 20);
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, None, Instant::now());
        let str = |v: &str| ScriptValue::Str(v.into());
        let range = |l0, c0, l1, c1| ScriptValue::map([("start", pos(l0, c0)), ("end", pos(l1, c1))]);
        let rows = |s: &mut Session| screen_text(&s.frame(), GEOMETRY);
        let id = run(&mut s, "load-image", &[str(png.to_str().unwrap())]).unwrap().0;
        let alpha = ScriptValue::map([("conceal", ScriptValue::map([("text", str("α"))]))]);
        run(&mut s, "text.add-decoration", &[range(0, 1, 0, 7), alpha]).unwrap();
        let image = ScriptValue::map([(
            "conceal",
            ScriptValue::map([
                ("image", ScriptValue::map([("image", id.clone()), ("alt", str("P"))])),
                ("cursorLine", ScriptValue::Bool(true)),
            ]),
        )]);
        run(&mut s, "text.add-decoration", &[range(1, 0, 1, 4), image]).unwrap();
        // The cursor's line as it is; the image hidden there too.
        let r = rows(&mut s);
        assert!(r[0].starts_with("a\\alpha b") && r[1].starts_with("  ."), "{r:?}");
        let l = s.frame().layers[0];
        assert_eq!((l.dst.x, l.dst.y, l.dst.w, l.dst.h), (0, 20, 20, 20));
        // Elsewhere, concealed; the cursor in a range at its replacement.
        run(&mut s, "text.set-cursor", &[pos(2, 0)]).unwrap();
        assert!(rows(&mut s)[0].starts_with("aα b"), "{:?}", rows(&mut s));
        run(&mut s, "text.set-cursor", &[pos(1, 2)]).unwrap();
        assert_eq!(s.frame().cursor, Some((0, 1)));
        // Its image unloaded: the alt text.
        run(&mut s, "unload-image", &[id]).unwrap();
        assert!(s.frame().layers.is_empty());
        assert!(rows(&mut s)[1].starts_with("P."), "{:?}", rows(&mut s));
        // Within one line only.
        let bad = ScriptValue::map([("conceal", ScriptValue::map([("text", str("x"))]))]);
        assert!(run(&mut s, "text.add-decoration", &[range(0, 1, 1, 1), bad]).unwrap_err().contains("one line"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn virtual_lines_are_rows_of_text_and_images_cut_where_scrolled() {
        let (mut s, path) = text_session("vlines.txt", "ab\ncd\n");
        let dir = path.parent().unwrap().to_owned();
        let png = dir.join("pic.png");
        // 10x20-pixel cells: 30x60 pixels are three columns, three rows.
        write_png(&png, 30, 60);
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, None, Instant::now());
        let str = |v: &str| ScriptValue::Str(v.into());
        let at = |l| ScriptValue::map([("start", pos(l, 0)), ("end", pos(l, 0))]);
        let id = run(&mut s, "load-image", &[str(png.to_str().unwrap())]).unwrap().0;
        let below = ScriptValue::map([(
            "virtLines",
            ScriptValue::map([
                ("text", ScriptValue::List(vec![str("note")])),
                ("image", ScriptValue::map([("image", id), ("alt", str("P"))])),
            ]),
        )]);
        run(&mut s, "text.add-decoration", &[at(0), below]).unwrap();
        let above = ScriptValue::map([(
            "virtLines",
            ScriptValue::map([
                ("text", ScriptValue::List(vec![ScriptValue::List(vec![str("u"), str("p")])])),
                ("above", ScriptValue::Bool(true)),
            ]),
        )]);
        run(&mut s, "text.add-decoration", &[at(1), above]).unwrap();
        let rows = screen_text(&s.frame(), GEOMETRY);
        assert_eq!(rows[..7].iter().map(|r| r.trim_end()).collect::<Vec<_>>(), ["ab", "note", "", "", "", "up", "cd"]);
        let l = s.frame().layers[0];
        assert_eq!((l.dst.x, l.dst.y, l.dst.w, l.dst.h), (0, 40, 30, 60));
        assert_eq!((l.src.y, l.src.h), (0, 60));
        // Scrolled into the image: its lower two thirds at the top.
        run(&mut s, "text.scroll", &[ScriptValue::Num(3.0)]).unwrap();
        let l = s.frame().layers[0];
        assert_eq!((l.dst.y, l.dst.h, l.src.y, l.src.h), (0, 40, 20, 40));
        let bad = ScriptValue::map([("virtLines", ScriptValue::map([("text", str("x"))]))]);
        assert!(run(&mut s, "text.add-decoration", &[at(0), bad]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn virtual_text_shows_in_the_pane_and_the_cursor_skips_it() {
        let (mut s, path) = text_session("virt.txt", "abc\n");
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| s.run_command(name, args, None, Instant::now());
        let at = |a: usize| ScriptValue::map([("start", pos(0, a)), ("end", pos(0, a))]);
        let virt = |text: &str, where_: &str| {
            ScriptValue::map([
                ("virtText", ScriptValue::List(vec![ScriptValue::map([("text", ScriptValue::Str(text.into()))])])),
                ("virtTextPos", ScriptValue::Str(where_.into())),
            ])
        };
        let row = |s: &mut Session| -> String {
            s.frame().text[0].rows[0]
                .iter()
                .filter(|c| c.width > 0)
                .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
                .collect::<String>()
        };
        let id = run(&mut s, "text.add-decoration", &[at(0), virt("> ", "inline")]).unwrap().0;
        run(&mut s, "text.add-decoration", &[at(3), virt("note", "eol")]).unwrap();
        run(&mut s, "text.add-decoration", &[at(1), virt("B", "overlay")]).unwrap();
        assert!(row(&mut s).starts_with("> aBc note"), "{:?}", row(&mut s));
        assert_eq!(s.frame().cursor, Some((2, 0)), "after the prompt");
        // Typed at its place: after it (it stays before what is inserted).
        run(&mut s, "text.insert", &[ScriptValue::Str("x".into()), pos(0, 0)]).unwrap();
        assert!(row(&mut s).starts_with("> xaBc note"), "{:?}", row(&mut s));
        let d = run(&mut s, "text.decoration", &[id]).unwrap().0;
        assert_eq!(d.get("virtTextPos"), Some(&ScriptValue::Str("inline".into())));
        let bad = ScriptValue::map([("virtTextPos", ScriptValue::Str("left".into()))]);
        assert!(run(&mut s, "text.add-decoration", &[at(0), bad]).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn encodings_pairs_word_characters_variables_and_the_clipboard() {
        let (mut s, path) = text_session("misc.txt", "f(a, (b))\nfoo-bar\n");
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| {
            s.run_command(name, args, None, Instant::now()).unwrap().0
        };
        let str = |v: &str| ScriptValue::Str(v.into());
        // A Shift_JIS file, named.
        let sjis = path.parent().unwrap().join("sjis.txt");
        std::fs::write(&sjis, b"\x82\xa0\n").unwrap();
        assert!(s.run_command("open-file", &[str(sjis.to_str().unwrap())], None, Instant::now()).is_err());
        let o = ScriptValue::map([("encoding", str("shift_jis"))]);
        run(&mut s, "open-file", &[str(sjis.to_str().unwrap()), o]);
        assert_eq!(text_of(&mut s), "あ\n");
        key(&mut s, "い");
        cmd(&mut s, "write", &[], None).unwrap();
        assert_eq!(std::fs::read(&sjis).unwrap(), b"\x82\xa2\x82\xa0\n", "written back in Shift_JIS");
        cmd(&mut s, "close-pane", &[], None).unwrap();
        // Brackets.
        let p = run(&mut s, "text.find-pair", &[pos(0, 1)]);
        assert_eq!(p, pos(0, 8));
        let inner = ScriptValue::map([("direction", str("backward"))]);
        assert_eq!(run(&mut s, "text.find-pair", &[pos(0, 7), inner]), pos(0, 5));
        // Word characters per buffer.
        run(&mut s, "text.set-cursor", &[pos(1, 0)]);
        run(&mut s, "text.set-wordchars", &[str(r"[\p{L}\p{N}_-]")]);
        run(&mut s, "text.move", &[str("wordEnd")]);
        assert_eq!(cursor_of(&mut s), (1, 7), "foo-bar is one word now");
        assert!(s.run_command("text.set-wordchars", &[str("[")], None, Instant::now()).is_err());
        // Variables, kept by the server, gone with their owner.
        run(&mut s, "set-var", &[str("buffer"), str("x"), ScriptValue::Num(3.0)]);
        run(&mut s, "set-var", &[str("pane"), str("y"), str("p")]);
        assert_eq!(run(&mut s, "get-var", &[str("buffer"), str("x")]), ScriptValue::Num(3.0));
        assert_eq!(run(&mut s, "get-var", &[str("pane"), str("y")]), str("p"));
        assert_eq!(run(&mut s, "get-var", &[str("tab"), str("y")]), ScriptValue::Null);
        let (pane, _) = s.run_command("split", &[], None, Instant::now()).unwrap();
        let pane = pane.as_num().unwrap();
        assert_eq!(run(&mut s, "get-var", &[str("pane"), str("y")]), ScriptValue::Null, "the new pane's own");
        cmd(&mut s, "close-pane", &[], None).unwrap();
        assert!(
            s.run_command("get-var", &[str("pane"), str("y"), ScriptValue::Num(pane)], None, Instant::now()).is_err()
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn command_names_are_unique() {
        // A name twice: the first one found would hide the other (`copy`,
        // the PDF pane's, once was).
        let specs = commands::command_specs();
        let mut seen = std::collections::HashSet::new();
        for c in &specs {
            assert!(seen.insert(c.name.as_str()), "{} twice", c.name);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "the file system")]
    fn copies_registers_and_the_clipboard() {
        let (mut s, path) = text_session("copies.txt", "one\ntwo\n");
        let run = |s: &mut Session, name: &str, args: &[ScriptValue]| {
            s.run_command(name, args, None, Instant::now()).unwrap().0
        };
        let str = |v: &str| ScriptValue::Str(v.into());
        let opts = |kv: &[(&str, ScriptValue)]| ScriptValue::map(kv.iter().cloned());
        let texts = |v: ScriptValue| match v {
            ScriptValue::List(l) => l.iter().map(|e| e.get("text").unwrap().as_str().unwrap().to_owned()).collect(),
            _ => Vec::<String>::new(),
        };
        // Copies and deletes go to the clipboard (o.clipboard "all").
        let e = run(&mut s, "copy-text", &[str("a")]);
        assert_eq!(e.get("kind"), Some(&str("char")));
        assert_eq!(e.get("source").and_then(|x| x.get("kind")), Some(&str("edit")));
        run(&mut s, "copy-text", &[str("gone"), opts(&[("how", str("delete"))])]);
        assert_eq!(copied(&mut s), ["a", "gone"]);
        // Lines end with a line break.
        run(
            &mut s,
            "copy-text",
            &[str("line"), opts(&[("kind", str("line")), ("clipboard", ScriptValue::Bool(false))])],
        );
        assert_eq!(copied(&mut s), Vec::<String>::new());
        assert_eq!(texts(run(&mut s, "history", &[])), ["line\n", "gone", "a"]);
        // "copies": not deletes.
        s.set_option("clipboard", &str("copies")).unwrap();
        run(&mut s, "copy-text", &[str("cut"), opts(&[("how", str("delete"))])]);
        assert_eq!(copied(&mut s), Vec::<String>::new());
        // Named registers: not to the clipboard; appending lines to text.
        run(&mut s, "copy-text", &[str("r1"), opts(&[("register", str("a"))])]);
        run(
            &mut s,
            "copy-text",
            &[str("r2"), opts(&[("register", str("a")), ("kind", str("line")), ("append", ScriptValue::Bool(true))])],
        );
        assert_eq!(copied(&mut s), Vec::<String>::new());
        let a = run(&mut s, "register", &[str("a")]);
        assert_eq!((a.get("text"), a.get("kind")), (Some(&str("r1\nr2\n")), Some(&str("line"))));
        assert_eq!(run(&mut s, "register", &[str("b")]), ScriptValue::Null);
        // The history keeps o.copyhistory entries.
        s.set_option("copyhistory", &ScriptValue::Num(2.0)).unwrap();
        run(&mut s, "copy-text", &[str("last")]);
        assert_eq!(copied(&mut s), ["last"]);
        assert_eq!(texts(run(&mut s, "history", &[])), ["last", "r1\nr2\n"]);
        assert_eq!(s.cli_get_copy(None, 1), Ok("r1\nr2\n".to_owned()));
        assert_eq!(s.cli_get_copy(Some("a"), 0), Ok("r1\nr2\n".to_owned()));
        assert!(s.cli_get_copy(None, 5).is_err());
        // Putting: characters at the cursor, lines before the cursor's line.
        run(&mut s, "text.set-cursor", &[pos(1, 1)]);
        run(&mut s, "put", &[]);
        assert_eq!(text_of(&mut s), "one\ntlastwo\n");
        run(&mut s, "put", &[opts(&[("register", str("a"))])]);
        assert_eq!(text_of(&mut s), "one\nr1\nr2\ntlastwo\n");
        assert!(s.run_command("put", &[opts(&[("register", str("z"))])], None, Instant::now()).is_err());
        // Forgetting.
        let ScriptValue::List(history) = run(&mut s, "history", &[]) else { panic!() };
        let id = history[0].get("id").cloned().unwrap();
        run(&mut s, "forget-copy", &[id]);
        assert_eq!(texts(run(&mut s, "history", &[])), ["r1\nr2\n"]);
        run(&mut s, "forget-copy", &[]);
        assert_eq!(texts(run(&mut s, "history", &[])), Vec::<String>::new());
        assert_eq!(run(&mut s, "register", &[str("a")]), ScriptValue::Null);
        // Reading the clipboard asks the client; no answer in time fails it.
        let p = run(&mut s, "read-clipboard", &[opts(&[("primary", ScriptValue::Bool(true))])]);
        let ticket = p.get("$promise").and_then(ScriptValue::as_num).unwrap() as u64;
        assert_eq!(s.take_effects(), [temzdf_scene::Effect::ReadClipboard { ticket, primary: true }]);
        let due = s.next_deadline().unwrap();
        assert!(due > Instant::now() + Duration::from_secs(5));
        s.poll(due + Duration::from_millis(1));
        assert!(s.copies_deadline().is_none(), "given up");
        // A late answer is dropped.
        assert_eq!(s.clipboard_answer(ticket, Ok("late".into())), Outcome::None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
