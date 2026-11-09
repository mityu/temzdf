//! The graphics mode's input: a [`Document`] as HTML for Blitz. Every
//! block carries its first source line (`data-line`), so positions map
//! between the page and the source; headings carry their anchors as ids
//! and footnotes `fn-LABEL`, as the text mode names them.

use crate::doc::{AlertKind, Align, Block, Document, Inline};

/// Math as HTML (an SVG, say): the TeX and whether it is a display block.
pub type MathHtml<'a> = &'a dyn Fn(&str, bool) -> String;

/// `doc` as the body of an HTML page (the caller adds the head and style).
pub fn body(doc: &Document, math: MathHtml<'_>) -> String {
    let mut out = String::new();
    let mut footnotes = false;
    for b in &doc.blocks {
        if matches!(b, Block::Footnote { .. }) && !footnotes {
            footnotes = true;
            out.push_str("<section class=\"footnotes\"><hr>\n");
        }
        block(b, math, &mut out);
    }
    if footnotes {
        out.push_str("</section>\n");
    }
    out
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

fn block(b: &Block, math: MathHtml<'_>, out: &mut String) {
    let line = b.lines().start;
    match b {
        Block::Heading { level, inlines, anchor, .. } => {
            out.push_str(&format!("<h{level} id=\""));
            escape(anchor, out);
            out.push_str(&format!("\" data-line=\"{line}\">"));
            inlines_html(inlines, math, out);
            out.push_str(&format!("</h{level}>\n"));
        }
        Block::Paragraph { inlines, .. } => {
            out.push_str(&format!("<p data-line=\"{line}\">"));
            inlines_html(inlines, math, out);
            out.push_str("</p>\n");
        }
        Block::List { start, items, tight, .. } => {
            let task = items.iter().any(|i| i.task.is_some());
            let class = if task { " class=\"contains-task-list\"" } else { "" };
            match start {
                Some(s) if *s != 1 => out.push_str(&format!("<ol start=\"{s}\"{class} data-line=\"{line}\">\n")),
                Some(_) => out.push_str(&format!("<ol{class} data-line=\"{line}\">\n")),
                None => out.push_str(&format!("<ul{class} data-line=\"{line}\">\n")),
            }
            for (n, item) in items.iter().enumerate() {
                out.push_str(if item.task.is_some() { "<li class=\"task-list-item\">" } else { "<li>" });
                if let Some(s) = start {
                    out.push_str(&format!("<span class=\"list-number\">{}.</span>", s + n));
                }
                if let Some(done) = item.task {
                    out.push_str(if done {
                        "<input type=\"checkbox\" checked disabled> "
                    } else {
                        "<input type=\"checkbox\" disabled> "
                    });
                }
                for (i, b) in item.blocks.iter().enumerate() {
                    // A tight list's paragraphs are bare text.
                    match b {
                        Block::Paragraph { inlines, .. } if *tight => {
                            if i > 0 {
                                out.push('\n');
                            }
                            inlines_html(inlines, math, out);
                        }
                        b => block(b, math, out),
                    }
                }
                out.push_str("</li>\n");
            }
            out.push_str(if start.is_some() { "</ol>\n" } else { "</ul>\n" });
        }
        Block::Code { lang, text, .. } => {
            out.push_str(&format!("<pre data-line=\"{line}\"><code"));
            if !lang.is_empty() {
                out.push_str(" class=\"language-");
                escape(lang, out);
                out.push('"');
            }
            out.push('>');
            escape(text, out);
            out.push_str("</code></pre>\n");
        }
        Block::Math { tex, .. } => {
            out.push_str(&format!("<div class=\"math-display\" data-line=\"{line}\">"));
            out.push_str(&math(tex, true));
            out.push_str("</div>\n");
        }
        Block::Quote { blocks, .. } => {
            out.push_str(&format!("<blockquote data-line=\"{line}\">\n"));
            blocks.iter().for_each(|b| block(b, math, out));
            out.push_str("</blockquote>\n");
        }
        Block::Alert { kind, title, blocks, .. } => {
            let name = match kind {
                AlertKind::Note => "note",
                AlertKind::Tip => "tip",
                AlertKind::Important => "important",
                AlertKind::Warning => "warning",
                AlertKind::Caution => "caution",
            };
            out.push_str(&format!("<div class=\"markdown-alert markdown-alert-{name}\" data-line=\"{line}\">\n"));
            out.push_str("<p class=\"markdown-alert-title\">");
            escape(title.as_deref().unwrap_or(kind.name()), out);
            out.push_str("</p>\n");
            blocks.iter().for_each(|b| block(b, math, out));
            out.push_str("</div>\n");
        }
        Block::Table { aligns, header, rows, .. } => {
            let cell = |tag: &str, i: usize, c: &[Inline], out: &mut String| {
                let align = match aligns.get(i).copied().unwrap_or(Align::None) {
                    Align::None => "",
                    Align::Left => " align=\"left\"",
                    Align::Center => " align=\"center\"",
                    Align::Right => " align=\"right\"",
                };
                out.push_str(&format!("<{tag}{align}>"));
                inlines_html(c, math, out);
                out.push_str(&format!("</{tag}>"));
            };
            out.push_str(&format!("<table data-line=\"{line}\">\n<thead><tr>"));
            header.iter().enumerate().for_each(|(i, c)| cell("th", i, c, out));
            out.push_str("</tr></thead>\n<tbody>\n");
            for r in rows {
                out.push_str("<tr>");
                r.iter().enumerate().for_each(|(i, c)| cell("td", i, c, out));
                out.push_str("</tr>\n");
            }
            out.push_str("</tbody></table>\n");
        }
        Block::Html { html, .. } => {
            out.push_str(&format!("<div data-line=\"{line}\">"));
            out.push_str(html);
            out.push_str("</div>\n");
        }
        Block::Rule { .. } => out.push_str(&format!("<hr data-line=\"{line}\">\n")),
        Block::Footnote { label, number, blocks, .. } => {
            out.push_str("<div class=\"footnote\" id=\"fn-");
            escape(label, out);
            out.push_str(&format!("\" data-line=\"{line}\"><span class=\"footnote-number\">{number}.</span> "));
            blocks.iter().for_each(|b| block(b, math, out));
            out.push_str("</div>\n");
        }
    }
}

fn inlines_html(inlines: &[Inline], math: MathHtml<'_>, out: &mut String) {
    for i in inlines {
        match i {
            Inline::Text(t) => escape(t, out),
            Inline::Code(c) => {
                out.push_str("<code>");
                escape(c, out);
                out.push_str("</code>");
            }
            Inline::Emph(c) => wrap("em", c, math, out),
            Inline::Strong(c) => wrap("strong", c, math, out),
            Inline::Strike(c) => wrap("del", c, math, out),
            Inline::Link { url, children } => {
                out.push_str("<a href=\"");
                escape(url, out);
                out.push_str("\">");
                inlines_html(children, math, out);
                out.push_str("</a>");
            }
            Inline::Image { url, alt } => {
                out.push_str("<img src=\"");
                escape(url, out);
                out.push_str("\" alt=\"");
                escape(alt, out);
                out.push_str("\">");
            }
            Inline::Math(tex) => out.push_str(&math(tex, false)),
            Inline::Html(h) => out.push_str(h),
            Inline::FootnoteRef { label, number } => {
                out.push_str("<sup class=\"footnote-ref\"><a href=\"#fn-");
                escape(label, out);
                out.push_str(&format!("\">{number}</a></sup>"));
            }
            Inline::SoftBreak => out.push('\n'),
            Inline::HardBreak => out.push_str("<br>\n"),
        }
    }
}

fn wrap(tag: &str, children: &[Inline], math: MathHtml<'_>, out: &mut String) {
    out.push_str(&format!("<{tag}>"));
    inlines_html(children, math, out);
    out.push_str(&format!("</{tag}>"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(src: &str) -> String {
        body(&Document::parse(src), &|tex, display| format!("[{}{tex}]", if display { "D:" } else { "" }))
    }

    #[test]
    fn blocks_carry_source_lines_and_headings_their_anchors() {
        assert_eq!(
            html("# A & B\n\ntext 1 < 2 *em*\n\n- [x] t\n"),
            "<h1 id=\"a--b\" data-line=\"0\">A &amp; B</h1>\n<p data-line=\"2\">text 1 &lt; 2 <em>em</em></p>\n\
             <ul class=\"contains-task-list\" data-line=\"4\">\n<li class=\"task-list-item\"><input type=\"checkbox\" checked disabled> t</li>\n</ul>\n"
        );
    }

    #[test]
    fn math_links_images_and_footnotes() {
        let h = html("$x$ [l](u \"t\") ![a](i.png)[^n]\n\n$$\ny\n$$\n\n[^n]: F.\n");
        assert!(h.contains("[x] <a href=\"u\">l</a> <img src=\"i.png\" alt=\"a\">"), "{h}");
        let ol = html("3. a\n4. b\n");
        assert!(
            ol.contains("<ol start=\"3\" data-line=\"0\">\n<li><span class=\"list-number\">3.</span>a</li>"),
            "{ol}"
        );
        assert!(h.contains("<sup class=\"footnote-ref\"><a href=\"#fn-n\">1</a></sup>"), "{h}");
        assert!(h.contains("<div class=\"math-display\" data-line=\"2\">[D:y]</div>"), "{h}");
        assert!(
            h.contains("<section class=\"footnotes\"><hr>\n<div class=\"footnote\" id=\"fn-n\" data-line=\"6\">"),
            "{h}"
        );
    }
}
