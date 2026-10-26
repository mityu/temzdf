//! Everything the viewer shows and how it reacts to input, independent of the
//! terminal and of threads: the page viewer, the UI chrome (status bar, ...),
//! and the key/mouse bindings. `app.rs` drives it interactively; headless mode
//! and tests drive it directly.

use std::cell::Ref;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use temzdf_scene::{Frame, ImageKey, Images, Layer, Pointer};
use termpix_core::geom::{Geometry, Rect};
use termpix_core::input::{
    CoordUnit, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind, ScrollDirection,
};
use termpix_ui::{Canvas, Color, EditResult, LineInput, ListView, StatusBar, Text, Theme};

use crate::document::{self, Document, SharedDocument};
use crate::keys::{Command, Key, Keymap};
use crate::outline::{self, OutlinePanel};
use crate::pdf::{DocInfo, Link, LinkTarget, PageRect, Target};
use crate::search::SearchState;
use crate::selection::{Caret, PageText, Selection};
use crate::synctex::PdfBox;
use crate::viewer::{LAYER_KEYS_END, Need, Peek, Viewer};

pub const ZOOM_STEP: f32 = 1.1;
/// UI layers sit above page tiles but, like them, below terminal text.
const Z_UI: i32 = -1_073_741_800;
const KEY_STATUS: u32 = LAYER_KEYS_END;
const KEY_SIDEBAR: u32 = LAYER_KEYS_END + 1;
const KEY_INPUT: u32 = LAYER_KEYS_END + 2;
const KEY_PREVIEW: u32 = LAYER_KEYS_END + 3;
/// Link previews float above the UI (still below terminal text).
const Z_PREVIEW: i32 = Z_UI + 10;
/// Link preview size as a fraction of the page area, its border, and how long
/// the pointer may be off both the link and the preview before it closes (time
/// to move from one to the other).
const PREVIEW_WIDTH: f32 = 0.6;
const PREVIEW_HEIGHT: f32 = 0.35;
const PREVIEW_BORDER: u32 = 2;
const PREVIEW_GRACE: Duration = Duration::from_millis(300);
pub const DEFAULT_PREVIEW_DELAY: Duration = Duration::from_millis(300);
/// Overlays (search hits, selection) use keys from here on, one per rectangle.
const KEY_OVERLAYS: u32 = LAYER_KEYS_END + 0x1000;
/// Overlays: above page tiles, below UI.
const Z_OVERLAY: i32 = -1_073_741_850;
const HIT_COLOR: Color = Color::rgba(255, 210, 0, 96);
const CURRENT_HIT_COLOR: Color = Color::rgba(255, 120, 0, 150);
const SELECTION_COLOR: Color = Color::rgba(70, 130, 255, 90);
/// Forward-search target, shown briefly.
const FLASH_COLOR: Color = Color::rgba(255, 70, 70, 80);
const FLASH_DURATION: std::time::Duration = std::time::Duration::from_millis(1500);
/// Selection auto-scroll: pixels per second per pixel of pointer depth into the
/// edge zone (or beyond the edge).
const AUTOSCROLL_GAIN: f32 = 25.0;
/// How long a text reload anchor waits for the pages around it to load.
const TEXT_ANCHOR_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// A line of text from before a reload (`[reload] anchor = "text"`), with its
/// neighbours to tell repeated lines apart and its place on screen.
#[derive(Clone, Debug)]
struct TextAnchor {
    line: String,
    prev: String,
    next: String,
    page: usize,
    /// Doc-space y (points) of the line in the old document.
    doc_y: f32,
    screen_y: f32,
    deadline: Instant,
}

/// An internal link under the pointer, previewed once the pointer rests there
/// until `due`.
#[derive(Clone, Debug)]
struct HoverLink {
    page: usize,
    rect: PageRect,
    target: Target,
    due: Instant,
}

/// An open link preview: a [`Peek`] window at the link's target.
#[derive(Clone, Debug)]
struct Preview {
    /// The link it previews: its page and page-space rectangle.
    page: usize,
    link: PageRect,
    peek: Peek,
    /// When the pointer left both the link and the preview.
    left_at: Option<Instant>,
}

/// Work the session asks the caller to do (on a worker, or inline).
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Search {
        generation: u64,
        query: String,
        start_page: usize,
        doc_generation: u32,
    },
    /// Open an external link with the system handler.
    OpenUri(String),
    /// Put text on the clipboard (OSC 52).
    Copy(String),
    /// SyncTeX inverse search at a page-space point.
    Inverse {
        page: usize,
        x: f32,
        y: f32,
    },
}

/// Movement (pixels) below which a press and release count as a click.
const CLICK_SLOP: f32 = 4.0;
const HISTORY_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Redraw,
    Quit,
}

/// The state a widget image was rendered from; re-rendered when it changes.
#[derive(Clone, Debug, PartialEq)]
enum WidgetState {
    Status(StatusBar),
    List(ListView, u32),
    Input(LineInput),
    /// The border around a link preview.
    Frame,
}

struct WidgetImage {
    image: ImageKey,
    state: WidgetState,
    size: (u32, u32),
}

pub struct PdfPane {
    pub viewer: Viewer,
    pub info: DocInfo,
    pub name: String,
    geometry: Geometry,
    text: Text,
    theme: Theme,
    widgets: HashMap<u32, WidgetImage>,
    /// Error shown in the status bar until the next key press.
    pub message: Option<String>,
    /// Informational message, shown the same way in a calmer colour.
    notice: Option<String>,
    outline: OutlinePanel,
    sidebar_open: bool,
    pub search: SearchState,
    /// Search generations start here, so panes sharing a document's search
    /// thread tell their results apart.
    search_base: u64,
    /// The search prompt, while typing a query.
    input: Option<LineInput>,
    /// Where the view was when the prompt opened (restored on Esc).
    search_origin: Option<(Target, f32, f32)>,
    /// Enter was pressed before any hit was selected (results arrive from a
    /// worker): select the first one after this position once it is found.
    pending_first_hit: Option<Target>,
    requests: Vec<Request>,
    /// Solid overlay images by (width, height, colour).
    overlay_images: HashMap<(u32, u32, [u8; 4]), ImageKey>,
    selection: Option<Selection>,
    /// A left drag is extending the selection.
    selecting: bool,
    /// Selection auto-scroll speed in pixels per second (0 when off).
    autoscroll: f32,
    autoscroll_at: Option<Instant>,
    /// `[reload] anchor = "text"`: keep the line being read in place on reload.
    pub anchor_by_text: bool,
    /// Page-view key bindings (`[keys]` in the config).
    pub keymap: Keymap,
    /// A line from before the last reload, still to be found in the new text.
    text_anchor: Option<TextAnchor>,
    /// Search hit to re-select once a re-run search (after reload) reaches it.
    restore_hit: Option<(usize, usize)>,
    /// The file changed and a reload is pending (shown in the status bar).
    pub reloading: bool,
    /// Forward-search result highlighted until the deadline.
    flash: Option<(Vec<PdfBox>, Instant)>,
    /// Terminal cell for the text cursor (input caret), if shown.
    caret_cell: Option<(u16, u16)>,
    pointer_shape: Pointer,
    /// `[links] preview`: preview internal links after the pointer rests on
    /// them this long (`None`: never).
    pub preview_delay: Option<Duration>,
    hover: Option<HoverLink>,
    preview: Option<Preview>,
    /// The external link under the pointer, shown in the status bar.
    hover_uri: Option<String>,
    /// Outline state kept across a reload (see [`PdfPane::before_reload`]).
    reload_state: Option<(Vec<Vec<String>>, Option<String>)>,
    /// Where the left button went down (screen pixels).
    press: Option<(f32, f32)>,
    /// Jump history: positions (with horizontal scroll) to go back / forward to.
    back: Vec<(Target, f32)>,
    forward: Vec<(Target, f32)>,
    // Input state
    count: Option<usize>,
    pointer: (f32, f32),
    drag: Option<((f32, f32), (f32, f32))>,
}

impl PdfPane {
    pub fn new(name: String, info: DocInfo, geometry: Geometry) -> Self {
        let doc = Document::shared(info.bounds.clone());
        Self::with_document(name, info, doc, geometry)
    }

    /// A pane on a document other panes may show too (`doc` must be `info`'s).
    pub fn with_document(name: String, info: DocInfo, doc: SharedDocument, geometry: Geometry) -> Self {
        let (w, h) = page_area(geometry);
        let outline = OutlinePanel::new(&info.outline, &info.labels);
        let pages = info.bounds.len();
        Self {
            outline,
            sidebar_open: false,
            search: SearchState::new(pages),
            search_base: 0,
            input: None,
            search_origin: None,
            pending_first_hit: None,
            requests: Vec::new(),
            overlay_images: HashMap::new(),
            selection: None,
            selecting: false,
            autoscroll: 0.0,
            autoscroll_at: None,
            anchor_by_text: false,
            keymap: Keymap::default(),
            text_anchor: None,
            restore_hit: None,
            reloading: false,
            flash: None,
            caret_cell: None,
            pointer_shape: Pointer::Default,
            preview_delay: Some(DEFAULT_PREVIEW_DELAY),
            hover: None,
            preview: None,
            hover_uri: None,
            reload_state: None,
            press: None,
            back: Vec::new(),
            forward: Vec::new(),
            viewer: Viewer::with_document(doc, w, h),
            info,
            name,
            geometry,
            text: Text::new(),
            theme: Theme::dark(),
            widgets: HashMap::new(),
            message: None,
            notice: None,
            count: None,
            pointer: (0.0, 0.0),
            drag: None,
        }
    }

    /// See `search_base`. Call before any search.
    pub fn set_search_base(&mut self, base: u64) {
        self.search_base = base;
        self.search.generation = base;
    }

