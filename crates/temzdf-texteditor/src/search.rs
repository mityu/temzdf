//! Searching the text with Rust regular expressions (doc/design/text-editor.md
//! section 6): `regex-cursor` searches the rope in place, so a large buffer
//! is not copied; patterns it cannot take (back references, look-around)
//! go to `fancy-regex`, which searches a copy of the range. Converting Vim's
//! patterns is the scripts' job.

use regex_cursor::engines::meta::Regex as CursorRegex;
use regex_cursor::{Input, RopeyCursor};

use crate::motion::{Class, WordChars, char_class};
use crate::text::Text;

/// A compiled pattern.
#[derive(Clone, Debug)]
pub enum Pattern {
    Plain(Box<CursorRegex>),
    Fancy(Box<fancy_regex::Regex>),
}

impl Pattern {
    /// Rust `regex` syntax, or `fancy-regex`'s when it needs more. `^` and
    /// `$` match at line starts and ends (multi-line mode, as editors
    /// search); `\A` and `\z` at the text's.
    pub fn new(pattern: &str) -> Result<Self, String> {
        let syntax = regex_cursor::regex_automata::util::syntax::Config::new().multi_line(true);
        match CursorRegex::builder().syntax(syntax).build(pattern) {
            Ok(re) => Ok(Pattern::Plain(Box::new(re))),
            Err(plain) => fancy_regex::Regex::new(&format!("(?m){pattern}"))
                .map(|re| Pattern::Fancy(Box::new(re)))
                .map_err(|_| plain.to_string()),
        }
    }
}

/// Where and how to look.
#[derive(Clone, Copy, Debug, Default)]
pub struct SearchOptions {
    /// Forward: the first match starting at or after `from`; backward: the
    /// last one starting before it.
    pub from: usize,
    pub backward: bool,
    /// Past the end (start), go on from the other end.
    pub wrap: bool,
    /// Only these lines (inclusive).
    pub lines: Option<(usize, usize)>,
    /// The capture group that counts as the match (0: all of it; Vim's
    /// `\zs`/`\ze` become a group).
    pub group: usize,
    /// The match must start and end at word boundaries of the buffer's word
    /// characters (Vim's `\<` and `\>` against `iskeyword`).
    pub word: bool,
}

/// A match: its range (of the counted group), and every group's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<(usize, usize)>>,
}

/// Every match in `[start, end)`, in order.
pub fn find_all(
    text: &Text,
    pat: &Pattern,
    start: usize,
    end: usize,
    opts: &SearchOptions,
    words: &WordChars,
) -> Vec<Found> {
    let (start, end) = (start.min(text.len_chars()), end.min(text.len_chars()).max(start));
    let rope = text.rope();
    let mut out = Vec::new();
    match pat {
        Pattern::Plain(re) => {
            let base = rope.char_to_byte(start);
            let slice = rope.slice(start..end);
            let input = Input::new(RopeyCursor::new(slice));
            for caps in re.captures_iter(input) {
                let groups: Vec<Option<(usize, usize)>> = (0..caps.group_len())
                    .map(|i| {
                        caps.get_group(i).map(|s| (rope.byte_to_char(base + s.start), rope.byte_to_char(base + s.end)))
                    })
                    .collect();
                push(&mut out, groups, text, opts, words);
            }
        }
        Pattern::Fancy(re) => {
            let s = text.slice(start, end);
            // Byte offsets of the copy to characters of the text.
            let chars = |b: usize| start + s[..b].chars().count();
            for caps in re.captures_iter(&s).flatten() {
                let groups = (0..caps.len()).map(|i| caps.get(i).map(|m| (chars(m.start()), chars(m.end())))).collect();
                push(&mut out, groups, text, opts, words);
            }
        }
    }
    out
}

fn push(
    out: &mut Vec<Found>,
    groups: Vec<Option<(usize, usize)>>,
    text: &Text,
    opts: &SearchOptions,
    words: &WordChars,
) {
    let Some(Some((start, end))) = groups.get(opts.group).copied() else { return };
    if opts.word && !at_word_bounds(text, start, end, words) {
        return;
    }
    out.push(Found { start, end, groups });
}

/// `[start, end)` starts and ends a word: word characters inside its ends,
/// none just outside.
fn at_word_bounds(text: &Text, start: usize, end: usize, words: &WordChars) -> bool {
    let word = |o: usize| text.char_at(o).is_some_and(|c| matches!(char_class(c, words), Class::Word(_)));
    end > start && word(start) && word(end - 1) && (start == 0 || !word(start - 1)) && !word(end)
}

