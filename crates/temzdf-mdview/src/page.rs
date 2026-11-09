//! A page: a Markdown document laid out by Blitz at one width and zoom,
//! with what panes ask of it: where source lines and anchors are, the
//! link at a point, search hits, and its strips' drawing recorded for
//! worker threads to paint ([`StripJob`]).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use blitz_dom::{BaseDocument, DocumentConfig, local_name};
use blitz_html::HtmlDocument;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};
use blitz_traits::shell::{ColorScheme, Viewport};
use temzdf_scene::tiles::Job;

/// Strips are this many device pixels high (the full page width).
pub const STRIP: u32 = 256;

/// The page's background (`markdown.css`), under strips not painted yet.
pub const BACKGROUND: [u8; 4] = [255, 255, 255, 255];

/// Tells pages apart, for ever (a page laid out again is a new one).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PageId(u64);

/// A strip of a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StripKey {
    pub page: PageId,
    pub strip: u32,
}

/// A strip's drawing, to paint on a worker thread.
pub struct StripJob {
    pub key: StripKey,
    /// Whose it is, for the caller (passed on to [`Strip`]).
    pub tag: u32,
    scene: anyrender::Scene,
    width: u32,
    rows: u32,
}

/// A painted strip.
pub struct Strip {
    pub key: StripKey,
    pub tag: u32,
    pub width: u32,
    pub rows: u32,
    pub pixels: Vec<u8>,
}

impl Job for StripJob {
    type Key = (u32, StripKey);
    type Output = Strip;

    fn key(&self) -> (u32, StripKey) {
        (self.tag, self.key)
    }

    fn run(self) -> Strip {
        use anyrender::PaintScene;
        let (w, h) = (self.width, self.rows);
        let scene = self.scene;
        let pixels = anyrender::render_to_buffer::<anyrender_vello_cpu::VelloCpuImageRenderer, _>(
            |p| p.append_scene(scene, kurbo::Affine::IDENTITY),
            w,
            h,
        );
        Strip { key: self.key, tag: self.tag, width: w, rows: h, pixels }
    }
}

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
    pub id: PageId,
    doc: BaseDocument,
    pub width: u32,
    pub zoom: f32,
    height: u32,
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
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = PageId(NEXT.fetch_add(1, Ordering::Relaxed));
        let mut page = Page { id, doc, width, zoom, height: 0, lines: Vec::new() };
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

    /// How many strips the page has.
    pub fn strips(&self) -> u32 {
        self.height.div_ceil(STRIP)
    }

    /// Strip `strip`'s drawing recorded (cheap: its painting is the
    /// work), for `tag`'s caller; none past the last.
    pub fn record(&mut self, strip: u32, tag: u32) -> Option<StripJob> {
        let y = strip.checked_mul(STRIP).filter(|y| *y < self.height)?;
        let rows = STRIP.min(self.height - y);
        let scale = self.scale();
        self.doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: f64::from(y) / scale });
        let mut scene = anyrender::Scene::new();
        blitz_paint::paint_scene(&mut scene, &mut self.doc, scale, self.width, rows, 0, 0);
        Some(StripJob { key: StripKey { page: self.id, strip }, tag, scene, width: self.width, rows })
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
    fn strips_are_recorded_and_painted_as_the_page_draws_them() {
        let mut p = page(900, 1.0);
        assert!(p.height() > 1000, "{}", p.height());
        assert_eq!(p.strips(), p.height().div_ceil(STRIP));
        // A strip painted from its recording: the page's own pixels (its
        // background white, its heading's text dark somewhere).
        let s = p.record(0, 7).unwrap().run();
        assert_eq!((s.key, s.tag, s.width, s.rows), (StripKey { page: p.id, strip: 0 }, 7, 900, STRIP));
        assert_eq!(&s.pixels[..4], &BACKGROUND);
        assert!(s.pixels.chunks(4).any(|px| px[0] < 100), "text drawn");
        // The last is as high as what is left; none after it.
        let last = p.strips() - 1;
        assert_eq!(p.record(last, 0).unwrap().run().rows, p.height() - last * STRIP);
        assert!(p.record(last + 1, 0).is_none());
        // Another page is another, even at the same size.
        assert_ne!(page(900, 1.0).id, p.id);
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
