//! A page: a Markdown document laid out by Blitz at one width and zoom,
//! painted in strips the panes on it share, with what panes ask of it:
//! where source lines and anchors are, the link at a point, search hits.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use blitz_dom::{BaseDocument, DocumentConfig, local_name};
use blitz_html::HtmlDocument;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};
use blitz_traits::shell::{ColorScheme, Viewport};
use temzdf_scene::{ImageKey, Images};

/// Strips are this many device pixels high (the full page width).
pub const STRIP: u32 = 256;

/// Painted strips kept per page.
const KEEP: usize = 32;

/// Images fetched before (remote ones, by URL): the page reads only these
/// and local files, never the network.
pub type Fetched = Arc<HashMap<String, Arc<Vec<u8>>>>;

/// Resources from files (relative to the document) and fetched images.
struct Net {
    fetched: Fetched,
}

impl NetProvider for Net {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url.to_string();
        if let Some(bytes) = self.fetched.get(&url) {
            handler.bytes(url, Bytes::from(bytes.as_ref().clone()));
        } else if request.url.scheme() == "file"
            && let Ok(path) = request.url.to_file_path()
            && let Ok(bytes) = std::fs::read(path)
        {
            handler.bytes(url, Bytes::from(bytes));
        }
    }
}

/// A rectangle in device pixels of the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

pub struct Page {
    doc: BaseDocument,
    pub width: u32,
    pub zoom: f32,
    height: u32,
    strips: HashMap<u32, ImageKey>,
    used: VecDeque<u32>,
    /// Blocks' first source lines and where they start (device pixels),
    /// by position.
    lines: Vec<(f64, usize)>,
}

/// The page's style: GitHub-like, light.
const CSS: &str = include_str!("markdown.css");

