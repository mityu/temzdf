//! Thin layer over the `mupdf` crate: opening documents, page geometry, display
//! lists and tile rasterisation.
//!
//! Threading: `mupdf::Document` is not `Send`, so it lives on one thread (see
//! `docthread.rs`). `DisplayList` is `Send + Sync`; tiles are rasterised from
//! display lists on worker threads (`render.rs`).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use mupdf::pdf::PdfDocument;
use mupdf::{Colorspace, DestinationKind, Device, DisplayList, IRect, Matrix, Pixmap};

/// Page bounds in PDF points (MuPDF page space, y down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBounds {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PageBounds {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    /// Pixel size of the whole page at `scale`.
    pub fn pixel_size(&self, scale: f32) -> (u32, u32) {
        ((self.width() * scale).ceil().max(1.0) as u32, (self.height() * scale).ceil().max(1.0) as u32)
    }
}

/// The document type we work with. `PdfDocument` derefs to `mupdf::Document`
/// and adds PDF-only features (page labels).
pub type Doc = PdfDocument;

pub fn open(path: &Path) -> Result<Doc> {
    let s = path.to_str().context("path is not valid UTF-8")?;
    PdfDocument::open(s).with_context(|| format!("cannot open {}", path.display()))
}

/// Whether a PDF file looks completely written: `%%EOF` appears within its last
/// 1024 bytes (the PDF spec allows it anywhere there). This catches most files
/// still being written by TeX, before trying to parse them. Learned from Skim.
pub fn looks_complete(path: &Path) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let tail = len.min(1024);
    f.seek(SeekFrom::Start(len - tail))?;
    let mut buf = Vec::with_capacity(tail as usize);
    f.read_to_end(&mut buf)?;
    Ok(buf.windows(5).any(|w| w == b"%%EOF"))
}

/// A point to jump to: a page and an optional y in page space (points from the
/// top of the page).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub page: usize,
    pub y: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutlineEntry {
    pub title: String,
    pub depth: usize,
    pub target: Option<Target>,
    /// Number of entries nested below this one (they follow it directly).
    pub descendants: usize,
}

/// Static information about a document, gathered once when it is opened.
#[derive(Clone, Debug, PartialEq)]
pub struct DocInfo {
    pub bounds: Vec<PageBounds>,
    /// Page labels (e.g. "iv", "12"); the 1-based number when the PDF has none.
    pub labels: Vec<String>,
    /// Outline flattened in document order.
    pub outline: Vec<OutlineEntry>,
}

pub fn doc_info(doc: &Doc) -> Result<DocInfo> {
    let bounds = page_bounds(doc)?;
    let labels = (0..bounds.len())
        .map(|i| doc.page_label(i).ok().filter(|l| !l.is_empty()).unwrap_or_else(|| (i + 1).to_string()))
        .collect();
    let mut outline = Vec::new();
    flatten_outline(&doc.outlines()?, 0, &mut outline);
    Ok(DocInfo { bounds, labels, outline })
}

fn flatten_outline(items: &[mupdf::Outline], depth: usize, out: &mut Vec<OutlineEntry>) {
    for item in items {
        let index = out.len();
        let target = item.dest.map(|d| Target { page: d.loc.page_number as usize, y: dest_top(&d.kind) });
        out.push(OutlineEntry { title: item.title.trim().to_owned(), depth, target, descendants: 0 });
        flatten_outline(&item.down, depth + 1, out);
        out[index].descendants = out.len() - index - 1;
    }
}

/// The vertical position a destination asks for, in page space, if any.
pub fn dest_top(kind: &DestinationKind) -> Option<f32> {
    match *kind {
        DestinationKind::XYZ { top, .. } => top,
        DestinationKind::FitH { top } | DestinationKind::FitBH { top } => top,
        DestinationKind::FitR { top, bottom, .. } => Some(top.min(bottom)),
        _ => None,
    }
}

pub fn page_bounds(doc: &Doc) -> Result<Vec<PageBounds>> {
    let n = doc.page_count()?;
    (0..n)
        .map(|i| {
            let r = doc.load_page(i)?.bounds()?;
            Ok(PageBounds { x0: r.x0, y0: r.y0, x1: r.x1, y1: r.y1 })
        })
        .collect()
}

