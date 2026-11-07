//! The undo tree kept in a file (Vim's undofile): written when the buffer
//! is saved, read back when the file is opened again, if the file still
//! holds what was saved (a hash of its text) and the path is the same.
//! JSON, versioned. Not kept: the marks a step collapsed (marks are not
//! kept across sessions) and the panes steps were made in.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cursor::{Cursor, SelectionKind};
use crate::edit::Change;
use crate::text::Text;
use crate::undo::{Batch, Node, StepKind, UndoTree};

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    /// The file it is for (checked when read back).
    path: String,
    /// FNV-1a of the text as saved ([`text_hash`]).
    hash: String,
    current: usize,
    saved: Option<usize>,
    nodes: Vec<FileNode>,
}

#[derive(Serialize, Deserialize)]
struct FileNode {
    parent: usize,
    redo: Option<usize>,
    /// Each batch's changes: (start, removed, inserted).
    batches: Vec<Vec<(usize, String, String)>>,
    before: FileCursor,
    after: FileCursor,
    typing: bool,
}

#[derive(Serialize, Deserialize)]
struct FileCursor {
    pos: usize,
    anchor: Option<(usize, u8)>,
    want: Option<usize>,
}

impl From<&Cursor> for FileCursor {
    fn from(c: &Cursor) -> Self {
        let kind = |k: SelectionKind| match k {
            SelectionKind::Char => 0,
            SelectionKind::Line => 1,
            SelectionKind::Block => 2,
        };
        Self { pos: c.pos, anchor: c.anchor.map(|(a, k)| (a, kind(k))), want: c.want_col }
    }
}

impl From<&FileCursor> for Cursor {
    fn from(c: &FileCursor) -> Self {
        let kind = |k: u8| match k {
            1 => SelectionKind::Line,
            2 => SelectionKind::Block,
            _ => SelectionKind::Char,
        };
        Cursor { pos: c.pos, anchor: c.anchor.map(|(a, k)| (a, kind(k))), want_col: c.want }
    }
}

/// A 64-bit FNV-1a of `bytes`, as 16 hex digits: stable across builds (file
/// names and the text check).
pub fn fnv1a(bytes: impl IntoIterator<Item = u8>) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn text_hash(text: &Text) -> String {
    fnv1a(text.rope().bytes())
}

impl UndoTree {
    /// The tree as the undo file of `path` holding `text` (as just saved).
    pub fn to_file(&self, text: &Text, path: &Path) -> String {
        let nodes = self
            .nodes()
            .iter()
            .map(|n| FileNode {
                parent: n.parent,
                redo: n.redo,
                batches: n
                    .batches
                    .iter()
                    .map(|b| b.changes.iter().map(|c| (c.start, c.removed.clone(), c.inserted.clone())).collect())
                    .collect(),
                before: (&n.before).into(),
                after: (&n.after).into(),
                typing: n.kind == StepKind::Typing,
            })
            .collect();
        let file = File {
            version: VERSION,
            path: path.to_string_lossy().into_owned(),
            hash: text_hash(text),
            current: self.current(),
            saved: self.saved(),
            nodes,
        };
        serde_json::to_string(&file).unwrap_or_default()
    }

