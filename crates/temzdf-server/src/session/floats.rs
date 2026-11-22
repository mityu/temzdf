//! Floating panes: panes outside the layout tree, stacked over it (the last
//! on top), each in a one-cell frame with a title (or none: `border`).
//! They show and hide together, but those that never take the focus
//! (`focusable`); what lies under a shown float is cut out of the panes
//! below (see `crate::clip`). Design: `doc/design/floating-panes.md`.

use temzdf_scene::{Attrs, Cell, Color, Pointer, Rect, TextGrid};
use termpix_core::input::{MouseButton, MouseEvent, MouseEventKind};

use std::path::PathBuf;

use temzdf_term::Spawn;

use super::tabs::FloatTab;
use super::{BACKGROUND, BufferId, Outcome, Session, Spot};
use crate::layout::{Direction, PaneId};
use temzdf_protocol::script::ScriptValue;

/// The smallest float content, in cells (as the smallest tiled pane);
/// without a frame, one cell.
const MIN_COLS: u32 = 10;
const MIN_ROWS: u32 = 4;
/// How far a float opened from another one is moved right and down.
const CASCADE: i32 = 2;

/// A length in cells, or a share of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Extent {
    Cells(u32),
    Percent(u32),
}

impl Extent {
    pub(super) fn of(self, whole: u32) -> u32 {
        match self {
            Self::Cells(n) => n,
            Self::Percent(p) => whole * p.min(100) / 100,
        }
    }

    /// A number (cells) or a string like `"50%"`.
    pub(super) fn parse(v: &ScriptValue) -> Option<Self> {
        match v {
            ScriptValue::Num(n) if *n >= 0.0 => Some(Self::Cells(*n as u32)),
            ScriptValue::Str(s) => match s.strip_suffix('%') {
                Some(p) => p.trim().parse().ok().map(Self::Percent),
                None => s.trim().parse().ok().map(Self::Cells),
            },
            _ => None,
        }
    }
}

/// Where a float goes and how large it is. Sizes in cells are the content's
/// (the frame comes around it, as in Neovim); shares of the screen are the
/// whole float's. A missing position centres it. Without `border`, the
/// float is its content alone (no frame, no title).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FloatSpec {
    pub width: Extent,
    pub height: Extent,
    pub col: Option<Extent>,
    pub row: Option<Extent>,
    pub border: bool,
    /// When it opens (a file may open later): its title, and whether it
    /// takes the focus.
    pub title: Option<String>,
    pub focus: bool,
    /// The tabs it shows on; `None`: the current one when it opens.
    pub tab: Option<FloatTab>,
    /// Whether it ever takes the focus (when it opens; then the float's).
    pub focusable: bool,
}

impl Default for FloatSpec {
    fn default() -> Self {
        Self {
            width: Extent::Percent(80),
            height: Extent::Percent(80),
            col: None,
            row: None,
            border: true,
            title: None,
            focus: true,
            tab: None,
            focusable: true,
        }
    }
}

impl FloatSpec {
    /// The float's frame (content and frame) on a screen of `area`, kept
    /// on the screen.
    pub(super) fn frame(&self, area: Rect) -> Rect {
        let pad = self.pad();
        let outer = |e: Extent, whole: u32| match e {
            Extent::Cells(n) => n.saturating_add(pad),
            Extent::Percent(_) => e.of(whole),
        };
        let (min_w, min_h) = if self.border { (MIN_COLS + 2, MIN_ROWS + 2) } else { (1, 1) };
        let w = outer(self.width, area.w).clamp(min_w.min(area.w), area.w.max(1));
        let h = outer(self.height, area.h).clamp(min_h.min(area.h), area.h.max(1));
        let place = |pos: Option<Extent>, size: u32, whole: u32| {
            let at = pos.map_or(whole.saturating_sub(size) / 2, |p| p.of(whole));
            at.min(whole.saturating_sub(size)) as i32
        };
        Rect::new(area.x + place(self.col, w, area.w), area.y + place(self.row, h, area.h), w, h)
    }

    /// Cells the frame adds to the content's width and height.
    fn pad(&self) -> u32 {
        if self.border { 2 } else { 0 }
    }

    /// The same float pinned where `frame` is, its content `w` x `h` cells.
    fn at(&self, frame: Rect, w: u32, h: u32) -> Self {
        Self {
            width: Extent::Cells(w),
            height: Extent::Cells(h),
            col: Some(Extent::Cells(frame.x.max(0) as u32)),
            row: Some(Extent::Cells(frame.y.max(0) as u32)),
            border: self.border,
            focusable: self.focusable,
            ..Self::default()
        }
    }

