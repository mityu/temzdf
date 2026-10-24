//! Widgets: each renders itself into an RGBA image of a given size and answers
//! hit tests in its own pixel coordinates. State lives in the widget structs;
//! rendering is a pure function of that state (cheap to snapshot-test).

use crate::canvas::{Canvas, Color, Text, TextStyle};

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub bar_bg: Color,
    pub bar_fg: Color,
    pub bar_dim: Color,
    pub panel_bg: Color,
    pub panel_fg: Color,
    pub panel_dim: Color,
    pub accent: Color,
    pub selection_bg: Color,
    pub hover_bg: Color,
    pub input_bg: Color,
    pub error: Color,
    /// Font size relative to the row height.
    pub font_ratio: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bar_bg: Color::rgb(0x24, 0x26, 0x2b),
            bar_fg: Color::rgb(0xdc, 0xdf, 0xe4),
            bar_dim: Color::rgb(0x8b, 0x90, 0x99),
            panel_bg: Color::rgb(0x1e, 0x20, 0x24),
            panel_fg: Color::rgb(0xdc, 0xdf, 0xe4),
            panel_dim: Color::rgb(0x8b, 0x90, 0x99),
            accent: Color::rgb(0x61, 0xaf, 0xef),
            selection_bg: Color::rgb(0x2f, 0x4f, 0x75),
            hover_bg: Color::rgb(0x2c, 0x30, 0x37),
            input_bg: Color::rgb(0x16, 0x18, 0x1c),
            error: Color::rgb(0xe0, 0x6c, 0x75),
            font_ratio: 0.62,
        }
    }

    fn style(&self, row_h: f32, color: Color, bold: bool) -> TextStyle {
        TextStyle { size: (row_h * self.font_ratio).max(6.0), color, bold, family: "sans-serif" }
    }
}

/// One-row bar with left- and right-aligned text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusBar {
    pub left: String,
    pub right: String,
    /// Shown instead of `left` in the error colour when set.
    pub error: Option<String>,
    /// Shown instead of `left` in the accent colour when set (and no error).
    pub notice: Option<String>,
}

impl StatusBar {
    pub fn render(&self, text: &mut Text, theme: &Theme, width: u32, height: u32) -> Vec<u8> {
        let (w, h) = (width as f32, height as f32);
        let pad = h * 0.4;
        let mut c = Canvas::new(width, height);
        c.fill_rect(0.0, 0.0, w, h, theme.bar_bg);
        let right = text.line(&self.right, &theme.style(h, theme.bar_dim, false), w / 2.0);
        let (left_text, left_color) = match (&self.error, &self.notice) {
            (Some(e), _) => (e.as_str(), theme.error),
            (None, Some(n)) => (n.as_str(), theme.accent),
            (None, None) => (self.left.as_str(), theme.bar_fg),
        };
        let left = text.line(left_text, &theme.style(h, left_color, false), (w - right.width() - 3.0 * pad).max(0.0));
        c.text(&left, pad, (h - left.height()) / 2.0);
        c.text(&right, w - pad - right.width(), (h - right.height()) / 2.0);
        c.finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    pub label: String,
    /// Secondary text on the right (e.g. a page number).
    pub detail: String,
    /// Indentation level.
    pub depth: usize,
    /// `Some(open)` when the item has children.
    pub expanded: Option<bool>,
}

/// A scrollable list of rows (outline, search results). Rows are `row_h` high.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListView {
    pub items: Vec<ListItem>,
    pub selected: Option<usize>,
    pub hover: Option<usize>,
    /// Index of the first visible row.
    pub scroll: usize,
    /// Optional title row at the top.
    pub title: Option<String>,
}

impl ListView {
    fn header_rows(&self) -> usize {
        usize::from(self.title.is_some())
    }

    pub fn visible_rows(&self, height: u32, row_h: u32) -> usize {
        (height / row_h.max(1)) as usize - self.header_rows().min((height / row_h.max(1)) as usize)
    }

    /// Item index at pixel y, if any.
    pub fn hit(&self, y: f32, row_h: u32) -> Option<usize> {
        let row = (y / row_h as f32).floor() as isize - self.header_rows() as isize;
        if row < 0 {
            return None;
        }
        let i = self.scroll + row as usize;
        (i < self.items.len()).then_some(i)
    }

    /// Scroll by whole rows, clamped.
    pub fn scroll_by(&mut self, rows: isize, height: u32, row_h: u32) {
        let max = self.items.len().saturating_sub(self.visible_rows(height, row_h));
        self.scroll = (self.scroll as isize + rows).clamp(0, max as isize) as usize;
    }

