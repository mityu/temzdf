//! Decorations and marks: one mechanism (a mark is a decoration of no width
//! and no style, as Neovim's extmarks). They follow edits; one whose text is
//! deleted collapses and is flagged, and undo puts it back.

use std::collections::{BTreeMap, BTreeSet};
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

/// A buffer's decorations, by id, and indexed so that what a frame needs
/// costs what it shows, not how many there are (tree-sitter and LSP
/// highlighting make thousands of them):
///
/// - by id (a `BTreeMap`: `get`, `set`, `remove` in O(log n); iterated in
///   the order they were added);
/// - by position: those at most [`SHORT`] characters long by start (a
///   `BTreeMap`), longer ones (few: blocks, folds) in a list of their own,
///   so [`overlapping`](Self::overlapping) visits those starting at most
///   `SHORT` characters before the range, in it, and the long ones;
/// - the few that layout reads wherever they are (inline virtual text,
///   conceal, virtual lines), those with images and those that expire,
///   each in a set of their own.
///
/// Edits move every mark after them ([`follow`](Self::follow) is linear,
/// and builds the position index again); the specs never change after
/// `add` (but images' sizes), so the sets by kind only change on `add` and
/// removals.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    marks: BTreeMap<MarkId, Mark>,
    next: MarkId,
    /// Short ones by `(start, id)`, their end the value.
    by_start: BTreeMap<(usize, MarkId), usize>,
    /// Those longer than [`SHORT`].
    long: BTreeSet<MarkId>,
    /// Inline virtual text or an inline image ([`Marks::inline`]).
    inline: BTreeSet<MarkId>,
    conceal: BTreeSet<MarkId>,
    virt_lines: BTreeSet<MarkId>,
    /// Showing an image somewhere ([`Marks::set_image`]).
    images: BTreeSet<MarkId>,
    expiring: BTreeSet<(Instant, MarkId)>,
}

/// Marks at most this many characters long are indexed by start; a range
/// query looks this far back for them.
const SHORT: usize = 256;

