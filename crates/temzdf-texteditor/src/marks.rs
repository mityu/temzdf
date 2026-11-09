//! Decorations and marks: one mechanism (a mark is a decoration of no width
//! and no style, as Neovim's extmarks). They follow edits; one whose text is
//! deleted collapses and is flagged, and undo puts it back.

use std::time::Instant;

use crate::edit::{Change, Gravity, map_offset};

pub type MarkId = u64;

/// Where a decoration's virtual text shows (Neovim's `virt_text_pos`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VirtPos {
    /// Between the characters, before the one at the decoration's start:
    /// it takes columns (layout counts it), the cursor never goes in it.
    #[default]
    Inline,
    /// After the end of the line (one blank between).
    Eol,
    /// Over the text from the decoration's start, hiding what is there.
    Overlay,
}

/// A piece of virtual text and its style (a highlight group).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VirtChunk {
    pub text: String,
    pub style: Option<String>,
}

/// An image a decoration shows (the session's, by id), its size in pixels
/// (layout turns it into cells), and the text shown instead where images
/// cannot be.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VirtImage {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub alt: String,
    /// At most this many columns.
    pub max_cols: Option<usize>,
    /// At most this many rows (virtual lines; one row elsewhere).
    pub max_rows: Option<usize>,
    /// The image is gone (unloaded): `alt` is shown as text.
    pub missing: bool,
}

/// What a range is shown as instead of its text (Neovim's conceal): text,
/// an image (one row high), or nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conceal {
    pub text: Vec<VirtChunk>,
    pub image: Option<VirtImage>,
    /// Hidden on the cursor's line too (else that line shows the text).
    pub cursor_line: bool,
}

/// Lines shown below (or above) the line a decoration starts in, not in
/// the buffer: rows of text, then an image as many rows high as its
/// pixels fill.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VirtLines {
    pub text: Vec<Vec<VirtChunk>>,
    pub image: Option<VirtImage>,
    pub above: bool,
}

/// What a decoration is made of, apart from where it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkSpec {
    /// Cleared together (`clear(ns)`).
    pub namespace: String,
    /// The style's name (a highlight group), or none for a mark.
    pub style: Option<String>,
    pub priority: i32,
    /// Shown only in this pane.
    pub pane: Option<u32>,
    /// Where each end goes on an insertion exactly there.
    pub start_gravity: Gravity,
    pub end_gravity: Gravity,
    /// Gone after this (a yank flash).
    pub expires: Option<Instant>,
    /// Text shown that is not in the buffer (a prompt, a hint), at
    /// `virt_pos`.
    pub virt_text: Vec<VirtChunk>,
    pub virt_pos: VirtPos,
    /// An image at `virt_pos` (inline: one row high, after `virt_text`).
    pub virt_image: Option<VirtImage>,
    /// The range (within one line) shown as something else.
    pub conceal: Option<Conceal>,
    /// Lines below or above its line.
    pub virt_lines: Option<VirtLines>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub id: MarkId,
    pub start: usize,
    pub end: usize,
    pub spec: MarkSpec,
    /// Its text was deleted: it collapsed to where the deletion was.
    pub deleted: bool,
}

/// A buffer's decorations, in no particular order.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    marks: Vec<Mark>,
    next: MarkId,
}

/// Marks as they were before a batch moved them (to restore on undo).
pub(crate) type Snapshot = Vec<(MarkId, usize, usize, bool)>;

impl Marks {
    pub fn add(&mut self, start: usize, end: usize, spec: MarkSpec) -> MarkId {
        self.next += 1;
        let (start, end) = (start.min(end), start.max(end));
        self.marks.push(Mark { id: self.next, start, end, spec, deleted: false });
        self.next
    }

    pub fn get(&self, id: MarkId) -> Option<&Mark> {
        self.marks.iter().find(|m| m.id == id)
    }

    /// Move one (it is no longer deleted).
    pub fn set(&mut self, id: MarkId, start: usize, end: usize) -> bool {
        match self.marks.iter_mut().find(|m| m.id == id) {
            Some(m) => {
                (m.start, m.end, m.deleted) = (start.min(end), start.max(end), false);
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, id: MarkId) -> bool {
        let before = self.marks.len();
        self.marks.retain(|m| m.id != id);
        self.marks.len() != before
    }

    pub fn clear(&mut self, namespace: &str) {
        self.marks.retain(|m| m.spec.namespace != namespace);
    }

    /// Drop those whose time is up; whether any were.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.marks.len();
        self.marks.retain(|m| m.spec.expires.is_none_or(|t| t > now));
        self.marks.len() != before
    }

    /// The soonest expiry, for a timer.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.marks.iter().filter_map(|m| m.spec.expires).min()
    }

    /// Those touching `[start, end]`, by priority (lowest first, drawn
    /// first).
    pub fn overlapping(&self, start: usize, end: usize) -> Vec<&Mark> {
        let mut v: Vec<&Mark> = self.marks.iter().filter(|m| m.start <= end && m.end >= start).collect();
        v.sort_by_key(|m| (m.spec.priority, m.id));
        v
    }

    pub fn iter(&self) -> impl Iterator<Item = &Mark> {
        self.marks.iter()
    }

