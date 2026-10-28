//! Fonts: the session's font lists (`o.guifont`, `o.guifontwide`), the
//! fonts built in (as WezTerm's: JetBrains Mono, Noto Color Emoji, Symbols
//! Nerd Font Mono; embedded from `TEMZDF_BUNDLED_FONTS` by build.rs), and
//! which font draws what.
//!
//! A glyph takes the first installed font of the list, then JetBrains Mono,
//! that has all its characters, at that font's size. When none has, the
//! first one does, with cosmic-text's fallbacks behind it: the emoji and
//! symbol fonts, then the platform's lists for the script and locale, then
//! any installed font. The first font sets the cell size.

use std::sync::Arc;

use glyphon::cosmic_text::{Attrs, Buffer, Fallback, Family, FontSystem, Metrics, PlatformFallback, Shaping, fontdb};
use temzdf_scene::FontEntry;
use unicode_script::Script;

include!(concat!(env!("OUT_DIR"), "/bundled_fonts.rs"));

/// The font after the configured ones (built in).
pub const BUILTIN_FAMILY: &str = "JetBrains Mono";
/// Fallbacks before the platform's (built in).
pub const BUILTIN_FALLBACKS: [&str; 2] = ["Noto Color Emoji", "Symbols Nerd Font Mono"];

/// cosmic-text's fallbacks: the built-in ones, then the platform's.
struct Fallbacks {
    platform: PlatformFallback,
    common: Vec<&'static str>,
}

impl Fallbacks {
    fn new() -> Self {
        let platform = PlatformFallback;
        let mut common = BUILTIN_FALLBACKS.to_vec();
        common.extend_from_slice(platform.common_fallback());
        Self { platform, common }
    }
}

impl Fallback for Fallbacks {
    fn common_fallback(&self) -> &[&'static str] {
        &self.common
    }

    fn forbidden_fallback(&self) -> &[&'static str] {
        self.platform.forbidden_fallback()
    }

    fn script_fallback(&self, script: Script, locale: &str) -> &[&'static str] {
        self.platform.script_fallback(script, locale)
    }
}

/// The installed fonts and the built-in ones.
pub fn system() -> FontSystem {
    let locale = sys_locale::get_locale().unwrap_or_else(|| "en-US".into());
    with_builtin(locale, |db| db.load_system_fonts())
}

/// Only the fonts in `dir`, and the built-in ones, in the `en-US` locale
/// (for tests that must not depend on the host, its locale included).
pub fn from_dir(dir: &std::path::Path) -> FontSystem {
    with_builtin("en-US".into(), |db| db.load_fonts_dir(dir))
}

/// `locale` picks the platform fallbacks for Han characters (Japanese,
/// Chinese or Korean forms).
fn with_builtin(locale: String, load: impl FnOnce(&mut fontdb::Database)) -> FontSystem {
    let mut db = fontdb::Database::new();
    load(&mut db);
    for data in BUNDLED {
        db.load_font_source(fontdb::Source::Binary(Arc::new(*data)));
    }
    db.set_monospace_family(BUILTIN_FAMILY);
    FontSystem::new_with_locale_and_db_and_fallback(locale, db, Fallbacks::new())
}

/// A font of the chain that is installed.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub family: String,
    pub points: f32,
    /// Where it was found (built in: none).
    pub path: Option<std::path::PathBuf>,
    id: fontdb::ID,
}

impl Face {
    pub fn attrs(&self) -> Attrs<'_> {
        Attrs::new().family(Family::Name(&self.family))
    }

    /// The size in physical pixels at `scale` (see [`PIXELS_PER_POINT`]).
    pub fn pixels(&self, scale: f64) -> f32 {
        pixels(self.points, scale)
    }
}

/// Logical pixels per point, each platform's convention (as Ghostty's and
/// kitty's font sizes): on macOS a point is a logical pixel (72 dpi), elsewhere
/// points are at 96 dpi.
const PIXELS_PER_POINT: f64 = if cfg!(target_os = "macos") { 1.0 } else { 96.0 / 72.0 };

