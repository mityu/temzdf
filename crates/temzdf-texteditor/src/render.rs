//! Drawing a view into text cells: the gutter, the text, decorations, the
//! selection and the cursor line, styled from highlight groups.

use std::collections::HashMap;

use temzdf_scene::{Attrs, Cell, Color, Underline};

use crate::buffer::Buffer;
use crate::cursor::SelectionKind;
use crate::layout::GlyphKind;
use crate::view::{NumberMode, View};

/// A style: what it sets of the colours and attributes. Later styles drawn
/// over earlier ones replace what they set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub attrs: Option<Attrs>,
}

impl Style {
    fn apply(&self, cell: &mut Cell) {
        if let Some(fg) = self.fg {
            cell.fg = fg;
        }
        if let Some(bg) = self.bg {
            cell.bg = bg;
        }
        if let Some(a) = self.attrs {
            cell.attrs = a;
        }
    }
}

/// Highlight groups by name. Scripts define them (and the colour scheme);
/// these defaults only make the built-in parts visible.
#[derive(Clone, Debug)]
pub struct Highlights {
    groups: HashMap<String, Style>,
}

impl Default for Highlights {
    fn default() -> Self {
        let attrs = |f: fn(&mut Attrs)| {
            let mut a = Attrs::default();
            f(&mut a);
            Some(a)
        };
        let dim = Style { fg: Some(Color::Indexed(8)), ..Style::default() };
        let groups = [
            ("Selection", Style { attrs: attrs(|a| a.inverse = true), ..Style::default() }),
            ("LineNr", dim),
            ("CursorLineNr", Style { attrs: attrs(|a| a.bold = true), ..Style::default() }),
            ("CursorLine", Style::default()),
            ("NonText", dim),
            ("SpecialKey", Style { fg: Some(Color::Indexed(4)), ..Style::default() }),
            // The cursor of a pane without the focus (terminals draw only
            // one cursor).
            ("CursorUnfocused", Style { attrs: attrs(|a| a.underline = Underline::Single), ..Style::default() }),
        ];
        Self { groups: groups.into_iter().map(|(k, v)| (k.to_owned(), v)).collect() }
    }
}

impl Highlights {
    pub fn get(&self, name: &str) -> Option<&Style> {
        self.groups.get(name)
    }

    pub fn set(&mut self, name: &str, style: Style) {
        self.groups.insert(name.to_owned(), style);
    }
}

/// A drawn view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Drawn {
    /// `height` rows of `width` cells.
    pub rows: Vec<Vec<Cell>>,
    /// Where the cursor is (column, row), when it is shown and the pane has
    /// the focus.
    pub cursor: Option<(u16, u16)>,
}

