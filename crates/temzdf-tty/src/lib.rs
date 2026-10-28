//! The terminal frontend: shows a pane's scene (`temzdf-scene` images and
//! frames) through termpix. [`Presenter`] mirrors the image store into a
//! [`Compositor`] and maps layers; [`Screen`] turns a [`Frame`] into the bytes
//! for the terminal (title, images, text cursor); [`pointer_sequence`] and
//! [`copy_sequence`] cover the pointer shape and the clipboard.

use std::collections::HashMap;
use std::io;

mod frontend;
mod text;
pub use frontend::{Input, Presented, TtyFrontend};
pub use text::TextScreen;

use temzdf_scene::{CursorShape, Frame, ImageKey, ImageUpdate, Images, Layer, Pointer};
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
        self.apply(comp, images.take_updates());
    }

    /// Apply changes of an image store kept elsewhere (another process).
    pub fn apply(&mut self, comp: &mut Compositor, updates: Vec<ImageUpdate>) {
        for update in updates {
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
    shape: CursorShape,
    text: TextScreen,
    /// Screen size the text was written for.
    size: (u16, u16),
}

impl Screen {
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes for one frame: the window title when it changed, the
    /// compositor's commands with the text cells that changed, and the text
    /// cursor (shown only where text is typed, where IME preedit follows it).
    /// Empty when nothing changed.
    pub fn render(&mut self, frame: &Frame, presenter: &Presenter, comp: &mut Compositor) -> io::Result<&[u8]> {
        self.out.clear();
        self.title(frame);
        let g = comp.geometry();
        if (g.cols, g.rows) != self.size {
            // The terminal may have reflowed or dropped our text: start clean.
            self.size = (g.cols, g.rows);
            self.text.resize(g.cols, g.rows);
            self.out.extend_from_slice(b"\x1b[2J");
            comp.rewrite_placeholder_text();
        }
        // Text leaving cells that tmux placeholder text covered before: write
        // the placeholders again, and the text over them.
        if comp.placeholder_text_pending() || (self.text.would_clear(&frame.text) && is_placeholders(comp)) {
            comp.rewrite_placeholder_text();
            self.text.invalidate();
        }
        let before = self.text.clear_gone(&frame.text);
        let (text, grids) = (&mut self.text, &frame.text);
        comp.present_with(
            &presenter.layers(&frame.layers),
            &before,
            |placed| {
                text.redraw_wide(placed);
                text.render(grids)
            },
            &mut self.out,
        )?;
        self.cursor(frame);
        Ok(&self.out)
    }

    /// Like [`Screen::render`], but leaves the images as they are: for a
    /// frame whose images have to wait (the frame-rate cap) while its text
    /// (a shell echoing keys) should not. `None` when text and images must
    /// change together: a new screen size, or tmux placeholder text to write
    /// again.
    pub fn render_text(&mut self, frame: &Frame, comp: &mut Compositor) -> Option<&[u8]> {
        let g = comp.geometry();
        if (g.cols, g.rows) != self.size
            || comp.placeholder_text_pending()
            || (self.text.would_clear(&frame.text) && is_placeholders(comp))
        {
            return None;
        }
        self.out.clear();
        self.title(frame);
        let text = self.text.render(&frame.text);
        comp.present_text(&text, &mut self.out);
        self.cursor(frame);
        Some(&self.out)
    }

    fn title(&mut self, frame: &Frame) {
        if frame.title != self.title {
            // Control characters would end the OSC early.
            let clean: String = frame.title.chars().filter(|c| !c.is_control()).collect();
            self.out.extend_from_slice(format!("\x1b]2;{clean}\x1b\\").as_bytes());
            self.title = frame.title.clone();
        }
    }

    fn cursor(&mut self, frame: &Frame) {
        let out = &mut self.out;
        if frame.cursor_shape != self.shape {
            let n = match frame.cursor_shape {
                CursorShape::Default => 0,
                CursorShape::Block { blink } => 2 - blink as u8,
                CursorShape::Underline { blink } => 4 - blink as u8,
                CursorShape::Bar { blink } => 6 - blink as u8,
            };
            out.extend_from_slice(format!("\x1b[{n} q").as_bytes());
            self.shape = frame.cursor_shape;
        }
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
            // Placing images and writing text move the cursor; put it back.
            out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
        }
    }
}

fn is_placeholders(comp: &Compositor) -> bool {
    matches!(comp.backend(), termpix::compositor::Backend::Placeholders { .. })
}

