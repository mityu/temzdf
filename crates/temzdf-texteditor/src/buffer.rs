//! A buffer: the text, its marks and its undo tree, changed together.

use crate::cursor::Cursor;
use crate::edit::{self, Change, TextEdit};
use crate::marks::Marks;
use crate::motion::WordChars;
use crate::text::{LineEnding, Range, Text};
use crate::undo::{Batch, StepKind, UndoTree};

/// Why an edit was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    Overlap(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::Overlap(e) => f.write_str(e),
        }
    }
}

/// Who edits: the kind of step (for undo grouping), and the pane's cursor
/// before the edit (undo puts it back there).
#[derive(Clone, Copy, Debug, Default)]
pub struct Origin {
    pub kind: StepKind,
    pub pane: Option<u32>,
    pub cursor: Cursor,
}

/// What an edit, undo or redo changed: batches in order (each in offsets of
/// the text before it), for cursors elsewhere to follow; for undo and redo,
/// where the cursor of the pane that made the step goes; and the changes
/// as scripts hear of them (`TextChanged`).
#[derive(Clone, Debug, Default)]
pub struct Edited {
    pub batches: Vec<Vec<Change>>,
    pub cursor: Option<(Option<u32>, Cursor)>,
    pub reported: Vec<Reported>,
}

/// One change as scripts see it: the range it replaced, in positions of
/// the text before its batch, that range's length in UTF-16 units, and the
/// text put there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reported {
    pub range: Range,
    pub range_length: usize,
    pub text: String,
}

/// `changes` (a batch applied to `before`) as scripts see them.
fn report(before: &Text, changes: &[Change]) -> Vec<Reported> {
    changes
        .iter()
        .map(|c| Reported {
            range: before.range(c.start, c.start + c.removed_chars()),
            range_length: c.removed.encode_utf16().count(),
            text: c.inserted.clone(),
        })
        .collect()
}