    /// The inside of `frame` (all of it without a border).
    fn content(&self, frame: Rect) -> Rect {
        if self.border { content(frame) } else { frame }
    }
}

/// A floating pane.
pub(super) struct Float {
    pub pane: PaneId,
    pub spec: FloatSpec,
    /// Its rectangle with the frame, in cells (the pane's `rect` is inside).
    pub frame: Rect,
    /// Shown on the frame instead of the buffer's name.
    pub title: Option<String>,
    /// The tabs it shows on.
    pub tab: FloatTab,
    /// Takes the focus (Neovim's `focusable`): else clicks go through it,
    /// cycling skips it, and `toggleFloats` neither hides nor counts it.
    pub focusable: bool,
    /// Keeps the keys and the focus until it closes; reserved for modal
    /// floats, unused yet.
    #[allow(dead_code)]
    pub modal: bool,
}

impl Float {
    pub(super) fn new(pane: PaneId, spec: FloatSpec, tab: FloatTab) -> Self {
        let focusable = spec.focusable;
        Self { pane, spec, frame: Rect::default(), title: None, tab, focusable, modal: false }
    }

    /// Where its pane goes: inside its frame.
    pub(super) fn content(&self) -> Rect {
        self.spec.content(self.frame)
    }
}

/// What dragging a part of a float's frame does: the title edge moves it,
/// the other edges and the corners resize it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Grip {
    Move,
    Resize { left: bool, right: bool, top: bool, bottom: bool },
}

impl Grip {
    pub(super) fn pointer(self) -> Pointer {
        match self {
            Self::Move => Pointer::Move,
            Self::Resize { left, right, top, bottom } => {
                if (left && top) || (right && bottom) {
                    Pointer::ResizeNwse
                } else if (right && top) || (left && bottom) {
                    Pointer::ResizeNesw
                } else if left || right {
                    Pointer::ResizeColumns
                } else {
                    Pointer::ResizeRows
                }
            }
        }
    }

    /// `start` (a frame) dragged by `dx`, `dy` cells, no smaller than the
    /// smallest float.
    fn drag(self, start: Rect, dx: i32, dy: i32) -> Rect {
        let (mut x, mut y, mut w, mut h) = (start.x, start.y, start.w as i32, start.h as i32);
        let (min_w, min_h) = (MIN_COLS as i32 + 2, MIN_ROWS as i32 + 2);
        match self {
            Self::Move => (x, y) = (x + dx, y + dy),
            Self::Resize { left, right, top, bottom } => {
                if left {
                    let nw = (w - dx).max(min_w);
                    (x, w) = (x + w - nw, nw);
                }
                if right {
                    w = (w + dx).max(min_w);
                }
                if top {
                    let nh = (h - dy).max(min_h);
                    (y, h) = (y + h - nh, nh);
                }
                if bottom {
                    h = (h + dy).max(min_h);
                }
            }
        }
        Rect::new(x.max(0), y.max(0), w as u32, h as u32)
    }
}

/// A float's frame being dragged.
#[derive(Clone, Copy, Debug)]
pub(super) struct FloatDrag {
    pane: PaneId,
    grip: Grip,
    /// Where the drag started (cell) and the frame then.
    from: (i32, i32),
    start: Rect,
}

impl FloatDrag {
    pub(super) fn pointer(&self) -> Pointer {
        self.grip.pointer()
    }
}

/// What a new float shows.
pub(super) enum FloatWhat {
    Spawn(Spawn),
    File(PathBuf),
    Buffer(BufferId),
}

