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
use termpix_ui::{StatusBar, Text, Theme};

use crate::pdf::DocInfo;
use crate::viewer::{LAYER_KEYS_END, Need, Viewer};

pub const ZOOM_STEP: f32 = 1.1;
/// UI layers sit above page tiles but, like them, below terminal text.
const Z_UI: i32 = -1_073_741_800;
const KEY_STATUS: u32 = LAYER_KEYS_END;

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
    // Input state
    count: Option<usize>,
    pointer: (f32, f32),
    drag: Option<((f32, f32), (f32, f32))>,
}

impl Session {
    pub fn new(name: String, info: DocInfo, geometry: Geometry) -> Self {
        let (w, h) = page_area(geometry);
        Self {
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
        let (w, h) = page_area(geometry);
        self.viewer.resize(w, h);
    }

    fn bar_height(&self) -> u32 {
        self.geometry.cell.height
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
        (layers, needs)
    }

    fn widget(&mut self, comp: &mut Compositor, key: u32, state: WidgetState, rect: Rect) -> Layer {
        let size = (rect.w, rect.h);
        let fresh = self.widgets.get(&key).is_some_and(|w| w.state == state && w.size == size);
        if !fresh {
            let px = match &state {
                WidgetState::Status(s) => s.render(&mut self.text, &self.theme, rect.w, rect.h),
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
        let v = &mut self.viewer;
        let mods = m.modifiers;
        match m.kind {
            MouseEventKind::Scroll(dir) if mods.contains(Modifiers::CTRL) => {
                let f = match dir {
                    ScrollDirection::Up => ZOOM_STEP,
                    ScrollDirection::Down => 1.0 / ZOOM_STEP,
                    _ => return false,
                };
                v.zoom(f, self.pointer);
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
