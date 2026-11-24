//! A session: panes (PDF views and terminals), their layout and focus, the
//! prefix key, and the composed frame every attached client shows. No sockets
//! or terminals here: the standalone viewer, `--render-frame` and the session
//! server all drive a `Session` (see doc/design/session-server.md).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, bail};
use temzdf_pdfcore::doc_cache::Need;
use temzdf_pdfcore::workers::DocEvent;
use temzdf_pdfview::document::Document;
use temzdf_pdfview::host::{DocumentHost, Forward, Opening};
use temzdf_pdfview::pane::{self, PdfPane};
use temzdf_pdfview::viewer::{StandIn, ViewState};
use temzdf_scene::{CursorShape, Effect, Frame, Hints, Images, Layer, Pointer, Rect, Region, RegionKind, TextGrid};
use temzdf_term::{Spawn, TermBuffer, TermEvent, TermView};
use termpix_core::geom::Geometry;
use termpix_core::input::{KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::action::PaneSpec;
use crate::clip;
use crate::forward;
use crate::keymap::{Fallback, History, Keepalive, Layers, Matcher, Modes, Step, Stroke, Target, notation};
use crate::layout::{BorderId, Layout, PaneId, Split};
use temzdf_protocol::script::{FromHost, ScriptValue};

mod borders;
mod commands;
mod floats;
mod frame;
#[cfg(test)]
mod host_tests;
mod images;
mod input;
#[cfg(test)]
mod js_tests;
mod markdown;
mod notices;
mod options;
#[cfg(test)]
mod path_tests;
mod pdf;
mod registers;
mod script;
mod statusline;
mod swap;
mod tabs;
mod test_actions;
#[cfg(test)]
mod tests;
mod text;
#[cfg(test)]
mod vim_tests;

use frame::Keys;
use input::Invoked;
use pdf::DocEntry;

pub use commands::action_names;
pub use options::{Options, ShowTabline, TextOptions};

pub use script::ScriptSettings;
pub use test_actions::TestReport;

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
    Forward(DocId, Forward),
    Term(BufferId, TermEvent),
    /// A message from the script host (of this generation: hosts are
    /// replaced on reload, and an old one's last words must not count).
    Script(u32, FromHost),
    /// The script host of this generation ended.
    ScriptExited(u32, String),
    /// A text buffer's file changed (or may have: a watcher's event).
    TextFile(BufferId),
    /// A Markdown viewer's file changed (or may have).
    Markdown(u32),
    /// A Markdown viewer's remote image arrived: its URL and bytes, or why
    /// not.
    MarkdownImage(u32, String, std::result::Result<Vec<u8>, String>),
    /// A strip of a Markdown viewer's page was painted.
    MarkdownStrip(temzdf_mdview::Strip),
}

/// Where a session's events go (returns false once nobody listens).
pub type EventSink = temzdf_scene::sink::Sink<SessionEvent>;

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

/// The lowest z of programs' images in terminal panes: never under the cell
/// backgrounds (the GUI draws layers below -2^30 under them).
const Z_PROGRAMS_MIN: i32 = -1_073_741_698;
/// What a PDF pane says to frontends without images.
const NO_GRAPHICS_PDF: &str = "PDF viewer disabled (no kitty graphics protocol support)";
const NO_GRAPHICS_MARKDOWN: &str =
    "Markdown graphics mode disabled (no kitty graphics protocol support): markdown.setMode(\"text\")";
/// The session's own cells: borders, frames, the tab line.
const BACKGROUND: [u8; 4] = [0x3a, 0x3a, 0x3a, 0xff];
const BORDER: [u8; 4] = [0x5a, 0x5a, 0x5a, 0xff];
/// How deeply mappings may feed keys to mappings (Vim's `maxmapdepth`).
const MAX_MAP_DEPTH: usize = 1000;

/// What a pane shows and where, for a split of it to show the same.
enum SplitPlace {
    Pdf(ViewState),
    /// The source line at the top, and the graphics mode's zoom.
    Markdown {
        line: usize,
        graphics: bool,
        zoom: f32,
    },
    Text {
        cursor: temzdf_texteditor::Cursor,
        top: usize,
        top_skip: usize,
        left: usize,
    },
}

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

struct TermEntry {
    term: Box<TermBuffer>,
    /// The program ended; the screen stays until the buffer is deleted.
    exited: bool,
}

/// The kinds of buffers by name, as scripts see them (`buffers()`,
/// `panes()`, the events, a copy's source; see `Session::kind_buffer` and
/// `Session::buffer_info`), and the kinds of views by name, which base
/// modes go by (`Kind::mode_kind`).
mod kind_name {
    pub(super) const PDF: &str = "pdf";
    pub(super) const TERMINAL: &str = "terminal";
    pub(super) const EDIT: &str = "edit";
    pub(super) const MARKDOWN: &str = "markdown";
}

