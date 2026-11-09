//! Laying text out in display rows: widths, tabs, control characters,
//! wrapping (`linebreak`, `breakindent`, `showbreak`). Wrapping is greedy,
//! so a row depends only on where it starts: a view keeps the offset its top
//! row starts at and lays out from there, only what it shows (a 1 MB line
//! costs what is visible, plus the way back to its start when scrolling up
//! inside it).

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::marks::{Marks, VirtChunk, VirtImage, VirtPos};
use crate::text::Text;

/// How a pane lays text out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutOptions {
    pub wrap: bool,
    /// Wrap after a blank rather than anywhere.
    pub linebreak: bool,
    /// Continuation rows start at the line's indent.
    pub breakindent: bool,
    /// Shown at the start of continuation rows.
    pub showbreak: String,
    /// Display columns between tab stops (a buffer's).
    pub tabstop: usize,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self { wrap: true, linebreak: false, breakindent: false, showbreak: String::new(), tabstop: 8 }
    }
}

/// One grapheme (or a tab, or a control character) as shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub offset: usize,
    /// Characters of the text it shows.
    pub chars: usize,
    /// Its first display column in the row, prefix included.
    pub col: usize,
    pub width: usize,
    pub kind: GlyphKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlyphKind {
    Text(String),
    /// Blank up to the next tab stop.
    Tab,
    /// `^X` for a control character.
    Control(char),
    /// A grapheme of inline virtual text (no characters of the text: its
    /// `offset` is where it shows, before the character there).
    Virtual {
        text: String,
        style: Option<String>,
    },
    /// An inline image, `width` columns and one row (no characters either),
    /// and what is shown where images cannot be.
    Image {
        id: u64,
        alt: String,
    },
    /// A concealed range shown as nothing: no columns, its characters.
    Concealed,
}

/// Inline virtual text at `offset`, as layout places it: its graphemes, each
/// with its width and style.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inline {
    pub offset: usize,
    pub graphemes: Vec<(String, usize, Option<String>)>,
    /// An image after them: its id, columns and text instead.
    pub image: Option<(u64, usize, String)>,
}

/// Columns an image `w` x `h` pixels takes one row high, cells being
/// `cell` pixels: as wide as its aspect asks, at least one, at most `max`.
pub fn image_cols(w: u32, h: u32, cell: (u32, u32), max: usize) -> usize {
    if h == 0 || cell.0 == 0 {
        return 1;
    }
    let cols = (u64::from(w) * u64::from(cell.1)).div_ceil(u64::from(h) * u64::from(cell.0)) as usize;
    cols.clamp(1, max.max(1))
}

impl Inline {
    fn width(&self) -> usize {
        self.graphemes.iter().map(|g| g.1).sum::<usize>() + self.image.as_ref().map_or(0, |i| i.1)
    }

    /// The inline virtual text (and images) of `marks` shown in `pane`, by
    /// offset (then priority, then age); images in `cell`-pixel cells, at
    /// most `width` columns.
    pub fn of(marks: &Marks, pane: Option<u32>, cell: (u32, u32), width: usize) -> Vec<Inline> {
        let mut v: Vec<_> = marks
            .iter()
            .filter(|m| {
                m.spec.virt_pos == VirtPos::Inline
                    && (!m.spec.virt_text.is_empty() || m.spec.virt_image.is_some())
                    && !m.deleted
                    && m.spec.pane.is_none_or(|p| Some(p) == pane)
            })
            .collect();
        v.sort_by_key(|m| (m.start, m.spec.priority, m.id));
        v.into_iter()
            .map(|m| Inline::pieces(m.start, &m.spec.virt_text, m.spec.virt_image.as_ref(), cell, width))
            .filter(|i| !i.graphemes.is_empty() || i.image.is_some())
            .collect()
    }

    /// Text and an image at `offset`, as layout places them.
    fn pieces(offset: usize, chunks: &[VirtChunk], img: Option<&VirtImage>, cell: (u32, u32), width: usize) -> Inline {
        let text = |t: &str, style: &Option<String>| -> Vec<(String, usize, Option<String>)> {
            t.graphemes(true)
                .filter(|g| !g.chars().any(control))
                .map(|g| (g.to_owned(), grapheme_width(g), style.clone()))
                .collect()
        };
        let mut graphemes: Vec<_> = chunks.iter().flat_map(|c| text(&c.text, &c.style)).collect();
        let mut image = None;
        if let Some(img) = img {
            if img.missing {
                // Gone: its text instead.
                graphemes.extend(text(&img.alt, &None));
            } else {
                let max = img.max_cols.unwrap_or(usize::MAX).min(width);
                image = Some((img.id, image_cols(img.width, img.height, cell, max), img.alt.clone()));
            }
        }
        Inline { offset, graphemes, image }
    }
}

