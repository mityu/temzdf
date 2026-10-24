//! Everything the viewer shows and how it reacts to input, independent of the
//! terminal and of threads: the page viewer, the UI chrome (status bar, ...),
//! and the key/mouse bindings. `app.rs` drives it interactively; headless mode
//! and tests drive it directly.

use std::collections::HashMap;
use std::time::Instant;

use termpix::compositor::{Compositor, ImageId, Layer};
use termpix::geom::{Geometry, Rect};
use termpix::input::{
    CoordUnit, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind, ScrollDirection,
};
use termpix_ui::{Canvas, Color, EditResult, LineInput, ListView, StatusBar, Text, Theme};

use crate::outline::{self, OutlinePanel};
use crate::pdf::{DocInfo, Link, LinkTarget, PageRect, Target};
use crate::search::SearchState;
use crate::selection::{Caret, PageText, Selection};
use crate::synctex::PdfBox;
use crate::viewer::{LAYER_KEYS_END, Need, Viewer};

pub const ZOOM_STEP: f32 = 1.1;
/// UI layers sit above page tiles but, like them, below terminal text.
const Z_UI: i32 = -1_073_741_800;
const KEY_STATUS: u32 = LAYER_KEYS_END;
const KEY_SIDEBAR: u32 = LAYER_KEYS_END + 1;
const KEY_INPUT: u32 = LAYER_KEYS_END + 2;
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

/// Mouse pointer shapes (OSC 22 names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointer {
    Default,
    Link,
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
}

struct WidgetImage {
    image: ImageId,
    state: WidgetState,
    size: (u32, u32),
}

