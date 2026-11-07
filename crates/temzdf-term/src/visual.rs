//! Visual mode: a cursor of our own over the screen and its scrollback,
//! moved by keys, selecting characters, lines or a block, copied with `y`
//! (Vim's Visual mode on a terminal buffer, tmux's copy mode). The session
//! decides when it is on (its `visual` mode); the keys are in doc/config.md.
//!
//! Positions are libghostty's tracked references, so the cursor and the
//! selection's other end stay on their characters while the program prints
//! and old rows go. Rows are screen rows (as in Vim's terminal buffers);
//! copying joins soft-wrapped ones.

use std::time::Instant;

use libghostty_vt::screen::{CellWide, RowSemanticPrompt, TrackedGridRef};
use libghostty_vt::selection::{FormatOptions, Order, SelectWordOptions, Selection};
use libghostty_vt::terminal::{Point, PointCoordinate, PointSpace, ScrollViewport};
use temzdf_scene::{Cell, Color, CopyKind, Effect};
use termpix_core::input::{CoordUnit, MouseButton, MouseEvent, MouseEventKind, ScrollDirection, ScrollUnit};

use super::{MULTI_CLICK, TermBuffer, WHEEL_ROWS};

/// What a key does in visual mode (`visual.*` commands).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    FirstNonBlank,
    LineEnd,
    WordForward,
    WordBackward,
    WordEnd,
    BigWordForward,
    BigWordBackward,
    BigWordEnd,
    Top,
    Bottom,
    ScreenTop,
    ScreenMiddle,
    ScreenBottom,
    HalfPageUp,
    HalfPageDown,
    PageUp,
    PageDown,
    ScrollUp,
    ScrollDown,
    PrevPrompt,
    NextPrompt,
    Charwise,
    Linewise,
    Blockwise,
    SwapEnds,
    Yank,
    YankLines,
    Escape,
    NextMatch,
    PrevMatch,
}

impl Op {
    /// The commands, without their `visual.` namespace, and what they do.
    pub const NAMES: &[(Op, &str, &str)] = &[
        (Op::Left, "left", "One character left (Vim's h)"),
        (Op::Right, "right", "One character right (l)"),
        (Op::Up, "up", "One row up, keeping the column (k)"),
        (Op::Down, "down", "One row down, keeping the column (j)"),
        (Op::LineStart, "line-start", "To the first column (0)"),
        (Op::FirstNonBlank, "first-non-blank", "To the first character that is not blank (^)"),
        (Op::LineEnd, "line-end", "To the last character of the row ($)"),
        (Op::WordForward, "word-forward", "To the start of the next word (w)"),
        (Op::WordBackward, "word-backward", "To the start of the word (b)"),
        (Op::WordEnd, "word-end", "To the end of the word (e)"),
        (Op::BigWordForward, "big-word-forward", "To the start of the next blank-separated word (W)"),
        (Op::BigWordBackward, "big-word-backward", "To the start of the blank-separated word (B)"),
        (Op::BigWordEnd, "big-word-end", "To the end of the blank-separated word (E)"),
        (Op::Top, "top", "To the first row of the scrollback, or row {count} (gg)"),
        (Op::Bottom, "bottom", "To the last row with text, or row {count} (G)"),
        (Op::ScreenTop, "screen-top", "To the top row shown (H)"),
        (Op::ScreenMiddle, "screen-middle", "To the middle row shown (M)"),
        (Op::ScreenBottom, "screen-bottom", "To the bottom row shown (L)"),
        (Op::HalfPageUp, "half-page-up", "Half a screen up, the view with it (<C-u>)"),
        (Op::HalfPageDown, "half-page-down", "Half a screen down, the view with it (<C-d>)"),
        (Op::PageUp, "page-up", "A screen up, the view with it (<C-b>)"),
        (Op::PageDown, "page-down", "A screen down, the view with it (<C-f>)"),
        (Op::ScrollUp, "scroll-up", "The view a row up, the cursor kept in it (<C-y>)"),
        (Op::ScrollDown, "scroll-down", "The view a row down, the cursor kept in it (<C-e>)"),
        (Op::PrevPrompt, "prev-prompt", "To the shell prompt above (OSC 133 marks)"),
        (Op::NextPrompt, "next-prompt", "To the shell prompt below (OSC 133 marks)"),
        (Op::Charwise, "charwise", "Select characters from the cursor; again: stop selecting (v)"),
        (Op::Linewise, "linewise", "Select rows from the cursor; again: stop selecting (V)"),
        (Op::Blockwise, "blockwise", "Select a block from the cursor; again: stop selecting (<C-v>)"),
        (Op::SwapEnds, "swap-ends", "The cursor to the other end of the selection (o)"),
        (Op::Yank, "yank", "Copy the selection and leave the visual mode (y)"),
        (Op::YankLines, "yank-lines", "Copy the selected rows (else the cursor's, {count} of them) and leave (Y)"),
        (Op::Escape, "escape", "Drop the selection; without one, leave the visual mode (<Esc>)"),
        (Op::NextMatch, "next-match", "To the next match of the search (n)"),
        (Op::PrevMatch, "prev-match", "To the previous match of the search (N)"),
    ];

    /// The operation of a command name, `visual.` taken off.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.strip_prefix("visual.")?;
        Self::NAMES.iter().find(|(_, n, _)| *n == name).map(|(op, _, _)| *op)
    }
}

/// What a selection takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Char,
    Line,
    Block,
}

/// What an operation asks of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Stay in the visual mode (`true`: the screen changed).
    Stay(bool),
    /// Leave it (copied, or `<Esc>` with nothing selected).
    Leave,
}

/// A position on the screen, scrollback included: row first, so positions
/// order as text does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Pos {
    pub y: u32,
    pub x: u16,
}

impl Pos {
    fn point(self) -> Point {
        Point::Screen(PointCoordinate { x: self.x, y: self.y })
    }
}

/// One character of a row: its first column, its text (a blank for an empty
/// cell) and whether it is blank.
struct Ch {
    x: u16,
    text: String,
    blank: bool,
}

/// What a word motion counts as one word: classes of characters (0 is blank).
fn class(text: &str, big: bool) -> u8 {
    let Some(c) = text.chars().next() else { return 0 };
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        2
    } else {
        1
    }
}

struct Search {
    query: String,
    backward: bool,
}

