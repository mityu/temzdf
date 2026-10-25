//! Text on a page (characters with boxes, grouped in lines) and text
//! selections spanning one or more pages.

use mupdf::{DisplayList, TextPageFlags};

use crate::pdf::{PageBounds, PageRect};

#[derive(Clone, Debug, PartialEq)]
pub struct Char {
    pub c: char,
    pub rect: PageRect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub rect: PageRect,
    /// Range of `PageText::chars` in this line.
    pub start: usize,
    pub end: usize,
}

/// The characters of one page in reading order (MuPDF's structured text order).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageText {
    pub chars: Vec<Char>,
    pub lines: Vec<Line>,
}

impl PageText {
    pub fn extract(list: &DisplayList, bounds: PageBounds) -> anyhow::Result<Self> {
        let tp = list.to_text_page(TextPageFlags::empty())?;
        let mut text = PageText::default();
        let rel = |r: mupdf::Rect| PageRect {
            x0: r.x0 - bounds.x0,
            y0: r.y0 - bounds.y0,
            x1: r.x1 - bounds.x0,
            y1: r.y1 - bounds.y0,
        };
        for block in tp.blocks() {
            for line in block.lines() {
                let start = text.chars.len();
                for ch in line.chars() {
                    let Some(c) = ch.char() else { continue };
                    let q = ch.quad();
                    let xs = [q.ul.x, q.ur.x, q.ll.x, q.lr.x];
                    let ys = [q.ul.y, q.ur.y, q.ll.y, q.lr.y];
                    let r = mupdf::Rect::new(
                        xs.iter().copied().fold(f32::INFINITY, f32::min),
                        ys.iter().copied().fold(f32::INFINITY, f32::min),
                        xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                        ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                    );
                    text.chars.push(Char { c, rect: rel(r) });
                }
                if text.chars.len() > start {
                    text.lines.push(Line { rect: rel(line.bounds()), start, end: text.chars.len() });
                }
            }
        }
        Ok(text)
    }

    /// Is (x, y) on a line of text? Vertically a little slack is allowed;
    /// horizontally one line height, so a press just before or after the text
    /// still starts a selection (as in GUI viewers).
    pub fn has_text_at(&self, x: f32, y: f32) -> bool {
        const SLACK_Y: f32 = 2.0;
        self.lines.iter().any(|l| {
            let slack_x = l.rect.y1 - l.rect.y0;
            x >= l.rect.x0 - slack_x && x <= l.rect.x1 + slack_x && y >= l.rect.y0 - SLACK_Y && y <= l.rect.y1 + SLACK_Y
        })
    }

    /// Caret position (index between characters, `0..=chars.len()`) closest to
    /// (x, y): the line nearest vertically, then the gap nearest horizontally.
    pub fn caret_at(&self, x: f32, y: f32) -> usize {
        let Some(line) = self.lines.iter().min_by(|a, b| line_distance(a, y).total_cmp(&line_distance(b, y))) else {
            return 0;
        };
        let chars = &self.chars[line.start..line.end];
        let i = chars.iter().position(|c| x < (c.rect.x0 + c.rect.x1) / 2.0).unwrap_or(chars.len());
        line.start + i
    }
}

fn line_distance(l: &Line, y: f32) -> f32 {
    if y < l.rect.y0 {
        l.rect.y0 - y
    } else if y > l.rect.y1 {
        y - l.rect.y1
    } else {
        0.0
    }
}

/// A position in the document: page and caret index on that page.
pub type Caret = (usize, usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Caret,
    pub head: Caret,
}

impl Selection {
    pub fn ordered(&self) -> (Caret, Caret) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Character range selected on `page`, if any.
    fn range_on(&self, page: usize, text: &PageText) -> Option<(usize, usize)> {
        let ((p0, i0), (p1, i1)) = self.ordered();
        if page < p0 || page > p1 {
            return None;
        }
        let start = if page == p0 { i0 } else { 0 };
        let end = if page == p1 { i1 } else { text.chars.len() };
        (start < end).then_some((start, end.min(text.chars.len())))
    }

