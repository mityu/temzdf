//! Minimal CPU raster helpers on straight-alpha RGBA8 buffers: blitting with
//! cropping, nearest-neighbour scaling and source-over blending. Used by the
//! `Composited` backend and by the mock terminal.

use std::io;
use std::path::Path;

use crate::geom::Rect;

/// An owned RGBA8 image with straight (non-premultiplied) alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl RgbaImage {
    pub fn new(width: u32, height: u32, fill: [u8; 4]) -> Self {
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width as usize * height as usize {
            data.extend_from_slice(&fill);
        }
        Self { width, height, data }
    }

    pub fn from_raw(width: u32, height: u32, data: Vec<u8>) -> Option<Self> {
        (data.len() == width as usize * height as usize * 4).then_some(Self { width, height, data })
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    pub fn fill(&mut self, color: [u8; 4]) {
        for px in self.data.as_chunks_mut::<4>().0 {
            px.copy_from_slice(&color);
        }
    }

    pub fn save_png(&self, path: &Path) -> io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut enc = png::Encoder::new(io::BufWriter::new(file), self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(io::Error::other)?;
        writer.write_image_data(&self.data).map_err(io::Error::other)?;
        Ok(())
    }
}

/// A borrowed RGBA8 source image.
#[derive(Clone, Copy, Debug)]
pub struct RgbaRef<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

/// How translucent pixels are combined. Terminals differ: kitty blends images
/// in linear light, so a translucent overlay looks lighter there than the same
/// blend done on sRGB values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Blending {
    /// Source-over on the sRGB-encoded values.
    #[default]
    Srgb,
    /// Source-over on linear-light values (decoded from sRGB, re-encoded after).
    Linear,
}

/// Draw `src_rect` of `src` into `dst` so that it covers `dst_rect` (scaling with
/// nearest neighbour when sizes differ), blending source-over. Parts outside
/// either image are clipped.
pub fn blit(dst: &mut RgbaImage, dst_rect: Rect, src: RgbaRef<'_>, src_rect: Rect) {
    blit_blended(dst, dst_rect, src, src_rect, Blending::Srgb);
}

/// [`blit`] with a choice of [`Blending`].
pub fn blit_blended(dst: &mut RgbaImage, dst_rect: Rect, src: RgbaRef<'_>, src_rect: Rect, blending: Blending) {
    if dst_rect.is_empty() || src_rect.is_empty() {
        return;
    }
    let Some(clip) = dst_rect.intersect(&Rect::new(0, 0, dst.width, dst.height)) else { return };
    let sx = src_rect.w as f64 / dst_rect.w as f64;
    let sy = src_rect.h as f64 / dst_rect.h as f64;
    for y in clip.y..clip.bottom() as i32 {
        let v = src_rect.y as f64 + ((y - dst_rect.y) as f64 + 0.5) * sy;
        let v = v.floor() as i64;
        if v < 0 || v >= src.height as i64 {
            continue;
        }
        let drow = y as usize * dst.width as usize * 4;
        let srow = v as usize * src.width as usize * 4;
        for x in clip.x..clip.right() as i32 {
            let u = src_rect.x as f64 + ((x - dst_rect.x) as f64 + 0.5) * sx;
            let u = u.floor() as i64;
            if u < 0 || u >= src.width as i64 {
                continue;
            }
            let si = srow + u as usize * 4;
            let di = drow + x as usize * 4;
            match blending {
                Blending::Srgb => blend(&mut dst.data[di..di + 4], &src.data[si..si + 4]),
                Blending::Linear => blend_linear(&mut dst.data[di..di + 4], &src.data[si..si + 4]),
            }
        }
    }
}

/// Source-over blending of straight-alpha pixels.
#[inline]
pub fn blend(dst: &mut [u8], src: &[u8]) {
    let sa = src[3] as u32;
    if sa == 255 {
        dst.copy_from_slice(src);
        return;
    }
    if sa == 0 {
        return;
    }
    let da = dst[3] as u32;
    // out_a = sa + da * (1 - sa), all in 0..=255 fixed point.
    let out_a = sa * 255 + da * (255 - sa);
    if out_a == 0 {
        dst.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    for c in 0..3 {
        let v = (src[c] as u32 * sa * 255 + dst[c] as u32 * da * (255 - sa) + out_a / 2) / out_a;
        dst[c] = v as u8;
    }
    dst[3] = ((out_a + 127) / 255) as u8;
}

fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (v * 255.0).round() as u8
}

/// Source-over blending of straight-alpha pixels in linear light.
pub fn blend_linear(dst: &mut [u8], src: &[u8]) {
    let sa = src[3] as f32 / 255.0;
    if src[3] == 255 {
        dst.copy_from_slice(src);
        return;
    }
    if src[3] == 0 {
        return;
    }
    let da = dst[3] as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    for c in 0..3 {
        let v = (srgb_to_linear(src[c]) * sa + srgb_to_linear(dst[c]) * da * (1.0 - sa)) / out_a;
        dst[c] = linear_to_srgb(v);
    }
    dst[3] = (out_a * 255.0).round() as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_blending_matches_kitty() {
        // The current search hit colour over white, as kitty 0.49 shows it.
        let mut px = [255, 255, 255, 255];
        blend_linear(&mut px, &[255, 120, 0, 150]);
        assert_eq!(px[..3], [255, 191, 172]);
        let mut px = [255, 255, 255, 255];
        blend(&mut px, &[255, 120, 0, 150]);
        assert_eq!(px[..3], [255, 176, 105]);
    }

    #[test]
    fn blit_crops_and_clips() {
        let mut dst = RgbaImage::new(4, 4, [0, 0, 0, 255]);
        let src = RgbaImage::new(2, 2, [255, 0, 0, 255]);
        let r = RgbaRef { width: 2, height: 2, data: &src.data };
        blit(&mut dst, Rect::new(3, 3, 2, 2), r, Rect::new(0, 0, 2, 2));
        assert_eq!(dst.pixel(3, 3), [255, 0, 0, 255]);
        assert_eq!(dst.pixel(2, 2), [0, 0, 0, 255]);
    }

    #[test]
    fn blit_scales_nearest() {
        let mut dst = RgbaImage::new(4, 2, [0, 0, 0, 0]);
        // 2x1 source: red, blue -> scaled to 4x2
        let src = [255, 0, 0, 255, 0, 0, 255, 255];
        blit(&mut dst, Rect::new(0, 0, 4, 2), RgbaRef { width: 2, height: 1, data: &src }, Rect::new(0, 0, 2, 1));
        assert_eq!(dst.pixel(1, 1), [255, 0, 0, 255]);
        assert_eq!(dst.pixel(2, 0), [0, 0, 255, 255]);
    }

    #[test]
    fn blend_half_alpha_over_opaque() {
        let mut d = [0, 0, 0, 255];
        blend(&mut d, &[255, 255, 255, 128]);
        assert_eq!(d, [128, 128, 128, 255]);
    }
}
