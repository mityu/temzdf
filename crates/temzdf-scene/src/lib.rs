//! What a pane shows, independent of the frontend that draws it.
//!
//! A pane (the PDF viewer today; terminals and web pages later) owns an
//! [`Images`] store and produces a [`Frame`] of [`Layer`]s referencing images in
//! it. A frontend (the terminal one in `temzdf-tty`, a GUI later) mirrors the
//! store by applying [`Images::take_updates`] and draws the layers. Image pixels
//! are shared ([`Arc`]), so mirroring them costs no copies.
//!
//! See `doc/session-server.md` for where this is going (several panes, clients
//! attached through a socket).

use std::collections::HashMap;
use std::sync::Arc;

pub use termpix_core::geom::Rect;
pub use termpix_core::input;

/// An image in an [`Images`] store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ImageKey(u64);

impl ImageKey {
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Straight-alpha RGBA8 pixels, row-major, `width * height * 4` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Image {
    pub width: u32,
    pub height: u32,
    #[cfg_attr(feature = "serde", serde(with = "bytes"))]
    pub pixels: Arc<[u8]>,
}

/// Pixels as one byte string on the wire, not a sequence of numbers.
#[cfg(feature = "serde")]
mod bytes {
    use std::sync::Arc;

    pub fn serialize<S: serde::Serializer>(v: &Arc<[u8]>, s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::serialize(&**v, s)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Arc<[u8]>, D::Error> {
        let buf: serde_bytes::ByteBuf = serde::Deserialize::deserialize(d)?;
        Ok(buf.into_vec().into())
    }
}

/// A change to an [`Images`] store, for frontends mirroring it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ImageUpdate {
    /// A new image, or new pixels (possibly another size) for an existing one.
    Set(ImageKey, Image),
    Remove(ImageKey),
}

/// The images a pane shows, with the changes since the last
/// [`Images::take_updates`].
#[derive(Debug, Default)]
pub struct Images {
    images: HashMap<ImageKey, Image>,
    next: u64,
    updates: Vec<ImageUpdate>,
}

impl Images {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, width: u32, height: u32, pixels: impl Into<Arc<[u8]>>) -> ImageKey {
        let key = ImageKey(self.next);
        self.next += 1;
        self.set(key, width, height, pixels.into());
        key
    }

    /// Replace the pixels of an image (it may change size).
    pub fn replace(&mut self, key: ImageKey, width: u32, height: u32, pixels: impl Into<Arc<[u8]>>) {
        if self.images.contains_key(&key) {
            self.set(key, width, height, pixels.into());
        }
    }

    fn set(&mut self, key: ImageKey, width: u32, height: u32, pixels: Arc<[u8]>) {
        debug_assert_eq!(pixels.len(), width as usize * height as usize * 4);
        let image = Image { width, height, pixels };
        self.images.insert(key, image.clone());
        self.updates.push(ImageUpdate::Set(key, image));
    }

    pub fn remove(&mut self, key: ImageKey) {
        if self.images.remove(&key).is_some() {
            self.updates.push(ImageUpdate::Remove(key));
        }
    }

    pub fn get(&self, key: ImageKey) -> Option<&Image> {
        self.images.get(&key)
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Every image, as updates that recreate the store from nothing (for a
    /// frontend that starts mirroring it late).
    pub fn snapshot(&self) -> Vec<ImageUpdate> {
        let mut keys: Vec<_> = self.images.keys().copied().collect();
        keys.sort();
        keys.into_iter().map(|k| ImageUpdate::Set(k, self.images[&k].clone())).collect()
    }

    /// The changes since the last call, in order.
    pub fn take_updates(&mut self) -> Vec<ImageUpdate> {
        std::mem::take(&mut self.updates)
    }
}

/// One image placed on screen: `src` (image pixels) shown at `dst` (screen
/// pixels, may extend past the edges; scaled when the sizes differ). Higher `z`
/// is drawn on top. `key` identifies the placement across frames, so a frontend
/// can move it instead of re-creating it; keys are unique within a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Layer {
    pub key: u32,
    pub image: ImageKey,
    pub src: Rect,
    pub dst: Rect,
    pub z: i32,
}

impl Layer {
    /// The part of the layer inside `clip` (screen pixels), its source cropped
    /// to match (proportionally for scaled layers, rounded to whole pixels).
    pub fn clipped(&self, clip: &Rect) -> Option<Layer> {
        let vis = self.dst.intersect(clip)?;
        if vis == self.dst {
            return Some(*self);
        }
        let fx = self.src.w as f64 / self.dst.w.max(1) as f64;
        let fy = self.src.h as f64 / self.dst.h.max(1) as f64;
        let x0 = (self.src.x as f64 + (vis.x - self.dst.x) as f64 * fx).round() as i32;
        let y0 = (self.src.y as f64 + (vis.y - self.dst.y) as f64 * fy).round() as i32;
        let x1 = (self.src.x as f64 + (vis.right() - self.dst.x as i64) as f64 * fx).round() as i32;
        let y1 = (self.src.y as f64 + (vis.bottom() - self.dst.y as i64) as f64 * fy).round() as i32;
        let src = Rect::new(x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
        Some(Layer { src, dst: vis, ..*self })
    }
}

/// Mouse pointer shapes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Pointer {
    #[default]
    Default,
    /// Over something clickable (a link).
    Link,
}

/// Everything a pane shows at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Frame {
    pub layers: Vec<Layer>,
    /// Text in cells (terminal panes), drawn above the layers. Cells outside
    /// every grid show the layers below.
    pub text: Vec<TextGrid>,
    /// Text cursor cell (column, row), shown where text is typed (a terminal
    /// pane, a prompt) so that input method preedit appears there.
    pub cursor: Option<(u16, u16)>,
    pub cursor_shape: CursorShape,
    pub pointer: Pointer,
    /// Window title.
    pub title: String,
}

