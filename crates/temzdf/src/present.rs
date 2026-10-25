//! Shows a pane's scene ([`Images`] and [`Layer`]s from `temzdf-scene`) through
//! a termpix [`Compositor`]: mirrors the image store into the compositor and
//! maps layers onto compositor layers.

use std::collections::HashMap;

use temzdf_scene::{ImageKey, ImageUpdate, Images, Layer};
use termpix::compositor::{self, Compositor, ImageId};

#[derive(Debug, Default)]
pub struct Presenter {
    ids: HashMap<ImageKey, ImageId>,
}

impl Presenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply the store's changes since the last call to the compositor.
    pub fn sync(&mut self, comp: &mut Compositor, images: &mut Images) {
        for update in images.take_updates() {
            match update {
                ImageUpdate::Set(key, img) => match self.ids.get(&key) {
                    Some(&id) => comp.replace_image(id, img.width, img.height, img.pixels),
                    None => {
                        let id = comp.add_image(img.width, img.height, img.pixels);
                        self.ids.insert(key, id);
                    }
                },
                ImageUpdate::Remove(key) => {
                    if let Some(id) = self.ids.remove(&key) {
                        comp.remove_image(id);
                    }
                }
            }
        }
    }

    /// Compositor layers for scene layers (after [`Presenter::sync`]). Layers
    /// whose image is unknown are skipped.
    pub fn layers(&self, layers: &[Layer]) -> Vec<compositor::Layer> {
        layers
            .iter()
            .filter_map(|l| {
                let image = *self.ids.get(&l.image)?;
                Some(compositor::Layer { key: l.key, image, src: l.src, dst: l.dst, z: l.z })
            })
            .collect()
    }
}
