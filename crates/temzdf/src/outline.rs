//! The outline (table of contents) panel: which entries are expanded, which are
//! visible, and the list widget state derived from that.

use termpix_ui::{ListItem, ListView};

use crate::layout::GAP_PT;
use crate::pdf::{OutlineEntry, Target};

pub struct OutlinePanel {
    pub list: ListView,
    expanded: Vec<bool>,
    /// Outline indices of the visible rows.
    visible: Vec<usize>,
}

impl OutlinePanel {
    pub fn new(entries: &[OutlineEntry], labels: &[String]) -> Self {
        // Everything open for small outlines; top level only for big ones.
        let open = entries.len() <= 60;
        let expanded = entries.iter().map(|e| open || e.depth > 0).collect();
        let mut p = Self {
            list: ListView { title: Some("Contents".into()), ..Default::default() },
            expanded,
            visible: Vec::new(),
        };
        p.rebuild(entries, labels);
        p
    }

    fn rebuild(&mut self, entries: &[OutlineEntry], labels: &[String]) {
        let selected = self.selected_entry();
        self.visible.clear();
        let mut i = 0;
        while i < entries.len() {
            self.visible.push(i);
            let e = &entries[i];
            i += if self.expanded[i] { 1 } else { 1 + e.descendants };
        }
        self.list.items = self
            .visible
            .iter()
            .map(|&i| {
                let e = &entries[i];
                ListItem {
                    label: e.title.clone(),
                    detail: e.target.and_then(|t| labels.get(t.page).cloned()).unwrap_or_default(),
                    depth: e.depth,
                    expanded: (e.descendants > 0).then_some(self.expanded[i]),
                }
            })
            .collect();
        self.list.selected = selected.and_then(|s| self.visible.iter().position(|&v| v == s));
        self.list.hover = None;
    }

    /// Outline index of the selected row.
    pub fn selected_entry(&self) -> Option<usize> {
        self.list.selected.and_then(|r| self.visible.get(r).copied())
    }

    pub fn entry_at_row(&self, row: usize) -> Option<usize> {
        self.visible.get(row).copied()
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let cur = self.list.selected.map_or(-1, |s| s as isize);
        let next = if cur < 0 && delta < 0 { self.visible.len() as isize - 1 } else { cur + delta };
        self.list.selected = Some(next.clamp(0, self.visible.len() as isize - 1) as usize);
    }

    pub fn select_first(&mut self) {
        self.list.selected = (!self.visible.is_empty()).then_some(0);
    }

    pub fn select_last(&mut self) {
        self.list.selected = self.visible.len().checked_sub(1);
    }

    /// Expand or collapse entry `index`. Returns true if anything changed.
    pub fn set_expanded(&mut self, entries: &[OutlineEntry], labels: &[String], index: usize, open: bool) -> bool {
        if entries[index].descendants == 0 || self.expanded[index] == open {
            return false;
        }
        self.expanded[index] = open;
        self.rebuild(entries, labels);
        true
    }

    pub fn is_expanded(&self, index: usize) -> bool {
        self.expanded[index]
    }

    /// Select the entry for the reading position: the last entry starting at or
    /// before `pos`. Collapsed ancestors are left alone; the nearest visible
    /// ancestor is selected instead.
    pub fn select_position(&mut self, entries: &[OutlineEntry], pos: Target) {
        let here_y = pos.y.unwrap_or(0.0);
        let mut best = None;
        for (i, e) in entries.iter().enumerate() {
            let Some(t) = e.target else { continue };
            // Viewer::goto puts targets slightly below the screen top; allow for that.
            if t.page < pos.page || (t.page == pos.page && t.y.unwrap_or(0.0) <= here_y + 5.0 * GAP_PT) {
                best = Some(i);
            }
        }
        let Some(mut i) = best else {
            self.select_first();
            return;
        };
        loop {
            if let Some(row) = self.visible.iter().position(|&v| v == i) {
                self.list.selected = Some(row);
                return;
            }
            match parent(entries, i) {
                Some(p) => i = p,
                None => return,
            }
        }
    }
}

/// The parent entry of `index` in a flattened outline.
pub fn parent(entries: &[OutlineEntry], index: usize) -> Option<usize> {
    let depth = entries[index].depth;
    (0..index).rev().find(|&i| entries[i].depth < depth && i + entries[i].descendants >= index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> (Vec<OutlineEntry>, Vec<String>) {
        let e = |title: &str, depth, page, descendants| OutlineEntry {
            title: title.into(),
            depth,
            target: Some(Target { page, y: Some(100.0) }),
            descendants,
        };
        (
            vec![e("A", 0, 0, 0), e("B", 0, 1, 2), e("B.1", 1, 1, 0), e("B.2", 1, 2, 0), e("C", 0, 3, 0)],
            vec!["i".into(), "1".into(), "2".into(), "3".into()],
        )
    }

    #[test]
    fn collapse_expand_and_select() {
        let (es, labels) = entries();
        let mut p = OutlinePanel::new(&es, &labels);
        assert_eq!(p.list.items.len(), 5);
        assert_eq!(p.list.items[3].detail, "2");
        p.select_position(&es, Target { page: 2, y: Some(500.0) });
        assert_eq!(p.selected_entry(), Some(3));
        assert!(p.set_expanded(&es, &labels, 1, false));
        assert_eq!(p.list.items.len(), 3);
        // The selected child is hidden; selecting the position falls back to its parent.
        p.select_position(&es, Target { page: 2, y: Some(500.0) });
        assert_eq!(p.selected_entry(), Some(1));
        assert_eq!(parent(&es, 3), Some(1));
        assert_eq!(parent(&es, 4), None);
    }
}
