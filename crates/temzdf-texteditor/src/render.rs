//! Drawing a view into text cells: the gutter, the text, decorations, the
//! selection and the cursor line, styled from highlight groups.

use std::collections::HashMap;

use temzdf_scene::{Attrs, Cell, Color, Underline};

use crate::buffer::Buffer;
use crate::cursor::SelectionKind;
use crate::layout::GlyphKind;
use crate::marks::{Mark, VirtPos};
use crate::search::{Pattern, SearchOptions, find_all};
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
            ("Search", Style { fg: Some(Color::Indexed(0)), bg: Some(Color::Indexed(3)), attrs: None }),
            ("IncSearch", Style { attrs: attrs(|a| a.inverse = true), ..Style::default() }),
            ("NonText", dim),
            ("SpecialKey", Style { fg: Some(Color::Indexed(4)), ..Style::default() }),
            // A prompt's virtual text (the theme's accent).
            ("Prompt", Style { fg: Some(Color::Rgb([0x61, 0xaf, 0xef])), ..Style::default() }),
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

/// Where an image goes in a drawn view: its cells (the view's, gutter
/// included), and the text shown instead where images cannot be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawnImage {
    pub id: u64,
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
    pub alt: String,
}

/// A drawn view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Drawn {
    /// `height` rows of `width` cells.
    pub rows: Vec<Vec<Cell>>,
    /// The images over them (their cells blank below).
    pub images: Vec<DrawnImage>,
    /// Where the cursor is (column, row), when it is shown and the pane has
    /// the focus.
    pub cursor: Option<(u16, u16)>,
}

