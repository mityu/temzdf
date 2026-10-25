//! Drawing primitives: a [`Canvas`] backed by `vello_cpu`, and text laid out
//! with `parley` using the system fonts (fontconfig on Linux, so CJK fallback
//! works out of the box).

use parley::{Alignment, AlignmentOptions, FontContext, FontWeight, Layout, LayoutContext, PositionedLayoutItem};
use vello_cpu::kurbo::{Affine, BezPath, Rect, RoundedRect, Shape};
use vello_cpu::peniko::color::{AlphaColor, Srgb};
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

/// Straight-alpha sRGB colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color(pub [u8; 4]);

impl Color {
    pub const TRANSPARENT: Self = Self([0, 0, 0, 0]);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b, 255])
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self([r, g, b, a])
    }

    fn to_vello(self) -> AlphaColor<Srgb> {
        let [r, g, b, a] = self.0;
        AlphaColor::from_rgba8(r, g, b, a)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub color: Color,
    pub bold: bool,
    /// A CSS-like family list, e.g. `"sans-serif"` or `"monospace"`.
    pub family: &'static str,
}

/// Shared text machinery: font database and layout scratch space. Create one per
/// application and reuse it.
pub struct Text {
    fonts: FontContext,
    layout: LayoutContext<Color>,
}

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

impl Text {
    pub fn new() -> Self {
        Self { fonts: FontContext::new(), layout: LayoutContext::new() }
    }

    /// Lay out `text`, wrapping at `max_width` pixels if given.
    pub fn layout(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> TextLayout {
        let mut b = self.layout.ranged_builder(&mut self.fonts, text, 1.0, true);
        b.push_default(parley::StyleProperty::FontSize(style.size));
        b.push_default(parley::StyleProperty::Brush(style.color));
        b.push_default(parley::StyleProperty::FontFamily(parley::FontFamily::from(style.family)));
        if style.bold {
            b.push_default(parley::StyleProperty::FontWeight(FontWeight::BOLD));
        }
        let mut layout: Layout<Color> = b.build(text);
        layout.break_all_lines(max_width);
        layout.align(Alignment::Start, AlignmentOptions::default());
        TextLayout { layout }
    }

    /// Lay out a single line, cut with an ellipsis to fit `max_width`.
    pub fn line(&mut self, text: &str, style: &TextStyle, max_width: f32) -> TextLayout {
        let full = self.layout(text, style, None);
        if full.width() <= max_width {
            return full;
        }
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let (mut lo, mut hi) = (0, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let candidate = format!("{}…", &text[..chars.get(mid).map_or(text.len(), |c| c.0)]);
            if self.layout(&candidate, style, None).width() <= max_width {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let cut = chars.get(lo).map_or(text.len(), |c| c.0);
        self.layout(&format!("{}…", &text[..cut]), style, None)
    }
}

pub struct TextLayout {
    layout: Layout<Color>,
}

impl TextLayout {
    pub fn width(&self) -> f32 {
        self.layout.width()
    }

    pub fn height(&self) -> f32 {
        self.layout.height()
    }

    /// Baseline of the first line, from the top of the layout.
    pub fn first_baseline(&self) -> f32 {
        self.layout.lines().next().map_or(0.0, |l| l.metrics().baseline)
    }
}

/// A fixed-size RGBA drawing surface.
pub struct Canvas {
    ctx: RenderContext,
    resources: Resources,
    width: u16,
    height: u16,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        let (w, h) = (width.clamp(1, u16::MAX as u32) as u16, height.clamp(1, u16::MAX as u32) as u16);
        Self { ctx: RenderContext::new(w, h), resources: Resources::new(), width: w, height: h }
    }

    pub fn width(&self) -> u32 {
        self.width as u32
    }

    pub fn height(&self) -> u32 {
        self.height as u32
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        self.ctx.set_paint(color.to_vello());
        self.ctx.fill_rect(&Rect::new(x as f64, y as f64, (x + w) as f64, (y + h) as f64));
    }

    pub fn fill_rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Color) {
        self.ctx.set_paint(color.to_vello());
        let r = RoundedRect::new(x as f64, y as f64, (x + w) as f64, (y + h) as f64, radius as f64);
        self.ctx.fill_path(&r.to_path(0.1));
    }

    /// Fill the closed polygon through `points`.
    pub fn fill_polygon(&mut self, points: &[(f32, f32)], color: Color) {
        let mut path = BezPath::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            let p = (x as f64, y as f64);
            if i == 0 { path.move_to(p) } else { path.line_to(p) }
        }
        path.close_path();
        self.ctx.set_paint(color.to_vello());
        self.ctx.fill_path(&path);
    }

    /// Draw a text layout with its top-left at `(x, y)`.
    pub fn text(&mut self, layout: &TextLayout, x: f32, y: f32) {
        for line in layout.layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else { continue };
                let style = run.style();
                self.ctx.set_paint(style.brush.to_vello());
                let font = run.run().font().clone();
                let size = run.run().font_size();
                let coords: Vec<i16> = run.run().normalized_coords().iter().map(|c| c.to_bits()).collect();
                let glyphs = run.positioned_glyphs().map(|g| Glyph { id: g.id, x: g.x + x, y: g.y + y });
                self.ctx.set_transform(Affine::IDENTITY);
                let _ = self
                    .ctx
                    .glyph_run(&mut self.resources, &font)
                    .font_size(size)
                    .normalized_coords(&coords)
                    .hint(true)
                    .fill_glyphs(glyphs);
            }
        }
    }

    /// Render and return straight-alpha RGBA8 pixels.
    pub fn finish(mut self) -> Vec<u8> {
        let mut pixmap = Pixmap::new(self.width, self.height);
        self.ctx.flush();
        self.ctx.render(&mut pixmap, &mut self.resources);
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for p in pixmap.data() {
            // vello_cpu produces premultiplied alpha; kitty expects straight alpha.
            let (r, g, b, a) = (p.r as u32, p.g as u32, p.b as u32, p.a as u32);
            if a == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let un = |c: u32| ((c * 255 + a / 2) / a).min(255) as u8;
                out.extend_from_slice(&[un(r), un(g), un(b), a as u8]);
            }
        }
        out
    }
}
