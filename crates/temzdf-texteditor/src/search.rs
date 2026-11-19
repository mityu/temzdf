//! Searching the text with Rust regular expressions (doc/design/text-editor.md
//! section 6): `regex-cursor` searches the rope in place, so a large buffer
//! is not copied; patterns it cannot take (back references, look-around)
//! go to `fancy-regex`, which searches a copy of the range. Converting Vim's
//! patterns is the scripts' job.
//!
//! [`search`] looks only as far as it must: forward it stops at the first
//! match it wants, backward it searches growing windows before `from` (see
//! [`backward`]), so a match next to the cursor costs little in a large buffer.

use std::ops::ControlFlow;

use regex_cursor::engines::meta::Regex as CursorRegex;
use regex_cursor::regex_automata::nfa::thompson::{self, State};
use regex_cursor::regex_automata::util::primitives::StateID;
use regex_cursor::{Input, RopeyCursor};

use crate::motion::{Class, WordChars, char_class};
use crate::text::Text;

/// A compiled pattern.
#[derive(Clone, Debug)]
pub enum Pattern {
    /// `one_line`: no match can contain a line break (see [`backward`]).
    Plain {
        re: Box<CursorRegex>,
        one_line: bool,
    },
    Fancy(Box<fancy_regex::Regex>),
}

impl Pattern {
    /// Rust `regex` syntax, or `fancy-regex`'s when it needs more. `^` and
    /// `$` match at line starts and ends (multi-line mode, as editors
    /// search); `\A` and `\z` at the text's.
    pub fn new(pattern: &str) -> Result<Self, String> {
        let syntax = regex_cursor::regex_automata::util::syntax::Config::new().multi_line(true);
        match CursorRegex::builder().syntax(syntax).build(pattern) {
            Ok(re) => Ok(Pattern::Plain { re: Box::new(re), one_line: never_matches_newline(pattern, syntax) }),
            Err(plain) => fancy_regex::Regex::new(&format!("(?m){pattern}"))
                .map(|re| Pattern::Fancy(Box::new(re)))
                .map_err(|_| plain.to_string()),
        }
    }
}