/// Draw `view` of `buf`. `pane` picks the decorations shown only in it;
/// `focused` decides how the cursor is shown.
pub fn render(buf: &Buffer, view: &View, hl: &Highlights, pane: Option<u32>, focused: bool) -> Drawn {
    let text = buf.text();
    let (width, height) = (view.width as usize, view.height as usize);
    let gutter = view.gutter(text.line_count());
    let rows = view.rows(text);
    let (vis_start, vis_end) = (view.top, rows.last().map_or(view.top, |r| r.end));
    let marks: Vec<_> = buf
        .marks()
        .overlapping(vis_start, vis_end)
        .into_iter()
        .filter(|m| m.spec.style.is_some() && m.start < m.end && m.spec.pane.is_none_or(|p| Some(p) == pane))
        .collect();
    let cursor_line = text.line_of(view.cursor.pos);
    let selection = view.cursor.selection();
    // A block selection's display columns.
    let block = selection.filter(|s| s.2 == SelectionKind::Block).map(|(a, b, _)| {
        let l = view.layouter(text);
        let (ca, cb) = (l.line_col(a), l.line_col(b));
        (text.line_of(a), text.line_of(b), ca.min(cb), ca.max(cb) + 1)
    });
    let style = |name: &str, cell: &mut Cell| {
        if let Some(s) = hl.get(name) {
            s.apply(cell);
        }
    };
    let mut drawn = Drawn::default();
    for (y, row) in rows.iter().enumerate() {
        let on_cursor_line = view.cursorline && row.line == cursor_line;
        let mut base = Cell::blank();
        if on_cursor_line {
            style("CursorLine", &mut base);
        }
        let mut cells = vec![base.clone(); width];
        // The gutter.
        if gutter > 0 && row.first {
            let n = match view.number {
                NumberMode::Absolute => row.line + 1,
                NumberMode::Relative => row.line.abs_diff(cursor_line),
                NumberMode::Both if row.line == cursor_line => row.line + 1,
                _ => row.line.abs_diff(cursor_line),
            };
            let digits = n.to_string();
            let group = if row.line == cursor_line { "CursorLineNr" } else { "LineNr" };
            for (i, ch) in digits.chars().enumerate() {
                let x = (gutter - 1).saturating_sub(digits.len()) + i;
                if x < width {
                    cells[x].text = ch.to_string();
                    style("LineNr", &mut cells[x]);
                    style(group, &mut cells[x]);
                }
            }
        }
        // The continuation prefix: showbreak, then blank indent.
        if !row.first && row.prefix > 0 {
            for (i, g) in unicode_segmentation::UnicodeSegmentation::graphemes(view.layout.showbreak.as_str(), true)
                .enumerate()
                .take(row.prefix)
            {
                let x = gutter + i;
                if x < width {
                    cells[x].text = g.to_owned();
                    style("NonText", &mut cells[x]);
                }
            }
        }
        for g in &row.glyphs {
            let Some(x) = (g.col + gutter).checked_sub(view.left) else { continue };
            if x < gutter || x + g.width > width {
                continue;
            }
            let mut cell = base.clone();
            for m in &marks {
                if m.start <= g.offset
                    && g.offset < m.end
                    && let Some(name) = &m.spec.style
                {
                    style(name, &mut cell);
                }
            }
            let selected = match (selection, block) {
                // Columns count from the row's start: block selections are
                // meant for views that do not wrap.
                (_, Some((l0, l1, c0, c1))) => {
                    (l0..=l1).contains(&row.line) && (c0..c1).contains(&(g.col - row.prefix))
                }
                (Some((a, b, SelectionKind::Char)), _) => a <= g.offset && g.offset < b,
                (Some((a, b, SelectionKind::Line)), _) => (text.line_of(a)..=text.line_of(b)).contains(&row.line),
                _ => false,
            };
            if selected {
                style("Selection", &mut cell);
            }
            match &g.kind {
                GlyphKind::Text(t) => {
                    cell.text = t.clone();
                    cell.width = g.width as u8;
                    cells[x] = cell.clone();
                    if g.width == 2 {
                        cells[x + 1] = Cell { text: String::new(), width: 0, ..cell };
                    }
                }
                GlyphKind::Tab => {
                    for c in &mut cells[x..x + g.width] {
                        *c = cell.blanked();
                    }
                }
                GlyphKind::Control(c) => {
                    style("SpecialKey", &mut cell);
                    cells[x] = Cell { text: "^".into(), width: 1, ..cell.clone() };
                    let shown = char::from_u32((*c as u32) ^ 0x40).unwrap_or('?');
                    cells[x + 1] = Cell { text: shown.to_string(), width: 1, ..cell };
                }
            }
        }
        // The cursor.
        if row.contains(view.cursor.pos) {
            let col = row.col_of(view.cursor.pos) + gutter;
            if let Some(x) = col.checked_sub(view.left).map(|x| x.min(width.saturating_sub(1)))
                && x >= gutter
            {
                if focused {
                    drawn.cursor = Some((x as u16, y as u16));
                } else {
                    style("CursorUnfocused", &mut cells[x]);
                }
            }
        }
        temzdf_scene::mend_wide(&mut cells);
        drawn.rows.push(cells);
    }
    while drawn.rows.len() < height {
        let mut cells = vec![Cell::blank(); width];
        if let Some(c) = cells.first_mut() {
            c.text = "~".into();
            style("NonText", c);
        }
        drawn.rows.push(cells);
    }
    drawn
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::Cursor;
    use crate::marks::MarkSpec;
    use crate::text::{LineEnding, Text};

    fn line(d: &Drawn, y: usize) -> String {
        d.rows[y]
            .iter()
            .map(|c| {
                if c.width == 0 {
                    ""
                } else if c.text.is_empty() {
                    " "
                } else {
                    &c.text
                }
            })
            .collect()
    }

    #[test]
    fn rows_numbers_and_the_cursor() {
        let buf = Buffer::new(Text::from("one\nあい\n\tx"), LineEnding::Lf);
        let mut v = View { width: 10, height: 4, number: NumberMode::Both, ..View::default() };
        v.layout.tabstop = 4;
        v.cursor = Cursor::at(5);
        let d = render(&buf, &v, &Highlights::default(), None, true);
        // Three columns of numbers (at least) and a space; the cursor's
        // line absolute, the others relative.
        assert_eq!(line(&d, 0), "  1 one   ");
        assert_eq!(line(&d, 1), "  2 あい  ");
        assert_eq!(line(&d, 2), "  1     x ");
        assert_eq!(line(&d, 3), "~         ");
        assert_eq!(d.cursor, Some((6, 1)), "after あ, two columns wide");
        let unfocused = render(&buf, &v, &Highlights::default(), None, false);
        assert_eq!(unfocused.cursor, None);
        assert_eq!(unfocused.rows[1][6].attrs.underline, Underline::Single, "drawn in the cell instead");
    }

    #[test]
    fn decorations_then_the_selection_on_top() {
        let mut buf = Buffer::new(Text::from("abcdef"), LineEnding::Lf);
        let mut hl = Highlights::default();
        hl.set("Search", Style { bg: Some(Color::Indexed(3)), ..Style::default() });
        buf.marks_mut().add(1, 4, MarkSpec { style: Some("Search".into()), ..MarkSpec::default() });
        buf.marks_mut().add(0, 6, MarkSpec { style: Some("Search".into()), pane: Some(9), ..MarkSpec::default() });
        let mut v = View { width: 8, height: 1, ..View::default() };
        v.cursor = Cursor { pos: 3, anchor: Some((2, SelectionKind::Char)), want_col: None };
        let d = render(&buf, &v, &hl, Some(1), true);
        let r = &d.rows[0];
        assert_eq!(
            (r[0].bg, r[1].bg, r[3].bg, r[4].bg),
            (Color::Default, Color::Indexed(3), Color::Indexed(3), Color::Default)
        );
        assert!(r[2].attrs.inverse && !r[3].attrs.inverse, "the selection excludes the active end");
        assert_eq!(r[5].bg, Color::Default, "another pane's decoration is not shown");
    }

    #[test]
    fn control_characters_and_wrapped_rows() {
        let buf = Buffer::new(Text::from("a\x01bcdef"), LineEnding::Lf);
        let mut v = View { width: 4, height: 3, ..View::default() };
        v.layout.showbreak = "+".into();
        let d = render(&buf, &v, &Highlights::default(), None, true);
        assert_eq!(line(&d, 0), "a^Ab");
        assert_eq!(line(&d, 1), "+cde");
        assert_eq!(line(&d, 2), "+f  ");
    }
}
