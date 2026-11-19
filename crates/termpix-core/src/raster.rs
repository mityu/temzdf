//! Minimal CPU raster helpers on straight-alpha RGBA8 buffers: blitting with
//! cropping, nearest-neighbour scaling and source-over blending. Used by the
//! `Composited` backend and by the mock terminal.

use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::geom::Rect;

/// Bytes of pixels, shared without copying: from a `Vec`, an `Arc<[u8]>`, or
/// anything else that holds bytes (memory shared with another process).
#[derive(Clone)]
pub struct Pixels(Arc<dyn AsRef<[u8]> + Send + Sync>);

impl Pixels {
    /// Pixels held by `owner` (kept alive as long as any clone).
    pub fn from_owner(owner: impl AsRef<[u8]> + Send + Sync + 'static) -> Self {
        Self(Arc::new(owner))
    }
}

impl std::ops::Deref for Pixels {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

impl AsRef<[u8]> for Pixels {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl From<Vec<u8>> for Pixels {
    fn from(v: Vec<u8>) -> Self {
        Self::from_owner(v)
    }
}

impl From<Arc<[u8]>> for Pixels {
    fn from(v: Arc<[u8]>) -> Self {
        Self::from_owner(v)
    }
}

impl PartialEq for Pixels {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl Eq for Pixels {}

impl std::fmt::Debug for Pixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pixels({} bytes)", self.len())
    }
}

/// An owned RGBA8 image with straight (non-premultiplied) alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl RgbaImage {
    pub fn new(width: u32, height: u32, fill: [u8; 4]) -> Self {
        Self { width, height, data: fill.repeat(width as usize * height as usize) }
    }

    /// Make this a `width` x `height` image filled with `fill`, keeping the
    /// allocation when it is large enough (a frame built again each present).
    pub fn reset(&mut self, width: u32, height: u32, fill: [u8; 4]) {
        let len = width as usize * height as usize * 4;
        self.width = width;
        self.height = height;
        self.data.clear();
        self.data.resize(len, 0);
        self.fill(fill);
    }

