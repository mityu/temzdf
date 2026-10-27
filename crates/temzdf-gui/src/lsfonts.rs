//! `temzdf-gui ls-fonts` (behind `temzdf ls-fonts`): which fonts a font
//! list resolves to, as WezTerm's `ls-fonts` shows them.

use std::fmt::Write;
use std::path::PathBuf;

use glyphon::cosmic_text::FontSystem;
use temzdf_scene::FontEntry;

use crate::fonts::{self, BUILTIN_FALLBACKS, Chain};

fn file(path: &Option<PathBuf>) -> String {
    path.as_ref().map_or_else(|| "<built-in>".to_owned(), |p| p.display().to_string())
}

/// The report for `font` and `wide` (`o.guifont`, `o.guifontwide`):
/// the chain, or with `text` the font of each of its characters, or with
/// `list_system` every family available.
pub fn report(
    fonts: &mut FontSystem,
    font: &[FontEntry],
    wide: &[FontEntry],
    text: Option<&str>,
    list_system: bool,
) -> String {
    let mut out = String::new();
    if list_system {
        for (family, path) in fonts::all_families(fonts) {
            let _ = writeln!(out, "{family}\t{}", file(&path));
        }
        return out;
    }
    let chain = Chain::new(fonts, font, wide);
    if let Some(text) = text {
        for (chars, family, path) in fonts::fonts_used(fonts, &chain, text) {
            let _ = writeln!(out, "{chars:?}\t{family}\t{}", file(&path));
        }
        return out;
    }
    let cell = fonts::measure(fonts, &chain, 1.0);
    let face = |f: &fonts::Face| format!("{} {}pt\t{}", f.family, f.points, file(&f.path));
    // The configured fonts in their order, found or not; then the built-in one.
    let list = |out: &mut String, entries: &[FontEntry], faces: &[fonts::Face], first: bool| {
        let mut faces = faces.iter().peekable();
        for e in entries {
            match faces.next_if(|f| f.family.eq_ignore_ascii_case(&e.family)) {
                Some(f) => {
                    let _ = writeln!(out, "  {}", face(f));
                }
                None => {
                    let _ = writeln!(out, "  {}\tnot found", e.family);
                }
            }
        }
        for f in faces {
            let _ = writeln!(out, "  {}", face(f));
        }
        if first {
            let _ = writeln!(out, "  (the cell: {}x{} px at scale 1, from the first found)", cell.width, cell.height);
        }
    };
    let _ = writeln!(out, "Fonts, first choice first (o.guifont, then built in):");
    list(&mut out, font, &chain.faces, true);
    if !wide.is_empty() {
        let _ = writeln!(out, "Wide characters first (o.guifontwide):");
        list(&mut out, wide, &chain.wide, false);
    }
    let _ = writeln!(out, "Then for what those lack:");
    for family in BUILTIN_FALLBACKS {
        match fonts::installed(fonts, family) {
            Some((name, path)) => {
                let _ = writeln!(out, "  {name}\t{}", file(&path));
            }
            None => {
                let _ = writeln!(out, "  {family}\tnot found");
            }
        }
    }
    let _ = writeln!(out, "  the system's fonts for each script (locale {})", fonts.locale());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_names_files_and_what_is_missing() {
        if fonts::BUNDLED.is_empty() {
            eprintln!("SKIPPED: built without TEMZDF_BUNDLED_FONTS");
            return;
        }
        let mut fonts = fonts::from_dir(std::path::Path::new("/nonexistent"));
        let font = [FontEntry { family: "No Such Font".into(), size: Some(12.0) }];
        let r = report(&mut fonts, &font, &[], None, false);
        assert!(r.contains("No Such Font\tnot found"), "{r}");
        let lines: Vec<&str> = r.lines().collect();
        assert_eq!(lines[1..3], ["  No Such Font\tnot found", "  JetBrains Mono 12pt\t<built-in>"], "in order");
        assert!(r.contains("Noto Color Emoji\t<built-in>"), "{r}");
        let r = report(&mut fonts, &font, &[], Some("a😀"), false);
        assert_eq!(r, "\"a\"\tJetBrains Mono\t<built-in>\n\"😀\"\tNoto Color Emoji\t<built-in>\n");
        let r = report(&mut fonts, &font, &[], None, true);
        assert!(r.lines().any(|l| l == "Symbols Nerd Font Mono\t<built-in>"), "{r}");
    }
}
