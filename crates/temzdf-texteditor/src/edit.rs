//! Edits: non-overlapping replacements applied at once, and what they did.

use crate::text::Text;

/// Replace the text between two offsets (in the text before any edit of
/// the batch) with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl TextEdit {
    pub fn new(start: usize, end: usize, text: impl Into<String>) -> Self {
        Self { start: start.min(end), end: start.max(end), text: text.into() }
    }
}

/// What one replacement did, in offsets of the text before its batch:
/// `removed` was at `start`, `inserted` took its place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
}

impl Change {
    pub fn removed_chars(&self) -> usize {
        self.removed.chars().count()
    }

    pub fn inserted_chars(&self) -> usize {
        self.inserted.chars().count()
    }
}

/// Where a point goes when text is inserted exactly at it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Gravity {
    /// Stays before the inserted text.
    #[default]
    Before,
    /// Moves after it (a cursor does).
    After,
}

/// Apply a batch: the edits must not overlap (two insertions at one point
/// do, as their order would be a guess). Returns the changes, ordered by
/// position.
pub(crate) fn apply(text: &mut Text, mut edits: Vec<TextEdit>) -> Result<Vec<Change>, String> {
    let len = text.len_chars();
    for e in &mut edits {
        e.end = e.end.min(len);
        e.start = e.start.min(e.end);
    }
    edits.sort_by_key(|e| (e.start, e.end));
    for w in edits.windows(2) {
        if w[1].start < w[0].end || (w[1].start == w[0].start && w[0].start == w[0].end) {
            return Err(format!("overlapping edits at {} and {}", w[0].start, w[1].start));
        }
    }
    let mut changes: Vec<Change> = edits
        .iter()
        .map(|e| Change { start: e.start, removed: text.slice(e.start, e.end), inserted: e.text.clone() })
        .collect();
    // Back to front, so earlier offsets stay valid.
    let rope = text.rope_mut();
    for (e, c) in edits.iter().zip(&changes).rev() {
        rope.remove(e.start..e.end);
        rope.insert(e.start, &c.inserted);
    }
    changes.retain(|c| c.removed != c.inserted);
    Ok(changes)
}

/// The edits that undo `changes` (a batch, ordered), in offsets of the text
/// after it.
pub(crate) fn inverse(changes: &[Change]) -> Vec<TextEdit> {
    let mut delta = 0isize;
    changes
        .iter()
        .map(|c| {
            let start = (c.start as isize + delta) as usize;
            delta += c.inserted_chars() as isize - c.removed_chars() as isize;
            TextEdit::new(start, start + c.inserted_chars(), c.removed.clone())
        })
        .collect()
}

/// Where `offset` goes after a batch of `changes`. A point inside removed
/// text collapses to where the replacement starts (`true`: it was inside);
/// a point at the end of removed text stays after the replacement.
pub fn map_offset(offset: usize, changes: &[Change], gravity: Gravity) -> (usize, bool) {
    let mut delta = 0isize;
    for c in changes {
        let (removed, inserted) = (c.removed_chars(), c.inserted_chars());
        let end = c.start + removed;
        let at = |o: usize| (o as isize + delta) as usize;
        if offset < c.start {
            return (at(offset), false);
        }
        if offset == c.start && (removed > 0 || gravity == Gravity::Before) {
            return (at(c.start), false);
        }
        if offset == c.start {
            return (at(c.start) + inserted, false);
        }
        if offset < end {
            return (at(c.start), true);
        }
        if offset == end {
            return (at(c.start) + inserted, false);
        }
        delta += inserted as isize - removed as isize;
    }
    ((offset as isize + delta) as usize, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_uses_the_original_offsets_and_undoes() {
        let mut t = Text::from("hello world");
        let edits = vec![TextEdit::new(6, 11, "there"), TextEdit::new(0, 5, "hi")];
        let changes = apply(&mut t, edits).unwrap();
        assert_eq!(t.to_string(), "hi there");
        assert_eq!(changes[0], Change { start: 0, removed: "hello".into(), inserted: "hi".into() });
        apply(&mut t, inverse(&changes)).unwrap();
        assert_eq!(t.to_string(), "hello world");
    }

    #[test]
    fn overlapping_edits_are_refused() {
        let mut t = Text::from("abcdef");
        assert!(apply(&mut t, vec![TextEdit::new(0, 3, "x"), TextEdit::new(2, 4, "y")]).is_err());
        assert!(apply(&mut t, vec![TextEdit::new(1, 1, "x"), TextEdit::new(1, 1, "y")]).is_err());
        assert!(apply(&mut t, vec![TextEdit::new(1, 1, "x"), TextEdit::new(1, 3, "y")]).is_err());
        assert_eq!(t.to_string(), "abcdef", "nothing applied");
        // Adjacent is fine.
        apply(&mut t, vec![TextEdit::new(0, 1, "A"), TextEdit::new(1, 2, "B")]).unwrap();
        assert_eq!(t.to_string(), "ABcdef");
    }

    #[test]
    fn points_follow_edits() {
        let mut t = Text::from("abcdef");
        // "cd" -> "XYZ"; insert "!" at 0.
        let changes = apply(&mut t, vec![TextEdit::new(2, 4, "XYZ"), TextEdit::new(0, 0, "!")]).unwrap();
        assert_eq!(t.to_string(), "!abXYZef");
        let m = |o, g| map_offset(o, &changes, g);
        assert_eq!(m(0, Gravity::Before), (0, false), "stays before an insertion there");
        assert_eq!(m(0, Gravity::After), (1, false), "or moves after it");
        assert_eq!(m(2, Gravity::After), (3, false), "at the start of removed text");
        assert_eq!(m(3, Gravity::Before), (3, true), "inside removed text collapses");
        assert_eq!(m(4, Gravity::Before), (6, false), "the end of removed text stays after");
        assert_eq!(m(6, Gravity::Before), (8, false));
    }
}
