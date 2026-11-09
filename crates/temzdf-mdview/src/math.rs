//! Math as SVG for the page (RaTeX: glyphs as paths, no fonts needed),
//! sitting on the text's baseline.

/// `tex` as an inline SVG at `size` CSS pixels per em (a display block
/// larger); its source in `<code>` when it does not parse.
pub fn svg(tex: &str, display: bool) -> String {
    use ratex_layout::{LayoutOptions, layout, to_display_list};
    use ratex_types::math_style::MathStyle;
    let size = 16.0;
    let style = if display { MathStyle::Display } else { MathStyle::Text };
    let Ok(ast) = ratex_parser::parser::parse(tex) else {
        let mut s = String::from("<code>");
        for c in tex.chars() {
            match c {
                '&' => s.push_str("&amp;"),
                '<' => s.push_str("&lt;"),
                '>' => s.push_str("&gt;"),
                c => s.push(c),
            }
        }
        s.push_str("</code>");
        return s;
    };
    let list = to_display_list(&layout(&ast, &LayoutOptions::default().with_style(style)));
    let opts = ratex_svg::SvgOptions {
        font_size: size,
        padding: 0.0,
        stroke_width: 1.0,
        embed_glyphs: true,
        font_dir: String::new(),
    };
    let svg = ratex_svg::render_to_svg(&list, &opts);
    svg.replacen("<svg ", &format!("<svg style=\"vertical-align:{:.2}px\" ", -list.depth * size), 1)
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg_attr(miri, ignore = "slow under Miri")]
    fn formulas_are_svgs_and_errors_their_source() {
        let s = super::svg(r"\frac{a}{b}", true);
        assert!(s.starts_with("<svg style=\"vertical-align:") && s.contains("<path"), "{s}");
        assert_eq!(super::svg(r"\frac{a", false), r"<code>\frac{a</code>");
    }
}
