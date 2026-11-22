//! `temzdf render-session`: attach to a session as a terminal client would,
//! take its frame once it settled, and save what a terminal shows: the
//! images (through the same bytes, into the mock terminal), the text of the
//! cells, a mask of the cells whose pixels the terminal draws itself, and
//! those cells as bytes for the terminal to draw on its own. scripts/e2e.sh
//! compares a real terminal against them (`expect` in a session).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

use anyhow::{Context, Result};
use temzdf_client::{Connection, Settling, lost_message};
use temzdf_protocol::{ClientMsg, ServerMsg};
use temzdf_scene::{Attrs, Cell, Color, Frame, Hints, Rect, Underline};
use temzdf_tty::{Presenter, Screen};
use termpix::compositor::{Backend, Compositor};
use termpix::geom::Geometry;
use termpix::graphics::Medium;
use termpix::mock::MockTerminal;
use termpix::raster::{Blending, RgbaImage};

pub struct SnapshotOptions {
    /// The terminal's: the session lays out for the latest client.
    pub geometry: Geometry,
    /// The terminal client's, for the same reason.
    pub hints: Hints,
    pub linear_blending: bool,
    /// Where the text of the screen goes (one line per row).
    pub text: Option<PathBuf>,
    /// Where the mask goes: opaque where the terminal draws the cell (text,
    /// its background, the cursor), clear where the images show.
    pub mask: Option<PathBuf>,
    /// Where the cells the terminal draws go as bytes (see [`screen_ansi`]).
    pub ansi: Option<PathBuf>,
    /// Where the mask of the cells to compare with the terminal's own
    /// drawing of `ansi` goes (see [`compared_cells`]).
    pub cells: Option<PathBuf>,
}

/// Where [`write_outputs`] writes what it was asked for, besides the image.
#[derive(Clone, Copy, Default)]
pub(crate) struct Outputs<'a> {
    pub text: Option<&'a Path>,
    pub mask: Option<&'a Path>,
    pub ansi: Option<&'a Path>,
    pub cells: Option<&'a Path>,
}

/// Render session `name`'s frame into `out` (see the module).
pub fn render_session(name: &str, out: &Path, opts: &SnapshotOptions) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let conn = Connection::open(name, tx, std::convert::identity, || {})?;
    let send = |msg: ClientMsg| conn.send(&msg).with_context(|| lost_message(name));
    conn.hello(opts.geometry, opts.hints, None).with_context(|| lost_message(name))?;
    // Into the mock terminal, as inline transfers (it reads no shared memory).
    let mut comp = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, opts.geometry);
    let mut presenter = Presenter::new();
    let mut latest: Option<Arc<Frame>> = None;
    temzdf_client::settle(name, &rx, |msg| {
        let ServerMsg::Frame { seq, updates, frame, complete, .. } = msg else { return Ok(Settling::Other) };
        presenter.apply(&mut comp, updates);
        latest = Some(frame);
        send(ClientMsg::FrameAck(seq))?;
        Ok(Settling::Frame { complete })
    })?;
    let _ = send(ClientMsg::Detach);
    let frame = latest.context("no frame")?;
    let mut screen = Screen::new();
    let bytes = screen.render(&frame, &presenter, &mut comp)?.to_vec();
    let mut term = MockTerminal::new(opts.geometry, [0, 0, 0, 255]);
    if opts.linear_blending {
        term.set_blending(Blending::Linear);
    }
    term.feed(&bytes);
    let outputs = Outputs {
        text: opts.text.as_deref(),
        mask: opts.mask.as_deref(),
        ansi: opts.ansi.as_deref(),
        cells: opts.cells.as_deref(),
    };
    write_outputs(&term.render(), &frame, opts.geometry, out, outputs)
}

