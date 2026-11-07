//! The undo tree: every state the buffer was in stays reachable; undo and
//! redo follow the current branch (the most recent child).
//!
//! Grouping: one step per invocation (the session calls
//! [`UndoTree::invocation`] before each), consecutive typing merged into one
//! step; [`UndoTree::begin`]/[`UndoTree::end`] group across invocations (a
//! Vim Insert session) and [`UndoTree::split`] splits inside one.

use crate::cursor::Cursor;
use crate::edit::Change;
use crate::marks::Snapshot;

/// What made a step, for merging consecutive typing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StepKind {
    /// Text typed into the buffer (keys no mapping took, IME, paste).
    Typing,
    #[default]
    Other,
}

/// One batch of changes and the marks it collapsed.
#[derive(Clone, Debug)]
pub(crate) struct Batch {
    pub changes: Vec<Change>,
    pub marks: Snapshot,
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub parent: usize,
    /// Where redo goes: the child made or visited last.
    pub redo: Option<usize>,
    pub batches: Vec<Batch>,
    /// The cursor before the step and after it, and the pane it was in.
    pub before: Cursor,
    pub after: Cursor,
    pub pane: Option<u32>,
    pub kind: StepKind,
}

#[derive(Clone, Debug)]
pub struct UndoTree {
    /// `nodes[0]` is the state the buffer started in.
    nodes: Vec<Node>,
    current: usize,
    saved: Option<usize>,
    /// The current node takes more batches (no boundary since it was made).
    open: bool,
    /// Explicit groups begun and not ended.
    depth: usize,
    /// The last step was typing and nothing came between: more typing
    /// joins it.
    typing_tail: bool,
}

impl Default for UndoTree {
    fn default() -> Self {
        let root = Node {
            parent: 0,
            redo: None,
            batches: Vec::new(),
            before: Cursor::default(),
            after: Cursor::default(),
            pane: None,
            kind: StepKind::Other,
        };
        Self { nodes: vec![root], current: 0, saved: Some(0), open: false, depth: 0, typing_tail: false }
    }
}

impl UndoTree {
    /// A new invocation starts: its changes are a step of their own (unless
    /// an explicit group is open, or it is typing after typing).
    pub fn invocation(&mut self) {
        if self.depth == 0 {
            self.typing_tail = self.open && self.nodes[self.current].kind == StepKind::Typing;
            self.open = false;
        }
    }

    /// Group what follows into one step, until [`end`](Self::end).
    pub fn begin(&mut self) {
        if self.depth == 0 {
            self.open = false;
            self.typing_tail = false;
        }
        self.depth += 1;
    }

    pub fn end(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 {
            self.open = false;
        }
    }

    /// The cursor moved by itself (not by an edit): typing after it is a
    /// step of its own (inside a group it still joins the group).
    pub fn cursor_moved(&mut self) {
        if self.depth == 0 && self.nodes[self.current].kind == StepKind::Typing {
            self.open = false;
        }
        self.typing_tail = false;
    }

    /// End the step here, even inside a group.
    pub fn split(&mut self) {
        self.open = false;
        self.typing_tail = false;
    }

    pub(crate) fn record(&mut self, batch: Batch, kind: StepKind, before: Cursor, after: Cursor, pane: Option<u32>) {
        let joins = self.open || (kind == StepKind::Typing && self.typing_tail);
        if joins && self.current != 0 {
            let node = &mut self.nodes[self.current];
            node.batches.push(batch);
            node.after = after;
        } else {
            let id = self.nodes.len();
            self.nodes.push(Node { parent: self.current, redo: None, batches: vec![batch], before, after, pane, kind });
            self.nodes[self.current].redo = Some(id);
            self.current = id;
        }
        self.open = true;
        self.typing_tail = false;
    }

    /// Step back: the node to undo (its batches in reverse), and close any
    /// group left open.
    pub(crate) fn back(&mut self) -> Option<Node> {
        self.depth = 0;
        self.open = false;
        self.typing_tail = false;
        if self.current == 0 {
            return None;
        }
        let node = self.nodes[self.current].clone();
        self.nodes[node.parent].redo = Some(self.current);
        self.current = node.parent;
        Some(node)
    }

    /// Step forward along the current branch.
    pub(crate) fn forward(&mut self) -> Option<Node> {
        self.depth = 0;
        self.open = false;
        self.typing_tail = false;
        let next = self.nodes[self.current].redo?;
        self.current = next;
        Some(self.nodes[next].clone())
    }

    /// Record a batch's marks again after a redo (they collapse anew).
    pub(crate) fn set_marks(&mut self, batch: usize, marks: Snapshot) {
        if let Some(b) = self.nodes[self.current].batches.get_mut(batch) {
            b.marks = marks;
        }
    }

    pub(crate) fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub(crate) fn current(&self) -> usize {
        self.current
    }

    pub(crate) fn saved(&self) -> Option<usize> {
        self.saved
    }

    /// A tree read back (an undo file): at node `at`, which is saved.
    pub(crate) fn restored(nodes: Vec<Node>, at: usize) -> Self {
        Self { nodes, current: at, saved: Some(at), open: false, depth: 0, typing_tail: false }
    }

    /// The buffer was saved as it is now. A change after it is a step of
    /// its own (joining the saved one, the buffer would look unmodified).
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.current);
        self.open = false;
        self.typing_tail = false;
    }

    /// Differs from what was saved (or read).
    pub fn modified(&self) -> bool {
        self.saved != Some(self.current)
    }

    pub fn can_undo(&self) -> bool {
        self.current != 0
    }

    pub fn can_redo(&self) -> bool {
        self.nodes[self.current].redo.is_some()
    }

    /// States reachable (the starting one included).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }
}