    /// The pane is closed: drop its own images (UI, overlays, stand-ins) from
    /// the store; the document's tiles stay with the document.
    pub fn release(&mut self, images: &mut Images) {
        for (_, w) in self.widgets.drain() {
            images.remove(w.image);
        }
        for (_, id) in self.overlay_images.drain() {
            images.remove(id);
        }
        self.viewer.release(images);
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    pub fn resize(&mut self, geometry: Geometry) {
        self.geometry = geometry;
        self.close_preview();
        self.apply_area();
    }

    /// Fit the page area to the screen, right of the sidebar when it is open.
    fn apply_area(&mut self) {
        let (w, h) = page_area(self.geometry);
        let side = if self.sidebar_open { self.sidebar_width() } else { 0 };
        self.viewer.set_area(side as i32, w.saturating_sub(side).max(1), h);
    }

    fn bar_height(&self) -> u32 {
        self.geometry.cell.height
    }

    fn row_height(&self) -> u32 {
        (self.geometry.cell.height as f32 * 1.3).round() as u32
    }

    fn sidebar_width(&self) -> u32 {
        let w = self.geometry.width_px();
        (w as f32 * 0.3).clamp(180.0_f32.min(w as f32 / 2.0), 420.0) as u32
    }

    /// Where the outline sidebar is drawn when open: the left side, with the
    /// page area pushed to its right.
    fn sidebar_rect(&self) -> Rect {
        Rect::new(0, 0, self.sidebar_width(), self.viewer.view.screen_h as u32)
    }

    pub fn toggle_sidebar(&mut self) {
        self.close_preview();
        self.sidebar_open = !self.sidebar_open;
        self.apply_area();
        if self.sidebar_open {
            self.outline.select_position(&self.info.outline, self.viewer.top_position());
            let r = self.sidebar_rect();
            let rh = self.row_height();
            self.outline.list.reveal_selected(r.h, rh);
        }
    }

    /// Jump to the selected outline entry.
    fn open_selected_entry(&mut self, now: Instant) {
        if let Some(t) = self.outline.selected_entry().and_then(|i| self.info.outline[i].target) {
            self.push_history();
            self.viewer.goto(t, true, now);
        }
    }

    fn sidebar_key(&mut self, k: &KeyEvent, now: Instant) -> Option<Action> {
        let entries = &self.info.outline;
        let labels = &self.info.labels;
        let r = self.sidebar_rect();
        let rh = self.row_height();
        let p = &mut self.outline;
        match k.code {
            KeyCode::Tab | KeyCode::Escape => {
                self.toggle_sidebar();
                return Some(Action::Redraw);
            }
            KeyCode::Char('j') | KeyCode::Down => p.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => p.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => p.select_first(),
            KeyCode::Char('G') | KeyCode::End => p.select_last(),
            KeyCode::PageDown => p.move_selection(p.list.visible_rows(r.h, rh) as isize),
            KeyCode::PageUp => p.move_selection(-(p.list.visible_rows(r.h, rh) as isize)),
            KeyCode::Char('l') | KeyCode::Right => {
                if let Some(i) = p.selected_entry() {
                    p.set_expanded(entries, labels, i, true);
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if let Some(i) = p.selected_entry() {
                    if p.is_expanded(i) && entries[i].descendants > 0 {
                        p.set_expanded(entries, labels, i, false);
                    } else if let Some(parent) = outline::parent(entries, i) {
                        let row = p.list.items.len();
                        p.move_selection(-(row as isize));
                        while p.selected_entry() != Some(parent) && p.list.selected < Some(row) {
                            p.move_selection(1);
                        }
                    }
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.open_selected_entry(now);
                return Some(Action::Redraw);
            }
            KeyCode::Char('q') => return Some(Action::Quit),
            _ => return None,
        }
        self.outline.list.reveal_selected(r.h, rh);
        Some(Action::Redraw)
    }

    /// Mouse over the open sidebar. Returns true when it needs redrawing.
    fn sidebar_mouse(&mut self, m: &MouseEvent, now: Instant) -> Option<bool> {
        let r = self.sidebar_rect();
        let (x, y) = self.pointer;
        let rh = self.row_height();
        let row = self.outline.list.hit(y - r.y as f32, rh);
        Some(match m.kind {
            MouseEventKind::Move => {
                let changed = self.outline.list.hover != row;
                self.outline.list.hover = row;
                changed
            }
            MouseEventKind::Scroll(ScrollDirection::Down) => {
                self.outline.list.scroll_by(3, r.h, rh);
                true
            }
            MouseEventKind::Scroll(ScrollDirection::Up) => {
                self.outline.list.scroll_by(-3, r.h, rh);
                true
            }
            MouseEventKind::Press(MouseButton::Left) => {
                let Some(row) = row else { return Some(false) };
                let Some(i) = self.outline.entry_at_row(row) else { return Some(false) };
                let e = &self.info.outline[i];
                // Clicking the arrow toggles; clicking the title jumps.
                let arrow_end = r.x as f32 + rh as f32 * (0.5 + e.depth as f32 * 0.9 + 0.9);
                if e.descendants > 0 && x < arrow_end {
                    let open = !self.outline.is_expanded(i);
                    self.outline.set_expanded(&self.info.outline, &self.info.labels, i, open);
                } else {
                    self.outline.list.selected = Some(row);
                    self.open_selected_entry(now);
                }
                true
            }
            _ => false,
        })
    }

    /// Current page as "label (n/N)".
    pub fn page_text(&self) -> String {
        let p = self.viewer.current_page();
        let n = self.viewer.page_count();
        let label = self.info.labels.get(p).cloned().unwrap_or_else(|| (p + 1).to_string());
        if label == (p + 1).to_string() { format!("{label} / {n}") } else { format!("{label} ({} / {n})", p + 1) }
    }

    /// This frame (layers, text cursor, pointer shape, title) and the work
    /// still needed.
    pub fn frame(&mut self, images: &mut Images) -> (Frame, Vec<Need>) {
        self.viewer.peek = self.preview.as_ref().map(|p| p.peek);
        let (mut layers, needs) = self.viewer.frame(images);
        // Zoom as a percentage of 96 dpi ("100%" = actual size on a typical screen).
        let zoom = (self.viewer.view.scale * 72.0 / 96.0 * 100.0).round();
        let (w, h) = (self.geometry.width_px(), self.bar_height());
        let y = self.geometry.height_px().saturating_sub(h) as i32;
        let bar = Rect::new(0, y, w, h);
        self.caret_cell = None;
        if let Some(mut input) = self.input.clone() {
            input.hint = self.search_hint();
            layers.push(self.widget(images, KEY_INPUT, WidgetState::Input(input.clone()), bar));
            // Put the terminal cursor at the caret so IME preedit shows there:
            // in the first cell starting at or after it, since the cursor
            // covers a whole cell and would hide the last character typed.
            let caret_x = input.caret_x(&mut self.text, &self.theme, h);
            let cell = self.geometry.cell;
            let col = (caret_x / cell.width as f32).ceil() as u32;
            let col = col.min(self.geometry.cols.saturating_sub(1) as u32);
            self.caret_cell = Some((col as u16, (y as u32 / cell.height) as u16));
        } else {
            let mut right = format!("{}   {zoom}%", self.page_text());
            if self.search.is_active() {
                right = format!("/{} {}   {right}", self.search.query, self.search_hint());
            }
            if self.reloading {
                right = format!("reloading…   {right}");
            }
            let notice = self.hover_uri.clone().or_else(|| self.notice.clone());
            let status = StatusBar { left: self.name.clone(), right, error: self.message.clone(), notice };
            layers.push(self.widget(images, KEY_STATUS, WidgetState::Status(status), bar));
        }
        self.overlay_layers(images, &mut layers);
        if self.sidebar_open {
            let r = self.sidebar_rect();
            let state = WidgetState::List(self.outline.list.clone(), self.row_height());
            layers.push(self.widget(images, KEY_SIDEBAR, state, r));
        }
        if let Some(r) = self.preview.as_ref().map(|p| p.peek.rect) {
            let b = PREVIEW_BORDER;
            let outer = Rect::new(r.x - b as i32, r.y - b as i32, r.w + 2 * b, r.h + 2 * b);
            layers.push(Layer { z: Z_PREVIEW, ..self.widget(images, KEY_PREVIEW, WidgetState::Frame, outer) });
        }
        let title = format!("{} — {}", self.name, self.page_text());
        (Frame { layers, cursor: self.caret_cell, pointer: self.pointer_shape, title, ..Default::default() }, needs)
    }

    fn widget(&mut self, images: &mut Images, key: u32, state: WidgetState, rect: Rect) -> Layer {
        // A canvas is at least 1x1 (a tiny pane can leave a widget no
        // room): the image must be as large as what was drawn.
        let rect = Rect { w: rect.w.max(1), h: rect.h.max(1), ..rect };
        let size = (rect.w, rect.h);
        let fresh = self.widgets.get(&key).is_some_and(|w| w.state == state && w.size == size);
        if !fresh {
            let px = match &state {
                WidgetState::Status(s) => s.render(&mut self.text, &self.theme, rect.w, rect.h),
                WidgetState::List(l, rh) => l.render(&mut self.text, &self.theme, rect.w, rect.h, *rh),
                WidgetState::Input(i) => i.render(&mut self.text, &self.theme, rect.w, rect.h).0,
                WidgetState::Frame => {
                    let mut c = Canvas::new(rect.w, rect.h);
                    c.fill_rect(0.0, 0.0, rect.w as f32, rect.h as f32, self.theme.accent);
                    c.finish()
                }
            };
            let image = match self.widgets.get(&key) {
                Some(w) => {
                    images.replace(w.image, rect.w, rect.h, px);
                    w.image
                }
                None => images.add(rect.w, rect.h, px),
            };
            self.widgets.insert(key, WidgetImage { image, state, size });
        }
        let image = self.widgets[&key].image;
        Layer { key, image, src: Rect::new(0, 0, rect.w, rect.h), dst: rect, z: Z_UI }
    }

    /// "query  k/N" for the status bar or prompt.
    fn search_hint(&self) -> String {
        if !self.search.is_active() {
            return String::new();
        }
        let total = self.search.total();
        let more = if self.search.is_complete() { "" } else { "+" };
        match self.search.current_ordinal() {
            Some(k) => format!("{k}/{total}{more}"),
            None if total == 0 && self.search.is_complete() => "no match".into(),
            None => format!("{total}{more}"),
        }
    }

    /// The line nearest a third of the way down the screen, with its neighbours,
    /// as an anchor to find again after a reload.
    fn capture_text_anchor(&mut self, now: Instant) -> Option<TextAnchor> {
        let v = &self.viewer;
        let (page, _, y) = v.screen_to_page_clamped(v.origin_x() as f32 + v.view.screen_w / 2.0, v.view.screen_h / 3.0);
        let text = self.text_of_page(page)?.clone();
        let line = |i: usize| -> String {
            text.lines
                .get(i)
                .map(|l| text.chars[l.start..l.end].iter().map(|c| c.c).collect::<String>())
                .unwrap_or_default()
        };
        // A line with enough letters to be recognisable, nearest the anchor point.
        let i = (0..text.lines.len())
            .filter(|&i| line(i).chars().filter(|c| c.is_alphanumeric()).count() >= 8)
            .min_by(|&a, &b| (text.lines[a].rect.y0 - y).abs().total_cmp(&(text.lines[b].rect.y0 - y).abs()))?;
        let y0 = text.lines[i].rect.y0;
        let (_, screen_y) = self.viewer.page_to_screen(page, 0.0, y0);
        Some(TextAnchor {
            line: line(i),
            prev: if i > 0 { line(i - 1) } else { String::new() },
            next: line(i + 1),
            page,
            doc_y: self.viewer.layout.pages[page].y + y0,
            screen_y,
            deadline: now + TEXT_ANCHOR_WAIT,
        })
    }

    /// Look for the remembered line in the pages around its old page once they
    /// have all loaded (or, with `force`, in those loaded so far), and put it
    /// back at the same height on screen. Returns true when the view moved.
    fn resolve_text_anchor(&mut self, force: bool) -> bool {
        let Some(a) = self.text_anchor.clone() else { return false };
        let n = self.viewer.page_count();
        let mut best: Option<(u32, f32, usize, f32)> = None; // (score, distance, page, y0)
        let pages = a.page.saturating_sub(2)..(a.page + 3).min(n);
        if !force && pages.clone().any(|p| self.viewer.display_list(p).is_none()) {
            return false;
        }
        self.text_anchor = None;
        for p in pages {
            let Some(text) = self.text_of_page(p).map(|t| t.clone()) else { continue };
            let line = |i: usize| -> String {
                text.lines
                    .get(i)
                    .map(|l| text.chars[l.start..l.end].iter().map(|c| c.c).collect::<String>())
                    .unwrap_or_default()
            };
            for (i, l) in text.lines.iter().enumerate() {
                if line(i) != a.line {
                    continue;
                }
                let prev = if i > 0 { line(i - 1) } else { String::new() };
                let score = u32::from(prev == a.prev) + u32::from(line(i + 1) == a.next);
                let dist = (self.viewer.layout.pages[p].y + l.rect.y0 - a.doc_y).abs();
                if best.is_none_or(|b| (score, -dist) > (b.0, -b.1)) {
                    best = Some((score, dist, p, l.rect.y0));
                }
            }
        }
        let Some((_, _, p, y0)) = best else { return false };
        let (_, sy) = self.viewer.page_to_screen(p, 0.0, y0);
        let v = &mut self.viewer;
        v.jump_to(v.view.scroll_x, v.view.scroll_y + (sy - a.screen_y));
        true
    }

    /// A page of the current document finished loading. Returns true when the
    /// view moved (a pending reload anchor was found).
    pub fn page_ready(&mut self) -> bool {
        self.resolve_text_anchor(false)
    }

    pub fn page_links(&mut self, page: usize, links: Vec<Link>) {
        self.viewer.doc.borrow_mut().set_links(page, links);
    }

    pub fn pointer(&self) -> Pointer {
        self.pointer_shape
    }

    /// The link at a page-space point.
    fn link_on_page(&self, page: usize, x: f32, y: f32) -> Option<Link> {
        self.viewer.doc.borrow().link_at(page, x, y).cloned()
    }

    /// The link under a screen pixel in the page view, and its page.
    fn link_at(&self, sx: f32, sy: f32) -> Option<(usize, Link)> {
        let (page, x, y) = self.viewer.screen_to_page(sx, sy)?;
        Some((page, self.link_on_page(page, x, y)?))
    }

    /// Track the link under the pointer in the page view: an external link's
    /// URI goes to the status bar; resting on an internal link opens its
    /// preview after the delay. Returns true when the status bar changed.
    fn update_hover(&mut self, m: &MouseEvent, now: Instant) -> bool {
        let (x, y) = self.pointer;
        let hit = if self.drag.is_none() && !self.selecting { self.link_at(x, y) } else { None };
        let hit = hit.map(|(page, l)| (page, l.clone()));
        let uri = match &hit {
            Some((_, Link { target: LinkTarget::External(u), .. })) => Some(u.clone()),
            _ => None,
        };
        let changed = self.hover_uri != uri;
        self.hover_uri = uri;
        match (hit, self.preview_delay) {
            (Some((page, Link { rect, target: LinkTarget::Internal(target) })), Some(delay))
                if m.kind == MouseEventKind::Move =>
            {
                let same = |p: usize, r: &PageRect| p == page && *r == rect;
                let shown = self.preview.as_ref().is_some_and(|p| same(p.page, &p.link));
                if !shown && !self.hover.as_ref().is_some_and(|h| same(h.page, &h.rect)) {
                    self.hover = Some(HoverLink { page, rect, target, due: now + delay });
                }
            }
            _ => self.hover = None,
        }
        changed
    }

    /// Open the preview of a hovered link: below the link (above it when there
    /// is no room), showing what following the link would put at the top of
    /// the screen, with the page's columns where the page view has them.
    fn open_preview(&mut self, h: HoverLink) {
        let v = &self.viewer;
        let s = v.view.scale;
        let area = Rect::new(v.origin_x(), 0, v.view.screen_w as u32, v.view.screen_h as u32);
        let b = PREVIEW_BORDER as i32;
        let target_page = h.target.page.min(v.page_count() - 1);
        let pb = v.layout.pages[target_page];
        let page_w = pb.w * s;
        let w = ((area.w as f32 * PREVIEW_WIDTH).min(page_w) as i32).min(area.w as i32 - 2 * b).max(1);
        let ht = ((area.h as f32 * PREVIEW_HEIGHT) as i32).min(area.h as i32 - 2 * b).max(1);
        let (_, link_top) = v.page_to_screen(h.page, h.rect.x0, h.rect.y0);
        let (_, link_bottom) = v.page_to_screen(h.page, h.rect.x1, h.rect.y1);
        let x = (self.pointer.0 as i32 - w / 2).clamp(area.x + b, (area.right() as i32 - w - b).max(area.x + b));
        let below = link_bottom.ceil() as i32 + b;
        let y = if below + ht + b <= area.bottom() as i32 { below } else { (link_top.floor() as i32 - b - ht).max(b) };
        let page_left = v.page_to_screen(target_page, 0.0, 0.0).0;
        let scroll_x = (pb.x * s + (x as f32 - page_left)).clamp(0.0, (v.layout.width * s - w as f32).max(0.0));
        let scroll_y = (v.target_top(h.target) * s).max(0.0);
        let peek = Peek { rect: Rect::new(x, y, w as u32, ht as u32), scroll_x, scroll_y, z: Z_PREVIEW + 1 };
        self.preview = Some(Preview { page: h.page, link: h.rect, peek, left_at: None });
    }

    /// Close the link preview (and forget a pending one). Returns true if one was open.
    fn close_preview(&mut self) -> bool {
        self.hover = None;
        self.preview.take().is_some()
    }

    /// Mouse events while a link preview is open. Over the preview it takes
    /// them all (`Some`, with whether to redraw): the wheel scrolls it, a click
    /// on a link follows it (external links open, the preview stays) and a
    /// click elsewhere jumps to what it shows. Elsewhere (`None`) the page view
    /// handles them; pressing a button or the wheel there closes the preview,
    /// and so does leaving both it and its link for `PREVIEW_GRACE`.
    fn preview_mouse(&mut self, m: &MouseEvent, now: Instant) -> Option<bool> {
        let p = self.preview.as_mut()?;
        let (x, y) = self.pointer;
        let r = p.peek.rect;
        let b = PREVIEW_BORDER as i32;
        let outer = Rect::new(r.x - b, r.y - b, r.w + 2 * b as u32, r.h + 2 * b as u32);
        if !outer.contains(x as i32, y as i32) {
            let (page, link) = (p.page, p.link);
            let on_link =
                self.viewer.screen_to_page(x, y).is_some_and(|(pg, px, py)| pg == page && contains(&link, px, py));
            let p = self.preview.as_mut()?;
            match m.kind {
                _ if on_link => p.left_at = None,
                MouseEventKind::Move => {
                    p.left_at.get_or_insert(now);
                }
                MouseEventKind::Press(_) | MouseEventKind::Scroll(_) => {
                    self.close_preview();
                }
                _ => {}
            }
            return None;
        }
        p.left_at = None;
        let peek = p.peek;
        self.hover = None;
        let link = self
            .viewer
            .peek_to_page(&peek, x, y)
            .and_then(|(page, px, py)| self.link_on_page(page, px, py))
            .map(|l| l.target.clone());
        self.pointer_shape = if link.is_some() { Pointer::Link } else { Pointer::Default };
        let uri = match &link {
            Some(LinkTarget::External(u)) => Some(u.clone()),
            _ => None,
        };
        let mut redraw = self.hover_uri != uri;
        self.hover_uri = uri;
        let ctrl = m.modifiers.contains(Modifiers::CTRL);
        match m.kind {
            MouseEventKind::Scroll(dir) if !ctrl => {
                let step = 3.0 * self.geometry.cell.height as f32;
                let d = match dir {
                    ScrollDirection::Down => step,
                    ScrollDirection::Up => -step,
                    _ => return Some(redraw),
                };
                let v = &self.viewer;
                let max = (v.layout.height * v.view.scale - peek.rect.h as f32).max(0.0);
                let p = self.preview.as_mut()?;
                p.peek.scroll_y = (p.peek.scroll_y + d).clamp(0.0, max);
                redraw = true;
            }
            MouseEventKind::Press(MouseButton::Left) => self.press = Some((x, y)),
            MouseEventKind::Release(MouseButton::Left) => {
                let clicked = self.press.take().is_some_and(|(px, py)| (px - x).hypot(py - y) < CLICK_SLOP);
                if clicked && !ctrl {
                    match link {
                        Some(uri @ LinkTarget::External(_)) => self.follow(uri, now),
                        Some(target) => {
                            self.close_preview();
                            self.follow(target, now);
                        }
                        None => {
                            let target = self.peek_top(&peek);
                            self.close_preview();
                            self.follow(LinkTarget::Internal(target), now);
                        }
                    }
                    redraw = true;
                }
            }
            _ => {}
        }
        Some(redraw)
    }

    /// The jump target that puts what a peek window shows at the top of the screen.
    fn peek_top(&self, peek: &Peek) -> Target {
        let v = &self.viewer;
        let dy = peek.scroll_y / v.view.scale + crate::viewer::TARGET_MARGIN;
        let page = v.layout.page_at(dy);
        let b = v.layout.pages[page];
        Target { page, y: Some((dy - b.y).clamp(0.0, b.h)) }
    }

    /// Remember the current position before a jump.
    fn push_history(&mut self) {
        let here = (self.viewer.top_position(), self.viewer.view.scroll_x);
        if self
            .back
            .last()
            .is_some_and(|(t, _)| t.page == here.0.page && (t.y.unwrap_or(0.0) - here.0.y.unwrap_or(0.0)).abs() < 1.0)
        {
            return;
        }
        self.back.push(here);
        if self.back.len() > HISTORY_LIMIT {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    fn history(&mut self, backwards: bool, now: Instant) {
        let (from, to) =
            if backwards { (&mut self.back, &mut self.forward) } else { (&mut self.forward, &mut self.back) };
        let Some((pos, sx)) = from.pop() else {
            self.notice = Some(
                if backwards { "Already at the oldest position" } else { "Already at the newest position" }.into(),
            );
            return;
        };
        to.push((self.viewer.top_position(), self.viewer.view.scroll_x));
        // Positions were recorded at the screen top; go there exactly.
        let v = &mut self.viewer;
        let b = v.layout.pages[pos.page.min(v.page_count() - 1)];
        let y = (b.y + pos.y.unwrap_or(0.0)) * v.view.scale;
        v.scroll_y.scroll_by(y as f64 - v.scroll_y.target(), now);
        v.scroll_x.jump_to(sx as f64);
    }

    fn follow(&mut self, target: LinkTarget, now: Instant) {
        match target {
            LinkTarget::Internal(t) => {
                self.push_history();
                self.viewer.goto(t, true, now);
            }
            LinkTarget::External(uri) => {
                self.notice = Some(format!("Opening {uri}"));
                self.requests.push(Request::OpenUri(uri));
            }
        }
    }

    /// Terminal cell where the text cursor should be shown, if any.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        self.caret_cell
    }

    /// Requests for the caller (searches to run).
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// Results for one page. Returns true when the screen should be redrawn.
    pub fn search_hits(&mut self, generation: u64, page: usize, rects: Vec<PageRect>, now: Instant) -> bool {
        if !self.search.add(generation, page, rects) {
            return false;
        }
        // After a reload, re-select the hit that was current if it still exists.
        if let Some((p, i)) = self.restore_hit
            && p == page
        {
            self.restore_hit = None;
            if self.search.hits[p].get(i).is_some() {
                self.search.current = Some((p, i));
            }
        }
        // Incremental search (while typing): move to the first match after where
        // the search began. Also after Enter if no match was selected yet.
        if self.search.current.is_none() {
            let from = match (&self.input, self.pending_first_hit) {
                (Some(_), _) => Some(self.search_origin.map_or_else(|| self.viewer.top_position(), |o| o.0)),
                (None, pending) => pending,
            };
            // A hit before `from` is one found by wrapping around: only when
            // every page is searched (later pages may have one still).
            let wrapped = |from: Target, hit: (usize, usize), search: &SearchState| {
                let y = search.hits[hit.0].get(hit.1).map_or(0.0, |r| r.y0);
                (hit.0, y) < (from.page, from.y.unwrap_or(0.0))
            };
            if let Some(from) = from
                && let Some(hit) = self.search.find_from(from, true)
                && self.search.hits[hit.0].get(hit.1).is_some()
                && (self.search.is_complete() || (hit.0 == page && !wrapped(from, hit, &self.search)))
            {
                self.search.current = Some(hit);
                self.pending_first_hit = None;
                self.reveal_hit(hit, now);
            }
        }
        true
    }

    fn reveal_hit(&mut self, hit: (usize, usize), now: Instant) {
        if let Some(t) = self.search.target(hit) {
            // Only scroll when the hit is not already comfortably on screen.
            let (_, sy) = self.viewer.page_to_screen(t.page, 0.0, t.y.unwrap_or(0.0));
            let h = self.viewer.view.screen_h;
            if sy < h * 0.1 || sy > h * 0.85 {
                self.viewer.goto(
                    Target { page: t.page, y: t.y.map(|y| (y - h * 0.3 / self.viewer.view.scale).max(0.0)) },
                    true,
                    now,
                );
            }
        }
    }

    fn start_search(&mut self) {
        let query = self.input.as_ref().map(|i| i.text.clone()).unwrap_or_default();
        self.run_search(query);
    }

    fn run_search(&mut self, query: String) {
        let generation = self.search.start(&query);
        if !query.trim().is_empty() {
            let start_page = self.search_origin.map_or_else(|| self.viewer.top_position().page, |o| o.0.page);
            let doc_generation = self.viewer.generation();
            self.requests.push(Request::Search { generation, query, start_page, doc_generation });
        }
    }

    /// Show a forward-search result: jump to the first box (unless it is already
    /// comfortably visible) and highlight the boxes briefly.
    pub fn forward_result(&mut self, mut boxes: Vec<PdfBox>, now: Instant) {
        // A SyncTeX file older than the PDF (written after it, or left from
        // a build without SyncTeX) can name pages it no longer has.
        let pages = self.viewer.page_count();
        boxes.retain(|b| b.page < pages);
        let Some(first) = boxes.first().copied() else { return };
        // The editor's position wins over a reload anchor still waiting for pages.
        self.text_anchor = None;
        let (_, sy) = self.viewer.page_to_screen(first.page, 0.0, first.rect.y0);
        let h = self.viewer.view.screen_h;
        if sy < h * 0.1 || sy > h * 0.8 {
            self.push_history();
            let offset = h * 0.3 / self.viewer.view.scale;
            self.viewer.goto(Target { page: first.page, y: Some((first.rect.y0 - offset).max(0.0)) }, true, now);
        }
        self.flash = Some((boxes, now + FLASH_DURATION));
    }

    /// When the screen next needs redrawing without any input (a highlight
    /// expiring), if ever.
    /// Move the selection head to the caret under the pointer.
    fn extend_selection(&mut self) -> bool {
        let (sx, sy) = self.pointer;
        let Some(head) = self.caret_at(sx, sy) else { return false };
        let Some(sel) = self.selection.as_mut() else { return false };
        let changed = sel.head != head;
        sel.head = head;
        changed
    }

    /// Whether anything is moving on its own (scroll animation, selection
    /// auto-scroll), so the caller should keep ticking.
    pub fn is_animating(&self) -> bool {
        self.viewer.is_animating() || self.autoscroll != 0.0
    }

    /// Advance animations to `now`. Returns true when the view changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = self.viewer.tick(now);
        if let Some(h) = self.hover.as_mut().filter(|h| now >= h.due) {
            if self.viewer.is_animating() {
                // The link is moving; wait until the page stands still.
                h.due = now + self.preview_delay.unwrap_or(DEFAULT_PREVIEW_DELAY);
            } else if let Some(h) = self.hover.take() {
                self.open_preview(h);
                changed = true;
            }
        }
        if self.preview.as_ref().and_then(|p| p.left_at).is_some_and(|t| now >= t + PREVIEW_GRACE) {
            changed |= self.close_preview();
        }
        if self.text_anchor.as_ref().is_some_and(|a| now >= a.deadline) {
            changed |= self.resolve_text_anchor(true);
        }
        if self.autoscroll != 0.0 {
            let dt = self.autoscroll_at.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
            let v = &mut self.viewer;
            let before = v.view.scroll_y;
            v.jump_to(v.view.scroll_x, before + self.autoscroll * dt);
            changed |= self.viewer.view.scroll_y != before;
            changed |= self.extend_selection();
            self.autoscroll_at = Some(now);
        } else {
            self.autoscroll_at = None;
        }
        changed
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let flash = self.flash.as_ref().map(|f| f.1);
        let anchor = self.text_anchor.as_ref().map(|a| a.deadline);
        let hover = self.hover.as_ref().map(|h| h.due);
        let leave = self.preview.as_ref().and_then(|p| p.left_at).map(|t| t + PREVIEW_GRACE);
        [flash, anchor, hover, leave].into_iter().flatten().min()
    }

    /// The file changed and the document thread has the new version. Keep the
    /// reading position (see `Viewer::replace_pages`), sidebar state, outline
    /// expansion and the search query (re-run on the new text); drop the text
    /// selection and per-page caches. History entries are clamped to the new
    /// page count. See doc/reload.md.
    pub fn reload(&mut self, images: &mut Images, info: DocInfo) {
        self.before_reload();
        self.viewer.doc.borrow_mut().replace(images, info.bounds.clone());
        self.after_reload(images, info);
    }

    /// First half of [`PdfPane::reload`] for documents shared by several panes:
    /// call it on every pane while the old document is still there, then
    /// [`Document::replace`] once, then [`PdfPane::after_reload`] on every pane.
    pub fn before_reload(&mut self) {
        let collapsed = self.outline.collapsed_paths(&self.info.outline);
        let selected_title = self.outline.selected_entry().map(|i| self.info.outline[i].title.clone());
        // Remember the line being read while the old text is still there.
        self.text_anchor = if self.anchor_by_text { self.capture_text_anchor(Instant::now()) } else { None };
        self.reload_state = Some((collapsed, selected_title));
    }

    /// Second half of [`PdfPane::reload`], after the document was replaced.
    pub fn after_reload(&mut self, images: &mut Images, info: DocInfo) {
        let (collapsed, selected_title) = self.reload_state.take().unwrap_or_default();
        self.close_preview();
        self.hover_uri = None;
        self.viewer.document_replaced(images, info.bounds.clone());
        let pages = info.bounds.len();
        self.selection = None;
        self.selecting = false;
        for (t, _) in self.back.iter_mut().chain(self.forward.iter_mut()) {
            t.page = t.page.min(pages - 1);
        }
        self.outline = OutlinePanel::new(&info.outline, &info.labels);
        self.outline.restore_collapsed(&info.outline, &info.labels, &collapsed);
        self.info = info;
        if let Some(title) = selected_title
            && let Some(i) = self.info.outline.iter().position(|e| e.title == title)
        {
            self.outline.select_entry(i);
        }
        let query = self.search.query.clone();
        let current = self.search.current;
        // Generations only go up: a search from before an earlier reload
        // must not pass for the new one.
        let generation = self.search.generation.max(self.search_base);
        self.search = SearchState::new(pages);
        self.search.generation = generation;
        if !query.is_empty() {
            self.run_search(query);
            // Remember which hit was current; it is restored as results arrive.
            self.restore_hit = current;
        }
    }

    fn input_key(&mut self, k: &KeyEvent, now: Instant) -> Action {
        let Some(input) = self.input.as_mut() else { return Action::None };
        let ctrl = k.modifiers.without_locks().contains(Modifiers::CTRL);
        let r = match k.code {
            KeyCode::Escape | KeyCode::Char('c') if k.code == KeyCode::Escape || ctrl => {
                // Cancel: forget the query and go back to where we were.
                self.input = None;
                self.search.clear();
                if let Some((pos, sx, _)) = self.search_origin.take() {
                    self.viewer.goto(pos, false, now);
                    self.viewer.jump_to(sx, self.viewer.view.scroll_y);
                }
                return Action::Redraw;
            }
            KeyCode::Enter => {
                self.input = None;
                let origin = self.search_origin.take();
                if let Some((pos, sx, _)) = origin {
                    self.back.push((pos, sx));
                    self.forward.clear();
                }
                if self.search.is_active() && self.search.current.is_none() {
                    self.pending_first_hit = Some(origin.map_or_else(|| self.viewer.top_position(), |o| o.0));
                }
                if self.search.is_active() && self.search.total() == 0 && self.search.is_complete() {
                    self.message = Some(format!("Pattern not found: {}", self.search.query));
                }
                return Action::Redraw;
            }
            KeyCode::Backspace if input.text.is_empty() => {
                self.input = None;
                self.search.clear();
                return Action::Redraw;
            }
            KeyCode::Backspace => input.backspace(),
            KeyCode::Delete => input.delete(),
            KeyCode::Left => input.left(),
            KeyCode::Right => input.right(),
            KeyCode::Home | KeyCode::Char('a') if k.code == KeyCode::Home || ctrl => {
                input.home();
                EditResult::Moved
            }
            KeyCode::End | KeyCode::Char('e') if k.code == KeyCode::End || ctrl => {
                input.end();
                EditResult::Moved
            }
            KeyCode::Char('w') if ctrl => input.delete_word(),
            KeyCode::Char('u') if ctrl => {
                input.clear();
                EditResult::Changed
            }
            KeyCode::Char(c) if !ctrl => {
                let text = k.text.clone().unwrap_or_else(|| c.to_string());
                input.insert(&text);
                EditResult::Changed
            }
            _ => EditResult::Ignored,
        };
        match r {
            EditResult::Changed => {
                self.start_search();
                Action::Redraw
            }
            EditResult::Moved => Action::Redraw,
            EditResult::Ignored => Action::None,
        }
    }

    /// Bracketed paste: goes into the search prompt when it is open.
    pub fn paste(&mut self, text: &str) -> Action {
        let Some(input) = self.input.as_mut() else { return Action::None };
        input.insert(&text.replace(['\n', '\r'], " "));
        self.start_search();
        Action::Redraw
    }

    fn open_search(&mut self) {
        let v = &self.viewer;
        self.search_origin = Some((v.top_position(), v.view.scroll_x, v.view.scroll_y));
        self.input = Some(LineInput { prompt: "/".into(), ..Default::default() });
        self.search.clear();
        self.pending_first_hit = None;
    }

    fn step_search(&mut self, forward: bool, now: Instant) {
        if !self.search.is_active() {
            return;
        }
        let hit = match self.search.current {
            Some(_) => self.search.step(forward),
            None => {
                let h = self.search.find_from(self.viewer.top_position(), forward);
                self.search.current = h;
                h
            }
        };
        match hit {
            Some(h) => self.reveal_hit(h, now),
            // Not found yet: say so, and (forward) go to the first hit
            // when it comes, as after Enter.
            None if !self.search.is_complete() => {
                if forward {
                    self.pending_first_hit = Some(self.viewer.top_position());
                }
                self.message = Some(format!("Searching: {}", self.search.query));
            }
            None => self.message = Some(format!("Pattern not found: {}", self.search.query)),
        }
    }

    /// Overlay layers on visible pages: search hits and the text selection.
    fn overlay_layers(&mut self, images: &mut Images, layers: &mut Vec<Layer>) {
        let mut rects: Vec<(usize, PageRect, Color)> = Vec::new();
        for page in self.viewer.visible_pages() {
            if self.search.is_active() {
                for (i, r) in self.search.hits[page].iter().enumerate() {
                    let current = self.search.current == Some((page, i));
                    rects.push((page, *r, if current { CURRENT_HIT_COLOR } else { HIT_COLOR }));
                }
            }
            if let Some(sel) = self.selection
                && let Some(Some(text)) = self.viewer.doc.borrow().texts.get(page)
            {
                rects.extend(sel.rects(page, text).into_iter().map(|r| (page, r, SELECTION_COLOR)));
            }
            if let Some((boxes, _)) = &self.flash {
                rects.extend(boxes.iter().filter(|b| b.page == page).map(|b| (page, b.rect, FLASH_COLOR)));
            }
        }
        if self.flash.as_ref().is_some_and(|f| Instant::now() >= f.1) {
            self.flash = None;
            rects.retain(|r| r.2 != FLASH_COLOR);
        }
        let scale = self.viewer.view.scale;
        let area =
            Rect::new(self.viewer.origin_x(), 0, self.viewer.view.screen_w as u32, self.viewer.view.screen_h as u32);
        for (key, (page, r, color)) in (KEY_OVERLAYS..).zip(rects) {
            let (x0, y0) = self.viewer.page_to_screen(page, r.x0, r.y0);
            let w = ((r.x1 - r.x0) * scale).round().max(1.0) as u32;
            let h = ((r.y1 - r.y0) * scale).round().max(1.0) as u32;
            let dst = Rect::new(x0.round() as i32, y0.round() as i32, w, h);
            // Clip to the page area so overlays never cover the UI.
            let Some(vis) = dst.intersect(&area) else { continue };
            let image = *self.overlay_images.entry((w, h, color.0)).or_insert_with(|| {
                let mut c = Canvas::new(w, h);
                c.fill_rect(0.0, 0.0, w as f32, h as f32, color);
                images.add(w, h, c.finish())
            });
            let src = Rect::new(vis.x - dst.x, vis.y - dst.y, vis.w, vis.h);
            layers.push(Layer { key, image, src, dst: vis, z: Z_OVERLAY });
        }
        // Bound the overlay image cache (sizes change with zoom).
        if self.overlay_images.len() > 256 {
            for (_, id) in self.overlay_images.drain() {
                images.remove(id);
            }
        }
    }

    /// Text of a page, extracted on first use from its display list.
    fn text_of_page(&self, page: usize) -> Option<Ref<'_, PageText>> {
        document::text_of_page(&self.viewer.doc, page)
    }

    /// The caret nearest a screen point, on the page under it (or the nearest page).
    fn caret_at(&mut self, sx: f32, sy: f32) -> Option<Caret> {
        let (page, x, y) = self.viewer.screen_to_page_clamped(sx, sy);
        Some((page, self.text_of_page(page)?.caret_at(x, y)))
    }

    fn copy_selection(&mut self) {
        let Some(sel) = self.selection.filter(|s| !s.is_empty()) else { return };
        let (p0, p1) = (sel.ordered().0.0, sel.ordered().1.0);
        // Text comes from pages' display lists; those not loaded (far
        // pages, evicted) are left out: say so rather than copy less quietly.
        let missing = (p0..=p1).filter(|&p| self.text_of_page(p).is_none()).count();
        let text = sel.text(&self.viewer.doc.borrow().texts);
        if !text.is_empty() {
            let n = text.chars().count();
            self.notice = Some(match missing {
                0 => format!("Copied {n} characters"),
                m => format!("Copied {n} characters; {m} page(s) not loaded were left out"),
            });
            self.requests.push(Request::Copy(text));
        }
    }

    /// The keys the pane handles itself, before mappings: any key closes a
    /// link preview, the search prompt takes all keys, the outline its own.
    /// `None`: the key is for mappings (and the pane may need a redraw if
    /// `redraw` became true).
    pub fn key_first(&mut self, k: &KeyEvent, now: Instant, redraw: &mut bool) -> Option<Action> {
        if k.kind == KeyEventKind::Release || matches!(k.code, KeyCode::Modifier(_)) {
            return Some(Action::None);
        }
        if self.close_preview() {
            if k.code == KeyCode::Escape {
                return Some(Action::Redraw);
            }
            *redraw = true;
        }
        self.message = None;
        // The reader moved on; a pending reload anchor would pull them back.
        self.text_anchor = None;
        self.notice = None;
        if self.input.is_some() {
            return Some(self.input_key(k, now));
        }
        if self.sidebar_open
            && let Some(action) = self.sidebar_key(k, now)
        {
            return Some(action);
        }
        None
    }

    pub fn key(&mut self, k: &KeyEvent, now: Instant) -> Action {
        if k.kind == KeyEventKind::Release || matches!(k.code, KeyCode::Modifier(_)) {
            return Action::None;
        }
        // Any key closes a link preview; Esc does nothing else then.
        if self.close_preview() {
            if k.code == KeyCode::Escape {
                return Action::Redraw;
            }
            return match self.key_inner(k, now) {
                Action::None => Action::Redraw,
                a => a,
            };
        }
        self.key_inner(k, now)
    }

    fn key_inner(&mut self, k: &KeyEvent, now: Instant) -> Action {
        self.message = None;
        // The reader moved on; a pending reload anchor would pull them back.
        self.text_anchor = None;
        self.notice = None;
        if self.input.is_some() {
            return self.input_key(k, now);
        }
        if self.sidebar_open
            && let Some(action) = self.sidebar_key(k, now)
        {
            return action;
        }
        let count = self.count.take();
        // Digits are counts (`3G`); 0 only after another digit (alone it is a key).
        if let KeyCode::Char(c @ '0'..='9') = k.code
            && Key::of(k).mods.is_empty()
            && (c != '0' || count.is_some())
        {
            self.count = Some(count.unwrap_or(0).saturating_mul(10).saturating_add(c as usize - '0' as usize));
            return Action::None;
        }
        let Some(cmd) = self.keymap.get(k) else { return Action::None };
        self.command(cmd, count, now)
    }

    /// Run a page-view command (see `keys::Command`), with an optional count.
    pub fn command(&mut self, cmd: Command, count: Option<usize>, now: Instant) -> Action {
        let v = &mut self.viewer;
        let page = v.view.screen_h as f64 * 0.9;
        let half = v.view.screen_h as f64 * 0.5;
        let centre = (v.view.screen_w / 2.0, v.view.screen_h / 2.0);
        match cmd {
            Command::Quit => return Action::Quit,
            Command::Cancel if self.selection.is_some() => self.selection = None,
            Command::Cancel if self.search.is_active() => self.search.clear(),
            Command::Cancel => return Action::Quit,
            Command::Copy => self.copy_selection(),
            Command::ToggleSidebar => self.toggle_sidebar(),
            Command::ScrollDown => wheel_n(v, 1.0, count, now),
            Command::ScrollUp => wheel_n(v, -1.0, count, now),
            Command::ScrollRight => v.scroll_x.wheel(1.0, now),
            Command::ScrollLeft => v.scroll_x.wheel(-1.0, now),
            Command::PageDown => v.scroll_y.scroll_by(page, now),
            Command::PageUp => v.scroll_y.scroll_by(-page, now),
            Command::HalfPageDown => v.scroll_y.scroll_by(half, now),
            Command::HalfPageUp => v.scroll_y.scroll_by(-half, now),
            Command::FirstPage => {
                self.push_history();
                self.viewer.goto_page(count.map_or(0, |c| c.saturating_sub(1)), true, now);
            }
            Command::LastPage => {
                self.push_history();
                match count {
                    Some(c) => self.viewer.goto_page(c.saturating_sub(1), true, now),
                    None => self.viewer.scroll_y.scroll_by(f64::INFINITY, now),
                }
            }
            Command::Back => self.history(true, now),
            Command::Forward => self.history(false, now),
            Command::ZoomIn => v.zoom(ZOOM_STEP, centre),
            Command::ZoomOut => v.zoom(1.0 / ZOOM_STEP, centre),
            Command::FitWidth => v.fit_width(),
            Command::Search => self.open_search(),
            Command::NextMatch => self.step_search(true, now),
            Command::PrevMatch => self.step_search(false, now),
        }
        Action::Redraw
    }

    /// Returns true when the view changed.
    pub fn mouse(&mut self, m: &MouseEvent, now: Instant) -> bool {
        let cell = self.geometry.cell;
        let to_px = |c: i32, size: u32| match m.unit {
            CoordUnit::Pixel => c as f32,
            CoordUnit::Cell => (c as f32 + 0.5) * size as f32,
        };
        self.pointer = (to_px(m.x, cell.width), to_px(m.y, cell.height));
        if matches!(m.kind, MouseEventKind::Press(_) | MouseEventKind::Scroll(_)) {
            self.text_anchor = None;
        }
        let had_preview = self.preview.is_some();
        if let Some(redraw) = self.preview_mouse(m, now) {
            return redraw;
        }
        let closed = had_preview && self.preview.is_none();
        let (x, y) = self.pointer;
        // A selection or drag that moved over the sidebar is still the page's.
        let over_sidebar = self.sidebar_open
            && self.drag.is_none()
            && !self.selecting
            && self.sidebar_rect().contains(x as i32, y as i32);
        if over_sidebar {
            self.hover = None;
            let uri_cleared = self.hover_uri.take().is_some();
            return self.sidebar_mouse(m, now).unwrap_or(false) || closed || uri_cleared;
        }
        // The status bar is not the page under it: no links, no selection or
        // drag starts there (the wheel still scrolls; a drag or selection
        // begun on the page goes on).
        let over_status = y >= self.viewer.view.screen_h
            && self.drag.is_none()
            && !self.selecting
            && !matches!(m.kind, MouseEventKind::Scroll(_));
        if over_status {
            self.hover = None;
            self.pointer_shape = Pointer::Default;
            self.press = None;
            let uri_cleared = self.hover_uri.take().is_some();
            return closed || uri_cleared;
        }
        // Leaving the sidebar clears its hover highlight.
        let mut redraw = self.outline.list.hover.take().is_some() || closed;
        let (x, y) = self.pointer;
        // Links: pointer shape and preview on hover, follow on click (press and
        // release in place).
        self.pointer_shape =
            if self.drag.is_none() && self.link_at(x, y).is_some() { Pointer::Link } else { Pointer::Default };
        redraw |= self.update_hover(m, now);
        match m.kind {
            MouseEventKind::Press(MouseButton::Left) => self.press = Some((x, y)),
            MouseEventKind::Release(MouseButton::Left) => {
                let clicked = self.press.take().is_some_and(|(px, py)| (px - x).hypot(py - y) < CLICK_SLOP);
                if clicked
                    && !m.modifiers.contains(Modifiers::CTRL)
                    && let Some(target) = self.link_at(x, y).map(|(_, l)| l.target.clone())
                {
                    self.close_preview();
                    self.drag = None;
                    self.follow(target, now);
                    return true;
                }
            }
            MouseEventKind::Press(MouseButton::Other(8)) => {
                self.history(true, now);
                return true;
            }
            MouseEventKind::Press(MouseButton::Other(9)) => {
                self.history(false, now);
                return true;
            }
            _ => {}
        }
        self.page_mouse(m, now) || redraw
    }

    fn page_mouse(&mut self, m: &MouseEvent, now: Instant) -> bool {
        let v = &mut self.viewer;
        // Pointer in page-area coordinates.
        let pointer = (self.pointer.0 - v.origin_x() as f32, self.pointer.1);
        let mods = m.modifiers;
        match m.kind {
            MouseEventKind::Scroll(dir) if mods.contains(Modifiers::CTRL) => {
                let f = match dir {
                    ScrollDirection::Up => ZOOM_STEP,
                    ScrollDirection::Down => 1.0 / ZOOM_STEP,
                    _ => return false,
                };
                v.zoom(f, pointer);
            }
            MouseEventKind::Scroll(dir) => {
                let shift = mods.contains(Modifiers::SHIFT);
                match (dir, shift) {
                    (ScrollDirection::Down, false) => v.scroll_y.wheel(1.0, now),
                    (ScrollDirection::Up, false) => v.scroll_y.wheel(-1.0, now),
                    (ScrollDirection::Right, _) | (ScrollDirection::Down, true) => v.scroll_x.wheel(1.0, now),
                    (ScrollDirection::Left, _) | (ScrollDirection::Up, true) => v.scroll_x.wheel(-1.0, now),
                }
            }
            // Left drag over text selects; elsewhere (and with the middle or
            // right button) it pans. Pressing on a link does neither (it is a click).
            // Ctrl+click: SyncTeX inverse search (open the source in the editor).
            MouseEventKind::Press(MouseButton::Left) if mods.contains(Modifiers::CTRL) => {
                let (sx, sy) = self.pointer;
                if let Some((page, x, y)) = self.viewer.screen_to_page(sx, sy) {
                    self.requests.push(Request::Inverse { page, x, y });
                }
                return false;
            }
            MouseEventKind::Press(MouseButton::Left) => {
                let (sx, sy) = self.pointer;
                let had_selection = self.selection.take().is_some();
                if self.link_at(sx, sy).is_some() {
                    return had_selection;
                }
                let over_text = self
                    .viewer
                    .screen_to_page(sx, sy)
                    .is_some_and(|(page, x, y)| self.text_of_page(page).is_some_and(|t| t.has_text_at(x, y)));
                if over_text && let Some(caret) = self.caret_at(sx, sy) {
                    self.selection = Some(Selection { anchor: caret, head: caret });
                    self.selecting = true;
                    return true;
                }
                let v = &self.viewer;
                self.drag = Some((self.pointer, (v.view.scroll_x, v.view.scroll_y)));
                return had_selection;
            }
            MouseEventKind::Press(MouseButton::Middle | MouseButton::Right) => {
                self.drag = Some((self.pointer, (v.view.scroll_x, v.view.scroll_y)));
                return false;
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selecting => {
                // Near (or past) the top or bottom edge, keep scrolling while the
                // button is held; the speed grows with the distance into the zone.
                let h = self.viewer.view.screen_h;
                let zone = self.geometry.cell.height as f32 * 1.5;
                let y = self.pointer.1;
                self.autoscroll = if y < zone {
                    -(zone - y) * AUTOSCROLL_GAIN
                } else if y > h - zone {
                    (y - (h - zone)) * AUTOSCROLL_GAIN
                } else {
                    0.0
                };
                return self.extend_selection();
            }
            MouseEventKind::Drag(_) => {
                let Some((p0, s0)) = self.drag else { return false };
                v.jump_to(s0.0 - (self.pointer.0 - p0.0), s0.1 - (self.pointer.1 - p0.1));
            }
            MouseEventKind::Release(_) => {
                self.drag = None;
                self.autoscroll = 0.0;
                if std::mem::take(&mut self.selecting) {
                    if self.selection.is_some_and(|s| s.is_empty()) {
                        self.selection = None;
                    } else {
                        self.copy_selection();
                    }
                    return true;
                }
                return false;
            }
            _ => return false,
        }
        true
    }
}

fn contains(r: &PageRect, x: f32, y: f32) -> bool {
    x >= r.x0 && x <= r.x1 && y >= r.y0 && y <= r.y1
}

/// Pixel size of the page area: the screen minus the status bar row.
fn page_area(g: Geometry) -> (u32, u32) {
    (g.width_px(), g.height_px().saturating_sub(g.cell.height).max(1))
}

fn wheel_n(v: &mut Viewer, dir: f64, count: Option<usize>, now: Instant) {
    for _ in 0..count.unwrap_or(1).min(100) {
        v.scroll_y.wheel(dir, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf;
    use termpix_core::geom::CellSize;
    use termpix_core::input::{CoordUnit, MouseEvent, MouseEventKind};

    /// A session on the fixture with every page loaded, at 900x600 pixels.
    fn session() -> PdfPane {
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        let info = pdf::doc_info(&doc).unwrap();
        let geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };
        let mut s = PdfPane::new("sample.pdf".into(), info, geometry);
        for p in 0..s.viewer.page_count() {
            s.viewer.page_loaded(0, p, pdf::display_list(&doc, p).unwrap());
            s.page_links(p, pdf::page_links(&doc, p, s.info.bounds[p]).unwrap());
        }
        s
    }

    fn mouse(s: &mut PdfPane, kind: MouseEventKind, (x, y): (f32, f32)) -> bool {
        let ev = MouseEvent { kind, x: x as i32, y: y as i32, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE };
        s.mouse(&ev, Instant::now())
    }

    /// Screen position of the middle of the first character of `needle` on page 0.
    fn char_pos(s: &mut PdfPane, needle: &str, offset: usize) -> (f32, f32) {
        let text = s.text_of_page(0).unwrap().clone();
        let all: String = text.chars.iter().map(|c| c.c).collect();
        let i = all[..all.find(needle).unwrap()].chars().count() + offset;
        let r = text.chars[i].rect;
        s.viewer.page_to_screen(0, (r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0)
    }

    #[test]
    fn forward_results_past_the_last_page_are_ignored() {
        let mut s = session();
        let pages = s.viewer.page_count();
        let rect = PageRect { x0: 10.0, y0: 10.0, x1: 50.0, y1: 20.0 };
        let top = s.viewer.view_state().top;
        s.forward_result(vec![PdfBox { page: pages + 3, rect }], Instant::now());
        assert!(s.flash.is_none());
        assert_eq!(s.viewer.view_state().top.page, top.page, "no jump");
        // Valid boxes after an invalid one still count.
        s.forward_result(vec![PdfBox { page: pages, rect }, PdfBox { page: pages - 1, rect }], Instant::now());
        assert_eq!(s.flash.as_ref().map(|f| f.0.len()), Some(1));
    }

    #[test]
    fn typing_a_search_does_not_wrap_to_an_earlier_hit_before_the_rest_is_searched() {
        let mut s = session();
        let now = Instant::now();
        s.viewer.goto(Target { page: 1, y: Some(400.0) }, false, now);
        s.key(&KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE), now);
        s.key(&KeyEvent::new(KeyCode::Char('x'), Modifiers::NONE), now);
        let generation = match s.take_requests().as_slice() {
            [Request::Search { generation, start_page: 1, .. }] => *generation,
            other => panic!("{other:?}"),
        };
        let rect = |y| PageRect { x0: 10.0, y0: y, x1: 50.0, y1: y + 10.0 };
        // The page the search started on comes first: its only hit is above
        // where the search began.
        s.search_hits(generation, 1, vec![rect(10.0)], now);
        assert_eq!(s.search.current, None, "a later page may have one");
        s.search_hits(generation, 2, vec![rect(100.0)], now);
        assert_eq!(s.search.current, Some((2, 0)));
    }

    #[test]
    fn next_match_before_the_search_finished_waits_for_it() {
        let mut s = session();
        let now = Instant::now();
        for ch in ['/', 'x'] {
            s.key(&KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE), now);
        }
        s.key(&KeyEvent::new(KeyCode::Enter, Modifiers::NONE), now);
        let generation = match s.take_requests().last() {
            Some(Request::Search { generation, .. }) => *generation,
            other => panic!("{other:?}"),
        };
        s.pending_first_hit = None;
        s.key(&KeyEvent::new(KeyCode::Char('n'), Modifiers::NONE), now);
        assert!(s.message.as_deref().is_some_and(|m| m.starts_with("Searching")), "{:?}", s.message);
        let rect = PageRect { x0: 10.0, y0: 100.0, x1: 50.0, y1: 110.0 };
        s.search_hits(generation, 0, vec![rect], now);
        assert_eq!(s.search.current, Some((0, 0)), "the hit it waited for");
    }

    #[test]
    fn the_status_bar_is_not_the_page_under_it() {
        let mut s = session();
        let y = s.viewer.view.screen_h + 5.0;
        let x = s.viewer.origin_x() as f32 + s.viewer.view.screen_w / 2.0;
        mouse(&mut s, MouseEventKind::Press(MouseButton::Left), (x, y));
        assert!(s.drag.is_none() && !s.selecting, "nothing starts there");
        mouse(&mut s, MouseEventKind::Release(MouseButton::Left), (x, y));
        assert!(s.take_requests().is_empty(), "no link followed");
    }

    #[test]
    fn copies_say_when_pages_not_loaded_were_left_out() {
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        let info = pdf::doc_info(&doc).unwrap();
        let geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };
        let mut s = PdfPane::new("sample.pdf".into(), info, geometry);
        // Page 1 not loaded.
        for p in [0, 2] {
            s.viewer.page_loaded(0, p, pdf::display_list(&doc, p).unwrap());
        }
        s.selection = Some(crate::selection::Selection { anchor: (0, 0), head: (2, 5) });
        s.copy_selection();
        assert!(matches!(s.take_requests()[..], [Request::Copy(_)]));
        let notice = s.notice.clone().unwrap_or_default();
        assert!(notice.contains("1 page(s) not loaded were left out"), "{notice}");
    }

    #[test]
    fn tiny_panes_draw_without_panicking() {
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        let info = pdf::doc_info(&doc).unwrap();
        for (cols, rows, cw, ch) in [(1, 1, 1, 1), (2, 1, 10, 20), (90, 1, 10, 1)] {
            let geometry = Geometry { cols, rows, cell: CellSize { width: cw, height: ch } };
            let mut s = PdfPane::new("sample.pdf".into(), info.clone(), geometry);
            s.sidebar_open = true;
            let mut images = Images::new();
            s.frame(&mut images);
        }
    }

    #[test]
    fn a_selection_released_over_the_sidebar_ends_and_copies() {
        let mut s = session();
        let a = char_pos(&mut s, "temzdf sample", 0);
        s.sidebar_open = true;
        let side = s.sidebar_rect();
        let over = ((side.x + 5) as f32, a.1);
        assert!(a.0 > over.0, "the text is right of the sidebar");
        mouse(&mut s, MouseEventKind::Press(MouseButton::Left), (a.0 + 1.0, a.1));
        mouse(&mut s, MouseEventKind::Drag(MouseButton::Left), (a.0 + 40.0, a.1));
        mouse(&mut s, MouseEventKind::Drag(MouseButton::Left), over);
        mouse(&mut s, MouseEventKind::Release(MouseButton::Left), over);
        assert!(!s.selecting, "the release reached the page");
        assert!(matches!(s.take_requests()[..], [Request::Copy(_)]));
    }

    #[test]
    fn drag_over_text_selects_and_copies() {
        let mut s = session();
        let a = char_pos(&mut s, "temzdf sample", 0);
        let b = char_pos(&mut s, "temzdf sample document", "temzdf sample document".len() - 1);
        // Pressing a few pixels left of the text still selects (the e2e case).
        mouse(&mut s, MouseEventKind::Press(MouseButton::Left), (a.0 - 8.0, a.1));
        mouse(&mut s, MouseEventKind::Drag(MouseButton::Left), ((a.0 + b.0) / 2.0, a.1));
        mouse(&mut s, MouseEventKind::Drag(MouseButton::Left), (b.0 + 4.0, b.1));
        mouse(&mut s, MouseEventKind::Release(MouseButton::Left), (b.0 + 4.0, b.1));
        assert_eq!(s.take_requests(), vec![Request::Copy("temzdf sample document".into())]);
    }

    fn info(name: &str) -> DocInfo {
        pdf::doc_info(&pdf::open(&pdf::fixture(name)).unwrap()).unwrap()
    }

    fn comp() -> Images {
        Images::new()
    }

    #[test]
    fn reload_keeps_the_reading_position() {
        let mut s = session();
        let mut c = comp();
        let b = s.viewer.layout.pages[1];
        // Top of the screen 40% into page 2.
        s.viewer.jump_to(0.0, (b.y + 0.4 * b.h) * s.viewer.view.scale);
        let before = s.viewer.top_position();
        s.reload(&mut c, info("sample.pdf"));
        let after = s.viewer.top_position();
        assert_eq!(after.page, before.page);
        assert!((after.y.unwrap() - before.y.unwrap()).abs() < 0.5, "{before:?} {after:?}");
        assert_eq!(s.viewer.generation(), 1);
    }

    #[test]
    fn text_anchor_keeps_the_line_in_place_when_text_is_inserted_above() {
        let mut s = session();
        s.anchor_by_text = true;
        let mut c = comp();
        let b = s.viewer.layout.pages[0];
        s.viewer.jump_to(0.0, (b.y + 0.3 * b.h) * s.viewer.view.scale);
        s.reload(&mut c, info("sample-insert.pdf"));
        let anchor = s.text_anchor.clone().expect("a line to anchor on");
        assert!(anchor.line.contains("exercises"), "below the inserted text: {anchor:?}");
        let position_based = s.viewer.view.scroll_y;
        let doc = pdf::open(&pdf::fixture("sample-insert.pdf")).unwrap();
        for p in 0..s.viewer.page_count() {
            s.viewer.page_loaded(1, p, pdf::display_list(&doc, p).unwrap());
            s.page_ready();
        }
        assert!(s.text_anchor.is_none());
        assert_ne!(s.viewer.view.scroll_y, position_based, "the inserted text moved the line");
        // The same line is back at the same height on screen.
        let (page, _, y) = s.viewer.screen_to_page_clamped(450.0, anchor.screen_y + 1.0);
        let text = s.text_of_page(page).unwrap().clone();
        let line = text
            .lines
            .iter()
            .find(|l| text.chars[l.start..l.end].iter().map(|c| c.c).collect::<String>() == anchor.line)
            .expect("the line is on the page under its old height");
        assert!((line.rect.y0 - y).abs() < 2.0, "{} vs {y}", line.rect.y0);
    }

    #[test]
    fn input_cancels_a_pending_text_anchor() {
        let mut s = session();
        s.anchor_by_text = true;
        let mut c = comp();
        s.reload(&mut c, info("sample-insert.pdf"));
        assert!(s.text_anchor.is_some());
        mouse(&mut s, MouseEventKind::Scroll(ScrollDirection::Down), (450.0, 300.0));
        assert!(s.text_anchor.is_none());
    }

    #[test]
    fn reload_clamps_to_the_last_page_and_keeps_the_fraction() {
        let mut s = session();
        let mut c = comp();
        let b = s.viewer.layout.pages[2];
        s.viewer.jump_to(0.0, (b.y + 0.25 * b.h) * s.viewer.view.scale);
        s.reload(&mut c, info("sample-short.pdf"));
        assert_eq!(s.viewer.page_count(), 2);
        let after = s.viewer.top_position();
        // Page 3 no longer exists: last page, same fraction (unless the view is
        // clamped at the end of the document).
        assert_eq!(after.page, 1);
        let h = s.viewer.layout.pages[1].h;
        let at_end = s.viewer.view.scroll_y >= s.viewer.view.max_scroll_y(&s.viewer.layout) - 0.5;
        assert!(at_end || (after.y.unwrap() / h - 0.25).abs() < 0.01, "{after:?}");
    }

    #[test]
    fn reload_reruns_the_search_and_keeps_the_sidebar() {
        let mut s = session();
        let mut c = comp();
        let now = Instant::now();
        s.key(&KeyEvent::new(KeyCode::Tab, Modifiers::NONE), now);
        s.key(&KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE), now);
        for ch in "needle".chars() {
            s.key(&KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE), now);
        }
        s.key(&KeyEvent::new(KeyCode::Enter, Modifiers::NONE), now);
        // Typing searched for each prefix; the last one counts.
        let first = match s.take_requests().last() {
            Some(Request::Search { generation, .. }) => *generation,
            other => panic!("{other:?}"),
        };
        s.reload(&mut c, info("sample-short.pdf"));
        assert!(s.sidebar_open);
        let second = match s.take_requests().as_slice() {
            [Request::Search { query, doc_generation: 1, generation, .. }] => {
                assert_eq!(query, "needle");
                *generation
            }
            other => panic!("{other:?}"),
        };
        // Each reload's search is new: results of an earlier one are stale.
        s.reload(&mut c, info("sample-short.pdf"));
        let third = match s.take_requests().as_slice() {
            [Request::Search { generation, .. }] => *generation,
            other => panic!("{other:?}"),
        };
        assert!(first < second && second < third, "{first} {second} {third}");
        assert!(!s.search_hits(second, 0, Vec::new(), now), "the previous reload's results");
    }