/// A rectangle in page space (points from the page's top-left).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Upper bound on hits reported per page.
const MAX_HITS_PER_PAGE: u32 = 500;

/// Find `needle` on one page (MuPDF matching: case-insensitive, whitespace
/// normalised). Each hit may span several rectangles (one per line); they are
/// returned as separate rectangles, in reading order.
pub fn search_page(doc: &Doc, page: usize, bounds: PageBounds, needle: &str) -> Result<Vec<PageRect>> {
    if needle.trim().is_empty() {
        return Ok(Vec::new());
    }
    let quads = doc.load_page(page as i32)?.search(needle, MAX_HITS_PER_PAGE)?;
    Ok(quads
        .iter()
        .map(|q| {
            let xs = [q.ul.x, q.ur.x, q.ll.x, q.lr.x];
            let ys = [q.ul.y, q.ur.y, q.ll.y, q.lr.y];
            PageRect {
                x0: xs.iter().copied().fold(f32::INFINITY, f32::min) - bounds.x0,
                y0: ys.iter().copied().fold(f32::INFINITY, f32::min) - bounds.y0,
                x1: xs.iter().copied().fold(f32::NEG_INFINITY, f32::max) - bounds.x0,
                y1: ys.iter().copied().fold(f32::NEG_INFINITY, f32::max) - bounds.y0,
            }
        })
        .collect())
}

#[derive(Clone, Debug, PartialEq)]
pub enum LinkTarget {
    Internal(Target),
    External(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub rect: PageRect,
    pub target: LinkTarget,
}

/// Links on a page, with rectangles in page space.
pub fn page_links(doc: &Doc, page: usize, bounds: PageBounds) -> Result<Vec<Link>> {
    let p = doc.load_page(page as i32)?;
    // mupdf-rs unwraps while resolving destinations; never let a malformed link
    // take the document thread down.
    let links = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.links().map(|it| it.collect::<Vec<_>>())))
        .map_err(|_| anyhow::anyhow!("malformed link on page {}", page + 1))??;
    Ok(links
        .into_iter()
        .filter_map(|l| {
            let r = l.bounds;
            let rect =
                PageRect { x0: r.x0 - bounds.x0, y0: r.y0 - bounds.y0, x1: r.x1 - bounds.x0, y1: r.y1 - bounds.y0 };
            let target = match l.dest {
                Some(d) => LinkTarget::Internal(Target { page: d.loc.page_number as usize, y: dest_top(&d.kind) }),
                None if !l.uri.is_empty() && !l.uri.starts_with('#') => LinkTarget::External(l.uri),
                None => return None,
            };
            Some(Link { rect, target })
        })
        .collect())
}

pub fn display_list(doc: &Doc, page: usize) -> Result<Arc<DisplayList>> {
    let page = doc.load_page(page as i32)?;
    Ok(Arc::new(page.to_display_list(true)?))
}

