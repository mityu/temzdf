//! Text cells on the terminal: what a frame's [`TextGrid`]s ask for, written as
//! cursor moves, SGR and characters, only where it differs from what the
//! terminal already shows.

use std::fmt::Write as _;

use temzdf_scene::{Attrs, Cell, Color, TextGrid, Underline};

/// What we last wrote to each cell. `None`: nothing of ours (the cell shows
/// the images below, or tmux placeholder text).
#[derive(Debug, Default)]
pub struct TextScreen {
    cols: u16,
    rows: u16,
    shown: Vec<Option<Cell>>,
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
        Self { cols, rows, shown: vec![None; cols as usize * rows as usize] }
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
    pub fn would_clear(&self, grids: &[TextGrid]) -> bool {
        let want = self.desired(grids);
        self.shown.iter().zip(&want).any(|(s, w)| s.is_some() && w.is_none())
    }

    /// Bytes that make the terminal show `grids`; empty when it already does.
    pub fn render(&mut self, grids: &[TextGrid]) -> Vec<u8> {
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
                if want[i] == self.shown[i] && again != Some(i) {
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
                    Some(c) => {
                        (c.fg, c.bg, c.attrs, if c.text.is_empty() { " " } else { c.text.as_str() }, c.width.max(1))
                    }
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
        if pen.is_some() {
            out.push_str("\x1b[0m");
        }
        out.into_bytes()
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
        term.feed(&t.render(&[grid(2, 0, &["hello", "world"])]));
        assert_eq!(term.text_rows(), vec!["  hello", "  world", ""]);
        assert!(t.render(&[grid(2, 0, &["hello", "world"])]).is_empty(), "nothing changed");
        let out = t.render(&[grid(2, 0, &["help!", "world"])]);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("p!") && !s.contains("he") && !s.contains("world"), "{s:?}");
        term.feed(&out);
        assert_eq!(term.text_rows(), vec!["  help!", "  world", ""]);
        assert!(t.would_clear(&[grid(2, 0, &["help!"])]));
        term.feed(&t.render(&[grid(2, 0, &["help!"])]));
        assert_eq!(term.text_rows(), vec!["  help!", "", ""]);
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
        let out = String::from_utf8(t.render(std::slice::from_ref(&g))).unwrap();
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
            t.render(std::slice::from_ref(&before));
            let _ = tx.send((t.render(std::slice::from_ref(&after)), t.render(std::slice::from_ref(&after))));
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
        let out = String::from_utf8(t.render(std::slice::from_ref(&g))).unwrap();
        assert!(!out.contains('数'), "{out:?}");
        let mut term =
            MockTerminal::new(Geometry { cols: 4, rows: 2, cell: CellSize { width: 8, height: 16 } }, [0, 0, 0, 255]);
        term.feed(out.as_bytes());
        assert_eq!(term.text_rows(), vec!["", ""], "nothing wrapped onto the next row");
    }
}