/// A terminal's name while its program has set no title.
const TERMINAL_NAME: &str = "shell";

enum Kind {
    Pdf {
        pane: Box<PdfPane>,
        doc: DocId,
    },
    Term {
        buf: BufferId,
        view: TermView,
    },
    Text {
        buf: BufferId,
        view: Box<text::TextView>,
    },
    /// A Markdown viewer in its graphics mode (its text mode is `Text`).
    Markdown {
        pane: Box<temzdf_mdview::MdPane>,
        doc: markdown::MdId,
    },
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
            Kind::Term { .. } | Kind::Text { .. } | Kind::Markdown { .. } => None,
        }
    }

    fn doc(&self) -> Option<DocId> {
        match &self.kind {
            Kind::Pdf { doc, .. } => Some(*doc),
            Kind::Term { .. } | Kind::Text { .. } | Kind::Markdown { .. } => None,
        }
    }

    fn term(&self) -> Option<BufferId> {
        match &self.kind {
            Kind::Term { buf, .. } => Some(*buf),
            Kind::Pdf { .. } | Kind::Text { .. } | Kind::Markdown { .. } => None,
        }
    }

    fn buffer(&self) -> BufferId {
        match &self.kind {
            Kind::Pdf { doc, .. } | Kind::Markdown { doc, .. } => *doc,
            Kind::Term { buf, .. } | Kind::Text { buf, .. } => *buf,
        }
    }

    /// See [`Kind::mode_kind`].
    fn mode_kind(&self) -> &'static str {
        self.kind.mode_kind()
    }
}