impl Edited {
    pub fn is_empty(&self) -> bool {
        self.batches.iter().all(Vec::is_empty)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Buffer {
    text: Text,
    marks: Marks,
    undo: UndoTree,
    /// Bumped on every change.
    version: u64,
    pub line_ending: LineEnding,
    pub wordchars: WordChars,
}

impl Buffer {
    pub fn new(text: Text, line_ending: LineEnding) -> Self {
        Self { text, line_ending, ..Self::default() }
    }

    pub fn text(&self) -> &Text {
        &self.text
    }

    pub fn marks(&self) -> &Marks {
        &self.marks
    }

    /// Adding, moving and removing marks is not an edit (no undo).
    pub fn marks_mut(&mut self) -> &mut Marks {
        &mut self.marks
    }

    pub fn undo_tree(&self) -> &UndoTree {
        &self.undo
    }

    /// Grouping (`begin`, `end`, `split`, `invocation`) and the saved state.
    pub fn undo_tree_mut(&mut self) -> &mut UndoTree {
        &mut self.undo
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn modified(&self) -> bool {
        self.undo.modified()
    }

    /// Apply a batch, recorded for undo. The cursor given in `origin`
    /// follows it (an insertion at it leaves it after).
    pub fn edit(&mut self, edits: Vec<TextEdit>, origin: Origin) -> Result<Edited, EditError> {
        let before = self.text.clone();
        let changes = edit::apply(&mut self.text, edits).map_err(EditError::Overlap)?;
        if changes.is_empty() {
            return Ok(Edited::default());
        }
        let reported = report(&before, &changes);
        self.version += 1;
        let marks = self.marks.follow(&changes);
        let mut after = origin.cursor;
        after.follow(&changes);
        self.undo.record(Batch { changes: changes.clone(), marks }, origin.kind, origin.cursor, after, origin.pane);
        Ok(Edited { batches: vec![changes], cursor: None, reported })
    }

    /// Undo the current step: its cursor goes back to where it was before.
    pub fn undo(&mut self) -> Option<Edited> {
        let node = self.undo.back()?;
        let mut done = Edited::default();
        for batch in node.batches.iter().rev() {
            let before = self.text.clone();
            let changes = edit::apply(&mut self.text, edit::inverse(&batch.changes)).expect("an undo applies");
            done.reported.extend(report(&before, &changes));
            self.marks.follow(&changes);
            self.marks.restore(&batch.marks);
            done.batches.push(changes);
        }
        self.version += 1;
        done.cursor = Some((node.pane, node.before));
        Some(done)
    }

    /// Redo along the current branch: the cursor goes where the step left
    /// it.
    pub fn redo(&mut self) -> Option<Edited> {
        let node = self.undo.forward()?;
        let mut done = Edited::default();
        for (i, batch) in node.batches.iter().enumerate() {
            let edits = batch
                .changes
                .iter()
                .map(|c| TextEdit::new(c.start, c.start + c.removed_chars(), c.inserted.clone()))
                .collect();
            let before = self.text.clone();
            let changes = edit::apply(&mut self.text, edits).expect("a redo applies");
            done.reported.extend(report(&before, &changes));
            let marks = self.marks.follow(&changes);
            self.undo.set_marks(i, marks);
            done.batches.push(changes);
        }
        self.version += 1;
        done.cursor = Some((node.pane, node.after));
        Some(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marks::MarkSpec;

    fn typed(b: &mut Buffer, at: usize, s: &str) -> Cursor {
        b.undo_tree_mut().invocation();
        let o = Origin { kind: StepKind::Typing, pane: Some(1), cursor: Cursor::at(at) };
        b.edit(vec![TextEdit::new(at, at, s)], o).unwrap();
        Cursor::at(at + s.chars().count())
    }

    fn other(b: &mut Buffer, e: TextEdit) {
        b.undo_tree_mut().invocation();
        let o = Origin { kind: StepKind::Other, pane: Some(1), cursor: Cursor::at(e.start) };
        b.edit(vec![e], o).unwrap();
    }

    #[test]
    fn typing_merges_and_other_steps_split() {
        let mut b = Buffer::new(Text::from(""), LineEnding::Lf);
        let mut c = typed(&mut b, 0, "a");
        c = typed(&mut b, c.pos, "b");
        typed(&mut b, c.pos, "c");
        other(&mut b, TextEdit::new(0, 1, ""));
        assert_eq!(b.text().to_string(), "bc");
        let u = b.undo().unwrap();
        assert_eq!(b.text().to_string(), "abc");
        assert_eq!(u.cursor.unwrap().1.pos, 0, "back where the step started");
        b.undo().unwrap();
        assert_eq!(b.text().to_string(), "", "the typing was one step");
        assert!(b.undo().is_none());
        assert!(!b.modified(), "back at the state it was read in");
        b.redo().unwrap();
        assert_eq!(b.text().to_string(), "abc");
        assert_eq!(b.redo().unwrap().cursor.unwrap().1.pos, 0);
        assert_eq!(b.text().to_string(), "bc");
    }

    #[test]
    fn moving_the_cursor_ends_a_typing_step() {
        let mut b = Buffer::new(Text::from(""), LineEnding::Lf);
        typed(&mut b, 0, "ab");
        b.undo_tree_mut().cursor_moved();
        typed(&mut b, 0, "x");
        b.undo().unwrap();
        assert_eq!(b.text().to_string(), "ab");
    }

    #[test]
    fn changes_are_reported_in_positions_before_them() {
        let mut b = Buffer::new(Text::from("a😀b\nc"), LineEnding::Lf);
        let o = Origin::default();
        let e = b.edit(vec![TextEdit::new(1, 2, "xy"), TextEdit::new(4, 5, "")], o).unwrap();
        let r = &e.reported;
        assert_eq!((r[0].range.start.character, r[0].range.end.character, r[0].range_length), (1, 3, 2));
        assert_eq!(r[0].text, "xy");
        assert_eq!((r[1].range.start.line, r[1].range.start.character, r[1].range_length), (1, 0, 1));
        let u = b.undo().unwrap();
        assert_eq!(u.reported.len(), 2, "undo reports its changes too");
    }

    #[test]
    fn a_group_spans_invocations_and_split_cuts_it() {
        let mut b = Buffer::new(Text::from("x"), LineEnding::Lf);
        b.undo_tree_mut().begin();
        other(&mut b, TextEdit::new(1, 1, "1"));
        other(&mut b, TextEdit::new(2, 2, "2"));
        b.undo_tree_mut().split();
        other(&mut b, TextEdit::new(3, 3, "3"));
        b.undo_tree_mut().end();
        b.undo().unwrap();
        assert_eq!(b.text().to_string(), "x12");
        b.undo().unwrap();
        assert_eq!(b.text().to_string(), "x");
    }

    #[test]
    fn the_tree_keeps_branches_and_the_saved_state() {
        let mut b = Buffer::new(Text::from(""), LineEnding::Lf);
        other(&mut b, TextEdit::new(0, 0, "a"));
        b.undo_tree_mut().mark_saved();
        other(&mut b, TextEdit::new(1, 1, "b"));
        assert!(b.modified());
        b.undo().unwrap();
        assert!(!b.modified());
        // A new branch; redo follows it, the old one stays in the tree.
        other(&mut b, TextEdit::new(1, 1, "c"));
        b.undo().unwrap();
        b.redo().unwrap();
        assert_eq!(b.text().to_string(), "ac");
        assert_eq!(b.undo_tree().len(), 4);
    }

    #[test]
    fn undo_restores_deleted_marks() {
        let mut b = Buffer::new(Text::from("keep gone keep"), LineEnding::Lf);
        let m = b.marks_mut().add(5, 9, MarkSpec::default());
        other(&mut b, TextEdit::new(5, 10, ""));
        assert!(b.marks().get(m).unwrap().deleted);
        b.undo().unwrap();
        let mark = b.marks().get(m).unwrap();
        assert_eq!((mark.start, mark.end, mark.deleted), (5, 9, false));
        b.redo().unwrap();
        assert!(b.marks().get(m).unwrap().deleted, "deleted again");
        b.undo().unwrap();
        assert!(!b.marks().get(m).unwrap().deleted, "and restored again");
    }
}
