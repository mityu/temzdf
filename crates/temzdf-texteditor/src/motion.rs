//! Motions that do not depend on the display: graphemes, words, the parts
//! of a line, the document's ends. (Lines keeping a column, display lines
//! and pages need the layout.)

use unicode_script::{Script, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;

use crate::text::Text;

/// Word characters, a regex character class per buffer (Vim's `iskeyword`).
#[derive(Clone, Debug)]
pub struct WordChars {
    class: String,
    re: regex::Regex,
}

impl WordChars {
    /// `class` is one character class, e.g. `[\p{L}\p{N}_]`.
    pub fn new(class: &str) -> Result<Self, String> {
        let re = regex::Regex::new(&format!("^(?:{class})$")).map_err(|e| e.to_string())?;
        Ok(Self { class: class.to_owned(), re })
    }

    pub fn class(&self) -> &str {
        &self.class
    }

    pub fn is_word(&self, c: char) -> bool {
        let mut buf = [0u8; 4];
        self.re.is_match(c.encode_utf8(&mut buf))
    }
}

impl Default for WordChars {
    fn default() -> Self {
        Self::new(r"[\p{L}\p{N}_]").expect("the default word characters")
    }
}

/// What a character is to word motions: blanks separate words; word
/// characters of different scripts (hiragana, katakana, kanji, Latin, ...)
/// are different words, as in Vim, so unspaced Japanese is not one word;
/// other characters (punctuation) are words of their own kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Blank,
    Punct,
    Word(Script),
}

pub fn char_class(c: char, words: &WordChars) -> Class {
    if c.is_whitespace() {
        return Class::Blank;
    }
    if !words.is_word(c) {
        return Class::Punct;
    }
    Class::Word(match c.script() {
        // The prolonged sound mark (ー) and the like go with katakana.
        Script::Common | Script::Inherited if ('\u{30fc}'..='\u{30fe}').contains(&c) => Script::Katakana,
        // Digits, `_`, Latin and the rest of the common characters are one
        // kind.
        Script::Common | Script::Inherited | Script::Latin => Script::Latin,
        s => s,
    })
}

/// A unit to move by. Counts are signed: negative goes backwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Grapheme,
    /// The start of a word (Vim's `w`, backwards `b`).
    WordStart,
    /// Just after the end of a word (Vim's `e` is one grapheme before;
    /// backwards `ge`).
    WordEnd,
    /// The same for blank-separated words (Vim's `W`, `B`, `E`, `gE`).
    BigWordStart,
    BigWordEnd,
    LineStart,
    /// The first character that is not a blank.
    FirstNonBlank,
    LineEnd,
    DocumentStart,
    DocumentEnd,
}

impl Unit {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "grapheme" => Unit::Grapheme,
            "wordStart" => Unit::WordStart,
            "wordEnd" => Unit::WordEnd,
            "bigWordStart" => Unit::BigWordStart,
            "bigWordEnd" => Unit::BigWordEnd,
            "lineStart" => Unit::LineStart,
            "firstNonBlank" => Unit::FirstNonBlank,
            "lineEnd" => Unit::LineEnd,
            "documentStart" => Unit::DocumentStart,
            "documentEnd" => Unit::DocumentEnd,
            _ => return None,
        })
    }
}

/// How far around a position grapheme boundaries are looked for: clusters
/// are short, and a long line is not copied whole for one step.
const WINDOW: usize = 256;

/// The grapheme boundary after `offset` (within its line; the line break
/// is a step of its own).
pub fn next_grapheme(text: &Text, offset: usize) -> usize {
    let line = text.line_of(offset);
    let end = text.line_end(line);
    if offset >= end {
        return (offset + 1).min(text.len_chars());
    }
    let from = offset.saturating_sub(WINDOW).max(text.line_start(line));
    let to = (offset + WINDOW).min(end);
    let s = text.slice(from, to);
    let mut at = from;
    for g in s.graphemes(true) {
        at += g.chars().count();
        if at > offset {
            return at;
        }
    }
    offset + 1
}

/// The grapheme boundary before `offset`.
pub fn prev_grapheme(text: &Text, offset: usize) -> usize {
    let offset = offset.min(text.len_chars());
    let line = text.line_of(offset);
    let start = text.line_start(line);
    if offset <= start {
        return offset.saturating_sub(1);
    }
    let from = offset.saturating_sub(WINDOW).max(start);
    let to = (offset + WINDOW).min(text.line_end(line));
    let s = text.slice(from, to);
    let mut at = from;
    let mut last = from;
    for g in s.graphemes(true) {
        if at >= offset {
            break;
        }
        last = at;
        at += g.chars().count();
    }
    last
}