/// Read a float's options (`width`, `height`, `col`, `row`, `border`,
/// `focusable`, `title`) onto `spec` and `title`.
pub(super) fn read_opts(o: &ScriptValue, spec: &mut FloatSpec, title: &mut Option<String>) -> Result<(), String> {
    if !matches!(o, ScriptValue::Map(_)) {
        return Err("float options must be an object".into());
    }
    let extent = |key: &str| match o.get(key) {
        None | Some(ScriptValue::Null) => Ok(None),
        Some(v) => Extent::parse(v).map(Some).ok_or_else(|| format!("{key}: {v:?} is not cells or \"N%\"")),
    };
    if let Some(w) = extent("width")? {
        spec.width = w;
    }
    if let Some(h) = extent("height")? {
        spec.height = h;
    }
    if let Some(c) = extent("col")? {
        spec.col = Some(c);
    }
    if let Some(r) = extent("row")? {
        spec.row = Some(r);
    }
    for (key, flag) in [("border", &mut spec.border), ("focusable", &mut spec.focusable)] {
        match o.get(key) {
            None | Some(ScriptValue::Null) => {}
            Some(ScriptValue::Bool(b)) => *flag = *b,
            Some(v) => return Err(format!("{key}: {v:?} is not true or false")),
        }
    }
    if let Some(t) = o.get("title").and_then(ScriptValue::as_str) {
        *title = Some(t.to_owned());
    }
    Ok(())
}

/// The inside of a frame.
fn content(frame: Rect) -> Rect {
    Rect::new(frame.x + 1, frame.y + 1, frame.w.saturating_sub(2), frame.h.saturating_sub(2))
}

impl Session {
    pub(super) fn is_float(&self, pane: PaneId) -> bool {
        self.floats.iter().any(|f| f.pane == pane)
    }

    pub(super) fn float_mut(&mut self, pane: PaneId) -> Option<&mut Float> {
        self.floats.iter_mut().find(|f| f.pane == pane)
    }

    /// The current tab's floats (its own and those on all tabs), bottom to
    /// top, shown or hidden.
    pub(super) fn tab_floats(&self) -> Vec<&Float> {
        self.floats.iter().filter(|f| self.here(f.tab)).collect()
    }

    /// The floats on screen, bottom to top (while they are hidden, those
    /// that never take the focus).
    pub(super) fn shown_floats(&self) -> Vec<&Float> {
        self.tab_floats().into_iter().filter(|f| !self.floats_hidden || !f.focusable).collect()
    }

    /// The shown floats that take the focus, bottom to top.
    pub(super) fn focusable_floats(&self) -> Vec<&Float> {
        self.shown_floats().into_iter().filter(|f| f.focusable).collect()
    }

    /// What covers `pane` (the session's own layers with `None`), in cells:
    /// the frames of the shown floats above it, and the floating messages
    /// (text over everything; WezTerm draws images over text, whatever their
    /// z, so the images below text that covers them are cut out too).
    pub(super) fn covers(&self, pane: Option<PaneId>) -> Vec<Rect> {
        let floats = self.shown_floats();
        let above = pane.and_then(|p| floats.iter().position(|f| f.pane == p)).map_or(0, |i| i + 1);
        let mut covers: Vec<Rect> = floats[above..].iter().map(|f| f.frame).collect();
        covers.extend(self.notice_rects());
        covers
    }

    /// Put pane `id` on top of the floats. Returns its content rectangle.
    pub(super) fn place_float(&mut self, id: PaneId, spec: FloatSpec) -> Rect {
        let frame = spec.frame(self.area());
        let title = spec.title.clone();
        let tab = spec.tab.unwrap_or(FloatTab::Only(self.tab_id()));
        let focusable = spec.focusable;
        let float = Float { frame, title, ..Float::new(id, spec, tab) };
        let rect = float.content();
        self.floats.push(float);
        // A new float shows the others of its tab page too (one that never
        // takes the focus shows alone).
        if focusable && self.here(tab) {
            self.floats_hidden = false;
        } else if focusable
            && let FloatTab::Only(t) = tab
            && let Some(i) = self.tab_index(t)
        {
            self.show_tab_floats(i);
        }
        rect
    }

    /// Whether a pane put at `spot` takes the focus: not a float opened
    /// without it or never taking it, nor one for another tab page (keys
    /// would go to a pane nobody sees).
    pub(super) fn takes_focus(&self, spot: &Spot) -> bool {
        match spot {
            Spot::Float(s) => s.focus && s.focusable && s.tab.is_none_or(|t| self.here(t)),
            _ => true,
        }
    }

    /// Raise a float to the top.
    pub(super) fn raise(&mut self, pane: PaneId) {
        if let Some(i) = self.floats.iter().position(|f| f.pane == pane) {
            let f = self.floats.remove(i);
            self.floats.push(f);
        }
    }