#[cfg(test)]
thread_local! {
    /// The marks [`Marks::touching`] and [`Marks::iter`] looked at on
    /// this thread (tests count work with it).
    pub(crate) static VISITED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn visited(n: usize) {
    VISITED.set(VISITED.get() + n);
}

/// Marks as they were before a batch moved them (to restore on undo).
pub(crate) type Snapshot = Vec<(MarkId, usize, usize, bool)>;

impl Marks {
    pub fn add(&mut self, start: usize, end: usize, spec: MarkSpec) -> MarkId {
        self.next += 1;
        let id = self.next;
        let (start, end) = (start.min(end), start.max(end));
        let s = &spec;
        if s.virt_pos == VirtPos::Inline && (!s.virt_text.is_empty() || s.virt_image.is_some()) {
            self.inline.insert(id);
        }
        if s.conceal.is_some() {
            self.conceal.insert(id);
        }
        if s.virt_lines.is_some() {
            self.virt_lines.insert(id);
        }
        if has_image(s) {
            self.images.insert(id);
        }
        if let Some(t) = s.expires {
            self.expiring.insert((t, id));
        }
        self.place(id, start, end);
        self.marks.insert(id, Mark { id, start, end, spec, deleted: false });
        id
    }

    pub fn get(&self, id: MarkId) -> Option<&Mark> {
        self.marks.get(&id)
    }

    /// Move one (it is no longer deleted).
    pub fn set(&mut self, id: MarkId, start: usize, end: usize) -> bool {
        let Some(m) = self.marks.get(&id) else { return false };
        let (old_start, old_end) = (m.start, m.end);
        let (start, end) = (start.min(end), start.max(end));
        self.unplace(id, old_start, old_end);
        self.place(id, start, end);
        let m = self.marks.get_mut(&id).expect("there");
        (m.start, m.end, m.deleted) = (start, end, false);
        true
    }

    pub fn remove(&mut self, id: MarkId) -> bool {
        let Some(m) = self.marks.remove(&id) else { return false };
        self.unplace(id, m.start, m.end);
        self.inline.remove(&id);
        self.conceal.remove(&id);
        self.virt_lines.remove(&id);
        self.images.remove(&id);
        if let Some(t) = m.spec.expires {
            self.expiring.remove(&(t, id));
        }
        true
    }

    pub fn clear(&mut self, namespace: &str) {
        let gone: Vec<MarkId> = self.marks.values().filter(|m| m.spec.namespace == namespace).map(|m| m.id).collect();
        if gone.len() == self.marks.len() {
            // All of them (a namespace drawn again): no need to unindex
            // one by one.
            let next = self.next;
            *self = Self { next, ..Self::default() };
            return;
        }
        for id in gone {
            self.remove(id);
        }
    }

    /// Drop those whose time is up; whether any were.
    pub fn expire(&mut self, now: Instant) -> bool {
        let mut any = false;
        while let Some(&(t, id)) = self.expiring.first()
            && t <= now
        {
            self.remove(id);
            any = true;
        }
        any
    }

    /// The soonest expiry, for a timer.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.expiring.first().map(|(t, _)| *t)
    }

    /// Those touching `[start, end]`, in no particular order.
    pub fn touching(&self, start: usize, end: usize) -> impl Iterator<Item = &Mark> {
        let short = self
            .by_start
            .range((start.saturating_sub(SHORT), 0)..=(end, MarkId::MAX))
            .filter(move |(_, e)| {
                #[cfg(test)]
                visited(1);
                **e >= start
            })
            .map(|(&(_, id), _)| id);
        let long = self.long.iter().copied().filter(move |id| {
            #[cfg(test)]
            visited(1);
            let m = &self.marks[id];
            m.start <= end && m.end >= start
        });
        short.chain(long).map(|id| &self.marks[&id])
    }

    /// Those touching `[start, end]`, by priority (lowest first, drawn
    /// first).
    pub fn overlapping(&self, start: usize, end: usize) -> Vec<&Mark> {
        let mut v: Vec<&Mark> = self.touching(start, end).collect();
        v.sort_by_key(|m| (m.spec.priority, m.id));
        v
    }

    /// Those with inline virtual text or an inline image (layout reads
    /// them wherever they are), by id.
    pub fn inline(&self) -> impl Iterator<Item = &Mark> {
        self.inline.iter().map(|id| &self.marks[id])
    }

    /// Those that conceal their range, by id.
    pub fn concealing(&self) -> impl Iterator<Item = &Mark> {
        self.conceal.iter().map(|id| &self.marks[id])
    }

    /// Those with virtual lines, by id.
    pub fn with_virt_lines(&self) -> impl Iterator<Item = &Mark> {
        self.virt_lines.iter().map(|id| &self.marks[id])
    }

    /// All of them, by id (the order they were added).
    pub fn iter(&self) -> impl Iterator<Item = &Mark> {
        self.marks.values().inspect(|_| {
            #[cfg(test)]
            visited(1);
        })
    }

    /// Follow a batch; returns how the marks it collapsed were before.
    pub(crate) fn follow(&mut self, changes: &[Change]) -> Snapshot {
        let mut moved = Snapshot::new();
        for m in self.marks.values_mut() {
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
        if !changes.is_empty() {
            self.reindex();
        }
        moved
    }

    /// Image `id` now has this size (read again), or is gone.
    pub fn set_image(&mut self, id: u64, size: Option<(u32, u32)>) {
        for mark in &self.images {
            let m = self.marks.get_mut(mark).expect("indexed");
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
            if let Some(m) = self.marks.get(&id) {
                let (old_start, old_end) = (m.start, m.end);
                self.unplace(id, old_start, old_end);
                self.place(id, start, end);
                let m = self.marks.get_mut(&id).expect("there");
                (m.start, m.end, m.deleted) = (start, end, deleted);
            }
        }
    }

    fn place(&mut self, id: MarkId, start: usize, end: usize) {
        if end - start <= SHORT {
            self.by_start.insert((start, id), end);
        } else {
            self.long.insert(id);
        }
    }

    fn unplace(&mut self, id: MarkId, start: usize, end: usize) {
        if end - start <= SHORT {
            self.by_start.remove(&(start, id));
        } else {
            self.long.remove(&id);
        }
    }

    /// The position index built again (every mark moved).
    fn reindex(&mut self) {
        let (mut short, mut long) = (Vec::new(), BTreeSet::new());
        for m in self.marks.values() {
            if m.end - m.start <= SHORT {
                short.push(((m.start, m.id), m.end));
            } else {
                long.insert(m.id);
            }
        }
        // Mostly in order already (by id, which is mostly by position);
        // a sorted input builds the map in one pass.
        short.sort_unstable_by_key(|e| e.0);
        self.by_start = short.into_iter().collect();
        self.long = long;
    }
}

/// Whether a spec shows an image anywhere.
fn has_image(s: &MarkSpec) -> bool {
    s.virt_image.is_some()
        || s.conceal.as_ref().is_some_and(|c| c.image.is_some())
        || s.virt_lines.as_ref().is_some_and(|v| v.image.is_some())
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

    /// A tiny deterministic generator (xorshift) for the property test.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n.max(1) as u64) as usize
        }
    }

    /// The indexes say what a scan over all of them would.
    fn check(m: &Marks, len: usize, rng: &mut Rng) {
        let all: Vec<&Mark> = m.marks.values().collect();
        for _ in 0..8 {
            let a = rng.below(len + 1);
            let b = (a + rng.below(SHORT * 3)).min(len);
            let mut want: Vec<MarkId> = all.iter().filter(|x| x.start <= b && x.end >= a).map(|x| x.id).collect();
            let mut got: Vec<MarkId> = m.touching(a, b).map(|x| x.id).collect();
            want.sort();
            got.sort();
            assert_eq!(got, want, "touching({a}, {b})");
        }
        let ids = |it: &mut dyn Iterator<Item = &Mark>| it.map(|x| x.id).collect::<Vec<_>>();
        let virt = |x: &Mark| x.spec.virt_pos == VirtPos::Inline && !x.spec.virt_text.is_empty();
        assert_eq!(ids(&mut m.inline()), ids(&mut all.iter().copied().filter(|x| virt(x))));
        assert_eq!(ids(&mut m.concealing()), ids(&mut all.iter().copied().filter(|x| x.spec.conceal.is_some())));
        assert_eq!(
            ids(&mut m.with_virt_lines()),
            ids(&mut all.iter().copied().filter(|x| x.spec.virt_lines.is_some()))
        );
        let soonest = all.iter().filter_map(|x| x.spec.expires).min();
        assert_eq!(m.next_expiry(), soonest);
        assert_eq!(m.by_start.len() + m.long.len(), all.len(), "each indexed once");
    }

    #[test]
    fn the_indexes_follow_adds_moves_removals_edits_and_undo() {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut t = Text::from("abcdefghij\n".repeat(200).as_str());
        let mut m = Marks::default();
        let now = Instant::now();
        let mut snaps = Vec::new();
        for step in 0..600 {
            let len = t.len_chars();
            let a = rng.below(len + 1);
            // Short and long ones (past SHORT: the list of their own).
            let b = (a + if rng.below(4) == 0 { rng.below(SHORT * 4) } else { rng.below(12) }).min(len);
            let ids: Vec<MarkId> = m.marks.keys().copied().collect();
            let any = |rng: &mut Rng| ids.get(rng.below(ids.len())).copied().unwrap_or(0);
            match rng.below(10) {
                0..=3 => {
                    let spec = MarkSpec {
                        namespace: ["a", "b"][rng.below(2)].into(),
                        virt_text: if rng.below(5) == 0 {
                            vec![VirtChunk { text: "v".into(), style: None }]
                        } else {
                            vec![]
                        },
                        conceal: (rng.below(6) == 0).then(Conceal::default),
                        virt_lines: (rng.below(7) == 0).then(VirtLines::default),
                        expires: (rng.below(8) == 0).then(|| now + std::time::Duration::from_secs(rng.below(5) as u64)),
                        start_gravity: if rng.below(2) == 0 { Gravity::Before } else { Gravity::After },
                        end_gravity: if rng.below(2) == 0 { Gravity::Before } else { Gravity::After },
                        ..MarkSpec::default()
                    };
                    m.add(b, a, spec);
                }
                4 => assert_eq!(m.set(any(&mut rng), a, b), !ids.is_empty()),
                5 => assert_eq!(m.remove(any(&mut rng)), !ids.is_empty()),
                6 | 7 => {
                    let text = "xyz\n"[..rng.below(5)].to_owned();
                    snaps.push(edit(&mut t, &mut m, vec![TextEdit::new(a, b, &text)]));
                }
                8 => {
                    if let Some(s) = snaps.pop() {
                        m.restore(&s);
                    }
                }
                _ => match step % 3 {
                    0 => m.clear("a"),
                    1 => _ = m.expire(now + std::time::Duration::from_secs(2)),
                    _ => _ = m.get(any(&mut rng)),
                },
            }
            check(&m, t.len_chars(), &mut rng);
        }
    }

    #[test]
    fn a_range_visits_only_the_marks_near_it() {
        // One mark per line of 10 000 (a highlighted buffer) and a few
        // long ones: a screenful's query looks at what is near it.
        let mut m = Marks::default();
        for line in 0..10_000 {
            m.add(line * 5, line * 5 + 4, MarkSpec::default());
        }
        for i in 0..3 {
            m.add(i, 50_000 - i, MarkSpec::default());
        }
        let before = VISITED.get();
        let shown = m.overlapping(25_000, 25_100);
        let visited = VISITED.get() - before;
        assert_eq!(shown.len(), 21 + 3);
        assert!(visited <= 21 + SHORT / 5 + 1 + 3, "{visited} visited");
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
