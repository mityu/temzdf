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
pub struct ImageKey(u64);

impl ImageKey {
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Straight-alpha RGBA8 pixels, row-major, `width * height * 4` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
}

/// A change to an [`Images`] store, for frontends mirroring it.
#[derive(Clone, Debug, PartialEq, Eq)]
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
pub struct Layer {
    pub key: u32,
    pub image: ImageKey,
    pub src: Rect,
    pub dst: Rect,
    pub z: i32,
}

/// Mouse pointer shapes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pointer {
    #[default]
    Default,
    /// Over something clickable (a link).
    Link,
}

/// Everything a pane shows at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub layers: Vec<Layer>,
    /// Text cursor cell (column, row), shown only where text is typed so that
    /// input method preedit appears there.
    pub cursor: Option<(u16, u16)>,
    pub pointer: Pointer,
    /// Window title.
    pub title: String,
}

/// What a frontend is good at, so panes can choose how to draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
pub enum Effect {
    /// Put text on the clipboard.
    Copy(String),
    /// Open an external link with the system handler.
    OpenUri(String),
    /// Run a shell command detached from the terminal (SyncTeX inverse search).
    RunShell(String),
}

#[cfg(test)]
mod tests {
    use super::*;

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