/// A line break counts as a blank between words, and an empty line as a
/// word of its own (Vim stops there). `big`: every non-blank is one class.
fn class_of(text: &Text, offset: usize, words: &WordChars, big: bool) -> Option<Class> {
    text.char_at(offset).map(|c| match c {
        '\n' => Class::Blank,
        c if big && !c.is_whitespace() => Class::Punct,
        c => char_class(c, words),
    })
}

fn empty_line_at(text: &Text, offset: usize) -> bool {
    let line = text.line_of(offset);
    text.line_start(line) == offset && text.line_end(line) == offset
}

fn word_start_forward(text: &Text, mut at: usize, words: &WordChars, big: bool) -> usize {
    let class_at = |t: &Text, o: usize, w: &WordChars| class_of(t, o, w, big);
    let len = text.len_chars();
    let first = class_at(text, at, words);
    // Leave the current word.
    if let Some(c) = first.filter(|c| *c != Class::Blank) {
        while at < len && class_at(text, at, words) == Some(c) {
            at += 1;
        }
    }
    // Skip blanks, stopping at an empty line.
    while at < len && class_at(text, at, words) == Some(Class::Blank) {
        at += 1;
        if at < len && empty_line_at(text, at) {
            return at;
        }
    }
    at
}

fn word_start_backward(text: &Text, mut at: usize, words: &WordChars, big: bool) -> usize {
    let class_at = |t: &Text, o: usize, w: &WordChars| class_of(t, o, w, big);
    while at > 0 && class_at(text, at - 1, words) == Some(Class::Blank) {
        at -= 1;
        if empty_line_at(text, at) && at > 0 {
            return at;
        }
    }
    if at == 0 {
        return 0;
    }
    let c = class_at(text, at - 1, words);
    while at > 0 && class_at(text, at - 1, words) == c {
        at -= 1;
    }
    at
}

fn word_end_forward(text: &Text, mut at: usize, words: &WordChars, big: bool) -> usize {
    let class_at = |t: &Text, o: usize, w: &WordChars| class_of(t, o, w, big);
    let len = text.len_chars();
    while at < len && class_at(text, at, words) == Some(Class::Blank) {
        at += 1;
    }
    let c = class_at(text, at, words);
    while at < len && class_at(text, at, words) == c {
        at += 1;
    }
    at
}

fn word_end_backward(text: &Text, mut at: usize, words: &WordChars, big: bool) -> usize {
    let class_at = |t: &Text, o: usize, w: &WordChars| class_of(t, o, w, big);
    // Leave the word the position is in or just after.
    if at > 0
        && let Some(c) = class_at(text, at - 1, words).filter(|c| *c != Class::Blank)
    {
        while at > 0 && class_at(text, at - 1, words) == Some(c) {
            at -= 1;
        }
    }
    while at > 0 && class_at(text, at - 1, words) == Some(Class::Blank) {
        at -= 1;
    }
    at
}

/// Where `count` units from `from` is (negative counts go backwards); the
/// text's ends stop it.
pub fn position_of(text: &Text, from: usize, unit: Unit, count: i64, words: &WordChars) -> usize {
    let from = from.min(text.len_chars());
    let n = count.unsigned_abs().max(1);
    let back = count < 0;
    let line = text.line_of(from);
    match unit {
        Unit::LineStart => text.line_start(line),
        Unit::LineEnd => text.line_end(line),
        Unit::FirstNonBlank => {
            let (start, end) = (text.line_start(line), text.line_end(line));
            (start..end).find(|&o| text.char_at(o).is_some_and(|c| c != ' ' && c != '\t')).unwrap_or(end)
        }
        Unit::DocumentStart => 0,
        Unit::DocumentEnd => text.len_chars(),
        Unit::Grapheme | Unit::WordStart | Unit::WordEnd | Unit::BigWordStart | Unit::BigWordEnd => {
            let mut at = from;
            for _ in 0..n {
                let next = match (unit, back) {
                    (Unit::Grapheme, false) => next_grapheme(text, at),
                    (Unit::Grapheme, true) => prev_grapheme(text, at),
                    (Unit::WordStart, false) => word_start_forward(text, at, words, false),
                    (Unit::WordStart, true) => word_start_backward(text, at, words, false),
                    (Unit::WordEnd, false) => word_end_forward(text, at, words, false),
                    (Unit::WordEnd, true) => word_end_backward(text, at, words, false),
                    (Unit::BigWordStart, false) => word_start_forward(text, at, words, true),
                    (Unit::BigWordStart, true) => word_start_backward(text, at, words, true),
                    (Unit::BigWordEnd, false) => word_end_forward(text, at, words, true),
                    (_, _) => word_end_backward(text, at, words, true),
                };
                if next == at {
                    break;
                }
                at = next;
            }
            at
        }
    }
}