    /// Follow a batch; returns how the marks it collapsed were before.
    pub(crate) fn follow(&mut self, changes: &[Change]) -> Snapshot {
        let mut moved = Snapshot::new();
        for m in &mut self.marks {
            let before = (m.id, m.start, m.end, m.deleted);
            let (s, s_in) = map_offset(m.start, changes, m.spec.start_gravity);
            let (e, e_in) = map_offset(m.end, changes, m.spec.end_gravity);
            // Both ends in one deletion: its text is gone. A point at the
            // edge of deleted text is not in it.
            let gone = if m.start == m.end { s_in } else { s_in && e_in || inside_one(m.start, m.end, changes) };
            (m.start, m.end) = (s, e.max(s));
            if gone {
                m.deleted = true;
                m.end = m.start;
            }
            if s_in || e_in || gone {
                moved.push(before);
            }
        }
        moved
    }

    /// Image `id` now has this size (read again), or is gone.
    pub fn set_image(&mut self, id: u64, size: Option<(u32, u32)>) {
        for m in &mut self.marks {
            let images = m
                .spec
                .virt_image
                .iter_mut()
                .chain(m.spec.conceal.iter_mut().filter_map(|c| c.image.as_mut()))
                .chain(m.spec.virt_lines.iter_mut().filter_map(|v| v.image.as_mut()));
            for img in images.filter(|i| i.id == id) {
                match size {
                    Some((w, h)) => (img.width, img.height, img.missing) = (w, h, false),
                    None => img.missing = true,
                }
            }
        }
    }

    /// Put back marks as they were (undo).
    pub(crate) fn restore(&mut self, snapshot: &Snapshot) {
        for &(id, start, end, deleted) in snapshot {
            if let Some(m) = self.marks.iter_mut().find(|m| m.id == id) {
                (m.start, m.end, m.deleted) = (start, end, deleted);
            }
        }
    }
}

/// Whether `[start, end]` (not empty) is all inside one change's removed
/// text, edges included.
fn inside_one(start: usize, end: usize, changes: &[Change]) -> bool {
    changes.iter().any(|c| c.removed_chars() > 0 && c.start <= start && end <= c.start + c.removed_chars())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{TextEdit, apply};
    use crate::text::Text;

    fn edit(t: &mut Text, marks: &mut Marks, edits: Vec<TextEdit>) -> Snapshot {
        let changes = apply(t, edits).unwrap();
        marks.follow(&changes)
    }

    #[test]
    fn marks_follow_and_collapse_when_their_text_goes() {
        let mut t = Text::from("one two three");
        let mut m = Marks::default();
        let word = m.add(4, 7, MarkSpec { style: Some("Search".into()), ..MarkSpec::default() });
        let point = m.add(5, 5, MarkSpec::default());
        let edge = m.add(8, 8, MarkSpec::default());
        edit(&mut t, &mut m, vec![TextEdit::new(0, 0, ">> ")]);
        assert_eq!((m.get(word).unwrap().start, m.get(word).unwrap().end), (7, 10));
        // Delete "two " (7..11): the word and the point inside go; the
        // point at the deletion's end stays.
        let snap = edit(&mut t, &mut m, vec![TextEdit::new(7, 11, "")]);
        assert_eq!(t.to_string(), ">> one three");
        let w = m.get(word).unwrap();
        assert!(w.deleted && w.start == 7 && w.end == 7, "{w:?}");
        assert!(m.get(point).unwrap().deleted);
        assert!(!m.get(edge).unwrap().deleted, "at the edge, not inside");
        assert_eq!(m.get(edge).unwrap().start, 7);
        m.restore(&snap);
        assert_eq!((m.get(word).unwrap().start, m.get(word).unwrap().deleted), (7, false));
    }

    #[test]
    fn gravity_decides_at_an_insertion_point() {
        let mut t = Text::from("ab");
        let mut m = Marks::default();
        let stay = m.add(1, 1, MarkSpec::default());
        let go =
            m.add(1, 1, MarkSpec { start_gravity: Gravity::After, end_gravity: Gravity::After, ..Default::default() });
        // A decoration grows at its end only when its end moves after.
        let grow = m.add(0, 1, MarkSpec { end_gravity: Gravity::After, ..Default::default() });
        edit(&mut t, &mut m, vec![TextEdit::new(1, 1, "XY")]);
        assert_eq!(m.get(stay).unwrap().start, 1);
        assert_eq!(m.get(go).unwrap().start, 3);
        assert_eq!((m.get(grow).unwrap().start, m.get(grow).unwrap().end), (0, 3));
    }

    #[test]
    fn expiry_and_namespaces() {
        let mut m = Marks::default();
        let now = Instant::now();
        let flash = m.add(0, 1, MarkSpec { expires: Some(now), ..Default::default() });
        m.add(0, 1, MarkSpec { namespace: "hl".into(), ..Default::default() });
        assert_eq!(m.next_expiry(), Some(now));
        assert!(m.expire(now));
        assert!(m.get(flash).is_none());
        m.clear("hl");
        assert_eq!(m.iter().count(), 0);
    }
}