impl Page {
    /// Lay out `body` (from `temzdf_markdown::html::body`) for a pane
    /// `width` x `height` device pixels at `zoom`; relative URLs from `dir`.
    pub fn new(body: &str, dir: &Path, width: u32, height: u32, zoom: f32, fetched: Fetched) -> Page {
        let html = format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>{CSS}</style></head>\
             <body><article class=\"markdown-body\">{body}</article></body></html>"
        );
        let base = url::Url::from_directory_path(dir).map(|u| u.to_string()).ok();
        let config = DocumentConfig {
            viewport: Some(Viewport {
                color_scheme: ColorScheme::Light,
                window_size: (width.max(1), height.max(1)),
                hidpi_scale: 1.0,
                zoom,
            }),
            base_url: base,
            net_provider: Some(Arc::new(Net { fetched })),
            ..Default::default()
        };
        let doc = HtmlDocument::from_html(&html, config).into_inner();
        let mut page =
            Page { doc, width, zoom, height: 0, strips: HashMap::new(), used: VecDeque::new(), lines: Vec::new() };
        page.resolve();
        page
    }

    fn scale(&self) -> f64 {
        self.doc.viewport().scale_f64()
    }

    /// Lay out (again), resources read meanwhile included.
    fn resolve(&mut self) {
        for _ in 0..10 {
            self.doc.handle_messages();
            self.doc.resolve(0.0);
            if !self.doc.has_pending_critical_resources() {
                break;
            }
        }
        let scale = self.scale();
        self.height = (f64::from(self.doc.root_element().final_layout().size.height) * scale).ceil() as u32;
        let mut lines: Vec<(f64, usize)> = self
            .doc
            .query_selector_all("[data-line]")
            .unwrap_or_default()
            .iter()
            .filter_map(|&id| {
                let n = self.doc.get_node(id)?;
                let line = n.attr(blitz_dom::LocalName::from("data-line"))?.parse().ok()?;
                Some((f64::from(n.absolute_position(0.0, 0.0).y) * scale, line))
            })
            .collect();
        lines.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        self.lines = lines;
    }

    /// The page's height in device pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The strips covering rows `y..y + h` (device pixels), painted if not
    /// yet: each strip's top and its image.
    pub fn strips(&mut self, y: u32, h: u32, images: &mut Images) -> Vec<(u32, ImageKey)> {
        let mut out = Vec::new();
        if self.height == 0 {
            return out;
        }
        let last = (self.height - 1) / STRIP;
        let (first, end) = (y / STRIP, ((y + h).saturating_sub(1) / STRIP).min(last));
        for i in first..=end {
            let key = match self.strips.get(&i) {
                Some(k) => *k,
                None => {
                    let rows = STRIP.min(self.height - i * STRIP);
                    let px = self.paint(i * STRIP, rows);
                    let key = images.add(self.width, rows, px);
                    self.strips.insert(i, key);
                    key
                }
            };
            self.used.retain(|u| *u != i);
            self.used.push_back(i);
            out.push((i * STRIP, key));
        }
        // The least used go, not the ones shown now.
        while self.used.len() > KEEP.max(out.len()) {
            if let Some(old) = self.used.pop_front()
                && let Some(k) = self.strips.remove(&old)
            {
                images.remove(k);
            }
        }
        out
    }

    fn paint(&mut self, y: u32, h: u32) -> Vec<u8> {
        let scale = self.scale();
        self.doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: f64::from(y) / scale });
        let (w, doc) = (self.width, &mut self.doc);
        anyrender::render_to_buffer::<anyrender_vello_cpu::VelloCpuImageRenderer, _>(
            |scene| blitz_paint::paint_scene(scene, doc, scale, w, h, 0, 0),
            w,
            h,
        )
    }

    /// The strips' images leave the store (the page goes, or is drawn
    /// again).
    pub fn release(&mut self, images: &mut Images) {
        for (_, k) in self.strips.drain() {
            images.remove(k);
        }
        self.used.clear();
    }

    /// The link (`href`, as written) at device pixel (`x`, `y`) of the page.
    pub fn link_at(&self, x: f64, y: f64) -> Option<String> {
        let scale = self.scale();
        let hit = self.doc.hit((x / scale) as f32, (y / scale) as f32)?;
        let mut id = Some(hit.node_id);
        while let Some(n) = id.and_then(|i| self.doc.get_node(i)) {
            if let Some(href) = n.attr(local_name!("href")) {
                return Some(href.to_owned());
            }
            id = n.parent;
        }
        None
    }

    /// The top of the element `anchor` names (a heading's, a footnote's).
    pub fn anchor_y(&self, anchor: &str) -> Option<f64> {
        let id = self.doc.get_element_by_id(anchor)?;
        Some(f64::from(self.doc.get_node(id)?.absolute_position(0.0, 0.0).y) * self.scale())
    }

    /// The source line of the block at device row `y` (the last that
    /// starts there or above).
    pub fn line_at(&self, y: f64) -> usize {
        let i = self.lines.partition_point(|(top, _)| *top <= y + 0.5);
        i.checked_sub(1).map_or_else(|| self.lines.first().map_or(0, |l| l.1), |i| self.lines[i].1)
    }

    /// Where the block of source line `line` starts (the first block from
    /// it on).
    pub fn y_of_line(&self, line: usize) -> f64 {
        let mut best: Option<(f64, usize)> = None;
        for &(y, l) in &self.lines {
            if l >= line && best.is_none_or(|(_, b)| l < b) {
                best = Some((y, l));
            }
        }
        best.map_or(f64::from(self.height), |(y, _)| y)
    }

    /// Where `query` is in the text (case not counted for ASCII), in
    /// reading order.
    pub fn find(&self, query: &str) -> Vec<Rect> {
        let mut out = Vec::new();
        if query.is_empty() {
            return out;
        }
        let q = query.to_ascii_lowercase();
        let scale = self.scale();
        for (_, node) in self.doc.tree().iter() {
            let Some(el) = node.element_data() else { continue };
            let Some(text) = el.inline_layout_data.as_ref() else { continue };
            let hay = text.text.to_ascii_lowercase();
            let lay = node.final_layout();
            let pos = node.absolute_position(0.0, 0.0);
            let ox = f64::from(pos.x + lay.padding.left + lay.border.left) * scale;
            let oy = (f64::from(pos.y + lay.padding.top + lay.border.top) + f64::from(text.block_offset)) * scale;
            for (start, _) in hay.match_indices(&q) {
                use parley::layout::Affinity;
                let sel = parley::Selection::new(
                    parley::Cursor::from_byte_index(&text.layout, start, Affinity::Downstream),
                    parley::Cursor::from_byte_index(&text.layout, start + q.len(), Affinity::Upstream),
                );
                for (b, _) in sel.geometry(&text.layout) {
                    out.push(Rect { x0: ox + b.x0, y0: oy + b.y0, x1: ox + b.x1, y1: oy + b.y1 });
                }
            }
        }
        out.sort_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temzdf_markdown::{Document, html};

    fn fixture() -> (Document, std::path::PathBuf) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").canonicalize().unwrap();
        let src = std::fs::read_to_string(dir.join("sample.md")).unwrap();
        (Document::parse(&src), dir)
    }

    fn page(width: u32, zoom: f32) -> Page {
        let (doc, dir) = fixture();
        let body = html::body(&doc, &crate::math::svg);
        Page::new(&body, &dir, width, 600, zoom, Fetched::default())
    }

    #[test]
    #[cfg_attr(miri, ignore = "Blitz under Miri is far too slow")]
    fn strips_are_painted_once_and_shared() {
        let mut p = page(900, 1.0);
        assert!(p.height() > 1000, "{}", p.height());
        let mut images = Images::new();
        let a = p.strips(0, 600, &mut images);
        assert_eq!(a.iter().map(|s| s.0).collect::<Vec<_>>(), [0, 256, 512]);
        let b = p.strips(300, 600, &mut images);
        assert_eq!(b[0], a[1], "the same strip, not painted again");
        // The last strip is as high as what is left.
        let last = p.strips(p.height() - 1, 1, &mut images);
        let (w, h) = images.get(last[0].1).map(|i| (i.width, i.height)).unwrap();
        assert_eq!((w, h), (900, p.height() - last[0].0));
        p.release(&mut images);
        assert!(images.get(a[0].1).is_none());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Blitz under Miri is far too slow")]
    fn lines_anchors_links_and_search() {
        let p = page(900, 1.0);
        // Headings in order; a source line maps to its block and back.
        let lists = p.anchor_y("lists").unwrap();
        let tables = p.anchor_y("tables").unwrap();
        assert!(0.0 < lists && lists < tables);
        let (doc, _) = fixture();
        let line = doc.heading("tables").unwrap().lines.start;
        assert_eq!(p.y_of_line(line), tables);
        assert_eq!(p.line_at(tables + 1.0), line);
        // The link in the first paragraph: found where its text is.
        let hits = p.find("link to the PDF");
        assert_eq!(hits.len(), 1, "{hits:?}");
        let r = hits[0];
        let href = p.link_at((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
        assert_eq!(href.as_deref(), Some("#pdf-viewing"));
        assert_eq!(p.link_at(r.x0 - 200.0, (r.y0 + r.y1) / 2.0), None);
        // Case does not count; hits come in reading order.
        let items = p.find("ITEM");
        assert!(items.len() >= 4 && items.windows(2).all(|w| w[0].y0 <= w[1].y0), "{items:?}");
        assert!(p.find("no such words").is_empty());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Blitz under Miri is far too slow")]
    fn zoom_makes_the_page_taller() {
        let (a, b) = (page(900, 1.0), page(900, 2.0));
        assert!(b.height() > a.height() * 3 / 2, "{} {}", a.height(), b.height());
        assert!(b.anchor_y("tables").unwrap() > a.anchor_y("tables").unwrap() * 1.5);
    }
}
