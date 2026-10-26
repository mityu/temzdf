//! Tab pages, as in Vim: each has its own layout, focus and floats shown or
//! hidden; buffers are shared. As Vim keeps the current tab's windows in
//! globals (`firstwin`, `curwin`), the current tab's state lives in the
//! session's fields (`layout`, `focus`, `tiled_focus`, `floats_hidden`) and
//! the other tabs keep theirs in [`Tab::saved`]; switching swaps them.

use super::Session;
use crate::layout::{Layout, PaneId};

pub(super) type TabId = u32;

/// Which tabs a float shows on (Vim's popup `tabpage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FloatTab {
    Only(TabId),
    All,
}

pub(super) struct Tab {
    pub id: TabId,
    /// Set with `rename-tab`; the tab line shows the focused buffer else.
    #[allow(dead_code)]
    pub name: Option<String>,
    /// The tab's state while another tab is current.
    saved: Option<Saved>,
}

impl Tab {
    pub(super) fn new(id: TabId) -> Self {
        Self { id, name: None, saved: None }
    }
}

/// What the session's fields hold for the current tab.
#[derive(Default)]
struct Saved {
    layout: Option<Layout>,
    floats_hidden: bool,
    focus: PaneId,
    tiled_focus: PaneId,
}

impl Session {
    /// The current tab's id.
    pub(super) fn tab_id(&self) -> TabId {
        self.tabs[self.tab].id
    }

    /// Every tab's layout (index, layout), the current one included.
    pub(super) fn layouts(&self) -> impl Iterator<Item = (usize, &Layout)> {
        self.tabs.iter().enumerate().filter_map(move |(i, t)| {
            if i == self.tab { self.layout.as_ref() } else { t.saved.as_ref()?.layout.as_ref() }.map(|l| (i, l))
        })
    }

    /// Whether tab `i`'s floats are hidden.
    pub(super) fn tab_floats_hidden(&self, i: usize) -> bool {
        if i == self.tab { self.floats_hidden } else { self.tabs[i].saved.as_ref().is_some_and(|s| s.floats_hidden) }
    }

    /// The index of tab `id`.
    pub(super) fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Whether a float on `tab` shows on the current tab (shown or hidden).
    pub(super) fn here(&self, tab: FloatTab) -> bool {
        tab == FloatTab::All || tab == FloatTab::Only(self.tab_id())
    }

    /// Whether `pane` is on screen in some tab: a tiled pane of any tab, or
    /// a float its tab shows (a float on all tabs: the current tab's say).
    pub(super) fn shown_anywhere(&self, pane: PaneId) -> bool {
        match self.floats.iter().find(|f| f.pane == pane) {
            Some(f) => match f.tab {
                FloatTab::All => !self.floats_hidden,
                FloatTab::Only(t) => self.tab_index(t).is_some_and(|i| !self.tab_floats_hidden(i)),
            },
            None => self.layouts().any(|(_, l)| l.panes().contains(&pane)),
        }
    }

    /// Make tab `i` current: its state goes into the session's fields, the
    /// current tab's into its record. Nothing else (focus reports, events).
    #[allow(dead_code)]
    pub(super) fn swap_tab(&mut self, i: usize) {
        if i == self.tab || i >= self.tabs.len() {
            return;
        }
        let current = Saved {
            layout: self.layout.take(),
            floats_hidden: self.floats_hidden,
            focus: self.focus,
            tiled_focus: self.tiled_focus,
        };
        self.tabs[self.tab].saved = Some(current);
        let next = self.tabs[i].saved.take().unwrap_or_default();
        self.layout = next.layout;
        self.floats_hidden = next.floats_hidden;
        self.focus = next.focus;
        self.tiled_focus = next.tiled_focus;
        self.tab = i;
    }
}
