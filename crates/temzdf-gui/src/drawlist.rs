//! A frame of the scene as an ordered list of things to draw, decided here
//! so the GPU stage only executes it (and this part is tested without a
//! GPU). The order follows kitty's rules, which the scene was made for:
//!
//! 1. the window's background (the default background colour);
//! 2. image layers with z below -2^30 (under cell backgrounds: pages,
//!    the session's chrome);
//! 3. cell backgrounds that are not the default;
//! 4. layers with z from -2^30 to 0 (between backgrounds and text);
//! 5. glyphs, then underlines and strike-throughs, then the cursor (and
//!    the character under a block cursor, in the background colour);
//! 6. layers with z of 0 and more (over the text: programs' images).
//!
//! Text grids are laid over each other in order, the later winning, as on a
//! terminal screen.

use temzdf_scene::{Attrs, Cell, CursorShape, Frame, ImageKey, Rect, Underline};

use crate::palette::Colors;

/// Layers below this z go under cell backgrounds (kitty's rule).
const UNDER_BACKGROUNDS: i32 = -1_073_741_824;

/// The pixel grid text cells sit on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u32,
    pub cell_h: u32,
}

impl Grid {
    /// Cells (`col`, `row`, `w` wide) in pixels.
    pub fn cells(&self, col: u16, row: u16, w: u32) -> Rect {
        Rect::new(col as i32 * self.cell_w as i32, row as i32 * self.cell_h as i32, w * self.cell_w, self.cell_h)
    }
}

/// An image layer to draw: `src` of the image scaled into `dst` (pixels).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    pub image: ImageKey,
    pub src: Rect,
    pub dst: Rect,
}

/// A rectangle of one colour (RGBA, straight alpha).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub rect: Rect,
    pub color: [u8; 4],
}

/// A character in its cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub col: u16,
    pub row: u16,
    pub text: String,
    /// Cells it spans (2 for wide characters).
    pub width: u8,
    pub color: [u8; 3],
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrawList {
    pub background: [u8; 3],
    pub under: Vec<Image>,
    pub cells: Vec<Fill>,
    pub between: Vec<Image>,
    pub glyphs: Vec<Glyph>,
    /// Underlines, strike-throughs and overlines.
    pub marks: Vec<Fill>,
    pub cursor: Option<Fill>,
    /// The character under a block cursor, drawn over it.
    pub cursor_glyph: Option<Glyph>,
    /// Glyphs over the marks (an input method's text).
    pub over_glyphs: Vec<Glyph>,
    pub over: Vec<Image>,
}

/// The screen's cells after laying the frame's text grids over each other.
fn screen(frame: &Frame, grid: Grid) -> Vec<Option<Cell>> {
    let (cols, rows) = (grid.cols as usize, grid.rows as usize);
    let mut cells: Vec<Option<Cell>> = vec![None; cols * rows];
    for g in &frame.text {
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
                cells[y * cols + x] = Some(cell.clone());
            }
        }
    }
    cells
}

/// Halfway between two colours (faint text).
fn mix(a: [u8; 3], b: [u8; 3]) -> [u8; 3] {
    [0, 1, 2].map(|i| ((a[i] as u16 + b[i] as u16) / 2) as u8)
}

/// The foreground and background a cell shows (inverse and faint applied).
fn cell_colors(c: &Cell, colors: &Colors) -> ([u8; 3], [u8; 3], bool) {
    let mut fg = colors.resolve(c.fg, colors.foreground);
    let mut bg = colors.resolve(c.bg, colors.background);
    let mut default_bg = c.bg == temzdf_scene::Color::Default;
    if c.attrs.inverse {
        std::mem::swap(&mut fg, &mut bg);
        default_bg = false;
    }
    if c.attrs.faint {
        fg = mix(fg, bg);
    }
    (fg, bg, default_bg)
}

/// Underline, strike-through and overline marks of a cell, in pixels.
fn decorations(a: &Attrs, rect: Rect, color: [u8; 3]) -> Vec<Fill> {
    let thick = (rect.h / 14).max(1);
    let line = |y: i32| Fill { rect: Rect::new(rect.x, y, rect.w, thick), color: [color[0], color[1], color[2], 255] };
    let bottom = rect.y + rect.h as i32 - thick as i32 * 2;
    let mut out = Vec::new();
    match a.underline {
        Underline::None => {}
        Underline::Double => {
            out.push(line(bottom));
            out.push(line(bottom - thick as i32 * 2));
        }
        Underline::Single => out.push(line(bottom)),
        Underline::Curly => out.extend(curly(rect, bottom, thick, color)),
        Underline::Dotted => out.extend(dashes(rect, bottom, thick, thick.max(2), color)),
        Underline::Dashed => out.extend(dashes(rect, bottom, thick, (rect.h / 4).max(3), color)),
    }
    if a.strikethrough {
        out.push(line(rect.y + rect.h as i32 / 2));
    }
    if a.overline {
        out.push(line(rect.y));
    }
    out
}

