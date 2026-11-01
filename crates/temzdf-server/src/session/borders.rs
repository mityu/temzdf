//! Borders between tiled panes and the frames of floats, drawn as text
//! (box-drawing characters), so any terminal shows them, with or without
//! graphics: the one-cell gaps between panes get lines joined where they
//! meet (the focused pane's in the accent colour), a float's frame cells a
//! rounded box.

use temzdf_scene::{Attrs, Cell, Color, Rect, TextGrid};

use super::{BACKGROUND, BORDER, PaneId};

fn rgb(c: [u8; 4]) -> Color {
    Color::Rgb([c[0], c[1], c[2]])
}

fn accent() -> Color {
    rgb(termpix_ui::Theme::dark().accent.0)
}

fn cell(c: char, fg: Color) -> Cell {
    Cell { text: c.to_string(), width: 1, fg, bg: rgb(BACKGROUND), attrs: Attrs::default() }
}

/// The line through a gap cell joined to the gap cells up, down, left and
/// right of it.
fn line(up: bool, down: bool, left: bool, right: bool) -> char {
    match (up, down, left, right) {
        (false, false, false, false) => ' ',
        (true, true, true, true) => '┼',
        (true, true, false, true) => '├',
        (true, true, true, false) => '┤',
        (false, true, true, true) => '┬',
        (true, false, true, true) => '┴',
        (false, true, false, true) => '┌',
        (false, true, true, false) => '┐',
        (true, false, false, true) => '└',
        (true, false, true, false) => '┘',
        (_, _, false, false) => '│',
        (false, false, _, _) => '─',
    }
}

/// The gaps between `panes` (tiled, in cells) inside `area`, as rows of
/// lines: one grid per run of gap cells in a row. Cells next to `focus`
/// (corners too) are in the accent colour.
pub(super) fn gaps(area: Rect, panes: &[(PaneId, Rect)], focus: PaneId) -> Vec<TextGrid> {
    if panes.len() < 2 || area.w == 0 || area.h == 0 {
        return Vec::new();
    }
    let (w, h) = (area.w as usize, area.h as usize);
    let mut covered = vec![false; w * h];
    for (_, r) in panes {
        for y in r.y.max(area.y)..(r.y + r.h as i32).min(area.y + h as i32) {
            for x in r.x.max(area.x)..(r.x + r.w as i32).min(area.x + w as i32) {
                covered[(y - area.y) as usize * w + (x - area.x) as usize] = true;
            }
        }
    }
    let gap = |x: i64, y: i64| {
        x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && !covered[y as usize * w + x as usize]
    };
    let near_focus = panes
        .iter()
        .find(|(p, _)| *p == focus)
        .map(|(_, r)| Rect::new(r.x - 1 - area.x, r.y - 1 - area.y, r.w + 2, r.h + 2));
    let mut out = Vec::new();
    for y in 0..h as i64 {
        let mut run: Option<(usize, Vec<Cell>)> = None;
        for x in 0..=w as i64 {
            if x < w as i64 && gap(x, y) {
                let c = line(gap(x, y - 1), gap(x, y + 1), gap(x - 1, y), gap(x + 1, y));
                let lit = near_focus.is_some_and(|r| r.contains(x as i32, y as i32));
                let c = cell(c, if lit { accent() } else { rgb(BORDER) });
                run.get_or_insert_with(|| (x as usize, Vec::new())).1.push(c);
            } else if let Some((x0, cells)) = run.take() {
                out.push(TextGrid {
                    col: (area.x + x0 as i32) as u16,
                    row: (area.y + y as i32) as u16,
                    rows: vec![cells],
                });
            }
        }
    }
    out
}

/// A float's frame (the cells around its content, `frame` in cells) as a
/// rounded box: the top and bottom rows and the sides' cells.
pub(super) fn frame(frame: Rect, focus: bool) -> Vec<TextGrid> {
    if frame.w == 0 || frame.h == 0 || frame.x < 0 || frame.y < 0 {
        return Vec::new();
    }
    let fg = if focus { accent() } else { rgb(BORDER) };
    let (w, h) = (frame.w as usize, frame.h as usize);
    let edge = |l: char, r: char| -> Vec<Cell> {
        (0..w)
            .map(|i| {
                cell(
                    if i == 0 {
                        l
                    } else if i == w - 1 {
                        r
                    } else {
                        '─'
                    },
                    fg,
                )
            })
            .collect()
    };
    let (x, y) = (frame.x as u16, frame.y as u16);
    let mut out = vec![TextGrid { col: x, row: y, rows: vec![edge('╭', '╮')] }];
    if h > 1 {
        out.push(TextGrid { col: x, row: y + h as u16 - 1, rows: vec![edge('╰', '╯')] });
    }
    if h > 2 {
        let side = || (1..h - 1).map(|_| vec![cell('│', fg)]).collect::<Vec<_>>();
        out.push(TextGrid { col: x, row: y + 1, rows: side() });
        if w > 1 {
            out.push(TextGrid { col: x + w as u16 - 1, row: y + 1, rows: side() });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grids drawn over a blank `w` x `h` screen, as text.
    fn draw(grids: &[TextGrid], w: usize, h: usize) -> Vec<String> {
        let mut screen = vec![vec![' '; w]; h];
        for g in grids {
            for (dy, row) in g.rows.iter().enumerate() {
                for (dx, c) in row.iter().enumerate() {
                    screen[g.row as usize + dy][g.col as usize + dx] = c.text.chars().next().unwrap_or(' ');
                }
            }
        }
        screen.into_iter().map(|r| r.into_iter().collect()).collect()
    }

    #[test]
    fn gaps_join_where_they_meet() {
        // 1 | 2 over 3, all over 4 (11 x 7): a T and a cross-less join.
        let area = Rect::new(0, 0, 11, 7);
        let panes = [
            (1, Rect::new(0, 0, 5, 4)),
            (2, Rect::new(6, 0, 5, 2)),
            (3, Rect::new(6, 3, 5, 1)),
            (4, Rect::new(0, 5, 11, 2)),
        ];
        let grids = gaps(area, &panes, 3);
        assert_eq!(
            draw(&grids, 11, 7),
            ["     │     ", "     │     ", "     ├─────", "     │     ", "─────┴─────", "           ", "           ",]
        );
        // The cells around pane 3 are lit, the others not.
        let at = |x: u16, y: u16| {
            grids.iter().find_map(|g| {
                let dx = x.checked_sub(g.col)? as usize;
                (g.row == y).then(|| g.rows[0].get(dx).cloned()).flatten()
            })
        };
        assert_eq!(at(7, 2).unwrap().fg, accent(), "above pane 3");
        assert_eq!(at(5, 3).unwrap().fg, accent(), "left of pane 3");
        assert_eq!(at(5, 0).unwrap().fg, rgb(BORDER), "between 1 and 2");
        assert_eq!(at(1, 4).unwrap().fg, rgb(BORDER), "below 1");
        assert!(gaps(area, &panes[..1], 1).is_empty(), "one pane: no gaps");
    }

    #[test]
    fn a_frame_is_a_rounded_box() {
        let grids = frame(Rect::new(1, 1, 6, 4), true);
        assert_eq!(draw(&grids, 8, 6), ["        ", " ╭────╮ ", " │    │ ", " │    │ ", " ╰────╯ ", "        "]);
        assert!(grids.iter().all(|g| g.rows.iter().flatten().all(|c| c.fg == accent())));
        // Squeezed: what fits.
        assert_eq!(draw(&frame(Rect::new(0, 0, 3, 2), false), 3, 2), ["╭─╮", "╰─╯"]);
    }
}