    pub fn from_raw(width: u32, height: u32, data: Vec<u8>) -> Option<Self> {
        (data.len() == width as usize * height as usize * 4).then_some(Self { width, height, data })
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    pub fn fill(&mut self, color: [u8; 4]) {
        let data = &mut self.data;
        if data.len() < 4 {
            return;
        }
        data[..4].copy_from_slice(&color);
        // Double the filled prefix with whole copies.
        let mut done = 4;
        while done < data.len() {
            let n = done.min(data.len() - done);
            data.copy_within(..n, done);
            done += n;
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
    if src_rect.w == dst_rect.w && src_rect.h == dst_rect.h {
        blit_unscaled(
            dst,
            clip,
            src,
            src_rect.x as i64 - dst_rect.x as i64,
            src_rect.y as i64 - dst_rect.y as i64,
            blending,
        );
        return;
    }
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

/// [`blit_blended`] without scaling: destination pixel (x, y) takes source
/// pixel (x + `dx`, y + `dy`), for the destination pixels in `clip` (inside
/// `dst`) whose source pixel is inside `src`. The same pixels and the same
/// blending as the scaling path, with the bounds worked out once per blit:
/// with equal sizes its `floor(src.x + (x - dst.x) + 0.5)` is exactly
/// `x + dx`.
fn blit_unscaled(dst: &mut RgbaImage, clip: Rect, src: RgbaRef<'_>, dx: i64, dy: i64, blending: Blending) {
    let x0 = (clip.x as i64).max(-dx);
    let x1 = clip.right().min(src.width as i64 - dx);
    let y0 = (clip.y as i64).max(-dy);
    let y1 = clip.bottom().min(src.height as i64 - dy);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let n = (x1 - x0) as usize * 4;
    let dst_w = dst.width as usize * 4;
    let src_w = src.width as usize * 4;
    for y in y0..y1 {
        let di = y as usize * dst_w + x0 as usize * 4;
        let si = (y + dy) as usize * src_w + (x0 + dx) as usize * 4;
        let drow = &mut dst.data[di..di + n];
        let srow = &src.data[si..si + n];
        match blending {
            Blending::Srgb => blend_row(drow, srow),
            Blending::Linear => {
                for (d, s) in drow.as_chunks_mut::<4>().0.iter_mut().zip(srow.as_chunks::<4>().0) {
                    blend_linear(d, s);
                }
            }
        }
    }
}

/// [`blend`] over a row: runs of opaque source pixels are copied whole.
fn blend_row(dst: &mut [u8], src: &[u8]) {
    let (spx, _) = src.as_chunks::<4>();
    let mut i = 0;
    while i < spx.len() {
        if spx[i][3] == 255 {
            let start = i;
            while i < spx.len() && spx[i][3] == 255 {
                i += 1;
            }
            dst[start * 4..i * 4].copy_from_slice(&src[start * 4..i * 4]);
        } else {
            blend(&mut dst[i * 4..i * 4 + 4], &spx[i]);
            i += 1;
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
    fn pixels_from_any_owner_compare_by_their_bytes() {
        let v = Pixels::from(vec![1u8, 2, 3]);
        let a = Pixels::from(Arc::<[u8]>::from(vec![1u8, 2, 3]));
        struct Owner([u8; 3]);
        impl AsRef<[u8]> for Owner {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }
        let o = Pixels::from_owner(Owner([1, 2, 3]));
        assert_eq!(&*v, &[1, 2, 3]);
        assert!(v == a && a == o);
        assert_ne!(v, Pixels::from(vec![1u8, 2]));
        let c = o.clone();
        drop(o);
        assert_eq!(c.len(), 3, "a clone keeps the owner alive");
        assert_eq!(format!("{c:?}"), "Pixels(3 bytes)");
    }

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

    /// The scaling path of [`blit_blended`], for any sizes: what the
    /// unscaled path must equal.
    fn blit_scaling(dst: &mut RgbaImage, dst_rect: Rect, src: RgbaRef<'_>, src_rect: Rect, blending: Blending) {
        let Some(clip) = dst_rect.intersect(&Rect::new(0, 0, dst.width, dst.height)) else { return };
        let sx = src_rect.w as f64 / dst_rect.w as f64;
        let sy = src_rect.h as f64 / dst_rect.h as f64;
        for y in clip.y..clip.bottom() as i32 {
            let v = (src_rect.y as f64 + ((y - dst_rect.y) as f64 + 0.5) * sy).floor() as i64;
            if v < 0 || v >= src.height as i64 {
                continue;
            }
            for x in clip.x..clip.right() as i32 {
                let u = (src_rect.x as f64 + ((x - dst_rect.x) as f64 + 0.5) * sx).floor() as i64;
                if u < 0 || u >= src.width as i64 {
                    continue;
                }
                let si = (v as usize * src.width as usize + u as usize) * 4;
                let di = (y as usize * dst.width as usize + x as usize) * 4;
                match blending {
                    Blending::Srgb => blend(&mut dst.data[di..di + 4], &src.data[si..si + 4]),
                    Blending::Linear => blend_linear(&mut dst.data[di..di + 4], &src.data[si..si + 4]),
                }
            }
        }
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        /// An image with alpha 0, 255 and in between, in runs.
        fn image(&mut self, w: u32, h: u32) -> RgbaImage {
            let mut data = Vec::with_capacity(w as usize * h as usize * 4);
            let mut alpha = 255;
            for _ in 0..w * h {
                if self.below(4) == 0 {
                    alpha = match self.below(3) {
                        0 => 0,
                        1 => 255,
                        _ => self.below(256) as u8,
                    };
                }
                let c = self.next().to_le_bytes();
                data.extend_from_slice(&[c[0], c[1], c[2], alpha]);
            }
            RgbaImage { width: w, height: h, data }
        }
    }

    #[test]
    fn unscaled_blit_equals_the_scaling_path() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let cases = if cfg!(miri) { 30 } else { 3000 };
        for case in 0..cases {
            let (dw, dh) = (1 + rng.below(23) as u32, 1 + rng.below(19) as u32);
            let (sw, sh) = (1 + rng.below(21) as u32, 1 + rng.below(17) as u32);
            let base = rng.image(dw, dh);
            let src = rng.image(sw, sh);
            // Rectangles reaching past every edge of both images.
            let (w, h) = (1 + rng.below(30) as u32, 1 + rng.below(30) as u32);
            let dst_rect = Rect::new(rng.below(40) as i32 - 15, rng.below(40) as i32 - 15, w, h);
            let src_rect = Rect::new(rng.below(40) as i32 - 15, rng.below(40) as i32 - 15, w, h);
            let r = RgbaRef { width: sw, height: sh, data: &src.data };
            for blending in [Blending::Srgb, Blending::Linear] {
                let mut fast = base.clone();
                blit_blended(&mut fast, dst_rect, r, src_rect, blending);
                let mut slow = base.clone();
                blit_scaling(&mut slow, dst_rect, r, src_rect, blending);
                assert!(
                    fast == slow,
                    "case {case}: {dst_rect:?} <- {src_rect:?} of {sw}x{sh} on {dw}x{dh}, {blending:?}"
                );
            }
        }
    }

    #[test]
    fn reset_reuses_and_fills() {
        let mut img = RgbaImage::new(3, 5, [1, 2, 3, 4]);
        assert_eq!(img, RgbaImage { width: 3, height: 5, data: [1, 2, 3, 4].repeat(15) });
        let ptr = img.data.as_ptr();
        img.reset(4, 2, [9, 8, 7, 6]);
        assert_eq!(img, RgbaImage::new(4, 2, [9, 8, 7, 6]));
        assert_eq!(img.data.as_ptr(), ptr, "a smaller image keeps the allocation");
        img.reset(7, 3, [5, 5, 5, 5]);
        assert_eq!(img, RgbaImage::new(7, 3, [5, 5, 5, 5]));
        img.reset(0, 0, [0; 4]);
        assert!(img.data.is_empty());
    }

    /// Composing a 1920x1080 frame of a few unscaled layers:
    /// `cargo test --release -p termpix-core -- --ignored --nocapture compose_timing`.
    #[test]
    #[ignore = "timing, not a check"]
    fn compose_timing() {
        let mut rng = Rng(1);
        let page = RgbaImage::new(1600, 1080, [255, 255, 255, 255]);
        let bar = rng.image(1920, 40);
        let sidebar = RgbaImage::new(320, 1080, [30, 30, 30, 230]);
        let layers = [
            (&page, Rect::new(160, 0, 1600, 1080)),
            (&sidebar, Rect::new(0, 0, 320, 1080)),
            (&bar, Rect::new(0, 1040, 1920, 40)),
        ];
        let runs = 50;
        let t = std::time::Instant::now();
        for _ in 0..runs {
            let mut frame = RgbaImage::new(1920, 1080, [0, 0, 0, 255]);
            for (img, rect) in layers {
                let r = RgbaRef { width: img.width, height: img.height, data: &img.data };
                blit_scaling(&mut frame, rect, r, Rect::new(0, 0, rect.w, rect.h), Blending::Srgb);
            }
            std::hint::black_box(&frame);
        }
        let before = t.elapsed() / runs;
        let mut frame = RgbaImage::new(0, 0, [0; 4]);
        let t = std::time::Instant::now();
        for _ in 0..runs {
            frame.reset(1920, 1080, [0, 0, 0, 255]);
            for (img, rect) in layers {
                let r = RgbaRef { width: img.width, height: img.height, data: &img.data };
                blit(&mut frame, rect, r, Rect::new(0, 0, rect.w, rect.h));
            }
            std::hint::black_box(&frame);
        }
        let after = t.elapsed() / runs;
        eprintln!("compose 1920x1080: new buffer + scaling path {before:?}, reused buffer + unscaled path {after:?}");
    }
}
