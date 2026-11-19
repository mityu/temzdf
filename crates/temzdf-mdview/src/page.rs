//! A page: a Markdown document laid out by Blitz at one width and zoom,
//! with what panes ask of it: where source lines and anchors are, the
//! link at a point, search hits, places in its text and what a selection
//! between two covers, and its strips' drawing recorded for
//! worker threads to paint ([`StripJob`]); also tiles of it magnified (a
//! pinch shows the page larger without laying it out again, as browsers
//! do), painted from the same layout.

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

/// Tiles of a magnified page are this many device pixels wide and high
/// (the last ones in a row or column smaller).
pub const TILE: u32 = 512;

/// The page's background (`markdown.css`), under strips not painted yet.
pub const BACKGROUND: [u8; 4] = [255, 255, 255, 255];

/// Tells pages apart, for ever (a page laid out again is a new one).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PageId(u64);

/// A strip of a page, or a tile of it magnified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StripKey {
    pub page: PageId,
    /// Its row: of strips, or of tiles.
    pub strip: u32,
    /// The magnification a tile is painted at, in hundredths; 0 for a
    /// strip (of the page as laid out).
    pub magnify: u32,
    /// A tile's column (strips are as wide as the page).
    pub col: u32,
}

impl StripKey {
    /// Strip `strip` of `page`.
    pub fn strip(page: PageId, strip: u32) -> Self {
        Self { page, strip, magnify: 0, col: 0 }
    }
}

/// A strip's drawing, to paint on a worker thread.
pub struct StripJob {
    pub key: StripKey,
    /// Whose it is, for the caller (passed on to [`Strip`]).
    pub tag: u32,
    scene: anyrender::Scene,
    /// How much larger it is painted than recorded (a tile's
    /// magnification; 1 for a strip).
    magnify: f64,
    /// Where the painting starts, magnified (a tile's left).
    left: u32,
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
            |p| {
                let at = kurbo::Affine::translate((-f64::from(self.left), 0.0)) * kurbo::Affine::scale(self.magnify);
                p.append_scene(scene, at)
            },
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
        if let Some(bytes) = fetched_for(&self.fetched, &request.url) {
            handler.bytes(url, Bytes::from(bytes.as_ref().clone()));
        } else if request.url.scheme() == "file"
            && let Ok(path) = request.url.to_file_path()
            && let Ok(bytes) = std::fs::read(path)
        {
            handler.bytes(url, Bytes::from(bytes));
        }
    }
}

/// The bytes fetched for `url`: they are kept under the URL as the
/// Markdown wrote it, the page asks with it normalised (`画像.png` as
/// `%E7%94%BB%E5%83%8F.png`, the host in lower case, ...).
fn fetched_for<'a>(fetched: &'a HashMap<String, Arc<Vec<u8>>>, url: &url::Url) -> Option<&'a Arc<Vec<u8>>> {
    fetched
        .get(url.as_str())
        .or_else(|| fetched.iter().find(|(k, _)| url::Url::parse(k).is_ok_and(|u| u == *url)).map(|(_, v)| v))
}