/// Pieces `on` pixels long with gaps as long, on a line `thick` high at
/// `y`. They follow the window's x, not the cell's, so neighbouring cells
/// continue each other's pattern.
fn dashes(rect: Rect, y: i32, thick: u32, on: u32, color: [u8; 3]) -> Vec<Fill> {
    let period = on as i32 * 2;
    let mut out = Vec::new();
    let mut x = rect.x;
    let end = rect.x + rect.w as i32;
    while x < end {
        let phase = x.rem_euclid(period);
        let stop = (x - phase + if phase < on as i32 { on as i32 } else { period }).min(end);
        if phase < on as i32 {
            out.push(Fill {
                rect: Rect::new(x, y, (stop - x) as u32, thick),
                color: [color[0], color[1], color[2], 255],
            });
        }
        x = stop;
    }
    out
}

/// A wave `thick` high whose lowest point is the underline's place `y`,
/// one period a cell height long, following the window's x like `dashes`.
/// Each pixel column reaches its neighbour's height so a thin wave has no
/// holes; columns of the same span are merged.
fn curly(rect: Rect, y: i32, thick: u32, color: [u8; 3]) -> Vec<Fill> {
    let amp = (thick * 2).max(2) as f64;
    let period = rect.h.max(4) as f64 / 2.0;
    let top = |x: i32| {
        let s = (x as f64 / period * std::f64::consts::TAU).sin();
        y - (amp * (1.0 + s) / 2.0).round() as i32
    };
    let mut out: Vec<Fill> = Vec::new();
    for x in rect.x..rect.x + rect.w as i32 {
        let (a, b) = (top(x), top(x + 1));
        let (from, to) = (a.min(b), a.max(b) + thick as i32);
        match out.last_mut() {
            Some(f) if f.rect.y == from && f.rect.h == (to - from) as u32 && f.rect.x + f.rect.w as i32 == x => {
                f.rect.w += 1;
            }
            _ => out.push(Fill {
                rect: Rect::new(x, from, 1, (to - from) as u32),
                color: [color[0], color[1], color[2], 255],
            }),
        }
    }
    out
}

pub fn build(frame: &Frame, grid: Grid, colors: &Colors) -> DrawList {
    let mut list = DrawList { background: colors.background, ..DrawList::default() };
    let mut layers = frame.layers.clone();
    layers.sort_by_key(|l| l.z);
    for l in &layers {
        let image = Image { image: l.image, src: l.src, dst: l.dst };
        match l.z {
            z if z < UNDER_BACKGROUNDS => list.under.push(image),
            z if z < 0 => list.between.push(image),
            _ => list.over.push(image),
        }
    }
    let cells = screen(frame, grid);
    for row in 0..grid.rows {
        for col in 0..grid.cols {
            let Some(c) = &cells[row as usize * grid.cols as usize + col as usize] else { continue };
            if c.width == 0 {
                continue;
            }
            let (fg, bg, default_bg) = cell_colors(c, colors);
            let rect = grid.cells(col, row, c.width.max(1) as u32);
            if !default_bg {
                list.cells.push(Fill { rect, color: [bg[0], bg[1], bg[2], 255] });
            }
            if !c.text.is_empty() && c.text != " " && !c.attrs.invisible {
                list.glyphs.push(Glyph {
                    col,
                    row,
                    text: c.text.clone(),
                    width: c.width.max(1),
                    color: fg,
                    bold: c.attrs.bold,
                    italic: c.attrs.italic,
                });
            }
            list.marks.extend(decorations(&c.attrs, rect, fg));
        }
    }
    if let Some((col, row)) = frame.cursor.filter(|&(c, r)| c < grid.cols && r < grid.rows) {
        let cell = grid.cells(col, row, 1);
        let fg = colors.foreground;
        let color = [fg[0], fg[1], fg[2], 255];
        let bar = (grid.cell_w / 8).max(2);
        let rect = match frame.cursor_shape {
            CursorShape::Bar { .. } => Rect::new(cell.x, cell.y, bar, cell.h),
            CursorShape::Underline { .. } => Rect::new(cell.x, cell.y + (cell.h - bar) as i32, cell.w, bar),
            // A block shows the character under it in the background colour.
            CursorShape::Block { .. } | CursorShape::Default => {
                if let Some(i) = list.glyphs.iter().position(|g| (g.col, g.row) == (col, row)) {
                    let g = list.glyphs.remove(i);
                    list.cursor_glyph = Some(Glyph { color: colors.background, ..g });
                }
                cell
            }
        };
        list.cursor = Some(Fill { rect, color });
    }
    list
}