/// A range shown as something else (conceal): its text is skipped, `with`
/// shown at its start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Concealed {
    pub start: usize,
    pub end: usize,
    pub with: Inline,
}

impl Concealed {
    /// The ranges of `marks` concealed in `pane` (within one line, not on
    /// `cursor_line` unless they say so), by start.
    pub fn of(
        text: &Text,
        marks: &Marks,
        pane: Option<u32>,
        cell: (u32, u32),
        width: usize,
        cursor_line: usize,
    ) -> Vec<Concealed> {
        let mut v: Vec<_> = marks
            .iter()
            .filter(|m| {
                m.start < m.end
                    && !m.deleted
                    && m.spec.pane.is_none_or(|p| Some(p) == pane)
                    && text.line_of(m.start) == text.line_of(m.end.saturating_sub(1))
                    && m.end <= text.line_end(text.line_of(m.start))
            })
            .filter_map(|m| {
                let c = m.spec.conceal.as_ref()?;
                (c.cursor_line || text.line_of(m.start) != cursor_line).then_some((m, c))
            })
            .collect();
        // Of overlapping ones layout shows the first (one inside another
        // is never reached).
        v.sort_by_key(|(m, _)| (m.start, -m.spec.priority, m.id));
        v.into_iter()
            .map(|(m, c)| Concealed {
                start: m.start,
                end: m.end,
                with: Inline::pieces(m.start, &c.text, c.image.as_ref(), cell, width),
            })
            .collect()
    }
}

/// A display row: part of one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    /// Where it starts and ends (the end is the next row's start, or the
    /// line's end).
    pub start: usize,
    pub end: usize,
    /// The first row of its line.
    pub first: bool,
    /// Columns taken before the text (`showbreak`, `breakindent`).
    pub prefix: usize,
    pub glyphs: Vec<Glyph>,
    /// The line goes on in the next row.
    pub wrapped: bool,
}

impl Row {
    /// The display column of `offset` in this row (its end for offsets past
    /// it).
    pub fn col_of(&self, offset: usize) -> usize {
        self.glyphs
            .iter()
            .find(|g| g.offset + g.chars > offset)
            .map_or_else(|| self.glyphs.last().map_or(self.prefix, |g| g.col + g.width), |g| g.col)
    }

    /// The offset shown at display column `col` (the row's end past its
    /// text; a wide glyph's second column is the glyph).
    pub fn offset_at(&self, col: usize) -> usize {
        self.glyphs.iter().find(|g| col < g.col + g.width).map_or(self.end, |g| g.offset)
    }

    /// Whether the cursor at `offset` shows in this row (a line's end shows
    /// in its last row).
    pub fn contains(&self, offset: usize) -> bool {
        self.start <= offset && offset < self.end || offset == self.end && !self.wrapped
    }
}

/// The display width of a grapheme: as the GUI and terminals draw it, 1 or
/// 2.
pub fn grapheme_width(g: &str) -> usize {
    UnicodeWidthStr::width(g).clamp(1, 2)
}

fn control(c: char) -> bool {
    (c as u32) < 0x20 || c == '\x7f'
}

/// Lays out rows of one text at one width.
pub struct Layout<'a> {
    pub text: &'a Text,
    pub opts: &'a LayoutOptions,
    /// Columns for text (the pane minus the gutter).
    pub width: usize,
    /// Inline virtual text, by offset ([`Inline::of`]).
    pub inline: Vec<Inline>,
    /// Concealed ranges, by start ([`Concealed::of`]).
    pub concealed: Vec<Concealed>,
}

/// How far ahead a row is read at once (more if it needs it).
const CHUNK: usize = 1024;

