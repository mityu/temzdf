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
use termpix_ui::{ListView, StatusBar, Text, Theme};

use crate::outline::{self, OutlinePanel};
use crate::pdf::DocInfo;
use crate::viewer::{LAYER_KEYS_END, Need, Viewer};

pub const ZOOM_STEP: f32 = 1.1;
/// UI layers sit above page tiles but, like them, below terminal text.
const Z_UI: i32 = -1_073_741_800;
const KEY_STATUS: u32 = LAYER_KEYS_END;
const KEY_SIDEBAR: u32 = LAYER_KEYS_END + 1;

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
    // Input state
    count: Option<usize>,
    pointer: (f32, f32),
    drag: Option<((f32, f32), (f32, f32))>,
}

impl Session {
    pub fn new(name: String, info: DocInfo, geometry: Geometry) -> Self {
        let (w, h) = page_area(geometry);
        let outline = OutlinePanel::new(&info.outline, &info.labels);
        Self {
            outline,
            sidebar_open: false,
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
        let status = StatusBar {
            left: self.name.clone(),
            right: format!("{}   {zoom}%", self.page_text()),
            error: self.message.clone(),
        };
        let (w, h) = (self.geometry.width_px(), self.bar_height());
        let y = self.geometry.height_px().saturating_sub(h) as i32;
        layers.push(self.widget(comp, KEY_STATUS, WidgetState::Status(status), Rect::new(0, y, w, h)));
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

    pub fn key(&mut self, k: &KeyEvent, now: Instant) -> Action {
        if k.kind == KeyEventKind::Release || matches!(k.code, KeyCode::Modifier(_)) {
            return Action::None;
        }
        self.message = None;
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
            KeyCode::Char('q') | KeyCode::Escape => return Action::Quit,
            KeyCode::Char('c') if ctrl => return Action::Quit,
            KeyCode::Char('j') | KeyCode::Down => wheel_n(v, 1.0, count, now),
            KeyCode::Char('k') | KeyCode::Up => wheel_n(v, -1.0, count, now),
            KeyCode::Char('l') | KeyCode::Right => v.scroll_x.wheel(1.0, now),
            KeyCode::Char('h') | KeyCode::Left => v.scroll_x.wheel(-1.0, now),
            KeyCode::Char(' ') | KeyCode::PageDown => v.scroll_y.scroll_by(page, now),
            KeyCode::Char('f') if ctrl => v.scroll_y.scroll_by(page, now),
            KeyCode::Char('b') | KeyCode::PageUp => v.scroll_y.scroll_by(-page, now),
            KeyCode::Char('d') => v.scroll_y.scroll_by(half, now),
            KeyCode::Char('u') => v.scroll_y.scroll_by(-half, now),
            KeyCode::Char('g') | KeyCode::Home => v.goto_page(count.map_or(0, |c| c.saturating_sub(1)), true, now),
            KeyCode::Char('G') | KeyCode::End => match count {
                Some(c) => v.goto_page(c.saturating_sub(1), true, now),
                None => v.scroll_y.scroll_by(f64::INFINITY, now),
            },
            KeyCode::Char('+' | '=') => v.zoom(ZOOM_STEP, centre),
            KeyCode::Char('-') => v.zoom(1.0 / ZOOM_STEP, centre),
            KeyCode::Char('0') | KeyCode::Char('w') => v.fit_width(),
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
