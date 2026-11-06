//! A pane's view of a buffer: its cursor and selection, where it is
//! scrolled to, how it lays text out, and the motions that need all that
//! (lines keeping a column, display lines, pages).

use crate::buffer::Buffer;
use crate::cursor::{Cursor, SelectionKind};
use crate::edit::{Change, Gravity, map_offset};
use crate::layout::{Layout, LayoutOptions, Row};
use crate::motion::{self, Unit};
use crate::text::Text;

/// Line numbers in the gutter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NumberMode {
    #[default]
    Off,
    Absolute,
    /// Distance from the cursor's line.
    Relative,
    /// Relative, with the cursor's line absolute.
    Both,
}

/// Where `reveal` puts a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reveal {
    /// Scroll as little as possible (keeping `scrolloff`).
    Minimal,
    Center,
    Top,
}

/// What a pane moves by beyond [`Unit`]: these need the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewUnit {
    Text(Unit),
    /// Buffer lines, keeping the display column (`j`, `k`).
    Line,
    /// Display rows, keeping the column in the row (`gj`, `gk`).
    DisplayLine,
    /// A screenful less two rows.
    Page,
}

impl ViewUnit {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "line" => ViewUnit::Line,
            "displayLine" => ViewUnit::DisplayLine,
            "page" => ViewUnit::Page,
            other => ViewUnit::Text(Unit::from_name(other)?),
        })
    }
}

#[derive(Clone, Debug)]
pub struct View {
    pub cursor: Cursor,
    /// Where the top row starts.
    pub top: usize,
    /// The first display column shown, without wrapping.
    pub left: usize,
    /// The pane's size in cells, gutter included.
    pub width: u16,
    pub height: u16,
    pub layout: LayoutOptions,
    pub number: NumberMode,
    pub cursorline: bool,
    /// Rows kept between the cursor and the top or bottom.
    pub scrolloff: usize,
}

impl Default for View {
    fn default() -> Self {
        Self {
            cursor: Cursor::default(),
            top: 0,
            left: 0,
            width: 80,
            height: 24,
            layout: LayoutOptions::default(),
            number: NumberMode::Off,
            cursorline: false,
            scrolloff: 0,
        }
    }
}

impl View {
    /// Columns of the gutter (numbers and a space), for a text of `lines`.
    pub fn gutter(&self, lines: usize) -> usize {
        match self.number {
            NumberMode::Off => 0,
            _ => lines.max(999).to_string().len() + 1,
        }
    }

    /// Columns for the text.
    pub fn text_width(&self, text: &Text) -> usize {
        (self.width as usize).saturating_sub(self.gutter(text.line_count())).max(1)
    }

    pub fn layouter<'a>(&'a self, text: &'a Text) -> Layout<'a> {
        Layout { text, opts: &self.layout, width: self.text_width(text) }
    }

    /// The rows shown, top first.
    pub fn rows(&self, text: &Text) -> Vec<Row> {
        let l = self.layouter(text);
        let limit = self.left + l.width;
        let mut rows = Vec::with_capacity(self.height as usize);
        let mut at = Some(self.top);
        while let Some(start) = at
            && rows.len() < self.height as usize
        {
            let row = l.row_at(start, limit);
            at = l.next_row(&row);
            rows.push(row);
        }
        rows
    }

    /// Follow edits: the cursor (an insertion at it leaves it after) and
    /// the top (kept at a row start).
    pub fn follow(&mut self, text: &Text, changes: &[Change]) {
        self.cursor.follow(changes);
        let top = map_offset(self.top, changes, Gravity::Before).0;
        let top = self.layouter(text).row_start_of(top);
        self.top = top;
    }

    /// The layout changed (width, wrapping): the top must be a row start
    /// again.
    pub fn relayout(&mut self, text: &Text) {
        let top = self.layouter(text).row_start_of(self.top.min(text.len_chars()));
        self.top = top;
        self.reveal(text, self.cursor.pos, Reveal::Minimal);
    }