impl Layout<'_> {
    /// The row starting at `start` (a row start: a line's start or where
    /// the row before it ended). Without wrapping, at most `limit` columns
    /// of it.
    pub fn row_at(&self, start: usize, limit: usize) -> Row {
        let text = self.text;
        let line = text.line_of(start);
        let (line_start, line_end) = (text.line_start(line), text.line_end(line));
        let first = start <= line_start;
        let start = start.max(line_start).min(line_end);
        let prefix = if first || !self.opts.wrap { 0 } else { self.continuation_prefix(line_start, line_end) };
        let width = if self.opts.wrap { self.width.max(prefix + 2) } else { usize::MAX };
        let mut row = Row { line, start, end: line_end, first, prefix, glyphs: Vec::new(), wrapped: false };
        let mut col = prefix;
        let mut at = start;
        // Inline virtual text goes with the character after it (a row break
        // never comes between them), so a row still depends only on its
        // start: this row shows that at offsets from `start` on.
        let mut vi = self.inline.partition_point(|v| v.offset < start);
        // Where a `linebreak` row may end: after the last blank.
        let mut last_blank: Option<usize> = None;
        while at < line_end {
            let to = (at + CHUNK).min(line_end);
            let chunk = text.slice(at, to);
            let mut graphemes = chunk.graphemes(true).peekable();
            let mut consumed = 0;
            while let Some(g) = graphemes.next() {
                // A concealed range here: what it is shown as (after the
                // virtual text before it, one unit with it), its text
                // skipped; read on after it.
                let ci = self.concealed.partition_point(|c| c.start < at);
                if let Some(c) = self.concealed.get(ci).filter(|c| c.start == at) {
                    let vj = vi + self.inline[vi..].iter().take_while(|v| v.offset == at).count();
                    let vw: usize = self.inline[vi..vj].iter().map(Inline::width).sum();
                    if self.opts.wrap && col + vw + c.with.width() > width && col > prefix {
                        row.wrapped = true;
                        row.end = at;
                        return row;
                    }
                    if !self.opts.wrap && col >= limit {
                        return row;
                    }
                    let room = |col: usize| if self.opts.wrap { width.saturating_sub(col) } else { usize::MAX };
                    let left = room(col);
                    self.place_inline(&mut row, &mut col, &self.inline[vi..vj], at, left);
                    let first = row.glyphs.len();
                    let left = room(col);
                    self.place_inline(&mut row, &mut col, std::slice::from_ref(&c.with), at, left);
                    // Offsets in the range show where it is.
                    let chars = c.end - c.start;
                    match row.glyphs.get_mut(first) {
                        Some(g) => g.chars = chars,
                        None => row.glyphs.push(Glyph { offset: at, chars, col, width: 0, kind: GlyphKind::Concealed }),
                    }
                    at = c.end;
                    consumed += chars;
                    vi += self.inline[vi..].iter().take_while(|v| v.offset < at).count();
                    break;
                }
                // A cluster cut by the chunk's end: read on from its start.
                if graphemes.peek().is_none() && to < line_end && consumed > 0 {
                    break;
                }
                let chars = g.chars().count();
                let c = g.chars().next().unwrap_or(' ');
                let (kind, w) = if c == '\t' {
                    let ts = self.opts.tabstop.max(1);
                    (GlyphKind::Tab, ts - (col - prefix) % ts)
                } else if control(c) {
                    (GlyphKind::Control(c), 2)
                } else {
                    (GlyphKind::Text(g.to_owned()), grapheme_width(g))
                };
                // The virtual text before this character, and its width.
                let vj = vi + self.inline[vi..].iter().take_while(|v| v.offset == at).count();
                let vw: usize = self.inline[vi..vj].iter().map(Inline::width).sum();
                if self.opts.wrap && col + vw + w > width && col > prefix {
                    row.wrapped = true;
                    row.end = at;
                    if self.opts.linebreak
                        && let Some(i) = last_blank
                    {
                        row.end = row.glyphs[i].offset + row.glyphs[i].chars;
                        row.glyphs.truncate(i + 1);
                    }
                    return row;
                }
                if !self.opts.wrap && col >= limit {
                    return row;
                }
                // In a row of its own it may not fit: cut, the character
                // still shown.
                let room = if self.opts.wrap { width.saturating_sub(col + w) } else { usize::MAX };
                self.place_inline(&mut row, &mut col, &self.inline[vi..vj], at, room);
                vi = vj;
                if !self.opts.wrap && col >= limit {
                    return row;
                }
                if matches!(kind, GlyphKind::Tab) || c == ' ' {
                    last_blank = Some(row.glyphs.len());
                }
                row.glyphs.push(Glyph { offset: at, chars, col, width: w, kind });
                col += w;
                at += chars;
                consumed += chars;
            }
            if consumed == 0 {
                // One cluster longer than a chunk: take the chunk as it is.
                at = to;
            }
        }
        // Virtual text at the line's end (a prompt on an empty line).
        let vj = vi + self.inline[vi..].iter().take_while(|v| v.offset == line_end).count();
        if vj > vi && at == line_end {
            let vw: usize = self.inline[vi..vj].iter().map(Inline::width).sum();
            if self.opts.wrap && col + vw > width && col > prefix {
                // On a row of its own after this one (same start and end).
                row.wrapped = true;
                row.end = line_end;
                return row;
            }
            let room = if self.opts.wrap { width.saturating_sub(col) } else { usize::MAX };
            self.place_inline(&mut row, &mut col, &self.inline[vi..vj], line_end, room);
        }
        row
    }

    /// Put inline virtual text at `offset` into `row`, at most `room`
    /// columns of it.
    fn place_inline(&self, row: &mut Row, col: &mut usize, inline: &[Inline], offset: usize, room: usize) {
        let mut left = room;
        for v in inline {
            for (g, w, style) in &v.graphemes {
                if *w > left {
                    return;
                }
                row.glyphs.push(Glyph {
                    offset,
                    chars: 0,
                    col: *col,
                    width: *w,
                    kind: GlyphKind::Virtual { text: g.clone(), style: style.clone() },
                });
                *col += w;
                left -= w;
            }
            // An image is not cut: shown whole or not at all.
            if let Some((id, w, alt)) = &v.image {
                let w = *w;
                if w > left {
                    return;
                }
                let kind = GlyphKind::Image { id: *id, alt: alt.clone() };
                row.glyphs.push(Glyph { offset, chars: 0, col: *col, width: w, kind });
                *col += w;
                left -= w;
            }
        }
    }

    fn continuation_prefix(&self, line_start: usize, line_end: usize) -> usize {
        let show = self.opts.showbreak.graphemes(true).map(grapheme_width).sum::<usize>();
        let indent = if self.opts.breakindent {
            let mut cols = 0;
            for o in line_start..line_end {
                match self.text.char_at(o) {
                    Some(' ') => cols += 1,
                    Some('\t') => cols += self.opts.tabstop.max(1) - cols % self.opts.tabstop.max(1),
                    _ => break,
                }
            }
            cols
        } else {
            0
        };
        // Never so much that no text fits.
        (show + indent).min(self.width.saturating_sub(2))
    }

    /// The start of the row after `row`, if any.
    pub fn next_row(&self, row: &Row) -> Option<usize> {
        if row.wrapped {
            Some(row.end)
        } else if row.line + 1 < self.text.line_count() {
            Some(self.text.line_start(row.line + 1))
        } else {
            None
        }
    }

    /// The start of the row `offset` is in (laid out from its line's start).
    pub fn row_start_of(&self, offset: usize) -> usize {
        self.row_of(offset).start
    }

    /// The row `offset` is in.
    pub fn row_of(&self, offset: usize) -> Row {
        let line = self.text.line_of(offset);
        let mut row = self.row_at(self.text.line_start(line), usize::MAX);
        while row.wrapped && offset >= row.end {
            row = self.row_at(row.end, usize::MAX);
        }
        row
    }

    /// The start of the row before the one starting at `start`.
    pub fn prev_row(&self, start: usize) -> Option<usize> {
        let line = self.text.line_of(start);
        let line_start = self.text.line_start(line);
        if start > line_start {
            // Laid out from the line's start to the row before.
            let mut row = self.row_at(line_start, usize::MAX);
            let mut prev = row.start;
            while row.wrapped && row.end < start {
                prev = row.end;
                row = self.row_at(row.end, usize::MAX);
            }
            return Some(if row.end >= start { row.start } else { prev });
        }
        if line == 0 {
            return None;
        }
        // The last row of the line before.
        Some(self.row_start_of(self.text.line_end(line - 1)))
    }

    /// The display column of `offset` from its line's start, across rows
    /// (what `j` and `k` keep).
    pub fn line_col(&self, offset: usize) -> usize {
        let line = self.text.line_of(offset);
        let mut row = self.row_at(self.text.line_start(line), usize::MAX);
        let mut before = 0;
        while row.wrapped && offset >= row.end {
            before += row.glyphs.last().map_or(0, |g| g.col + g.width) - row.prefix;
            row = self.row_at(row.end, usize::MAX);
        }
        before + row.col_of(offset) - row.prefix
    }

    /// The offset at display column `col` of `line`, counted across rows
    /// (the line's end past its text).
    pub fn offset_at_line_col(&self, line: usize, col: usize) -> usize {
        let mut row = self.row_at(self.text.line_start(line), usize::MAX);
        let mut left = col;
        loop {
            let span = row.glyphs.last().map_or(0, |g| g.col + g.width) - row.prefix;
            if !row.wrapped || left < span {
                return row.offset_at(row.prefix + left);
            }
            left -= span;
            row = self.row_at(row.end, usize::MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str, width: usize, opts: LayoutOptions) -> Vec<String> {
        rows_with(text, width, opts, &[])
    }

    /// Inline virtual text `(offset, text)`.
    fn inline(v: &[(usize, &str)]) -> Vec<Inline> {
        v.iter()
            .map(|(offset, text)| Inline {
                offset: *offset,
                graphemes: text.graphemes(true).map(|g| (g.to_owned(), grapheme_width(g), None)).collect(),
                image: None,
            })
            .collect()
    }

    fn rows_with(text: &str, width: usize, opts: LayoutOptions, virt: &[(usize, &str)]) -> Vec<String> {
        let t = Text::from(text);
        let l = Layout { text: &t, opts: &opts, width, inline: inline(virt), concealed: Vec::new() };
        show(&l)
    }

    /// Ranges `(start, end, shown as, cursor_line)` concealed, the cursor
    /// on line `cursor`.
    fn concealed(t: &Text, v: &[(usize, usize, &str, bool)], cursor: usize) -> Vec<Concealed> {
        let mut marks = Marks::default();
        for &(start, end, with, cursor_line) in v {
            let text = if with.is_empty() { Vec::new() } else { vec![VirtChunk { text: with.into(), style: None }] };
            let conceal = Some(crate::marks::Conceal { text, image: None, cursor_line });
            marks.add(start, end, crate::marks::MarkSpec { conceal, ..Default::default() });
        }
        Concealed::of(t, &marks, None, (10, 20), 80, cursor)
    }

    fn rows_concealed(text: &str, width: usize, v: &[(usize, usize, &str, bool)], cursor: usize) -> Vec<String> {
        let t = Text::from(text);
        let opts = LayoutOptions::default();
        let l = Layout { text: &t, opts: &opts, width, inline: Vec::new(), concealed: concealed(&t, v, cursor) };
        show(&l)
    }

    fn show(l: &Layout) -> Vec<String> {
        let mut out = Vec::new();
        let mut at = Some(0);
        while let Some(start) = at {
            let r = l.row_at(start, usize::MAX);
            let mut s = " ".repeat(r.prefix);
            for g in &r.glyphs {
                match &g.kind {
                    GlyphKind::Text(t) => s.push_str(t),
                    GlyphKind::Tab => s.push_str(&">".repeat(g.width)),
                    GlyphKind::Control(c) => s.push_str(&format!("^{}", ((*c as u8) ^ 0x40) as char)),
                    // Virtual text is written in capitals in these tests.
                    GlyphKind::Virtual { text, .. } => s.push_str(text),
                    // Images are written as # per column in these tests.
                    GlyphKind::Image { .. } => s.push_str(&"#".repeat(g.width)),
                    GlyphKind::Concealed => {}
                }
            }
            out.push(s);
            at = l.next_row(&r);
        }
        out
    }

    #[test]
    fn rows_wrap_by_width_and_wide_characters_do_not_split() {
        assert_eq!(rows("abcdefg\nhi", 3, LayoutOptions::default()), ["abc", "def", "g", "hi"]);
        // あいう are two columns each: a row of 5 holds two.
        assert_eq!(rows("あいう", 5, LayoutOptions::default()), ["あい", "う"]);
        assert_eq!(rows("a\tb\x01", 20, LayoutOptions { tabstop: 4, ..Default::default() }), ["a>>>b^A"]);
    }

    #[test]
    fn linebreak_breakindent_and_showbreak() {
        let opts = LayoutOptions { linebreak: true, ..Default::default() };
        assert_eq!(rows("aa bb cc", 6, opts), ["aa bb ", "cc"]);
        let opts = LayoutOptions { breakindent: true, showbreak: ">".into(), ..Default::default() };
        assert_eq!(rows("  abcdefg", 5, opts), ["  abc", "   de", "   fg"]);
    }

    #[test]
    fn rows_are_found_from_offsets_and_columns_kept_across_them() {
        let t = Text::from("abcdefgh\nxy");
        let opts = LayoutOptions::default();
        let l = Layout { text: &t, opts: &opts, width: 3, inline: Vec::new(), concealed: Vec::new() };
        assert_eq!(l.row_start_of(4), 3);
        assert_eq!(l.prev_row(6), Some(3));
        assert_eq!(l.prev_row(9), Some(6), "the last row of the line before");
        assert_eq!(l.prev_row(0), None);
        assert_eq!(l.line_col(7), 7);
        assert_eq!(l.offset_at_line_col(0, 7), 7);
        assert_eq!(l.offset_at_line_col(1, 7), 11, "past a short line's end");
    }

    #[test]
    fn a_long_line_is_laid_out_only_as_far_as_shown() {
        let long = "x".repeat(1 << 20);
        let t = Text::from(long.as_str());
        let opts = LayoutOptions { wrap: false, ..Default::default() };
        let l = Layout { text: &t, opts: &opts, width: 80, inline: Vec::new(), concealed: Vec::new() };
        let r = l.row_at(0, 80);
        assert_eq!(r.glyphs.len(), 80);
    }

    #[test]
    fn inline_virtual_text_takes_columns_before_its_character() {
        let nowrap = || LayoutOptions { wrap: false, ..Default::default() };
        assert_eq!(rows_with("abc", 80, nowrap(), &[(1, "XY")]), ["aXYbc"]);
        // On an empty line, and at a line's end.
        assert_eq!(rows_with("", 80, nowrap(), &[(0, "> ")]), ["> "]);
        assert_eq!(rows_with("ab\ncd", 80, nowrap(), &[(2, "!")]), ["ab!", "cd"]);
        // The cursor goes after it: columns of offsets skip it.
        let t = Text::from("abc");
        let opts = nowrap();
        let l =
            Layout { text: &t, opts: &opts, width: 80, inline: inline(&[(0, "> "), (1, "XY")]), concealed: Vec::new() };
        let r = l.row_at(0, usize::MAX);
        assert_eq!((r.col_of(0), r.col_of(1), r.col_of(3)), (2, 5, 7));
        assert_eq!(r.offset_at(0), 0, "a click on it: where it is");
        assert_eq!(r.offset_at(3), 1);
        assert_eq!(l.line_col(2), 6, "vertical moves count it");
    }

    #[test]
    fn inline_virtual_text_wraps_with_the_character_after_it() {
        let wrap = LayoutOptions::default;
        // Never alone at a row's end: with the character it is before.
        assert_eq!(rows_with("abcd", 4, wrap(), &[(3, "XY")]), ["abc", "XYd"]);
        assert_eq!(rows_with("abcd", 4, wrap(), &[(2, "X")]), ["abXc", "d"]);
        // At the line's end: on a row of its own when it does not fit.
        assert_eq!(rows_with("abcd", 4, wrap(), &[(4, "XY")]), ["abcd", "XY"]);
        // Too wide for any row: cut, the character still shown.
        assert_eq!(rows_with("ab", 4, wrap(), &[(0, "XXXXXX")]), ["XXXa", "b"]);
        // linebreak: a row ending at a blank keeps what follows together.
        let lb = LayoutOptions { linebreak: true, ..Default::default() };
        assert_eq!(rows_with("aa bb", 5, lb, &[(3, "X")]), ["aa ", "Xbb"]);
    }

    #[test]
    fn rows_with_inline_virtual_text_are_found_from_offsets() {
        let t = Text::from("abcdef");
        let opts = LayoutOptions::default();
        let l = Layout { text: &t, opts: &opts, width: 4, inline: inline(&[(2, "XY")]), concealed: Vec::new() };
        // "abXY", "cdef".
        assert_eq!(l.row_start_of(1), 0);
        assert_eq!(l.row_start_of(2), 2, "it went down with its character");
        assert_eq!(l.prev_row(2), Some(0));
        assert_eq!(l.offset_at_line_col(0, 4), 2);
    }

    #[test]
    fn images_take_the_columns_their_aspect_asks_one_row_high() {
        // 10x20-pixel cells: a 40x20 image is two rows' worth wide -> 4
        // columns one row high; a square one 2 columns.
        assert_eq!(image_cols(40, 20, (10, 20), 80), 4);
        assert_eq!(image_cols(20, 20, (10, 20), 80), 2);
        assert_eq!(image_cols(21, 20, (10, 20), 80), 3, "rounded up");
        assert_eq!(image_cols(400, 20, (10, 20), 6), 6, "at most the limit");
        assert_eq!(image_cols(1, 100, (10, 20), 80), 1, "at least one");
        // Other cells, other columns.
        assert_eq!(image_cols(40, 20, (8, 16), 80), 4);
        assert_eq!(image_cols(40, 20, (16, 16), 80), 2);
        // In a row: whole, with the character after it.
        let t = Text::from("ab");
        let opts = LayoutOptions::default();
        let img = Inline { offset: 1, graphemes: Vec::new(), image: Some((7, 3, "x".into())) };
        let l = Layout { text: &t, opts: &opts, width: 5, inline: vec![img.clone()], concealed: Vec::new() };
        let r = l.row_at(0, usize::MAX);
        assert_eq!(r.glyphs.iter().map(|g| g.width).collect::<Vec<_>>(), [1, 3, 1]);
        assert_eq!(r.col_of(1), 4, "the cursor after it");
        let l = Layout { text: &t, opts: &opts, width: 4, inline: vec![img], concealed: Vec::new() };
        assert_eq!(l.row_at(0, usize::MAX).end, 1, "wrapped before it, with the b");
    }

    #[test]
    fn concealed_ranges_show_what_replaces_them() {
        // \alpha as α, or as nothing; the cursor on another line.
        assert_eq!(rows_concealed("a\\alpha b\nx", 80, &[(1, 7, "A", false)], 1), ["aA b", "x"]);
        assert_eq!(rows_concealed("abcd", 80, &[(1, 3, "", false)], 1), ["ad"]);
        // On the cursor's line shown as it is, unless it says so.
        assert_eq!(rows_concealed("abcd", 80, &[(1, 3, "X", false)], 0), ["abcd"]);
        assert_eq!(rows_concealed("abcd", 80, &[(1, 3, "X", true)], 0), ["aXd"]);
        // Overlapping: the first one; across lines: none.
        assert_eq!(rows_concealed("abcd", 80, &[(0, 2, "X", true), (1, 3, "Y", true)], 0), ["Xcd"]);
        assert_eq!(rows_concealed("abcd", 80, &[(1, 2, "Y", true), (0, 3, "X", true)], 0), ["Xd"]);
        assert_eq!(rows_concealed("ab\ncd", 80, &[(1, 4, "X", true)], 0), ["ab", "cd"]);
        // Wrapped as one unit: "abcd" with bc as XYZ at width 4.
        assert_eq!(rows_concealed("abcde", 4, &[(1, 3, "XYZ", true)], 0), ["aXYZ", "de"]);
        assert_eq!(rows_concealed("abcde", 4, &[(2, 4, "XYZ", true)], 0), ["ab", "XYZe"]);
        // Offsets in the range are where its replacement is.
        let t = Text::from("abcde");
        let opts = LayoutOptions::default();
        let l = Layout {
            text: &t,
            opts: &opts,
            width: 80,
            inline: Vec::new(),
            concealed: concealed(&t, &[(1, 4, "X", true)], 0),
        };
        let r = l.row_at(0, usize::MAX);
        assert_eq!((r.col_of(1), r.col_of(2), r.col_of(4)), (1, 1, 2));
        assert_eq!(r.offset_at(1), 1);
        assert_eq!(r.offset_at(2), 4);
        // Shown as nothing: the cursor in it is where it was.
        let l = Layout {
            text: &t,
            opts: &opts,
            width: 80,
            inline: Vec::new(),
            concealed: concealed(&t, &[(1, 4, "", true)], 0),
        };
        let r = l.row_at(0, usize::MAX);
        assert_eq!((r.col_of(2), r.col_of(4)), (1, 1));
        assert_eq!(r.offset_at(1), 4);
    }
}