/// The mouse in the visual mode: where the left button went down and how
/// many quick clicks that was.
struct Press {
    at: Pos,
    time: Instant,
    clicks: u8,
    dragging: bool,
}

pub(crate) struct Visual {
    cursor: TrackedGridRef,
    /// The column up and down keep (Vim's curswant); `u16::MAX`: the end.
    want: u16,
    selection: Option<(TrackedGridRef, Kind)>,
    /// The session's visual mode is not on (the focus went to another pane,
    /// e.g. the search's input line): kept to go on with.
    pub suspended: bool,
    search: Option<Search>,
    /// Where a search being typed began (`search-preview`).
    origin: Option<TrackedGridRef>,
    press: Option<Press>,
}

impl TermBuffer {
    /// Enter the visual mode: a cursor at the program's, nothing selected,
    /// or what a suspended one had. Returns whether the screen changed.
    pub fn visual_begin(&mut self) -> bool {
        if let Some(v) = &mut self.visual {
            v.suspended = false;
            self.grid.clear();
            return true;
        }
        self.clear_selection();
        let at = PointCoordinate {
            x: self.terminal.cursor_x().unwrap_or(0),
            y: self.terminal.cursor_y().unwrap_or(0) as u32,
        };
        let Ok(cursor) = self.terminal.track_grid_ref(Point::Active(at)) else { return false };
        self.visual = Some(Visual {
            cursor,
            want: at.x,
            selection: None,
            suspended: false,
            search: None,
            origin: None,
            press: None,
        });
        self.grid.clear();
        true
    }

    /// The visual mode is off while another pane has the focus; its cursor
    /// and selection stay for [`Self::visual_begin`].
    pub fn visual_suspend(&mut self) {
        if let Some(v) = &mut self.visual {
            v.suspended = true;
            self.grid.clear();
        }
    }

    /// Leave the visual mode: the selection goes and the view returns to the
    /// live screen. Returns whether there was one.
    pub fn visual_end(&mut self) -> bool {
        if self.visual.take().is_none() {
            return false;
        }
        let _ = self.terminal.set_selection(None);
        self.selecting.shown = false;
        self.scrolled = false;
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
        self.grid.clear();
        true
    }

    /// In the visual mode now (not suspended).
    pub fn visual_on(&self) -> bool {
        self.visual.as_ref().is_some_and(|v| !v.suspended)
    }

    /// A visual mode, on or suspended.
    pub fn visual_kept(&self) -> bool {
        self.visual.is_some()
    }