    /// The floats' rectangles from their specs (after the screen changed).
    pub(super) fn relayout_floats(&mut self) {
        let area = self.area();
        let rects: Vec<(PaneId, Rect)> = self
            .floats
            .iter_mut()
            .map(|f| {
                f.frame = f.spec.frame(area);
                (f.pane, f.content())
            })
            .collect();
        for (id, r) in rects {
            self.set_rect(id, r);
        }
    }

    /// Where a float opened from the focused float goes: beside it, moved
    /// right and down (zellij).
    pub(super) fn cascade(&self) -> FloatSpec {
        let Some(f) = self.floats.iter().find(|f| f.pane == self.focus) else { return FloatSpec::default() };
        let r = f.content();
        let moved =
            Rect::new(f.frame.x.saturating_add(CASCADE), f.frame.y.saturating_add(CASCADE), f.frame.w, f.frame.h);
        FloatSpec { tab: Some(f.tab), ..f.spec.at(moved, r.w, r.h) }
    }

    /// Move or resize a float: `cells` added to its width or height
    /// (`resizePane` moves its right or bottom edge), or a new size.
    pub(super) fn resize_float(&mut self, pane: PaneId, width: Option<i64>, height: Option<i64>, absolute: bool) {
        let Some(f) = self.float_mut(pane) else { return };
        let r = f.content();
        let size = |cur: u32, v: Option<i64>| match v {
            Some(n) if absolute => n.max(0) as u32,
            Some(n) => (cur as i64 + n).max(0) as u32,
            None => cur,
        };
        f.spec = f.spec.at(f.frame, size(r.w, width), size(r.h, height));
        self.relayout();
    }

    /// The float next to the focused one in `dir`, by their centres (floats
    /// overlap, so none need be wholly beyond the other).
    pub(super) fn float_neighbor(&self, dir: Direction) -> Option<PaneId> {
        let floats = self.focusable_floats();
        let from = floats.iter().find(|f| f.pane == self.focus)?.frame;
        let centre = |r: &Rect| (r.x as i64 * 2 + r.w as i64, r.y as i64 * 2 + r.h as i64);
        let c = centre(&from);
        floats
            .iter()
            .filter(|f| f.pane != self.focus)
            .filter_map(|f| {
                let (x, y) = centre(&f.frame);
                let (ahead, across) = match dir {
                    Direction::Left => (c.0 - x, (y - c.1).abs()),
                    Direction::Right => (x - c.0, (y - c.1).abs()),
                    Direction::Up => (c.1 - y, (x - c.0).abs()),
                    Direction::Down => (y - c.1, (x - c.0).abs()),
                };
                (ahead > 0).then_some((ahead, across, f.pane))
            })
            .min()
            .map(|(_, _, p)| p)
    }

    /// The panes in focus order: the shown floats that take the focus as
    /// they were opened (not as stacked: focusing raises, which would
    /// change the order while cycling), then the tiled panes.
    pub(super) fn focus_order(&self) -> Vec<PaneId> {
        let mut v: Vec<PaneId> = self.focusable_floats().iter().map(|f| f.pane).collect();
        v.sort();
        v.extend(self.layout.as_ref().map(|l| l.panes()).unwrap_or_default());
        v
    }

    /// Open a float showing `what`, focused unless `focus` is false. A file
    /// still opening has no pane yet (`None`).
    pub(super) fn open_float(&mut self, what: FloatWhat, spec: FloatSpec) -> Result<Option<PaneId>, String> {
        self.open_what(what, Spot::Float(spec))
    }

    /// Open a pane at `spot` showing `what`. A file still opening has no
    /// pane yet (`None`; its spot waits with it).
    pub(super) fn open_what(&mut self, what: FloatWhat, spot: Spot) -> Result<Option<PaneId>, String> {
        Ok(match what {
            FloatWhat::Spawn(spawn) => Some(self.open_terminal(&spawn, spot).map_err(|e| format!("{e:#}"))?),
            FloatWhat::File(path) => {
                // An open document gets its pane now, another one once open.
                let next = self.next_id;
                self.open_at(&path, spot).map_err(|e| format!("{e:#}"))?;
                (next..self.next_id).find(|p| self.panes.contains_key(p))
            }
            // A document that finds no room says so, as when its file
            // opens (`add_pdf_pane`).
            FloatWhat::Buffer(buf) if self.docs.get(&buf).is_some_and(|d| d.host.is_some()) => {
                self.add_pdf_pane(buf, spot)
            }
            FloatWhat::Buffer(buf) => self.add_pane(buf, spot)?,
        })
    }