fn pixels(points: f32, scale: f64) -> f32 {
    (points as f64 * PIXELS_PER_POINT * scale) as f32
}

/// The fonts glyphs are drawn with: the configured lists resolved against
/// what is installed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chain {
    /// `o.guifont`'s installed fonts, then JetBrains Mono unless listed.
    pub faces: Vec<Face>,
    /// `o.guifontwide`'s installed fonts.
    pub wide: Vec<Face>,
    /// Configured fonts that are not installed.
    pub missing: Vec<String>,
    /// The size when no font is installed at all.
    pub base_points: f32,
}

impl Chain {
    pub fn new(fonts: &FontSystem, font: &[FontEntry], wide: &[FontEntry]) -> Self {
        let base = temzdf_scene::base_points(font);
        let mut chain = Chain { base_points: base, ..Chain::default() };
        let builtin = FontEntry { family: BUILTIN_FAMILY.into(), size: None };
        let builtin_listed = font.iter().any(|f| f.family.eq_ignore_ascii_case(BUILTIN_FAMILY));
        let entries = font.iter().chain((!builtin_listed).then_some(&builtin));
        for (entry, wide) in entries.map(|e| (e, false)).chain(wide.iter().map(|e| (e, true))) {
            let face = find(fonts.db(), &entry.family).map(|(family, id, path)| Face {
                family,
                points: entry.size.unwrap_or(base),
                path,
                id,
            });
            match (face, wide) {
                (Some(f), false) => chain.faces.push(f),
                (Some(f), true) => chain.wide.push(f),
                (None, _) if entry.family != BUILTIN_FAMILY || builtin_listed => {
                    chain.missing.push(entry.family.clone())
                }
                (None, _) => {}
            }
        }
        chain
    }

    /// The font that sets the cell size.
    pub fn primary(&self) -> Option<&Face> {
        self.faces.first()
    }

    /// The font to draw `text` with (`wide`: a wide character): the first
    /// that has every character, else the first.
    pub fn pick(&self, fonts: &mut FontSystem, text: &str, wide: bool) -> Option<&Face> {
        let wide = if wide { &self.wide[..] } else { &[] };
        wide.iter().chain(&self.faces).find(|f| covers(fonts, f.id, text)).or_else(|| self.primary())
    }

    /// Attributes and size in pixels for `text`.
    pub fn attrs_for(&self, fonts: &mut FontSystem, text: &str, wide: bool, scale: f64) -> (Attrs<'_>, f32) {
        match self.pick(fonts, text, wide) {
            Some(f) => (f.attrs(), f.pixels(scale)),
            None => (Attrs::new().family(Family::Monospace), pixels(self.base_points, scale)),
        }
    }
}

/// `family` as installed (ignoring ASCII case) and its file (`None`: built in).
pub fn installed(fonts: &FontSystem, family: &str) -> Option<(String, Option<std::path::PathBuf>)> {
    find(fonts.db(), family).map(|(name, _, path)| (name, path))
}