/// No match of `pattern` can contain `\n`: no transition reachable from the
/// anchored start of its NFA consumes that byte (look-arounds consume
/// nothing). Conservative: `false` when the NFA cannot be built.
fn never_matches_newline(pattern: &str, syntax: regex_cursor::regex_automata::util::syntax::Config) -> bool {
    let config = thompson::Config::new().which_captures(thompson::WhichCaptures::None);
    let Ok(nfa) = thompson::Compiler::new().syntax(syntax).configure(config).build(pattern) else { return false };
    let nl = b'\n';
    let mut seen = vec![false; nfa.states().len()];
    let mut stack = vec![nfa.start_anchored()];
    while let Some(id) = stack.pop() {
        if std::mem::replace(&mut seen[id.as_usize()], true) {
            continue;
        }
        match nfa.state(id) {
            State::ByteRange { trans } => {
                if (trans.start..=trans.end).contains(&nl) {
                    return false;
                }
                stack.push(trans.next);
            }
            State::Sparse(s) => {
                for t in s.transitions.iter() {
                    if (t.start..=t.end).contains(&nl) {
                        return false;
                    }
                    stack.push(t.next);
                }
            }
            State::Dense(d) => {
                if d.transitions[usize::from(nl)] != StateID::ZERO {
                    return false;
                }
                stack.extend(d.transitions.iter().copied().filter(|&n| n != StateID::ZERO));
            }
            State::Look { next, .. } | State::Capture { next, .. } => stack.push(*next),
            State::Union { alternates } => stack.extend(alternates.iter().copied()),
            State::BinaryUnion { alt1, alt2 } => stack.extend([*alt1, *alt2]),
            State::Fail | State::Match { .. } => {}
        }
    }
    true
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

/// Every group of one match, in characters (group 0: the whole match).
type Groups = Vec<Option<(usize, usize)>>;

/// How many lines before `from` a backward search looks first; the window
/// doubles until it finds a match or reaches the range's start.
const WINDOW_LINES: usize = 1000;

#[cfg(test)]
thread_local! {
    /// Matches the regex engines produced, and bytes counted converting
    /// fancy-regex's offsets: the tests check how much a search looks at.
    static VISITED: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
fn visit(matches: usize, bytes: usize) {
    VISITED.with(|v| v.set((v.get().0 + matches, v.get().1 + bytes)));
}

#[cfg(not(test))]
fn visit(_: usize, _: usize) {}

/// Every match the regex finds in `[start, end)`, in order, until `f` breaks.
fn raw_each(text: &Text, pat: &Pattern, start: usize, end: usize, f: impl FnMut(Groups) -> ControlFlow<()>) {
    match pat {
        Pattern::Plain { re, .. } => plain_each(text, re, start, start, end, f),
        Pattern::Fancy(re) => fancy_each(text, re, start, end, f),
    }
}

/// The matches a search of `[ctx, end)` finds once it has come to `scan`, in
/// order, until `f` breaks: the regex starts at `scan` but sees from `ctx`
/// (`^`, `\A`, `\b` look before `scan`), as `regex-automata`'s own iterator
/// sees the haystack when it resumes after a match.
fn plain_each(
    text: &Text,
    re: &CursorRegex,
    ctx: usize,
    scan: usize,
    end: usize,
    mut f: impl FnMut(Groups) -> ControlFlow<()>,
) {
    let rope = text.rope();
    let base = rope.char_to_byte(ctx);
    let slice = rope.slice(ctx..end);
    let input = Input::new(RopeyCursor::new(slice)).range(rope.char_to_byte(scan) - base..);
    for caps in re.captures_iter(input) {
        visit(1, 0);
        let groups = (0..caps.group_len())
            .map(|i| caps.get_group(i).map(|s| (rope.byte_to_char(base + s.start), rope.byte_to_char(base + s.end))))
            .collect();
        if f(groups).is_break() {
            return;
        }
    }
}

fn fancy_each(
    text: &Text,
    re: &fancy_regex::Regex,
    start: usize,
    end: usize,
    mut f: impl FnMut(Groups) -> ControlFlow<()>,
) {
    let s = text.slice(start, end);
    // Byte offsets of the copy to characters of the text, counting only
    // from the last offset converted: matches come in order, so this is
    // linear in the copy. Groups may lie before the previous one (a match's
    // groups in any order, look-behind before the match): count back then.
    let mut at = (0, 0);
    let mut chars = |b: usize| {
        let (lo, hi) = (b.min(at.0), b.max(at.0));
        let n = s[lo..hi].chars().count();
        visit(0, hi - lo);
        at = (b, if b >= at.0 { at.1 + n } else { at.1 - n });
        start + at.1
    };
    for caps in re.captures_iter(&s).flatten() {
        visit(1, 0);
        let groups = (0..caps.len()).map(|i| caps.get(i).map(|m| (chars(m.start()), chars(m.end())))).collect();
        if f(groups).is_break() {
            return;
        }
    }
}

/// Every match in `[start, end)`, in order, until `f` breaks (what
/// [`find_all`] collects).
pub(crate) fn find_each(
    text: &Text,
    pat: &Pattern,
    start: usize,
    end: usize,
    opts: &SearchOptions,
    words: &WordChars,
    mut f: impl FnMut(Found) -> ControlFlow<()>,
) {
    let (start, end) = clamp(text, start, end);
    raw_each(text, pat, start, end, |groups| match keep(groups, text, opts, words) {
        Some(m) => f(m),
        None => ControlFlow::Continue(()),
    });
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
    let mut out = Vec::new();
    find_each(text, pat, start, end, opts, words, |m| {
        out.push(m);
        ControlFlow::Continue(())
    });
    out
}

fn clamp(text: &Text, start: usize, end: usize) -> (usize, usize) {
    (start.min(text.len_chars()), end.min(text.len_chars()).max(start))
}

/// The match `groups` make when it counts: its counted group took part and,
/// with `opts.word`, it is a word.
fn keep(groups: Groups, text: &Text, opts: &SearchOptions, words: &WordChars) -> Option<Found> {
    let Some(Some((start, end))) = groups.get(opts.group).copied() else { return None };
    if opts.word && !at_word_bounds(text, start, end, words) {
        return None;
    }
    Some(Found { start, end, groups })
}

/// `[start, end)` starts and ends a word: word characters inside its ends,
/// none just outside.
fn at_word_bounds(text: &Text, start: usize, end: usize, words: &WordChars) -> bool {
    let word = |o: usize| text.char_at(o).is_some_and(|c| matches!(char_class(c, words), Class::Word(_)));
    end > start && word(start) && word(end - 1) && (start == 0 || !word(start - 1)) && !word(end)
}

/// The match `opts` asks for (see [`SearchOptions`]).
pub fn search(text: &Text, pat: &Pattern, opts: &SearchOptions, words: &WordChars) -> Option<Found> {
    search_with(text, pat, opts, words, WINDOW_LINES)
}

/// [`search`] with backward windows starting at `window` lines.
fn search_with(text: &Text, pat: &Pattern, opts: &SearchOptions, words: &WordChars, window: usize) -> Option<Found> {
    let (lo, hi) = match opts.lines {
        Some((a, b)) => (text.line_start(a.min(b)), text.line_end(a.max(b))),
        None => (0, text.len_chars()),
    };
    let from = opts.from.clamp(lo, hi);
    if opts.backward {
        return backward(text, pat, (lo, hi), from, opts, words, window);
    }
    // Matches starting at `from` count: search from its line's start, so
    // anchors and look-behind see what is before it.
    let line_start = text.line_start(text.line_of(from)).max(lo);
    let mut found = None;
    find_each(text, pat, line_start, hi, opts, words, |m| {
        if m.start >= from {
            found = Some(m);
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    if found.is_none() && opts.wrap {
        find_each(text, pat, lo, hi, opts, words, |m| {
            found = Some(m);
            ControlFlow::Break(())
        });
    }
    found
}

/// The last match of `[lo, hi)` starting before `from` (wrapping: else the
/// last of all), the same as enumerating every match of the range (matches
/// may reach past `from`) and picking.
///
/// A plain pattern none of whose matches can contain a line break is
/// searched in windows `[w, hi)`, `w` a line start, seeing from `lo`
/// ([`plain_each`]). That finds exactly the matches of the whole
/// enumeration starting at or after `w`: let `m` be the enumeration's last
/// match starting before `w`. It ends before `w` (it would contain the line
/// break at `w - 1` otherwise), so after it the enumeration searches from
/// `q = m.end < w` (or `q + 1` past an empty match at `q`), and its next
/// match is the leftmost one starting at or after `q` that it accepts, which
/// starts at or after `w`: no match starts in `[q + 1, w)`. The leftmost-first
/// match from there is then the one a search from `w` finds (same haystack,
/// same context), it is not empty at the previous match's end (`q < w`), and
/// from it on both enumerations are the same. Groups lie inside their match
/// for plain patterns, so their starts only grow along the enumeration, and
/// the matches before `w` all come before those in the window: a window's
/// answer is the whole range's. Without one, the window doubles toward
/// `lo`, scanning only what it has not yet scanned.
///
/// Other patterns (that can match a line break, or fancy-regex's: the range
/// is copied anyway) are enumerated from `lo`, stopping at `from` when that
/// cannot change the answer.
fn backward(
    text: &Text,
    pat: &Pattern,
    (lo, hi): (usize, usize),
    from: usize,
    opts: &SearchOptions,
    words: &WordChars,
    window: usize,
) -> Option<Found> {
    let Pattern::Plain { re, one_line: true } = pat else {
        // Group starts grow along the enumeration unless fancy-regex puts
        // one in a look-behind, before its match.
        let stop = !opts.wrap && (matches!(pat, Pattern::Plain { .. }) || opts.group == 0);
        let (mut before, mut last) = (None, None);
        raw_each(text, pat, lo, hi, |groups| {
            if stop && groups[0].is_some_and(|(s, _)| s >= from) {
                return ControlFlow::Break(());
            }
            if let Some(m) = keep(groups, text, opts, words) {
                // `last` is the last of all when nothing starts before `from`.
                if m.start < from {
                    before = Some(m);
                } else {
                    last = Some(m);
                }
            }
            ControlFlow::Continue(())
        });
        return before.or(if opts.wrap { last } else { None });
    };
    // The last match in windows growing back from line `top`, scanning each
    // only up to where the previous one began (`limit`), and no further back
    // than line `bottom`; `want` says which matches count.
    let windows = |top: usize, bottom: usize, mut limit: usize, want: &dyn Fn(&Found) -> bool| {
        let (mut top, mut n) = (top, window.max(1));
        loop {
            let first = top.saturating_sub(n).max(bottom);
            let w = text.line_start(first);
            let mut found = None;
            plain_each(text, re, lo, w, hi, |groups| {
                if groups[0].is_some_and(|(s, _)| s >= limit) {
                    return ControlFlow::Break(());
                }
                if let Some(m) = keep(groups, text, opts, words).filter(|m| want(m)) {
                    found = Some(m);
                }
                ControlFlow::Continue(())
            });
            if found.is_some() || first == bottom {
                return found;
            }
            (top, n, limit) = (first, n.saturating_mul(2), w);
        }
    };
    let (lo_line, from_line) = (text.line_of(lo), text.line_of(from));
    windows(from_line, lo_line, from, &|m| m.start < from).or_else(|| {
        // Nothing starts before `from`: wrapping, the last match is at or
        // after it, and a match starting before `from`'s line ends before it.
        opts.wrap.then(|| windows(text.line_of(hi), from_line, usize::MAX, &|_| true)).flatten()
    })
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
    #[cfg_attr(miri, ignore = "5 MiB of text: over ten minutes under Miri")]
    fn a_large_buffer_is_searched_in_place() {
        let mut s = "x".repeat(5 << 20);
        s.push_str("needle");
        let t = Text::from(s.as_str());
        let m = search(&t, &Pattern::new("needle").unwrap(), &SearchOptions::default(), &WordChars::default());
        assert_eq!(m.map(|m| m.start), Some(5 << 20));
    }

    /// The search before it stopped early: every match, then pick.
    fn reference_search(text: &Text, pat: &Pattern, opts: &SearchOptions, words: &WordChars) -> Option<Found> {
        let (lo, hi) = match opts.lines {
            Some((a, b)) => (text.line_start(a.min(b)), text.line_end(a.max(b))),
            None => (0, text.len_chars()),
        };
        let from = opts.from.clamp(lo, hi);
        let all = |a: usize, b: usize| reference_find_all(text, pat, a, b, opts, words);
        if opts.backward {
            let found = all(lo, hi);
            found.iter().rev().find(|m| m.start < from).or(if opts.wrap { found.last() } else { None }).cloned()
        } else {
            let line_start = text.line_start(text.line_of(from)).max(lo);
            let found = all(line_start, hi).into_iter().find(|m| m.start >= from);
            found.or_else(|| if opts.wrap { all(lo, hi).into_iter().next() } else { None })
        }
    }

    /// `find_all` before it counted fancy-regex's offsets incrementally.
    fn reference_find_all(
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
            Pattern::Plain { re, .. } => {
                let base = rope.char_to_byte(start);
                let input = Input::new(RopeyCursor::new(rope.slice(start..end)));
                for caps in re.captures_iter(input) {
                    let groups = (0..caps.group_len())
                        .map(|i| {
                            caps.get_group(i)
                                .map(|s| (rope.byte_to_char(base + s.start), rope.byte_to_char(base + s.end)))
                        })
                        .collect();
                    out.extend(keep(groups, text, opts, words));
                }
            }
            Pattern::Fancy(re) => {
                let s = text.slice(start, end);
                let chars = |b: usize| start + s[..b].chars().count();
                for caps in re.captures_iter(&s).flatten() {
                    let groups =
                        (0..caps.len()).map(|i| caps.get(i).map(|m| (chars(m.start()), chars(m.end())))).collect();
                    out.extend(keep(groups, text, opts, words));
                }
            }
        }
        out
    }

    #[test]
    fn only_patterns_that_can_match_a_line_break_are_multi_line() {
        let one_line = |p: &str| match Pattern::new(p).unwrap() {
            Pattern::Plain { one_line, .. } => one_line,
            Pattern::Fancy(_) => panic!("{p} is plain"),
        };
        for p in ["foo", "^foo$", r"\bfo+\b", ".*", r"\w+", "[a-z]", "", "^", r"[^\n]+", "日本"] {
            assert!(one_line(p), "{p}");
        }
        for p in [r"a\nb", r"x+\n?y", r"\s", "[^a]", "(?s).", r"\n", r"\W", "(?s:a.b)"] {
            assert!(!one_line(p), "{p}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "tens of thousands of searches: hours under Miri")]
    fn stopping_early_finds_what_enumerating_every_match_did() {
        // Sparse matches far apart, so the backward windows widen many times.
        let mut far = String::from("foo ab x\n");
        for i in 0..24 {
            far.push_str(if i % 9 == 5 { "y\nxy foo\n" } else { "-\n" });
        }
        far.push_str("a\nb foo");
        let texts = [
            "",
            "\n\n",
            "ab ab\nab",
            // A match crossing the line break before a window's start.
            "a\nb\nc\na\nb",
            "foo bar\n\nfoobar foo\n日本語 foo\nbaz foo bar\n\n",
            "a\nb\na\nb\nab\na\n\nb",
            "xxy\nx\ny\nxx\n\nyxy\nx\n",
            "aa bb\ncc a\n\na b\nbb aa\nab",
            far.as_str(),
        ];
        let patterns = [
            "foo",
            "^foo",
            "foo$",
            r"\bfoo\b",
            "fo*",
            "a|b",
            "",
            "^",
            "$",
            "^$",
            "x*",
            r"\w+",
            r"\Aa",
            r"a\z",
            "日本",
            "foo(bar)",
            "(b)?a",
            r"a\nb",
            r"x+\n?y",
            r"(a\n)?b",
            r"\n",
            r"\s+",
            "[^a]+",
            "(?s).b",
            r"(\w)\1",
            r"(?<=a)b",
            r"a(?=\n)",
            r"(?<=(a))b",
            r"(\w)\n\1",
        ];
        let w = WordChars::default();
        let mut checked = 0;
        for s in texts {
            let t = Text::from(s);
            let last = t.line_of(t.len_chars());
            let ranges = [None, Some((0, 0)), Some((last, last / 2)), Some((last.min(1), last))];
            // The long text with fewer options, to keep the test quick.
            let (ranges, words) =
                if s == far { (&ranges[2..], &[false][..]) } else { (&ranges[..], &[false, true][..]) };
            for p in patterns {
                let pat = Pattern::new(p).unwrap();
                let groups = if p.contains('(') { &[0, 1][..] } else { &[0] };
                for (&group, word, lines) in groups
                    .iter()
                    .flat_map(|g| words.iter().map(move |&word| (g, word)))
                    .flat_map(|(g, word)| ranges.iter().map(move |&lines| (g, word, lines)))
                {
                    let o = SearchOptions { group, word, lines, ..Default::default() };
                    assert_eq!(
                        find_all(&t, &pat, 0, t.len_chars(), &o, &w),
                        reference_find_all(&t, &pat, 0, t.len_chars(), &o, &w),
                        "find_all {p:?} in {s:?}, {o:?}"
                    );
                    for from in 0..=t.len_chars() + 1 {
                        for (backward, wrap) in [(false, false), (false, true), (true, false), (true, true)] {
                            let o = SearchOptions { from, backward, wrap, ..o };
                            let want = reference_search(&t, &pat, &o, &w);
                            for window in if backward { &[1, 2, 1000][..] } else { &[1000] } {
                                let got = search_with(&t, &pat, &o, &w, *window);
                                assert_eq!(got, want, "{p:?} in {s:?}, {o:?}, window {window}");
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 10_000);
    }

    fn visited<T>(f: impl FnOnce() -> T) -> (T, (usize, usize)) {
        VISITED.with(|v| v.set((0, 0)));
        let r = f();
        (r, VISITED.with(|v| v.get()))
    }

    #[test]
    fn a_match_near_the_cursor_is_found_without_visiting_the_rest() {
        let lines = 200_000;
        let mut s = "foo bar baz qux\n".repeat(lines);
        let mid = s.len() / 2;
        s.insert_str(mid, "needle ");
        let t = Text::from(s.as_str());
        let w = WordChars::default();
        let at = |p: &str, from, backward| {
            let o = SearchOptions { from, backward, ..Default::default() };
            visited(|| search(&t, &Pattern::new(p).unwrap(), &o, &w).map(|m| m.start))
        };
        assert_eq!(at("needle", mid - 3, false), (Some(mid), (1, 0)));
        assert_eq!(at("needle", mid + 3, true), (Some(mid), (1, 0)));
        let (m, (n, _)) = at("baz", mid, false);
        assert_eq!((m, n), (Some(mid + 7 + 8), 1));
        // One window back: at most a match per line of it.
        let (m, (n, _)) = at("baz", mid, true);
        assert_eq!(m, Some(mid - 8));
        assert!(n <= WINDOW_LINES + 1, "{n} of {lines} matches visited");
    }

    #[test]
    fn fancy_offsets_are_counted_once() {
        // Fewer under Miri, where the long text takes hours.
        let n = if cfg!(miri) { 500 } else { 200_000 };
        let s = "aa ".repeat(n);
        let t = Text::from(s.as_str());
        let p = Pattern::new(r"(\w)\1").unwrap();
        let (all, (_, bytes)) =
            visited(|| find_all(&t, &p, 0, t.len_chars(), &SearchOptions::default(), &WordChars::default()));
        assert_eq!(all.len(), n);
        assert_eq!(all.last().map(|m| (m.start, m.groups[1])), Some((s.len() - 3, Some((s.len() - 3, s.len() - 2)))));
        // Going back from each match's end to its group's start costs little.
        assert!(bytes <= 3 * s.len(), "{bytes} bytes counted for a copy of {}", s.len());
    }
}
