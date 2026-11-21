//! What the panes' searches share: the case rule of the Markdown viewer's
//! and the terminal's (the PDF pane keeps MuPDF's), and how the status
//! line counts hits.

use std::ops::Range;

/// `c` with its case folded, one character for one: the first of its
/// Unicode lowercase (full-width Latin, Greek and Cyrillic letters too).
pub fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Where `needle` is in `haystack`, case folded ([`fold`]): the byte
/// ranges in `haystack`, in order, not overlapping.
pub fn find_folded(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    let needle: Vec<char> = needle.chars().map(fold).collect();
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let chars: Vec<(usize, char)> = haystack.char_indices().map(|(i, c)| (i, fold(c))).collect();
    let mut i = 0;
    while i + needle.len() <= chars.len() {
        if chars[i..i + needle.len()].iter().map(|(_, c)| *c).eq(needle.iter().copied()) {
            let end = chars.get(i + needle.len()).map_or(haystack.len(), |(b, _)| *b);
            out.push(chars[i].0..end);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// The search's count on a status line: `k/N` with hit `ordinal` (from 1)
/// selected, else `N`; `+` after while hits are still being counted
/// (`complete` false), `no match` when the count is done and found none.
pub fn hit_count(ordinal: Option<usize>, total: usize, complete: bool) -> String {
    let more = if complete { "" } else { "+" };
    match ordinal {
        Some(k) => format!("{k}/{total}{more}"),
        None if total == 0 && complete => "no match".into(),
        None => format!("{total}{more}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_finds_any_case_by_the_original_bytes() {
        assert_eq!(find_folded("Ａｂｃ ａＢＣ abc", "ａｂｃ"), vec![0..9, 10..19]);
        assert_eq!(find_folded("ÉTÉ été", "été"), vec![0..5, 6..11]);
        // İ folds to i (its lowercase is i and a combining dot).
        assert_eq!(find_folded("xİx", "i"), vec![1..3]);
        assert_eq!(find_folded("aaaa", "aa"), vec![0..2, 2..4]);
        assert!(find_folded("abc", "").is_empty());
    }

    #[test]
    fn counts_read_alike_in_every_pane() {
        assert_eq!(hit_count(Some(3), 12, true), "3/12");
        assert_eq!(hit_count(Some(3), 12, false), "3/12+");
        assert_eq!(hit_count(None, 12, true), "12");
        assert_eq!(hit_count(None, 0, false), "0+");
        assert_eq!(hit_count(None, 0, true), "no match");
    }
}
