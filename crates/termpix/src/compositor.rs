//! Layered compositing on top of the kitty graphics protocol.
//!
//! The application owns images (uploaded once) and, every frame, submits the
//! list of [`Layer`]s to show. The compositor turns that into the minimal set of
//! kitty commands, wrapped in synchronized output.
//!
//! Two backends:
//! - [`Backend::Layered`]: each layer is a kitty placement. Moving a layer only
//!   re-places it (a few dozen bytes); pixels are never re-sent. For kitty and Ghostty.
//! - [`Backend::Composited`]: layers are blended on the CPU into one screen-sized
//!   frame that is re-sent whenever it changes. For terminals whose placement
//!   handling is unreliable (WezTerm, zellij).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::geom::{Geometry, Rect};
use crate::graphics::{Delete, Encoder, Medium, PixelFormat, Pixels, Placement};
use crate::input::GraphicsReply;
use crate::raster::{self, RgbaImage, RgbaRef};

const FIRST_IMAGE_ID: u32 = 2;
/// Image ids for the strips of the composited frame (strip i is `STRIP_IDS + i`).
const STRIP_IDS: u32 = 0x7e_1000;

pub const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
pub const SYNC_END: &[u8] = b"\x1b[?2026l";

/// Handle to an image owned by a [`Compositor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageId(u32);

impl ImageId {
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// One thing to show this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layer {
    /// Stable, non-zero identity of this layer across frames (it becomes the kitty
    /// placement id). Keep it the same while the layer moves so it is re-placed
    /// instead of deleted and recreated.
    pub key: u32,
    pub image: ImageId,
    /// Part of the image to show, in image pixels.
    pub src: Rect,
    /// Where to show it, in screen pixels. May extend past the screen edges; it is
    /// cropped. If the size differs from `src`, the image is scaled (in `Layered`
    /// mode the scaled size is rounded to whole cells).
    pub dst: Rect,
    /// Stacking order; higher is on top.
    pub z: i32,
}

impl Backend {
    /// Composited, one whole-screen image per frame (most compatible).
    pub const fn composited() -> Self {
        Self::Composited { background: [0, 0, 0, 0], strip_rows: 0 }
    }

