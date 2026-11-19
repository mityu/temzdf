//! Text cells on the terminal: what a frame's [`TextGrid`]s ask for, written as
//! cursor moves, SGR and characters, only where it differs from what the
//! terminal already shows.

use std::fmt::Write as _;

use temzdf_scene::{Attrs, Cell, Color, TextGrid, Underline};
use termpix::compositor::CellRange;

/// What we last wrote to each cell. `None`: nothing of ours (the cell shows
/// the images below, or tmux placeholder text).
#[derive(Debug, Default)]
pub struct TextScreen {
    cols: u16,
    rows: u16,
    shown: Vec<Option<Cell>>,
    /// Cells the next render writes even if `shown` matches (see
    /// [`TextScreen::redraw_wide`]).
    redraw: Vec<bool>,
}

/// The pen: the SGR state the terminal is in while writing a run.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pen {
    fg: Color,
    bg: Color,
    attrs: Attrs,
}

impl TextScreen {
    pub fn new(cols: u16, rows: u16) -> Self {
        let n = cols as usize * rows as usize;
        Self { cols, rows, shown: vec![None; n], redraw: vec![false; n] }
    }

    /// The terminal changed size (it may have moved or dropped text): start
    /// from an unknown screen.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        *self = Self::new(cols, rows);
    }

    /// Forget what was written (something else overwrote the screen, e.g.
    /// placeholder text): the next render writes every text cell again.
    pub fn invalidate(&mut self) {
        self.shown.fill(None);
    }

    /// Write the wide characters in `cells` again with the next render (both
    /// their halves): images were placed there, and some terminals (WezTerm)
    /// drop the wide characters under a placement. Where the next frame has no
    /// text, the usual rule applies (our text is cleared).
    pub fn redraw_wide(&mut self, cells: &[CellRange]) {
        let cols = self.cols as usize;
        for r in cells {
            for y in r.row as usize..(r.row + r.rows).min(self.rows as u32) as usize {
                for x in r.col as usize..(r.col + r.cols).min(self.cols as u32) as usize {
                    let i = y * cols + x;
                    match self.shown[i].as_ref().map(|c| c.width) {
                        Some(2) => {
                            self.redraw[i] = true;
                            if x + 1 < cols {
                                self.redraw[i + 1] = true;
                            }
                        }
                        Some(0) if x > 0 => {
                            self.redraw[i] = true;
                            self.redraw[i - 1] = true;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// The cells `grids` ask for (later grids win where they overlap), built
    /// once per frame for [`TextScreen::would_clear`],
    /// [`TextScreen::clear_gone`] and [`TextScreen::render`]. It borrows the
    /// grids' cells: one is cloned only where it changed. Build it after
    /// [`TextScreen::resize`].
    pub fn wanted<'a>(&self, grids: impl IntoIterator<Item = &'a TextGrid>) -> Wanted<'a> {
        let (cols, rows) = (self.cols as usize, self.rows as usize);
        let mut cells = vec![None; self.shown.len()];
        for g in grids {
            for (dy, row) in g.rows.iter().enumerate() {
                let y = g.row as usize + dy;
                if y >= rows {
                    break;
                }
                for (dx, cell) in row.iter().enumerate() {
                    let x = g.col as usize + dx;
                    if x >= cols {
                        break;
                    }
                    cells[y * cols + x] = Some(cell);
                }
            }
        }
        Wanted { cells }
    }

    /// Whether showing `want` takes text away from cells that had some (the
    /// images below must show there again).
    pub fn would_clear(&self, want: &Wanted) -> bool {
        debug_assert_eq!(want.cells.len(), self.shown.len());
        self.shown.iter().zip(&want.cells).any(|(s, w)| s.is_some() && w.is_none())
    }

    /// Bytes that blank our text where `want` has none (the images below
    /// show there again), ahead of the images: see
    /// [`termpix::compositor::Compositor::present_with`]. [`TextScreen::render`]
    /// then writes the rest.
    pub fn clear_gone(&mut self, want: &Wanted) -> Vec<u8> {
        debug_assert_eq!(want.cells.len(), self.shown.len());
        let want = &want.cells;
        let cols = self.cols as usize;
        let mut out = String::new();
        for y in 0..self.rows as usize {
            let mut at: Option<usize> = None;
            for x in 0..cols {
                let i = y * cols + x;
                if self.shown[i].is_none() || want[i].is_some() {
                    continue;
                }
                if at != Some(x) {
                    let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
                }
                out.push(' ');
                at = Some(x + 1);
                self.shown[i] = None;
            }
        }
        if !out.is_empty() {
            out.insert_str(0, "\x1b[0m");
        }
        out.into_bytes()
    }

    /// Bytes that make the terminal show `want`; empty when it already does.
    pub fn render(&mut self, want: &Wanted) -> Vec<u8> {
        debug_assert_eq!(want.cells.len(), self.shown.len());
        let want = &want.cells;
        let cols = self.cols as usize;
        let mut out = String::new();
        let mut pen: Option<Pen> = None;
        for y in 0..self.rows as usize {
            // Where the cursor is after the last character written (`None`:
            // somewhere else, so move it before writing).
            let mut at: Option<usize> = None;
            // A wide character to write again although it did not change:
            // only its spacer did (a selection ending in its right half).
            let mut again: Option<usize> = None;
            let mut x = 0;
            while x < cols {
                let i = y * cols + x;
                let redraw = self.redraw[i] && want[i].is_some();
                if want[i] == self.shown[i].as_ref() && again != Some(i) && !redraw {
                    x += 1;
                    continue;
                }
                let cell = want[i];
                // A spacer is written with the wide character before it.
                if cell.as_ref().is_some_and(|c| c.width == 0)
                    && x > 0
                    && want[i - 1].as_ref().is_some_and(|c| c.width == 2)
                {
                    x -= 1;
                    at = None;
                    again = Some(i - 1);
                    continue;
                }
                if at != Some(x) {
                    let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
                }
                let (fg, bg, attrs, text, width) = match cell {
                    // Our text goes away: blank with the default pen.
                    None => (Color::Default, Color::Default, Attrs::default(), " ", 1),
                    Some(c) if c.width == 0 => (c.fg, c.bg, c.attrs, " ", 1),
                    // In the last column it would wrap, or scroll the screen.
                    Some(c) if c.width == 2 && x + 1 >= cols => (c.fg, c.bg, c.attrs, " ", 1),
                    // A control character would reach the terminal as one
                    // (an escape sequence from a file name or a document):
                    // whoever made the cell should have left it out.
                    Some(c) if c.text.is_empty() || c.text.chars().any(char::is_control) => {
                        (c.fg, c.bg, c.attrs, " ", c.width.max(1))
                    }
                    Some(c) => (c.fg, c.bg, c.attrs, c.text.as_str(), c.width.max(1)),
                };
                let p = Pen { fg, bg, attrs };
                if pen != Some(p) {
                    sgr(&mut out, &p);
                    pen = Some(p);
                }
                out.push_str(text);
                keep(&mut self.shown[i], cell);
                if width == 2 && x + 1 < cols {
                    keep(&mut self.shown[i + 1], want[i + 1]);
                    // The terminal may not agree that this character is wide.
                    at = None;
                    x += 2;
                } else {
                    at = Some(x + 1);
                    x += 1;
                }
            }
        }
        self.redraw.fill(false);
        if pen.is_some() {
            out.push_str("\x1b[0m");
        }
        out.into_bytes()
    }
}

/// The cells a frame's grids ask for: see [`TextScreen::wanted`].
#[derive(Debug)]
pub struct Wanted<'a> {
    cells: Vec<Option<&'a Cell>>,
}

/// Remember `want` as shown, cloning it only when it differs.
fn keep(shown: &mut Option<Cell>, want: Option<&Cell>) {
    if shown.as_ref() != want {
        *shown = want.cloned();
    }
}

fn sgr(out: &mut String, p: &Pen) {
    out.push_str("\x1b[0");
    let a = &p.attrs;
    for (on, code) in [
        (a.bold, "1"),
        (a.faint, "2"),
        (a.italic, "3"),
        (a.blink, "5"),
        (a.inverse, "7"),
        (a.invisible, "8"),
        (a.strikethrough, "9"),
        (a.overline, "53"),
    ] {
        if on {
            out.push(';');
            out.push_str(code);
        }
    }
    match a.underline {
        Underline::None => {}
        Underline::Single => out.push_str(";4"),
        Underline::Double => out.push_str(";4:2"),
        Underline::Curly => out.push_str(";4:3"),
        Underline::Dotted => out.push_str(";4:4"),
        Underline::Dashed => out.push_str(";4:5"),
    }
    color(out, p.fg, 30, 90, 38);
    color(out, p.bg, 40, 100, 48);
    out.push('m');
}

/// SGR parameters for a colour: the 8 basic and 8 bright palette colours
/// with their short codes (`base`, `bright`), the rest of the palette and RGB
/// with the extended code (`ext`).
fn color(out: &mut String, c: Color, base: u8, bright: u8, ext: u8) {
    match c {
        Color::Default => {}
        Color::Indexed(n @ 0..8) => {
            let _ = write!(out, ";{}", base + n);
        }
        Color::Indexed(n @ 8..16) => {
            let _ = write!(out, ";{}", bright + n - 8);
        }
        Color::Indexed(n) => {
            let _ = write!(out, ";{ext};5;{n}");
        }
        Color::Rgb([r, g, b]) => {
            let _ = write!(out, ";{ext};2;{r};{g};{b}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termpix::geom::{CellSize, Geometry};
    use termpix::mock::MockTerminal;

    impl TextScreen {
        fn show(&mut self, grids: &[TextGrid]) -> Vec<u8> {
            let want = self.wanted(grids);
            self.render(&want)
        }

        fn clears(&self, grids: &[TextGrid]) -> bool {
            self.would_clear(&self.wanted(grids))
        }
    }

    fn cell(text: &str) -> Cell {
        Cell { text: text.into(), ..Cell::blank() }
    }

    fn grid(col: u16, row: u16, lines: &[&str]) -> TextGrid {
        TextGrid { col, row, rows: lines.iter().map(|l| l.chars().map(|c| cell(&c.to_string())).collect()).collect() }
    }

    fn mock() -> MockTerminal {
        MockTerminal::new(Geometry { cols: 12, rows: 3, cell: CellSize { width: 8, height: 16 } }, [0, 0, 0, 255])
    }

    #[test]
    fn writes_only_what_changed_and_clears_what_went_away() {
        let mut t = TextScreen::new(12, 3);
        let mut term = mock();
        term.feed(&t.show(&[grid(2, 0, &["hello", "world"])]));
        assert_eq!(term.text_rows(), vec!["  hello", "  world", ""]);
        assert!(t.show(&[grid(2, 0, &["hello", "world"])]).is_empty(), "nothing changed");
        let out = t.show(&[grid(2, 0, &["help!", "world"])]);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("p!") && !s.contains("he") && !s.contains("world"), "{s:?}");
        term.feed(&out);
        assert_eq!(term.text_rows(), vec!["  help!", "  world", ""]);
        assert!(t.clears(&[grid(2, 0, &["help!"])]));
        term.feed(&t.show(&[grid(2, 0, &["help!"])]));
        assert_eq!(term.text_rows(), vec!["  help!", "", ""]);
    }

    #[test]
    fn control_characters_never_reach_the_terminal() {
        let mut t = TextScreen::new(12, 3);
        let mut term = mock();
        let out = t.show(&[TextGrid {
            col: 0,
            row: 0,
            rows: vec![vec![cell("a"), cell("\x1b]52;c;eA==\x07"), cell("\u{9b}2J"), cell("\x7f"), cell("b")]],
        }]);
        let s = String::from_utf8_lossy(&out);
        assert!(!s.contains(['\x07', '\u{9b}', '\x7f']) && !s.contains("\x1b]"), "{s:?}");
        term.feed(&out);
        assert_eq!(term.text_rows(), vec!["a   b", "", ""]);
    }

    #[test]
    fn wide_characters_and_styles() {
        let mut t = TextScreen::new(12, 3);
        let wide = Cell { text: "数".into(), width: 2, ..Cell::default() };
        let spacer = Cell { width: 0, ..Cell::default() };
        let red = Cell {
            text: "x".into(),
            fg: Color::Rgb([255, 0, 0]),
            bg: Color::Indexed(4),
            attrs: Attrs { bold: true, ..Attrs::default() },
            ..Cell::blank()
        };
        let bright = Cell { text: "z".into(), fg: Color::Indexed(9), bg: Color::Indexed(200), ..Cell::blank() };
        let g = TextGrid { col: 0, row: 1, rows: vec![vec![wide, spacer, red, cell("y"), bright]] };
        let out = String::from_utf8(t.show(std::slice::from_ref(&g))).unwrap();
        // The cursor is placed again after the wide character, the pen changes
        // only where the style does, and is reset at the end.
        assert_eq!(out, "\x1b[2;1H\x1b[0m数\x1b[2;3H\x1b[0;1;38;2;255;0;0;44mx\x1b[0my\x1b[0;91;48;5;200mz\x1b[0m");
        let mut term = mock();
        term.feed(out.as_bytes());
        assert_eq!(term.text_rows()[1], "数 xyz");
    }

    #[test]
    fn a_changed_spacer_writes_its_wide_character_again() {
        // A selection ending in the right half of a wide character changes
        // only the spacer; that once looped forever.
        let wide = Cell { text: "数".into(), width: 2, ..Cell::default() };
        let spacer = Cell { width: 0, ..Cell::default() };
        let inverse = Cell { attrs: Attrs { inverse: true, ..Attrs::default() }, ..spacer.clone() };
        let before = TextGrid { col: 0, row: 0, rows: vec![vec![wide.clone(), spacer, cell("a")]] };
        let after = TextGrid { col: 0, row: 0, rows: vec![vec![wide, inverse, cell("a")]] };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut t = TextScreen::new(12, 3);
            t.show(std::slice::from_ref(&before));
            let _ = tx.send((t.show(std::slice::from_ref(&after)), t.show(std::slice::from_ref(&after))));
        });
        let (out, again) = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("render finishes");
        let out = String::from_utf8(out).unwrap();
        assert!(out.starts_with("\x1b[1;1H") && out.contains('数'), "{out:?}");
        assert!(again.is_empty(), "shown now");
    }

    #[test]
    fn a_wide_character_in_the_last_column_is_written_as_a_blank() {
        let mut t = TextScreen::new(4, 2);
        let wide = Cell { text: "数".into(), width: 2, ..Cell::default() };
        let g = TextGrid { col: 3, row: 0, rows: vec![vec![wide]] };
        let out = String::from_utf8(t.show(std::slice::from_ref(&g))).unwrap();
        assert!(!out.contains('数'), "{out:?}");
        let mut term =
            MockTerminal::new(Geometry { cols: 4, rows: 2, cell: CellSize { width: 8, height: 16 } }, [0, 0, 0, 255]);
        term.feed(out.as_bytes());
        assert_eq!(term.text_rows(), vec!["", ""], "nothing wrapped onto the next row");
    }

    #[test]
    fn wide_characters_under_a_placement_are_written_again() {
        // WezTerm drops the wide characters in cells an image is placed on.
        let wide = |t: &str| Cell { text: t.into(), width: 2, ..Cell::default() };
        let spacer = Cell { width: 0, ..Cell::default() };
        let row = vec![wide("い"), spacer.clone(), cell("a"), wide("ろ"), spacer.clone(), cell("b")];
        let g = TextGrid { col: 0, row: 0, rows: vec![row] };
        let mut t = TextScreen::new(12, 3);
        t.show(std::slice::from_ref(&g));
        assert!(t.show(std::slice::from_ref(&g)).is_empty());
        // An image over columns 1-3: the spacer of い and the first half of ろ.
        t.redraw_wide(&[CellRange { col: 1, row: 0, cols: 3, rows: 1 }]);
        let out = String::from_utf8(t.show(std::slice::from_ref(&g))).unwrap();
        assert_eq!(out, "\x1b[1;1H\x1b[0mい\x1b[1;4Hろ\x1b[0m", "both, whole; not the ASCII a");
        assert!(t.show(std::slice::from_ref(&g)).is_empty(), "once");
        // Where the next frame has no text, ours is still cleared.
        t.redraw_wide(&[CellRange { col: 0, row: 0, cols: 2, rows: 1 }]);
        let without = TextGrid { col: 2, row: 0, rows: vec![g.rows[0][2..].to_vec()] };
        let out = String::from_utf8(t.show(&[without])).unwrap();
        assert_eq!(out, "\x1b[1;1H\x1b[0m  \x1b[0m");
    }

    // The implementation before the wanted cells were borrowed (they were
    // cloned, up to three times a frame): the reference the bytes are
    // compared with below.
    /// What we last wrote to each cell. `None`: nothing of ours (the cell shows
    /// the images below, or tmux placeholder text).
    struct OldScreen {
        cols: u16,
        rows: u16,
        shown: Vec<Option<Cell>>,
        /// Cells the next render writes even if `shown` matches (see
        /// `redraw_wide`).
        redraw: Vec<bool>,
    }

    impl OldScreen {
        fn new(cols: u16, rows: u16) -> Self {
            let n = cols as usize * rows as usize;
            Self { cols, rows, shown: vec![None; n], redraw: vec![false; n] }
        }

        fn invalidate(&mut self) {
            self.shown.fill(None);
        }

        /// Write the wide characters in `cells` again with the next render (both
        /// their halves): images were placed there, and some terminals (WezTerm)
        /// drop the wide characters under a placement. Where the next frame has no
        /// text, the usual rule applies (our text is cleared).
        fn redraw_wide(&mut self, cells: &[CellRange]) {
            let cols = self.cols as usize;
            for r in cells {
                for y in r.row as usize..(r.row + r.rows).min(self.rows as u32) as usize {
                    for x in r.col as usize..(r.col + r.cols).min(self.cols as u32) as usize {
                        let i = y * cols + x;
                        match self.shown[i].as_ref().map(|c| c.width) {
                            Some(2) => {
                                self.redraw[i] = true;
                                if x + 1 < cols {
                                    self.redraw[i + 1] = true;
                                }
                            }
                            Some(0) if x > 0 => {
                                self.redraw[i] = true;
                                self.redraw[i - 1] = true;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        /// The desired cell contents for `grids` (later grids win where they overlap).
        fn desired(&self, grids: &[TextGrid]) -> Vec<Option<Cell>> {
            let mut want = vec![None; self.shown.len()];
            for g in grids {
                for (dy, row) in g.rows.iter().enumerate() {
                    let y = g.row as usize + dy;
                    if y >= self.rows as usize {
                        break;
                    }
                    for (dx, cell) in row.iter().enumerate() {
                        let x = g.col as usize + dx;
                        if x >= self.cols as usize {
                            break;
                        }
                        want[y * self.cols as usize + x] = Some(cell.clone());
                    }
                }
            }
            want
        }

        /// Whether showing `grids` takes text away from cells that had some (the
        /// images below must show there again).
        fn would_clear(&self, grids: &[TextGrid]) -> bool {
            let want = self.desired(grids);
            self.shown.iter().zip(&want).any(|(s, w)| s.is_some() && w.is_none())
        }

        /// Bytes that blank our text where `grids` have none (the images below
        /// show there again), ahead of the images: see
        /// the compositor. `render`
        /// then writes the rest.
        fn clear_gone(&mut self, grids: &[TextGrid]) -> Vec<u8> {
            let want = self.desired(grids);
            let cols = self.cols as usize;
            let mut out = String::new();
            for y in 0..self.rows as usize {
                let mut at: Option<usize> = None;
                for x in 0..cols {
                    let i = y * cols + x;
                    if self.shown[i].is_none() || want[i].is_some() {
                        continue;
                    }
                    if at != Some(x) {
                        let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
                    }
                    out.push(' ');
                    at = Some(x + 1);
                    self.shown[i] = None;
                }
            }
            if !out.is_empty() {
                out.insert_str(0, "\x1b[0m");
            }
            out.into_bytes()
        }

        /// Bytes that make the terminal show `grids`; empty when it already does.
        fn render(&mut self, grids: &[TextGrid]) -> Vec<u8> {
            let want = self.desired(grids);
            let cols = self.cols as usize;
            let mut out = String::new();
            let mut pen: Option<Pen> = None;
            for y in 0..self.rows as usize {
                // Where the cursor is after the last character written (`None`:
                // somewhere else, so move it before writing).
                let mut at: Option<usize> = None;
                // A wide character to write again although it did not change:
                // only its spacer did (a selection ending in its right half).
                let mut again: Option<usize> = None;
                let mut x = 0;
                while x < cols {
                    let i = y * cols + x;
                    let redraw = self.redraw[i] && want[i].is_some();
                    if want[i] == self.shown[i] && again != Some(i) && !redraw {
                        x += 1;
                        continue;
                    }
                    let cell = want[i].clone();
                    // A spacer is written with the wide character before it.
                    if cell.as_ref().is_some_and(|c| c.width == 0)
                        && x > 0
                        && want[i - 1].as_ref().is_some_and(|c| c.width == 2)
                    {
                        x -= 1;
                        at = None;
                        again = Some(i - 1);
                        continue;
                    }
                    if at != Some(x) {
                        let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
                    }
                    let (fg, bg, attrs, text, width) = match &cell {
                        // Our text goes away: blank with the default pen.
                        None => (Color::Default, Color::Default, Attrs::default(), " ", 1),
                        Some(c) if c.width == 0 => (c.fg, c.bg, c.attrs, " ", 1),
                        // In the last column it would wrap, or scroll the screen.
                        Some(c) if c.width == 2 && x + 1 >= cols => (c.fg, c.bg, c.attrs, " ", 1),
                        // A control character would reach the terminal as one
                        // (an escape sequence from a file name or a document):
                        // whoever made the cell should have left it out.
                        Some(c) if c.text.is_empty() || c.text.chars().any(char::is_control) => {
                            (c.fg, c.bg, c.attrs, " ", c.width.max(1))
                        }
                        Some(c) => (c.fg, c.bg, c.attrs, c.text.as_str(), c.width.max(1)),
                    };
                    let p = Pen { fg, bg, attrs };
                    if pen != Some(p) {
                        sgr(&mut out, &p);
                        pen = Some(p);
                    }
                    out.push_str(text);
                    self.shown[i] = cell;
                    if width == 2 && x + 1 < cols {
                        self.shown[i + 1] = want[i + 1].clone();
                        // The terminal may not agree that this character is wide.
                        at = None;
                        x += 2;
                    } else {
                        at = Some(x + 1);
                        x += 1;
                    }
                }
            }
            self.redraw.fill(false);
            if pen.is_some() {
                out.push_str("\x1b[0m");
            }
            out.into_bytes()
        }
    }

    /// A small deterministic pseudo-random sequence (xorshift).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// A row of random cells: ASCII, wide characters (with their spacers,
    /// sometimes styled apart), control text, styles, blanks.
    fn random_row(rng: &mut Rng, len: usize) -> Vec<Cell> {
        let style = |rng: &mut Rng| -> (Color, Color, Attrs) {
            let pick = |rng: &mut Rng| match rng.below(4) {
                0 => Color::Default,
                1 => Color::Indexed(rng.below(256) as u8),
                2 => Color::Indexed(rng.below(16) as u8),
                _ => Color::Rgb([rng.below(3) as u8 * 100, 0, 7]),
            };
            let attrs = Attrs { bold: rng.below(4) == 0, inverse: rng.below(5) == 0, ..Attrs::default() };
            (pick(rng), pick(rng), attrs)
        };
        let mut row = Vec::new();
        while row.len() < len {
            let (fg, bg, attrs) = if rng.below(3) == 0 { style(rng) } else { Default::default() };
            match rng.below(10) {
                0 | 1 if row.len() + 1 < len => {
                    let text = ["数", "い", "ろ"][rng.below(3) as usize].to_string();
                    row.push(Cell { text, width: 2, fg, bg, attrs });
                    let (fg, bg, attrs) = if rng.below(3) == 0 { style(rng) } else { (fg, bg, attrs) };
                    row.push(Cell { width: 0, fg, bg, attrs, ..Cell::default() });
                }
                2 => row.push(Cell { fg, bg, attrs, ..Cell::blank() }),
                3 if rng.below(4) == 0 => row.push(Cell { text: "\x1b[2J".into(), fg, bg, attrs, width: 1 }),
                _ => {
                    let text = ((b'a' + rng.below(4) as u8) as char).to_string();
                    row.push(Cell { text, fg, bg, attrs, width: 1 });
                }
            }
        }
        row.truncate(len);
        row
    }

    #[test]
    fn writes_the_same_bytes_as_the_cloning_implementation() {
        let (cols, rows) = (20u16, 6u16);
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut new = TextScreen::new(cols, rows);
        let mut old = OldScreen::new(cols, rows);
        // A full screen (a shell), sometimes with a status line over it.
        let mut screen = TextGrid { col: 0, row: 0, rows: (0..rows).map(|_| random_row(&mut rng, 20)).collect() };
        let mut written = 0;
        // Fewer frames under Miri, where the whole run takes hours.
        let frames = if cfg!(miri) { 40 } else { 2000 };
        for frame in 0..frames {
            match rng.below(8) {
                // Typing: one cell changes.
                0..=2 => {
                    let y = rng.below(rows as u64) as usize;
                    let x = rng.below(screen.rows[y].len().max(1) as u64) as usize;
                    let c = random_row(&mut rng, 1).remove(0);
                    if let Some(at) = screen.rows[y].get_mut(x) {
                        *at = c;
                    }
                }
                // Scrolling: rows move up, a new one at the bottom.
                3 => {
                    screen.rows.remove(0);
                    let n = rng.below(25) as usize;
                    screen.rows.push(random_row(&mut rng, n));
                }
                // A selection: a run of cells changes style (it may end in a
                // wide character's half).
                4 => {
                    let y = rng.below(rows as u64) as usize;
                    let a = rng.below(cols as u64) as usize;
                    let b = a + rng.below(8) as usize;
                    for c in screen.rows[y].iter_mut().take(b).skip(a) {
                        c.attrs.inverse = !c.attrs.inverse;
                    }
                }
                // Cells cleared: a shorter row, or the grid moved.
                5 => {
                    let y = rng.below(rows as u64) as usize;
                    let n = rng.below(cols as u64) as usize;
                    screen.rows[y].truncate(n);
                }
                6 => screen.col = rng.below(3) as u16,
                _ => {
                    let y = rng.below(rows as u64) as usize;
                    screen.rows[y] = random_row(&mut rng, 22);
                }
            }
            let mut grids = vec![screen.clone()];
            if rng.below(3) == 0 {
                let col = rng.below(cols as u64) as u16;
                grids.push(TextGrid { col, row: rows - 1, rows: vec![random_row(&mut rng, 6)] });
            }
            if rng.below(10) == 0 {
                let r = CellRange {
                    col: rng.below(cols as u64) as u32,
                    row: rng.below(rows as u64) as u32,
                    cols: rng.below(6) as u32,
                    rows: rng.below(3) as u32,
                };
                new.redraw_wide(&[r]);
                old.redraw_wide(&[r]);
            }
            if rng.below(50) == 0 {
                new.invalidate();
                old.invalidate();
            }
            let want = new.wanted(&grids);
            assert_eq!(new.would_clear(&want), old.would_clear(&grids), "frame {frame}");
            let (a, b) = (new.clear_gone(&want), old.clear_gone(&grids));
            assert_eq!(String::from_utf8_lossy(&a), String::from_utf8_lossy(&b), "clear_gone, frame {frame}");
            let (a, b) = (new.render(&want), old.render(&grids));
            assert_eq!(String::from_utf8_lossy(&a), String::from_utf8_lossy(&b), "render, frame {frame}");
            assert_eq!(new.shown, old.shown, "frame {frame}");
            assert_eq!(new.redraw, old.redraw, "frame {frame}");
            written += a.len();
        }
        assert!(written > frames * 5, "the frames wrote something: {written}");
    }
}
