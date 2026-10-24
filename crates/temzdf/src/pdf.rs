//! Thin layer over the `mupdf` crate: opening documents, page geometry, display
//! lists and tile rasterisation.
//!
//! Threading: `mupdf::Document` is not `Send`, so it lives on one thread (see
//! `docthread.rs`). `DisplayList` is `Send + Sync`; tiles are rasterised from
//! display lists on worker threads (`render.rs`).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use mupdf::{Colorspace, Device, DisplayList, Document, IRect, Matrix, Pixmap};

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

pub fn open(path: &Path) -> Result<Document> {
    let s = path.to_str().context("path is not valid UTF-8")?;
    Document::open(s).with_context(|| format!("cannot open {}", path.display()))
}

pub fn page_bounds(doc: &Document) -> Result<Vec<PageBounds>> {
    let n = doc.page_count()?;
    (0..n)
        .map(|i| {
            let r = doc.load_page(i)?.bounds()?;
            Ok(PageBounds { x0: r.x0, y0: r.y0, x1: r.x1, y1: r.y1 })
        })
        .collect()
}

pub fn display_list(doc: &Document, page: usize) -> Result<Arc<DisplayList>> {
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
