//! The font and the cell grid it makes: the session's `guifont` ("Family:h14",
//! as Neovim's) at the window's scale.

use glyphon::cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

/// A `guifont` value.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub family: String,
    /// Size in points.
    pub points: f32,
}

/// Points when `guifont` gives none.
const DEFAULT_POINTS: f32 = 14.0;

impl FontSpec {
    /// "Family:h14" (Neovim's form; other options after `:` are ignored),
    /// or a bare family.
    pub fn parse(s: &str) -> Self {
        let mut parts = s.split(':');
        let family = parts.next().unwrap_or("").trim().to_owned();
        let points = parts.find_map(|p| p.strip_prefix('h')?.parse().ok()).unwrap_or(DEFAULT_POINTS);
        Self { family, points }
    }

    /// The size in physical pixels at `scale` (points at 96 dpi).
    pub fn pixels(&self, scale: f64) -> f32 {
        (self.points as f64 * 96.0 / 72.0 * scale) as f32
    }

    pub fn attrs(&self) -> Attrs<'_> {
        if self.family.is_empty() {
            Attrs::new().family(Family::Monospace)
        } else {
            Attrs::new().family(Family::Name(&self.family))
        }
    }
}

/// The cell grid the font makes, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    /// The font size in pixels.
    pub font_px: f32,
}

/// Measure the font: a cell is as wide as `0` advances and as high as the
/// font's ascent, descent and line gap, rounded up to whole pixels so rows
/// and columns sit on pixel boundaries.
pub fn measure(fonts: &mut FontSystem, spec: &FontSpec, scale: f64) -> CellMetrics {
    let font_px = spec.pixels(scale).max(1.0);
    let mut buffer = Buffer::new(fonts, Metrics::new(font_px, font_px * 2.0));
    buffer.set_text("0", &spec.attrs(), Shaping::Advanced, None);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guifont_values() {
        assert_eq!(FontSpec::parse("UDEV Gothic NF:h14"), FontSpec { family: "UDEV Gothic NF".into(), points: 14.0 });
        assert_eq!(FontSpec::parse("Mono:b:h9.5").points, 9.5);
        assert_eq!(FontSpec::parse("Mono"), FontSpec { family: "Mono".into(), points: DEFAULT_POINTS });
        assert!((FontSpec::parse("x:h12").pixels(2.0) - 32.0).abs() < 1e-4, "12 pt at 96 dpi, doubled");
    }
}