/// Draw `view` of `buf`. `pane` picks the decorations shown only in it;
/// `focused` decides how the cursor is shown. `patterns` paint their
/// matches in what is shown (`highlightPattern`: hlsearch), under
/// decorations.
pub fn render(
    buf: &Buffer,
    view: &View,
    hl: &Highlights,
    pane: Option<u32>,
    focused: bool,
    patterns: &[(&Pattern, &str)],
) -> Drawn {
    let text = buf.text();
    let (width, height) = (view.width as usize, view.height as usize);
    let gutter = view.gutter(text.line_count());
    let rows = view.rows(buf);
    let (vis_start, vis_end) = (view.top, rows.last().map_or(view.top, |r| r.end));
    let marks: Vec<_> = buf
        .marks()
        .overlapping(vis_start, vis_end)
        .into_iter()
        .filter(|m| m.spec.style.is_some() && m.start < m.end && m.spec.pane.is_none_or(|p| Some(p) == pane))
        .collect();
    // Virtual text drawn over the rows (end of line, overlay).
    let virt: Vec<&Mark> = buf
        .marks()
        .overlapping(vis_start, vis_end)
        .into_iter()
        .filter(|m| {
            m.spec.virt_pos != VirtPos::Inline
                && !m.spec.virt_text.is_empty()
                && !m.deleted
                && m.spec.pane.is_none_or(|p| Some(p) == pane)
        })
        .collect();
    // Matches of the patterns in the rows shown.
    let painted: Vec<(usize, usize, &str)> = patterns
        .iter()
        .flat_map(|(p, style)| {
            find_all(text, p, vis_start, vis_end, &SearchOptions::default(), &buf.wordchars)
                .into_iter()
                .filter(|m| m.start < m.end)
                .map(move |m| (m.start, m.end, *style))
        })
        .collect();
    let cursor_line = text.line_of(view.cursor.pos);
    let selection = view.cursor.selection();
    // A block selection's display columns.
    let block = selection.filter(|s| s.2 == SelectionKind::Block).map(|(a, b, _)| {
        let l = view.layouter(buf);
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
            // An image: blank cells, the image over them.
            if let GlyphKind::Image { id, alt } = &g.kind {
                for c in &mut cells[x..x + g.width] {
                    *c = base.blanked();
                }
                drawn.images.push(DrawnImage {
                    id: *id,
                    col: x as u16,
                    row: y as u16,
                    cols: g.width as u16,
                    rows: 1,
                    alt: alt.clone(),
                });
                continue;
            }
            // Inline virtual text: its own style only.
            if let GlyphKind::Virtual { text, style: vs } = &g.kind {
                let mut cell = base.clone();
                if let Some(name) = vs {
                    style(name, &mut cell);
                }
                put(&mut cells, x, text, g.width, cell);
                continue;
            }
            let mut cell = base.clone();
            for (a, b, name) in &painted {
                if *a <= g.offset && g.offset < *b {
                    style(name, &mut cell);
                }
            }
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
                // Drawn above.
                GlyphKind::Virtual { .. } | GlyphKind::Image { .. } | GlyphKind::Concealed => {}
            }
        }
        // Virtual text at the end of the line (its last row) and over the
        // text.
        let mut eol = row.glyphs.last().map_or(row.prefix, |g| g.col + g.width) + 1;
        for m in &virt {
            let at = match m.spec.virt_pos {
                VirtPos::Eol if !row.wrapped && text.line_of(m.start) == row.line => {
                    let at = eol;
                    eol += m.spec.virt_text.iter().map(|c| text_width(&c.text)).sum::<usize>() + 1;
                    at
                }
                VirtPos::Overlay if row.contains(m.start) => row.col_of(m.start),
                _ => continue,
            };
            let mut col = at + gutter;
            for chunk in &m.spec.virt_text {
                let mut cell = base.clone();
                if let Some(name) = &chunk.style {
                    style(name, &mut cell);
                }
                for g in unicode_segmentation::UnicodeSegmentation::graphemes(chunk.text.as_str(), true) {
                    if g.chars().any(char::is_control) {
                        continue;
                    }
                    let w = crate::layout::grapheme_width(g);
                    if let Some(x) = col.checked_sub(view.left)
                        && x >= gutter
                        && x + w <= width
                    {
                        put(&mut cells, x, g, w, cell.clone());
                    }
                    col += w;
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

/// Put a grapheme `w` columns wide at `x` (a wide one takes the next cell
/// too); a wide character it covers half of is mended after.
fn put(cells: &mut [Cell], x: usize, text: &str, w: usize, cell: Cell) {
    cells[x] = Cell { text: text.to_owned(), width: w as u8, ..cell.clone() };
    if w == 2 && x + 1 < cells.len() {
        cells[x + 1] = Cell { text: String::new(), width: 0, ..cell };
    }
}

fn text_width(s: &str) -> usize {
    unicode_segmentation::UnicodeSegmentation::graphemes(s, true)
        .filter(|g| !g.chars().any(char::is_control))
        .map(crate::layout::grapheme_width)
        .sum()
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
        let d = render(&buf, &v, &Highlights::default(), None, true, &[]);
        // Three columns of numbers (at least) and a space; the cursor's
        // line absolute, the others relative.
        assert_eq!(line(&d, 0), "  1 one   ");
        assert_eq!(line(&d, 1), "  2 あい  ");
        assert_eq!(line(&d, 2), "  1     x ");
        assert_eq!(line(&d, 3), "~         ");
        assert_eq!(d.cursor, Some((6, 1)), "after あ, two columns wide");
        let unfocused = render(&buf, &v, &Highlights::default(), None, false, &[]);
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
        let d = render(&buf, &v, &hl, Some(1), true, &[]);
        let r = &d.rows[0];
        assert_eq!(
            (r[0].bg, r[1].bg, r[3].bg, r[4].bg),
            (Color::Default, Color::Indexed(3), Color::Indexed(3), Color::Default)
        );
        assert!(r[2].attrs.inverse && !r[3].attrs.inverse, "the selection excludes the active end");
        assert_eq!(r[5].bg, Color::Default, "another pane's decoration is not shown");
    }

    #[test]
    fn patterns_paint_their_matches_under_decorations() {
        let mut buf = Buffer::new(Text::from("foo bar foo"), LineEnding::Lf);
        let mut hl = Highlights::default();
        hl.set("Search", Style { bg: Some(Color::Indexed(3)), ..Style::default() });
        hl.set("Mark", Style { bg: Some(Color::Indexed(5)), ..Style::default() });
        buf.marks_mut().add(8, 9, MarkSpec { style: Some("Mark".into()), ..MarkSpec::default() });
        let v = View { width: 12, height: 1, ..View::default() };
        let p = Pattern::new("foo").unwrap();
        let d = render(&buf, &v, &hl, None, false, &[(&p, "Search")]);
        let bg: Vec<Color> = d.rows[0].iter().map(|c| c.bg).collect();
        assert_eq!(&bg[..4], [Color::Indexed(3), Color::Indexed(3), Color::Indexed(3), Color::Default]);
        assert_eq!(bg[8], Color::Indexed(5), "a decoration over the match");
        assert_eq!(bg[9], Color::Indexed(3));
    }

    #[test]
    fn control_characters_and_wrapped_rows() {
        let buf = Buffer::new(Text::from("a\x01bcdef"), LineEnding::Lf);
        let mut v = View { width: 4, height: 3, ..View::default() };
        v.layout.showbreak = "+".into();
        let d = render(&buf, &v, &Highlights::default(), None, true, &[]);
        assert_eq!(line(&d, 0), "a^Ab");
        assert_eq!(line(&d, 1), "+cde");
        assert_eq!(line(&d, 2), "+f  ");
    }

    fn virt(text: &str, pos: crate::marks::VirtPos, style: Option<&str>) -> MarkSpec {
        MarkSpec {
            virt_text: vec![crate::marks::VirtChunk { text: text.into(), style: style.map(str::to_owned) }],
            virt_pos: pos,
            ..MarkSpec::default()
        }
    }

    #[test]
    fn virtual_text_inline_at_the_end_and_over_the_text() {
        use crate::marks::VirtPos;
        let mut buf = Buffer::new(Text::from("abcd\nあいう"), LineEnding::Lf);
        let mut hl = Highlights::default();
        hl.set("Comment", Style { fg: Some(Color::Indexed(4)), ..Style::default() });
        buf.marks_mut().add(0, 0, virt("> ", VirtPos::Inline, Some("Comment")));
        buf.marks_mut().add(2, 2, virt("hint", VirtPos::Eol, None));
        buf.marks_mut().add(1, 1, virt("ZZ", VirtPos::Overlay, None));
        // Over the first half of あ: the rest of it goes (no half
        // characters).
        buf.marks_mut().add(5, 5, virt("Q", VirtPos::Overlay, None));
        let mut v = View { width: 16, height: 2, ..View::default() };
        v.cursor = Cursor::at(0);
        let d = render(&buf, &v, &hl, None, true, &[]);
        assert_eq!(line(&d, 0), "> aZZd hint     ");
        assert_eq!(d.rows[0][0].fg, Color::Indexed(4), "in its style");
        assert_eq!(d.cursor, Some((2, 0)), "after the inline text");
        assert_eq!(line(&d, 1), "Q いう          ");
        // Another pane's is not shown, and takes no columns there.
        buf.marks_mut().add(3, 3, MarkSpec { pane: Some(9), ..virt("P", VirtPos::Inline, None) });
        let d = render(&buf, &v, &hl, None, true, &[]);
        assert_eq!(line(&d, 0), "> aZZd hint     ");
        let mut v9 = View { pane: Some(9), ..v.clone() };
        v9.cursor = Cursor::at(0);
        let d = render(&buf, &v9, &hl, Some(9), true, &[]);
        assert_eq!(line(&d, 0), "> aZZPd hint    ");
    }

    #[test]
    fn images_leave_their_cells_blank_and_say_where_they_go() {
        use crate::marks::{VirtImage, VirtPos};
        let mut buf = Buffer::new(Text::from("ab"), LineEnding::Lf);
        let img = VirtImage { id: 7, width: 40, height: 20, alt: "pic".into(), max_cols: None, missing: false };
        buf.marks_mut().add(1, 1, MarkSpec { virt_image: Some(img), virt_pos: VirtPos::Inline, ..MarkSpec::default() });
        let mut v = View { width: 10, height: 1, ..View::default() };
        v.cursor = Cursor::at(0);
        let d = render(&buf, &v, &Highlights::default(), None, true, &[]);
        assert_eq!(line(&d, 0), "a    b    ");
        assert_eq!(d.images, [DrawnImage { id: 7, col: 1, row: 0, cols: 4, rows: 1, alt: "pic".into() }]);
        // Gone: its text instead, no image.
        buf.marks_mut().set_image(7, None);
        let d = render(&buf, &v, &Highlights::default(), None, true, &[]);
        assert_eq!(line(&d, 0), "apicb     ");
        assert!(d.images.is_empty());
        // Back, another size.
        buf.marks_mut().set_image(7, Some((20, 20)));
        let d = render(&buf, &v, &Highlights::default(), None, true, &[]);
        assert_eq!(d.images[0].cols, 2);
    }
}