    /// Show or hide every float that takes the focus; with none, open a
    /// floating shell.
    pub(super) fn toggle_floats(&mut self) -> anyhow::Result<()> {
        if !self.tab_floats().iter().any(|f| f.focusable) {
            return self.open_terminal(&Spawn::default(), Spot::Float(FloatSpec::default())).map(drop);
        }
        self.floats_hidden = !self.floats_hidden;
        if self.floats_hidden {
            if self.is_float(self.focus) {
                self.set_focus(self.tiled_focus);
            }
        } else if let Some(top) = self.focusable_floats().last().map(|f| f.pane) {
            self.set_focus(top);
        }
        self.relayout();
        Ok(())
    }

    /// Turn a tiled pane into a float, or a float into a tiled pane beside
    /// the last focused tiled one. The last tiled pane stays.
    pub(super) fn toggle_floating(&mut self, pane: PaneId) -> Result<(), String> {
        // (Another tab's pane would stay in its layout there.)
        if !self.panes.contains_key(&pane) || self.tab_of(pane) != Some(self.tab) {
            return Err(format!("no pane {pane} in this tab"));
        }
        if self.is_float(pane) {
            let target = self.tiled_focus;
            let before = !self.options.splitright;
            let area = self.area();
            let layout = self.layout.as_mut().ok_or("no tiled pane")?;
            if !layout.split_at(Some(target), pane, crate::layout::Split::Horizontal, before, area) {
                return Err("no room for another pane".into());
            }
            self.floats.retain(|f| f.pane != pane);
        } else {
            let layout = self.layout.as_mut().ok_or("no tiled pane")?;
            if layout.panes().len() <= 1 {
                return Err("the last tiled pane cannot float".into());
            }
            layout.remove(pane);
            self.place_float(pane, FloatSpec::default());
            if self.tiled_focus == pane {
                self.tiled_focus = self.layout.as_ref().map_or(pane, |l| l.panes()[0]);
            }
        }
        self.dragging = None;
        self.set_focus(pane);
        self.relayout();
        Ok(())
    }

    /// The topmost shown float whose frame (not its content) is at cell
    /// (`col`, `row`), and that part of the frame. Floats that never take
    /// the focus let the mouse through.
    fn grip_at(&self, col: i32, row: i32) -> Option<(PaneId, Grip)> {
        let f = self.focusable_floats().into_iter().rev().find(|f| f.frame.contains(col, row))?;
        if f.content().contains(col, row) {
            return None;
        }
        let r = f.frame;
        let (left, right) = (col == r.x, col as i64 == r.right() - 1);
        let (top, bottom) = (row == r.y, row as i64 == r.bottom() - 1);
        let corner = (left || right) && (top || bottom);
        let grip = if top && !corner { Grip::Move } else { Grip::Resize { left, right, top, bottom } };
        Some((f.pane, grip))
    }

    /// Floats' frames under the mouse: a press focuses (and raises) the
    /// float and starts moving or resizing it; its programs get their new
    /// size on release. Nothing over a frame reaches the panes. `Some` when
    /// the event was a frame's.
    pub(super) fn float_mouse(&mut self, m: &MouseEvent, col: i32, row: i32) -> Option<Outcome> {
        if let Some(d) = self.float_drag {
            return Some(match m.kind {
                MouseEventKind::Drag(_) => {
                    let frame = d.grip.drag(d.start, col - d.from.0, row - d.from.1);
                    if let Some(f) = self.float_mut(d.pane) {
                        f.spec = f.spec.at(frame, frame.w.saturating_sub(2), frame.h.saturating_sub(2));
                    }
                    self.relayout();
                    Outcome::Redraw
                }
                MouseEventKind::Release(_) => {
                    self.float_drag = None;
                    self.captured = None;
                    self.over_frame = self.grip_at(col, row).map(|(_, g)| g);
                    self.fit_terms();
                    Outcome::Redraw
                }
                _ => Outcome::None,
            });
        }
        // A button held for a pane (a selection) passes over frames.
        let hit = if self.captured.is_none() { self.grip_at(col, row) } else { None };
        self.over_frame = hit.map(|(_, g)| g);
        let (pane, grip) = hit?;
        Some(match m.kind {
            MouseEventKind::Press(MouseButton::Left) => {
                self.set_focus(pane);
                let start = self.floats.iter().find(|f| f.pane == pane).map_or_else(Rect::default, |f| f.frame);
                self.float_drag = Some(FloatDrag { pane, grip, from: (col, row), start });
                Outcome::Redraw
            }
            MouseEventKind::Press(_) => {
                self.set_focus(pane);
                Outcome::Redraw
            }
            _ => Outcome::None,
        })
    }