/// A rectangle in page pixels at some scale (origin = page top-left).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Rasterise part of a page: `area` in page pixels at `scale`, white background,
/// returned as straight RGBA8.
pub fn render(list: &DisplayList, bounds: PageBounds, scale: f32, area: PixelRect) -> Result<Vec<u8>> {
    let rect = IRect::new(area.x as i32, area.y as i32, (area.x + area.w) as i32, (area.y + area.h) as i32);
    let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), rect, false)?;
    pixmap.clear_with(255)?;
    {
        let device = Device::from_pixmap(&pixmap)?;
        let mut ctm = Matrix::new_translate(-bounds.x0, -bounds.y0);
        ctm.concat(Matrix::new_scale(scale, scale));
        let clip = mupdf::Rect::new(rect.x0 as f32, rect.y0 as f32, rect.x1 as f32, rect.y1 as f32);
        list.run(&device, &ctm, clip)?;
    }
    let n = pixmap.n() as usize;
    let stride = pixmap.stride() as usize;
    let samples = pixmap.samples();
    let mut out = Vec::with_capacity(area.w as usize * area.h as usize * 4);
    for row in 0..area.h as usize {
        let line = &samples[row * stride..row * stride + area.w as usize * n];
        for px in line.chunks_exact(n) {
            out.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_fixture_and_reads_a4_pages() {
        let doc = open(&fixture("sample.pdf")).unwrap();
        let pages = page_bounds(&doc).unwrap();
        assert!(pages.len() >= 3, "{} pages", pages.len());
        assert!((pages[0].width() - 595.3).abs() < 1.0);
        assert!((pages[0].height() - 841.9).abs() < 1.0);
    }

    #[test]
    fn outline_and_labels() {
        let doc = open(&fixture("sample.pdf")).unwrap();
        let info = doc_info(&doc).unwrap();
        assert_eq!(info.labels, ["1", "2", "3"]);
        let titles: Vec<_> = info.outline.iter().map(|e| (e.title.as_str(), e.depth)).collect();
        assert_eq!(
            titles,
            [
                ("Introduction", 0),
                ("Mathematics", 0),
                ("Filler", 0),
                ("First part", 1),
                ("Second part", 1),
                ("Conclusion", 0)
            ]
        );
        assert_eq!(info.outline[2].descendants, 2);
        let filler = info.outline[2].target.unwrap();
        assert_eq!(filler.page, 1);
        let y = filler.y.expect("hyperref outline entries carry a position");
        assert!(y > 0.0 && y < 300.0, "Section 3 starts near the top of page 2, y={y}");
    }

    #[test]
    fn search_finds_needles_on_every_page() {
        let doc = open(&fixture("sample.pdf")).unwrap();
        let bounds = page_bounds(&doc).unwrap();
        let counts: Vec<usize> =
            (0..bounds.len()).map(|p| search_page(&doc, p, bounds[p], "needle").unwrap().len()).collect();
        assert_eq!(counts, [2, 1, 1], "per-page hits");
        let hit = search_page(&doc, 0, bounds[0], "NEEDLE").unwrap()[0];
        assert!(hit.x1 > hit.x0 && hit.y1 > hit.y0 && hit.y1 < bounds[0].height(), "{hit:?}");
        assert!(search_page(&doc, 0, bounds[0], "  ").unwrap().is_empty());
    }

    #[test]
    fn links_internal_and_external() {
        let doc = open(&fixture("sample.pdf")).unwrap();
        let bounds = page_bounds(&doc).unwrap();
        let links = page_links(&doc, 0, bounds[0]).unwrap();
        assert!(links.iter().any(|l| l.target == LinkTarget::External("https://example.com/".into())), "{links:?}");
        // The TOC and \ref links point inside the document; "Filler" is on page 2.
        assert!(links.iter().any(|l| matches!(l.target, LinkTarget::Internal(Target { page: 1, .. }))), "{links:?}");
        for l in &links {
            assert!(l.rect.x1 > l.rect.x0 && l.rect.y1 > l.rect.y0);
        }
    }

    #[test]
    fn completeness_check() {
        let dir = std::env::temp_dir().join(format!("temzdf-complete-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let full = std::fs::read(fixture("sample.pdf")).unwrap();
        let p = dir.join("a.pdf");
        std::fs::write(&p, &full).unwrap();
        assert!(looks_complete(&p).unwrap());
        std::fs::write(&p, &full[..full.len() / 2]).unwrap();
        assert!(!looks_complete(&p).unwrap(), "truncated file");
        std::fs::write(&p, b"").unwrap();
        assert!(!looks_complete(&p).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tiles_match_full_render() {
        let doc = open(&fixture("sample.pdf")).unwrap();
        let bounds = page_bounds(&doc).unwrap()[0];
        let list = display_list(&doc, 0).unwrap();
        let scale = 1.5;
        let (w, h) = bounds.pixel_size(scale);
        let full = render(&list, bounds, scale, PixelRect { x: 0, y: 0, w, h }).unwrap();
        assert!(full.as_chunks::<4>().0.iter().any(|p| p[0] < 128), "page has dark (text) pixels");
        let tile = PixelRect { x: 200, y: 300, w: 128, h: 96 };
        let part = render(&list, bounds, scale, tile).unwrap();
        for row in 0..tile.h as usize {
            let a = &part[row * tile.w as usize * 4..(row + 1) * tile.w as usize * 4];
            let start = ((tile.y as usize + row) * w as usize + tile.x as usize) * 4;
            assert_eq!(a, &full[start..start + tile.w as usize * 4], "row {row}");
        }
    }
}