/// The match `opts` asks for (see [`SearchOptions`]).
pub fn search(text: &Text, pat: &Pattern, opts: &SearchOptions, words: &WordChars) -> Option<Found> {
    let (lo, hi) = match opts.lines {
        Some((a, b)) => (text.line_start(a.min(b)), text.line_end(a.max(b))),
        None => (0, text.len_chars()),
    };
    let from = opts.from.clamp(lo, hi);
    let all = |a: usize, b: usize| find_all(text, pat, a, b, opts, words);
    if opts.backward {
        // Matches may reach past `from`: look in all of the range, keep
        // those starting before it.
        let found = all(lo, hi);
        found.iter().rev().find(|m| m.start < from).or(if opts.wrap { found.last() } else { None }).cloned()
    } else {
        // Matches starting at `from` count: search from its line's start,
        // so anchors and look-behind see what is before it.
        let line_start = text.line_start(text.line_of(from)).max(lo);
        let found = all(line_start, hi).into_iter().find(|m| m.start >= from);
        found.or_else(|| if opts.wrap { all(lo, hi).into_iter().next() } else { None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(text: &str, pat: &str, opts: SearchOptions) -> Option<(usize, usize)> {
        let t = Text::from(text);
        search(&t, &Pattern::new(pat).unwrap(), &opts, &WordChars::default()).map(|m| (m.start, m.end))
    }

    #[test]
    fn forward_backward_and_wrapping() {
        let s = "ab ab\nab";
        let o = |from, backward, wrap| SearchOptions { from, backward, wrap, ..Default::default() };
        assert_eq!(find(s, "ab", o(0, false, false)), Some((0, 2)), "at the start counts");
        assert_eq!(find(s, "ab", o(1, false, false)), Some((3, 5)));
        assert_eq!(find(s, "ab", o(7, false, false)), None);
        assert_eq!(find(s, "ab", o(7, false, true)), Some((0, 2)), "wrapped");
        assert_eq!(find(s, "ab", o(3, true, false)), Some((0, 2)));
        assert_eq!(find(s, "ab", o(0, true, true)), Some((6, 8)), "wrapped backwards");
        assert_eq!(find(s, "^ab", o(1, false, false)), Some((6, 8)), "^ is a line's start");
        assert_eq!(find(s, "ab$", o(0, false, false)), Some((3, 5)), "$ a line's end");
        assert_eq!(find(s, r"\Aab", o(1, false, true)), Some((0, 2)), "\\A the text's start");
    }

    #[test]
    fn groups_lines_words_and_characters_not_bytes() {
        let s = "日本語 foo\nfoobar foo";
        let t = Text::from(s);
        let w = WordChars::default();
        let all = find_all(&t, &Pattern::new("foo").unwrap(), 0, t.len_chars(), &SearchOptions::default(), &w);
        assert_eq!(all.iter().map(|m| m.start).collect::<Vec<_>>(), [4, 8, 15]);
        let word = SearchOptions { word: true, ..Default::default() };
        let all = find_all(&t, &Pattern::new("foo").unwrap(), 0, t.len_chars(), &word, &w);
        assert_eq!(all.iter().map(|m| m.start).collect::<Vec<_>>(), [4, 15], "foobar's foo is no word");
        // \zs-like: group 1 counts.
        let g = SearchOptions { group: 1, ..Default::default() };
        assert_eq!(find(s, "foo(bar)", g), Some((11, 14)));
        let lines = SearchOptions { lines: Some((1, 1)), ..Default::default() };
        assert_eq!(find(s, "foo", lines), Some((8, 11)));
    }

    #[test]
    fn back_references_go_to_fancy_regex() {
        let p = Pattern::new(r"(\w)\1").unwrap();
        assert!(matches!(p, Pattern::Fancy(_)));
        assert_eq!(find("abccd", r"(\w)\1", SearchOptions::default()), Some((2, 4)));
        assert_eq!(find("xay ab", r"a(?=b)", SearchOptions::default()), Some((4, 5)), "look-ahead");
        assert!(Pattern::new("(").is_err());
    }

    #[test]
    fn a_large_buffer_is_searched_in_place() {
        let mut s = "x".repeat(5 << 20);
        s.push_str("needle");
        let t = Text::from(s.as_str());
        let m = search(&t, &Pattern::new("needle").unwrap(), &SearchOptions::default(), &WordChars::default());
        assert_eq!(m.map(|m| m.start), Some(5 << 20));
    }
}