    /// What the label at the top right says in the visual mode.
    pub fn visual_label(&self) -> Option<&'static str> {
        let v = self.visual.as_ref().filter(|v| !v.suspended)?;
        Some(match v.selection.as_ref().map(|(_, k)| *k) {
            None => "VISUAL",
            Some(Kind::Char) => "VISUAL CHAR",
            Some(Kind::Line) => "VISUAL LINE",
            Some(Kind::Block) => "VISUAL BLOCK",
        })
    }

    /// The search to highlight (the one being typed, else the last one).
    pub fn visual_query(&self) -> Option<&str> {
        let v = self.visual.as_ref().filter(|v| !v.suspended)?;
        v.search.as_ref().map(|s| s.query.as_str()).filter(|q| !q.is_empty())
    }

    /// The visual cursor in viewport cells, when shown.
    pub(crate) fn visual_cursor(&self) -> Option<(u16, u16)> {
        let v = self.visual.as_ref().filter(|v| !v.suspended)?;
        let p = v.cursor.point(PointSpace::Viewport).ok()??;
        Some((p.x, p.y.try_into().ok()?))
    }

    /// Run `op` `count` times (or with `count` as a row number).
    pub fn visual(&mut self, op: Op, count: Option<usize>) -> Outcome {
        if !self.visual_on() {
            return Outcome::Stay(false);
        }
        self.visual_repair();
        let n = count.unwrap_or(1).clamp(1, 1_000_000);
        let Some(cur) = self.visual_pos() else { return Outcome::Stay(false) };
        let rows = self.geometry.rows.max(1) as u32;
        let last = self.last_row();
        let mut to = cur;
        let mut want = None;
        match op {
            Op::Left | Op::Right => {
                let row = self.row_chars(cur.y);
                let end = end_of(&row);
                for _ in 0..n {
                    let next = if op == Op::Left {
                        row.iter().rev().find(|c| c.x < to.x).map(|c| c.x)
                    } else {
                        row.iter().find(|c| c.x > to.x && c.x <= end).map(|c| c.x)
                    };
                    match next {
                        Some(x) => to.x = x,
                        None => break,
                    }
                }
                want = Some(to.x);
            }
            Op::Up | Op::Down => {
                let y = if op == Op::Up { cur.y.saturating_sub(n as u32) } else { (cur.y + n as u32).min(last) };
                to = Pos { y, x: self.column_in(y, self.visual.as_ref().map_or(0, |v| v.want)) };
            }
            Op::LineStart => {
                to.x = 0;
                want = Some(0);
            }
            Op::FirstNonBlank => {
                to.x = first_non_blank(&self.row_chars(cur.y));
                want = Some(to.x);
            }
            Op::LineEnd => {
                let y = (cur.y + n as u32 - 1).min(last);
                to = Pos { y, x: end_of(&self.row_chars(y)) };
                want = Some(u16::MAX);
            }
            Op::WordForward | Op::BigWordForward => {
                for _ in 0..n {
                    to = self.word_forward(to, op == Op::BigWordForward);
                }
                want = Some(to.x);
            }
            Op::WordBackward | Op::BigWordBackward => {
                for _ in 0..n {
                    to = self.word_backward(to, op == Op::BigWordBackward);
                }
                want = Some(to.x);
            }
            Op::WordEnd | Op::BigWordEnd => {
                for _ in 0..n {
                    to = self.word_end(to, op == Op::BigWordEnd);
                }
                want = Some(to.x);
            }
            Op::Top | Op::Bottom => {
                let y = match count {
                    Some(c) => (c as u32).saturating_sub(1).min(last),
                    None if op == Op::Top => 0,
                    None => self.last_text_row(),
                };
                to = Pos { y, x: first_non_blank(&self.row_chars(y)) };
                want = Some(to.x);
            }
            Op::ScreenTop | Op::ScreenMiddle | Op::ScreenBottom => {
                let top = self.view_top();
                let bottom = (top + rows - 1).min(last);
                let y = match op {
                    Op::ScreenTop => (top + n as u32 - 1).min(bottom),
                    Op::ScreenMiddle => top + (bottom - top) / 2,
                    _ => bottom.saturating_sub(n as u32 - 1).max(top),
                };
                to = Pos { y, x: first_non_blank(&self.row_chars(y)) };
                want = Some(to.x);
            }
            Op::HalfPageUp | Op::HalfPageDown | Op::PageUp | Op::PageDown => {
                let page = match op {
                    Op::HalfPageUp | Op::HalfPageDown => (rows / 2).max(1),
                    _ => rows.saturating_sub(2).max(1),
                } * n as u32;
                let up = matches!(op, Op::HalfPageUp | Op::PageUp);
                self.scroll_view(if up { -(page as isize) } else { page as isize });
                let y = if up { cur.y.saturating_sub(page) } else { (cur.y + page).min(last) };
                to = Pos { y, x: self.column_in(y, self.visual.as_ref().map_or(0, |v| v.want)) };
            }
            Op::ScrollUp | Op::ScrollDown => {
                self.scroll_view(if op == Op::ScrollUp { -(n as isize) } else { n as isize });
                let top = self.view_top();
                let y = cur.y.clamp(top, (top + rows - 1).min(last));
                if y != cur.y {
                    to = Pos { y, x: self.column_in(y, self.visual.as_ref().map_or(0, |v| v.want)) };
                }
            }
            Op::PrevPrompt | Op::NextPrompt => {
                for _ in 0..n {
                    match self.prompt_from(to.y, op == Op::NextPrompt) {
                        Some(y) => to = Pos { y, x: 0 },
                        None => break,
                    }
                }
                want = Some(to.x);
            }
            Op::Charwise | Op::Linewise | Op::Blockwise => {
                let kind = match op {
                    Op::Charwise => Kind::Char,
                    Op::Linewise => Kind::Line,
                    _ => Kind::Block,
                };
                let anchor = self.terminal.track_grid_ref(cur.point()).ok();
                let v = self.visual.as_mut().expect("on");
                v.selection = match v.selection.take() {
                    Some((_, k)) if k == kind => None,
                    Some((a, _)) => Some((a, kind)),
                    None => anchor.map(|a| (a, kind)),
                };
            }
            Op::SwapEnds => {
                if let Some(a) = self.anchor_pos() {
                    if let Some(v) = self.visual.as_mut()
                        && let Some((anchor, _)) = &mut v.selection
                    {
                        let _ = anchor.set(&mut self.terminal, cur.point());
                    }
                    to = a;
                    want = Some(a.x);
                }
            }
            Op::Yank => {
                if self.visual.as_ref().is_some_and(|v| v.selection.is_some()) {
                    self.apply_selection();
                    let kind = match self.visual.as_ref().and_then(|v| v.selection.as_ref()).map(|(_, k)| *k) {
                        Some(Kind::Line) => CopyKind::Line,
                        Some(Kind::Block) => CopyKind::Block,
                        _ => CopyKind::Char,
                    };
                    self.copy_selection(None, kind);
                    return Outcome::Leave;
                }
                return Outcome::Stay(false);
            }
            Op::YankLines => {
                let (a, b) = match self.anchor_pos() {
                    Some(a) => (a.y.min(cur.y), a.y.max(cur.y)),
                    None => (cur.y, (cur.y + n as u32 - 1).min(last)),
                };
                self.copy_rows(a, b);
                return Outcome::Leave;
            }
            Op::Escape => {
                let v = self.visual.as_mut().expect("on");
                if v.selection.take().is_none() {
                    return Outcome::Leave;
                }
            }
            Op::NextMatch | Op::PrevMatch => {
                let Some((query, backward)) =
                    self.visual.as_ref().and_then(|v| v.search.as_ref()).map(|s| (s.query.clone(), s.backward))
                else {
                    return Outcome::Stay(false);
                };
                let backward = backward != (op == Op::PrevMatch);
                for _ in 0..n {
                    match self.find(&query, to, backward) {
                        Some(p) => to = p,
                        None => break,
                    }
                }
                want = Some(to.x);
            }
        }
        self.visual_move(to, want);
        Outcome::Stay(true)
    }

    /// Search the screen and scrollback for `query` (as the PDF search:
    /// plain text, case-insensitive) from the cursor, or from where the
    /// preview began. A preview moves to the first match after that place as
    /// typed (back there when nothing matches); a search keeps the query for
    /// `n` and `N`. Returns whether something matched.
    pub fn visual_search(&mut self, query: &str, backward: bool, preview: bool) -> bool {
        if self.visual.is_none() {
            return false;
        }
        self.visual_repair();
        let Some(cur) = self.visual_pos() else { return false };
        let from = self
            .visual
            .as_ref()
            .and_then(|v| v.origin.as_ref())
            .and_then(|o| o.point(PointSpace::Screen).ok().flatten())
            .map_or(cur, |p| Pos { y: p.y, x: p.x });
        if preview && self.visual.as_ref().is_some_and(|v| v.origin.is_none()) {
            let origin = self.terminal.track_grid_ref(cur.point()).ok();
            if let Some(v) = &mut self.visual {
                v.origin = origin;
            }
        }
        let hit = if query.is_empty() { None } else { self.find(query, from, backward) };
        if let Some(v) = &mut self.visual {
            v.search = Some(Search { query: query.to_owned(), backward });
            if !preview {
                v.origin = None;
            }
        }
        self.visual_move(hit.unwrap_or(from), Some(hit.unwrap_or(from).x));
        hit.is_some()
    }

    /// Drop a search being typed: back to where it began.
    pub fn visual_search_cancel(&mut self) -> bool {
        let Some(origin) = self.visual.as_mut().and_then(|v| v.origin.take()) else { return false };
        let Some(p) = origin.point(PointSpace::Screen).ok().flatten() else { return false };
        if let Some(v) = &mut self.visual {
            v.search = None;
        }
        self.visual_move(Pos { y: p.y, x: p.x }, Some(p.x));
        true
    }

    /// A mouse event in the visual mode (terminal coordinates): a click
    /// moves the cursor, a drag selects characters, double and triple
    /// clicks a word and a row; nothing is copied until `y`. The wheel
    /// scrolls, the cursor kept in view. Programs get none of it.
    pub fn visual_mouse(&mut self, m: &MouseEvent) -> bool {
        if !self.visual_on() {
            return false;
        }
        self.visual_repair();
        let cell = self.geometry.cell;
        let (x, y) = match m.unit {
            CoordUnit::Pixel => (m.x as f32, m.y as f32),
            CoordUnit::Cell => ((m.x as f32 + 0.5) * cell.width as f32, (m.y as f32 + 0.5) * cell.height as f32),
        };
        let rows = match m.kind {
            MouseEventKind::Scroll(ScrollDirection::Up) => -WHEEL_ROWS as f32,
            MouseEventKind::Scroll(ScrollDirection::Down) => WHEEL_ROWS as f32,
            MouseEventKind::ScrollBy { dy, unit: ScrollUnit::Line, .. } => dy as f32 / 100.0 * WHEEL_ROWS as f32,
            MouseEventKind::ScrollBy { dy, unit: ScrollUnit::Pixel, .. } => {
                dy as f32 / 100.0 / cell.height.max(1) as f32
            }
            _ => 0.0,
        };
        if rows != 0.0 {
            self.scroll_rows += rows;
            let whole = self.scroll_rows.trunc();
            self.scroll_rows -= whole;
            if whole == 0.0 {
                return false;
            }
            return self.visual(if whole < 0.0 { Op::ScrollUp } else { Op::ScrollDown }, Some(whole.abs() as usize))
                != Outcome::Stay(false);
        }
        let at = self.cell_at(x, y);
        let Some(p) = self
            .terminal
            .grid_ref(Point::Viewport(at))
            .ok()
            .and_then(|g| self.terminal.point_from_grid_ref(&g, PointSpace::Screen).ok().flatten())
            .map(|p| self.char_start(Pos { y: p.y, x: p.x }))
        else {
            return false;
        };
        let now = Instant::now();
        match m.kind {
            MouseEventKind::Press(MouseButton::Left) => {
                let clicks = match self.visual.as_ref().and_then(|v| v.press.as_ref()) {
                    Some(prev) if prev.at == p && now.duration_since(prev.time) < MULTI_CLICK => prev.clicks % 3 + 1,
                    _ => 1,
                };
                if let Some(v) = &mut self.visual {
                    v.press = Some(Press { at: p, time: now, clicks, dragging: clicks == 1 });
                    v.selection = None;
                }
                match clicks {
                    1 => self.visual_move(p, Some(p.x)),
                    2 => {
                        let word = self.terminal.grid_ref(p.point()).ok().and_then(|g| {
                            let sel = self.terminal.select_word(SelectWordOptions::new(g)).ok()??;
                            let sel = sel.to_ordered(&self.terminal, Order::Forward).ok()?;
                            let point = |g: libghostty_vt::screen::GridRef<'_>| {
                                self.terminal.point_from_grid_ref(&g, PointSpace::Screen).ok().flatten()
                            };
                            Some((point(sel.start())?, point(sel.end())?))
                        });
                        if let Some((a, b)) = word {
                            self.set_anchor(Pos { y: a.y, x: a.x }, Kind::Char);
                            self.visual_move(Pos { y: b.y, x: b.x }, Some(b.x));
                        }
                    }
                    _ => {
                        let end = end_of(&self.row_chars(p.y));
                        self.set_anchor(Pos { y: p.y, x: 0 }, Kind::Line);
                        self.visual_move(Pos { y: p.y, x: end }, Some(u16::MAX));
                    }
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(start) =
                    self.visual.as_ref().and_then(|v| v.press.as_ref()).filter(|p| p.dragging).map(|p| p.at)
                else {
                    return false;
                };
                if self.visual.as_ref().is_some_and(|v| v.selection.is_none()) {
                    if p == start {
                        return false;
                    }
                    self.set_anchor(start, Kind::Char);
                }
                self.visual_move(p, Some(p.x));
                true
            }
            MouseEventKind::Release(MouseButton::Left) => {
                if let Some(press) = self.visual.as_mut().and_then(|v| v.press.as_mut()) {
                    press.dragging = false;
                }
                false
            }
            _ => false,
        }
    }

    // --- Positions and rows.

    /// The cursor or the selection's other end lost its row (old rows went):
    /// the cursor goes to the first row, the selection goes.
    fn visual_repair(&mut self) {
        let Some(v) = &mut self.visual else { return };
        if !v.cursor.has_value() {
            let _ = v.cursor.set(&mut self.terminal, Point::Screen(PointCoordinate { x: 0, y: 0 }));
        }
        if v.selection.as_ref().is_some_and(|(a, _)| !a.has_value()) {
            v.selection = None;
        }
        if v.origin.as_ref().is_some_and(|o| !o.has_value()) {
            v.origin = None;
        }
    }

    fn visual_pos(&self) -> Option<Pos> {
        let p = self.visual.as_ref()?.cursor.point(PointSpace::Screen).ok()??;
        Some(Pos { y: p.y, x: p.x })
    }

    fn anchor_pos(&self) -> Option<Pos> {
        let (a, _) = self.visual.as_ref()?.selection.as_ref()?;
        let p = a.point(PointSpace::Screen).ok()??;
        Some(Pos { y: p.y, x: p.x })
    }

    fn set_anchor(&mut self, at: Pos, kind: Kind) {
        let anchor = self.terminal.track_grid_ref(at.point()).ok();
        if let Some(v) = &mut self.visual {
            v.selection = anchor.map(|a| (a, kind));
        }
    }

    /// Put the cursor at `to` (and keep `want` for up and down), bring it
    /// into view, and show the selection.
    fn visual_move(&mut self, to: Pos, want: Option<u16>) {
        if let Some(v) = &mut self.visual {
            let _ = v.cursor.set(&mut self.terminal, to.point());
            if let Some(w) = want {
                v.want = w;
            }
        }
        self.reveal(to.y);
        self.apply_selection();
        self.grid.clear();
    }

    /// Scroll the view so that row `y` shows.
    fn reveal(&mut self, y: u32) {
        let rows = self.geometry.rows.max(1) as u32;
        let top = self.view_top();
        let delta = if y < top {
            y as isize - top as isize
        } else if y >= top + rows {
            (y - (top + rows - 1)) as isize
        } else {
            0
        };
        if delta != 0 {
            self.scroll_view(delta);
        }
    }

    /// Scroll the view by `rows` (negative: up), noting whether it left the
    /// bottom.
    fn scroll_view(&mut self, rows: isize) {
        self.terminal.scroll_viewport(ScrollViewport::Delta(rows));
        let shown = self.geometry.rows.max(1) as u32;
        self.scrolled = self.view_top() + shown < self.total_rows();
    }

    /// The first row shown (screen coordinates).
    fn view_top(&self) -> u32 {
        self.terminal
            .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: 0 }))
            .ok()
            .and_then(|g| self.terminal.point_from_grid_ref(&g, PointSpace::Screen).ok().flatten())
            .map_or(0, |p| p.y)
    }

    fn total_rows(&self) -> u32 {
        self.terminal.total_rows().unwrap_or(0) as u32
    }

    fn last_row(&self) -> u32 {
        self.total_rows().saturating_sub(1)
    }

    /// The last row with text (the live screen has empty rows below the
    /// prompt).
    fn last_text_row(&self) -> u32 {
        let last = self.last_row();
        let rows = self.geometry.rows.max(1) as u32;
        (last.saturating_sub(rows)..=last).rev().find(|&y| self.row_chars(y).iter().any(|c| !c.blank)).unwrap_or(last)
    }

    /// The characters of row `y`.
    fn row_chars(&self, y: u32) -> Vec<Ch> {
        let cols = self.geometry.cols.max(1);
        let mut out = Vec::with_capacity(cols as usize);
        let mut buf = ['\0'; 16];
        for x in 0..cols {
            let Ok(g) = self.terminal.grid_ref(Point::Screen(PointCoordinate { x, y })) else { break };
            let Ok(cell) = g.cell() else { continue };
            if matches!(cell.wide(), Ok(CellWide::SpacerTail | CellWide::SpacerHead)) {
                continue;
            }
            let n = g.graphemes(&mut buf).unwrap_or(0).min(buf.len());
            let text: String = buf[..n].iter().collect();
            let blank = text.chars().all(char::is_whitespace);
            out.push(Ch { x, text: if text.is_empty() { " ".into() } else { text }, blank });
        }
        out
    }

    /// The column on row `y` closest to `want` (not past its last character).
    fn column_in(&self, y: u32, want: u16) -> u16 {
        let row = self.row_chars(y);
        let target = want.min(end_of(&row));
        row.iter().rev().find(|c| c.x <= target).map_or(0, |c| c.x)
    }

    /// The first column of the character covering `p`.
    fn char_start(&self, p: Pos) -> Pos {
        let row = self.row_chars(p.y);
        Pos { y: p.y, x: row.iter().rev().find(|c| c.x <= p.x).map_or(0, |c| c.x) }
    }

    /// The row of the next (or previous) shell prompt after row `y`.
    fn prompt_from(&self, y: u32, down: bool) -> Option<u32> {
        let prompt = |y: u32| {
            self.terminal
                .grid_ref(Point::Screen(PointCoordinate { x: 0, y }))
                .and_then(|g| g.row())
                .and_then(|r| r.semantic_prompt())
                .is_ok_and(|p| p == RowSemanticPrompt::Prompt)
        };
        if down { (y + 1..=self.last_row()).find(|&y| prompt(y)) } else { (0..y).rev().find(|&y| prompt(y)) }
    }

    // --- Words.

    /// Every character from `from` on (or back), rows apart by a blank.
    fn chars_from(&self, from: Pos, back: bool, big: bool) -> impl Iterator<Item = (Pos, u8)> + '_ {
        let last = self.last_row();
        let mut y = from.y;
        let mut row: Vec<(Pos, u8)> = Vec::new();
        let mut first = true;
        std::iter::from_fn(move || {
            loop {
                if let Some(next) = if back { row.pop() } else { (!row.is_empty()).then(|| row.remove(0)) } {
                    return Some(next);
                }
                if !first {
                    if back {
                        y = y.checked_sub(1)?;
                    } else if y >= last {
                        return None;
                    } else {
                        y += 1;
                    }
                }
                let chars = self.row_chars(y);
                let mut cells: Vec<(Pos, u8)> =
                    chars.iter().map(|c| (Pos { y, x: c.x }, if c.blank { 0 } else { class(&c.text, big) })).collect();
                // Rows are apart: a blank between them (a soft wrap is not).
                let wrapped = |y: u32| {
                    self.terminal
                        .grid_ref(Point::Screen(PointCoordinate { x: 0, y }))
                        .and_then(|g| g.row())
                        .and_then(|r| r.is_wrapped())
                        .unwrap_or(false)
                };
                if !wrapped(y) {
                    cells.push((Pos { y, x: u16::MAX }, 0));
                }
                if first {
                    cells.retain(|(p, _)| if back { *p < from } else { *p > from });
                    first = false;
                }
                row = cells;
            }
        })
    }

    fn class_at(&self, p: Pos, big: bool) -> u8 {
        self.row_chars(p.y).iter().find(|c| c.x == p.x).map_or(0, |c| if c.blank { 0 } else { class(&c.text, big) })
    }

    /// `w`: past this word and the blanks after it.
    fn word_forward(&self, from: Pos, big: bool) -> Pos {
        let mut class = self.class_at(from, big);
        for (p, c) in self.chars_from(from, false, big) {
            if c != 0 && c != class {
                return p;
            }
            if c == 0 {
                class = 0;
            }
        }
        from
    }

    /// `b`: back to the start of this word, or of the one before.
    fn word_backward(&self, from: Pos, big: bool) -> Pos {
        let mut at: Option<(Pos, u8)> = None;
        for (p, c) in self.chars_from(from, true, big) {
            match at {
                None if c == 0 => {}
                None => at = Some((p, c)),
                Some((_, class)) if c == class => at = Some((p, c)),
                Some(_) => break,
            }
        }
        at.map_or(from, |(p, _)| p)
    }

    /// `e`: to the end of this word, or of the next.
    fn word_end(&self, from: Pos, big: bool) -> Pos {
        let mut at: Option<(Pos, u8)> = None;
        for (p, c) in self.chars_from(from, false, big) {
            match at {
                None if c == 0 => {}
                None => at = Some((p, c)),
                Some((_, class)) if c == class => at = Some((p, c)),
                Some(_) => break,
            }
        }
        at.map_or(from, |(p, _)| p)
    }

    // --- The selection.

    /// Install the selection (from the other end to the cursor) in the
    /// terminal, which draws and copies it.
    fn apply_selection(&mut self) {
        let (Some(cur), Some(anchor)) = (self.visual_pos(), self.anchor_pos()) else {
            let _ = self.terminal.set_selection(None);
            return;
        };
        let kind = self.visual.as_ref().and_then(|v| v.selection.as_ref()).map_or(Kind::Char, |(_, k)| *k);
        let (a, b) = match kind {
            Kind::Line => {
                let (top, bottom) = (anchor.y.min(cur.y), anchor.y.max(cur.y));
                (Pos { y: top, x: 0 }, Pos { y: bottom, x: self.geometry.cols.max(1) - 1 })
            }
            _ => (anchor, cur),
        };
        let t = &self.terminal;
        if let (Ok(ga), Ok(gb)) = (t.grid_ref(a.point()), t.grid_ref(b.point())) {
            let sel = Selection::new(ga, gb, kind == Kind::Block);
            let _ = t.set_selection(Some(&sel));
        }
    }

    /// Copy the installed selection (or `sel`), as `kind`.
    fn copy_selection(&mut self, sel: Option<&Selection<'_>>, kind: CopyKind) {
        let mut options =
            FormatOptions::new().with_emit_format(libghostty_vt::fmt::Format::Plain).with_unwrap(true).with_trim(true);
        if let Some(s) = sel {
            options = options.with_selection(s);
        }
        if let Ok(Some(bytes)) = self.terminal.format_selection_alloc(None, options) {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if !text.is_empty() {
                self.effects.borrow_mut().push(Effect::Copied { text, kind });
            }
        }
    }

    /// Copy rows `a` to `b` whole.
    fn copy_rows(&mut self, a: u32, b: u32) {
        let cols = self.geometry.cols.max(1);
        let t = &self.terminal;
        let (Ok(ga), Ok(gb)) = (
            t.grid_ref(Point::Screen(PointCoordinate { x: 0, y: a })),
            t.grid_ref(Point::Screen(PointCoordinate { x: cols - 1, y: b })),
        ) else {
            return;
        };
        let sel = Selection::new(ga, gb, false);
        let options = FormatOptions::new()
            .with_emit_format(libghostty_vt::fmt::Format::Plain)
            .with_unwrap(true)
            .with_trim(true)
            .with_selection(&sel);
        if let Ok(Some(bytes)) = t.format_selection_alloc(None, options) {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if !text.is_empty() {
                self.effects.borrow_mut().push(Effect::Copied { text, kind: CopyKind::Line });
            }
        }
    }

    // --- Search.

    /// The rows of the screen and scrollback as text, and the row the first
    /// one is.
    fn screen_lines(&self) -> Option<(u32, Vec<String>)> {
        let t = &self.terminal;
        let all = t.select_all().ok()??.to_ordered(t, Order::Forward).ok()?;
        let first = t.point_from_grid_ref(&all.start(), PointSpace::Screen).ok()??.y;
        let options = FormatOptions::new()
            .with_emit_format(libghostty_vt::fmt::Format::Plain)
            .with_unwrap(false)
            .with_trim(true)
            .with_selection(&all);
        let bytes = t.format_selection_alloc(None, options).ok()??;
        let text = String::from_utf8_lossy(&bytes);
        Some((first, text.split('\n').map(str::to_owned).collect()))
    }

    /// The columns where `needle` (lowercase) starts on row `y`.
    fn matches_on(&self, y: u32, needle: &[char]) -> Vec<u16> {
        let row = self.row_chars(y);
        let mut chars: Vec<(char, u16)> = Vec::new();
        for c in &row {
            for ch in c.text.chars() {
                chars.push((lower(ch), c.x));
            }
        }
        (0..chars.len())
            .filter(|&i| {
                chars.len() - i >= needle.len()
                    && chars[i..i + needle.len()].iter().map(|(c, _)| *c).eq(needle.iter().copied())
            })
            .map(|i| chars[i].1)
            .collect()
    }

    /// The next match of `query` after `from` (or before it), wrapping
    /// around the scrollback.
    fn find(&self, query: &str, from: Pos, backward: bool) -> Option<Pos> {
        let needle: Vec<char> = query.chars().map(lower).collect();
        let needle_s: String = needle.iter().collect();
        let (first, lines) = self.screen_lines()?;
        let n = lines.len() as u32;
        if n == 0 {
            return None;
        }
        let start = from.y.saturating_sub(first).min(n - 1);
        // Rows in search order, the starting row again at the end (its other
        // side).
        for k in 0..=n {
            let i = if backward { (start + n - k % n) % n } else { (start + k) % n };
            let line: String = lines[i as usize].chars().map(lower).collect();
            if !line.contains(&needle_s) {
                continue;
            }
            let y = first + i;
            let xs = self.matches_on(y, &needle);
            let pick = match (k, backward) {
                (0, false) => xs.into_iter().find(|&x| x > from.x),
                (0, true) => xs.into_iter().rev().find(|&x| x < from.x),
                (_, false) => xs.into_iter().next(),
                (_, true) => xs.into_iter().next_back(),
            };
            if let Some(x) = pick {
                return Some(Pos { y, x });
            }
        }
        None
    }
}