pub struct Session {
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
    /// The search prompt, while typing a query.
    input: Option<LineInput>,
    /// Where the view was when the prompt opened (restored on Esc).
    search_origin: Option<(Target, f32, f32)>,
    requests: Vec<Request>,
    /// Solid overlay images by (width, height, colour).
    overlay_images: HashMap<(u32, u32, [u8; 4]), ImageId>,
    /// Page text for selection, extracted on demand.
    texts: Vec<Option<PageText>>,
    selection: Option<Selection>,
    /// A left drag is extending the selection.
    selecting: bool,
    /// Search hit to re-select once a re-run search (after reload) reaches it.
    restore_hit: Option<(usize, usize)>,
    /// The file changed and a reload is pending (shown in the status bar).
    pub reloading: bool,
    /// Forward-search result highlighted until the deadline.
    flash: Option<(Vec<PdfBox>, Instant)>,
    /// Terminal cell for the text cursor (input caret), if shown.
    caret_cell: Option<(u16, u16)>,
    /// Links per page, filled as pages load.
    links: Vec<Option<Vec<Link>>>,
    pointer_shape: Pointer,
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

impl Session {
    pub fn new(name: String, info: DocInfo, geometry: Geometry) -> Self {
        let (w, h) = page_area(geometry);
        let outline = OutlinePanel::new(&info.outline, &info.labels);
        let pages = info.bounds.len();
        Self {
            outline,
            sidebar_open: false,
            search: SearchState::new(pages),
            input: None,
            search_origin: None,
            requests: Vec::new(),
            overlay_images: HashMap::new(),
            texts: vec![None; pages],
            selection: None,
            selecting: false,
            restore_hit: None,
            reloading: false,
            flash: None,
            caret_cell: None,
            links: vec![None; pages],
            pointer_shape: Pointer::Default,
            press: None,
            back: Vec::new(),
            forward: Vec::new(),
            viewer: Viewer::new(info.bounds.clone(), w, h),
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

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    pub fn resize(&mut self, geometry: Geometry) {
        self.geometry = geometry;
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

    /// Layers for this frame and the work still needed.
    pub fn frame(&mut self, comp: &mut Compositor) -> (Vec<Layer>, Vec<Need>) {
        let (mut layers, needs) = self.viewer.frame(comp);
        // Zoom as a percentage of 96 dpi ("100%" = actual size on a typical screen).
        let zoom = (self.viewer.view.scale * 72.0 / 96.0 * 100.0).round();
        let (w, h) = (self.geometry.width_px(), self.bar_height());
        let y = self.geometry.height_px().saturating_sub(h) as i32;
        let bar = Rect::new(0, y, w, h);
        self.caret_cell = None;
        if let Some(mut input) = self.input.clone() {
            input.hint = self.search_hint();
            layers.push(self.widget(comp, KEY_INPUT, WidgetState::Input(input.clone()), bar));
            // Put the terminal cursor at the caret so IME preedit shows there.
            let caret_x = input.caret_x(&mut self.text, &self.theme, h);
            let cell = self.geometry.cell;
            self.caret_cell = Some(((caret_x as u32 / cell.width) as u16, (y as u32 / cell.height) as u16));
        } else {
            let mut right = format!("{}   {zoom}%", self.page_text());
            if self.search.is_active() {
                right = format!("/{} {}   {right}", self.search.query, self.search_hint());
            }
            if self.reloading {
                right = format!("reloading…   {right}");
            }
            let status =
                StatusBar { left: self.name.clone(), right, error: self.message.clone(), notice: self.notice.clone() };
            layers.push(self.widget(comp, KEY_STATUS, WidgetState::Status(status), bar));
        }
        self.overlay_layers(comp, &mut layers);
        if self.sidebar_open {
            let r = self.sidebar_rect();
            let state = WidgetState::List(self.outline.list.clone(), self.row_height());
            layers.push(self.widget(comp, KEY_SIDEBAR, state, r));
        }
        (layers, needs)
    }

    fn widget(&mut self, comp: &mut Compositor, key: u32, state: WidgetState, rect: Rect) -> Layer {
        let size = (rect.w, rect.h);
        let fresh = self.widgets.get(&key).is_some_and(|w| w.state == state && w.size == size);
        if !fresh {
            let px = match &state {
                WidgetState::Status(s) => s.render(&mut self.text, &self.theme, rect.w, rect.h),
                WidgetState::List(l, rh) => l.render(&mut self.text, &self.theme, rect.w, rect.h, *rh),
                WidgetState::Input(i) => i.render(&mut self.text, &self.theme, rect.w, rect.h).0,
            };
            let image = match self.widgets.get(&key) {
                Some(w) => {
                    comp.replace_image(w.image, rect.w, rect.h, px);
                    w.image
                }
                None => comp.add_image(rect.w, rect.h, px),
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

    pub fn page_links(&mut self, page: usize, links: Vec<Link>) {
        if let Some(slot) = self.links.get_mut(page) {
            *slot = Some(links);
        }
    }

    pub fn pointer(&self) -> Pointer {
        self.pointer_shape
    }

    /// The link under a screen pixel.
    fn link_at(&self, sx: f32, sy: f32) -> Option<&Link> {
        let (page, x, y) = self.viewer.screen_to_page(sx, sy)?;
        self.links
            .get(page)?
            .as_ref()?
            .iter()
            .find(|l| x >= l.rect.x0 && x <= l.rect.x1 && y >= l.rect.y0 && y <= l.rect.y1)
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
        // the search began.
        if self.input.is_some() && self.search.current.is_none() {
            let from = self.search_origin.map_or_else(|| self.viewer.top_position(), |o| o.0);
            if let Some(hit) = self.search.find_from(from, true)
                && self.search.hits[hit.0].get(hit.1).is_some()
                && (self.search.is_complete() || hit.0 == page)
            {
                self.search.current = Some(hit);
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
    pub fn forward_result(&mut self, boxes: Vec<PdfBox>, now: Instant) {
        let Some(first) = boxes.first().copied() else { return };
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
    pub fn next_deadline(&self) -> Option<Instant> {
        self.flash.as_ref().map(|f| f.1)
    }

    /// The file changed and the document thread has the new version. Keep the
    /// reading position (see `Viewer::replace_pages`), sidebar state, outline
    /// expansion and the search query (re-run on the new text); drop the text
    /// selection and per-page caches. History entries are clamped to the new
    /// page count. See doc/reload.md.
    pub fn reload(&mut self, comp: &mut Compositor, info: DocInfo) {
        let collapsed = self.outline.collapsed_paths(&self.info.outline);
        let selected_title = self.outline.selected_entry().map(|i| self.info.outline[i].title.clone());
        self.viewer.replace_pages(comp, info.bounds.clone());
        let pages = info.bounds.len();
        self.links = vec![None; pages];
        self.texts = vec![None; pages];
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
        self.search = SearchState::new(pages);
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
                if let Some((pos, sx, _)) = self.search_origin.take() {
                    self.back.push((pos, sx));
                    self.forward.clear();
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
            None => self.message = Some(format!("Pattern not found: {}", self.search.query)),
        }
    }

    /// Overlay layers on visible pages: search hits and the text selection.
    fn overlay_layers(&mut self, comp: &mut Compositor, layers: &mut Vec<Layer>) {
        let mut rects: Vec<(usize, PageRect, Color)> = Vec::new();
        for page in self.viewer.visible_pages() {
            if self.search.is_active() {
                for (i, r) in self.search.hits[page].iter().enumerate() {
                    let current = self.search.current == Some((page, i));
                    rects.push((page, *r, if current { CURRENT_HIT_COLOR } else { HIT_COLOR }));
                }
            }
            if let Some(sel) = self.selection
                && let Some(Some(text)) = self.texts.get(page)
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
                comp.add_image(w, h, c.finish())
            });
            let src = Rect::new(vis.x - dst.x, vis.y - dst.y, vis.w, vis.h);
            layers.push(Layer { key, image, src, dst: vis, z: Z_OVERLAY });
        }
        // Bound the overlay image cache (sizes change with zoom).
        if self.overlay_images.len() > 256 {
            for (_, id) in self.overlay_images.drain() {
                comp.remove_image(id);
            }
        }
    }

    /// Text of a page, extracted on first use from its display list.
    fn text_of_page(&mut self, page: usize) -> Option<&PageText> {
        if self.texts.get(page)?.is_none() {
            let list = self.viewer.display_list(page)?;
            let text = PageText::extract(&list, self.info.bounds[page]).unwrap_or_default();
            self.texts[page] = Some(text);
        }
        self.texts[page].as_ref()
    }

    /// The caret nearest a screen point, on the page under it (or the nearest page).
    fn caret_at(&mut self, sx: f32, sy: f32) -> Option<Caret> {
        let (page, x, y) = self.viewer.screen_to_page_clamped(sx, sy);
        Some((page, self.text_of_page(page)?.caret_at(x, y)))
    }

    fn copy_selection(&mut self) {
        let Some(sel) = self.selection.filter(|s| !s.is_empty()) else { return };
        let (p0, p1) = (sel.ordered().0.0, sel.ordered().1.0);
        for p in p0..=p1 {
            self.text_of_page(p);
        }
        let text = sel.text(&self.texts);
        if !text.is_empty() {
            self.notice = Some(format!("Copied {} characters", text.chars().count()));
            self.requests.push(Request::Copy(text));
        }
    }

    pub fn key(&mut self, k: &KeyEvent, now: Instant) -> Action {
        if k.kind == KeyEventKind::Release || matches!(k.code, KeyCode::Modifier(_)) {
            return Action::None;
        }
        self.message = None;
        self.notice = None;
        if self.input.is_some() {
            return self.input_key(k, now);
        }
        if self.sidebar_open
            && let Some(action) = self.sidebar_key(k, now)
        {
            return action;
        }
        if k.code == KeyCode::Tab {
            self.toggle_sidebar();
            return Action::Redraw;
        }
        let v = &mut self.viewer;
        let mods = k.modifiers.without_locks();
        let ctrl = mods.contains(Modifiers::CTRL);
        let page = v.view.screen_h as f64 * 0.9;
        let half = v.view.screen_h as f64 * 0.5;
        let count = self.count.take();
        let centre = (v.view.screen_w / 2.0, v.view.screen_h / 2.0);
        match k.code {
            KeyCode::Char(c @ '0'..='9') if !ctrl && (c != '0' || count.is_some()) => {
                self.count = Some(count.unwrap_or(0).saturating_mul(10).saturating_add(c as usize - '0' as usize));
                return Action::None;
            }
            KeyCode::Escape if self.selection.is_some() => self.selection = None,
            KeyCode::Escape if self.search.is_active() => self.search.clear(),
            KeyCode::Char('y') => self.copy_selection(),
            KeyCode::Char('q') | KeyCode::Escape => return Action::Quit,
            KeyCode::Char('c') if ctrl => return Action::Quit,
            KeyCode::Char('j') | KeyCode::Down => wheel_n(v, 1.0, count, now),
            KeyCode::Char('k') | KeyCode::Up => wheel_n(v, -1.0, count, now),
            KeyCode::Left if k.modifiers.contains(Modifiers::ALT) => self.history(true, now),
            KeyCode::Right if k.modifiers.contains(Modifiers::ALT) => self.history(false, now),
            KeyCode::Char('l') | KeyCode::Right => v.scroll_x.wheel(1.0, now),
            KeyCode::Char('h') | KeyCode::Left => v.scroll_x.wheel(-1.0, now),
            KeyCode::Char(' ') | KeyCode::PageDown => v.scroll_y.scroll_by(page, now),
            KeyCode::Char('f') if ctrl => v.scroll_y.scroll_by(page, now),
            KeyCode::Char('b') | KeyCode::PageUp => v.scroll_y.scroll_by(-page, now),
            KeyCode::Char('d') => v.scroll_y.scroll_by(half, now),
            KeyCode::Char('u') => v.scroll_y.scroll_by(-half, now),
            KeyCode::Char('g') | KeyCode::Home => {
                self.push_history();
                self.viewer.goto_page(count.map_or(0, |c| c.saturating_sub(1)), true, now);
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.push_history();
                match count {
                    Some(c) => self.viewer.goto_page(c.saturating_sub(1), true, now),
                    None => self.viewer.scroll_y.scroll_by(f64::INFINITY, now),
                }
            }
            KeyCode::Char('o') if ctrl => self.history(true, now),
            KeyCode::Char('+' | '=') => v.zoom(ZOOM_STEP, centre),
            KeyCode::Char('-') => v.zoom(1.0 / ZOOM_STEP, centre),
            KeyCode::Char('0') | KeyCode::Char('w') => v.fit_width(),
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('n') => self.step_search(true, now),
            KeyCode::Char('N') => self.step_search(false, now),
            _ => return Action::None,
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
        let (x, y) = self.pointer;
        let over_sidebar = self.sidebar_open && self.drag.is_none() && self.sidebar_rect().contains(x as i32, y as i32);
        if over_sidebar {
            return self.sidebar_mouse(m, now).unwrap_or(false);
        }
        // Leaving the sidebar clears its hover highlight.
        let hover_cleared = self.outline.list.hover.take().is_some();
        let (x, y) = self.pointer;
        // Links: pointer shape on hover, follow on click (press and release in place).
        self.pointer_shape =
            if self.drag.is_none() && self.link_at(x, y).is_some() { Pointer::Link } else { Pointer::Default };
        match m.kind {
            MouseEventKind::Press(MouseButton::Left) => self.press = Some((x, y)),
            MouseEventKind::Release(MouseButton::Left) => {
                let clicked = self.press.take().is_some_and(|(px, py)| (px - x).hypot(py - y) < CLICK_SLOP);
                if clicked
                    && !m.modifiers.contains(Modifiers::CTRL)
                    && let Some(target) = self.link_at(x, y).map(|l| l.target.clone())
                {
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
        self.page_mouse(m, now) || hover_cleared
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
                let (sx, sy) = self.pointer;
                let Some(head) = self.caret_at(sx, sy) else { return false };
                let Some(sel) = self.selection.as_mut() else { return false };
                let changed = sel.head != head;
                sel.head = head;
                return changed;
            }
            MouseEventKind::Drag(_) => {
                let Some((p0, s0)) = self.drag else { return false };
                v.jump_to(s0.0 - (self.pointer.0 - p0.0), s0.1 - (self.pointer.1 - p0.1));
            }
            MouseEventKind::Release(_) => {
                self.drag = None;
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
    use termpix::geom::CellSize;
    use termpix::input::{CoordUnit, MouseEvent, MouseEventKind};

    /// A session on the fixture with every page loaded, at 900x600 pixels.
    fn session() -> Session {
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        let info = pdf::doc_info(&doc).unwrap();
        let geometry = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };
        let mut s = Session::new("sample.pdf".into(), info, geometry);
        for p in 0..s.viewer.page_count() {
            s.viewer.page_loaded(0, p, pdf::display_list(&doc, p).unwrap());
            s.page_links(p, pdf::page_links(&doc, p, s.info.bounds[p]).unwrap());
        }
        s
    }

    fn mouse(s: &mut Session, kind: MouseEventKind, (x, y): (f32, f32)) -> bool {
        let ev = MouseEvent { kind, x: x as i32, y: y as i32, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE };
        s.mouse(&ev, Instant::now())
    }

    /// Screen position of the middle of the first character of `needle` on page 0.
    fn char_pos(s: &mut Session, needle: &str, offset: usize) -> (f32, f32) {
        let text = s.text_of_page(0).unwrap().clone();
        let all: String = text.chars.iter().map(|c| c.c).collect();
        let i = all[..all.find(needle).unwrap()].chars().count() + offset;
        let r = text.chars[i].rect;
        s.viewer.page_to_screen(0, (r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0)
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

    fn comp() -> Compositor {
        let g = Geometry { cols: 90, rows: 30, cell: CellSize { width: 10, height: 20 } };
        Compositor::new(termpix::compositor::Backend::Layered, termpix::graphics::Medium::Direct { compress: false }, g)
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
        s.take_requests();
        s.reload(&mut c, info("sample-short.pdf"));
        assert!(s.sidebar_open);
        match s.take_requests().as_slice() {
            [Request::Search { query, doc_generation: 1, .. }] => assert_eq!(query, "needle"),
            other => panic!("{other:?}"),
        }
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
}
