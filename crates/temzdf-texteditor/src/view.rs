//! A pane's view of a buffer: its cursor and selection, where it is
//! scrolled to, how it lays text out, and the motions that need all that
//! (lines keeping a column, display lines, pages).

use std::collections::HashMap;

use crate::buffer::Buffer;
use crate::cursor::{Cursor, SelectionKind};
use crate::edit::{Change, Gravity, map_offset};
use crate::layout::{Concealed, Inline, Layout, LayoutOptions, Row, VirtLine};
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

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub cursor: Cursor,
    /// Where the top row starts.
    pub top: usize,
    /// Display rows of the top row's group (its virtual lines above, the
    /// row, those below) scrolled past.
    pub top_skip: usize,
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
    /// The pane it is in: decorations shown only there count (inline
    /// virtual text takes columns).
    pub pane: Option<u32>,
    /// A cell's size in pixels (images take as many cells as their pixels
    /// fill).
    pub cell: (u32, u32),
    /// Highlight groups drawn as others in this view (Neovim's
    /// `winhighlight`): a group's name to the one used instead (`"None"`:
    /// nothing).
    pub highlights: HashMap<String, String>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            cursor: Cursor::default(),
            top: 0,
            top_skip: 0,
            left: 0,
            width: 80,
            height: 24,
            layout: LayoutOptions::default(),
            number: NumberMode::Off,
            cursorline: false,
            scrolloff: 0,
            pane: None,
            cell: (10, 20),
            highlights: HashMap::new(),
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

    /// Lays out `buf` as this view shows it (its inline virtual text too).
    pub fn layouter<'a>(&'a self, buf: &'a Buffer) -> Layout<'a> {
        self.layouter_with(buf, &self.layout)
    }

    /// [`layouter`](Self::layouter) with `opts` (a copy of the view's, so
    /// the view can change meanwhile).
    fn layouter_with<'a>(&self, buf: &'a Buffer, opts: &'a LayoutOptions) -> Layout<'a> {
        let text = buf.text();
        let width = self.text_width(text);
        let inline = Inline::of(buf.marks(), self.pane, self.cell, width);
        let concealed = Concealed::of(text, buf.marks(), self.pane, self.cell, width, text.line_of(self.cursor.pos));
        let virt_lines = VirtLine::of(text, buf.marks(), self.pane, self.cell, width);
        Layout { text, opts, width, inline, concealed, virt_lines }
    }

    /// The rows shown, top first (virtual lines among them).
    pub fn rows(&self, buf: &Buffer) -> Vec<Row> {
        let l = self.layouter(buf);
        let limit = self.left + l.width;
        let height = self.height as usize;
        let mut rows = Vec::with_capacity(height);
        let mut at = Some(self.top);
        let mut skip = self.top_skip;
        while let Some(start) = at
            && rows.len() < height
        {
            let row = l.row_at(start, limit);
            at = l.next_row(&row);
            for r in l.group(row) {
                if skip > 0 {
                    skip -= 1;
                } else if rows.len() < height {
                    rows.push(r);
                }
            }
        }
        rows
    }

    /// Follow edits: the cursor (an insertion at it leaves it after) and
    /// the top (kept at a row start).
    pub fn follow(&mut self, buf: &Buffer, changes: &[Change]) {
        self.cursor.follow(changes);
        let top = map_offset(self.top, changes, Gravity::Before).0;
        let l = self.layouter(buf);
        let top = l.row_start_of(top);
        let skip = self.top_skip.min(l.group_len(top) - 1);
        (self.top, self.top_skip) = (top, skip);
    }

    /// The layout changed (width, wrapping): the top must be a row start
    /// again.
    pub fn relayout(&mut self, buf: &Buffer) {
        let l = self.layouter(buf);
        let top = l.row_start_of(self.top.min(buf.text().len_chars()));
        let skip = self.top_skip.min(l.group_len(top) - 1);
        (self.top, self.top_skip) = (top, skip);
        self.reveal(buf, self.cursor.pos, Reveal::Minimal);
    }

    /// Where `count` units from `from` is, and the column vertical moves
    /// keep (`None` for other units).
    pub fn position_of(&self, buf: &Buffer, from: usize, unit: ViewUnit, count: i64) -> (usize, Option<usize>) {
        let text = buf.text();
        let l = self.layouter(buf);
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
        self.set_cursor(buf, to, select);
        self.cursor.want_col = want;
    }

    /// Put the cursor at `offset` (see [`move_cursor`](Self::move_cursor)).
    pub fn set_cursor(&mut self, buf: &Buffer, offset: usize, select: bool) {
        let offset = offset.min(buf.text().len_chars());
        if select {
            let anchor = self.cursor.anchor.unwrap_or((self.cursor.pos, SelectionKind::Char));
            self.cursor.anchor = Some(anchor);
        } else {
            self.cursor.anchor = None;
        }
        self.cursor.pos = offset;
        self.cursor.want_col = None;
        self.reveal(buf, offset, Reveal::Minimal);
    }

    /// Scroll by `rows` display rows (negative: up); the cursor stays in
    /// sight.
    pub fn scroll(&mut self, buf: &Buffer, rows: i64) {
        let opts = self.layout.clone();
        let l = self.layouter_with(buf, &opts);
        for _ in 0..rows.unsigned_abs() {
            let next = if rows < 0 { l.up((self.top, self.top_skip)) } else { l.down((self.top, self.top_skip)) };
            match next {
                Some(s) => (self.top, self.top_skip) = s,
                None => break,
            }
        }
        // The cursor into the text rows shown (keeping its column).
        let shown: Vec<Row> = self.rows(buf).into_iter().filter(|r| r.virt.is_none()).collect();
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
    pub fn reveal(&mut self, buf: &Buffer, offset: usize, how: Reveal) {
        let opts = self.layout.clone();
        let l = self.layouter_with(buf, &opts);
        let text = buf.text();
        // Display rows as (row start, rows into its group): the target's
        // row, and its group's first (the virtual lines above it).
        let row = l.row_of(offset.min(text.len_chars()));
        let (target, group) = ((row.start, l.above_len(&row)), (row.start, 0));
        let height = self.height.max(1) as usize;
        let so = self.scrolloff.min((height - 1) / 2);
        // Rows from `spot` back `n` (as far as there are).
        let back = |mut spot: (usize, usize), n: usize| {
            for _ in 0..n {
                match l.up(spot) {
                    Some(s) => spot = s,
                    None => break,
                }
            }
            spot
        };
        let top = (self.top, self.top_skip);
        let new = match how {
            Reveal::Top => back(target, so),
            Reveal::Center => back(target, (height - 1) / 2),
            // Up to it: its virtual lines above shown too.
            Reveal::Minimal if target < top => back(group, so),
            Reveal::Minimal => {
                // Rows from the top to the target, if within sight.
                let mut spot = top;
                let mut n = 0;
                while spot < target && n < height {
                    match l.down(spot) {
                        Some(s) => spot = s,
                        None => break,
                    }
                    n += 1;
                }
                if n < so && top > (0, 0) {
                    back(target, so)
                } else if n + so >= height {
                    back(target, height - 1 - so)
                } else {
                    top
                }
            }
        };
        (self.top, self.top_skip) = new;
        if !opts.wrap {
            let col = l.row_at(l.text.line_start(l.text.line_of(offset)), usize::MAX).col_of(offset);
            let width = l.width;
            if col < self.left {
                // Back to the start when the cursor fits there (a prompt's
                // inline virtual text before it shows again).
                self.left = if col < width { 0 } else { col };
            } else if col >= self.left + width {
                self.left = col + 1 - width;
            }
        }
    }

    /// The offset shown at cell (`col`, `row`) of the pane (gutter
    /// included): a row's end past its text, the last row's below the text.
    pub fn offset_at_cell(&self, buf: &Buffer, col: usize, row: usize) -> usize {
        let text = buf.text();
        let rows = self.rows(buf);
        let Some(r) = rows.get(row).or(rows.last()) else { return 0 };
        if row >= rows.len() {
            return r.end;
        }
        let col = (col + self.left).saturating_sub(self.gutter(text.line_count())).max(r.prefix);
        let at = r.offset_at(col);
        // Past a wrapped row's text: its last character, not the next row.
        if r.wrapped && at >= r.end { r.glyphs.last().map_or(r.start, |g| g.offset) } else { at }
    }

    /// The cell (`col`, `row`) of the pane (gutter included) where `offset`
    /// shows, as the cursor there would be drawn (a wide character's first
    /// cell, after inline virtual text before it, a concealed range's
    /// first cell); `None` when it is not in sight (scrolled away, beyond
    /// the edge without wrapping). The inverse of
    /// [`offset_at_cell`](Self::offset_at_cell).
    pub fn cell_of(&self, buf: &Buffer, offset: usize) -> Option<(usize, usize)> {
        let gutter = self.gutter(buf.text().line_count());
        let width = self.width as usize;
        let (y, r) = self.rows(buf).into_iter().enumerate().find(|(_, r)| r.virt.is_none() && r.contains(offset))?;
        let x = (r.col_of(offset) + gutter).checked_sub(self.left).filter(|x| *x >= gutter)?;
        // A line's end just past the last column: drawn in the last cell
        // (as the cursor is), unless the line goes on beyond the edge.
        let whole = r.glyphs.last().is_none_or(|g| g.offset + g.chars >= r.end);
        if x == width && offset == r.end && whole && width > gutter {
            return Some((width - 1, y));
        }
        (x < width).then_some((x, y))
    }

    /// The text shown: from the top row's start to the last row's end.
    pub fn visible_range(&self, buf: &Buffer) -> (usize, usize) {
        let rows = self.rows(buf);
        (self.top, rows.last().map_or(self.top, |r| r.end))
    }

    /// The buffer lines shown, first and last (0-based, inclusive): those
    /// of the top and bottom rows, a row of virtual lines counting as its
    /// line's and a wrapped line's rows as the line; past the text's end
    /// the last line.
    pub fn visible_lines(&self, buf: &Buffer) -> (usize, usize) {
        let rows = self.rows(buf);
        let top = buf.text().line_of(self.top);
        (rows.first().map_or(top, |r| r.line), rows.last().map_or(top, |r| r.line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marks::MarkSpec;
    use crate::text::LineEnding;

    fn view(width: u16, height: u16) -> View {
        View { width, height, ..View::default() }
    }

    #[test]
    fn vertical_moves_keep_the_column_across_short_lines() {
        let buf = Buffer::new(Text::from("abcdef\nab\nabcdef"), LineEnding::Lf);
        let mut v = view(20, 5);
        v.set_cursor(&buf, 5, false);
        v.move_cursor(&buf, ViewUnit::Line, 1, false);
        assert_eq!(buf.text().position(v.cursor.pos).character, 2, "clamped to the short line");
        v.move_cursor(&buf, ViewUnit::Line, 1, false);
        assert_eq!(buf.text().position(v.cursor.pos).character, 5, "and back to the column");
    }

    #[test]
    fn display_lines_step_through_wrapped_rows() {
        let buf = Buffer::new(Text::from("abcdefgh\nxy"), LineEnding::Lf);
        let mut v = view(3, 5);
        v.set_cursor(&buf, 1, false);
        v.move_cursor(&buf, ViewUnit::DisplayLine, 1, false);
        assert_eq!(v.cursor.pos, 4);
        v.move_cursor(&buf, ViewUnit::DisplayLine, 2, false);
        assert_eq!(v.cursor.pos, 10, "into the next line, same column");
        v.move_cursor(&buf, ViewUnit::DisplayLine, -1, false);
        assert_eq!(v.cursor.pos, 7);
    }

    #[test]
    fn visible_lines_count_wrapped_rows_as_their_line() {
        let buf = Buffer::new(Text::from("abcdefgh\nxy\nz\n"), LineEnding::Lf);
        let mut v = view(3, 2);
        assert_eq!(v.visible_lines(&buf), (0, 0), "two rows of line 0");
        v.scroll(&buf, 2);
        assert_eq!(v.visible_lines(&buf), (0, 1), "its last row, then line 1");
        v.height = 10;
        assert_eq!(v.visible_lines(&buf), (0, 3), "past the end: the last (empty) line");
    }

    #[test]
    fn the_cursor_stays_in_sight_with_scrolloff() {
        let text: String = (0..50).map(|i| format!("{i}\n")).collect();
        let buf = Buffer::new(Text::from(text.as_str()), LineEnding::Lf);
        let mut v = View { scrolloff: 2, ..view(10, 10) };
        v.set_cursor(&buf, buf.text().line_start(20), false);
        assert_eq!(buf.text().line_of(v.top), 13, "line 20 two rows above the bottom");
        v.set_cursor(&buf, buf.text().line_start(16), false);
        assert_eq!(buf.text().line_of(v.top), 13, "within sight: no scroll");
        v.set_cursor(&buf, buf.text().line_start(14), false);
        assert_eq!(buf.text().line_of(v.top), 12, "two rows kept above");
        v.reveal(&buf, v.cursor.pos, Reveal::Center);
        assert_eq!(buf.text().line_of(v.top), 10);
        v.scroll(&buf, 10);
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
    fn cells_map_to_offsets() {
        let buf = Buffer::new(
            Text::from(
                "abcdef
xy",
            ),
            LineEnding::Lf,
        );
        let v = View { number: NumberMode::Absolute, ..view(8, 4) };
        // A gutter of four columns, four of text: "abcd", "ef", "xy".
        assert_eq!(v.offset_at_cell(&buf, 5, 0), 1);
        assert_eq!(v.offset_at_cell(&buf, 0, 0), 0, "in the gutter: the row's start");
        assert_eq!(v.offset_at_cell(&buf, 7, 1), 6, "past the text: the line's end");
        assert_eq!(v.offset_at_cell(&buf, 9, 0), 3, "past a wrapped row: its last character");
        assert_eq!(v.offset_at_cell(&buf, 4, 3), 9, "below the text: its end");
    }

    /// Where the drawn cursor of `v` (focused) is with the cursor at
    /// `offset`, as `render` draws it.
    fn drawn_cursor(buf: &Buffer, v: &View, offset: usize) -> Option<(usize, usize)> {
        let mut v = v.clone();
        v.cursor = crate::cursor::Cursor::at(offset);
        let d = crate::render::render(buf, &v, &crate::render::Highlights::default(), v.pane, true, &[]);
        d.cursor.map(|(x, y)| (x as usize, y as usize))
    }

    /// `cell_of` agrees with the drawn cursor at every offset of `buf`
    /// (the cursor there: its line shows concealed text). Beyond the right
    /// edge without wrapping, the cursor is drawn in the last column, and
    /// the offset is not in sight.
    fn cells_agree_with_the_cursor(buf: &Buffer, v: &View) {
        for offset in 0..=buf.text().len_chars() {
            let at = View { cursor: crate::cursor::Cursor::at(offset), ..v.clone() };
            let (cell, drawn) = (at.cell_of(buf, offset), drawn_cursor(buf, v, offset));
            let clamped = !v.layout.wrap && cell.is_none() && drawn.is_some_and(|(x, _)| x + 1 == v.width as usize);
            assert!(cell == drawn || clamped, "offset {offset}: {cell:?}, drawn at {drawn:?} in {v:?}");
        }
    }

    #[test]
    fn offsets_map_to_the_cells_the_cursor_is_drawn_in() {
        use crate::marks::{Conceal, VirtChunk, VirtPos};
        let chunk = |t: &str| VirtChunk { text: t.into(), style: None };
        let mut buf = Buffer::new(Text::from("one\n\tあいx\n  wrapped long line here\nend"), LineEnding::Lf);
        buf.marks_mut().add(
            4,
            4,
            MarkSpec { virt_text: vec![chunk(">>")], virt_pos: VirtPos::Inline, ..Default::default() },
        );
        let at = buf.text().line_start(2) + 2;
        let conceal = Conceal { text: vec![chunk("W")], ..Default::default() };
        buf.marks_mut().add(at, at + 7, MarkSpec { conceal: Some(conceal), ..Default::default() });
        let mut v = View { number: NumberMode::Absolute, ..view(14, 6) };
        v.layout.tabstop = 4;
        v.layout.showbreak = "+".into();
        v.layout.breakindent = true;
        // The cursor away from the concealed line, which then shows "W".
        v.cursor = crate::cursor::Cursor::at(0);
        cells_agree_with_the_cursor(&buf, &v);
        // Spot checks: a gutter of four; "o" of "one"; after the tab and
        // the inline text; the wide character's first cell.
        assert_eq!(v.cell_of(&buf, 0), Some((4, 0)));
        assert_eq!(v.cell_of(&buf, 4), Some((6, 1)), "the tab, after the inline text before it");
        assert_eq!(v.cell_of(&buf, 5), Some((10, 1)), "after the tab (a tab stop's width)");
        assert_eq!(v.cell_of(&buf, 6), Some((12, 1)), "after あ, two columns wide");
        assert_eq!(
            v.cell_of(&buf, at + 3),
            Some((6, 3)),
            "in a concealed range: its first cell (line 1 took two rows)"
        );
        // Scrolled down: the first lines are not in sight.
        v.top = buf.text().line_start(2);
        assert_eq!(v.cell_of(&buf, 0), None);
        assert_eq!(v.cell_of(&buf, v.top), Some((4, 0)));
        cells_agree_with_the_cursor(&buf, &v);
        // Narrower, wrapping more: continuation rows after showbreak.
        let narrow = View { width: 9, top: 0, top_skip: 0, ..v.clone() };
        cells_agree_with_the_cursor(&buf, &narrow);
        // Without wrapping, scrolled sideways.
        let mut side = View { left: 3, ..v.clone() };
        side.top = 0;
        side.layout.wrap = false;
        cells_agree_with_the_cursor(&buf, &side);
        assert_eq!(side.cell_of(&buf, 0), None, "left of the edge");
        assert_eq!(side.cell_of(&buf, 3), Some((4, 0)), "the line's end of \"one\", at the first text column");
    }

    #[test]
    fn a_full_rows_end_shows_in_its_last_cell_but_not_past_the_edge() {
        let buf = Buffer::new(Text::from("abcd\nabcdefgh"), LineEnding::Lf);
        let v = view(4, 4);
        assert_eq!(v.cell_of(&buf, 4), Some((3, 0)), "the end of a line as wide as the pane");
        cells_agree_with_the_cursor(&buf, &v);
        let mut side = View { layout: LayoutOptions { wrap: false, ..Default::default() }, ..view(4, 4) };
        assert_eq!(side.cell_of(&buf, 5 + 3), Some((3, 1)));
        assert_eq!(side.cell_of(&buf, 5 + 4), None, "beyond the right edge");
        assert_eq!(side.cell_of(&buf, 5 + 8), None, "a line's end beyond the edge");
        side.left = 5;
        assert_eq!(side.cell_of(&buf, 5 + 8), Some((3, 1)));
    }

    #[test]
    fn cells_and_offsets_go_both_ways() {
        let buf = with_lines("one\ntwo words\nthree", &[(1, true, &["A"])]);
        let v = View { number: NumberMode::Absolute, ..view(10, 6) };
        for offset in 0..buf.text().len_chars() {
            let (x, y) = v.cell_of(&buf, offset).expect("in sight");
            assert_eq!(v.offset_at_cell(&buf, x, y), offset, "offset {offset} at ({x}, {y})");
        }
        assert_eq!(v.cell_of(&buf, 4), Some((4, 2)), "below the virtual line");
    }

    #[test]
    fn without_wrapping_the_view_scrolls_sideways() {
        let buf = Buffer::new(Text::from(&*"x".repeat(100)), LineEnding::Lf);
        let mut v = View { layout: LayoutOptions { wrap: false, ..Default::default() }, ..view(10, 3) };
        v.set_cursor(&buf, 50, false);
        assert_eq!(v.left, 41);
        v.set_cursor(&buf, 30, false);
        assert_eq!(v.left, 30);
        v.set_cursor(&buf, 3, false);
        assert_eq!(v.left, 0, "back to the start when it fits there");
    }

    /// `text` with virtual lines `(line, above, rows of text)`.
    fn with_lines(text: &str, lines: &[(usize, bool, &[&str])]) -> Buffer {
        use crate::marks::{VirtChunk, VirtLines};
        let mut buf = Buffer::new(Text::from(text), LineEnding::Lf);
        for &(line, above, rows) in lines {
            let at = buf.text().line_start(line);
            let text = rows.iter().map(|r| vec![VirtChunk { text: (*r).into(), style: None }]).collect();
            let spec = MarkSpec { virt_lines: Some(VirtLines { text, image: None, above }), ..Default::default() };
            buf.marks_mut().add(at, at, spec);
        }
        buf
    }

    /// The rows shown: text, or virtual lines in capitals.
    fn shown(v: &View, buf: &Buffer) -> Vec<String> {
        v.rows(buf)
            .iter()
            .map(|r| match &r.virt {
                Some(crate::layout::VirtRow::Text(g)) => g.iter().map(|g| g.0.as_str()).collect(),
                Some(crate::layout::VirtRow::Image { index, .. }) => format!("#{index}"),
                None => buf.text().slice(r.start, r.end).to_string(),
            })
            .collect()
    }

    #[test]
    fn virtual_lines_are_rows_the_cursor_skips() {
        let buf = with_lines("one\ntwo\nthree", &[(1, false, &["B1", "B2"]), (1, true, &["A"])]);
        let mut v = view(10, 10);
        assert_eq!(shown(&v, &buf), ["one", "A", "two", "B1", "B2", "three"]);
        // Down from one: two, then three (display lines too).
        v.move_cursor(&buf, ViewUnit::Line, 1, false);
        assert_eq!(buf.text().line_of(v.cursor.pos), 1);
        v.move_cursor(&buf, ViewUnit::DisplayLine, 1, false);
        assert_eq!(buf.text().line_of(v.cursor.pos), 2);
        // A click on one: its line's end (below) or start (above).
        assert_eq!(v.offset_at_cell(&buf, 1, 3), 7);
        assert_eq!(v.offset_at_cell(&buf, 1, 1), 4);
        // Below a wrapped line: after its last row; above: before its first.
        let buf = with_lines("abcdef", &[(0, false, &["B"]), (0, true, &["A"])]);
        assert_eq!(shown(&view(3, 10), &buf), ["A", "abc", "def", "B"]);
    }

    #[test]
    fn scrolling_goes_through_virtual_lines_a_row_at_a_time() {
        let buf = with_lines("one\ntwo\nthree\nfour", &[(0, false, &["B1", "B2"]), (0, true, &["A"])]);
        let mut v = view(10, 3);
        assert_eq!(shown(&v, &buf), ["A", "one", "B1"]);
        v.scroll(&buf, 2);
        assert_eq!(shown(&v, &buf), ["B1", "B2", "two"]);
        assert_eq!(buf.text().line_of(v.cursor.pos), 1, "the cursor into the text shown");
        v.scroll(&buf, -1);
        assert_eq!(shown(&v, &buf), ["one", "B1", "B2"]);
        // Up to the first line: the lines above it shown too.
        v.set_cursor(&buf, buf.text().line_start(3), false);
        assert_eq!(shown(&v, &buf), ["two", "three", "four"]);
        v.set_cursor(&buf, 0, false);
        assert_eq!(shown(&v, &buf), ["A", "one", "B1"]);
        // Down to a line: its row at the bottom.
        v.set_cursor(&buf, buf.text().line_start(2), false);
        assert_eq!(shown(&v, &buf), ["B2", "two", "three"]);
    }
}