    /// Make sure the selected item is visible.
    pub fn reveal_selected(&mut self, height: u32, row_h: u32) {
        let Some(sel) = self.selected else { return };
        let rows = self.visible_rows(height, row_h).max(1);
        if sel < self.scroll {
            self.scroll = sel;
        } else if sel >= self.scroll + rows {
            self.scroll = sel + 1 - rows;
        }
    }

    pub fn render(&self, text: &mut Text, theme: &Theme, width: u32, height: u32, row_h: u32) -> Vec<u8> {
        let (w, rh) = (width as f32, row_h as f32);
        let pad = rh * 0.5;
        let mut c = Canvas::new(width, height);
        c.fill_rect(0.0, 0.0, w, height as f32, theme.panel_bg);
        let mut y = 0.0;
        if let Some(t) = &self.title {
            let l = text.line(t, &theme.style(rh, theme.panel_dim, true), w - 2.0 * pad);
            c.text(&l, pad, y + (rh - l.height()) / 2.0);
            y += rh;
        }
        for (i, item) in self.items.iter().enumerate().skip(self.scroll) {
            if y >= height as f32 {
                break;
            }
            if Some(i) == self.selected {
                c.fill_rect(0.0, y, w, rh, theme.selection_bg);
                c.fill_rect(0.0, y, rh * 0.15, rh, theme.accent);
            } else if Some(i) == self.hover {
                c.fill_rect(0.0, y, w, rh, theme.hover_bg);
            }
            let indent = pad + item.depth as f32 * rh * 0.9;
            if let Some(open) = item.expanded {
                let arrow = text.line(if open { "▾" } else { "▸" }, &theme.style(rh, theme.panel_dim, false), rh);
                c.text(&arrow, indent, y + (rh - arrow.height()) / 2.0);
            }
            let label_x = indent + rh * 0.9;
            let detail = text.line(&item.detail, &theme.style(rh, theme.panel_dim, false), w / 3.0);
            let label_w = (w - label_x - detail.width() - 2.0 * pad).max(0.0);
            let label = text.line(&item.label, &theme.style(rh, theme.panel_fg, item.depth == 0), label_w);
            c.text(&label, label_x, y + (rh - label.height()) / 2.0);
            c.text(&detail, w - pad - detail.width(), y + (rh - detail.height()) / 2.0);
            y += rh;
        }
        c.finish()
    }
}

/// Single-line text input with a caret (byte index into `text`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineInput {
    pub prompt: String,
    pub text: String,
    pub caret: usize,
    /// Right-aligned hint (e.g. "3/12").
    pub hint: String,
}

/// What an edit key did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditResult {
    Changed,
    Moved,
    Ignored,
}

