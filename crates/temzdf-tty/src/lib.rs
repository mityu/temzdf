//! The terminal frontend: shows a pane's scene (`temzdf-scene` images and
//! frames) through termpix. [`Presenter`] mirrors the image store into a
//! [`Compositor`] and maps layers; [`Screen`] turns a [`Frame`] into the bytes
//! for the terminal (title, images, text cursor); [`pointer_sequence`] and
//! [`copy_sequence`] cover the pointer shape and the clipboard.

use std::collections::HashMap;
use std::io;

use temzdf_scene::{Frame, ImageKey, ImageUpdate, Images, Layer, Pointer};
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

/// What was last written to the terminal besides images.
#[derive(Debug, Default)]
pub struct Screen {
    out: Vec<u8>,
    title: String,
    cursor: Option<(u16, u16)>,
}

impl Screen {
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes for one frame: the window title when it changed, the
    /// compositor's commands, and the text cursor (shown only at an input
    /// caret, where IME preedit follows it). Empty when nothing changed.
    pub fn render(&mut self, frame: &Frame, presenter: &Presenter, comp: &mut Compositor) -> io::Result<&[u8]> {
        let out = &mut self.out;
        out.clear();
        if frame.title != self.title {
            // Control characters would end the OSC early.
            let clean: String = frame.title.chars().filter(|c| !c.is_control()).collect();
            out.extend_from_slice(format!("\x1b]2;{clean}\x1b\\").as_bytes());
            self.title = frame.title.clone();
        }
        comp.present(&presenter.layers(&frame.layers), out)?;
        let cursor = frame.cursor;
        if cursor != self.cursor {
            match cursor {
                Some((col, row)) => out.extend_from_slice(format!("\x1b[{};{}H\x1b[?25h", row + 1, col + 1).as_bytes()),
                None => out.extend_from_slice(b"\x1b[?25l"),
            }
            self.cursor = cursor;
        } else if let Some((col, row)) = cursor
            && !out.is_empty()
        {
            // Placing images moves the cursor; put it back.
            out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
        }
        Ok(out)
    }
}

/// OSC 22: set the mouse pointer shape.
pub fn pointer_sequence(pointer: Pointer) -> Vec<u8> {
    let name = match pointer {
        Pointer::Default => "default",
        Pointer::Link => "pointer",
    };
    format!("\x1b]22;{name}\x1b\\").into_bytes()
}

/// OSC 52: put `text` on the clipboard. Inside tmux (default `set-clipboard
/// external`, which keeps OSC 52 from applications to itself) it is passed
/// straight through to the terminal.
pub fn copy_sequence(text: &str, in_tmux: bool) -> Vec<u8> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let osc = format!("\x1b]52;c;{b64}\x1b\\").into_bytes();
    if in_tmux { termpix::placeholder::tmux_passthrough(&osc) } else { osc }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termpix::compositor::Backend;
    use termpix::geom::{CellSize, Geometry};
    use termpix::graphics::Medium;

    #[test]
    fn screen_writes_title_and_cursor_only_when_they_change() {
        let g = Geometry { cols: 10, rows: 5, cell: CellSize { width: 8, height: 16 } };
        let mut comp = Compositor::new(Backend::Layered, Medium::Direct { compress: false }, g);
        let mut images = Images::new();
        let mut presenter = Presenter::new();
        let key = images.add(1, 1, vec![255; 4]);
        presenter.sync(&mut comp, &mut images);
        let layer = Layer {
            key: 1,
            image: key,
            src: termpix::geom::Rect::new(0, 0, 1, 1),
            dst: termpix::geom::Rect::new(0, 0, 8, 8),
            z: 0,
        };
        let mut frame =
            Frame { layers: vec![layer], cursor: Some((2, 3)), title: "a\x07b".into(), ..Default::default() };
        let mut screen = Screen::new();
        let first = String::from_utf8_lossy(screen.render(&frame, &presenter, &mut comp).unwrap()).into_owned();
        assert!(first.starts_with("\x1b]2;ab\x1b\\"), "title without control characters: {first:?}");
        assert!(first.contains("a=T") || first.contains("a=t"), "the image: {first:?}");
        assert!(first.ends_with("\x1b[4;3H\x1b[?25h"), "cursor shown: {first:?}");
        assert!(screen.render(&frame, &presenter, &mut comp).unwrap().is_empty(), "nothing changed");
        frame.cursor = None;
        assert_eq!(screen.render(&frame, &presenter, &mut comp).unwrap(), b"\x1b[?25l");
    }

    #[test]
    fn pointer_and_clipboard_sequences() {
        assert_eq!(pointer_sequence(Pointer::Link), b"\x1b]22;pointer\x1b\\");
        assert_eq!(copy_sequence("hi", false), b"\x1b]52;c;aGk=\x1b\\");
        assert!(copy_sequence("hi", true).starts_with(b"\x1bPtmux;"));
    }
}