    #[test]
    fn ctrl_wheel_zooms_around_the_pointer() {
        let mut s = session();
        let ctrl_wheel = |s: &mut PdfPane, dir, at: (f32, f32)| {
            let ev = MouseEvent {
                kind: MouseEventKind::Scroll(dir),
                x: at.0 as i32,
                y: at.1 as i32,
                unit: CoordUnit::Pixel,
                modifiers: Modifiers::CTRL,
            };
            s.mouse(&ev, Instant::now());
            s.tick(Instant::now() + std::time::Duration::from_secs(5));
        };
        for (dir, at) in [
            (ScrollDirection::Up, (700.0, 400.0)),
            (ScrollDirection::Up, (150.0, 100.0)),
            (ScrollDirection::Down, (600.0, 300.0)),
        ] {
            let before = s.viewer.screen_to_page(at.0, at.1).unwrap();
            ctrl_wheel(&mut s, dir, at);
            let after = s.viewer.screen_to_page(at.0, at.1).unwrap();
            assert_eq!(before.0, after.0, "{dir:?} at {at:?}");
            assert!(
                (before.1 - after.1).abs() < 0.5 && (before.2 - after.2).abs() < 0.5,
                "{dir:?} at {at:?}: {before:?} -> {after:?}"
            );
        }
    }

