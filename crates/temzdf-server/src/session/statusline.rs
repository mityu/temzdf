//! Pane status lines: the bottom row of a pane (unless hidden), drawn by the
//! session as text from what the pane says about itself (a PDF pane: its
//! file, page, zoom, search and messages; a terminal: nothing yet). How to
//! change what they say is for later (`doc/design/floating-panes.md`).

use temzdf_scene::{Attrs, Cell, Color, TextGrid};
use termpix_ui::{StatusBar, Theme};

use super::notices::width;

/// The status line of a pane whose rectangle (cells) is `rect`, saying
/// `bar`: the left text (or an error, or a notice, in their colours) from
/// the left, the right text at the right; the left side gives way.
pub(super) fn grid(bar: &StatusBar, rect: temzdf_scene::Rect, focused: bool) -> TextGrid {
    let row = rect.y + rect.h as i32 - 1;
    TextGrid { col: rect.x.max(0) as u16, row: row.max(0) as u16, rows: vec![cells(bar, rect.w as usize, focused)] }
}

/// The cells of `bar`, `cols` wide.
pub(super) fn cells(bar: &StatusBar, cols: usize, focused: bool) -> Vec<Cell> {
    let theme = Theme::dark();
    let rgb = |c: termpix_ui::Color| Color::Rgb([c.0[0], c.0[1], c.0[2]]);
    let bg = rgb(theme.bar_bg);
    let (left, left_fg) = match (&bar.error, &bar.notice) {
        (Some(e), _) => (e.as_str(), theme.error),
        (None, Some(n)) => (n.as_str(), theme.accent),
        (None, None) => (bar.left.as_str(), if focused { theme.bar_fg } else { theme.bar_dim }),
    };
    let mut row = vec![Cell { text: " ".into(), width: 1, fg: Color::Default, bg, attrs: Attrs::default() }; cols];
    // One blank cell at each end, three between the two sides.
    let right_w = text_width(&bar.right).min(cols.saturating_sub(2));
    let right_at = cols.saturating_sub(1 + right_w);
    let left_room = if right_w == 0 { cols.saturating_sub(2) } else { right_at.saturating_sub(4) };
    put(&mut row, 1, left, left_room, rgb(left_fg), bg);
    put(&mut row, right_at, &bar.right, right_w, rgb(theme.bar_dim), bg);
    row
}

/// The window's status line, `cols` wide: the mode at the left on the
/// accent colour, `right` at the right, on the bars' background.
pub(super) fn window(mode: &str, right: &str, cols: usize) -> Vec<Cell> {
    let theme = Theme::dark();
    let rgb = |c: termpix_ui::Color| Color::Rgb([c.0[0], c.0[1], c.0[2]]);
    let bg = rgb(theme.bar_bg);
    let mut row = vec![Cell { text: " ".into(), width: 1, fg: Color::Default, bg, attrs: Attrs::default() }; cols];
    let label = format!(" {mode} ");
    let label_w = text_width(&label).min(cols);
    let accent = rgb(theme.accent);
    put(&mut row, 0, &label, label_w, rgb(theme.bar_bg), accent);
    for c in &mut row[..label_w] {
        c.bg = accent;
        c.attrs.bold = true;
    }
    let right_w = text_width(right).min(cols.saturating_sub(label_w + 2));
    put(&mut row, cols.saturating_sub(1 + right_w), right, right_w, rgb(theme.bar_dim), bg);
    row
}

/// `text` on the middle row of `rect`, centred (cut to fit), dimmed, on
/// the terminal's background.
pub(super) fn centred(text: &str, rect: temzdf_scene::Rect) -> TextGrid {
    let theme = Theme::dark();
    let fg = Color::Rgb([theme.bar_dim.0[0], theme.bar_dim.0[1], theme.bar_dim.0[2]]);
    let cols = rect.w as usize;
    let w = text_width(text).min(cols);
    let mut row = vec![Cell { text: " ".into(), width: 1, fg, bg: Color::Default, attrs: Attrs::default() }; w];
    put(&mut row, 0, text, w, fg, Color::Default);
    let col = rect.x + ((cols - w) / 2) as i32;
    let line = rect.y + (rect.h as i32 - 1) / 2;
    TextGrid { col: col.max(0) as u16, row: line.max(0) as u16, rows: vec![row] }
}

fn text_width(s: &str) -> usize {
    s.chars().filter(|c| !c.is_control()).map(width).sum()
}

/// Write `text` into `row` from column `at`, at most `room` columns (a wide
/// character that would not fit is left out).
fn put(row: &mut [Cell], at: usize, text: &str, room: usize, fg: Color, bg: Color) {
    let mut col = at;
    for c in text.chars().filter(|c| !c.is_control()) {
        let w = width(c);
        if col + w > at + room || col + w > row.len() {
            break;
        }
        row[col] = Cell { text: c.to_string(), width: w as u8, fg, bg, attrs: Attrs::default() };
        if w == 2 {
            row[col + 1] = Cell { text: String::new(), width: 0, fg, bg, attrs: Attrs::default() };
        }
        col += w;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(row: &[Cell]) -> String {
        row.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn left_and_right_with_the_left_giving_way() {
        let bar = StatusBar { left: "sample.pdf".into(), right: "1 / 3   111%".into(), ..Default::default() };
        let row = cells(&bar, 30, true);
        assert_eq!(text(&row), " sample.pdf      1 / 3   111% ");
        // Narrow: the right side stays whole, the left is cut (three cells
        // between them).
        assert_eq!(text(&cells(&bar, 20, true)), " sam   1 / 3   111% ");
        // An error replaces the left side, in the error colour.
        let err = StatusBar { error: Some("Pattern not found: x".into()), ..bar.clone() };
        let row = cells(&err, 40, true);
        assert!(text(&row).starts_with(" Pattern not found: x"));
        assert_eq!(
            row[1].fg,
            Color::Rgb({
                let c = Theme::dark().error.0;
                [c[0], c[1], c[2]]
            })
        );
        // Nothing to say: a blank row in the bar's colour.
        let row = cells(&StatusBar::default(), 5, false);
        assert_eq!(text(&row), "     ");
        assert!(row.iter().all(|c| c.bg != Color::Default));
    }

    #[test]
    fn centred_lines_are_cut_to_fit() {
        let g = centred("abcdef", temzdf_scene::Rect::new(10, 2, 10, 5));
        assert_eq!((g.col, g.row, text(&g.rows[0])), (12, 4, "abcdef".into()));
        let g = centred("abcdef", temzdf_scene::Rect::new(0, 0, 4, 1));
        assert_eq!((g.col, g.row, text(&g.rows[0])), (0, 0, "abcd".into()));
    }

    #[test]
    fn wide_characters_take_two_cells_and_are_not_split() {
        let bar = StatusBar { left: "日本語.pdf".into(), ..Default::default() };
        let row = cells(&bar, 6, true);
        // Room for 4 columns: two wide characters.
        assert_eq!(text(&row), " 日本 ");
        assert_eq!((row.len(), row[2].width, row[2].text.as_str()), (6, 0, ""), "a wide character's spacer");
    }
}