/// A rectangle of text cells at (`col`, `row`) of the screen; every row has
/// the same number of cells.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TextGrid {
    pub col: u16,
    pub row: u16,
    pub rows: Vec<Vec<Cell>>,
}

impl TextGrid {
    pub fn width(&self) -> u16 {
        self.rows.first().map_or(0, |r| r.len() as u16)
    }
}

/// One text cell. A wide character takes two cells: the second is a spacer
/// (`width` 0, empty text).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cell {
    /// The grapheme; empty for a blank cell or a spacer.
    pub text: String,
    /// 1, 2 for a wide character, 0 for the spacer after one.
    pub width: u8,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

/// A text colour. Palette colours stay indices, so the user's terminal shows
/// them in its own theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Color {
    /// The terminal's default foreground or background.
    #[default]
    Default,
    /// An index into the 256-colour palette.
    Indexed(u8),
    Rgb([u8; 3]),
}

impl Cell {
    pub fn blank() -> Self {
        Self { width: 1, ..Default::default() }
    }
}

/// Text attributes, as SGR knows them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Attrs {
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub underline: Underline,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// How the text cursor looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CursorShape {
    /// Whatever the user's terminal shows by default.
    #[default]
    Default,
    Block {
        blink: bool,
    },
    Underline {
        blink: bool,
    },
    Bar {
        blink: bool,
    },
}

/// What a frontend is good at, so panes can choose how to draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Hints {
    /// Scaled layers are shown exactly where asked (a terminal compositing
    /// frames itself, or a GPU); otherwise they may snap to cells.
    pub exact_scaling: bool,
    /// New images reach the screen cheaply (shared memory, files, a GPU), so
    /// short-lived images are fine.
    pub fast_uploads: bool,
}

/// Something a frontend does on a pane's behalf, in the frontend's own
/// environment (its clipboard, its `DISPLAY`, its user's editor).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Effect {
    /// Put text on the clipboard.
    Copy(String),
    /// Open an external link: with `command` (the URI appended), else the
    /// system handler (`xdg-open`, `open`).
    OpenUri { uri: String, command: Option<Vec<String>> },
    /// Run a shell command detached from the terminal (SyncTeX inverse search).
    RunShell(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_crops_the_source_to_match() {
        let l =
            Layer { key: 1, image: ImageKey(0), src: Rect::new(0, 0, 100, 50), dst: Rect::new(10, 10, 100, 50), z: 0 };
        assert_eq!(l.clipped(&Rect::new(0, 0, 500, 500)), Some(l));
        let c = l.clipped(&Rect::new(50, 0, 30, 30)).unwrap();
        assert_eq!((c.dst, c.src), (Rect::new(50, 10, 30, 20), Rect::new(40, 0, 30, 20)));
        assert!(l.clipped(&Rect::new(200, 0, 10, 10)).is_none());
        // Scaled 2x: the source shrinks by half.
        let s = Layer { src: Rect::new(0, 0, 50, 25), ..l };
        let c = s.clipped(&Rect::new(60, 10, 50, 50)).unwrap();
        assert_eq!((c.dst, c.src), (Rect::new(60, 10, 50, 50), Rect::new(25, 0, 25, 25)));
    }

    #[test]
    fn updates_mirror_the_store() {
        let mut images = Images::new();
        let a = images.add(1, 1, vec![1, 2, 3, 4]);
        let b = images.add(1, 1, vec![5, 6, 7, 8]);
        images.replace(a, 2, 1, vec![0; 8]);
        images.remove(b);
        images.remove(b);
        images.replace(b, 1, 1, vec![0; 4]);
        let updates = images.take_updates();
        assert_eq!(updates.len(), 4, "{updates:?}");
        assert!(matches!(&updates[2], ImageUpdate::Set(k, img) if *k == a && img.width == 2));
        assert_eq!(updates[3], ImageUpdate::Remove(b));
        assert!(images.take_updates().is_empty());
        assert_eq!(images.len(), 1);
        assert_eq!(images.get(a).unwrap().pixels.len(), 8);
    }
}
