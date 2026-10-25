//! A session: panes on documents, their layout and focus, the prefix key, and
//! the composed frame every attached client shows. No sockets or terminals
//! here: the standalone viewer, `--render-frame` and the session server all
//! drive a `Session` (see doc/session-server.md).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use temzdf_pdfcore::doc_cache::Need;
use temzdf_pdfcore::workers::DocEvent;
use temzdf_pdfview::document::Document;
use temzdf_pdfview::host::{DocumentHost, Forward, HostOptions, Opening};
use temzdf_pdfview::keys::{Key, Keymap};
use temzdf_pdfview::pane::{self, PdfPane};
use temzdf_pdfview::viewer::StandIn;
use temzdf_scene::{CursorShape, Effect, Frame, Hints, ImageKey, Images, Layer, Pointer, Rect, TextGrid};
use termpix_core::geom::Geometry;
use termpix_core::input::{CoordUnit, KeyCode, KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};
use termpix_core::scroll::ScrollConfig;

use crate::action::Action;
use crate::forward::{self, ForwardRequest};
use crate::layout::{Direction, Layout, PaneId, Split};

pub type DocId = u32;

/// Events for the session from its documents' threads and from other
/// processes (forward search).
pub enum SessionEvent {
    Doc(DocId, DocEvent),
    Forward(DocId, ForwardRequest),
}

/// Where a session's events go (returns false once nobody listens).
pub type EventSink = Arc<dyn Fn(SessionEvent) -> bool + Send + Sync>;

/// How new panes and documents are set up (from the configuration).
#[derive(Clone)]
pub struct Settings {
    pub keymap: Keymap,
    pub preview_delay: Option<Duration>,
    pub anchor_by_text: bool,
    pub scroll: ScrollConfig,
    pub host: HostOptions,
    /// Listen for `temzdf --forward` on each document's socket (not for
    /// headless rendering, which must not take a running viewer's socket).
    pub serve_forward: bool,
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
}

/// The prefix key for session commands (fixed for now).
const PREFIX: &str = "C-s";
/// Session layers: the background below every pane, borders above them.
const Z_SESSION_BACKGROUND: i32 = -1_073_741_950;
const Z_BORDER: i32 = -1_073_741_700;
const Z_BORDER_FOCUS: i32 = -1_073_741_699;
const BACKGROUND: [u8; 4] = [0x3a, 0x3a, 0x3a, 0xff];
const BORDER: [u8; 4] = [0x5a, 0x5a, 0x5a, 0xff];
/// Border line thickness in pixels (centred in the one-cell gap).
const BORDER_PX: u32 = 2;
/// Layer key owner for the session's own layers.
const OWNER_SESSION: u32 = u32::MAX;

/// Where a pane goes once its document opened.
#[derive(Clone, Copy, Debug)]
enum Spot {
    /// The first pane of the session.
    First,
    /// Beside or below this pane.
    SplitOf(PaneId, Split),
}

struct DocEntry {
    path: PathBuf,
    name: String,
    host: Option<DocumentHost>,
    opening: Option<Opening>,
    /// Panes to create once it opened.
    waiting: Vec<Spot>,
    _forward: Option<forward::Server>,
}

struct PaneEntry {
    pane: PdfPane,
    doc: DocId,
    /// In cells.
    rect: Rect,
}

/// Placement keys of the panes' layers, made unique across panes and kept
/// stable from frame to frame (so the compositor moves rather than re-creates).
#[derive(Default)]
struct Keys {
    map: HashMap<(u32, u32), u32>,
    next: u32,
}

impl Keys {
    fn get(&mut self, owner: u32, key: u32) -> u32 {
        let next = &mut self.next;
        *self.map.entry((owner, key)).or_insert_with(|| {
            *next = next.wrapping_add(1).max(1);
            *next
        })
    }