impl Kind {
    /// The view's own kind, which picks the pane's base mode: a
    /// Markdown viewer in its text mode is a text view here (`edit`).
    /// Scripts see its buffer's kind (`markdown`, `Session::kind_buffer`).
    fn mode_kind(&self) -> &'static str {
        match self {
            Kind::Pdf { .. } => kind_name::PDF,
            Kind::Term { .. } => kind_name::TERMINAL,
            Kind::Text { .. } => kind_name::EDIT,
            Kind::Markdown { .. } => kind_name::MARKDOWN,
        }
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
    /// Stamped, as `highlights` and `patterns`: text panes' frames are
    /// cached on their stamps (`frame::TextFrameKey`).
    loaded_images: temzdf_texteditor::Stamped<images::LoadedImages>,
    /// Markdown files shown in viewers, by id.
    md_docs: HashMap<markdown::MdId, markdown::MdDoc>,
    /// Where each viewer pane was before it followed a link to another
    /// file (`markdown.back`): the file and the source line at the top.
    md_history: HashMap<PaneId, Vec<(PathBuf, usize)>>,
    /// The threads painting Markdown viewers' strips (once one needs them).
    md_pool: Option<temzdf_scene::tiles::WorkPool<temzdf_mdview::StripJob>>,
    /// Where swap files go (o.swapfile; tests set their own).
    swap_dir: Option<PathBuf>,
    /// Highlight groups for text panes (scripts define them).
    highlights: temzdf_texteditor::Stamped<temzdf_texteditor::Highlights>,
    /// Buffer, pane and tab page variables (`getVar`, `setVar`).
    vars: HashMap<(VarOwner, String), ScriptValue>,
    /// `highlightPattern`: (namespace, pattern, style), painted in text panes.
    patterns: temzdf_texteditor::Stamped<Vec<(String, temzdf_texteditor::Pattern, String)>>,
    /// While a `temzdf action` request waits for the scripts it started:
    /// why a "before" hook cancelled what it asked for, if one did.
    cli_cancel: Option<Option<String>>,
    /// The session ends because it was killed (`kill-session`).
    killed: bool,
    /// Invocations (doc/design/text-editor.md section 9): the last one's
    /// number, and the one under way (its edits name it in `TextChanged`).
    inv_seq: u64,
    pub(super) current_invocation: Option<u64>,
    /// The keys of the last invocation started (mapped and fallback
    /// functions get them as `ctx.keys`).
    invocation_keys: String,
    /// Counts invocations (keys, text, pastes, CLI requests): a text
    /// buffer's undo step ends when a new one edits it.
    invocation: u64,
    /// What a new shell pane runs; `None`: `$SHELL` (`%COMSPEC%` on Windows).
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
    /// The latest time input or events came with: what commands from the
    /// script host run at (a headless render's clock is simulated).
    clock: Instant,
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
    /// How deeply mappings fed the keys of what runs now (0: typed); keys
    /// it feeds are one deeper (Vim's `maxmapdepth`).
    map_depth: usize,
    keys: Keys,
    /// Each text pane's last frame and what it was drawn from.
    text_frames: HashMap<PaneId, (frame::TextFrameKey, Frame)>,
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
    /// The lines text panes showed when `PaneScrolled` last looked (none
    /// while nothing hooks it).
    scrolled: Option<HashMap<PaneId, text::Shown>>,
    /// Scripts' tests: their waits and reports (`test.*`).
    test: test_actions::TestState,
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
            loaded_images: Default::default(),
            md_docs: HashMap::new(),
            md_history: HashMap::new(),
            md_pool: None,
            // (Unit tests set theirs: none in the user's state directory.)
            swap_dir: if cfg!(test) { None } else { swap::swap_dir() },
            highlights: Default::default(),
            patterns: Default::default(),
            vars: HashMap::new(),
            invocation: 0,
            cli_cancel: None,
            killed: false,
            inv_seq: 0,
            current_invocation: None,
            invocation_keys: String::new(),
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
            clock: Instant::now(),
            options: Options::default(),
            configured_fonts: None,
            notices: Vec::new(),
            escape_prefix: false,
            visual: None,
            map_depth: 0,
            keys: Keys::default(),
            text_frames: HashMap::new(),
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
            scrolled: None,
            test: test_actions::TestState::default(),
        }
    }

    /// What new shell panes run (`None`: `$SHELL`, `%COMSPEC%` on Windows, the
    /// default).
    pub fn set_shell(&mut self, argv: Option<Vec<String>>) {
        self.shell = argv;
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
        } else {
            Spot::Split { target: Some(self.focus), split: Split::Horizontal, before: false, size: None }
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

    /// Show buffer `buf` at `spot`: in pane `p` instead of what it shows
    /// for `Spot::Replace(p)` (see `show_in`), else in a new pane, focused
    /// as `takes_focus` says. Returns the pane; a document still opening
    /// has none yet (`None`: the spot waits with it). A Markdown document
    /// shows in the viewer (`md_open_doc`).
    fn add_pane(&mut self, buf: BufferId, spot: Spot) -> Result<Option<PaneId>, String> {
        if let Spot::Replace(p) = spot {
            self.show_in(p, buf)?;
            return Ok(Some(p));
        }
        if self.md_docs.contains_key(&buf) {
            return self.md_open_doc(buf, spot).map(Some);
        }
        if self.docs.get(&buf).is_some_and(|d| d.host.is_none()) {
            let spot = self.pin_spot(spot);
            self.docs.get_mut(&buf).expect("found").waiting.push(spot);
            return Ok(None);
        }
        if !self.terms.contains_key(&buf) && !self.texts.contains_key(&buf) && !self.docs.contains_key(&buf) {
            return Err(format!("no buffer {buf}"));
        }
        let focus = self.takes_focus(&spot);
        self.open_pane(buf, spot, focus).map(Some)
    }

    /// A new pane at `spot` (not `Spot::Replace`) showing buffer `buf`
    /// (a terminal, a text or an open document), focused if `focus`.
    fn open_pane(&mut self, buf: BufferId, spot: Spot, focus: bool) -> Result<PaneId, String> {
        let id = self.alloc_id();
        let rect = self.place(id, spot).map_err(|e| format!("{e:#}"))?;
        self.insert_pane(id, rect, buf, focus)?;
        Ok(id)
    }

    /// Pane `id`, placed at `rect`, shows buffer `buf`: hooks hear
    /// `PaneOpened` (then `PaneFocused` if `focus`). A document no other
    /// pane shows is shown the way its last pane did.
    fn insert_pane(&mut self, id: PaneId, rect: Rect, buf: BufferId, focus: bool) -> Result<(), String> {
        // The size its pane gives it (a status line shows by default).
        let g = content_geometry(content_rect(rect, true), self.geometry.cell, None);
        let kind = match self.kind_for(buf, id, g) {
            Ok(kind) => kind,
            Err(e) => {
                self.unplace(id);
                return Err(e);
            }
        };
        let restore = matches!(kind, Kind::Pdf { .. }) && !self.shown(buf);
        let name = self.kind_buffer(&kind).1;
        self.panes.insert(id, PaneEntry::new(kind, rect));
        self.event("PaneOpened", vec![ScriptValue::Num(id as f64), ScriptValue::Str(name.into())]);
        if focus {
            self.set_focus(id);
        }
        self.relayout();
        if restore {
            self.restore_view(id);
        }
        Ok(())
    }

    /// The view pane `pane` starts with on buffer `buf`, its content `g`
    /// large. A text view takes its size from the pane when laid out
    /// (`fit_text_view`); a page view starts at the size of the whole
    /// area and the layout resizes it right after. A Markdown document
    /// is shown through `md_open_doc` (a drawing, or the graphics mode),
    /// and a document still opening has no view yet.
    fn kind_for(&mut self, buf: BufferId, pane: PaneId, g: Geometry) -> Result<Kind, String> {
        if self.terms.contains_key(&buf) {
            return Ok(Kind::Term { buf, view: TermView::new(g) });
        }
        if self.texts.contains_key(&buf) {
            return Ok(Kind::Text { buf, view: Box::new(self.text_view_for(buf, pane)) });
        }
        let geometry = content_geometry(content_rect(self.area(), true), self.geometry.cell, None);
        let Some(entry) = self.docs.get_mut(&buf) else { return Err(format!("no buffer {buf}")) };
        let host = entry.host.as_mut().ok_or("the document is not open")?;
        let mut view = PdfPane::with_document(entry.name.clone(), host.info.clone(), host.doc.clone(), geometry);
        view.preview_delay = self.options.preview_delay();
        view.flash_length = self.options.forwardflash;
        view.anchor_by_text = self.options.reloadanchor_text;
        view.viewer.viewport.set_config(self.options.scroll());
        view.viewer.stand_in_mode = StandIn::for_hints(self.hints);
        view.set_search_base((pane as u64) << 32);
        if let Some(e) = host.watcher_error.take() {
            view.message = Some(e);
        }
        Ok(Kind::Pdf { pane: Box::new(view), doc: buf })
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

    /// The buffer `kind` shows as scripts see it, and the buffer's kind: a
    /// Markdown viewer's is its document in either mode (its drawings are
    /// internal).
    fn kind_buffer(&self, kind: &Kind) -> (BufferId, &'static str) {
        match kind {
            Kind::Pdf { doc, .. } => (*doc, kind_name::PDF),
            Kind::Markdown { doc, .. } => (*doc, kind_name::MARKDOWN),
            Kind::Term { buf, .. } => (*buf, kind_name::TERMINAL),
            Kind::Text { buf, .. } => match self.texts.get(buf).and_then(|t| t.markdown) {
                Some(md) => (md, kind_name::MARKDOWN),
                None => (*buf, kind_name::EDIT),
            },
        }
    }

    /// Buffer `buf`'s kind, as scripts see it, and its name: the
    /// document's file name, the terminal's title, the text's name (an
    /// input's prompt). A Markdown viewer's drawing answers for its
    /// document (scripts see that).
    fn buffer_info(&self, buf: BufferId) -> Option<(&'static str, String)> {
        if let Some(t) = self.terms.get(&buf) {
            return Some((kind_name::TERMINAL, t.term.title().unwrap_or_else(|| TERMINAL_NAME.into())));
        }
        if let Some(t) = self.texts.get(&buf) {
            return match t.markdown {
                Some(md) => self.buffer_info(md),
                None => Some((kind_name::EDIT, t.name.clone())),
            };
        }
        if let Some(d) = self.md_docs.get(&buf) {
            return Some((kind_name::MARKDOWN, d.name.clone()));
        }
        self.docs.get(&buf).map(|d| (kind_name::PDF, d.name.clone()))
    }

    /// Buffer `buf` was made: hooks hear `BufferCreated` with its kind.
    fn buffer_created(&mut self, buf: BufferId) {
        let kind = self.buffer_info(buf).map_or("", |(kind, _)| kind);
        self.event("BufferCreated", vec![ScriptValue::Num(buf as f64), ScriptValue::Str(kind.into())]);
    }

    /// The buffer pane `pane` shows, as scripts see it (`kind_buffer`).
    fn pane_buffer(&self, pane: PaneId) -> Option<BufferId> {
        self.panes.get(&pane).map(|e| self.kind_buffer(&e.kind).0)
    }

    /// Whether a pane shows buffer `buf`.
    fn shown(&self, buf: BufferId) -> bool {
        self.panes.values().any(|e| self.kind_buffer(&e.kind).0 == buf)
    }

    /// The panes showing `buf`.
    fn panes_showing(&self, buf: BufferId) -> Vec<PaneId> {
        let mut v: Vec<PaneId> =
            self.panes.iter().filter(|(_, e)| self.kind_buffer(&e.kind).0 == buf).map(|(id, _)| *id).collect();
        v.sort();
        v
    }

    /// Show buffer `buf` in pane `pane` instead of what it shows (Vim's
    /// `:buffer`). A document still opening shows once it opened; a
    /// Markdown document in the viewer at the pane's width.
    fn show_in(&mut self, pane: PaneId, buf: BufferId) -> Result<Outcome, String> {
        let Some(e) = self.panes.get(&pane) else { return Err(format!("no pane {pane}")) };
        if e.buffer() == buf || self.kind_buffer(&e.kind).0 == buf {
            return Ok(Outcome::None);
        }
        if self.md_docs.contains_key(&buf) {
            self.md_open_doc(buf, Spot::Replace(pane))?;
            return Ok(Outcome::Redraw);
        }
        let g = content_geometry(e.content(), self.geometry.cell, None);
        if let Some(d) = self.docs.get_mut(&buf)
            && d.host.is_none()
        {
            d.waiting.push((Spot::Replace(pane), self.tabs[self.tab].id));
            return Ok(Outcome::None);
        }
        // A terminal shown again shows its end.
        if let Some(t) = self.terms.get_mut(&buf) {
            t.term.scroll_to_bottom();
        }
        let kind = self.kind_for(buf, pane, g)?;
        let restore = matches!(kind, Kind::Pdf { .. }) && !self.shown(buf);
        let focused = pane == self.focus;
        if focused {
            self.term_focus(pane, false);
        }
        let old = std::mem::replace(&mut self.panes.get_mut(&pane).expect("checked").kind, kind);
        self.hide(old);
        self.relayout();
        if restore {
            self.restore_view(pane);
        }
        if focused {
            self.term_focus(pane, true);
            self.reset_mode(pane);
        }
        Ok(Outcome::Redraw)
    }

    /// What a pane showed is no longer shown there. A buffer no pane shows
    /// stays, hidden (`BufferHidden`): a document keeps its position (and
    /// drops its tiles), a terminal keeps running; an ended terminal goes.
    fn hide(&mut self, kind: Kind) {
        let (buf, name) = self.kind_buffer(&kind);
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
                    if let Some(h) = &mut d.host {
                        h.forget(&mut self.images);
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
                // A viewer's drawing no pane shows any more goes; its
                // document stays, at the line its last pane showed.
                if let Some(md) = self.texts.get(&buf).and_then(|t| t.markdown) {
                    self.md_hidden(md, &kind);
                    self.md_collect(md);
                }
            }
            Kind::Markdown { doc, .. } => {
                self.md_hidden(doc, &kind);
                if let Kind::Markdown { mut pane, .. } = kind {
                    pane.release(&mut self.images);
                }
                self.md_collect(doc);
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

    /// Whether `buf` is a buffer scripts know (a Markdown viewer's
    /// drawings are not: its document is).
    fn has_buffer(&self, buf: BufferId) -> bool {
        self.terms.contains_key(&buf)
            || self.docs.contains_key(&buf)
            || self.texts.get(&buf).is_some_and(|t| t.markdown.is_none())
            || self.md_docs.contains_key(&buf)
    }

    /// Delete a buffer: end its program or close its document, and close the
    /// panes showing it (the last pane: the session ends; a Markdown
    /// document's in either mode). `PaneClosePre`
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

    /// Forget a buffer no pane shows. A Markdown viewer's drawing goes
    /// without `BufferDeleted` (scripts never saw it), its document with.
    fn remove_buffer(&mut self, buf: BufferId) {
        if self.md_docs.contains_key(&buf) {
            self.md_remove(buf);
            return;
        }
        if let Some(d) = self.docs.remove(&buf) {
            if let Some(host) = d.host {
                host.doc.borrow_mut().release(&mut self.images);
            }
        } else if let Some(mut t) = self.terms.remove(&buf) {
            t.term.release(&mut self.images);
        } else {
            self.drop_swap(buf);
            match self.texts.remove(&buf) {
                None => return,
                Some(t) if t.markdown.is_some() => return,
                Some(_) => {}
            }
        }
        self.event("BufferDeleted", vec![ScriptValue::Num(buf as f64)]);
    }

    /// The buffers, for `buffers()`: `unlisted` ones only with `all`, a
    /// Markdown viewer's drawings never (its document is listed).
    fn buffer_list(&self, all: bool) -> ScriptValue {
        let mut ids: Vec<BufferId> = self
            .docs
            .iter()
            .filter(|(_, d)| d.host.is_some())
            .map(|(id, _)| *id)
            .chain(self.md_docs.keys().copied())
            .chain(self.terms.keys().copied())
            .chain(
                self.texts
                    .iter()
                    .filter(|(_, t)| t.markdown.is_none() && (all || !t.has("unlisted")))
                    .map(|(id, _)| *id),
            )
            .collect();
        ids.sort();
        ScriptValue::List(
            ids.into_iter()
                .filter_map(|id| {
                    let (kind, name) = self.buffer_info(id)?;
                    let exited = self.terms.get(&id).is_some_and(|t| t.exited);
                    let panes = self.panes_showing(id).into_iter().map(|p| ScriptValue::Num(p as f64)).collect();
                    Some(ScriptValue::map([
                        ("id", ScriptValue::Num(id as f64)),
                        ("kind", ScriptValue::Str(kind.into())),
                        ("name", ScriptValue::Str(name)),
                        ("panes", ScriptValue::List(panes)),
                        ("exited", ScriptValue::Bool(exited)),
                        ("modified", ScriptValue::Bool(self.texts.get(&id).is_some_and(|t| t.modified()))),
                        ("attrs", self.texts.get(&id).map_or(ScriptValue::List(Vec::new()), |t| t.attrs_value())),
                    ]))
                })
                .collect(),
        )
    }

    /// The number of buffers (for `temzdf ls`).
    pub fn buffer_count(&self) -> usize {
        self.docs.values().filter(|d| d.host.is_some()).count()
            + self.md_docs.len()
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
        self.buffer_created(buf);
        self.insert_pane(id, rect, buf, focus).map_err(anyhow::Error::msg)?;
        Ok(id)
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
            self.term_focus(p, gained);
        }
        self.focus = id;
        self.event("PaneFocused", vec![ScriptValue::Num(id as f64)]);
        // The history starts again from the pane's own mode.
        self.reset_mode(id);
    }

    /// An error message, in a floating notice until the next key.
    fn message(&mut self, m: &str) {
        self.notify(notices::Kind::Error, m);
    }

    /// A pane's search found nothing: every kind says so alike.
    fn not_found(&mut self, query: &str) {
        self.message(&format!("Pattern not found: {query}"));
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
                Kind::Text { .. } | Kind::Markdown { .. } => {}
            }
        }
        self.fit_text_view(id);
        self.md_fit_graphics(id);
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
        self.clock = self.clock.max(now);
        // (A script's answer lets queued keys go on, past a prefix's
        // timeout when it ran long.)
        self.test_hold_timeout(now);
        let out = match ev {
            SessionEvent::Script(generation, msg) => {
                return if generation == self.script.generation { self.handle_script(msg) } else { Outcome::None };
            }
            SessionEvent::ScriptExited(generation, why) => {
                return if generation == self.script.generation { self.script_exited(&why) } else { Outcome::None };
            }
            ev => self.handle_inner(ev, now),
        };
        out.merge(self.flush_events())
    }

    fn handle_inner(&mut self, ev: SessionEvent, now: Instant) -> Outcome {
        match ev {
            SessionEvent::Script(..) | SessionEvent::ScriptExited(..) => Outcome::None,
            SessionEvent::TextFile(buf) => {
                self.text_file_changed(buf);
                Outcome::None
            }
            SessionEvent::Markdown(md) => self.md_file_changed(md),
            SessionEvent::MarkdownImage(md, url, got) => self.md_image_fetched(md, url, got),
            SessionEvent::MarkdownStrip(s) => self.md_strip_painted(s),
            SessionEvent::Doc(id, DocEvent::Opened(result)) => {
                let Some(entry) = self.docs.get_mut(&id) else { return Outcome::None };
                let Some(opening) = entry.opening.take() else { return Outcome::None };
                match result {
                    Ok(info) if !info.bounds.is_empty() => {
                        let doc = Document::shared(info.bounds.clone());
                        entry.host = Some(opening.finish(info, doc, self.options.host()));
                        self.buffer_created(id);
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
                            && let Some(top) = self.floats.iter().rev().find(|f| f.focusable).map(|f| f.pane)
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
                    host.forward(req, pane);
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

    /// Options changed: panes and documents follow.
    fn apply_options(&mut self) {
        let (scroll, preview, anchor, host) =
            (self.options.scroll(), self.options.preview_delay(), self.options.reloadanchor_text, self.options.host());
        let flash = self.options.forwardflash;
        for e in self.panes.values_mut() {
            match &mut e.kind {
                Kind::Pdf { pane: p, .. } => {
                    p.viewer.viewport.set_config(scroll);
                    p.preview_delay = preview;
                    p.flash_length = flash;
                    p.anchor_by_text = anchor;
                }
                Kind::Markdown { pane, .. } => pane.set_scroll_config(scroll),
                Kind::Term { .. } | Kind::Text { .. } => {}
            }
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
            glyph_overflow: o.guiglyphoverflow,
        }
    }

    /// The current mode's name.
    #[cfg(test)]
    pub fn current_mode(&self) -> &str {
        self.history.current()
    }

    /// Split `target` (the whole layout for `None`): the new pane shows
    /// `buf`, or what `target` shows where it shows it (the same document
    /// again, a Markdown viewer in the same mode, a text buffer with the
    /// same cursor and scroll, as Vim's `:split`; a new shell where it is);
    /// `size` cells wide (or high) and fixed when given. From a float: a new
    /// float instead. Returns the new pane (`None` for a document still
    /// opening).
    pub(super) fn split_pane(
        &mut self,
        target: Option<PaneId>,
        split: Split,
        before: bool,
        size: Option<u32>,
        buf: Option<BufferId>,
    ) -> Result<Option<PaneId>, String> {
        let from = target.unwrap_or(self.focus);
        let float = self.is_float(from);
        let spot = if float { Spot::Float(self.cascade()) } else { Spot::Split { target, split, before, size } };
        // Where `from` is, taken before the split resizes it.
        let place = if buf.is_some() { None } else { self.split_place(from) };
        // A graphics-mode viewer opens as its text mode (and goes on in
        // graphics below, but in a float).
        let md_text = match self.panes.get(&from).map(|e| &e.kind) {
            Some(Kind::Markdown { doc, .. }) => Some(*doc),
            _ => None,
        }
        .map(|doc| self.md_buffer(doc, 80));
        let what = match (buf.or(md_text), self.panes.get(&from).map(|e| &e.kind)) {
            (Some(b), _) => floats::FloatWhat::Buffer(b),
            // The same document or input again.
            (None, Some(Kind::Pdf { doc: b, .. } | Kind::Text { buf: b, .. } | Kind::Markdown { doc: b, .. })) => {
                floats::FloatWhat::Buffer(*b)
            }
            // A new shell where the focused one is.
            (None, Some(Kind::Term { buf, .. })) => {
                let cwd = self.terms.get(buf).and_then(|t| t.term.pwd()).filter(|d| d.is_dir());
                floats::FloatWhat::Spawn(Spawn { argv: None, cwd, env: Vec::new() })
            }
            (None, None) => return Err(format!("no pane {from}")),
        };
        let new = self.open_what(what, spot)?;
        if let (Some(new), Some(place)) = (new, place) {
            self.split_show(new, place, float)?;
        }
        Ok(new)
    }

    /// What pane `from` shows and where, for a split of it to show the same.
    fn split_place(&self, from: PaneId) -> Option<SplitPlace> {
        Some(match &self.panes.get(&from)?.kind {
            Kind::Pdf { pane, .. } => SplitPlace::Pdf(pane.viewer.view_state()),
            Kind::Markdown { pane, .. } => {
                SplitPlace::Markdown { line: self.md_top_line(from)?, graphics: true, zoom: pane.zoom }
            }
            Kind::Text { buf, .. } if self.texts.get(buf)?.markdown.is_some() => {
                SplitPlace::Markdown { line: self.md_top_line(from)?, graphics: false, zoom: 1.0 }
            }
            Kind::Text { view, .. } => SplitPlace::Text {
                cursor: temzdf_texteditor::Cursor { anchor: None, ..view.view.cursor },
                top: view.view.top,
                top_skip: view.view.top_skip,
                left: view.view.left,
            },
            Kind::Term { .. } => return None,
        })
    }

    /// Show `place` in pane `new`, just split (a graphics viewer goes on in
    /// text mode in a float).
    fn split_show(&mut self, new: PaneId, place: SplitPlace, float: bool) -> Result<(), String> {
        match place {
            SplitPlace::Pdf(state) => {
                if let Some(Kind::Pdf { pane, .. }) = self.panes.get_mut(&new).map(|e| &mut e.kind) {
                    pane.viewer.restore_same_scale(&state);
                }
            }
            SplitPlace::Markdown { line, graphics, zoom } if graphics && !float => {
                self.md_set_mode(new, "graphics")?;
                // The same zoom: a page laid out for it, then the line on it.
                if let Some(Kind::Markdown { pane, doc }) = self.panes.get_mut(&new).map(|e| &mut e.kind) {
                    pane.zoom = zoom;
                    let doc = *doc;
                    if let Some((p, page)) = self.md_graphics(new) {
                        p.show_line(page, line);
                    }
                    self.md_collect(doc);
                }
            }
            SplitPlace::Markdown { line, .. } => self.md_place_text(new, line),
            SplitPlace::Text { cursor, top, top_skip, left } => {
                if let Some(Kind::Text { buf, view }) = self.panes.get_mut(&new).map(|e| &mut e.kind)
                    && let Some(t) = self.texts.get(buf)
                {
                    let v = &mut view.view;
                    (v.cursor, v.top, v.top_skip, v.left) = (cursor, top, top_skip, left);
                    v.relayout(&t.buf);
                }
            }
        }
        Ok(())
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
            self.focusable_floats().last().map_or(self.tiled_focus, |f| f.pane)
        } else {
            let own = tabs::FloatTab::Only(self.tab_id());
            let top = self.focusable_floats().into_iter().rev().find(|f| f.tab == own).map(|f| f.pane);
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

    /// Effects for the frontend, including requests panes made since the last
    /// call (call every loop iteration: inverse search does not redraw).
    pub fn take_effects(&mut self) -> Vec<Effect> {
        let _ = self.collect_pane_requests();
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
            Some(Kind::Markdown { pane, .. }) => pane.pointer(),
            Some(Kind::Term { buf, .. }) if self.terms.get(buf).is_some_and(|t| t.term.hovering_link()) => {
                Pointer::Link
            }
            _ => Pointer::Default,
        }
    }

    /// Whether every pane showed its visible pages fully rendered last frame.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Nothing is being opened, loaded, rendered or searched, and no script
    /// runs (keys wait for it).
    pub fn is_idle(&self) -> bool {
        !self.script.busy()
            && self
                .docs
                .values()
                .all(|d| d.host.as_ref().is_some_and(|h| h.doc.borrow().cache.is_idle() && h.synctex_idle()))
            && self.md_docs.values().all(|d| d.strips.is_idle())
            && self.panes.values().all(|e| match &e.kind {
                Kind::Pdf { pane, .. } => !pane.search.is_active() || pane.search.is_complete(),
                Kind::Term { .. } | Kind::Text { .. } | Kind::Markdown { .. } => true,
            })
    }

    pub fn is_animating(&self) -> bool {
        self.panes.values().any(|e| match &e.kind {
            Kind::Pdf { pane, .. } => pane.is_animating(),
            Kind::Markdown { pane, .. } => pane.is_animating(),
            Kind::Term { .. } | Kind::Text { .. } => false,
        })
    }

    /// Advance animations and timers. Returns whether to redraw.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.clock = self.clock.max(now);
        let mut changed = false;
        for e in self.panes.values_mut() {
            match &mut e.kind {
                Kind::Pdf { pane: p, .. } => {
                    changed |= p.tick(now);
                    // A highlight expired.
                    changed |= p.next_deadline().is_some_and(|d| now >= d);
                }
                Kind::Markdown { pane, doc } => {
                    let page = self.md_docs.get(doc).and_then(|d| d.page(pane.page_key()));
                    changed |= pane.tick(page, now);
                }
                Kind::Term { .. } | Kind::Text { .. } => {}
            }
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
            // A program's synchronized update left on: its screen shows.
            changed |= t.term.sync_timeout(now);
        }
        changed
    }

    /// [`Session::tick`], plus scripting's timers (the configuration's load
    /// timeout, the "Script is running" notice). For event loops.
    pub fn poll(&mut self, now: Instant) -> Outcome {
        self.clock = self.clock.max(now);
        self.test_hold_timeout(now);
        let expired = self.expire_clipboard_reads(now);
        self.write_due_swaps(now);
        let mut out = self
            .script_tick(now)
            .or(self.tick(now))
            .or(self.expire_notices(now))
            .or(self.expire_decorations(now))
            .merge(expired);
        if self.matcher.deadline(self.options.timeoutlen).is_some_and(|d| now >= d) {
            out = out.merge(self.drive(now));
        }
        // A submode left alone for its timeout goes.
        if self.submode_deadline().is_some_and(|d| now >= d) && !self.holds_input() {
            self.leave_mode();
            self.last_key = now;
            out = out.merge(Outcome::Redraw);
        }
        // What panes copied in the events just handled, or their searches
        // not finding anything: the notice now, not after the next frame
        // (take_effects comes after it).
        out = out.merge(self.collect_pane_requests());
        out = out.merge(self.flush_events());
        // Waits of scripts' tests, once all of the above is done.
        out.merge(self.test_waits(now))
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let panes = self.panes.values().filter_map(|e| match &e.kind {
            Kind::Pdf { pane, .. } => pane.next_deadline(),
            Kind::Term { .. } | Kind::Text { .. } | Kind::Markdown { .. } => None,
        });
        let docs = self.docs.values().filter_map(|d| d.host.as_ref()?.next_deadline());
        let terms = self.terms.values().flat_map(|t| [t.term.autoscroll_deadline(), t.term.sync_deadline()]).flatten();
        panes
            .chain(docs)
            .chain(terms)
            .chain(self.script_deadline())
            .chain(self.decorations_deadline())
            .chain(self.copies_deadline())
            .chain(self.swap_deadline())
            // While a script runs or a paste is waited for, these wait for
            // it (after_scripts, the answer and the next poll see to them): a
            // deadline past meanwhile would spin the event loop.
            .chain(self.matcher.deadline(self.options.timeoutlen).filter(|_| !self.holds_input()))
            .chain(self.submode_deadline().filter(|_| !self.holds_input()))
            .chain(self.notice_deadline())
            .chain(self.test_deadline())
            .min()
    }

    pub fn focus(&self) -> PaneId {
        self.focus
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// The focused pane when it shows a PDF (for options applied after
    /// start-up, e.g. `--page`).
    pub fn focused_pane(&mut self) -> Option<&mut PdfPane> {
        self.panes.get_mut(&self.focus).and_then(PaneEntry::pdf)
    }
}

/// Who a variable belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum VarOwner {
    Buffer(BufferId),
    Pane(PaneId),
    Tab(tabs::TabId),
}

/// The built-in modes, without mappings (the shipped JavaScript defaults,
/// `crates/temzdf-script/runtime/defaults.ts`, map them): `normal`
/// (operating), `terminal` (pass-through), `pdf`, `input` and `edit`
/// (pass-through: typed characters go in) and `visual` (a terminal's visual mode, entered
/// from the others; see [`Session::mode_changed`]).
fn builtin_modes() -> Modes<Target> {
    let mut m = Modes::default();
    for (name, fallback, count) in [
        ("normal", Fallback::Drop, true),
        ("terminal", Fallback::Passthrough, false),
        ("pdf", Fallback::Drop, true),
        ("markdown", Fallback::Drop, true),
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