/// The last column with text on a row (0 when it has none).
fn end_of(row: &[Ch]) -> u16 {
    row.iter().rev().find(|c| !c.blank).map_or(0, |c| c.x)
}

fn first_non_blank(row: &[Ch]) -> u16 {
    row.iter().find(|c| !c.blank).map_or(0, |c| c.x)
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Highlight where `query` shows in `rows` (cells of the view), as Vim's
/// `hlsearch`: black on yellow.
pub(crate) fn highlight(rows: &mut [Vec<Cell>], query: &str) {
    let needle: Vec<char> = query.chars().map(lower).collect();
    if needle.is_empty() {
        return;
    }
    for row in rows {
        let mut chars: Vec<(char, usize)> = Vec::new();
        for (i, c) in row.iter().enumerate() {
            if c.width == 0 {
                continue;
            }
            if c.text.is_empty() {
                chars.push((' ', i));
            }
            for ch in c.text.chars() {
                chars.push((lower(ch), i));
            }
        }
        let mut i = 0;
        while i + needle.len() <= chars.len() {
            if chars[i..i + needle.len()].iter().map(|(c, _)| *c).eq(needle.iter().copied()) {
                let (from, to) = (chars[i].1, chars[i + needle.len() - 1].1);
                for c in &mut row[from..=to] {
                    c.fg = Color::Indexed(0);
                    c.bg = Color::Indexed(11);
                }
                i += needle.len();
            } else {
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Sink, Spawn, TermView};
    use std::sync::Arc;
    use temzdf_scene::Images;
    use termpix_core::geom::{CellSize, Geometry};
    use termpix_core::input::{KeyCode, KeyEvent, Modifiers};

    const GEOMETRY: Geometry = Geometry { cols: 40, rows: 6, cell: CellSize { width: 10, height: 20 } };

    /// A terminal showing `text` (fed as output; its program only sleeps),
    /// in the visual mode.
    fn term(text: &str) -> TermBuffer {
        let sink: Sink = Arc::new(|_| true);
        let spawn = Spawn { argv: Some(vec!["sleep".into(), "30".into()]), ..Default::default() };
        let mut t = TermBuffer::spawn(&spawn, GEOMETRY, sink).unwrap();
        t.feed(text.replace('\n', "\r\n").as_bytes());
        assert!(t.visual_begin());
        t
    }

    /// The cursor: row and column.
    fn at(t: &TermBuffer) -> (u32, u16) {
        let p = t.visual_pos().unwrap();
        (p.y, p.x)
    }

    fn go(t: &mut TermBuffer, op: Op) -> Outcome {
        t.visual(op, None)
    }

    fn copies(t: &mut TermBuffer) -> Vec<String> {
        t.take_effects()
            .into_iter()
            .filter_map(|e| match e {
                Effect::Copied { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    /// The rows the view shows, as text.
    fn shown(t: &mut TermBuffer) -> Vec<String> {
        TermView::new(GEOMETRY).frame(t, &mut Images::new()).text[0]
            .rows
            .iter()
            .map(|r| {
                r.iter()
                    .filter(|c| c.width > 0)
                    .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn moves_by_characters_rows_and_words() {
        let mut t = term("hello world\n  foo-bar baz\n日本語 text");
        assert_eq!(at(&t), (2, 11), "it starts at the program's cursor");
        go(&mut t, Op::LineStart);
        // Wide characters are one character each.
        go(&mut t, Op::Right);
        assert_eq!(at(&t), (2, 2));
        go(&mut t, Op::Right);
        assert_eq!(at(&t), (2, 4));
        go(&mut t, Op::Left);
        assert_eq!(at(&t), (2, 2));
        // Up and down keep the column.
        go(&mut t, Op::Up);
        assert_eq!(at(&t), (1, 2));
        t.visual(Op::Up, Some(5));
        assert_eq!(at(&t), (0, 2), "a count past the top stops there");
        go(&mut t, Op::LineEnd);
        assert_eq!(at(&t), (0, 10));
        go(&mut t, Op::Down);
        assert_eq!(at(&t), (1, 12), "after $, down goes to the end of the row");
        go(&mut t, Op::FirstNonBlank);
        assert_eq!(at(&t), (1, 2));
        // Words: letters, then punctuation, then blanks part them.
        let mut words = Vec::new();
        for _ in 0..4 {
            go(&mut t, Op::WordForward);
            words.push(at(&t));
        }
        assert_eq!(words, [(1, 5), (1, 6), (1, 10), (2, 0)]);
        go(&mut t, Op::WordEnd);
        assert_eq!(at(&t), (2, 4), "the end of 日本語");
        go(&mut t, Op::WordBackward);
        assert_eq!(at(&t), (2, 0));
        go(&mut t, Op::WordBackward);
        assert_eq!(at(&t), (1, 10), "back across the row");
        t.visual(Op::BigWordBackward, Some(1));
        assert_eq!(at(&t), (1, 2), "foo-bar is one blank-separated word");
        go(&mut t, Op::BigWordForward);
        assert_eq!(at(&t), (1, 10));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn selections_copy_characters_rows_and_blocks() {
        let mut t = term("abc def\nghi jkl\nmno pqr");
        assert_eq!(t.visual_label(), Some("VISUAL"));
        go(&mut t, Op::Top);
        go(&mut t, Op::Charwise);
        assert_eq!(t.visual_label(), Some("VISUAL CHAR"));
        t.visual(Op::Right, Some(4));
        go(&mut t, Op::Down);
        assert!(t.frame().text[0].rows[1][3].attrs.inverse, "the selection shows inverted");
        assert_eq!(go(&mut t, Op::Yank), Outcome::Leave);
        assert_eq!(copies(&mut t), ["abc def\nghi j"]);
        t.visual_end();

        t.visual_begin();
        go(&mut t, Op::Top);
        go(&mut t, Op::Linewise);
        go(&mut t, Op::Down);
        assert_eq!(go(&mut t, Op::Yank), Outcome::Leave);
        assert_eq!(copies(&mut t), ["abc def\nghi jkl"], "no line break after the last row");
        t.visual_end();

        t.visual_begin();
        go(&mut t, Op::Top);
        go(&mut t, Op::Right);
        go(&mut t, Op::Blockwise);
        go(&mut t, Op::Down);
        go(&mut t, Op::Right);
        go(&mut t, Op::Yank);
        assert_eq!(copies(&mut t), ["bc\nhi"]);
        t.visual_end();

        // o: the cursor to the other end, which then moves.
        t.visual_begin();
        go(&mut t, Op::Top);
        t.visual(Op::Right, Some(2));
        go(&mut t, Op::Charwise);
        t.visual(Op::Right, Some(2));
        go(&mut t, Op::SwapEnds);
        assert_eq!(at(&t), (0, 2));
        go(&mut t, Op::Left);
        go(&mut t, Op::Yank);
        assert_eq!(copies(&mut t), ["bc d"]);
        t.visual_end();

        // The same key again stops selecting; <Esc> drops, then leaves.
        t.visual_begin();
        go(&mut t, Op::Charwise);
        go(&mut t, Op::Charwise);
        assert_eq!(t.visual_label(), Some("VISUAL"));
        assert_eq!(go(&mut t, Op::Yank), Outcome::Stay(false), "nothing to copy");
        go(&mut t, Op::Linewise);
        assert_eq!(go(&mut t, Op::Escape), Outcome::Stay(true));
        assert_eq!(t.visual_label(), Some("VISUAL"));
        assert_eq!(go(&mut t, Op::Escape), Outcome::Leave);

        // Y: rows, the count's many.
        go(&mut t, Op::Top);
        assert_eq!(t.visual(Op::YankLines, Some(2)), Outcome::Leave);
        assert_eq!(copies(&mut t), ["abc def\nghi jkl"]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn copies_join_soft_wrapped_rows() {
        let long = "0123456789".repeat(5);
        let mut t = term(&format!("{long}\nend"));
        go(&mut t, Op::Top);
        go(&mut t, Op::Linewise);
        go(&mut t, Op::Down);
        go(&mut t, Op::Yank);
        assert_eq!(copies(&mut t), [long]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn the_view_follows_the_cursor_through_the_scrollback() {
        let lines: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let mut t = term(&lines);
        assert_eq!(shown(&mut t)[0], "line 26", "at the bottom, no label");
        go(&mut t, Op::Top);
        assert_eq!(at(&t), (0, 0));
        let rows = shown(&mut t);
        assert!(rows[0].starts_with("line 1 ") && rows[0].ends_with(" [25/25]"), "{rows:?}");
        // New output: the cursor stays on its character.
        t.feed(b"more\r\nand more\r\n");
        let p = t.visual_pos().unwrap();
        assert_eq!(t.row_chars(p.y).iter().map(|c| c.text.as_str()).collect::<String>().trim_end(), "line 1");
        go(&mut t, Op::Bottom);
        assert_eq!(t.row_chars(at(&t).0).iter().map(|c| c.text.as_str()).collect::<String>().trim_end(), "and more");
        t.visual(Op::Top, Some(10));
        assert_eq!(at(&t), (9, 0), "a count is a row number");
        go(&mut t, Op::ScreenBottom);
        assert_eq!(at(&t).0, 14, "the bottom row shown");
        go(&mut t, Op::HalfPageDown);
        assert_eq!(at(&t).0, 17);
        // Leaving returns to the live screen.
        t.visual_end();
        assert_eq!(&shown(&mut t)[3..5], ["more", "and more"]);
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn prompts_are_found_by_their_marks() {
        let mut t = term("\x1b]133;A\x07$ one\nout\n\x1b]133;A\x07$ two\nout 2");
        go(&mut t, Op::PrevPrompt);
        assert_eq!(at(&t), (2, 0));
        go(&mut t, Op::PrevPrompt);
        assert_eq!(at(&t), (0, 0));
        go(&mut t, Op::PrevPrompt);
        assert_eq!(at(&t), (0, 0), "none above");
        go(&mut t, Op::NextPrompt);
        assert_eq!(at(&t), (2, 0));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn searches_wrap_ignore_case_and_preview() {
        let mut t = term("apple\nbanana\n日本 Apple pie\ncherry");
        assert!(t.visual_search("apple", false, false));
        assert_eq!(at(&t), (0, 0), "from the end, wrapping to the top");
        go(&mut t, Op::NextMatch);
        assert_eq!(at(&t), (2, 5), "case does not matter; wide characters count two columns");
        go(&mut t, Op::NextMatch);
        assert_eq!(at(&t), (0, 0));
        go(&mut t, Op::PrevMatch);
        assert_eq!(at(&t), (2, 5));
        // The match shows black on yellow.
        let frame = TermView::new(GEOMETRY).frame(&mut t, &mut Images::new());
        assert_eq!(frame.text[0].rows[2][6].bg, Color::Indexed(11));
        assert_eq!(frame.text[0].rows[2][0].bg, Color::Default);
        // A preview moves as typed and goes back when nothing matches or
        // when cancelled.
        assert!(t.visual_search("nan", false, true));
        assert_eq!(at(&t), (1, 2));
        assert!(!t.visual_search("nanx", false, true));
        assert_eq!(at(&t), (2, 5), "back where the preview began");
        t.visual_search("ch", false, true);
        assert_eq!(at(&t), (3, 0));
        assert!(t.visual_search_cancel());
        assert_eq!(at(&t), (2, 5));
        // Backwards.
        assert!(t.visual_search("an", true, false));
        assert_eq!(at(&t), (1, 3));
        go(&mut t, Op::NextMatch);
        assert_eq!(at(&t), (1, 1), "n keeps the direction");
    }

    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        let (x, y) = (col as i32 * 10 + 5, row as i32 * 20 + 10);
        MouseEvent { kind, x, y, unit: CoordUnit::Pixel, modifiers: Modifiers::NONE }
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn the_mouse_moves_and_selects_without_copying() {
        let mut t = term("hello world\nsecond line");
        let left = MouseButton::Left;
        t.visual_mouse(&mouse(MouseEventKind::Press(left), 6, 0));
        assert_eq!(at(&t), (0, 6), "a click moves the cursor");
        t.visual_mouse(&mouse(MouseEventKind::Drag(left), 2, 1));
        t.visual_mouse(&mouse(MouseEventKind::Release(left), 2, 1));
        assert!(copies(&mut t).is_empty(), "releasing copies nothing");
        assert_eq!(t.visual_label(), Some("VISUAL CHAR"));
        go(&mut t, Op::Yank);
        assert_eq!(copies(&mut t), ["world\nsec"]);
        t.visual_end();
        t.visual_begin();
        for kind in [MouseEventKind::Press(left), MouseEventKind::Release(left)].repeat(2) {
            t.visual_mouse(&mouse(kind, 8, 1));
        }
        go(&mut t, Op::Yank);
        assert_eq!(copies(&mut t), ["line"], "a double click selects a word");
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri cannot open a pty")]
    fn suspended_visual_modes_go_on_or_end_with_typing() {
        let mut t = term("one\ntwo");
        go(&mut t, Op::Top);
        go(&mut t, Op::Charwise);
        assert_eq!(t.frame().cursor, Some((0, 0)), "the cursor is the visual one");
        t.visual_suspend();
        assert!(!t.visual_on() && t.visual_kept());
        assert_eq!(t.visual_label(), None);
        assert_eq!(t.frame().cursor, Some((3, 1)), "the program's cursor while suspended");
        t.visual_begin();
        assert_eq!((at(&t), t.visual_label()), ((0, 0), Some("VISUAL CHAR")), "it goes on");
        t.visual_suspend();
        t.key(&KeyEvent::new(KeyCode::Char('x'), Modifiers::NONE));
        assert!(!t.visual_kept(), "typing ends it");
        assert!(!t.frame().text[0].rows[0][0].attrs.inverse);
    }
}
