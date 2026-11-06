//! The cursor and the one selection of a pane.

use crate::edit::{Change, Gravity, map_offset};

/// How a selection's ends are read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionKind {
    /// The characters from one end to the other, the active end excluded.
    #[default]
    Char,
    /// Whole lines.
    Line,
    /// A rectangle of display columns.
    Block,
}

/// A cursor between characters, and maybe a selection from `anchor` to it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    /// The cursor: the selection's active end.
    pub pos: usize,
    pub anchor: Option<(usize, SelectionKind)>,
    /// The display column vertical moves aim for (kept across short
    /// lines), `None` until one moves.
    pub want_col: Option<usize>,
}

impl Cursor {
    pub fn at(pos: usize) -> Self {
        Self { pos, anchor: None, want_col: None }
    }

    /// The selected characters' offsets, ordered (a character or line
    /// selection; a block's corners).
    pub fn selection(&self) -> Option<(usize, usize, SelectionKind)> {
        self.anchor.map(|(a, kind)| (a.min(self.pos), a.max(self.pos), kind))
    }

    /// Follow edits made elsewhere: an insertion exactly at the cursor
    /// leaves it after the text; the anchor stays before.
    pub fn follow(&mut self, changes: &[Change]) {
        self.pos = map_offset(self.pos, changes, Gravity::After).0;
        if let Some((a, _)) = &mut self.anchor {
            *a = map_offset(*a, changes, Gravity::Before).0;
        }
        self.want_col = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{TextEdit, apply};
    use crate::text::Text;

    #[test]
    fn the_cursor_moves_after_an_insertion_at_it() {
        let mut t = Text::from("abc");
        let mut c = Cursor { pos: 1, anchor: Some((1, SelectionKind::Char)), want_col: Some(4) };
        let changes = apply(&mut t, vec![TextEdit::new(1, 1, "XY")]).unwrap();
        c.follow(&changes);
        assert_eq!((c.pos, c.anchor, c.want_col), (3, Some((1, SelectionKind::Char)), None));
        assert_eq!(c.selection(), Some((1, 3, SelectionKind::Char)));
    }
}
