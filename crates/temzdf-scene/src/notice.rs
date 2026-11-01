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

/// `text` broken into lines of at most `max` cells.
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        let mut w = 0;
        for c in para.chars() {
            let cw = width(c);
            if w + cw > max && !line.is_empty() {
                out.push(std::mem::take(&mut line));
                w = 0;
            }
            line.push(c);
            w += cw;
        }
        out.push(line);
    }
    out
}

/// Messages (text, border colour), the first on top, as boxes from row
/// `top` down at the right of a `cols` x `rows` screen; those that do not
/// fit are left out.
pub fn boxes(messages: &[(&str, Color)], cols: usize, rows: usize, top: usize) -> Vec<TextGrid> {
    if cols < 8 || rows < 3 {
        return Vec::new();
    }
    let max = (cols / 2).max(20).min(cols - 4);
    let bg = Color::Rgb([0x26, 0x26, 0x2a]);
    let fg = Color::Rgb([0xee, 0xee, 0xee]);
    let mut out = Vec::new();
    let mut row = top;
    for &(text, border) in messages {
        let lines = wrap(text, max);
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
        out.push(TextGrid { col: (cols - w - 1) as u16, row: row as u16, rows: grid });
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
    }

    #[test]
    fn boxes_stack_from_the_top_right() {
        let b = boxes(&[("one", Color::Indexed(1)), ("two", Color::Indexed(4))], 40, 10, 1);
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].col, b[0].row, b[0].width(), b[0].rows.len()), (32, 1, 7, 3));
        assert_eq!(b[1].row, 4);
        assert!(boxes(&[("x", Color::Default)], 6, 10, 0).is_empty(), "too narrow");
    }
}