/// Every family available and its files (`None`: built in), by name.
pub fn all_families(fonts: &FontSystem) -> Vec<(String, Option<std::path::PathBuf>)> {
    let mut out: Vec<(String, Option<std::path::PathBuf>)> = fonts
        .db()
        .faces()
        .flat_map(|f| {
            let path = match &f.source {
                fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) => Some(p.clone()),
                fontdb::Source::Binary(_) => None,
            };
            f.families.iter().map(move |(n, _)| (n.clone(), path.clone()))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The face of `family` (ignoring ASCII case): its name as installed, its id
/// and its file, if installed.
fn find(db: &fontdb::Database, family: &str) -> Option<(String, fontdb::ID, Option<std::path::PathBuf>)> {
    let name = db.faces().flat_map(|f| &f.families).map(|(n, _)| n).find(|n| n.eq_ignore_ascii_case(family))?;
    let query = fontdb::Query { families: &[fontdb::Family::Name(name)], ..fontdb::Query::default() };
    let id = db.query(&query)?;
    let path = match &db.face(id)?.source {
        fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) => Some(p.clone()),
        fontdb::Source::Binary(_) => None,
    };
    Some((name.clone(), id, path))
}

/// Whether font `id` has every character of `text` (controls aside).
fn covers(fonts: &mut FontSystem, id: fontdb::ID, text: &str) -> bool {
    let weight = fonts.db().face(id).map_or(fontdb::Weight::NORMAL, |f| f.weight);
    fonts.get_font(id, weight).is_some_and(|font| {
        let map = font.as_swash().charmap();
        text.chars().all(|c| c.is_control() || map.map(c) != 0)
    })
}

/// The cell grid the font makes, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    /// The first font's size in pixels.
    pub font_px: f32,
}

/// Measure the first font: a cell is as wide as `0` advances and as high as
/// the font's ascent, descent and line gap, rounded up to whole pixels so
/// rows and columns sit on pixel boundaries.
pub fn measure(fonts: &mut FontSystem, chain: &Chain, scale: f64) -> CellMetrics {
    let (attrs, font_px) = match chain.primary() {
        Some(f) => (f.attrs(), f.pixels(scale)),
        None => (Attrs::new().family(Family::Monospace), pixels(chain.base_points, scale)),
    };
    let font_px = font_px.max(1.0);
    let mut buffer = Buffer::new(fonts, Metrics::new(font_px, font_px * 2.0));
    buffer.set_text("0", &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    let glyph = buffer.layout_runs().next().and_then(|r| r.glyphs.first().cloned());
    let width = glyph.as_ref().map_or(font_px * 0.6, |g| g.w);
    let height = glyph
        .and_then(|g| fonts.get_font(g.font_id, g.font_weight))
        .map(|f| {
            let m = f.as_swash().metrics(&[]).scale(font_px);
            m.ascent + m.descent + m.leading
        })
        .unwrap_or(font_px * 1.2);
    CellMetrics { width: width.ceil().max(1.0) as u32, height: height.ceil().max(1.0) as u32, font_px }
}

/// The font each character of `text` is drawn with, as shaped (fallbacks
/// included): `(text, family, file)`; a file of `None` is built in.
pub fn fonts_used(
    fonts: &mut FontSystem,
    chain: &Chain,
    text: &str,
) -> Vec<(String, String, Option<std::path::PathBuf>)> {
    let mut out = Vec::new();
    for g in unicode_segmentation_free(text) {
        let wide = unicode_width::UnicodeWidthStr::width(g.as_str()) > 1;
        let (attrs, px) = chain.attrs_for(fonts, &g, wide, 1.0);
        let attrs = attrs.clone();
        let mut buffer = Buffer::new(fonts, Metrics::new(px.max(1.0), px.max(1.0) * 2.0));
        buffer.set_text(&g, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);
        let id = buffer.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.font_id));
        let (family, path) = id
            .and_then(|id| fonts.db().face(id))
            .map(|f| {
                let path = match &f.source {
                    fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) => Some(p.clone()),
                    fontdb::Source::Binary(_) => None,
                };
                (f.families.first().map_or_else(String::new, |(n, _)| n.clone()), path)
            })
            .unwrap_or_default();
        out.push((g, family, path));
    }
    out
}

