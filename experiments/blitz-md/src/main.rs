//! Spike: can Blitz draw temzdf's Markdown (doc/survey/markdown-viewer.md)?
//!
//! `blitz-md FILE.md OUTDIR [REPEAT]`: Markdown -> comrak HTML (math as
//! RaTeX SVG) -> Blitz, then times layout and painting, writes PNGs of the
//! first screens, and tries hit testing, element positions and zoom.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use blitz_dom::{BaseDocument, DocumentConfig};
use blitz_html::HtmlDocument;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};
use blitz_traits::shell::{ColorScheme, Viewport};
use comrak::nodes::NodeValue;
use comrak::{Arena, Options, format_html, parse_document};

const WIDTH: u32 = 1800;
const HEIGHT: u32 = 1200;

/// Resources from the file system only (what the viewer reads by default).
struct FileNet;

impl NetProvider for FileNet {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        if request.url.scheme() == "file"
            && let Ok(path) = request.url.to_file_path()
            && let Ok(bytes) = std::fs::read(&path)
        {
            handler.bytes(request.url.to_string(), Bytes::from(bytes));
        } else {
            eprintln!("not fetched: {}", request.url);
        }
    }
}

/// A formula as a self-contained SVG (glyphs as paths), sized in px at
/// `size` px per em, sitting on the baseline.
fn math_svg(tex: &str, size: f64, display: bool) -> String {
    use ratex_layout::{LayoutOptions, layout, to_display_list};
    use ratex_types::math_style::MathStyle;
    let style = if display { MathStyle::Display } else { MathStyle::Text };
    let Ok(ast) = ratex_parser::parser::parse(tex) else {
        return format!("<code>{}</code>", html_escape(tex));
    };
    let lbox = layout(&ast, &LayoutOptions::default().with_style(style));
    let list = to_display_list(&lbox);
    let opts = ratex_svg::SvgOptions {
        font_size: size,
        padding: 0.0,
        stroke_width: 1.0,
        embed_glyphs: true,
        font_dir: String::new(),
    };
    let svg = ratex_svg::render_to_svg(&list, &opts);
    let depth = list.depth * size;
    let svg = svg.replacen("<svg ", &format!("<svg style=\"vertical-align:{:.2}px\" ", -depth), 1);
    if display { format!("<div class=\"math-display\">{svg}</div>") } else { svg }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn markdown_html(md: &str) -> String {
    let mut opts = Options::default();
    opts.extension.strikethrough = true;
    opts.extension.table = true;
    opts.extension.autolink = true;
    opts.extension.tasklist = true;
    opts.extension.footnotes = true;
    opts.extension.alerts = true;
    opts.extension.math_dollars = true;
    opts.extension.math_code = true;
    opts.render.r#unsafe = true;
    let arena = Arena::new();
    let root = parse_document(&arena, md, &opts);
    let t = Instant::now();
    let mut formulas = 0;
    for node in root.descendants() {
        let mut data = node.data.borrow_mut();
        if let NodeValue::Math(m) = &data.value {
            let svg = math_svg(&m.literal, 16.0, m.display_math);
            data.value = NodeValue::HtmlInline(svg);
            formulas += 1;
        }
    }
    eprintln!("math: {formulas} formulas in {:.1?}", t.elapsed());
    let mut body = String::new();
    format_html(root, &opts, &mut body).unwrap();
    format!("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>{CSS}</style></head><body><article class=\"markdown-body\">{body}</article></body></html>")
}

/// A GitHub-like light style, written for the spike.
const CSS: &str = r#"
html { background: #ffffff; }
body { margin: 0; }
.markdown-body { font-family: sans-serif; font-size: 16px; line-height: 1.5; color: #1f2328;
  padding: 32px; max-width: 980px; margin: 0 auto; }
.markdown-body h1, .markdown-body h2 { border-bottom: 1px solid #d1d9e0; padding-bottom: .3em; }
.markdown-body h1 { font-size: 2em; } .markdown-body h2 { font-size: 1.5em; }
.markdown-body a { color: #0969da; text-decoration: none; }
.markdown-body code { font-family: monospace; font-size: 85%; background: #eff1f3;
  padding: .2em .4em; border-radius: 6px; }
.markdown-body pre { background: #f6f8fa; padding: 16px; border-radius: 6px; overflow: auto; }
.markdown-body pre code { background: none; padding: 0; font-size: 85%; }
.markdown-body table { border-collapse: collapse; }
.markdown-body th, .markdown-body td { border: 1px solid #d1d9e0; padding: 6px 13px; }
.markdown-body tr:nth-child(2n) { background: #f6f8fa; }
.markdown-body blockquote { margin: 0; padding: 0 1em; color: #59636e; border-left: .25em solid #d1d9e0; }
.markdown-body .markdown-alert { padding: .5em 1em; border-left: .25em solid #0969da; margin-bottom: 16px; }
.markdown-body .markdown-alert-title { color: #0969da; font-weight: 600; }
.markdown-body hr { height: .25em; background: #d1d9e0; border: 0; }
.markdown-body img { max-width: 100%; }
.markdown-body .math-display { text-align: center; margin: 16px 0; }
.markdown-body ul.contains-task-list { list-style: none; padding-left: 1em; }
"#;

fn document(html: &str, base: &Path, zoom: f32) -> BaseDocument {
    let config = DocumentConfig {
        viewport: Some(Viewport { color_scheme: ColorScheme::Light, window_size: (WIDTH, HEIGHT), hidpi_scale: 1.0, zoom }),
        base_url: Some(url::Url::from_directory_path(base).unwrap().to_string()),
        net_provider: Some(Arc::new(FileNet)),
        ..Default::default()
    };
    HtmlDocument::from_html(html, config).into_inner()
}

fn resolve(doc: &mut BaseDocument) {
    // Resources arrive as messages (FileNet answers at once).
    for _ in 0..10 {
        doc.handle_messages();
        doc.resolve(0.0);
        if !doc.has_pending_critical_resources() {
            break;
        }
    }
}

fn height(doc: &BaseDocument) -> f32 {
    doc.root_element().final_layout().size.height
}

/// `h` rows of pixels from document y `y`, at `scale` device px per CSS px.
fn paint(doc: &mut BaseDocument, y: u32, h: u32, scale: f64) -> Vec<u8> {
    doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: y as f64 / scale });
    anyrender::render_to_buffer::<anyrender_vello_cpu::VelloCpuImageRenderer, _>(
        |scene| blitz_paint::paint_scene(scene, doc, scale, WIDTH, h, 0, 0),
        WIDTH,
        h,
    )
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}

/// The `href` of the link at (x, y), if any (walking up from the hit node).
fn link_at(doc: &BaseDocument, x: f32, y: f32) -> Option<String> {
    let hit = doc.hit(x, y)?;
    let mut id = Some(hit.node_id);
    while let Some(n) = id.and_then(|i| doc.get_node(i)) {
        if let Some(href) = n.attr(blitz_dom::local_name!("href")) {
            return Some(href.to_owned());
        }
        id = n.parent;
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let md_path = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);
    let repeat: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    std::fs::create_dir_all(&out).unwrap();
    let md = std::fs::read_to_string(&md_path).unwrap().repeat(repeat);
    let base = md_path.parent().unwrap().canonicalize().unwrap();

    let t = Instant::now();
    let html = markdown_html(&md);
    eprintln!("markdown -> html: {:.1?} ({} bytes)", t.elapsed(), html.len());

    let t = Instant::now();
    let mut doc = document(&html, &base, 1.0);
    eprintln!("parse html: {:.1?}", t.elapsed());
    let t = Instant::now();
    resolve(&mut doc);
    let h = height(&doc);
    eprintln!("style + layout: {:.1?} (document {WIDTH}x{h} px)", t.elapsed());

    for (name, y) in [("top", 0u32), ("middle", (h as u32 / 2).saturating_sub(HEIGHT / 2)), ("end", (h as u32).saturating_sub(HEIGHT))] {
        let t = Instant::now();
        let px = paint(&mut doc, y, HEIGHT, 1.0);
        eprintln!("paint {name} {WIDTH}x{HEIGHT} at y={y}: {:.1?}", t.elapsed());
        write_png(&out.join(format!("{name}.png")), WIDTH, HEIGHT, &px);
    }
    // A strip, as tiles would be painted.
    let t = Instant::now();
    let px = paint(&mut doc, h as u32 - 256, 256, 1.0);
    write_png(&out.join("bottom-strip.png"), WIDTH, 256, &px);
    eprintln!("paint strip {WIDTH}x256: {:.1?}", t.elapsed());

    // Links and element positions.
    let anchors = doc.query_selector_all("a").unwrap();
    eprintln!("links: {}", anchors.len());
    for &a in anchors.iter().take(4) {
        let n = doc.get_node(a).unwrap();
        let p = n.absolute_position(0.0, 0.0);
        let s = n.final_layout().size;
        let (cx, cy) = (p.x + s.width / 2.0, p.y + s.height / 2.0);
        eprintln!(
            "  <a href={:?}> at ({:.0},{:.0}) {:.0}x{:.0}; hit there -> {:?}",
            n.attr(blitz_dom::local_name!("href")),
            p.x,
            p.y,
            s.width,
            s.height,
            link_at(&doc, cx, cy)
        );
    }
    doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 0.0 });
    for (x, y) in [(1040.0, 146.0), (1300.0, 146.0), (420.0, 170.0), (600.0, 146.0)] {
        let hit = doc.hit(x, y);
        eprintln!("  hit ({x},{y}) -> node {:?} link {:?}", hit.map(|h| h.node_id), link_at(&doc, x, y));
    }
    for &hd in doc.query_selector_all("h1, h2").unwrap().iter().take(4) {
        let n = doc.get_node(hd).unwrap();
        eprintln!("  heading {:?} at y={:.0}", n.text_content(), n.absolute_position(0.0, 0.0).y);
    }

    // Zoom: lay out at width / zoom CSS px, paint at zoom.
    for zoom in [1.5f64, 0.75] {
        let t = Instant::now();
        doc.zoom_to(zoom as f32);
        resolve(&mut doc);
        let hz = height(&doc);
        eprintln!("zoom {zoom} in place: {:.1?} (height {hz} CSS px)", t.elapsed());
        let t = Instant::now();
        let scale = doc.viewport().scale_f64();
        let px = paint(&mut doc, 0, HEIGHT, scale);
        eprintln!("  paint: {:.1?}", t.elapsed());
        write_png(&out.join(format!("zoom-{zoom}.png")), WIDTH, HEIGHT, &px);
    }
}
