//! `temzdf render-session`: attach to a session as a terminal client would,
//! take its frame once it settled, and save what a terminal shows: the
//! images (through the same bytes, into the mock terminal), the text of the
//! cells, and a mask of the cells whose pixels the terminal draws itself.
//! scripts/e2e.sh compares a real terminal against them (`expect` in a
//! session).

use std::fmt::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use temzdf_protocol::{self as proto, ClientMsg, ServerMsg};
use temzdf_scene::{Attrs, Cell, Color, Frame, Hints};
use temzdf_tty::{Presenter, Screen};
use termpix::compositor::{Backend, Compositor};
use termpix::geom::Geometry;
use termpix::graphics::Medium;
use termpix::mock::MockTerminal;
use termpix::raster::{Blending, RgbaImage};

/// How long the frame may keep changing after it was first complete (the
/// resize to this geometry, late tiles).
const SETTLE: Duration = Duration::from_millis(500);
/// Longest wait for a complete frame.
const TIMEOUT: Duration = Duration::from_secs(20);

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
}

/// Render session `name`'s frame into `out` (see the module).
pub fn render_session(name: &str, out: &Path, opts: &SnapshotOptions) -> Result<()> {
    let stream = UnixStream::connect(proto::socket_path(name)?).with_context(|| format!("no session {name:?}"))?;
    let (tx, rx) = mpsc::channel();
    let mut reader = proto::shared::FdReader::new(stream.try_clone()?);
    std::thread::spawn(move || {
        while let Ok(Some((msg, _))) = proto::recv_server(&mut reader) {
            if tx.send(msg).is_err() {
                return;
            }
        }
    });
    let mut writer = stream;
    let mut send = |msg: ClientMsg| proto::send(&mut writer, &msg).context("lost the session");
    send(ClientMsg::Hello {
        version: proto::VERSION,
        geometry: opts.geometry,
        hints: opts.hints,
        inside: None,
        shared_memory: proto::shared_memory_wanted(),
    })?;
    // Into the mock terminal, as inline transfers (it reads no shared memory).
    let mut comp = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, opts.geometry);
    let mut presenter = Presenter::new();
    let deadline = Instant::now() + TIMEOUT;
    let mut latest: Option<Frame> = None;
    let mut complete_at: Option<Instant> = None;
    loop {
        let wait = match complete_at {
            Some(t) => (t + SETTLE).saturating_duration_since(Instant::now()),
            None => deadline.saturating_duration_since(Instant::now()),
        };
        match rx.recv_timeout(wait) {
            Ok(ServerMsg::Frame { seq, updates, frame, complete, .. }) => {
                presenter.apply(&mut comp, updates);
                latest = Some(frame);
                send(ClientMsg::FrameAck(seq))?;
                complete_at = complete.then(Instant::now);
            }
            Ok(ServerMsg::Exit(reason)) => bail!("session {name:?} ended the connection: {reason:?}"),
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) if complete_at.is_some() => break,
            Err(RecvTimeoutError::Timeout) => bail!("no complete frame from session {name:?} within {TIMEOUT:?}"),
            Err(RecvTimeoutError::Disconnected) => bail!("session {name:?} closed the connection"),
        }
    }
    let _ = send(ClientMsg::Detach);
    let frame = latest.context("no frame")?;
    let mut screen = Screen::new();
    let bytes = screen.render(&frame, &presenter, &mut comp)?.to_vec();
    let mut term = MockTerminal::new(opts.geometry, [0, 0, 0, 255]);
    if opts.linear_blending {
        term.set_blending(Blending::Linear);
    }
    term.feed(&bytes);
    term.render().save_png(out).with_context(|| format!("writing {}", out.display()))?;
    if let Some(path) = &opts.text {
        std::fs::write(path, screen_text(&frame, opts.geometry))
            .with_context(|| format!("writing {}", path.display()))?;
    }
    if let Some(path) = &opts.mask {
        cell_mask(&frame, opts.geometry).save_png(path).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// The cells of the frame's text grids, a line per row: spaces outside them
/// and for blank cells, a wide character once (its spacer adds nothing),
/// trailing spaces dropped. What a terminal reports as its screen's text.
pub(crate) fn screen_text(frame: &Frame, g: Geometry) -> String {
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
    let mut out = String::new();
    for row in rows {
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
    let (w, h) = (g.width_px(), g.height_px());
    let mut mask = RgbaImage { width: w, height: h, data: vec![0; (w * h * 4) as usize] };
    let mut fill = |col: u32, row: u32| {
        for y in row * g.cell.height..((row + 1) * g.cell.height).min(h) {
            for x in col * g.cell.width..((col + 1) * g.cell.width).min(w) {
                let i = ((y * w + x) * 4) as usize;
                mask.data[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    };
    for grid in &frame.text {
        for (r, cells) in grid.rows.iter().enumerate() {
            for (c, cell) in cells.iter().enumerate() {
                // A spacer goes with its wide character.
                let wide_spacer = cell.width == 0 && c > 0 && drawn_by_terminal(&cells[c - 1]);
                if drawn_by_terminal(cell) || wide_spacer {
                    fill(grid.col as u32 + c as u32, grid.row as u32 + r as u32);
                }
            }
        }
    }
    if let Some((col, row)) = frame.cursor {
        fill(col as u32, row as u32);
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
}