    #[test]
    fn enter_before_the_results_arrive_still_selects_the_first_hit() {
        let mut s = session();
        let now = Instant::now();
        s.key(&KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE), now);
        for ch in "needle".chars() {
            s.key(&KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE), now);
        }
        // Typed quickly (or through a slow link): Enter comes before any hit.
        s.key(&KeyEvent::new(KeyCode::Enter, Modifiers::NONE), now);
        let Some(Request::Search { generation, query, .. }) = s.take_requests().pop() else { panic!("a search") };
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        for p in 0..s.viewer.page_count() {
            let rects = pdf::search_page(&doc, p, s.info.bounds[p], &query).unwrap();
            s.search_hits(generation, p, rects, now);
        }
        assert_eq!(s.search.current, Some((0, 0)));
        assert!(s.search_hint().contains("1/4"), "{}", s.search_hint());
    }

    #[test]
    fn selection_drag_at_the_edge_scrolls_by_itself() {
        let mut s = session();
        let a = char_pos(&mut s, "temzdf sample", 0);
        mouse(&mut s, MouseEventKind::Press(MouseButton::Left), (a.0, a.1));
        // Into the bottom edge zone, then hold still.
        let bottom = s.viewer.view.screen_h - 5.0;
        mouse(&mut s, MouseEventKind::Drag(MouseButton::Left), (a.0, bottom));
        assert!(s.is_animating(), "auto-scroll started");
        let before = s.viewer.view.scroll_y;
        let head_before = s.selection.unwrap().head;
        let t0 = Instant::now();
        for i in 1..=30 {
            s.tick(t0 + std::time::Duration::from_millis(16 * i));
        }
        assert!(s.viewer.view.scroll_y > before + 100.0, "scrolled {}", s.viewer.view.scroll_y - before);
        assert!(s.selection.unwrap().head > head_before, "selection grew with the scroll");
        mouse(&mut s, MouseEventKind::Release(MouseButton::Left), (a.0, bottom));
        assert!(!s.is_animating());
    }

    #[test]
    fn drag_in_margin_pans() {
        let mut s = session();
        let before = s.viewer.view.scroll_y;
        mouse(&mut s, MouseEventKind::Press(MouseButton::Left), (20.0, 400.0));
        mouse(&mut s, MouseEventKind::Drag(MouseButton::Left), (20.0, 300.0));
        mouse(&mut s, MouseEventKind::Release(MouseButton::Left), (20.0, 300.0));
        assert_eq!(s.viewer.view.scroll_y, before + 100.0);
        assert!(s.take_requests().is_empty());
    }

    /// Rest the pointer on the first internal link from a later page back to
    /// page 1 (scrolled to `height` as a fraction of the screen) until its preview
    /// opens. Returns where the pointer is.
    fn open_back_link_preview(s: &mut PdfPane, height: f32) -> (f32, f32) {
        let (page, rect) = (1..s.viewer.page_count())
            .find_map(|p| {
                let doc = s.viewer.doc.borrow();
                let l =
                    doc.links(p)?.iter().find(|l| matches!(l.target, LinkTarget::Internal(Target { page: 0, .. })))?;
                Some((p, l.rect))
            })
            .expect("a link back to page 1");
        let b = s.viewer.layout.pages[page];
        let scale = s.viewer.view.scale;
        let y = ((b.y + rect.y0) * scale - s.viewer.view.screen_h * height).max(0.0);
        s.viewer.jump_to(s.viewer.view.scroll_x, y);
        let at = s.viewer.page_to_screen(page, (rect.x0 + rect.x1) / 2.0, (rect.y0 + rect.y1) / 2.0);
        mouse(s, MouseEventKind::Move, at);
        assert!(s.preview.is_none(), "not before the delay");
        let due = s.next_deadline().expect("a pending preview");
        assert!(s.tick(due + Duration::from_millis(1)));
        assert!(s.preview.is_some());
        at
    }

    /// Screen position of a page-space point in the open preview.
    fn in_preview(s: &PdfPane, page: usize, x: f32, y: f32) -> (f32, f32) {
        let p = s.preview.as_ref().unwrap().peek;
        let b = s.viewer.layout.pages[page];
        let scale = s.viewer.view.scale;
        (p.rect.x as f32 + (b.x + x) * scale - p.scroll_x, p.rect.y as f32 + (b.y + y) * scale - p.scroll_y)
    }

    #[test]
    fn resting_on_a_link_previews_its_target_with_clickable_urls() {
        let mut s = session();
        open_back_link_preview(&mut s, 0.33);
        let mut c = comp();
        let (frame, needs) = s.frame(&mut c);
        let layers = frame.layers;
        assert!(layers.iter().any(|l| l.z == Z_PREVIEW), "the border");
        assert!(layers.iter().any(|l| l.z == Z_PREVIEW + 1), "the window");
        assert!(matches!(&needs[0], Need::Render(j) if j.key.page == 0), "the preview's tiles first");

        let url = s
            .viewer
            .doc
            .borrow()
            .links(0)
            .unwrap()
            .iter()
            .find(|l| l.target == LinkTarget::External("https://example.com/".into()))
            .expect("the URL on page 1")
            .rect;
        let r = s.preview.as_ref().unwrap().peek.rect;
        let mut at = in_preview(&s, 0, (url.x0 + url.x1) / 2.0, (url.y0 + url.y1) / 2.0);
        for _ in 0..20 {
            if r.contains(at.0 as i32, at.1 as i32) {
                break;
            }
            mouse(&mut s, MouseEventKind::Scroll(ScrollDirection::Down), (r.x as f32 + 5.0, r.y as f32 + 5.0));
            at = in_preview(&s, 0, (url.x0 + url.x1) / 2.0, (url.y0 + url.y1) / 2.0);
        }
        mouse(&mut s, MouseEventKind::Move, at);
        assert_eq!(s.pointer(), Pointer::Link);
        assert_eq!(s.hover_uri.as_deref(), Some("https://example.com/"), "shown in the status bar");
        mouse(&mut s, MouseEventKind::Press(MouseButton::Left), at);
        mouse(&mut s, MouseEventKind::Release(MouseButton::Left), at);
        assert_eq!(s.take_requests(), vec![Request::OpenUri("https://example.com/".into())]);
        assert!(s.preview.is_some(), "stays open");
    }

    #[test]
    fn previews_go_below_the_link_or_above_it_near_the_bottom() {
        let mut s = session();
        let (_, y) = open_back_link_preview(&mut s, 0.33);
        assert!(s.preview.as_ref().unwrap().peek.rect.y as f32 > y);
        let mut s = session();
        let (_, y) = open_back_link_preview(&mut s, 0.9);
        let r = s.preview.as_ref().unwrap().peek.rect;
        assert!((r.bottom() as f32) < y, "{r:?} above {y}");
    }

    #[test]
    fn the_wheel_over_a_preview_scrolls_it_not_the_page() {
        let mut s = session();
        open_back_link_preview(&mut s, 0.33);
        let page_y = s.viewer.view.scroll_y;
        let peek = s.preview.as_ref().unwrap().peek;
        let centre = (peek.rect.x as f32 + peek.rect.w as f32 / 2.0, peek.rect.y as f32 + peek.rect.h as f32 / 2.0);
        assert!(mouse(&mut s, MouseEventKind::Scroll(ScrollDirection::Down), centre));
        assert!(s.preview.as_ref().unwrap().peek.scroll_y > peek.scroll_y);
        assert_eq!(s.viewer.view.scroll_y, page_y);
        assert!(!s.viewer.is_animating());
    }

    #[test]
    fn clicking_a_preview_jumps_there_and_keys_close_it() {
        let mut s = session();
        open_back_link_preview(&mut s, 0.33);
        let r = s.preview.as_ref().unwrap().peek.rect;
        let blank = (r.x as f32 + 2.0, r.y as f32 + 2.0);
        mouse(&mut s, MouseEventKind::Press(MouseButton::Left), blank);
        mouse(&mut s, MouseEventKind::Release(MouseButton::Left), blank);
        assert!(s.preview.is_none());
        s.tick(Instant::now() + Duration::from_secs(5));
        assert_eq!(s.viewer.top_position().page, 0);
        assert_eq!(s.back.len(), 1, "back returns to the link");

        let mut s = session();
        open_back_link_preview(&mut s, 0.33);
        let esc = KeyEvent::new(KeyCode::Escape, Modifiers::NONE);
        assert_eq!(s.key(&esc, Instant::now()), Action::Redraw, "closes, does not quit");
        assert!(s.preview.is_none());
    }

    #[test]
    fn previews_wait_for_the_pointer_and_close_after_it_leaves() {
        let mut s = session();
        let at = open_back_link_preview(&mut s, 0.33);
        // Away from both the link and the preview: closes after a grace period.
        mouse(&mut s, MouseEventKind::Move, (at.0, 2.0));
        let now = Instant::now();
        s.tick(now + Duration::from_millis(50));
        assert!(s.preview.is_some(), "time to reach the preview");
        s.tick(now + PREVIEW_GRACE + Duration::from_millis(50));
        assert!(s.preview.is_none());

        // Passing over the link does not open it.
        mouse(&mut s, MouseEventKind::Move, at);
        mouse(&mut s, MouseEventKind::Move, (at.0, 2.0));
        assert!(s.next_deadline().is_none());
        s.tick(Instant::now() + Duration::from_secs(1));
        assert!(s.preview.is_none());

        // Turned off in the config.
        s.preview_delay = None;
        mouse(&mut s, MouseEventKind::Move, at);
        assert!(s.next_deadline().is_none());
    }

    #[test]
    fn the_terminal_cursor_never_covers_typed_text() {
        let mut s = session();
        let mut c = comp();
        let now = Instant::now();
        s.key(&KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE), now);
        for ch in "needle".chars() {
            s.key(&KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE), now);
            s.frame(&mut c);
            let input = s.input.clone().unwrap();
            let h = s.bar_height();
            let caret_x = input.caret_x(&mut s.text, &s.theme, h);
            let (col, _) = s.cursor().unwrap();
            assert!(
                col as f32 * 10.0 >= caret_x,
                "after {:?}: cell {col} starts left of the caret at {caret_x}",
                input.text
            );
        }
    }

    /// Do a pane's rendering work inline until its frame needs nothing more.
    fn settle(s: &mut PdfPane, images: &mut Images) {
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        loop {
            let (_, needs) = s.frame(images);
            if needs.is_empty() {
                return;
            }
            for need in needs {
                match need {
                    Need::LoadPage(p) => s.viewer.page_loaded(0, p, pdf::display_list(&doc, p).unwrap()),
                    Need::Render(job) => {
                        let pixels = pdf::render(&job.list, job.bounds, job.scale, job.area).unwrap();
                        let r =
                            crate::doc_cache::Rendered { key: job.key, width: job.area.w, height: job.area.h, pixels };
                        s.viewer.rendered(images, r);
                    }
                }
            }
        }
    }

    #[test]
    fn panes_on_one_document_share_its_tiles_and_links() {
        let mut a = session();
        let mut images = comp();
        settle(&mut a, &mut images);
        let shown = images.len();
        let mut b = PdfPane::with_document("sample.pdf".into(), a.info.clone(), a.viewer.doc.clone(), a.geometry());
        let (frame, needs) = b.frame(&mut images);
        assert!(needs.is_empty(), "the second view needs no new tiles");
        assert!(frame.layers.len() > 1);
        // Only its own UI images (status bar) were added.
        assert!(images.len() <= shown + 2, "{} vs {shown}", images.len());
        let link = a.viewer.doc.borrow().links(0).unwrap()[0].clone();
        let (x, y) = ((link.rect.x0 + link.rect.x1) / 2.0, (link.rect.y0 + link.rect.y1) / 2.0);
        assert_eq!(b.link_on_page(0, x, y), Some(link), "links loaded once serve both");
    }
}
