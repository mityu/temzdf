//! Input buffers: one line of text typed into input panes, as Vim's command
//! line. Scripts make them (`new-input`), show them in panes (`split`,
//! `open-float`, `show-buffer`), read and set their text and hear of every
//! change (`InputChanged`). Editing is commands (`input.*`), mapped in the
//! `input` mode; keys no mapping takes that type a character, committed
//! input-method text and pastes are inserted.

use temzdf_scene::{Attrs, Cell, Color, Frame, TextGrid};
use termpix_ui::{EditResult, LineInput, Theme};

use super::notices::width;
use super::{BufferId, Kind, Outcome, PaneId, ScriptValue, Session};

/// An input buffer: its prompt and its text (the carets are the panes').
pub(super) struct InputBuffer {
    pub prompt: String,
    pub text: String,
}

/// An input pane's own state: its caret (a byte index into the text) and
/// how far the line is scrolled (cells).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct InputView {
    pub caret: usize,
    scroll: usize,
}

impl InputView {
    /// A view of `b` with the caret at the end.
    pub(super) fn at_end(b: &InputBuffer) -> Self {
        Self { caret: b.text.len(), scroll: 0 }
    }
}

/// An edit of the focused input pane (the `input.*` commands).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InputOp {
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    WordLeft,
    WordRight,
    DeleteWord,
    DeleteToStart,
    DeleteToEnd,
}

impl InputOp {
    /// The commands, without their `input.` namespace.
    pub(super) const NAMES: &[(InputOp, &str, &str)] = &[
        (InputOp::Backspace, "backspace", "Delete the character before the caret"),
        (InputOp::Delete, "delete", "Delete the character after the caret"),
        (InputOp::Left, "left", "Move the caret one character left"),
        (InputOp::Right, "right", "Move the caret one character right"),
        (InputOp::Home, "home", "Move the caret to the start"),
        (InputOp::End, "end", "Move the caret to the end"),
        (InputOp::WordLeft, "word-left", "Move the caret to the start of the word before it"),
        (InputOp::WordRight, "word-right", "Move the caret to the end of the word after it"),
        (InputOp::DeleteWord, "delete-word", "Delete the word before the caret"),
        (InputOp::DeleteToStart, "delete-to-start", "Delete from the start to the caret"),
        (InputOp::DeleteToEnd, "delete-to-end", "Delete from the caret to the end"),
    ];

    pub(super) fn from_name(name: &str) -> Option<Self> {
        let name = name.strip_prefix("input.")?;
        Self::NAMES.iter().find(|(_, n, _)| *n == name).map(|(op, _, _)| *op)
    }

    fn apply(self, line: &mut LineInput) -> EditResult {
        match self {
            Self::Backspace => line.backspace(),
            Self::Delete => line.delete(),
            Self::Left => line.left(),
            Self::Right => line.right(),
            Self::Home => {
                line.home();
                EditResult::Moved
            }
            Self::End => {
                line.end();
                EditResult::Moved
            }
            Self::WordLeft => line.word_left(),
            Self::WordRight => line.word_right(),
            Self::DeleteWord => line.delete_word(),
            Self::DeleteToStart => line.delete_to_start(),
            Self::DeleteToEnd => line.delete_to_end(),
        }
    }
}

