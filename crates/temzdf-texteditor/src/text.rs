//! The text, and positions in it as scripts see them.

use ropey::Rope;

/// A position as scripts see it: a zero-based line and a column in UTF-16
/// code units within it (JavaScript's string indices, LSP's default),
/// excluding the line ending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub line: usize,
    pub character: usize,
}

impl Position {
    pub fn new(line: usize, character: usize) -> Self {
        Self { line, character }
    }
}

/// A half-open range of positions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// The line ending a document is written with (inside it is always LF).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
}

/// The text of a buffer: a rope whose only line break is LF.
#[derive(Clone, Debug, Default)]
pub struct Text {
    rope: Rope,
}

impl Text {
    /// Text as read from a file: CRLF becomes LF, and the file's ending is
    /// the one most of its lines use (mixed files are normalised). A lone CR
    /// stays (it shows as `^M`).
    pub fn from_file_text(s: &str) -> (Self, LineEnding) {
        let crlf = s.matches("\r\n").count();
        let lf = s.matches('\n').count() - crlf;
        let ending = if crlf > lf { LineEnding::CrLf } else { LineEnding::Lf };
        let text = if crlf > 0 { s.replace("\r\n", "\n") } else { s.to_owned() };
        (Self::from(text.as_str()), ending)
    }

    /// The text to write, with `ending`.
    pub fn to_file_text(&self, ending: LineEnding) -> String {
        let s = self.rope.to_string();
        match ending {
            LineEnding::Lf => s,
            LineEnding::CrLf => s.replace('\n', "\r\n"),
        }
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub(crate) fn rope_mut(&mut self) -> &mut Rope {
        &mut self.rope
    }

    /// Characters (not bytes, not UTF-16 units).
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Lines; text ending with a line break has an empty last line (as VS
    /// Code shows it).
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// The offset where `line` starts (clamped to the last line).
    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.line_count() - 1))
    }

    /// The offset where `line`'s text ends, before its line break.
    pub fn line_end(&self, line: usize) -> usize {
        let line = line.min(self.line_count() - 1);
        if line + 1 < self.line_count() { self.rope.line_to_char(line + 1) - 1 } else { self.len_chars() }
    }

    /// The line `offset` is on.
    pub fn line_of(&self, offset: usize) -> usize {
        self.rope.char_to_line(offset.min(self.len_chars()))
    }

    /// `line`'s text without its line break.
    pub fn line(&self, line: usize) -> String {
        self.rope.slice(self.line_start(line)..self.line_end(line)).to_string()
    }

    /// The text between two offsets (clamped and ordered).
    pub fn slice(&self, a: usize, b: usize) -> String {
        let (a, b) = (a.min(b).min(self.len_chars()), a.max(b).min(self.len_chars()));
        self.rope.slice(a..b).to_string()
    }

    pub fn char_at(&self, offset: usize) -> Option<char> {
        (offset < self.len_chars()).then(|| self.rope.char(offset))
    }

    /// The offset of `pos`, clamped: past the last line is the last line,
    /// past a line's end is its end, and inside a surrogate pair is before
    /// the pair. Inside a grapheme cluster is fine.
    pub fn offset(&self, pos: Position) -> usize {
        let line = pos.line.min(self.line_count() - 1);
        let (start, end) = (self.rope.line_to_char(line), self.line_end(line));
        let base = self.rope.char_to_utf16_cu(start);
        let target = (base + pos.character).min(self.rope.char_to_utf16_cu(end));
        // ropey gives the character a unit falls in: before a split pair.
        self.rope.utf16_cu_to_char(target)
    }

    /// The position of `offset` (clamped to the end).
    pub fn position(&self, offset: usize) -> Position {
        let offset = offset.min(self.len_chars());
        let line = self.rope.char_to_line(offset);
        let start = self.rope.line_to_char(line);
        Position { line, character: self.rope.char_to_utf16_cu(offset) - self.rope.char_to_utf16_cu(start) }
    }

    /// The offsets of `range`, ordered (a reversed range is swapped).
    pub fn offsets(&self, range: Range) -> (usize, usize) {
        let (a, b) = (self.offset(range.start), self.offset(range.end));
        (a.min(b), a.max(b))
    }

    pub fn range(&self, a: usize, b: usize) -> Range {
        Range { start: self.position(a), end: self.position(b) }
    }

    /// UTF-16 offset from the start of the text (JavaScript's `offsetAt`).
    pub fn utf16_offset(&self, offset: usize) -> usize {
        self.rope.char_to_utf16_cu(offset.min(self.len_chars()))
    }

    /// The offset at a UTF-16 offset from the start (clamped, before a split
    /// pair).
    pub fn from_utf16_offset(&self, units: usize) -> usize {
        self.rope.utf16_cu_to_char(units.min(self.rope.len_utf16_cu()))
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Self { rope: Rope::from_str(s) }
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for chunk in self.rope.chunks() {
            f.write_str(chunk)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: usize, character: usize) -> Position {
        Position { line, character }
    }

    #[test]
    fn positions_count_utf16_units_and_clamp() {
        // "a😀b": the emoji is two UTF-16 units, one char.
        let t = Text::from("a😀b\nxy\n");
        assert_eq!(t.line_count(), 3, "a final line break leaves an empty last line");
        assert_eq!(t.offset(p(0, 1)), 1);
        assert_eq!(t.offset(p(0, 3)), 2, "after the pair");
        assert_eq!(t.offset(p(0, 2)), 1, "inside the pair goes before it");
        assert_eq!(t.offset(p(0, 99)), 3, "past the end of the line");
        assert_eq!(t.offset(p(9, 0)), t.len_chars(), "past the last line");
        assert_eq!(t.position(2), p(0, 3));
        assert_eq!(t.position(4), p(1, 0));
        for off in 0..=t.len_chars() {
            assert_eq!(t.offset(t.position(off)), off, "round trip at {off}");
        }
        let r = Range { start: p(1, 2), end: p(0, 1) };
        assert_eq!(t.offsets(r), (1, 6), "a reversed range is swapped");
    }

    #[test]
    fn crlf_files_are_lf_inside_and_written_back_as_they_were() {
        let (t, e) = Text::from_file_text("a\r\nb\r\nc\n");
        assert_eq!(e, LineEnding::CrLf, "most lines");
        assert_eq!(t.to_string(), "a\nb\nc\n");
        assert_eq!(t.to_file_text(e), "a\r\nb\r\nc\r\n", "mixed files are normalised");
        let (t, e) = Text::from_file_text("x\ry\n");
        assert_eq!(e, LineEnding::Lf);
        assert_eq!(t.line_count(), 2, "a lone CR is not a line break");
    }

    #[test]
    fn lines_exclude_their_break() {
        let t = Text::from("ab\n\ncd");
        assert_eq!((t.line(0).as_str(), t.line(1).as_str(), t.line(2).as_str()), ("ab", "", "cd"));
        assert_eq!((t.line_start(2), t.line_end(2), t.line_end(0)), (4, 6, 2));
        let empty = Text::default();
        assert_eq!((empty.line_count(), empty.line_end(0), empty.offset(p(3, 3))), (1, 0, 0));
    }
}