    /// The frames of the shown floats, bottom to top, each cut by the floats
    /// above it: a box (accent for the focused one) and the title, as text.
    pub(super) fn float_frames(&mut self, text: &mut Vec<TextGrid>) {
        let frames: Vec<(PaneId, Rect, String)> = self
            .shown_floats()
            .iter()
            .filter(|f| f.spec.border)
            .map(|f| (f.pane, f.frame, f.title.clone().unwrap_or_else(|| self.pane_name(f.pane))))
            .collect();
        for (pane, frame, title) in frames {
            let focus = pane == self.focus;
            let covers = self.covers(Some(pane));
            let title = title_grid(&title, frame, focus);
            for grid in super::borders::frame(frame, focus).into_iter().chain(title) {
                text.extend(crate::clip::text_pieces(grid, &covers));
            }
        }
    }

    /// A pane's buffer name: the document's file name, the terminal's title
    /// or the input's prompt.
    pub(super) fn pane_name(&self, pane: PaneId) -> String {
        self.pane_buffer(pane).and_then(|b| self.buffer_info(b)).map(|(_, name)| name).unwrap_or_default()
    }
}

/// The title on a frame's top edge, ` name ` from its third cell, cut to fit.
fn title_grid(title: &str, frame: Rect, focus: bool) -> Option<TextGrid> {
    let room = frame.w.checked_sub(6)? as usize;
    let fg = if focus { Color::Rgb([0xee, 0xee, 0xee]) } else { Color::Rgb([0xaa, 0xaa, 0xaa]) };
    let bg = Color::Rgb([BACKGROUND[0], BACKGROUND[1], BACKGROUND[2]]);
    let attrs = Attrs { bold: focus, ..Attrs::default() };
    let cell = |text: String, width: u8| Cell { text, width, fg, bg, attrs };
    let mut row = vec![cell(" ".into(), 1)];
    let mut used = 0;
    let title: String = title.chars().filter(|c| !c.is_control()).collect();
    for g in temzdf_scene::graphemes(&title) {
        let w = temzdf_scene::grapheme_width(g);
        if used + w > room {
            break;
        }
        row.push(cell(g.to_owned(), w as u8));
        if w == 2 {
            row.push(cell(String::new(), 0));
        }
        used += w;
    }
    row.push(cell(" ".into(), 1));
    Some(TextGrid { col: (frame.x + 2) as u16, row: frame.y as u16, rows: vec![row] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_place_floats_on_the_screen() {
        let area = Rect::new(0, 0, 100, 30);
        assert_eq!(FloatSpec::default().frame(area), Rect::new(10, 3, 80, 24), "centred, 80% by 80%");
        let cells = FloatSpec {
            width: Extent::Cells(20),
            height: Extent::Cells(5),
            col: Some(Extent::Cells(3)),
            row: Some(Extent::Percent(50)),
            ..FloatSpec::default()
        };
        assert_eq!(cells.frame(area), Rect::new(3, 15, 22, 7), "content in cells, the frame around it");
        let past = FloatSpec { col: Some(Extent::Cells(95)), row: Some(Extent::Cells(29)), ..cells.clone() };
        assert_eq!(past.frame(area), Rect::new(78, 23, 22, 7), "kept on the screen");
        let tiny = FloatSpec { width: Extent::Cells(1), height: Extent::Cells(1), ..FloatSpec::default() };
        assert_eq!(tiny.frame(area).w, MIN_COLS + 2);
        assert_eq!(Extent::parse(&ScriptValue::Str("50%".into())), Some(Extent::Percent(50)));
        assert_eq!(Extent::parse(&ScriptValue::Num(12.0)), Some(Extent::Cells(12)));
        assert_eq!(Extent::parse(&ScriptValue::Str("x".into())), None);
    }

    #[test]
    fn titles_fit_their_frame() {
        let g = title_grid("abcdefghij", Rect::new(4, 2, 10, 5), true).unwrap();
        assert_eq!((g.col, g.row), (6, 2));
        let text: String = g.rows[0].iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, " abcd ");
        assert!(title_grid("x", Rect::new(0, 0, 5, 5), false).is_none(), "no room");
    }
}