    fn retain(&mut self, used: &HashSet<(u32, u32)>) {
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
    panes: HashMap<PaneId, PaneEntry>,
    layout: Option<Layout>,
    focus: PaneId,
    next_id: u32,
    prefix: bool,
    keys: Keys,
    effects: Vec<Effect>,
    /// The pane under the pointer, and the one holding a pressed button.
    hovered: Option<PaneId>,
    captured: Option<PaneId>,
    /// Solid images (background, borders) by size and colour.
    solids: HashMap<(u32, u32, [u8; 4]), ImageKey>,
    complete: bool,
    /// Why the first document could not be opened.
    error: Option<String>,
}

impl Session {
    pub fn new(name: String, geometry: Geometry, hints: Hints, settings: Settings, events: EventSink) -> Self {
        Self {
            name,
            images: Images::new(),
            geometry,
            hints,
            settings,
            events,
            docs: HashMap::new(),
            panes: HashMap::new(),
            layout: None,
            focus: 0,
            next_id: 1,
            prefix: false,
            keys: Keys::default(),
            effects: Vec::new(),
            hovered: None,
            captured: None,
            solids: HashMap::new(),
            complete: false,
            error: None,
        }
    }

    pub fn images(&mut self) -> &mut Images {
        &mut self.images
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Why the session's first document failed to open, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// No pane yet (the first document is still opening).
    pub fn is_starting(&self) -> bool {
        self.layout.is_none() && self.error.is_none()
    }

    /// Open `path` in a new pane: the first one, or beside the focused pane.
    /// A file already open is shared, not opened again.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        let spot = if self.layout.is_some() { Spot::SplitOf(self.focus, Split::Horizontal) } else { Spot::First };
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        if let Some((&id, entry)) = self.docs.iter_mut().find(|(_, d)| d.path == canonical) {
            if entry.host.is_some() {
                self.add_pane(id, spot);
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

    /// Create a pane on an open document.
    fn add_pane(&mut self, doc: DocId, spot: Spot) -> Option<PaneId> {
        let id = self.alloc_id();
        let area = self.area();
        match (spot, self.layout.as_mut()) {
            (Spot::First, None) => self.layout = Some(Layout::new(id)),
            (Spot::SplitOf(target, split), Some(layout)) if self.panes.contains_key(&target) => {
                if !layout.split(target, id, split, area) {
                    self.message("no room for another pane");
                    return None;
                }
            }
            // The target went away meanwhile: split the focused pane instead.
            (_, Some(layout)) => {
                if !layout.split(self.focus, id, Split::Horizontal, area) {
                    self.message("no room for another pane");
                    return None;
                }
            }
            (Spot::SplitOf(..), None) => self.layout = Some(Layout::new(id)),
        }
        let entry = self.docs.get_mut(&doc)?;
        let host = entry.host.as_mut()?;
        let mut pane = PdfPane::with_document(entry.name.clone(), host.info.clone(), host.doc.clone(), self.geometry);
        pane.keymap = self.settings.keymap.clone();
        pane.preview_delay = self.settings.preview_delay;
        pane.anchor_by_text = self.settings.anchor_by_text;
        pane.viewer.scroll_x.config = self.settings.scroll;
        pane.viewer.scroll_y.config = self.settings.scroll;
        pane.viewer.stand_in_mode = StandIn::for_hints(self.hints);
        pane.set_search_base((id as u64) << 32);
        if let Some(e) = host.watcher_error.take() {
            pane.message = Some(e);
        }
        self.panes.insert(id, PaneEntry { pane, doc, rect: area });
        self.focus = id;
        self.relayout();
        Some(id)
    }

    fn message(&mut self, m: &str) {
        if let Some(e) = self.panes.get_mut(&self.focus) {
            e.pane.message = Some(m.into());
        }
    }

    /// Give every pane its rectangle (after the layout or the screen changed).
    fn relayout(&mut self) {
        let Some(layout) = &self.layout else { return };
        let cell = self.geometry.cell;
        for (id, r) in layout.rects(self.area()) {
            if let Some(e) = self.panes.get_mut(&id) {
                e.rect = r;
                let g = Geometry { cols: r.w as u16, rows: r.h as u16, cell };
                if g != e.pane.geometry() {
                    e.pane.resize(g);
                }
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
        for e in self.panes.values_mut() {
            e.pane.viewer.stand_in_mode = StandIn::for_hints(hints);
        }
    }

    /// The panes showing `doc`, the focused one first (it receives SyncTeX results).
    fn panes_of(panes: &mut HashMap<PaneId, PaneEntry>, doc: DocId, focus: PaneId) -> Vec<&mut PdfPane> {
        let mut v: Vec<(PaneId, &mut PdfPane)> =
            panes.iter_mut().filter(|(_, e)| e.doc == doc).map(|(id, e)| (*id, &mut e.pane)).collect();
        v.sort_by_key(|(id, _)| (*id != focus, *id));
        v.into_iter().map(|(_, p)| p).collect()
    }

    pub fn handle(&mut self, ev: SessionEvent, now: Instant) -> Outcome {
        match ev {
            SessionEvent::Doc(id, DocEvent::Opened(result)) => {
                let Some(entry) = self.docs.get_mut(&id) else { return Outcome::None };
                let Some(opening) = entry.opening.take() else { return Outcome::None };
                match result {
                    Ok(info) if !info.bounds.is_empty() => {
                        let doc = Document::shared(info.bounds.clone());
                        entry.host = Some(opening.finish(info, doc, self.settings.host.clone()));
                        for spot in std::mem::take(&mut entry.waiting) {
                            self.add_pane(id, spot);
                        }
                    }
                    other => {
                        let e = match other {
                            Err(e) => e,
                            Ok(_) => "the document has no pages".into(),
                        };
                        let name = entry.name.clone();
                        self.docs.remove(&id);
                        if self.layout.is_none() {
                            self.error = Some(e);
                            return Outcome::Quit;
                        }
                        self.message(&format!("{name}: {e}"));
                    }
                }
                Outcome::Redraw
            }
            SessionEvent::Doc(id, ev) => {
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
        }
    }

    pub fn key(&mut self, k: &KeyEvent, now: Instant) -> Outcome {
        if k.kind == KeyEventKind::Release || matches!(k.code, KeyCode::Modifier(_)) {
            return Outcome::None;
        }
        if std::mem::take(&mut self.prefix) {
            let action = match k.code {
                KeyCode::Char('%') => Action::Split(Split::Horizontal),
                KeyCode::Char('"') => Action::Split(Split::Vertical),
                KeyCode::Char('h') | KeyCode::Left => Action::MoveFocus(Direction::Left),
                KeyCode::Char('j') | KeyCode::Down => Action::MoveFocus(Direction::Down),
                KeyCode::Char('k') | KeyCode::Up => Action::MoveFocus(Direction::Up),
                KeyCode::Char('l') | KeyCode::Right => Action::MoveFocus(Direction::Right),
                KeyCode::Char('o') => Action::FocusNextPane,
                KeyCode::Char('x') => Action::ClosePane,
                KeyCode::Char('d') => Action::Detach,
                _ => return Outcome::None,
            };
            return self.action(action, now);
        }
        if Key::parse(PREFIX).is_ok_and(|p| p == Key::of(k)) {
            self.prefix = true;
            return Outcome::None;
        }
        let Some(e) = self.panes.get_mut(&self.focus) else { return Outcome::None };
        match e.pane.key(k, now) {
            pane::Action::Quit => self.close(self.focus),
            pane::Action::Redraw => Outcome::Redraw,
            pane::Action::None => Outcome::None,
        }
    }

    pub fn paste(&mut self, text: &str) -> Outcome {
        let Some(e) = self.panes.get_mut(&self.focus) else { return Outcome::None };
        Outcome::None.or(e.pane.paste(text) == pane::Action::Redraw)
    }

    pub fn action(&mut self, action: Action, _now: Instant) -> Outcome {
        match action {
            Action::Split(split) => {
                let Some(doc) = self.panes.get(&self.focus).map(|e| e.doc) else { return Outcome::None };
                self.add_pane(doc, Spot::SplitOf(self.focus, split));
            }
            Action::NewPane(path) => {
                if let Err(e) = self.open(&path) {
                    self.message(&format!("{}: {e:#}", path.display()));
                }
            }
            Action::MoveFocus(dir) => {
                if let Some(p) = self.layout.as_ref().and_then(|l| l.neighbor(self.focus, dir, self.area())) {
                    self.focus = p;
                }
            }
            Action::FocusNextPane => {
                if let Some(l) = &self.layout {
                    self.focus = l.next(self.focus);
                }
            }
            Action::ClosePane => return self.close(self.focus),
            Action::Detach => return Outcome::Detach,
        }
        Outcome::Redraw
    }

    /// Close a pane, and its document when no other pane shows it.
    fn close(&mut self, id: PaneId) -> Outcome {
        let Some(layout) = self.layout.as_mut() else { return Outcome::Quit };
        let next = layout.next(id);
        if !layout.remove(id) {
            // The last pane.
            return Outcome::Quit;
        }
        if let Some(mut e) = self.panes.remove(&id) {
            e.pane.release(&mut self.images);
            if !self.panes.values().any(|p| p.doc == e.doc)
                && let Some(d) = self.docs.remove(&e.doc)
                && let Some(host) = d.host
            {
                host.doc.borrow_mut().release(&mut self.images);
            }
        }
        if self.focus == id {
            self.focus = next;
        }
        for p in [&mut self.hovered, &mut self.captured] {
            if *p == Some(id) {
                *p = None;
            }
        }
        self.relayout();
        Outcome::Redraw
    }

    /// The pane at a screen pixel.
    fn pane_at(&self, x: f32, y: f32) -> Option<PaneId> {
        let cell = self.geometry.cell;
        self.panes
            .iter()
            .find(|(_, e)| {
                let r = e.rect;
                let (x0, y0) = (r.x as f32 * cell.width as f32, r.y as f32 * cell.height as f32);
                x >= x0 && y >= y0 && x < x0 + (r.w * cell.width) as f32 && y < y0 + (r.h * cell.height) as f32
            })
            .map(|(id, _)| *id)
    }

    pub fn mouse(&mut self, m: &MouseEvent, now: Instant) -> Outcome {
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
                && let Some(e) = self.panes.get_mut(&old)
            {
                let away = MouseEvent { kind: MouseEventKind::Move, x: -100_000, y: -100_000, ..*m };
                out = out.or(e.pane.mouse(&away, now));
            }
            self.hovered = under;
        }
        match m.kind {
            MouseEventKind::Press(_) => {
                self.captured = under;
                if let Some(p) = under
                    && p != self.focus
                {
                    self.focus = p;
                    out = Outcome::Redraw;
                }
            }
            MouseEventKind::Release(_) => self.captured = None,
            _ => {}
        }
        let Some(id) = target else { return out };
        let Some(e) = self.panes.get_mut(&id) else { return out };
        let (dx, dy) = match m.unit {
            CoordUnit::Pixel => ((e.rect.x * cell.width as i32), (e.rect.y * cell.height as i32)),
            CoordUnit::Cell => (e.rect.x, e.rect.y),
        };
        let local = MouseEvent { x: m.x - dx, y: m.y - dy, ..*m };
        out.or(e.pane.mouse(&local, now))
    }

    /// Effects for the frontend, including requests panes made since the last
    /// call (call every loop iteration: inverse search does not redraw).
    pub fn take_effects(&mut self) -> Vec<Effect> {
        for e in self.panes.values_mut() {
            for req in e.pane.take_requests() {
                if let Some(host) = self.docs.get_mut(&e.doc).and_then(|d| d.host.as_mut())
                    && let Some(effect) = host.request(req)
                {
                    self.effects.push(effect);
                }
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
        self.hovered.and_then(|p| self.panes.get(&p)).map_or(Pointer::Default, |e| e.pane.pointer())
    }

    /// Compose this frame from every pane and hand their work to the documents.
    pub fn frame(&mut self) -> Frame {
        let cell = self.geometry.cell;
        // No screen-wide background: terminal panes show the terminal's own
        // background, so only the gaps between panes get one (border_layers).
        let mut layers = Vec::new();
        let mut text = Vec::new();
        let mut used = HashSet::new();
        let order = self.layout.as_ref().map(|l| l.panes()).unwrap_or_default();
        let mut needs: HashMap<DocId, Vec<Need>> = HashMap::new();
        let mut cursor = None;
        let mut cursor_shape = CursorShape::Default;
        let mut title = self.name.clone();
        let mut complete = true;
        // The focused pane's work first.
        let mut ids = order.clone();
        ids.sort_by_key(|id| *id != self.focus);
        for id in ids {
            let Some(e) = self.panes.get_mut(&id) else { continue };
            let (frame, pane_needs) = e.pane.frame(&mut self.images);
            needs.entry(e.doc).or_default().extend(pane_needs);
            complete &= e.pane.viewer.is_complete();
            let (ox, oy) = (e.rect.x * cell.width as i32, e.rect.y * cell.height as i32);
            let clip = Rect::new(ox, oy, e.rect.w * cell.width, e.rect.h * cell.height);
            for l in frame.layers {
                let moved = Layer { dst: Rect::new(l.dst.x + ox, l.dst.y + oy, l.dst.w, l.dst.h), ..l };
                if let Some(c) = moved.clipped(&clip) {
                    used.insert((id, l.key));
                    layers.push(Layer { key: self.keys.get(id, l.key), ..c });
                }
            }
            for g in frame.text {
                text.push(TextGrid { col: g.col + e.rect.x as u16, row: g.row + e.rect.y as u16, ..g });
            }
            if id == self.focus {
                cursor = frame.cursor.map(|(c, r)| (c + e.rect.x as u16, r + e.rect.y as u16));
                cursor_shape = frame.cursor_shape;
                title = frame.title;
            }
        }
        self.complete = complete;
        self.border_layers(&mut layers, &mut used);
        self.keys.retain(&used);
        for (doc, needs) in needs {
            if let Some(host) = self.docs.get_mut(&doc).and_then(|d| d.host.as_mut()) {
                host.dispatch(needs);
            }
        }
        Frame { layers, text, cursor, cursor_shape, pointer: self.pointer(), title }
    }

    /// Lines in the one-cell gaps between panes; the focused pane's in the accent colour.
    fn border_layers(&mut self, layers: &mut Vec<Layer>, used: &mut HashSet<(u32, u32)>) {
        if self.panes.len() < 2 {
            return;
        }
        let cell = self.geometry.cell;
        let (cols, rows) = (self.geometry.cols as i32, self.geometry.rows as i32);
        let accent = termpix_ui::Theme::dark().accent.0;
        let mut lines = Vec::new();
        let mut gaps = Vec::new();
        for (id, e) in &self.panes {
            let r = e.rect;
            let (x0, y0) = (r.x * cell.width as i32, r.y * cell.height as i32);
            let (w, h) = (r.w * cell.width, r.h * cell.height);
            let focus = *id == self.focus;
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
            let k = (OWNER_SESSION, 0x1000 + i as u32);
            used.insert(k);
            layers.push(Layer {
                key: self.keys.get(k.0, k.1),
                image,
                src: Rect::new(0, 0, r.w, r.h),
                dst: r,
                z: Z_SESSION_BACKGROUND,
            });
        }
        for (i, (focus, r)) in lines.into_iter().enumerate() {
            let image = self.solid(r.w, r.h, if focus { accent } else { BORDER });
            let k = (OWNER_SESSION, 1 + i as u32);
            used.insert(k);
            let z = if focus { Z_BORDER_FOCUS } else { Z_BORDER };
            layers.push(Layer { key: self.keys.get(k.0, k.1), image, src: Rect::new(0, 0, r.w, r.h), dst: r, z });
        }
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
            && self.panes.values().all(|e| !e.pane.search.is_active() || e.pane.search.is_complete())
    }

    pub fn is_animating(&self) -> bool {
        self.panes.values().any(|e| e.pane.is_animating())
    }

    /// Advance animations and timers. Returns whether to redraw.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for e in self.panes.values_mut() {
            changed |= e.pane.tick(now);
            // A highlight expired.
            changed |= e.pane.next_deadline().is_some_and(|d| now >= d);
        }
        for (&id, d) in self.docs.iter_mut() {
            if let Some(host) = d.host.as_mut() {
                let mut panes = Self::panes_of(&mut self.panes, id, self.focus);
                changed |= host.poll(&mut panes, now);
            }
        }
        changed
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let panes = self.panes.values().filter_map(|e| e.pane.next_deadline());
        let docs = self.docs.values().filter_map(|d| d.host.as_ref()?.next_deadline());
        panes.chain(docs).min()
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

    /// The focused pane (for options applied after start-up, e.g. `--page`).
    pub fn focused_pane(&mut self) -> Option<&mut PdfPane> {
        self.panes.get_mut(&self.focus).map(|e| &mut e.pane)
    }

    /// Ask for a forward search in the focused pane's document.
    pub fn forward(&mut self, req: ForwardRequest) {
        if let Some(doc) = self.panes.get(&self.focus).map(|e| e.doc) {
            self.handle(SessionEvent::Forward(doc, req), Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver};
    use temzdf_pdfcore::doc_cache::TileKey;
    use temzdf_pdfcore::pdf;
    use termpix_core::geom::CellSize;
    use termpix_core::input::{Modifiers, MouseButton};

    const GEOMETRY: Geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };

    fn session() -> (Session, Receiver<SessionEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let settings = Settings {
            keymap: Keymap::default(),
            preview_delay: None,
            anchor_by_text: false,
            scroll: ScrollConfig::default(),
            host: HostOptions::default(),
            serve_forward: false,
        };
        let mut s = Session::new("test".into(), GEOMETRY, Hints::default(), settings, sink);
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
        s.key(&Key::parse(spec).unwrap().event(), Instant::now())
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
        assert_eq!(key(&mut s, "C-s"), Outcome::None, "waits for the command");
        assert_eq!(key(&mut s, "%"), Outcome::Redraw);
        assert_eq!(s.pane_count(), 2);
        let second = s.focus();
        assert_ne!(first, second, "the new pane has the focus");
        key(&mut s, "C-s");
        key(&mut s, "h");
        assert_eq!(s.focus(), first);
        // A click on the right half focuses the pane there.
        click(&mut s, 700, 200);
        assert_eq!(s.focus(), second);
        key(&mut s, "C-s");
        assert_eq!(key(&mut s, "x"), Outcome::Redraw);
        assert_eq!((s.pane_count(), s.focus()), (1, first));
        key(&mut s, "C-s");
        assert_eq!(key(&mut s, "d"), Outcome::Detach);
        assert_eq!(key(&mut s, "q"), Outcome::Quit, "quitting the last pane ends the session");
    }

    #[test]
    fn panes_are_composed_inside_their_rectangles_with_unique_keys() {
        let (mut s, rx) = session();
        s.action(Action::Split(Split::Horizontal), Instant::now());
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
        s.action(Action::Split(Split::Vertical), Instant::now());
        let (_, rendered) = settle(&mut s, &rx);
        assert!(!rendered.is_empty());
        let unique: HashSet<_> = rendered.iter().collect();
        assert_eq!(unique.len(), rendered.len(), "no tile rendered twice: {rendered:?}");
    }

    #[test]
    fn closing_a_pane_releases_its_document() {
        let (mut s, rx) = session();
        settle(&mut s, &rx);
        let before = s.images().len();
        s.action(Action::NewPane(pdf::fixture("sample-short.pdf")), Instant::now());
        while s.pane_count() < 2 {
            let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            s.handle(ev, Instant::now());
        }
        settle(&mut s, &rx);
        assert!(s.images().len() > before);
        s.action(Action::ClosePane, Instant::now());
        // The remaining pane is wider again: its new tiles replace the old ones
        // over time, but the closed document's tiles and the pane's own images
        // are gone at once.
        assert_eq!(s.docs.len(), 1);
        let (_, _) = settle(&mut s, &rx);
        assert!(s.panes.values().all(|e| e.doc != 0));
    }

    #[test]
    fn opening_a_missing_file_fails_the_session() {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let sink: EventSink = Arc::new(move |e| tx.lock().unwrap().send(e).is_ok());
        let settings = Settings {
            keymap: Keymap::default(),
            preview_delay: None,
            anchor_by_text: false,
            scroll: ScrollConfig::default(),
            host: HostOptions::default(),
            serve_forward: false,
        };
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
        s.action(Action::Split(Split::Horizontal), Instant::now());
        s.action(Action::Split(Split::Vertical), Instant::now());
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

        let (mut fresh, rx2) = session();
        fresh.resize(big);
        fresh.action(Action::Split(Split::Horizontal), Instant::now());
        fresh.action(Action::Split(Split::Vertical), Instant::now());
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
}