/// OSC 22: set the mouse pointer shape.
pub fn pointer_sequence(pointer: Pointer) -> Vec<u8> {
    let name = match pointer {
        Pointer::Default => "default",
        Pointer::Link => "pointer",
        Pointer::ResizeColumns => "ew-resize",
        Pointer::ResizeRows => "ns-resize",
        Pointer::Move => "move",
        Pointer::ResizeNwse => "nwse-resize",
        Pointer::ResizeNesw => "nesw-resize",
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
    use temzdf_scene::{Cell, TextGrid};
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
    fn text_goes_out_alone_while_images_wait() {
        let g = Geometry { cols: 10, rows: 5, cell: CellSize { width: 8, height: 16 } };
        let mut comp = Compositor::new(Backend::composited(), Medium::Direct { compress: false }, g);
        let mut images = Images::new();
        let mut presenter = Presenter::new();
        let key = images.add(1, 1, vec![255; 4]);
        presenter.sync(&mut comp, &mut images);
        let layer = |x| Layer {
            key: 1,
            image: key,
            src: termpix::geom::Rect::new(0, 0, 1, 1),
            dst: termpix::geom::Rect::new(x, 0, 8, 8),
            z: 0,
        };
        let text = |s: &str| {
            let cell = Cell { text: s.into(), width: 1, ..Cell::default() };
            vec![TextGrid { col: 0, row: 1, rows: vec![vec![cell]] }]
        };
        let mut frame = Frame { layers: vec![layer(0)], text: text("a"), ..Default::default() };
        let mut screen = Screen::new();
        assert!(screen.render_text(&frame, &mut comp).is_none(), "the first frame writes everything");
        screen.render(&frame, &presenter, &mut comp).unwrap();
        // The image moved and the text changed: the text alone, no graphics.
        frame.layers = vec![layer(8)];
        frame.text = text("b");
        let alone = String::from_utf8_lossy(screen.render_text(&frame, &mut comp).unwrap()).into_owned();
        assert!(alone.contains('b') && !alone.contains("\x1b_G"), "{alone:?}");
        // Later the images catch up, without writing the text again.
        let rest = String::from_utf8_lossy(screen.render(&frame, &presenter, &mut comp).unwrap()).into_owned();
        assert!(rest.contains("\x1b_G") && !rest.contains('b'), "{rest:?}");
    }

    #[test]
    fn text_that_goes_is_blanked_before_the_images_and_wide_text_after_them() {
        // WezTerm draws an image over a wide character in both its halves, and
        // drops the wide characters (and the image under text) where it places
        // one: the screen's image is sent again when a float opens over text.
        let g = Geometry { cols: 10, rows: 5, cell: CellSize { width: 8, height: 16 } };
        let mut comp = Compositor::new(Backend::composited(), Medium::Direct { compress: false }, g);
        let mut images = Images::new();
        let mut presenter = Presenter::new();
        let key = images.add(1, 1, vec![255; 4]);
        presenter.sync(&mut comp, &mut images);
        let layer = |x| Layer {
            key: 1,
            image: key,
            src: termpix::geom::Rect::new(0, 0, 1, 1),
            dst: termpix::geom::Rect::new(x, 0, 8, 8),
            z: 0,
        };
        let wide = |t: &str| Cell { text: t.into(), width: 2, ..Cell::default() };
        let spacer = Cell { width: 0, ..Cell::default() };
        let row = vec![wide("い"), spacer.clone(), wide("ろ"), spacer];
        let mut frame = Frame {
            layers: vec![layer(0)],
            text: vec![TextGrid { col: 0, row: 0, rows: vec![row] }],
            ..Default::default()
        };
        let mut screen = Screen::new();
        screen.render(&frame, &presenter, &mut comp).unwrap();
        // A float over ろ: its cells lose their text, and the image changes.
        frame.layers = vec![layer(8)];
        frame.text[0].rows[0].truncate(2);
        let out = String::from_utf8_lossy(screen.render(&frame, &presenter, &mut comp).unwrap()).into_owned();
        let image = out.find("\x1b_G").expect("the image is sent again");
        let blank = out.find("\x1b[1;3H  ").expect("ろ blanked");
        let again = out.rfind('い').expect("い written again");
        assert!(blank < image && image < again, "{out:?}");
    }

    #[test]
    fn pointer_and_clipboard_sequences() {
        assert_eq!(pointer_sequence(Pointer::Link), b"\x1b]22;pointer\x1b\\");
        assert_eq!(pointer_sequence(Pointer::ResizeRows), b"\x1b]22;ns-resize\x1b\\");
        assert_eq!(pointer_sequence(Pointer::ResizeNesw), b"\x1b]22;nesw-resize\x1b\\");
        assert_eq!(copy_sequence("hi", false), b"\x1b]52;c;aGk=\x1b\\");
        assert!(copy_sequence("hi", true).starts_with(b"\x1bPtmux;"));
    }
}
