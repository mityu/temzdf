//! Tab pages, as in Vim: each has its own layout, focus and floats shown or
//! hidden; buffers are shared. As Vim keeps the current tab's windows in
//! globals (`firstwin`, `curwin`), the current tab's state lives in the
//! session's fields (`layout`, `focus`, `tiled_focus`, `floats_hidden`) and
//! the other tabs keep theirs in [`Tab::saved`]; switching swaps them.

use temzdf_protocol::script::ScriptValue;
use temzdf_scene::{Attrs, Cell, Color, TextGrid};
use termpix_core::input::{MouseButton, MouseEvent, MouseEventKind};

use super::{BACKGROUND, Outcome, PaneEntry, Session};
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

    /// Show tab `i`'s floats (a new one opened there).
    pub(super) fn show_tab_floats(&mut self, i: usize) {
        if i == self.tab {
            self.floats_hidden = false;
        } else if let Some(saved) = self.tabs.get_mut(i).and_then(|t| t.saved.as_mut()) {
            saved.floats_hidden = false;
        }
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

    /// The tab `pane` is in: its layout's, or the tabs a float shows on
    /// (the current one for a float on all tabs).
    pub(super) fn tab_of(&self, pane: PaneId) -> Option<usize> {
        if let Some(f) = self.floats.iter().find(|f| f.pane == pane) {
            return match f.tab {
                FloatTab::All => Some(self.tab),
                FloatTab::Only(t) => self.tab_index(t),
            };
        }
        self.layouts().find(|(_, l)| l.panes().contains(&pane)).map(|(i, _)| i)
    }

    /// Tab `i`'s focused pane.
    fn tab_focus(&self, i: usize) -> PaneId {
        if i == self.tab { self.focus } else { self.tabs[i].saved.as_ref().map_or(0, |s| s.focus) }
    }

    /// What the tab line shows for tab `i`: its name, else its focused
    /// pane's buffer name.
    pub(super) fn tab_label(&self, i: usize) -> String {
        self.tabs[i].name.clone().unwrap_or_else(|| self.pane_name(self.tab_focus(i)))
    }

    /// Tell a terminal shown in `pane` that it gained or lost the focus.
    pub(super) fn term_focus(&mut self, pane: PaneId, gained: bool) {
        if let Some(t) = self.panes.get(&pane).and_then(PaneEntry::term).and_then(|b| self.terms.get_mut(&b)) {
            t.term.focus(gained);
        }
    }

    /// The current tab's focus on a pane it shows (after its panes changed
    /// while it was not current): the tiled pane focused last, else the first.
    fn fix_focus(&mut self) {
        let tiled = self.layout.as_ref().map(|l| l.panes()).unwrap_or_default();
        if !tiled.contains(&self.tiled_focus) {
            self.tiled_focus = tiled.first().copied().unwrap_or(0);
        }
        let shown = tiled.contains(&self.focus) || self.shown_floats().iter().any(|f| f.pane == self.focus);
        if !shown {
            self.focus = self.tiled_focus;
        }
    }

    /// Go to tab `i`: its focused pane gets the focus (and the mode).
    pub(super) fn enter_tab(&mut self, i: usize) {
        if i == self.tab || i >= self.tabs.len() {
            return;
        }
        let old = self.focus;
        self.term_focus(old, false);
        self.swap_tab(i);
        self.fix_focus();
        // Pointer state belonged to the other tab's panes.
        self.dragging = None;
        self.float_drag = None;
        self.hovered = None;
        self.captured = None;
        self.focus_click = false;
        self.over_border = None;
        self.over_frame = None;
        let focus = self.focus;
        self.term_focus(focus, true);
        self.event("TabEntered", vec![ScriptValue::Num((i + 1) as f64)]);
        if self.panes.contains_key(&focus) && focus != old {
            self.event("PaneFocused", vec![ScriptValue::Num(focus as f64)]);
        }
        let mode = self.history.current().to_owned();
        self.history.reset(self.pane_mode(focus));
        self.mode_changed(&mode);
        self.relayout();
    }

    /// A new, empty tab after the current one, made current (its first
    /// pane goes in next).
    pub(super) fn open_tab(&mut self) {
        let id = self.next_tab;
        self.next_tab += 1;
        self.tabs.insert(self.tab + 1, Tab::new(id));
        self.enter_tab(self.tab + 1);
    }

    /// Close tab `i` with its panes (its own floats included); their buffers
    /// stay, hidden. The tab after it becomes current if it was (Vim). The
    /// last tab: the session ends.
    /// The panes that go with tab `i`: its tiled ones and its own floats.
    pub(super) fn tab_panes(&self, i: usize) -> Vec<PaneId> {
        let id = self.tabs[i].id;
        let mut panes = self.layouts().find(|(t, _)| *t == i).map(|(_, l)| l.panes()).unwrap_or_default();
        panes.extend(self.floats.iter().filter(|f| f.tab == FloatTab::Only(id)).map(|f| f.pane));
        panes
    }

    pub(super) fn remove_tab(&mut self, i: usize) -> Outcome {
        if self.tabs.len() <= 1 {
            return Outcome::Quit;
        }
        if i == self.tab {
            self.enter_tab(if i + 1 < self.tabs.len() { i + 1 } else { i - 1 });
        }
        let id = self.tabs[i].id;
        let panes = self.tab_panes(i);
        self.floats.retain(|f| f.tab != FloatTab::Only(id));
        self.tabs.remove(i);
        if i < self.tab {
            self.tab -= 1;
        }
        for p in panes {
            self.discard_pane(p);
        }
        self.event("TabClosed", vec![ScriptValue::Num((i + 1) as f64)]);
        self.relayout();
        Outcome::Redraw
    }

    /// Forget pane `id` (already out of its layout or the floats): its
    /// buffer is hidden unless shown elsewhere.
    pub(super) fn discard_pane(&mut self, id: PaneId) {
        if let Some(e) = self.panes.remove(&id) {
            if id == self.focus
                && let Some(t) = e.term().and_then(|b| self.terms.get_mut(&b))
            {
                t.term.focus(false);
            }
            self.hide(e.kind);
        }
        self.event("PaneClosed", vec![ScriptValue::Num(id as f64)]);
        for p in [&mut self.hovered, &mut self.captured] {
            if *p == Some(id) {
                *p = None;
            }
        }
    }

    /// Close pane `id` of tab `i`, not the current one: as [`Session::close`]
    /// there, without moving the focus (the tab fixes it when entered).
    pub(super) fn close_in_tab(&mut self, i: usize, id: PaneId) -> Outcome {
        let back = self.tab_id();
        self.swap_tab(i);
        let mut gone = false;
        if self.is_float(id) {
            self.floats.retain(|f| f.pane != id);
        } else if let Some(layout) = self.layout.as_mut() {
            if layout.panes().len() <= 1 {
                // Its own shown float takes the place, else the tab closes.
                let tab = FloatTab::Only(self.tabs[i].id);
                let top = (!self.floats_hidden)
                    .then(|| self.floats.iter().rev().find(|f| f.tab == tab).map(|f| f.pane))
                    .flatten();
                match top {
                    Some(top) => {
                        layout.replace(id, top);
                        self.floats.retain(|f| f.pane != top);
                    }
                    None => gone = true,
                }
            } else {
                layout.remove(id);
            }
        }
        let back = self.tab_index(back).unwrap_or(0);
        if gone {
            self.swap_tab(back);
            return self.remove_tab(i);
        }
        // Not focused there any more: forget it as that tab's focus too.
        if self.focus == id {
            self.focus = 0;
        }
        self.fix_focus();
        self.swap_tab(back);
        let focus = self.focus;
        self.focus = 0;
        self.discard_pane(id);
        self.focus = focus;
        self.relayout();
        Outcome::Redraw
    }

    /// Move `pane` (of the current tab) to a new tab after it (Vim's
    /// `<C-w>T`). A tab's only tiled pane stays.
    pub(super) fn break_pane(&mut self, pane: PaneId) -> Result<(), String> {
        if !self.panes.contains_key(&pane) || self.tab_of(pane) != Some(self.tab) {
            return Err(format!("no pane {pane} in this tab"));
        }
        if self.is_float(pane) {
            self.floats.retain(|f| f.pane != pane);
        } else {
            let layout = self.layout.as_mut().ok_or("no tiled pane")?;
            if layout.panes().len() <= 1 {
                return Err("the tab's only tiled pane stays".into());
            }
            layout.remove(pane);
            if self.tiled_focus == pane {
                self.tiled_focus = layout.panes()[0];
            }
        }
        // The tab left behind keeps a focus it shows.
        self.term_focus(pane, false);
        self.focus = self.tiled_focus;
        self.open_tab();
        self.layout = Some(Layout::new(pane));
        self.set_focus(pane);
        self.relayout();
        Ok(())
    }

    /// Move the current tab as Vim's `:tabmove`: after tab `to` (counted
    /// from 1; 0: first), or by `+N`/`-N` places; `None`: last.
    pub(super) fn move_tab(&mut self, to: Option<&str>) -> Result<(), String> {
        let last = self.tabs.len() - 1;
        let target = match to.map(str::trim) {
            None | Some("") | Some("$") => last,
            Some(s) if s.starts_with('+') || s.starts_with('-') => {
                let n: i64 = s.parse().map_err(|_| format!("{s:?} is not +N or -N"))?;
                (self.tab as i64 + n).clamp(0, last as i64) as usize
            }
            Some(s) => {
                let n: usize = s.parse().map_err(|_| format!("{s:?} is not a tab number"))?;
                // After tab N, counted before the move.
                if n > self.tab { n.min(self.tabs.len()) - 1 } else { n }
            }
        };
        let t = self.tabs.remove(self.tab);
        self.tabs.insert(target, t);
        self.tab = target;
        Ok(())
    }

    /// The tabs, for `tabs()`.
    pub(super) fn tab_list(&self) -> ScriptValue {
        let list = (0..self.tabs.len())
            .map(|i| {
                let id = self.tabs[i].id;
                let mut panes = self.layouts().find(|(t, _)| *t == i).map(|(_, l)| l.panes()).unwrap_or_default();
                panes.extend(self.floats.iter().filter(|f| f.tab == FloatTab::Only(id)).map(|f| f.pane));
                ScriptValue::map([
                    ("index", ScriptValue::Num((i + 1) as f64)),
                    ("name", self.tabs[i].name.clone().map_or(ScriptValue::Null, ScriptValue::Str)),
                    ("label", ScriptValue::Str(self.tab_label(i))),
                    ("current", ScriptValue::Bool(i == self.tab)),
                    ("panes", ScriptValue::List(panes.into_iter().map(|p| ScriptValue::Num(p as f64)).collect())),
                ])
            })
            .collect();
        ScriptValue::List(list)
    }

    /// The panes on screen: the current tab's tiled panes and shown floats.
    pub(super) fn here_panes(&self) -> Vec<PaneId> {
        let mut v = self.layout.as_ref().map(|l| l.panes()).unwrap_or_default();
        v.extend(self.shown_floats().iter().map(|f| f.pane));
        v
    }

    /// A float's `tabpage` as Vim's popups take it: 0 the current tab, -1
    /// every tab, N tab page N.
    pub(super) fn float_tab(&self, v: &ScriptValue) -> Result<FloatTab, String> {
        let n = match v {
            ScriptValue::Num(n) => *n as i64,
            ScriptValue::Str(s) => s.trim().parse().map_err(|_| format!("tabpage: {s:?} is not a number"))?,
            other => return Err(format!("tabpage: {other:?} is not a number")),
        };
        Ok(match n {
            -1 => FloatTab::All,
            0 => FloatTab::Only(self.tab_id()),
            n if n > 0 => FloatTab::Only(self.tabs[self.tab_number(n as u32)?].id),
            n => return Err(format!("tabpage: {n} is not -1, 0 or a tab page")),
        })
    }

    /// The index of tab page `n` (counted from 1).
    pub(super) fn tab_number(&self, n: u32) -> Result<usize, String> {
        match (n as usize).checked_sub(1) {
            Some(i) if i < self.tabs.len() => Ok(i),
            _ => Err(format!("no tab page {n} (there are {})", self.tabs.len())),
        }
    }

    /// The number of tabs (for `temzdf ls`).
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Whether the tab line takes the top row.
    pub(super) fn tabline_shown(&self) -> bool {
        let wanted = match self.options.showtabline {
            0 => false,
            1 => self.tabs.len() > 1,
            _ => true,
        };
        wanted && self.geometry.rows > 1
    }

    /// The tab line, and the columns each tab takes on it.
    pub(super) fn tabline(&self) -> Option<(TextGrid, Vec<(usize, usize)>)> {
        if !self.tabline_shown() {
            return None;
        }
        let labels: Vec<(String, String)> =
            (0..self.tabs.len()).map(|i| (format!(" {} ", i + 1), self.tab_label(i))).collect();
        let (row, spans) = tabline_cells(&labels, self.tab, self.geometry.cols as usize);
        Some((TextGrid { col: 0, row: 0, rows: vec![row] }, spans))
    }

    /// Clicks on the tab line: the left button goes to the tab, the middle
    /// one closes it. `Some` when the event was the tab line's.
    pub(super) fn tabline_mouse(&mut self, m: &MouseEvent, col: i32, row: i32) -> Option<Outcome> {
        if row != 0 || self.captured.is_some() || self.dragging.is_some() || self.float_drag.is_some() {
            return None;
        }
        let (_, spans) = self.tabline()?;
        let tab = spans.iter().position(|&(a, b)| (a..b).contains(&(col.max(0) as usize)));
        Some(match (m.kind, tab) {
            (MouseEventKind::Press(MouseButton::Left), Some(i)) => {
                self.enter_tab(i);
                Outcome::Redraw
            }
            (MouseEventKind::Press(MouseButton::Middle), Some(i)) if self.tabs.len() > 1 => self.request_close_tab(i),
            _ => Outcome::None,
        })
    }
}