/// A rectangle in device pixels of the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// A place in a page's text: its inline root (blocks of text: a
/// paragraph, a heading, a cell), by reading order, and a byte offset in
/// the root's text. Carets order as the text reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Caret {
    pub root: usize,
    pub offset: usize,
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
    /// The inline roots (nodes with a text layout), in reading order.
    roots: Vec<blitz_dom::NodeId>,
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
        let mut page = Page { id, doc, width, zoom, height: 0, lines: Vec::new(), roots: Vec::new() };
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
        // Depth first through the layout tree (anonymous blocks of text
        // are only there): reading order.
        let mut roots = Vec::new();
        let mut stack = vec![self.doc.root_element().id];
        while let Some(id) = stack.pop() {
            let Some(node) = self.doc.get_node(id) else { continue };
            if node.element_data().is_some_and(|el| el.inline_layout_data.is_some()) {
                roots.push(id);
            }
            let children = node.layout_children.borrow();
            let children = children.as_deref().unwrap_or(&node.children);
            stack.extend(children.iter().rev());
        }
        self.roots = roots;
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
        Some(StripJob {
            key: StripKey::strip(self.id, strip),
            tag,
            scene,
            magnify: 1.0,
            left: 0,
            width: self.width,
            rows,
        })
    }

    /// The page's size magnified by `magnify` hundredths (device pixels).
    pub fn magnified_size(&self, magnify: u32) -> (u32, u32) {
        let m = f64::from(magnify) / 100.0;
        ((f64::from(self.width) * m).ceil() as u32, (f64::from(self.height) * m).ceil() as u32)
    }

    /// The drawing of the tile in column `col` and row `row` of the page
    /// magnified by `magnify` hundredths (tiles of [`TILE`] pixels), as
    /// [`Page::record`]: the page drawn larger, laid out as it is.
    pub fn record_magnified(&mut self, magnify: u32, col: u32, row: u32, tag: u32) -> Option<StripJob> {
        let (w, h) = self.magnified_size(magnify);
        let x = col.checked_mul(TILE).filter(|x| *x < w)?;
        let y = row.checked_mul(TILE).filter(|y| *y < h)?;
        let (width, rows) = (TILE.min(w - x), TILE.min(h - y));
        // Recorded as the page is drawn, from its left edge to the tile's
        // right one (and a row more, for the tile's bottom), from where the
        // tile's top is; painted larger, from the tile's left.
        let m = f64::from(magnify) / 100.0;
        let scale = self.scale();
        self.doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: f64::from(y) / m / scale });
        let mut scene = anyrender::Scene::new();
        let (right, bottom) = ((f64::from(x + width) / m).ceil() as u32, (f64::from(rows) / m).ceil() as u32 + 1);
        blitz_paint::paint_scene(&mut scene, &mut self.doc, scale, right.min(self.width), bottom, 0, 0);
        let key = StripKey { page: self.id, strip: row, magnify, col };
        Some(StripJob { key, tag, scene, magnify: m, left: x, width, rows })
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
        for root in 0..self.roots.len() {
            let Some((_, text)) = self.root(root) else { continue };
            for (start, _) in text.text.to_ascii_lowercase().match_indices(&q) {
                out.extend(self.range_rects(root, start, start + q.len()));
            }
        }
        out.sort_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)));
        out
    }

    /// Inline root `root`'s node and text layout.
    fn root(&self, root: usize) -> Option<(&blitz_dom::Node, &blitz_dom::node::TextLayout)> {
        let node = self.doc.get_node(*self.roots.get(root)?)?;
        Some((node, node.element_data()?.inline_layout_data.as_deref()?))
    }

    /// Where a root's text layout starts (device pixels of the page).
    fn origin(&self, node: &blitz_dom::Node, text: &blitz_dom::node::TextLayout) -> (f64, f64) {
        let scale = self.scale();
        let lay = node.final_layout();
        let pos = node.absolute_position(0.0, 0.0);
        let ox = f64::from(pos.x + lay.padding.left + lay.border.left) * scale;
        let oy = (f64::from(pos.y + lay.padding.top + lay.border.top) + f64::from(text.block_offset)) * scale;
        (ox, oy)
    }

    /// The boxes of bytes `start..end` of root `root`'s text, a box a line.
    fn range_rects(&self, root: usize, start: usize, end: usize) -> Vec<Rect> {
        use parley::layout::Affinity;
        let Some((node, text)) = self.root(root) else { return Vec::new() };
        if start >= end {
            return Vec::new();
        }
        let (ox, oy) = self.origin(node, text);
        let sel = parley::Selection::new(
            parley::Cursor::from_byte_index(&text.layout, start, Affinity::Downstream),
            parley::Cursor::from_byte_index(&text.layout, end, Affinity::Upstream),
        );
        sel.geometry(&text.layout)
            .into_iter()
            .map(|(b, _)| Rect { x0: ox + b.x0, y0: oy + b.y0, x1: ox + b.x1, y1: oy + b.y1 })
            .collect()
    }

    /// Whether there is text at device pixel (`x`, `y`) (a press there
    /// starts a selection).
    pub fn has_text_at(&self, x: f64, y: f64) -> bool {
        let scale = self.scale();
        self.doc.find_text_position((x / scale) as f32, (y / scale) as f32).is_some()
    }

    /// The place in the text nearest device pixel (`x`, `y`): in the text
    /// there, else in the root nearest above or below (a drag past the
    /// text, over a margin, keeps going). None without text.
    pub fn caret_at(&self, x: f64, y: f64) -> Option<Caret> {
        let scale = self.scale();
        if let Some((id, offset)) = self.doc.find_text_position((x / scale) as f32, (y / scale) as f32)
            && let Some(root) = self.roots.iter().position(|r| *r == id)
        {
            return Some(Caret { root, offset });
        }
        let mut best: Option<(f64, usize, f64)> = None;
        for root in 0..self.roots.len() {
            let Some((node, text)) = self.root(root) else { continue };
            let (_, oy) = self.origin(node, text);
            let h = f64::from(text.layout.height());
            let d = if y < oy {
                oy - y
            } else if y > oy + h {
                y - (oy + h)
            } else {
                0.0
            };
            if best.is_none_or(|(b, _, _)| d < b) {
                best = Some((d, root, (y - oy).clamp(0.0, (h - 0.5).max(0.0))));
            }
        }
        let (_, root, dy) = best?;
        let (node, text) = self.root(root)?;
        let (ox, _) = self.origin(node, text);
        // In the root's content box, CSS pixels, its text's offset included.
        let lx = ((x - ox) / scale).max(0.0) as f32;
        let ly = (dy / scale) as f32 + text.block_offset;
        let offset = node.text_offset_at_point(lx, ly).unwrap_or(if dy <= 0.0 { 0 } else { text.text.len() });
        Some(Caret { root, offset })
    }

    /// The text between carets `a` and `b` (either first): a line a block.
    pub fn selection_text(&self, a: Caret, b: Caret) -> String {
        let mut out = Vec::new();
        for (root, start, end) in self.spans(a, b) {
            if let Some((_, text)) = self.root(root) {
                out.push(text.text.get(start..end).unwrap_or_default());
            }
        }
        out.join("\n")
    }

    /// The boxes the text between carets `a` and `b` covers.
    pub fn selection_rects(&self, a: Caret, b: Caret) -> Vec<Rect> {
        self.spans(a, b).flat_map(|(root, start, end)| self.range_rects(root, start, end)).collect()
    }

    /// Byte ranges of each root between carets `a` and `b`.
    fn spans(&self, a: Caret, b: Caret) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        let (from, to) = if a <= b { (a, b) } else { (b, a) };
        (from.root..=to.root.min(self.roots.len().saturating_sub(1))).filter_map(move |root| {
            let len = self.root(root)?.1.text.len();
            let start = if root == from.root { from.offset.min(len) } else { 0 };
            let end = if root == to.root { to.offset.min(len) } else { len };
            (start < end).then_some((root, start, end))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temzdf_markdown::{Document, html};

    #[test]
    fn fetched_images_are_found_by_the_url_the_page_asks_with() {
        let bytes = Arc::new(vec![1u8]);
        let fetched = HashMap::from([
            ("https://Example.com:443/画像.png".to_owned(), bytes.clone()),
            ("https://example.com/a b.png".to_owned(), bytes.clone()),
        ]);
        let asked = |u: &str| fetched_for(&fetched, &url::Url::parse(u).unwrap()).is_some();
        assert!(asked("https://example.com/%E7%94%BB%E5%83%8F.png"));
        assert!(asked("https://example.com/a%20b.png"));
        assert!(!asked("https://example.com/other.png"));
    }

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
        assert_eq!((s.key, s.tag, s.width, s.rows), (StripKey::strip(p.id, 0), 7, 900, STRIP));
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
    fn magnified_tiles_draw_the_page_larger_as_it_is_laid_out() {
        let mut p = page(900, 1.0);
        let strip = p.record(1, 0).unwrap().run();
        let px = |pixels: &[u8], w: u32, x: u32, y: u32| -> [u8; 4] {
            let i = ((y * w + x) * 4) as usize;
            pixels[i..i + 4].try_into().unwrap()
        };
        // Unmagnified, tiles are the strips' pixels cut up (anti-aliasing
        // a step apart here and there): the second column (painted from
        // its left) and the second row of tiles (the page scrolled to it)
        // included.
        assert_eq!(p.magnified_size(100), (900, p.height()));
        let t = p.record_magnified(100, 1, 0, 3).unwrap().run();
        assert_eq!(
            (t.key, t.tag, t.width, t.rows),
            (StripKey { page: p.id, strip: 0, magnify: 100, col: 1 }, 3, 900 - TILE, TILE)
        );
        for y in 0..STRIP {
            for x in 0..t.width {
                let (a, b) = (px(&t.pixels, t.width, x, STRIP + y), px(&strip.pixels, 900, TILE + x, y));
                assert!(a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 2), "at {x},{y}: {a:?} {b:?}");
            }
        }
        // Twice as large: the same drawing.
        assert_eq!(p.magnified_size(200), (1800, p.height() * 2));
        let t = p.record_magnified(200, 1, 1, 0).unwrap().run();
        assert_eq!((t.width, t.rows), (TILE, TILE));
        // Two by two: of the dark pixels (text), few have only light
        // pixels around theirs in the strip (edges are painted, not
        // copied: anti-aliasing moves them a pixel, more so with thick
        // strokes, as DejaVu's were when CI used the runner's fonts).
        let (mut ink, mut off) = (0u32, 0u32);
        for y in 0..TILE {
            for x in 0..TILE {
                if px(&t.pixels, TILE, x, y)[0] >= 100 {
                    continue;
                }
                ink += 1;
                let (sx, sy) = ((TILE + x) / 2, (TILE + y) / 2 - STRIP);
                let near = (sx.saturating_sub(1)..=(sx + 1).min(899))
                    .flat_map(|x| (sy.saturating_sub(1)..=(sy + 1).min(STRIP - 1)).map(move |y| (x, y)));
                off += u32::from(near.into_iter().all(|(x, y)| px(&strip.pixels, 900, x, y)[0] > 200));
            }
        }
        assert!(ink > 100, "text drawn: {ink}");
        assert!(off * 10 < ink, "{off} of {ink} dark pixels light around in the strip");
        // None past the page.
        let (w, h) = p.magnified_size(200);
        assert!(p.record_magnified(200, w.div_ceil(TILE), 0, 0).is_none());
        assert!(p.record_magnified(200, 0, h.div_ceil(TILE), 0).is_none());
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

    /// The middle of the first box of `word` on `p`.
    fn middle(p: &Page, word: &str) -> (f64, f64) {
        let r = p.find(word)[0];
        ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0)
    }

    #[test]
    #[cfg_attr(miri, ignore = "Blitz under Miri is far too slow")]
    fn a_selection_covers_the_text_between_two_places_across_blocks() {
        let p = page(900, 1.0);
        // From the start of "paragraph" to the end of "Lists" (the next
        // heading, two blocks on): the blocks' text a line each.
        let r = p.find("paragraph")[0];
        let a = p.caret_at(r.x0 + 0.5, (r.y0 + r.y1) / 2.0).unwrap();
        let r = p.find("Lists")[0];
        let b = p.caret_at(r.x1 - 0.5, (r.y0 + r.y1) / 2.0).unwrap();
        let text = p.selection_text(a, b);
        assert!(text.starts_with("paragraph with emphasis"), "{text:?}");
        assert!(text.ends_with("\nLists"), "{text:?}");
        assert!(text.contains("docs.\n日本語") || text.contains("docs. 日本語"), "{text:?}");
        assert_eq!(text.matches('\n').count(), 1, "a line a block: {text:?}");
        // Either way round, the same text; boxes on each line it covers.
        assert_eq!(p.selection_text(b, a), text);
        let boxes = p.selection_rects(a, b);
        assert!(boxes.len() >= 3, "{boxes:?}");
        assert!(boxes.iter().any(|r| r.y0 < p.find("Lists")[0].y1 && r.y1 > p.find("Lists")[0].y0));
        // Nothing between a place and itself.
        assert_eq!(p.selection_text(a, a), "");
        assert!(p.selection_rects(a, a).is_empty());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Blitz under Miri is far too slow")]
    fn places_in_the_text_snap_from_margins_and_match_the_search_when_zoomed() {
        for zoom in [1.0, 1.5] {
            let p = page(900, zoom);
            // A word selected by its own box covers what the search marks.
            let r = p.find("Tables")[0];
            let y = (r.y0 + r.y1) / 2.0;
            let (a, b) = (p.caret_at(r.x0 + 0.5, y).unwrap(), p.caret_at(r.x1 - 0.5, y).unwrap());
            assert_eq!(p.selection_text(a, b), "Tables", "zoom {zoom}");
            assert_eq!(p.selection_rects(a, b), vec![r], "zoom {zoom}");
            assert!(p.has_text_at(middle(&p, "Tables").0, y));
        }
        let p = page(900, 1.0);
        // Left of the text (the page's margin): no text there to start a
        // selection, but a drag over it snaps to the line's start.
        let (_, y) = middle(&p, "Lists");
        assert!(!p.has_text_at(1.0, y));
        let c = p.caret_at(1.0, y).unwrap();
        assert_eq!(c.offset, 0);
        // Past the bottom of the page: the end of the last block.
        let end = p.caret_at(10.0, f64::from(p.height()) + 100.0).unwrap();
        assert_eq!(end.root, p.roots.len() - 1);
    }
}