/// `text` split into characters, a character followed by joiners and
/// variation selectors kept with it (emoji sequences).
fn unicode_segmentation_free(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut join = false;
    for c in text.chars() {
        let attaches = matches!(c, '\u{200d}' | '\u{fe00}'..='\u{fe0f}' | '\u{1f3fb}'..='\u{1f3ff}');
        match out.last_mut() {
            Some(last) if attaches || join => last.push(c),
            _ => out.push(c.to_string()),
        }
        join = c == '\u{200d}';
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_follow_the_platform_convention() {
        let expected = if cfg!(target_os = "macos") { 30.0 } else { 40.0 };
        assert_eq!(pixels(15.0, 2.0), expected);
    }

    fn entry(family: &str, size: Option<f32>) -> FontEntry {
        FontEntry { family: family.into(), size }
    }

    /// Only the built-in fonts (none when built without them).
    fn builtin_only() -> Option<FontSystem> {
        if BUNDLED.is_empty() {
            eprintln!("SKIPPED: built without TEMZDF_BUNDLED_FONTS");
            return None;
        }
        Some(with_builtin("en-US".into(), |_| {}))
    }

    #[test]
    fn the_chain_keeps_installed_fonts_then_jetbrains_mono() {
        let Some(mut fonts) = builtin_only() else { return };
        let chain = Chain::new(&fonts, &[entry("No Such Font", Some(11.0)), entry("Noto Color Emoji", None)], &[]);
        let names: Vec<&str> = chain.faces.iter().map(|f| f.family.as_str()).collect();
        assert_eq!(names, ["Noto Color Emoji", BUILTIN_FAMILY]);
        assert_eq!(chain.missing, ["No Such Font"]);
        assert_eq!(chain.faces[1].points, 11.0, "sizes left out follow the first font's");
        assert!(chain.faces.iter().all(|f| f.path.is_none()), "built in");
        // Listed already: not twice.
        let chain = Chain::new(&fonts, &[entry("jetbrains mono", Some(9.0))], &[]);
        assert_eq!(chain.faces.len(), 1);
        // A missing font does not decide the cell: JetBrains Mono does.
        let chain = Chain::new(&fonts, &[entry("No Such Font", Some(14.0))], &[]);
        let missing = measure(&mut fonts, &chain, 1.0);
        let chain = Chain::new(&fonts, &[entry(BUILTIN_FAMILY, Some(14.0))], &[]);
        let jetbrains = measure(&mut fonts, &chain, 1.0);
        assert_eq!(missing, jetbrains);
    }

    #[test]
    fn each_character_goes_to_a_font_that_has_it() {
        let Some(mut fonts) = builtin_only() else { return };
        let chain = Chain::new(&fonts, &[entry(BUILTIN_FAMILY, Some(14.0))], &[]);
        // U+F07B (a folder icon) is not in JetBrains Mono (its powerline
        // symbols are).
        let used = fonts_used(&mut fonts, &chain, "a😀\u{f07b}");
        let families: Vec<&str> = used.iter().map(|(_, f, _)| f.as_str()).collect();
        assert_eq!(families, [BUILTIN_FAMILY, "Noto Color Emoji", "Symbols Nerd Font Mono"]);
        // A later font of the list for what the first lacks, at its size.
        let chain = Chain::new(&fonts, &[entry(BUILTIN_FAMILY, None), entry("Noto Color Emoji", Some(20.0))], &[]);
        let picked = chain.pick(&mut fonts, "😀", false).map(|f| (f.family.as_str(), f.points));
        assert_eq!(picked, Some(("Noto Color Emoji", 20.0)));
        // The wide list first, for wide characters only.
        let chain = Chain::new(&fonts, &[entry(BUILTIN_FAMILY, None)], &[entry("Noto Color Emoji", Some(20.0))]);
        assert_eq!(chain.pick(&mut fonts, "😀", true).map(|f| f.points), Some(20.0));
        assert_eq!(chain.pick(&mut fonts, "a", false).map(|f| f.family.as_str()), Some(BUILTIN_FAMILY));
    }

    #[test]
    fn emoji_sequences_stay_together() {
        assert_eq!(unicode_segmentation_free("a👍🏽b❤\u{fe0f}"), ["a", "👍🏽", "b", "❤\u{fe0f}"]);
        assert_eq!(unicode_segmentation_free("👨\u{200d}👩"), ["👨\u{200d}👩"]);
    }
}