/// The matching bracket (Vim's `%`): from `at` on `open`, forward to its
/// `close`; on `close`, back to its `open`; with `forward` given, the
/// enclosing one that way. Nesting counts; strings and comments do not.
pub fn find_pair(text: &Text, at: usize, open: char, close: char, forward: Option<bool>) -> Option<usize> {
    let here = text.char_at(at);
    let (forward, mut o) = match forward {
        Some(f) => (f, if f { at } else { at.checked_sub(1)? }),
        None if here == Some(open) => (true, at + 1),
        None if here == Some(close) => (false, at.checked_sub(1)?),
        None => return None,
    };
    let (inner, outer) = if forward { (open, close) } else { (close, open) };
    let mut depth = 0usize;
    loop {
        let c = text.char_at(o)?;
        if c == outer {
            if depth == 0 {
                return Some(o);
            }
            depth -= 1;
        } else if c == inner {
            depth += 1;
        }
        if forward {
            o += 1;
        } else {
            o = o.checked_sub(1)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(s: &str, unit: Unit, count: i64) -> Vec<usize> {
        let t = Text::from(s);
        let w = WordChars::default();
        let mut at = if count < 0 { t.len_chars() } else { 0 };
        let mut seen = vec![at];
        loop {
            let next = position_of(&t, at, unit, count, &w);
            if next == at {
                return seen;
            }
            at = next;
            seen.push(at);
        }
    }

    #[test]
    fn graphemes_step_over_clusters_and_line_breaks() {
        // e + combining acute, a family emoji (one cluster, 5 chars), CRLF
        // read as LF.
        let s = "e\u{301}👨\u{200d}👩\u{200d}👧x\ny";
        assert_eq!(walk(s, Unit::Grapheme, 1), vec![0, 2, 7, 8, 9, 10]);
        assert_eq!(walk(s, Unit::Grapheme, -1), vec![10, 9, 8, 7, 2, 0]);
    }

    #[test]
    fn words_split_at_punctuation_scripts_and_empty_lines() {
        // Vim's w: foo, ., bar, (blank), 日本, ご, カタカナー, 漢字.
        let s = "foo.bar  日本ごカタカナー漢字";
        assert_eq!(walk(s, Unit::WordStart, 1), vec![0, 3, 4, 9, 11, 12, 17, 19]);
        assert_eq!(walk(s, Unit::WordStart, -1), vec![19, 17, 12, 11, 9, 4, 3, 0]);
        assert_eq!(walk(s, Unit::WordEnd, 1), vec![0, 3, 4, 7, 11, 12, 17, 19]);
        assert_eq!(walk("a\n\nb", Unit::WordStart, 1), vec![0, 2, 3, 4], "an empty line is a stop");
    }

    #[test]
    fn word_ends_backwards_and_line_parts() {
        let t = Text::from("one two  three");
        let w = WordChars::default();
        assert_eq!(position_of(&t, 10, Unit::WordEnd, -1, &w), 7, "ge from inside three");
        assert_eq!(position_of(&t, 7, Unit::WordEnd, -1, &w), 3, "from just after a word");
        assert_eq!(walk("one two  three", Unit::WordEnd, -1), vec![14, 7, 3, 0]);
        let t = Text::from("  \tab cd\nx");
        assert_eq!(position_of(&t, 6, Unit::FirstNonBlank, 1, &w), 3);
        assert_eq!(position_of(&t, 6, Unit::LineStart, 1, &w), 0);
        assert_eq!(position_of(&t, 1, Unit::LineEnd, 1, &w), 8);
        assert_eq!(position_of(&t, 1, Unit::DocumentEnd, 1, &w), 10);
    }

    #[test]
    fn big_words_are_blank_separated() {
        assert_eq!(walk("foo.bar  baz-q x", Unit::BigWordStart, 1), vec![0, 9, 15, 16]);
        assert_eq!(walk("foo.bar  baz", Unit::BigWordEnd, -1), vec![12, 7, 0]);
    }

    #[test]
    fn pairs_count_nesting() {
        let t = Text::from("f(a, (b), c)");
        assert_eq!(find_pair(&t, 1, '(', ')', None), Some(11));
        assert_eq!(find_pair(&t, 11, '(', ')', None), Some(1));
        assert_eq!(find_pair(&t, 7, '(', ')', None), Some(5));
        assert_eq!(find_pair(&t, 9, '(', ')', Some(false)), Some(1), "the enclosing one");
        assert_eq!(find_pair(&t, 3, '(', ')', Some(true)), Some(11));
        assert_eq!(find_pair(&t, 2, '(', ')', None), None, "not on a bracket");
        assert_eq!(find_pair(&Text::from("(("), 0, '(', ')', None), None);
    }

    #[test]
    fn word_characters_are_the_buffers() {
        let w = WordChars::new(r"[\p{L}\p{N}_-]").unwrap();
        let t = Text::from("foo-bar baz");
        assert_eq!(position_of(&t, 0, Unit::WordStart, 1, &w), 8, "- is a word character here");
        assert!(WordChars::new("[").is_err());
    }
}
