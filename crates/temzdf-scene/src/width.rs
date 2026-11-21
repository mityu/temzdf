//! Display width: the cells (or GUI columns) text takes, one rule for every
//! pane, frontend and piece of chrome (unicode-width, per grapheme).

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The display width of a grapheme: as the GUI and terminals draw it, 1 or
/// 2.
pub fn grapheme_width(g: &str) -> usize {
    UnicodeWidthStr::width(g).clamp(1, 2)
}

/// The graphemes of `s` (extended grapheme clusters), the units widths are
/// counted in.
pub fn graphemes(s: &str) -> impl Iterator<Item = &str> {
    s.graphemes(true)
}

/// The display width of `s`: its graphemes' widths summed.
pub fn str_width(s: &str) -> usize {
    graphemes(s).map(grapheme_width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_characters_take_two_cells() {
        assert_eq!(str_width("a"), 1);
        assert_eq!(str_width("数式"), 4);
        assert_eq!(grapheme_width("\u{2614}"), 2, "umbrella with rain drops (☔)");
        assert_eq!(str_width("a\u{2614}b"), 4);
    }

    #[test]
    fn combining_marks_take_no_cell_after_a_base() {
        assert_eq!(str_width("e\u{301}"), 1);
        assert_eq!(graphemes("e\u{301}x").collect::<Vec<_>>(), ["e\u{301}", "x"]);
        assert_eq!(str_width("か\u{3099}"), 2, "a wide base keeps its width");
    }
}