/// Text an input method is composing, drawn from the text cursor's cell
/// over whatever is there (the program sees it only once committed):
/// inverse colours, underlined, wide characters over two cells, cut at the
/// right edge. The cursor goes after it.
pub fn preedit(list: &mut DrawList, text: &str, at: (u16, u16), grid: Grid, colors: &Colors) {
    let (mut col, row) = at;
    if text.is_empty() || row >= grid.rows {
        return;
    }
    let (fg, bg) = (colors.background, colors.foreground);
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1).clamp(1, 2) as u16;
        if col + w > grid.cols {
            break;
        }
        let rect = grid.cells(col, row, w as u32);
        list.marks.push(Fill { rect, color: [bg[0], bg[1], bg[2], 255] });
        list.marks.extend(decorations(&Attrs { underline: Underline::Single, ..Attrs::default() }, rect, fg));
        // Drawn with the cursor's character, above the marks.
        list.over_glyphs.push(Glyph {
            col,
            row,
            text: c.to_string(),
            width: w as u8,
            color: fg,
            bold: false,
            italic: false,
        });
        col += w;
    }
    let cell = grid.cells(col.min(grid.cols.saturating_sub(1)), row, 1);
    let bar = (grid.cell_w / 8).max(2);
    list.cursor = Some(Fill { rect: Rect::new(cell.x, cell.y, bar, cell.h), color: [bg[0], bg[1], bg[2], 255] });
    list.cursor_glyph = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use temzdf_scene::{Color, Layer, TextGrid};

    const GRID: Grid = Grid { cols: 10, rows: 3, cell_w: 8, cell_h: 16 };

    fn cell(text: &str, width: u8) -> Cell {
        Cell { text: text.into(), width, ..Cell::default() }
    }

    fn layer(image: ImageKey, z: i32) -> Layer {
        Layer { key: 0, image, src: Rect::new(0, 0, 1, 1), dst: Rect::new(0, 0, 10, 10), z }
    }

    #[test]
    fn layers_go_under_backgrounds_between_or_over_text_by_z() {
        let mut store = temzdf_scene::Images::new();
        let k: Vec<ImageKey> = (0..4).map(|_| store.add(1, 1, vec![0; 4])).collect();
        let frame = Frame {
            layers: vec![layer(k[0], 5), layer(k[1], -1_073_741_900), layer(k[2], -10), layer(k[3], -1_073_741_800)],
            ..Frame::default()
        };
        let list = build(&frame, GRID, &Colors::default());
        let keys = |v: &[Image]| v.iter().map(|i| i.image).collect::<Vec<_>>();
        assert_eq!(keys(&list.under), [k[1]]);
        // -1_073_741_800 is above -2^30: between (the session's chrome).
        assert_eq!(keys(&list.between), [k[3], k[2]], "lowest first");
        assert_eq!(keys(&list.over), [k[0]]);
    }

    #[test]
    fn cells_become_backgrounds_glyphs_and_marks() {
        let colors = Colors::default();
        let red = Cell { fg: Color::Indexed(1), bg: Color::Rgb([0, 0, 255]), ..cell("a", 1) };
        let inverse = Cell { attrs: Attrs { inverse: true, ..Attrs::default() }, ..cell("b", 1) };
        let under = Cell { attrs: Attrs { underline: Underline::Single, ..Attrs::default() }, ..cell("c", 1) };
        let hidden = Cell { attrs: Attrs { invisible: true, ..Attrs::default() }, ..cell("d", 1) };
        let frame = Frame {
            text: vec![
                TextGrid { col: 0, row: 0, rows: vec![vec![red, inverse, under, hidden, cell("数", 2), cell("", 0)]] },
                // A later grid wins where they overlap.
                TextGrid { col: 2, row: 0, rows: vec![vec![cell("z", 1)]] },
            ],
            ..Frame::default()
        };
        let list = build(&frame, GRID, &colors);
        let glyphs: Vec<(&str, u16, u8, [u8; 3])> =
            list.glyphs.iter().map(|g| (g.text.as_str(), g.col, g.width, g.color)).collect();
        assert_eq!(
            glyphs,
            [
                ("a", 0, 1, [0xcd, 0, 0]),
                ("b", 1, 1, colors.background),
                ("z", 2, 1, colors.foreground),
                ("数", 4, 2, colors.foreground),
            ],
            "the invisible d and the spacer draw nothing"
        );
        // Backgrounds: the blue one and the inverse one only.
        let fills: Vec<(Rect, [u8; 4])> = list.cells.iter().map(|f| (f.rect, f.color)).collect();
        let fg = colors.foreground;
        assert_eq!(
            fills,
            [(Rect::new(0, 0, 8, 16), [0, 0, 255, 255]), (Rect::new(8, 0, 8, 16), [fg[0], fg[1], fg[2], 255])]
        );
        assert!(list.marks.is_empty(), "the underlined c was written over by z");
    }

    #[test]
    fn underline_styles_continue_across_cells_and_stay_inside_them() {
        let rect = |col| GRID.cells(col, 1, 1);
        let marks = |u, col| decorations(&Attrs { underline: u, ..Attrs::default() }, rect(col), [1, 2, 3]);
        // Every pixel column of the cell is covered by the wave, and none
        // leaves the cell.
        for col in 0..4 {
            let wave = marks(Underline::Curly, col);
            let covered: u32 = wave.iter().map(|f| f.rect.w).sum();
            assert_eq!(covered, GRID.cell_w, "column {col}");
            let cell = rect(col);
            for f in &wave {
                assert!(f.rect.y >= cell.y && f.rect.y + f.rect.h as i32 <= cell.y + cell.h as i32, "{f:?}");
            }
            assert!(wave.iter().map(|f| f.rect.y).collect::<std::collections::HashSet<_>>().len() > 1, "it waves");
        }
        // Two cells drawn apart are the same as one wide cell: the
        // pattern follows the window's x (cells 7 wide, which no period
        // divides).
        let odd = Grid { cell_w: 7, ..GRID };
        let pixels = |v: &[Fill]| v.iter().flat_map(|f| f.rect.x..f.rect.x + f.rect.w as i32).collect::<Vec<_>>();
        for u in [Underline::Dotted, Underline::Dashed, Underline::Curly] {
            let at = |r| decorations(&Attrs { underline: u, ..Attrs::default() }, r, [1, 2, 3]);
            let wide = at(odd.cells(0, 1, 2));
            let apart = [at(odd.cells(0, 1, 1)), at(odd.cells(1, 1, 1))].concat();
            let spans = |v: &[Fill]| {
                v.iter()
                    .flat_map(|f| (f.rect.x..f.rect.x + f.rect.w as i32).map(|x| (x, f.rect.y, f.rect.h)))
                    .collect::<Vec<_>>()
            };
            assert_eq!(spans(&wide), spans(&apart), "{u:?}");
        }
        // Dots and dashes leave gaps; dashes are longer.
        let dotted = marks(Underline::Dotted, 0);
        let dashed = marks(Underline::Dashed, 0);
        assert!(pixels(&dotted).len() < GRID.cell_w as usize && !dotted.is_empty());
        assert!(dashed.iter().map(|f| f.rect.w).max() > dotted.iter().map(|f| f.rect.w).max());
    }

    #[test]
    fn preedit_text_is_drawn_from_the_cursor_and_cut_at_the_edge() {
        let colors = Colors::default();
        let mut list = DrawList::default();
        preedit(&mut list, "にほんg", (5, 1), GRID, &colors);
        let glyphs: Vec<(&str, u16, u8)> = list.over_glyphs.iter().map(|g| (g.text.as_str(), g.col, g.width)).collect();
        assert_eq!(glyphs, [("に", 5, 2), ("ほ", 7, 2)], "ん would pass the edge (10 columns)");
        assert_eq!(list.marks.iter().filter(|m| m.rect.h == 16).count(), 2, "a background per character");
        assert_eq!(list.cursor.map(|c| c.rect.x), Some(9 * 8), "after the text");
    }

    #[test]
    fn a_block_cursor_shows_its_character_in_the_background_colour() {
        let colors = Colors::default();
        let frame = Frame {
            text: vec![TextGrid { col: 0, row: 1, rows: vec![vec![cell("x", 1), cell("y", 1)]] }],
            cursor: Some((1, 1)),
            cursor_shape: CursorShape::Block { blink: false },
            ..Frame::default()
        };
        let list = build(&frame, GRID, &colors);
        assert_eq!(list.cursor.map(|c| c.rect), Some(Rect::new(8, 16, 8, 16)));
        assert_eq!(list.cursor_glyph.as_ref().map(|g| (g.text.as_str(), g.color)), Some(("y", colors.background)));
        assert_eq!(list.glyphs.len(), 1, "y is drawn over the cursor, not under it");
        let bar = build(&Frame { cursor_shape: CursorShape::Bar { blink: false }, ..frame }, GRID, &colors);
        assert_eq!(bar.cursor.map(|c| c.rect), Some(Rect::new(8, 16, 2, 16)));
        assert!(bar.cursor_glyph.is_none());
    }
}
