//! Terminal panes: a program on a pty, its output interpreted by Ghostty's
//! terminal emulation (`libghostty-vt`) and shown as text in a
//! `temzdf-scene` frame. See doc/session-server.md (step 3).

#[cfg(test)]
mod tests {
    use libghostty_vt::render::{CellIterator, RowIterator};
    use libghostty_vt::{RenderState, Terminal, TerminalOptions};

    /// The text of each row, as libghostty renders it.
    fn rows(terminal: &Terminal<'_, '_>) -> Vec<String> {
        let mut state = RenderState::new().unwrap();
        let snapshot = state.update(terminal).unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut out = Vec::new();
        let mut row_iter = rows.update(&snapshot).unwrap();
        while let Some(row) = row_iter.next() {
            let mut line = String::new();
            let mut cell_iter = cells.update(row).unwrap();
            while let Some(cell) = cell_iter.next() {
                let g = cell.graphemes().unwrap();
                if g.is_empty() {
                    line.push(' ');
                }
                line.extend(g);
            }
            out.push(line.trim_end().to_owned());
        }
        out
    }

    #[test]
    fn libghostty_interprets_output() {
        let mut t = Terminal::new(TerminalOptions { cols: 20, rows: 3, max_scrollback: 100 }).unwrap();
        t.vt_write(b"hello \x1b[31mred\x1b[0m\r\n\xe6\x95\xb0\xe5\xbc\x8f x");
        // A wide character takes two cells; the second is an empty spacer.
        assert_eq!(rows(&t), vec!["hello red", "数 式  x", ""]);
        assert_eq!((t.cursor_x().unwrap(), t.cursor_y().unwrap()), (6, 1));
    }
}
