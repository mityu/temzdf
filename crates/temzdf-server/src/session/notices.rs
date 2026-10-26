//! Floating messages at the top right of the screen (as WezTerm's), drawn as
//! text cells over the panes: errors stay until the next key, information
//! fades after a few seconds, the script-host notice stays until the host is
//! back.

use std::time::{Duration, Instant};

use temzdf_scene::{Attrs, Cell, Color, TextGrid};

use super::Session;

/// How long information stays.
const INFO_FOR: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Stays until the next key.
    Error,
    /// Fades after [`INFO_FOR`].
    Info,
    /// Stays until taken back (the script host is down).
    Host,
}

pub struct Notice {
    pub kind: Kind,
    pub text: String,
    pub until: Option<Instant>,
}

/// Terminal cells a character takes (CJK and other wide characters two).
pub(super) fn width(c: char) -> usize {
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
fn wrap(text: &str, max: usize) -> Vec<String> {
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

impl Session {
    /// Show a floating message.
    pub(super) fn notify(&mut self, kind: Kind, text: &str) {
        eprintln!("temzdf: {text}");
        if kind == Kind::Host {
            self.notices.retain(|n| n.kind != Kind::Host);
        }
        let until = (kind == Kind::Info).then(|| Instant::now() + INFO_FOR);
        self.notices.push(Notice { kind, text: text.to_owned(), until });
        // Keep a handful: the newest.
        while self.notices.len() > 5 {
            self.notices.remove(0);
        }
    }

    /// A key was pressed: errors go.
    pub(super) fn dismiss_errors(&mut self) -> bool {
        let before = self.notices.len();
        self.notices.retain(|n| n.kind != Kind::Error);
        before != self.notices.len()
    }

    pub(super) fn clear_host_notice(&mut self) {
        self.notices.retain(|n| n.kind != Kind::Host);
    }

    /// Faded information goes. Returns whether something went.
    pub(super) fn expire_notices(&mut self, now: Instant) -> bool {
        let before = self.notices.len();
        self.notices.retain(|n| n.until.is_none_or(|t| now < t));
        before != self.notices.len()
    }

    pub(super) fn notice_deadline(&self) -> Option<Instant> {
        self.notices.iter().filter_map(|n| n.until).min()
    }

    /// The notices as boxes of text cells, stacked down from the top right.
    /// Where the floating messages are, in cells.
    pub(super) fn notice_rects(&self) -> Vec<temzdf_scene::Rect> {
        let rect =
            |g: &TextGrid| temzdf_scene::Rect::new(g.col as i32, g.row as i32, g.width() as u32, g.rows.len() as u32);
        self.notice_grids().iter().map(rect).collect()
    }

    pub(super) fn notice_grids(&self) -> Vec<TextGrid> {
        let cols = self.geometry.cols as usize;
        let rows = self.geometry.rows as usize;
        if cols < 8 || rows < 3 {
            return Vec::new();
        }
        let max = (cols / 2).max(20).min(cols - 4);
        let mut out = Vec::new();
        // Below the tab line.
        let mut row = self.area().y as usize;
        for n in self.notices.iter().rev() {
            let lines = wrap(&n.text, max);
            let inner = lines.iter().map(|l| l.chars().map(width).sum::<usize>()).max().unwrap_or(0).max(1);
            let w = inner + 4;
            if row + lines.len() + 2 > rows {
                break;
            }
            let border = match n.kind {
                Kind::Error | Kind::Host => Color::Indexed(1),
                Kind::Info => Color::Indexed(4),
            };
            let bg = Color::Rgb([0x26, 0x26, 0x2a]);
            let fg = Color::Rgb([0xee, 0xee, 0xee]);
            let cell = |text: &str, fg: Color| Cell { text: text.into(), width: 1, fg, bg, attrs: Attrs::default() };
            let mut grid = Vec::new();
            let edge = |l: &str, m: &str, r: &str| {
                let mut v = vec![cell(l, border)];
                v.extend((0..w - 2).map(|_| cell(m, border)));
                v.push(cell(r, border));
                v
            };
            grid.push(edge("╭", "─", "╮"));
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
}
