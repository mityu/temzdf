//! Pane status lines: the bottom row of a pane (unless hidden), drawn by the
//! session as text from what the pane says about itself (a PDF pane: its
//! file, page, zoom, search and messages; a terminal: nothing yet). How to
//! change what they say is for later (`doc/floating-panes.md`).

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
    fn wide_characters_take_two_cells_and_are_not_split() {
        let bar = StatusBar { left: "日本語.pdf".into(), ..Default::default() };
        let row = cells(&bar, 6, true);
        // Room for 4 columns: two wide characters.
        assert_eq!(text(&row), " 日本 ");
        assert_eq!((row.len(), row[2].width, row[2].text.as_str()), (6, 0, ""), "a wide character's spacer");
    }
}