/// A line of text as typed into one line: control characters go, line
/// breaks and tabs become spaces.
fn one_line(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

fn text_width(s: &str) -> usize {
    s.chars().map(width).sum()
}

impl Session {
    /// A new input buffer (`new-input`).
    pub(super) fn new_input(&mut self, prompt: &str, text: &str) -> BufferId {
        let buf = self.alloc_id();
        self.inputs.insert(buf, InputBuffer { prompt: one_line(prompt), text: one_line(text) });
        self.event("BufferCreated", vec![ScriptValue::Num(buf as f64), ScriptValue::Str("input".into())]);
        buf
    }

    /// The focused pane's input buffer.
    fn focused_input(&self) -> Option<BufferId> {
        match self.panes.get(&self.focus).map(|e| &e.kind) {
            Some(Kind::Input { buf, .. }) => Some(*buf),
            _ => None,
        }
    }

    /// Input buffer `buf`, or the focused pane's for `None`.
    pub(super) fn input_buffer(&self, buf: Option<BufferId>) -> Result<BufferId, String> {
        match buf {
            Some(b) if self.inputs.contains_key(&b) => Ok(b),
            Some(b) => Err(format!("no input buffer {b}")),
            None => self.focused_input().ok_or_else(|| "the focused pane is not an input".into()),
        }
    }

    /// Edit the focused input pane (an `input.*` command).
    pub(super) fn input_edit(&mut self, op: InputOp) -> Result<Outcome, String> {
        self.edit_focused_input(|line| op.apply(line))
    }

    /// Insert `text` at the focused input pane's caret (`input.insert`,
    /// typed characters, input-method text, pastes).
    pub(super) fn input_insert(&mut self, text: &str) -> Result<Outcome, String> {
        let text = one_line(text);
        self.edit_focused_input(|line| {
            if text.is_empty() {
                return EditResult::Ignored;
            }
            line.insert(&text);
            EditResult::Changed
        })
    }

    fn edit_focused_input(&mut self, edit: impl FnOnce(&mut LineInput) -> EditResult) -> Result<Outcome, String> {
        let focus = self.focus;
        let Some(Kind::Input { buf, view }) = self.panes.get_mut(&focus).map(|e| &mut e.kind) else {
            return Err("the focused pane is not an input".into());
        };
        let buf = *buf;
        let b = self.inputs.get_mut(&buf).ok_or("its buffer is gone")?;
        let mut line = LineInput { text: std::mem::take(&mut b.text), caret: view.caret, ..LineInput::default() };
        let result = edit(&mut line);
        (b.text, view.caret) = (line.text, line.caret);
        Ok(match result {
            EditResult::Changed => {
                self.input_changed(buf, Some(focus));
                Outcome::Redraw
            }
            EditResult::Moved => Outcome::Redraw,
            EditResult::Ignored => Outcome::None,
        })
    }

    /// Set an input buffer's text (`set-input-text`); its panes' carets go
    /// to the end.
    pub(super) fn set_input_text(&mut self, buf: BufferId, text: &str) -> Outcome {
        let Some(b) = self.inputs.get_mut(&buf) else { return Outcome::None };
        let text = one_line(text);
        if b.text == text {
            return Outcome::None;
        }
        b.text = text;
        let end = b.text.len();
        for e in self.panes.values_mut() {
            if let Kind::Input { buf: b, view } = &mut e.kind
                && *b == buf
            {
                view.caret = end;
            }
        }
        self.input_changed(buf, None);
        Outcome::Redraw
    }

    /// `buf`'s text changed (in pane `by`): the other panes' carets stay
    /// inside it, and `InputChanged` hooks hear of it.
    fn input_changed(&mut self, buf: BufferId, by: Option<PaneId>) {
        let Some(b) = self.inputs.get(&buf) else { return };
        let text = b.text.clone();
        for (id, e) in &mut self.panes {
            if let Kind::Input { buf: b, view } = &mut e.kind
                && *b == buf
                && Some(*id) != by
            {
                view.caret = view.caret.min(text.len());
                while !text.is_char_boundary(view.caret) {
                    view.caret -= 1;
                }
            }
        }
        self.event("InputChanged", vec![ScriptValue::Num(buf as f64), ScriptValue::Str(text)]);
    }
}

/// An input pane's frame, `cols` cells wide: the prompt and the text of its
/// buffer `b` on the first row, scrolled to keep the caret in sight, the
/// cursor at the caret (where input methods put their preedit).
pub(super) fn frame(b: Option<&InputBuffer>, view: &mut InputView, cols: u16) -> Frame {
    let Some(b) = b else { return Frame::default() };
    let cols = cols as usize;
    let theme = Theme::dark();
    let accent = Color::Rgb([theme.accent.0[0], theme.accent.0[1], theme.accent.0[2]]);
    let caret_col = text_width(&b.prompt) + text_width(&b.text[..view.caret.min(b.text.len())]);
    // The caret's cell is the last one shown at most; back at the start
    // (the prompt shown) when it fits there.
    if caret_col < view.scroll {
        view.scroll = if caret_col < cols { 0 } else { caret_col };
    } else if cols > 0 && caret_col >= view.scroll + cols {
        view.scroll = caret_col + 1 - cols;
    }
    let blank = Cell { text: " ".into(), width: 1, fg: Color::Default, bg: Color::Default, attrs: Attrs::default() };
    let mut row = vec![blank.clone(); cols];
    let mut col = 0usize;
    let chars = b.prompt.chars().map(|c| (c, accent)).chain(b.text.chars().map(|c| (c, Color::Default)));
    for (c, fg) in chars {
        let w = width(c);
        let at = col;
        col += w;
        // Cut at the left edge or not fitting at the right: blank.
        if at < view.scroll || col > view.scroll + cols {
            continue;
        }
        let x = at - view.scroll;
        row[x] = Cell { text: c.to_string(), width: w as u8, fg, ..blank.clone() };
        if w == 2 {
            row[x + 1] = Cell { text: String::new(), width: 0, fg, ..blank.clone() };
        }
    }
    Frame {
        text: vec![TextGrid { col: 0, row: 0, rows: vec![row] }],
        cursor: (cols > 0).then(|| ((caret_col - view.scroll) as u16, 0)),
        title: format!("{}{}", b.prompt, b.text),
        ..Frame::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_lose_breaks_and_controls() {
        assert_eq!(one_line("a\nb\tc\u{7}d"), "a b cd");
    }

    #[test]
    fn op_names_are_namespaced() {
        assert_eq!(InputOp::from_name("input.delete-word"), Some(InputOp::DeleteWord));
        assert_eq!(InputOp::from_name("delete-word"), None);
        assert_eq!(InputOp::from_name("input.nothing"), None);
    }
}