    /// The tree an undo file holds, for `path` read as `text`: refused when
    /// it is for another file, another version, or text other than this
    /// (changed elsewhere since). It starts where it was saved.
    pub fn from_file(json: &str, text: &Text, path: &Path) -> Result<UndoTree, String> {
        let file: File = serde_json::from_str(json).map_err(|e| format!("not an undo file: {e}"))?;
        if file.version != VERSION {
            return Err(format!("undo file version {} (this reads {VERSION})", file.version));
        }
        if file.path != path.to_string_lossy() {
            return Err("an undo file of another file".into());
        }
        if file.hash != text_hash(text) {
            return Err("the file changed since its undo file was written".into());
        }
        let n = file.nodes.len();
        let saved = file.saved.ok_or("no saved state")?;
        if n == 0 || saved >= n || file.current >= n {
            return Err("a broken undo file".into());
        }
        let mut nodes = Vec::with_capacity(n);
        for (i, f) in file.nodes.iter().enumerate() {
            let ok = (i == 0 || f.parent < i) && f.redo.is_none_or(|r| r < n && r > 0);
            if !ok {
                return Err("a broken undo file".into());
            }
            nodes.push(Node {
                parent: f.parent,
                redo: f.redo,
                batches: f
                    .batches
                    .iter()
                    .map(|b| Batch {
                        changes: b
                            .iter()
                            .map(|(start, removed, inserted)| Change {
                                start: *start,
                                removed: removed.clone(),
                                inserted: inserted.clone(),
                            })
                            .collect(),
                        marks: Vec::new(),
                    })
                    .collect(),
                before: (&f.before).into(),
                after: (&f.after).into(),
                pane: None,
                kind: if f.typing { StepKind::Typing } else { StepKind::Other },
            });
        }
        // The text read is the saved state: the tree goes on from there.
        Ok(UndoTree::restored(nodes, saved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{Buffer, Origin};
    use crate::edit::TextEdit;
    use crate::marks::MarkSpec;
    use crate::text::LineEnding;

    fn edit(b: &mut Buffer, e: TextEdit) {
        b.undo_tree_mut().invocation();
        let o = Origin { kind: StepKind::Other, pane: Some(1), cursor: Cursor::at(e.start) };
        b.edit(vec![e], o).unwrap();
    }

    /// A buffer with a branch: "" -> "a" -> "ab" (undone) and "a" -> "ac",
    /// saved at "ac".
    fn branched() -> Buffer {
        let mut b = Buffer::new(Text::from(""), LineEnding::Lf);
        edit(&mut b, TextEdit::new(0, 0, "a"));
        edit(&mut b, TextEdit::new(1, 1, "b"));
        b.undo().unwrap();
        edit(&mut b, TextEdit::new(1, 1, "c"));
        b.undo_tree_mut().mark_saved();
        b
    }

    #[test]
    fn a_tree_comes_back_with_its_branches_cursors_and_saved_state() {
        let path = Path::new("/notes/a.txt");
        let b = branched();
        let json = b.undo_tree().to_file(b.text(), path);
        // Opened again: the text read is what was saved.
        let mut r = Buffer::new(Text::from("ac"), LineEnding::Lf);
        *r.undo_tree_mut() = UndoTree::from_file(&json, r.text(), path).unwrap();
        assert!(!r.modified());
        assert_eq!(r.undo_tree().len(), 4);
        let u = r.undo().unwrap();
        assert_eq!(r.text().to_string(), "a");
        assert_eq!(u.cursor.unwrap().1.pos, 1, "where the step began");
        assert!(r.modified());
        r.undo().unwrap();
        assert_eq!(r.text().to_string(), "");
        assert!(r.undo().is_none());
        // Redo follows the branch last taken.
        r.redo().unwrap();
        r.redo().unwrap();
        assert_eq!(r.text().to_string(), "ac");
        assert!(!r.modified());
    }

    #[test]
    fn an_undo_file_of_other_text_another_file_or_version_is_refused() {
        let path = Path::new("/notes/a.txt");
        let b = branched();
        let json = b.undo_tree().to_file(b.text(), path);
        let changed = Text::from("ac!");
        assert!(UndoTree::from_file(&json, &changed, path).unwrap_err().contains("changed"));
        assert!(UndoTree::from_file(&json, b.text(), Path::new("/notes/b.txt")).is_err());
        let old = json.replacen("\"version\":1", "\"version\":0", 1);
        assert!(UndoTree::from_file(&old, b.text(), path).is_err());
        assert!(UndoTree::from_file("{", b.text(), path).is_err());
    }

    #[test]
    fn marks_are_not_kept_and_undo_still_works() {
        let path = Path::new("/notes/m.txt");
        let mut b = Buffer::new(Text::from("keep gone"), LineEnding::Lf);
        b.marks_mut().add(5, 9, MarkSpec::default());
        edit(&mut b, TextEdit::new(5, 9, ""));
        b.undo_tree_mut().mark_saved();
        let json = b.undo_tree().to_file(b.text(), path);
        let mut r = Buffer::new(Text::from("keep "), LineEnding::Lf);
        *r.undo_tree_mut() = UndoTree::from_file(&json, r.text(), path).unwrap();
        r.undo().unwrap();
        assert_eq!(r.text().to_string(), "keep gone");
    }
}
