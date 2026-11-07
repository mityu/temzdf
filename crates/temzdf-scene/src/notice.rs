//! Floating messages as boxes of text cells stacked down from the top right
//! of the screen (as WezTerm's): the session's notices, and a terminal
//! client's own (`temzdf-tty`).

use crate::{Attrs, Cell, Color, TextGrid};

/// Terminal cells a character takes (CJK and other wide characters two).
pub fn width(c: char) -> usize {
    let u = c as u32;
    let wide = (0x1100..=0x115f).contains(&u)
        || (0x2e80..=0xa4cf).contains(&u)
        || (0xac00..=0xd7a3).contains(&u)
        || (0xf900..=0xfaff).contains(&u)
        || (0xfe30..=0xfe4f).contains(&u)
        || (0xff00..=0xff60).contains(&u)
        || (0xffe0..=0xffe6).contains(&u)
        || (0x1f300..=0x1faff).contains(&u)
        || (0x20000..=0x3fffd).contains(&u);
    if wide { 2 } else { 1 }
}

/// `text` broken into lines of at most `max` cells, at a space where the
/// line has one (the space goes), else anywhere.
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    let w = |s: &str| s.chars().map(width).sum::<usize>();
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for c in para.chars() {
            if w(&line) + width(c) > max && !line.is_empty() {
                match line.rfind(' ').filter(|&i| i > 0) {
                    // The word typed so far goes on.
                    Some(i) if c != ' ' => {
                        let rest = line.split_off(i + 1);
                        line.pop();
                        out.push(std::mem::replace(&mut line, rest));
                    }
                    _ => out.push(std::mem::take(&mut line)),
                }
                if c == ' ' && line.is_empty() {
                    continue;
                }
            }
            line.push(c);
        }
        out.push(line);
    }
    out
}

/// Messages (text, border colour), the first on top, as boxes from row
/// `top` down at the right of a `cols` x `rows` screen. One too tall for
/// half the width takes the whole width; one still too tall for the screen
/// keeps its last lines when it is the first (a question asked ends it);
/// others that do not fit are left out.
pub fn boxes(messages: &[(&str, Color)], cols: usize, rows: usize, top: usize) -> Vec<TextGrid> {
    if cols < 8 || rows < 3 {
        return Vec::new();
    }
    // (A box is its text and four more columns, one left free at the right.)
    let max = (cols / 2).max(20).min(cols - 5);
    let bg = Color::Rgb([0x26, 0x26, 0x2a]);
    let fg = Color::Rgb([0xee, 0xee, 0xee]);
    let mut out = Vec::new();
    let mut row = top;
    for &(text, border) in messages {
        let mut lines = wrap(text, max);
        if row + lines.len() + 2 > rows {
            lines = wrap(text, cols - 5);
        }
        if row == top && row + lines.len() + 2 > rows {
            let keep = rows.saturating_sub(row + 2);
            if keep == 0 {
                break;
            }
            lines.drain(..lines.len() - keep);
        }
        let inner = lines.iter().map(|l| l.chars().map(width).sum::<usize>()).max().unwrap_or(0).max(1);
        let w = inner + 4;
        if row + lines.len() + 2 > rows {
            break;
        }
        let cell = |text: &str, fg: Color| Cell { text: text.into(), width: 1, fg, bg, attrs: Attrs::default() };
        let edge = |l: &str, m: &str, r: &str| {
            let mut v = vec![cell(l, border)];
            v.extend((0..w - 2).map(|_| cell(m, border)));
            v.push(cell(r, border));
            v
        };
        let mut grid = vec![edge("╭", "─", "╮")];
        for l in &lines {
            let mut v = vec![cell("│", border), cell(" ", fg)];
            let mut used = 0;
            for c in l.chars() {
                let cw = width(c);
                v.push(Cell { text: c.to_string(), width: cw as u8, fg, bg, attrs: Attrs::default() });
                if cw == 2 {
                    v.push(Cell { text: String::new(), width: 0, fg, bg, attrs: Attrs::default() });
                }
                used += cw;
            }
            v.extend((used..inner).map(|_| cell(" ", fg)));
            v.push(cell(" ", fg));
            v.push(cell("│", border));
            grid.push(v);
        }
        grid.push(edge("╰", "─", "╯"));
        out.push(TextGrid { col: cols.saturating_sub(w + 1) as u16, row: row as u16, rows: grid });
        row += lines.len() + 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_counts_wide_characters() {
        assert_eq!(wrap("abcdef", 4), ["abcd", "ef"]);
        assert_eq!(wrap("数式数式", 5), ["数式", "数式"]);
        assert_eq!(wrap("a\nb", 10), ["a", "b"]);
        // At spaces where the line has them.
        assert_eq!(wrap("one two three", 8), ["one two", "three"]);
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("aaaaaa bb", 4), ["aaaa", "aa", "bb"]);
    }

    #[test]
    fn boxes_stack_from_the_top_right() {
        let b = boxes(&[("one", Color::Indexed(1)), ("two", Color::Indexed(4))], 40, 10, 1);
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].col, b[0].row, b[0].width(), b[0].rows.len()), (32, 1, 7, 3));
        assert_eq!(b[1].row, 4);
        assert!(boxes(&[("x", Color::Default)], 6, 10, 0).is_empty(), "too narrow");
        // Too tall for half the width: the whole width.
        let long = "word ".repeat(30);
        let b = boxes(&[(long.as_str(), Color::Default)], 40, 8, 0);
        assert_eq!((b[0].width(), b[0].rows.len()), (38, 7), "five lines of 34 cells");
        let right = usize::from(b[0].col) + (b[0].width() as usize);
        assert!(right < 40, "inside the screen");
        // Narrow screens too.
        let b = boxes(&[("a long message for a narrow screen", Color::Default)], 22, 10, 0);
        let right = usize::from(b[0].col) + (b[0].width() as usize);
        assert!(right < 22, "a column left free at the right: {right}");
        // Still too tall: its last lines, the end (a question) kept.
        let many: String = (1..=20).map(|i| format!("line {i}\n")).collect::<String>() + "answer?";
        let b = boxes(&[(many.as_str(), Color::Default)], 40, 8, 0);
        assert_eq!(b[0].rows.len(), 8);
        let last: String = b[0].rows[6].iter().map(|c| c.text.as_str()).collect();
        assert!(last.contains("answer?"), "{last}");
    }
}