    /// Where `count` units from `from` is, and the column vertical moves
    /// keep (`None` for other units).
    pub fn position_of(&self, buf: &Buffer, from: usize, unit: ViewUnit, count: i64) -> (usize, Option<usize>) {
        let text = buf.text();
        let l = self.layouter(text);
        match unit {
            ViewUnit::Text(u) => (motion::position_of(text, from, u, count, &buf.wordchars), None),
            ViewUnit::Line => {
                let want = self.want(from, |o| l.line_col(o));
                let line = (text.line_of(from) as i64 + count).clamp(0, text.line_count() as i64 - 1) as usize;
                (l.offset_at_line_col(line, want), Some(want))
            }
            ViewUnit::DisplayLine | ViewUnit::Page => {
                let n =
                    if unit == ViewUnit::Page { count.saturating_mul((self.height as i64 - 2).max(1)) } else { count };
                let row = l.row_of(from);
                let want = self.want(from, |o| row.col_of(o) - row.prefix);
                let mut start = row.start;
                for _ in 0..n.unsigned_abs() {
                    let next = if n < 0 { l.prev_row(start) } else { l.next_row(&l.row_at(start, usize::MAX)) };
                    match next {
                        Some(s) => start = s,
                        None => break,
                    }
                }
                let row = l.row_at(start, usize::MAX);
                let mut at = row.offset_at(row.prefix + want);
                // The column fell past a wrapped row: its last character,
                // not the next row's start.
                if row.wrapped && at >= row.end {
                    at = row.glyphs.last().map_or(row.start, |g| g.offset);
                }
                (at, Some(want))
            }
        }
    }

    /// The column a vertical move keeps: the one kept so far, else the
    /// cursor's now.
    fn want(&self, from: usize, col: impl Fn(usize) -> usize) -> usize {
        match self.cursor.want_col {
            Some(w) if from == self.cursor.pos => w,
            _ => col(from),
        }
    }

    /// Move the cursor; with `select`, extend the selection (starting one
    /// at the old position), else drop it. Then keep it in sight.
    pub fn move_cursor(&mut self, buf: &Buffer, unit: ViewUnit, count: i64, select: bool) {
        let (to, want) = self.position_of(buf, self.cursor.pos, unit, count);
        self.set_cursor(buf.text(), to, select);
        self.cursor.want_col = want;
    }

    /// Put the cursor at `offset` (see [`move_cursor`](Self::move_cursor)).
    pub fn set_cursor(&mut self, text: &Text, offset: usize, select: bool) {
        let offset = offset.min(text.len_chars());
        if select {
            let anchor = self.cursor.anchor.unwrap_or((self.cursor.pos, SelectionKind::Char));
            self.cursor.anchor = Some(anchor);
        } else {
            self.cursor.anchor = None;
        }
        self.cursor.pos = offset;
        self.cursor.want_col = None;
        self.reveal(text, offset, Reveal::Minimal);
    }

    /// Scroll by `rows` display rows (negative: up); the cursor stays in
    /// sight.
    pub fn scroll(&mut self, text: &Text, rows: i64) {
        let opts = self.layout.clone();
        let l = Layout { text, opts: &opts, width: self.text_width(text) };
        for _ in 0..rows.unsigned_abs() {
            let next = if rows < 0 { l.prev_row(self.top) } else { l.next_row(&l.row_at(self.top, usize::MAX)) };
            match next {
                Some(s) => self.top = s,
                None => break,
            }
        }
        // The cursor into the rows shown (keeping its column).
        let shown = self.rows(text);
        let (Some(first), Some(last)) = (shown.first(), shown.last()) else { return };
        let so = self.scrolloff.min(shown.len().saturating_sub(1) / 2);
        let pos = self.cursor.pos;
        let target = if pos < first.start || (so > 0 && pos < shown[so].start) {
            Some(&shown[so.min(shown.len() - 1)])
        } else if pos >= last.end && !(last.contains(pos)) || so > 0 && pos >= shown[shown.len() - 1 - so].start {
            Some(&shown[shown.len() - 1 - so])
        } else {
            None
        };
        if let Some(row) = target
            && !row.contains(pos)
        {
            let want = self.cursor.want_col;
            self.cursor.pos = row.offset_at(row.prefix);
            self.cursor.want_col = want;
        }
    }

    /// Scroll so `offset` is shown, as `how` says.
    pub fn reveal(&mut self, text: &Text, offset: usize, how: Reveal) {
        let opts = self.layout.clone();
        let l = Layout { text, opts: &opts, width: self.text_width(text) };
        let target = l.row_start_of(offset.min(text.len_chars()));
        let height = self.height.max(1) as usize;
        let so = self.scrolloff.min((height - 1) / 2);
        // Rows from `start` back `n` (as far as there are).
        let back = |mut start: usize, n: usize| {
            for _ in 0..n {
                match l.prev_row(start) {
                    Some(s) => start = s,
                    None => break,
                }
            }
            start
        };
        match how {
            Reveal::Top => self.top = back(target, so),
            Reveal::Center => self.top = back(target, (height - 1) / 2),
            Reveal::Minimal => {
                if target < self.top || target < back(self.top, 0) {
                    self.top = back(target, so);
                } else {
                    // Rows from the top to the target, if within sight.
                    let mut start = self.top;
                    let mut n = 0;
                    while start < target && n < height {
                        match l.next_row(&l.row_at(start, usize::MAX)) {
                            Some(s) => start = s,
                            None => break,
                        }
                        n += 1;
                    }
                    if n < so && self.top > 0 {
                        self.top = back(target, so);
                    } else if n + so >= height {
                        self.top = back(target, height - 1 - so);
                    }
                }
            }
        }
        if !opts.wrap {
            let col = l.row_at(l.text.line_start(l.text.line_of(offset)), usize::MAX).col_of(offset);
            let width = l.width;
            if col < self.left {
                self.left = col;
            } else if col >= self.left + width {
                self.left = col + 1 - width;
            }
        }
    }

