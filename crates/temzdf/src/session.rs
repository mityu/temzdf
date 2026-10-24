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
use crate::viewer::{LAYER_KEYS_END, Need, Viewer};

pub const ZOOM_STEP: f32 = 1.1;
/// UI layers sit above page tiles but, like them, below terminal text.
const Z_UI: i32 = -1_073_741_800;
const KEY_STATUS: u32 = LAYER_KEYS_END;
const KEY_SIDEBAR: u32 = LAYER_KEYS_END + 1;
const KEY_INPUT: u32 = LAYER_KEYS_END + 2;
/// Search hit highlights use keys from here on (one per visible hit).
const KEY_HITS: u32 = LAYER_KEYS_END + 0x1000;
/// Hit highlights: above page tiles, below UI.
const Z_HITS: i32 = -1_073_741_850;
const HIT_COLOR: Color = Color::rgba(255, 210, 0, 96);
const CURRENT_HIT_COLOR: Color = Color::rgba(255, 120, 0, 150);

/// Work the session asks the caller to do (on a worker, or inline).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Search {
        generation: u64,
        query: String,
        start_page: usize,
    },
    /// Open an external link with the system handler.
    OpenUri(String),
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
    /// Error or notice shown in the status bar until the next key press.
    pub message: Option<String>,
    outline: OutlinePanel,
    sidebar_open: bool,
    pub search: SearchState,
    /// The search prompt, while typing a query.
    input: Option<LineInput>,
    /// Where the view was when the prompt opened (restored on Esc).
    search_origin: Option<(Target, f32, f32)>,
    requests: Vec<Request>,
    /// Solid highlight images by (width, height, is current).
    hit_images: HashMap<(u32, u32, bool), ImageId>,
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
            hit_images: HashMap::new(),
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
            let status = StatusBar { left: self.name.clone(), right, error: self.message.clone() };
            layers.push(self.widget(comp, KEY_STATUS, WidgetState::Status(status), bar));
        }
        self.hit_layers(comp, &mut layers);
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
            self.message = Some(
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
                self.message = Some(format!("Opening {uri}"));
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
        // Incremental search: move to the first match after where the search began.
        if self.search.current.is_none() {
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
        let generation = self.search.start(&query);
        if !query.trim().is_empty() {
            let start_page = self.search_origin.map_or_else(|| self.viewer.top_position().page, |o| o.0.page);
            self.requests.push(Request::Search { generation, query, start_page });
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

    /// Highlight layers for the hits on visible pages.
    fn hit_layers(&mut self, comp: &mut Compositor, layers: &mut Vec<Layer>) {
        if !self.search.is_active() {
            return;
        }
        let scale = self.viewer.view.scale;
        let area =
            Rect::new(self.viewer.origin_x(), 0, self.viewer.view.screen_w as u32, self.viewer.view.screen_h as u32);
        let mut key = KEY_HITS;
        for page in self.viewer.visible_pages() {
            for (i, r) in self.search.hits[page].iter().enumerate() {
                let (x0, y0) = self.viewer.page_to_screen(page, r.x0, r.y0);
                let w = ((r.x1 - r.x0) * scale).round().max(1.0) as u32;
                let h = ((r.y1 - r.y0) * scale).round().max(1.0) as u32;
                let dst = Rect::new(x0.round() as i32, y0.round() as i32, w, h);
                // Clip to the page area so highlights never cover the UI.
                let Some(vis) = dst.intersect(&area) else { continue };
                let current = self.search.current == Some((page, i));
                let image = *self.hit_images.entry((w, h, current)).or_insert_with(|| {
                    let mut c = Canvas::new(w, h);
                    c.fill_rect(0.0, 0.0, w as f32, h as f32, if current { CURRENT_HIT_COLOR } else { HIT_COLOR });
                    comp.add_image(w, h, c.finish())
                });
                let src = Rect::new(vis.x - dst.x, vis.y - dst.y, vis.w, vis.h);
                layers.push(Layer { key, image, src, dst: vis, z: Z_HITS });
                key += 1;
            }
        }
        // Bound the highlight image cache (sizes change with zoom).
        if self.hit_images.len() > 256 {
            for (_, id) in self.hit_images.drain() {
                comp.remove_image(id);
            }
        }
    }

    pub fn key(&mut self, k: &KeyEvent, now: Instant) -> Action {
        if k.kind == KeyEventKind::Release || matches!(k.code, KeyCode::Modifier(_)) {
            return Action::None;
        }
        self.message = None;
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
            KeyCode::Escape if self.search.is_active() => self.search.clear(),
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
                if clicked && let Some(target) = self.link_at(x, y).map(|l| l.target.clone()) {
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
            MouseEventKind::Press(MouseButton::Left) => {
                self.drag = Some((self.pointer, (v.view.scroll_x, v.view.scroll_y)));
                return false;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some((p0, s0)) = self.drag else { return false };
                v.jump_to(s0.0 - (self.pointer.0 - p0.0), s0.1 - (self.pointer.1 - p0.1));
            }
            MouseEventKind::Release(_) => {
                self.drag = None;
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