    /// Composited as strips of `rows` cell rows; only changed strips are sent.
    /// Much less traffic for small updates, but needs a terminal that handles
    /// many images placed side by side (zellij does; WezTerm does not).
    pub const fn composited_strips(rows: u32) -> Self {
        Self::Composited { background: [0, 0, 0, 0], strip_rows: rows }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Layered,
    /// `strip_rows`: send the frame as horizontal strips of this many cell rows,
    /// re-sending only strips that changed; 0 sends one whole-screen image.
    Composited {
        background: [u8; 4],
        strip_rows: u32,
    },
}

#[derive(Debug)]
struct StoredImage {
    width: u32,
    height: u32,
    data: Arc<[u8]>,
    uploaded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Placed {
    image: u32,
    col: u32,
    row: u32,
    placement: Placement,
}

#[derive(Debug)]
pub struct Compositor {
    backend: Backend,
    encoder: Encoder,
    geometry: Geometry,
    images: HashMap<u32, StoredImage>,
    next_id: u32,
    placed: HashMap<u32, Placed>,
    /// Commands that must be emitted with the next frame (deletions).
    pending: Vec<u8>,
    last_frame: Option<RgbaImage>,
    /// Number of composited strips currently shown.
    strips: u32,
    /// Images added but maybe not uploaded yet, oldest first (Layered only).
    not_uploaded: VecDeque<u32>,
    /// Unanswered fence queries (id, when sent); see [`Compositor::is_congested`].
    fences: VecDeque<(u32, Instant)>,
    next_fence: u32,
}

/// Image ids used for fence queries (never stored by the terminal).
const FENCE_IDS: std::ops::Range<u32> = 0x7f_0000..0x7f_ffff;
/// With this many frames unacknowledged, hold back new ones.
const MAX_FENCES: usize = 2;
/// A fence not answered within this time no longer counts (lost reply).
const FENCE_TIMEOUT: Duration = Duration::from_secs(2);

impl Compositor {
    pub fn new(backend: Backend, medium: Medium, geometry: Geometry) -> Self {
        Self {
            backend,
            encoder: Encoder::new(medium),
            geometry,
            images: HashMap::new(),
            next_id: FIRST_IMAGE_ID,
            placed: HashMap::new(),
            pending: Vec::new(),
            last_frame: None,
            strips: 0,
            not_uploaded: VecDeque::new(),
            fences: VecDeque::new(),
            next_fence: FENCE_IDS.start,
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    pub fn encoder_mut(&mut self) -> &mut Encoder {
        &mut self.encoder
    }

    /// Update the terminal geometry (after a resize or font size change). All
    /// placements are redone on the next frame.
    pub fn set_geometry(&mut self, geometry: Geometry) {
        if geometry == self.geometry {
            return;
        }
        self.geometry = geometry;
        self.forget_placements();
        self.last_frame = None;
    }

    /// Add an RGBA image (straight alpha). It is uploaded lazily, the first time a
    /// frame uses it.
    pub fn add_image(&mut self, width: u32, height: u32, data: impl Into<Arc<[u8]>>) -> ImageId {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(FIRST_IMAGE_ID);
        self.insert_image(id, width, height, data.into());
        ImageId(id)
    }

    /// Replace the pixels of an existing image (it may change size).
    pub fn replace_image(&mut self, id: ImageId, width: u32, height: u32, data: impl Into<Arc<[u8]>>) {
        // Re-transmitting an id makes the terminal drop that image's placements.
        self.placed.retain(|_, p| p.image != id.0);
        self.insert_image(id.0, width, height, data.into());
    }

    pub fn remove_image(&mut self, id: ImageId) {
        if let Some(img) = self.images.remove(&id.0)
            && img.uploaded
        {
            self.encoder.delete(&mut self.pending, Delete::Image { id: id.0, free: true });
        }
        self.placed.retain(|_, p| p.image != id.0);
    }

    pub fn image_size(&self, id: ImageId) -> Option<(u32, u32)> {
        self.images.get(&id.0).map(|i| (i.width, i.height))
    }

    fn insert_image(&mut self, id: u32, width: u32, height: u32, data: Arc<[u8]>) {
        assert_eq!(data.len(), width as usize * height as usize * 4, "image data must be RGBA8");
        self.images.insert(id, StoredImage { width, height, data, uploaded: false });
        if matches!(self.backend, Backend::Layered) {
            self.not_uploaded.push_back(id);
        }
    }

    /// Whether some images were added but not sent to the terminal yet.
    pub fn has_pending_uploads(&self) -> bool {
        self.not_uploaded.iter().any(|id| self.images.get(id).is_some_and(|i| !i.uploaded))
    }

    /// Send images that are not shown yet (e.g. pre-rendered neighbours), oldest
    /// first, up to about `budget` raw bytes. Call this when idle and not
    /// congested: on a slow link the transfer then happens before the images are
    /// scrolled into view, and showing them later is only a placement. Returns
    /// whether more remain. Layered backend only.
    pub fn upload_ahead(&mut self, out: &mut Vec<u8>, budget: usize) -> io::Result<bool> {
        let mut sent = 0;
        let mut body = Vec::new();
        while sent < budget {
            let Some(id) = self.not_uploaded.pop_front() else { break };
            let Some(img) = self.images.get_mut(&id) else { continue };
            if img.uploaded {
                continue;
            }
            let px = Pixels { width: img.width, height: img.height, format: PixelFormat::Rgba, data: &img.data };
            self.encoder.transmit(&mut body, id, px, None)?;
            img.uploaded = true;
            sent += img.data.len();
        }
        if !body.is_empty() {
            out.extend_from_slice(&body);
            self.fence(out)?;
        }
        Ok(self.has_pending_uploads())
    }

    /// Append a fence query for inline transfers (see `is_congested`).
    fn fence(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        if matches!(self.encoder.medium(), Medium::Direct { .. }) {
            let id = self.next_fence;
            self.next_fence = if id + 1 >= FENCE_IDS.end { FENCE_IDS.start } else { id + 1 };
            crate::graphics::query(out, id, Medium::Direct { compress: false })?;
            self.fences.push_back((id, Instant::now()));
        }
        Ok(())
    }

    /// React to a graphics reply from the terminal. Returns true when the frame
    /// must be presented again (an image was evicted and will be re-uploaded).
    pub fn handle_reply(&mut self, reply: &GraphicsReply) -> bool {
        if let Some(id) = reply.image_id
            && FENCE_IDS.contains(&id)
        {
            let before = self.fences.len();
            self.fences.retain(|(f, _)| *f != id);
            // A frame was acknowledged: a held-back frame may go out now.
            return self.fences.len() < before;
        }
        if reply.error_code() != Some("ENOENT") {
            return false;
        }
        let Some(id) = reply.image_id else { return false };
        let Some(img) = self.images.get_mut(&id) else { return false };
        img.uploaded = false;
        self.placed.retain(|_, p| p.image != id);
        true
    }

    /// Forget every placement so the next frame re-places all layers. Use after
    /// the screen was cleared or the terminal was reset.
    pub fn forget_placements(&mut self) {
        if matches!(self.backend, Backend::Layered) && !self.placed.is_empty() {
            self.encoder.delete(&mut self.pending, Delete::All { free: false });
        }
        self.placed.clear();
        self.last_frame = None;
    }

    /// True while the terminal has not yet processed recent inline frames. Do not
    /// present new frames then; a reply ([`Compositor::handle_reply`] returns
    /// true) or [`FENCE_TIMEOUT`] ends the congestion. Only inline (direct)
    /// transfers are fenced; shared memory and files are read locally.
    pub fn is_congested(&mut self, now: Instant) -> bool {
        while self.fences.front().is_some_and(|(_, t)| now.duration_since(*t) > FENCE_TIMEOUT) {
            self.fences.pop_front();
        }
        self.fences.len() >= MAX_FENCES
    }

    /// Emit the commands that make the terminal show `layers`. Writes nothing when
    /// the screen would not change.
    pub fn present(&mut self, layers: &[Layer], out: &mut Vec<u8>) -> io::Result<()> {
        let mut body = std::mem::take(&mut self.pending);
        match self.backend {
            Backend::Layered => self.present_layered(layers, &mut body)?,
            Backend::Composited { background, strip_rows } => {
                self.present_composited(layers, background, strip_rows, &mut body)?
            }
        }
        if !body.is_empty() {
            out.extend_from_slice(SYNC_BEGIN);
            out.extend_from_slice(&body);
            out.extend_from_slice(SYNC_END);
            // Inline transfers can outpace a slow link (SSH): ask the terminal to
            // acknowledge this frame, so callers can wait (see is_congested).
            self.fence(out)?;
        }
        // Keep a few recent shm objects: the terminal may not have read them yet.
        self.encoder.cleanup_shm(8);
        Ok(())
    }

    fn present_layered(&mut self, layers: &[Layer], out: &mut Vec<u8>) -> io::Result<()> {
        let screen = self.geometry.screen_rect();
        let cell = self.geometry.cell;
        let mut seen = HashSet::new();
        for layer in layers {
            debug_assert!(layer.key != 0, "layer keys must be non-zero");
            let Some(img) = self.images.get(&layer.image.0) else { continue };
            let src = layer.src.intersect(&Rect::new(0, 0, img.width, img.height));
            let (Some(src), Some(vis)) = (src, layer.dst.intersect(&screen)) else { continue };
            seen.insert(layer.key);

            // Map the visible part of dst back to the source.
            let sx = src.w as f64 / layer.dst.w as f64;
            let sy = src.h as f64 / layer.dst.h as f64;
            let crop = Rect::new(
                src.x + ((vis.x - layer.dst.x) as f64 * sx).round() as i32,
                src.y + ((vis.y - layer.dst.y) as f64 * sy).round() as i32,
                ((vis.w as f64 * sx).round() as u32).max(1),
                ((vis.h as f64 * sy).round() as u32).max(1),
            );
            let scaled = layer.dst.w != src.w || layer.dst.h != src.h;
            let (col, row, placement) = if scaled {
                // kitty scales into whole cells only.
                let col = (vis.x as f64 / cell.width as f64).round() as u32;
                let row = (vis.y as f64 / cell.height as f64).round() as u32;
                let c = ((vis.w as f64 / cell.width as f64).round() as u32).max(1);
                let r = ((vis.h as f64 / cell.height as f64).round() as u32).max(1);
                (
                    col,
                    row,
                    Placement {
                        placement_id: layer.key,
                        src: Some(crop),
                        cells: Some((c, r)),
                        offset: (0, 0),
                        z: layer.z,
                    },
                )
            } else {
                let col = vis.x as u32 / cell.width;
                let row = vis.y as u32 / cell.height;
                let offset = (vis.x as u32 - col * cell.width, vis.y as u32 - row * cell.height);
                (col, row, Placement { placement_id: layer.key, src: Some(crop), cells: None, offset, z: layer.z })
            };
            let placed = Placed { image: layer.image.0, col, row, placement };

            if !img.uploaded {
                let px = Pixels { width: img.width, height: img.height, format: PixelFormat::Rgba, data: &img.data };
                self.encoder.transmit(out, layer.image.0, px, None)?;
                self.images.get_mut(&layer.image.0).expect("present").uploaded = true;
            }
            match self.placed.get(&layer.key) {
                Some(prev) if *prev == placed => continue,
                Some(prev) if prev.image != placed.image => {
                    self.encoder.delete(out, Delete::Placement { id: prev.image, placement_id: layer.key });
                }
                _ => {}
            }
            write!(out, "\x1b[{};{}H", row + 1, col + 1)?;
            self.encoder.place(out, layer.image.0, &placement);
            self.placed.insert(layer.key, placed);
        }
        let gone: Vec<u32> = self.placed.keys().copied().filter(|k| !seen.contains(k)).collect();
        for key in gone {
            let prev = self.placed.remove(&key).expect("key from map");
            self.encoder.delete(out, Delete::Placement { id: prev.image, placement_id: key });
        }
        Ok(())
    }

    fn present_composited(
        &mut self,
        layers: &[Layer],
        background: [u8; 4],
        strip_rows: u32,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        let (w, h) = (self.geometry.width_px(), self.geometry.height_px());
        let mut frame = RgbaImage::new(w, h, background);
        let mut order: Vec<&Layer> = layers.iter().collect();
        order.sort_by_key(|l| l.z);
        for layer in order {
            let Some(img) = self.images.get(&layer.image.0) else { continue };
            let src = RgbaRef { width: img.width, height: img.height, data: &img.data };
            raster::blit(&mut frame, layer.dst, src, layer.src);
        }
        // Send the frame as horizontal strips of whole cell rows, and only the
        // strips that changed since the last frame.
        let strip_h = if strip_rows == 0 { h } else { strip_rows * self.geometry.cell.height }.max(1);
        let count = h.div_ceil(strip_h);
        let row_bytes = w as usize * 4;
        for i in 0..count {
            let y0 = i * strip_h;
            let y1 = (y0 + strip_h).min(h);
            let range = y0 as usize * row_bytes..y1 as usize * row_bytes;
            let unchanged = self.last_frame.as_ref().is_some_and(|last| {
                i < self.strips && last.data.get(range.clone()) == Some(&frame.data[range.clone()])
            });
            if unchanged {
                continue;
            }
            write!(out, "\x1b[{};1H", i * strip_h / self.geometry.cell.height.max(1) + 1)?;
            let px = Pixels { width: w, height: y1 - y0, format: PixelFormat::Rgba, data: &frame.data[range] };
            // Re-transmitting an id replaces the image and its placement in place.
            let place = Placement { placement_id: 1, ..Default::default() };
            self.encoder.transmit(out, STRIP_IDS + i, px, Some(&place))?;
        }
        // Strips that no longer exist (the screen got shorter).
        for i in count..self.strips {
            self.encoder.delete(out, Delete::Image { id: STRIP_IDS + i, free: true });
        }
        self.strips = count;
        self.last_frame = Some(frame);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::CellSize;
    use crate::mock::MockTerminal;

    fn geometry() -> Geometry {
        Geometry { cols: 8, rows: 4, cell: CellSize { width: 10, height: 20 } }
    }

    /// A 30x30 image: left half red, right half blue.
    fn test_image() -> Vec<u8> {
        let mut v = Vec::new();
        for _y in 0..30 {
            for x in 0..30 {
                v.extend_from_slice(if x < 15 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
            }
        }
        v
    }

    fn layer(img: ImageId, x: i32, y: i32) -> Layer {
        Layer { key: 7, image: img, src: Rect::new(0, 0, 30, 30), dst: Rect::new(x, y, 30, 30), z: 0 }
    }

    #[test]
    fn layered_places_with_pixel_offsets() {
        let mut c = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, geometry());
        let img = c.add_image(30, 30, test_image());
        let mut out = Vec::new();
        c.present(&[layer(img, 13, 25)], &mut out).unwrap();
        let mut term = MockTerminal::new(geometry(), [0, 0, 0, 255]);
        term.feed(&out);
        let screen = term.render();
        assert_eq!(screen.pixel(12, 25), [0, 0, 0, 255]);
        assert_eq!(screen.pixel(13, 25), [255, 0, 0, 255]);
        assert_eq!(screen.pixel(13 + 15, 30), [0, 0, 255, 255]);
        assert_eq!(screen.pixel(13 + 30, 30), [0, 0, 0, 255]);
    }

    #[test]
    fn layered_moves_without_reupload() {
        let mut c = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, geometry());
        let img = c.add_image(30, 30, test_image());
        let mut first = Vec::new();
        c.present(&[layer(img, 0, 0)], &mut first).unwrap();
        let mut second = Vec::new();
        c.present(&[layer(img, 0, 0)], &mut second).unwrap();
        assert!(second.is_empty(), "unchanged frame must emit nothing");
        let mut third = Vec::new();
        c.present(&[layer(img, 5, 3)], &mut third).unwrap();
        let text = String::from_utf8(third).unwrap();
        assert!(text.contains("a=p,"));
        assert!(!text.contains("a=t,"), "moving must not re-send pixels: {text}");
    }

    #[test]
    fn layered_crops_offscreen_parts() {
        let mut c = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, geometry());
        let img = c.add_image(30, 30, test_image());
        let mut out = Vec::new();
        // 20 px off the left edge: only the blue half and 5 px of red remain.
        c.present(&[layer(img, -10, 0)], &mut out).unwrap();
        let mut term = MockTerminal::new(geometry(), [0, 0, 0, 255]);
        term.feed(&out);
        let screen = term.render();
        assert_eq!(screen.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(screen.pixel(5, 0), [0, 0, 255, 255]);
        assert_eq!(screen.pixel(20, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn removed_layers_are_deleted() {
        let mut c = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, geometry());
        let img = c.add_image(30, 30, test_image());
        let mut out = Vec::new();
        c.present(&[layer(img, 0, 0)], &mut out).unwrap();
        c.present(&[], &mut out).unwrap();
        let mut term = MockTerminal::new(geometry(), [0, 0, 0, 255]);
        term.feed(&out);
        assert_eq!(term.placement_count(), 0);
        assert_eq!(term.render().pixel(0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn evicted_image_is_reuploaded() {
        let mut c = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, geometry());
        let img = c.add_image(30, 30, test_image());
        let mut term = MockTerminal::new(geometry(), [0, 0, 0, 255]);
        let mut out = Vec::new();
        c.present(&[layer(img, 0, 0)], &mut out).unwrap();
        term.feed(&out);
        term.evict(img.raw());
        // Moving the layer now fails with ENOENT.
        let mut out = Vec::new();
        c.present(&[layer(img, 10, 0)], &mut out).unwrap();
        term.feed(&out);
        let replies = crate::input::Parser::new().feed(&term.take_replies());
        let reply = replies
            .iter()
            .find_map(|e| match e {
                crate::input::Event::Graphics(g) if g.error_code() == Some("ENOENT") => Some(g),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{replies:?}"));
        assert!(c.handle_reply(reply));
        let mut out = Vec::new();
        c.present(&[layer(img, 10, 0)], &mut out).unwrap();
        term.feed(&out);
        assert_eq!(term.render().pixel(10, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn composited_matches_layered() {
        let layers_for = |img| {
            [
                Layer { key: 1, image: img, src: Rect::new(0, 0, 30, 30), dst: Rect::new(-7, 3, 30, 30), z: 0 },
                Layer { key: 2, image: img, src: Rect::new(10, 0, 20, 10), dst: Rect::new(40, 50, 20, 10), z: 1 },
            ]
        };
        let mut screens = Vec::new();
        for backend in [Backend::Layered, Backend::composited(), Backend::composited_strips(2)] {
            let mut c = Compositor::new(backend, Medium::Direct { compress: true }, geometry());
            let img = c.add_image(30, 30, test_image());
            let mut out = Vec::new();
            c.present(&layers_for(img), &mut out).unwrap();
            let mut term = MockTerminal::new(geometry(), [0, 0, 0, 255]);
            term.feed(&out);
            screens.push(term.render());
        }
        assert_eq!(screens[0], screens[1]);
    }

    #[test]
    fn inline_frames_are_fenced() {
        let mut c = Compositor::new(Backend::Layered, Medium::Direct { compress: true }, geometry());
        let img = c.add_image(30, 30, test_image());
        let mut term = MockTerminal::new(geometry(), [0, 0, 0, 255]);
        let now = Instant::now();
        for x in [0, 10] {
            let mut out = Vec::new();
            c.present(&[layer(img, x, 0)], &mut out).unwrap();
            term.feed(&out);
        }
        assert!(c.is_congested(now), "two frames unacknowledged");
        let replies = crate::input::Parser::new().feed(&term.take_replies());
        let mut released = false;
        for r in &replies {
            if let crate::input::Event::Graphics(g) = r {
                released |= c.handle_reply(g);
            }
        }
        assert!(released && !c.is_congested(now));
        // Lost replies stop counting after the timeout.
        let mut out = Vec::new();
        c.present(&[layer(img, 20, 0)], &mut out).unwrap();
        c.present(&[layer(img, 25, 0)], &mut out).unwrap();
        let after = Instant::now();
        assert!(c.is_congested(after));
        assert!(!c.is_congested(after + FENCE_TIMEOUT + Duration::from_millis(1)));
    }

    #[test]
    fn images_can_be_uploaded_ahead_of_use() {
        let mut c = Compositor::new(Backend::Layered, Medium::Direct { compress: true }, geometry());
        let a = c.add_image(30, 30, test_image());
        let _b = c.add_image(30, 30, test_image());
        assert!(c.has_pending_uploads());
        let mut out = Vec::new();
        // A budget smaller than one image still sends one.
        assert!(c.upload_ahead(&mut out, 1).unwrap(), "one image left");
        assert!(c.upload_ahead(&mut out, 1 << 20).is_ok_and(|more| !more));
        let mut term = MockTerminal::new(geometry(), [0, 0, 0, 255]);
        term.feed(&out);
        assert_eq!(term.image_count(), 2);
        // Showing an uploaded image now is only a placement.
        let mut frame = Vec::new();
        c.present(&[layer(a, 0, 0)], &mut frame).unwrap();
        let text = String::from_utf8_lossy(&frame);
        assert!(text.contains("a=p,") && !text.contains("a=t,"), "{text}");
    }

    #[test]
    fn shared_memory_frames_are_not_fenced() {
        let mut c = Compositor::new(Backend::Layered, Medium::SharedMemory, geometry());
        let img = c.add_image(30, 30, test_image());
        for x in [0, 10, 20] {
            c.present(&[layer(img, x, 0)], &mut Vec::new()).unwrap();
        }
        assert!(!c.is_congested(Instant::now()));
        c.encoder_mut().cleanup_shm(0);
    }

    #[test]
    fn composited_sends_only_changed_strips() {
        let g = geometry(); // 8x4 cells of 10x20 px: two strips of two rows
        let mut c = Compositor::new(Backend::composited_strips(2), Medium::Direct { compress: false }, g);
        let img = c.add_image(30, 30, test_image());
        let count = |out: &[u8]| String::from_utf8_lossy(out).matches("a=T,").count();
        let mut out = Vec::new();
        c.present(&[layer(img, 0, 0)], &mut out).unwrap();
        assert_eq!(count(&out), 2, "first frame sends every strip");
        let mut term = MockTerminal::new(g, [0, 0, 0, 255]);
        term.feed(&out);
        // Move the image within the top strip only (y < 40 px).
        let mut out2 = Vec::new();
        c.present(&[layer(img, 5, 0)], &mut out2).unwrap();
        assert_eq!(count(&out2), 1, "only the top strip changed");
        term.feed(&out2);
        assert_eq!(term.render().pixel(5, 0), [255, 0, 0, 255]);
        assert_eq!(term.render().pixel(4, 0), [0, 0, 0, 255], "uncovered: terminal background");
    }

    #[test]
    fn composited_skips_identical_frames() {
        let mut c = Compositor::new(Backend::composited(), Medium::Direct { compress: true }, geometry());
        let img = c.add_image(30, 30, test_image());
        let mut out = Vec::new();
        c.present(&[layer(img, 0, 0)], &mut out).unwrap();
        assert!(!out.is_empty());
        let mut again = Vec::new();
        c.present(&[layer(img, 0, 0)], &mut again).unwrap();
        assert!(again.is_empty());
    }
}