/// Write the rendered screen to `out`, and the text of the cells, the masks
/// and the cells' bytes where asked.
pub(crate) fn write_outputs(image: &RgbaImage, frame: &Frame, g: Geometry, out: &Path, to: Outputs) -> Result<()> {
    image.save_png(out).with_context(|| format!("writing {}", out.display()))?;
    if let Some(path) = to.text {
        std::fs::write(path, screen_text(frame, g)).with_context(|| format!("writing {}", path.display()))?;
    }
    if let Some(path) = to.mask {
        cell_mask(frame, g).save_png(path).with_context(|| format!("writing {}", path.display()))?;
    }
    if let Some(path) = to.ansi {
        std::fs::write(path, screen_ansi(frame, g)).with_context(|| format!("writing {}", path.display()))?;
    }
    if let Some(path) = to.cells {
        compared_cells(frame, g).save_png(path).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// The cells of the frame's text grids, a line per row: spaces outside them
/// and for blank cells, a wide character once (its spacer adds nothing),
/// trailing spaces dropped. What a terminal reports as its screen's text.
pub(crate) fn screen_text(frame: &Frame, g: Geometry) -> String {
    let mut out = String::new();
    for row in screen_cells(frame, g) {
        let mut line = String::new();
        for cell in row {
            match cell {
                Some(c) if c.width == 0 => {}
                Some(c) if !c.text.is_empty() => line.push_str(&c.text),
                _ => line.push(' '),
            }
        }
        let _ = writeln!(out, "{}", line.trim_end());
    }
    out
}

/// The cells of the frame's text grids on the screen, row by row (later grids
/// win where they overlap); `None` outside them.
fn screen_cells(frame: &Frame, g: Geometry) -> Vec<Vec<Option<&Cell>>> {
    let mut rows: Vec<Vec<Option<&Cell>>> = vec![vec![None; g.cols as usize]; g.rows as usize];
    for grid in &frame.text {
        for (r, cells) in grid.rows.iter().enumerate() {
            for (c, cell) in cells.iter().enumerate() {
                if let Some(slot) =
                    rows.get_mut(grid.row as usize + r).and_then(|row| row.get_mut(grid.col as usize + c))
                {
                    *slot = Some(cell);
                }
            }
        }
    }
    rows
}

/// The cells the terminal draws itself, as bytes that make a terminal draw
/// them on a cleared screen (`cat` in a shell): the cursor hidden, then each
/// such cell's position (CUP), attributes and colours (SGR) and character,
/// a wide character once, and the attributes reset at the end. No newline:
/// at the bottom row one would scroll. scripts/e2e.sh has the terminal draw
/// this and compares that with its screen during the run, inside
/// [`compared_cells`]. Written apart from temzdf-tty's own encoding, so a
/// mistake there does not show on both sides.
pub(crate) fn screen_ansi(frame: &Frame, g: Geometry) -> String {
    let mut out = String::from("\x1b[?25l");
    // Where the next character would go without a CUP, and the SGR in force.
    let mut at: Option<(usize, usize)> = None;
    let mut pen: Option<(Color, Color, Attrs)> = None;
    for (y, row) in screen_cells(frame, g).iter().enumerate() {
        for (x, cell) in row.iter().enumerate() {
            let Some(cell) = cell else { continue };
            let spacer_of_wide = cell.width == 0 && x > 0 && row[x - 1].is_some_and(|c| c.width == 2);
            if spacer_of_wide || !drawn_by_terminal(cell) {
                continue;
            }
            if at != Some((y, x)) {
                let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
            }
            let wide = cell.width == 2 && x + 1 < row.len();
            // Nothing a terminal would take as a control, nor a wide
            // character in the last column (it would wrap): a blank there.
            let text = if cell.text.is_empty() || cell.text.chars().any(char::is_control) || (cell.width == 2 && !wide)
            {
                " "
            } else {
                cell.text.as_str()
            };
            let p = (cell.fg, cell.bg, cell.attrs);
            if pen != Some(p) {
                out.push_str(&sgr_of(cell));
                pen = Some(p);
            }
            out.push_str(text);
            // After a wide character the terminal may disagree on its width.
            at = if wide { None } else { Some((y, x + 1)) };
        }
    }
    out.push_str("\x1b[0m");
    out
}

/// The SGR sequence that sets exactly `cell`'s attributes and colours.
fn sgr_of(cell: &Cell) -> String {
    let a = cell.attrs;
    let mut params = vec!["0".to_string()];
    let flags = [
        (a.bold, "1"),
        (a.faint, "2"),
        (a.italic, "3"),
        (a.blink, "5"),
        (a.inverse, "7"),
        (a.invisible, "8"),
        (a.strikethrough, "9"),
        (a.overline, "53"),
    ];
    params.extend(flags.iter().filter(|(on, _)| *on).map(|(_, code)| code.to_string()));
    let underline = match a.underline {
        Underline::None => None,
        Underline::Single => Some("4"),
        Underline::Double => Some("4:2"),
        Underline::Curly => Some("4:3"),
        Underline::Dotted => Some("4:4"),
        Underline::Dashed => Some("4:5"),
    };
    params.extend(underline.map(str::to_string));
    for (color, layer) in [(cell.fg, 0u8), (cell.bg, 10)] {
        // The 16 basic colours in their short forms (30-37 and 90-97 for the
        // foreground, 40-47 and 100-107 for the background), as temzdf
        // writes them: terminals may treat them apart from 38;5;N (bold
        // brightening).
        match color {
            Color::Default => {}
            Color::Indexed(n) if n < 8 => params.push((30 + layer + n).to_string()),
            Color::Indexed(n) if n < 16 => params.push((90 + layer + n - 8).to_string()),
            Color::Indexed(n) => params.push(format!("{};5;{n}", 38 + layer)),
            Color::Rgb([r, g, b]) => params.push(format!("{};2;{r};{g};{b}", 38 + layer)),
        }
    }
    format!("\x1b[{}m", params.join(";"))
}

/// Whether the terminal paints this cell itself (over or instead of the
/// images): text, a background or attributes that show on a blank cell.
fn drawn_by_terminal(cell: &Cell) -> bool {
    let visible_attrs = Attrs {
        underline: cell.attrs.underline,
        inverse: cell.attrs.inverse,
        strikethrough: cell.attrs.strikethrough,
        overline: cell.attrs.overline,
        ..Attrs::default()
    };
    !cell.text.is_empty() || cell.bg != Color::Default || visible_attrs != Attrs::default()
}

/// Opaque black over the cells the terminal draws, and the cursor's.
pub(crate) fn cell_mask(frame: &Frame, g: Geometry) -> RgbaImage {
    cells_where(frame, g, |_, _, _| true, true)
}

/// Opaque black over the cells to compare with the terminal's own drawing of
/// [`screen_ansi`]: [`cell_mask`]'s, but for the cursor's (the drawing hides
/// it) and for those whose background is the default over an image (the
/// image shows through there; the drawing has none).
pub(crate) fn compared_cells(frame: &Frame, g: Geometry) -> RgbaImage {
    let over_images = |col: u32, row: u32| {
        let cell = Rect::new((col * g.cell.width) as i32, (row * g.cell.height) as i32, g.cell.width, g.cell.height);
        frame.layers.iter().any(|l| l.dst.intersect(&cell).is_some())
    };
    let shows_through = |cell: &Cell| cell.bg == Color::Default && !cell.attrs.inverse;
    cells_where(frame, g, |cell, col, row| !(shows_through(cell) && over_images(col, row)), false)
}

/// Opaque black over the cells the terminal draws for which `keep` (the
/// cell, or the wide character a spacer goes with, its column and row)
/// holds, and over the cursor's with `cursor`.
fn cells_where(frame: &Frame, g: Geometry, keep: impl Fn(&Cell, u32, u32) -> bool, cursor: bool) -> RgbaImage {
    let (w, h) = (g.width_px(), g.height_px());
    let mut mask = RgbaImage { width: w, height: h, data: vec![0; (w * h * 4) as usize] };
    let mut paint = |col: u32, row: u32, alpha: u8| {
        for y in row * g.cell.height..((row + 1) * g.cell.height).min(h) {
            for x in col * g.cell.width..((col + 1) * g.cell.width).min(w) {
                let i = ((y * w + x) * 4) as usize;
                mask.data[i..i + 4].copy_from_slice(&[0, 0, 0, alpha]);
            }
        }
    };
    for (r, cells) in screen_cells(frame, g).iter().enumerate() {
        for (c, cell) in cells.iter().enumerate() {
            let Some(cell) = cell else { continue };
            // A spacer goes with its wide character.
            let wide = (c > 0 && cell.width == 0).then(|| cells[c - 1]).flatten().filter(|w| drawn_by_terminal(w));
            let owner = wide.unwrap_or(cell);
            if (drawn_by_terminal(cell) || wide.is_some()) && keep(owner, c as u32, r as u32) {
                paint(c as u32, r as u32, 255);
            }
        }
    }
    if let Some((col, row)) = frame.cursor {
        paint(col as u32, row as u32, if cursor { 255 } else { 0 });
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;
    use temzdf_scene::TextGrid;
    use termpix::geom::CellSize;

    fn cell(text: &str, width: u8) -> Cell {
        Cell { text: text.into(), width, ..Cell::default() }
    }

    fn geometry(cols: u16, rows: u16) -> Geometry {
        Geometry { cols, rows, cell: CellSize { width: 2, height: 3 } }
    }

    #[test]
    fn text_puts_grids_in_place_and_wide_characters_once() {
        let frame = Frame {
            text: vec![
                TextGrid { col: 1, row: 0, rows: vec![vec![cell("a", 1), Cell::blank(), cell("b", 1)]] },
                TextGrid { col: 0, row: 1, rows: vec![vec![cell("数", 2), cell("", 0), cell("c", 1)]] },
            ],
            ..Frame::default()
        };
        assert_eq!(screen_text(&frame, geometry(5, 3)), " a b\n数c\n\n");
    }

    #[test]
    fn mask_covers_text_backgrounds_wide_characters_and_the_cursor() {
        let coloured = Cell { bg: Color::Indexed(4), ..Cell::blank() };
        let frame = Frame {
            text: vec![TextGrid {
                col: 0,
                row: 0,
                rows: vec![vec![cell("x", 1), Cell::blank(), coloured, cell("数", 2), cell("", 0), Cell::blank()]],
            }],
            cursor: Some((5, 1)),
            ..Frame::default()
        };
        let g = geometry(6, 2);
        let mask = cell_mask(&frame, g);
        let masked = |col: u32, row: u32| {
            let (x, y) = (col * g.cell.width + 1, row * g.cell.height + 1);
            mask.data[((y * mask.width + x) * 4 + 3) as usize] == 255
        };
        let first_row: Vec<bool> = (0..6).map(|c| masked(c, 0)).collect();
        assert_eq!(first_row, [true, false, true, true, true, false]);
        assert!(masked(5, 1), "the cursor");
        assert!(!masked(4, 1));
    }

    #[test]
    fn ansi_draws_the_terminals_cells_with_their_attributes_and_hides_the_cursor() {
        let red_on_blue = Cell {
            fg: Color::Rgb([255, 0, 0]),
            bg: Color::Indexed(4),
            attrs: Attrs { bold: true, ..Attrs::default() },
            ..cell("x", 1)
        };
        let inverse =
            Cell { fg: Color::Indexed(9), attrs: Attrs { inverse: true, ..Attrs::default() }, ..Cell::blank() };
        let underlined = Cell {
            bg: Color::Indexed(200),
            attrs: Attrs { underline: Underline::Curly, ..Attrs::default() },
            ..cell("y", 1)
        };
        let frame = Frame {
            text: vec![
                TextGrid { col: 1, row: 0, rows: vec![vec![cell("a", 1), Cell::blank(), red_on_blue, cell("b", 1)]] },
                TextGrid {
                    col: 0,
                    row: 1,
                    rows: vec![vec![cell("数", 2), cell("", 0), cell("c", 1), inverse, underlined, cell("\x1b", 1)]],
                },
            ],
            cursor: Some((0, 0)),
            ..Frame::default()
        };
        assert_eq!(
            screen_ansi(&frame, geometry(6, 2)),
            concat!(
                "\x1b[?25l",
                // Blank cells in the default colours are left out.
                "\x1b[1;2H\x1b[0ma\x1b[1;4H\x1b[0;1;38;2;255;0;0;44mx\x1b[0mb",
                // After a wide character, a CUP; a control character as a blank.
                "\x1b[2;1H数\x1b[2;3Hc\x1b[0;7;91m \x1b[0;4:3;48;5;200my\x1b[0m ",
                "\x1b[0m"
            )
        );
    }

    #[test]
    fn ansi_writes_a_wide_character_in_the_last_column_as_a_blank() {
        let frame =
            Frame { text: vec![TextGrid { col: 2, row: 0, rows: vec![vec![cell("数", 2)]] }], ..Frame::default() };
        assert_eq!(screen_ansi(&frame, geometry(3, 1)), "\x1b[?25l\x1b[1;3H\x1b[0m \x1b[0m");
    }

    #[test]
    fn compared_cells_leave_out_the_cursor_and_default_backgrounds_over_images() {
        let coloured = Cell { bg: Color::Indexed(4), ..cell("x", 1) };
        let inverse = Cell { attrs: Attrs { inverse: true, ..Attrs::default() }, ..cell("i", 1) };
        let frame = Frame {
            // An image under the first two columns of row 0 and part of the
            // third.
            layers: vec![temzdf_scene::Layer {
                key: 1,
                image: temzdf_scene::Images::new().add(1, 1, vec![0u8; 4]),
                src: Rect::new(0, 0, 5, 3),
                dst: Rect::new(0, 0, 5, 3),
                z: -1,
            }],
            text: vec![
                TextGrid { col: 0, row: 0, rows: vec![vec![cell("a", 1), coloured, inverse, cell("b", 1)]] },
                TextGrid { col: 0, row: 1, rows: vec![vec![cell("c", 1), cell("d", 1)]] },
            ],
            cursor: Some((1, 1)),
            ..Frame::default()
        };
        let g = geometry(4, 2);
        let compared = compared_cells(&frame, g);
        let masked = |col: u32, row: u32| {
            let (x, y) = (col * g.cell.width + 1, row * g.cell.height + 1);
            compared.data[((y * compared.width + x) * 4 + 3) as usize] == 255
        };
        let first_row: Vec<bool> = (0..4).map(|c| masked(c, 0)).collect();
        assert_eq!(first_row, [false, true, true, true], "text over the image only with a background of its own");
        assert!(masked(0, 1), "below the image");
        assert!(!masked(1, 1), "the cursor");
    }
}
