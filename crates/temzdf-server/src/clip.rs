//! Cutting what lies under floating panes out of the panes below them.
//!
//! Images are always drawn below text, and inside tmux a cell shows one image
//! only, so stacking order cannot hide a lower pane under a float. Instead the
//! lower panes' layers are cut into the pieces outside the floats, and their
//! text cells there are left out (absent, so the float's own images show).

use temzdf_scene::{Layer, Rect, TextGrid};

/// `r` without every rectangle of `covers`, as disjoint pieces.
pub fn uncovered(r: Rect, covers: &[Rect]) -> Vec<Rect> {
    let mut parts = vec![r];
    for c in covers {
        parts = parts.iter().flat_map(|p| p.subtract(c)).collect();
    }
    parts
}

/// The pieces of `layer` outside `covers` (screen pixels); the layer itself
/// when nothing covers it. Scaled layers keep their scale.
pub fn layer_pieces(layer: &Layer, covers: &[Rect]) -> Vec<Layer> {
    if !covers.iter().any(|c| c.intersect(&layer.dst).is_some()) {
        return vec![*layer];
    }
    uncovered(layer.dst, covers).iter().filter_map(|p| layer.clipped(p)).collect()
}

/// The parts of `grid` outside `covers` (cells); the grid itself when nothing
/// covers it. A wide character cut in half becomes a blank cell.
pub fn text_pieces(grid: TextGrid, covers: &[Rect]) -> Vec<TextGrid> {
    let area = Rect::new(grid.col as i32, grid.row as i32, grid.width() as u32, grid.rows.len() as u32);
    if !covers.iter().any(|c| c.intersect(&area).is_some()) {
        return vec![grid];
    }
    uncovered(area, covers)
        .into_iter()
        .map(|p| {
            let (x0, y0) = ((p.x - area.x) as usize, (p.y - area.y) as usize);
            let rows = grid.rows[y0..y0 + p.h as usize]
                .iter()
                .map(|row| {
                    let mut cells = row[x0..x0 + p.w as usize].to_vec();
                    temzdf_scene::mend_wide(&mut cells);
                    cells
                })
                .collect();
            TextGrid { col: p.x as u16, row: p.y as u16, rows }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use temzdf_scene::Cell;
    use temzdf_scene::Images;

    fn layer(src: Rect, dst: Rect) -> Layer {
        let image = Images::new().add(1, 1, vec![0; 4]);
        Layer { key: 1, image, src, dst, z: 0 }
    }

    #[test]
    fn layers_are_cut_around_covers_and_keep_their_scale() {
        let l = layer(Rect::new(0, 0, 100, 100), Rect::new(0, 0, 100, 100));
        assert_eq!(layer_pieces(&l, &[Rect::new(200, 0, 10, 10)]), [l], "nothing covers it");
        assert!(layer_pieces(&l, &[Rect::new(0, 0, 100, 100)]).is_empty(), "covered");
        let pieces = layer_pieces(&l, &[Rect::new(20, 30, 40, 50)]);
        let dst: Vec<_> = pieces.iter().map(|p| p.dst).collect();
        assert_eq!(dst, Rect::new(0, 0, 100, 100).subtract(&Rect::new(20, 30, 40, 50)));
        assert!(pieces.iter().all(|p| p.src == p.dst), "unscaled: source pixels match the screen");
        // Shown at half size: each piece's source is twice its screen size.
        let half = layer(Rect::new(0, 0, 200, 200), Rect::new(0, 0, 100, 100));
        let pieces = layer_pieces(&half, &[Rect::new(50, 0, 50, 100)]);
        assert_eq!(pieces.len(), 1);
        assert_eq!((pieces[0].src, pieces[0].dst), (Rect::new(0, 0, 100, 200), Rect::new(0, 0, 50, 100)));
    }

    fn cell(text: &str, width: u8) -> Cell {
        Cell { text: text.into(), width, ..Cell::default() }
    }

    fn texts(g: &TextGrid) -> Vec<String> {
        g.rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|c| {
                        if c.width == 0 {
                            ""
                        } else if c.text.is_empty() {
                            "."
                        } else {
                            &c.text
                        }
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn text_is_left_out_under_covers() {
        let row = || vec![cell("a", 1), cell("あ", 2), cell("", 0), cell("b", 1), cell("c", 1)];
        let grid = TextGrid { col: 10, row: 5, rows: vec![row(), row(), row()] };
        assert_eq!(
            text_pieces(grid.clone(), &[Rect::new(0, 0, 5, 5)]),
            std::slice::from_ref(&grid),
            "nothing covers it"
        );
        // A cover over the middle row's columns 12..14 cuts the wide
        // character's spacer off: the character goes blank.
        let pieces = text_pieces(grid, &[Rect::new(12, 6, 2, 1)]);
        let got: Vec<_> = pieces.iter().map(|g| (g.col, g.row, texts(g))).collect();
        assert_eq!(
            got,
            [
                (10, 5, vec!["aあbc".to_owned()]),
                (10, 7, vec!["aあbc".to_owned()]),
                (10, 6, vec!["a.".to_owned()]),
                (14, 6, vec!["c".to_owned()]),
            ]
        );
    }
}