    /// Highlight rectangles on `page`: one per line run.
    pub fn rects(&self, page: usize, text: &PageText) -> Vec<PageRect> {
        let Some((start, end)) = self.range_on(page, text) else { return Vec::new() };
        text.lines
            .iter()
            .filter_map(|l| {
                let (s, e) = (start.max(l.start), end.min(l.end));
                (s < e).then(|| {
                    let first = &text.chars[s].rect;
                    let last = &text.chars[e - 1].rect;
                    PageRect { x0: first.x0, y0: l.rect.y0, x1: last.x1, y1: l.rect.y1 }
                })
            })
            .collect()
    }

    /// The selected text, with line breaks between lines and pages.
    /// `texts[p]` must be loaded for every selected page.
    pub fn text(&self, texts: &[Option<PageText>]) -> String {
        let ((p0, _), (p1, _)) = self.ordered();
        let mut out = String::new();
        for (page, text) in texts.iter().enumerate().take(p1 + 1).skip(p0) {
            let Some(text) = text else { continue };
            let Some((start, end)) = self.range_on(page, text) else { continue };
            for l in &text.lines {
                let (s, e) = (start.max(l.start), end.min(l.end));
                if s >= e {
                    continue;
                }
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.extend(text.chars[s..e].iter().map(|c| c.c));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf;

    #[test]
    fn select_words_on_the_fixture() {
        let doc = pdf::open(&pdf::fixture("sample.pdf")).unwrap();
        let bounds = pdf::page_bounds(&doc).unwrap()[0];
        let text = PageText::extract(&pdf::display_list(&doc, 0).unwrap(), bounds).unwrap();
        let all: String = text.chars.iter().map(|c| c.c).collect();
        let start = all.find("needle appears").expect("fixture text");
        // Find the caret before "needle" by clicking just left of its first char.
        let n = &text.chars[all[..start].chars().count()];
        let a = text.caret_at(n.rect.x0 + 0.1, (n.rect.y0 + n.rect.y1) / 2.0);
        let e = &text.chars[all[..start].chars().count() + "needle".len() - 1];
        let b = text.caret_at(e.rect.x1 - 0.1, (e.rect.y0 + e.rect.y1) / 2.0);
        assert!(text.has_text_at(n.rect.x0 + 1.0, (n.rect.y0 + n.rect.y1) / 2.0));
        let sel = Selection { anchor: (0, b), head: (0, a) };
        let texts = vec![Some(text.clone())];
        assert_eq!(sel.text(&texts), "needle");
        assert_eq!(sel.rects(0, &text).len(), 1);
        assert!(!sel.is_empty());
    }

    #[test]
    fn multi_line_and_multi_page_text() {
        let r = |x0, y0| PageRect { x0, y0, x1: x0 + 5.0, y1: y0 + 10.0 };
        let page = |s: &str| {
            let mut t = PageText::default();
            for (li, line) in s.split('\n').enumerate() {
                let start = t.chars.len();
                for (i, c) in line.chars().enumerate() {
                    t.chars.push(Char { c, rect: r(i as f32 * 5.0, li as f32 * 12.0) });
                }
                t.lines.push(Line {
                    rect: PageRect { x0: 0.0, y0: li as f32 * 12.0, x1: 100.0, y1: li as f32 * 12.0 + 10.0 },
                    start,
                    end: t.chars.len(),
                });
            }
            t
        };
        let texts = vec![Some(page("abc\ndef")), Some(page("ghi"))];
        let sel = Selection { anchor: (0, 1), head: (1, 2) };
        assert_eq!(sel.text(&texts), "bc\ndef\ngh");
        assert_eq!(sel.rects(0, texts[0].as_ref().unwrap()).len(), 2);
        assert_eq!(texts[0].as_ref().unwrap().caret_at(100.0, 13.0), 6, "end of second line");
    }
}