impl LineInput {
    pub fn insert(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.caret, &s);
        self.caret += s.len();
    }

    pub fn backspace(&mut self) -> EditResult {
        let Some((i, _)) = self.text[..self.caret].char_indices().next_back() else { return EditResult::Ignored };
        self.text.replace_range(i..self.caret, "");
        self.caret = i;
        EditResult::Changed
    }

    pub fn delete(&mut self) -> EditResult {
        let Some(c) = self.text[self.caret..].chars().next() else { return EditResult::Ignored };
        self.text.replace_range(self.caret..self.caret + c.len_utf8(), "");
        EditResult::Changed
    }

    pub fn left(&mut self) -> EditResult {
        match self.text[..self.caret].char_indices().next_back() {
            Some((i, _)) => {
                self.caret = i;
                EditResult::Moved
            }
            None => EditResult::Ignored,
        }
    }

    pub fn right(&mut self) -> EditResult {
        match self.text[self.caret..].chars().next() {
            Some(c) => {
                self.caret += c.len_utf8();
                EditResult::Moved
            }
            None => EditResult::Ignored,
        }
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.text.len();
    }

    /// Delete the word before the caret (Ctrl+W).
    pub fn delete_word(&mut self) -> EditResult {
        let before = &self.text[..self.caret];
        let trimmed = before.trim_end();
        let start = trimmed.rfind(char::is_whitespace).map_or(0, |i| i + 1);
        if start == self.caret {
            return EditResult::Ignored;
        }
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        EditResult::Changed
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    /// Where the text starts (after the prompt), in pixels.
    fn text_x(&self, text: &mut Text, theme: &Theme, h: f32) -> f32 {
        let pad = h * 0.4;
        pad + text.layout(&self.prompt, &theme.style(h, theme.accent, true), None).width() + pad * 0.5
    }

    /// The caret's x position for a widget of height `height` (for placing the
    /// terminal cursor, so IME preedit appears at the caret).
    pub fn caret_x(&self, text: &mut Text, theme: &Theme, height: u32) -> f32 {
        let h = height as f32;
        self.text_x(text, theme, h)
            + text.layout(&self.text[..self.caret], &theme.style(h, theme.bar_fg, false), None).width()
    }

    /// Render and return the pixels plus the caret x position.
    pub fn render(&self, text: &mut Text, theme: &Theme, width: u32, height: u32) -> (Vec<u8>, f32) {
        let (w, h) = (width as f32, height as f32);
        let pad = h * 0.4;
        let mut c = Canvas::new(width, height);
        c.fill_rect(0.0, 0.0, w, h, theme.input_bg);
        let prompt = text.layout(&self.prompt, &theme.style(h, theme.accent, true), None);
        c.text(&prompt, pad, (h - prompt.height()) / 2.0);
        let x0 = self.text_x(text, theme, h);
        let hint = text.line(&self.hint, &theme.style(h, theme.bar_dim, false), w / 3.0);
        c.text(&hint, w - pad - hint.width(), (h - hint.height()) / 2.0);
        let body = text.layout(&self.text, &theme.style(h, theme.bar_fg, false), None);
        c.text(&body, x0, (h - body.height()) / 2.0);
        let caret_x = self.caret_x(text, theme, height);
        c.fill_rect(caret_x, h * 0.18, (h * 0.08).max(1.0), h * 0.64, theme.accent);
        (c.finish(), caret_x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn save(name: &str, w: u32, h: u32, px: Vec<u8>) {
        if let Ok(dir) = std::env::var("TERMPIX_UI_SNAPSHOT_DIR") {
            let img = termpix::raster::RgbaImage::from_raw(w, h, px).unwrap();
            img.save_png(&std::path::Path::new(&dir).join(name)).unwrap();
        }
    }

    #[test]
    fn line_input_editing() {
        let mut i = LineInput::default();
        i.insert("数式 ab");
        assert_eq!(i.caret, i.text.len());
        i.left();
        i.backspace();
        assert_eq!(i.text, "数式 b");
        i.home();
        i.right();
        i.delete();
        assert_eq!(i.text, "数 b");
        i.end();
        i.insert(" word");
        i.delete_word();
        assert_eq!(i.text, "数 b ");
        i.insert("\x1bx\n");
        assert_eq!(i.text, "数 b x");
    }

    #[test]
    fn list_hit_and_scroll() {
        let mut l = ListView {
            items: (0..50)
                .map(|i| ListItem { label: format!("Item {i}"), detail: String::new(), depth: 0, expanded: None })
                .collect(),
            title: Some("Contents".into()),
            ..Default::default()
        };
        assert_eq!(l.hit(5.0, 20), None, "title row");
        assert_eq!(l.hit(25.0, 20), Some(0));
        l.scroll_by(100, 200, 20);
        assert_eq!(l.scroll, 41);
        l.selected = Some(3);
        l.reveal_selected(200, 20);
        assert_eq!(l.scroll, 3);
    }

    #[test]
    fn widgets_render() {
        let mut text = Text::new();
        let theme = Theme::dark();
        let bar = StatusBar { left: "sample.pdf".into(), right: "3 / 12  125%".into(), error: None, notice: None };
        let px = bar.render(&mut text, &theme, 400, 20);
        assert_eq!(px.len(), 400 * 20 * 4);
        save("status.png", 400, 20, px);
        let list = ListView {
            title: Some("Contents".into()),
            items: vec![
                ListItem { label: "1 Introduction".into(), detail: "1".into(), depth: 0, expanded: None },
                ListItem { label: "2 Mathematics".into(), detail: "1".into(), depth: 0, expanded: Some(true) },
                ListItem { label: "2.1 積分と級数".into(), detail: "2".into(), depth: 1, expanded: None },
            ],
            selected: Some(1),
            hover: Some(2),
            scroll: 0,
        };
        save("list.png", 220, 100, list.render(&mut text, &theme, 220, 100, 20));
        let input = LineInput { prompt: "/".into(), text: "needle 数式".into(), caret: 6, hint: "2/5".into() };
        let (px, caret) = input.render(&mut text, &theme, 400, 20);
        assert!(caret > 10.0);
        save("input.png", 400, 20, px);
    }
}
