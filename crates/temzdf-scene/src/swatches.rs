//! Solid-colour images by size and colour, for overlays (search hits, the
//! selection) drawn as large as they are shown: terminals scale a stretched
//! pixel each their own way, so a 1x1 image stretched is not enough.

use std::collections::HashMap;

use crate::{ImageKey, Images};

/// More than this many sizes and colours (zooming makes new sizes each
/// step), and [`Swatches::trim`] drops them all.
const MAX: usize = 256;

/// The images handed out, by width, height and straight-alpha RGBA.
#[derive(Debug, Default)]
pub struct Swatches(HashMap<(u32, u32, [u8; 4]), ImageKey>);

impl Swatches {
    /// A `w` x `h` image of `rgba`, added to `images` the first time.
    pub fn get(&mut self, images: &mut Images, w: u32, h: u32, rgba: [u8; 4]) -> ImageKey {
        *self.0.entry((w, h, rgba)).or_insert_with(|| images.add(w, h, rgba.repeat((w * h) as usize)))
    }

    /// Drop them all once too many piled up. Call before a frame names
    /// any, so none a frame shows goes.
    pub fn trim(&mut self, images: &mut Images) {
        if self.0.len() > MAX {
            self.release(images);
        }
    }

    /// They all leave `images` (the pane closed).
    pub fn release(&mut self, images: &mut Images) {
        for (_, key) in self.0.drain() {
            images.remove(key);
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_image_per_size_and_colour() {
        let mut images = Images::new();
        let mut s = Swatches::default();
        let a = s.get(&mut images, 3, 2, [1, 2, 3, 4]);
        assert_eq!(s.get(&mut images, 3, 2, [1, 2, 3, 4]), a);
        assert_ne!(s.get(&mut images, 3, 2, [1, 2, 3, 5]), a);
        assert_ne!(s.get(&mut images, 2, 3, [1, 2, 3, 4]), a);
        let image = images.get(a).unwrap();
        assert_eq!((image.width, image.height), (3, 2));
        assert_eq!(&image.pixels[..], [1, 2, 3, 4].repeat(6));
        s.release(&mut images);
        assert!(images.is_empty() && s.is_empty());
    }

    #[test]
    fn trim_drops_all_only_past_the_limit() {
        let mut images = Images::new();
        let mut s = Swatches::default();
        for w in 1..=MAX as u32 {
            s.get(&mut images, w, 1, [0; 4]);
        }
        s.trim(&mut images);
        assert_eq!(s.len(), MAX, "at the limit, kept");
        s.get(&mut images, MAX as u32 + 1, 1, [0; 4]);
        s.trim(&mut images);
        assert!(s.is_empty() && images.is_empty());
        // Handed out after the trim: in the store.
        let k = s.get(&mut images, 5, 5, [9; 4]);
        assert!(images.get(k).is_some());
    }
}
