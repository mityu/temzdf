//! Images programs show with the kitty graphics protocol. libghostty keeps the
//! images and their placements (as kitty does); each frame turns the visible
//! placements into scene layers, uploading the images they show into the
//! session's image store. Placements through Unicode placeholders (tmux inside
//! a pane) are not shown yet.

use std::collections::HashMap;
use std::io::Read;

use libghostty_vt::Terminal;
use libghostty_vt::alloc::{Allocator, Bytes};
use libghostty_vt::kitty::graphics::{
    Compression, DecodePng, DecodedImage, Image, ImageFormat, PlacementIterator, set_png_decoder,
};
use temzdf_scene::{ImageKey, Images, Layer, Rect};
use termpix_core::geom::CellSize;

/// How much image data a terminal keeps (kitty's default).
pub(crate) const STORAGE_LIMIT: u64 = 320 << 20;

pub(crate) struct Graphics {
    placements: PlacementIterator<'static>,
    /// Images uploaded to the store, by libghostty image id: the image's
    /// generation (it changes when the program replaces the image) and key.
    uploaded: HashMap<u32, (u64, ImageKey)>,
}

impl Graphics {
    pub(crate) fn new() -> Result<Self, libghostty_vt::Error> {
        install_png_decoder();
        Ok(Self { placements: PlacementIterator::new()?, uploaded: HashMap::new() })
    }

    /// The visible placements as layers in terminal pixels. Images no longer
    /// shown leave the store.
    pub(crate) fn layers(&mut self, terminal: &Terminal<'_, '_>, cell: CellSize, images: &mut Images) -> Vec<Layer> {
        let mut layers = Vec::new();
        let mut shown: HashMap<u32, (u64, ImageKey)> = HashMap::new();
        let mut keys: HashMap<u32, u32> = HashMap::new();
        if let Ok(graphics) = terminal.kitty_graphics()
            && let Ok(mut it) = self.placements.update(&graphics)
        {
            while let Some(p) = it.next() {
                let Ok(id) = p.image_id() else { continue };
                let Some(image) = graphics.image(id) else { continue };
                let Ok(info) = p.placement_render_info(&image, terminal) else { continue };
                if !info.viewport_visible || info.pixel_width == 0 || info.pixel_height == 0 {
                    continue;
                }
                let generation = image.generation().unwrap_or(0);
                let key = match shown.get(&id).or_else(|| self.uploaded.get(&id)) {
                    Some(&(g, key)) if g == generation => key,
                    old => {
                        let Some((w, h, pixels)) = rgba(&image) else { continue };
                        match old {
                            Some(&(_, key)) => {
                                images.replace(key, w, h, pixels);
                                key
                            }
                            None => images.add(w, h, pixels),
                        }
                    }
                };
                shown.insert(id, (generation, key));
                let x = info.viewport_col * cell.width as i32 + p.x_offset().unwrap_or(0) as i32;
                let y = info.viewport_row * cell.height as i32 + p.y_offset().unwrap_or(0) as i32;
                // Unique per placement: placements without an id count up.
                let base = id.wrapping_mul(0x9e37_79b1) ^ p.placement_id().unwrap_or(0);
                let n = keys.entry(base).or_default();
                let layer_key = base.wrapping_add(n.wrapping_mul(0x85eb_ca6b));
                *n += 1;
                layers.push(Layer {
                    key: layer_key,
                    image: key,
                    src: Rect::new(info.source_x as i32, info.source_y as i32, info.source_width, info.source_height),
                    dst: Rect::new(x, y, info.pixel_width, info.pixel_height),
                    z: p.z().unwrap_or(0),
                });
            }
        }
        for (id, (_, key)) in self.uploaded.drain() {
            if !shown.contains_key(&id) {
                images.remove(key);
            }
        }
        self.uploaded = shown;
        layers
    }

    /// Nothing shows the terminal: its images leave the store.
    pub(crate) fn release(&mut self, images: &mut Images) {
        for (_, (_, key)) in self.uploaded.drain() {
            images.remove(key);
        }
    }
}

/// An image's pixels as RGBA.
fn rgba(image: &Image<'_>) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h) = (image.width().ok()?, image.height().ok()?);
    let data = image.data().ok()?;
    let raw: Vec<u8> = match image.compression().ok()? {
        Compression::None => data.to_vec(),
        Compression::ZlibDeflate => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).ok()?;
            out
        }
        _ => return None,
    };
    let n = w as usize * h as usize;
    let channels = match image.format().ok()? {
        ImageFormat::Rgba => 4,
        ImageFormat::Rgb => 3,
        ImageFormat::GrayAlpha => 2,
        ImageFormat::Gray => 1,
        // PNG is decoded when it arrives; anything else is not ours to show.
        _ => return None,
    };
    let pixels = to_rgba(channels, &raw);
    (pixels.len() == n * 4).then_some((w, h, pixels))
}

/// libghostty decodes PNG images through a callback, per thread.
fn install_png_decoder() {
    thread_local! {
        static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !INSTALLED.replace(true) {
        let _ = set_png_decoder(Some(Box::new(PngDecoder)));
    }
}

struct PngDecoder;

impl DecodePng for PngDecoder {
    fn decode_png<'alloc>(&mut self, alloc: &'alloc Allocator<'_>, data: &[u8]) -> Option<DecodedImage<'alloc>> {
        let (width, height, pixels) = decode_png(data)?;
        let mut bytes = Bytes::new_with_alloc(alloc, pixels.len()).ok()?;
        bytes.copy_from_slice(&pixels);
        Some(DecodedImage { width, height, data: bytes })
    }
}

/// A PNG as 8-bit RGBA.
fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let buf = &buf[..info.buffer_size()];
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => return None,
    };
    Some((info.width, info.height, to_rgba(channels, buf)))
}

/// Pixels of 1 (grey), 2 (grey, alpha), 3 (RGB) or 4 (RGBA) bytes as RGBA.
fn to_rgba(channels: usize, raw: &[u8]) -> Vec<u8> {
    match channels {
        4 => raw.to_vec(),
        3 => raw.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
        2 => raw.as_chunks::<2>().0.iter().flat_map(|&[v, a]| [v, v, v, a]).collect(),
        _ => raw.iter().flat_map(|&v| [v, v, v, 255]).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pngs_decode_to_rgba() {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 1);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&[1, 2, 3, 4, 5, 6]).unwrap();
        }
        assert_eq!(decode_png(&out), Some((2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255])));
        assert_eq!(decode_png(b"not a png"), None);
    }
}