/// The tab line's cells for tabs (number, label), `current` highlighted,
/// `cols` wide, and the columns each tab takes. Labels shrink to fit, as
/// far as the numbers still do.
fn tabline_cells(tabs: &[(String, String)], current: usize, cols: usize) -> (Vec<Cell>, Vec<(usize, usize)>) {
    let width = |s: &str| s.chars().map(super::notices::width).sum::<usize>();
    let full: usize = tabs.iter().map(|(n, l)| width(n) + width(l) + 1).sum();
    // Room per label when they do not all fit.
    let room = if full <= cols {
        usize::MAX
    } else {
        let numbers: usize = tabs.iter().map(|(n, _)| width(n) + 1).sum();
        cols.saturating_sub(numbers) / tabs.len().max(1)
    };
    let bg = Color::Rgb([BACKGROUND[0], BACKGROUND[1], BACKGROUND[2]]);
    let mut row = Vec::new();
    let mut spans = Vec::new();
    for (i, (number, label)) in tabs.iter().enumerate() {
        let on = i == current;
        let fg = if on { Color::Rgb([0xee, 0xee, 0xee]) } else { Color::Rgb([0xaa, 0xaa, 0xaa]) };
        let bg = if on { Color::Rgb([0x5f, 0x5f, 0x66]) } else { bg };
        let attrs = Attrs { bold: on, ..Attrs::default() };
        let start = row.len();
        let mut used = 0;
        for (j, c) in number.chars().chain(label.chars().filter(|c| !c.is_control())).enumerate() {
            let w = super::notices::width(c);
            if j >= number.chars().count() && used + w > room {
                break;
            }
            if j >= number.chars().count() {
                used += w;
            }
            row.push(Cell { text: c.to_string(), width: w as u8, fg, bg, attrs });
            if w == 2 {
                row.push(Cell { text: String::new(), width: 0, fg, bg, attrs });
            }
        }
        row.push(Cell { text: " ".into(), width: 1, fg, bg, attrs });
        spans.push((start, row.len()));
    }
    // Cut at the screen's edge (a wide character there goes blank), then
    // fill the rest of the row.
    row.truncate(cols);
    if row.last().is_some_and(|c| c.width == 2) {
        let c = row.last_mut().expect("checked");
        (c.text, c.width) = (" ".into(), 1);
    }
    let blank = Cell { text: " ".into(), width: 1, fg: Color::Default, bg, attrs: Attrs::default() };
    row.resize(cols, blank);
    (row, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(row: &[Cell]) -> String {
        row.iter().map(|c| c.text.as_str()).collect::<String>().trim_end().to_owned()
    }

    #[test]
    fn the_tab_line_shows_numbers_and_labels_and_shrinks_them_to_fit() {
        let tabs = |l: &[&str]| -> Vec<(String, String)> {
            l.iter().enumerate().map(|(i, s)| (format!(" {} ", i + 1), (*s).to_owned())).collect()
        };
        let (row, spans) = tabline_cells(&tabs(&["shell", "paper.pdf"]), 1, 40);
        assert_eq!(text(&row), " 1 shell  2 paper.pdf");
        assert_eq!(spans, [(0, 9), (9, 22)]);
        assert_eq!(row.len(), 40);
        assert!(row[10].attrs.bold && !row[1].attrs.bold, "the current tab stands out");
        // Too narrow: the labels shrink, the numbers stay.
        let (row, spans) = tabline_cells(&tabs(&["abcdefgh", "ijklmnop"]), 0, 16);
        assert_eq!(text(&row), " 1 abcd  2 ijkl");
        assert_eq!(spans, [(0, 8), (8, 16)]);
        // Wide characters keep their spacer cell.
        let (row, _) = tabline_cells(&tabs(&["数式"]), 0, 20);
        assert_eq!(text(&row), " 1 数式");
        assert_eq!((row[3].width, row[4].width), (2, 0));
    }
}