    /// The text shown: from the top row's start to the last row's end.
    pub fn visible_range(&self, text: &Text) -> (usize, usize) {
        let rows = self.rows(text);
        (self.top, rows.last().map_or(self.top, |r| r.end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::LineEnding;

    fn view(width: u16, height: u16) -> View {
        View { width, height, ..View::default() }
    }

    #[test]
    fn vertical_moves_keep_the_column_across_short_lines() {
        let buf = Buffer::new(Text::from("abcdef\nab\nabcdef"), LineEnding::Lf);
        let mut v = view(20, 5);
        v.set_cursor(buf.text(), 5, false);
        v.move_cursor(&buf, ViewUnit::Line, 1, false);
        assert_eq!(buf.text().position(v.cursor.pos).character, 2, "clamped to the short line");
        v.move_cursor(&buf, ViewUnit::Line, 1, false);
        assert_eq!(buf.text().position(v.cursor.pos).character, 5, "and back to the column");
    }

    #[test]
    fn display_lines_step_through_wrapped_rows() {
        let buf = Buffer::new(Text::from("abcdefgh\nxy"), LineEnding::Lf);
        let mut v = view(3, 5);
        v.set_cursor(buf.text(), 1, false);
        v.move_cursor(&buf, ViewUnit::DisplayLine, 1, false);
        assert_eq!(v.cursor.pos, 4);
        v.move_cursor(&buf, ViewUnit::DisplayLine, 2, false);
        assert_eq!(v.cursor.pos, 10, "into the next line, same column");
        v.move_cursor(&buf, ViewUnit::DisplayLine, -1, false);
        assert_eq!(v.cursor.pos, 7);
    }

    #[test]
    fn the_cursor_stays_in_sight_with_scrolloff() {
        let text: String = (0..50).map(|i| format!("{i}\n")).collect();
        let buf = Buffer::new(Text::from(text.as_str()), LineEnding::Lf);
        let mut v = View { scrolloff: 2, ..view(10, 10) };
        v.set_cursor(buf.text(), buf.text().line_start(20), false);
        assert_eq!(buf.text().line_of(v.top), 13, "line 20 two rows above the bottom");
        v.set_cursor(buf.text(), buf.text().line_start(16), false);
        assert_eq!(buf.text().line_of(v.top), 13, "within sight: no scroll");
        v.set_cursor(buf.text(), buf.text().line_start(14), false);
        assert_eq!(buf.text().line_of(v.top), 12, "two rows kept above");
        v.reveal(buf.text(), v.cursor.pos, Reveal::Center);
        assert_eq!(buf.text().line_of(v.top), 10);
        v.scroll(buf.text(), 10);
        assert_eq!(buf.text().line_of(v.top), 20);
        assert_eq!(buf.text().line_of(v.cursor.pos), 22, "pulled down, scrolloff kept");
    }

    #[test]
    fn selections_start_at_the_old_cursor() {
        let buf = Buffer::new(Text::from("hello world"), LineEnding::Lf);
        let mut v = view(20, 3);
        v.move_cursor(&buf, ViewUnit::Text(Unit::WordStart), 1, true);
        assert_eq!(v.cursor.selection(), Some((0, 6, SelectionKind::Char)));
        v.move_cursor(&buf, ViewUnit::Text(Unit::LineEnd), 1, false);
        assert_eq!(v.cursor.selection(), None);
    }

    #[test]
    fn without_wrapping_the_view_scrolls_sideways() {
        let buf = Buffer::new(Text::from(&*"x".repeat(100)), LineEnding::Lf);
        let mut v = View { layout: LayoutOptions { wrap: false, ..Default::default() }, ..view(10, 3) };
        v.set_cursor(buf.text(), 50, false);
        assert_eq!(v.left, 41);
        v.set_cursor(buf.text(), 3, false);
        assert_eq!(v.left, 3);
    }
}
