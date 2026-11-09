//! Images text panes show (decorations, doc/design/text-editor.md section
//! 7): PNG files read into the session's image store, by id (the store's
//! key). Reading the same file again replaces the image under its id, so
//! what shows it is redrawn with the new one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use temzdf_protocol::script::ScriptValue;
use temzdf_scene::{ImageKey, Layer, Rect, TextGrid};
use temzdf_texteditor::DrawnImage;
use termpix_core::geom::CellSize;

use super::{Outcome, Session};

/// An image read: its key in the store, its file and size.
pub(super) struct Loaded {
    pub key: ImageKey,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

pub(super) type LoadedImages = HashMap<u64, Loaded>;

/// The commands of images.
pub(super) const COMMANDS: &[(&str, &str, &str)] = &[
    (
        "load-image",
        "(path: string): number",
        "Read a PNG for decorations to show (virtImage); the same file again replaces it under the same id. Returns its id",
    ),
    ("unload-image", "(id: number): void", "Drop an image: decorations showing it show their alt text instead"),
];

impl Session {
    pub(super) fn image_command(&mut self, name: &str, args: &[ScriptValue]) -> Result<(ScriptValue, Outcome), String> {
        match name {
            "load-image" => {
                let path = args.first().and_then(ScriptValue::as_str).ok_or("which file?")?;
                let id = self.load_image(Path::new(path))?;
                Ok((ScriptValue::Num(id as f64), Outcome::Redraw))
            }
            _ => {
                let id = args.first().and_then(ScriptValue::as_num).ok_or("which image?")? as u64;
                let loaded = self.loaded_images.remove(&id).ok_or_else(|| format!("no image {id}"))?;
                self.images.remove(loaded.key);
                for t in self.texts.values_mut() {
                    t.buf.marks_mut().set_image(id, None);
                }
                Ok((ScriptValue::Null, Outcome::Redraw))
            }
        }
    }

    /// Read the PNG at `path` (again: replaced under its id).
    fn load_image(&mut self, path: &Path) -> Result<u64, String> {
        let shown = path.display();
        let canonical = std::fs::canonicalize(path).map_err(|e| format!("{shown}: {e}"))?;
        let bytes = std::fs::read(&canonical).map_err(|e| format!("{shown}: {e}"))?;
        let (width, height, rgba) = temzdf_term::decode_png(&bytes).ok_or_else(|| format!("{shown}: not a PNG"))?;
        if let Some((&id, l)) = self.loaded_images.iter_mut().find(|(_, l)| l.path == canonical) {
            self.images.replace(l.key, width, height, rgba);
            (l.width, l.height) = (width, height);
            for t in self.texts.values_mut() {
                t.buf.marks_mut().set_image(id, Some((width, height)));
            }
            return Ok(id);
        }
        let key = self.images.add(width, height, rgba);
        self.loaded_images.insert(key.raw(), Loaded { key, path: canonical, width, height });
        Ok(key.raw())
    }
}

/// A drawn view's images as layers (pane pixels, over the text), the cells
/// they take (holes in the text) and their alt text (for frontends that
/// draw no images).
pub(super) fn image_layers(
    drawn: &[DrawnImage],
    loaded: &LoadedImages,
    cell: CellSize,
) -> (Vec<Layer>, Vec<termpix_core::geom::Rect>, Vec<TextGrid>) {
    let (cw, ch) = (cell.width, cell.height);
    let mut layers = Vec::new();
    let mut holes = Vec::new();
    let mut alt = Vec::new();
    for d in drawn {
        let Some(l) = loaded.get(&d.id) else { continue };
        let total = u32::from(d.total.max(1));
        let (bw, bh) = (u32::from(d.cols) * cw, total * ch);
        // As tall as its cells, as wide as its aspect asks (within them).
        let w =
            if l.height == 0 { bw } else { (u64::from(l.width) * u64::from(bh) / u64::from(l.height)) as u32 }.min(bw);
        // The part in the rows shown.
        let part = |r: u32| (u64::from(l.height) * u64::from(r) / u64::from(total)) as u32;
        let (sy, sh) = (part(u32::from(d.skip)), part(u32::from(d.skip + d.rows)) - part(u32::from(d.skip)));
        layers.push(Layer {
            key: (u32::from(d.row) << 16) | u32::from(d.col),
            image: l.key,
            src: Rect::new(0, sy as i32, l.width, sh.max(1)),
            dst: Rect::new(
                i32::from(d.col) * cw as i32,
                i32::from(d.row) * ch as i32,
                w.max(1),
                u32::from(d.rows) * ch,
            ),
            z: 1,
        });
        holes.push(termpix_core::geom::Rect::new(
            i32::from(d.col),
            i32::from(d.row),
            u32::from(d.cols),
            u32::from(d.rows),
        ));
        let mut rows = Vec::new();
        for r in 0..d.rows {
            let text: String =
                if r == 0 && d.skip == 0 { d.alt.chars().take(usize::from(d.cols)).collect() } else { String::new() };
            let mut row: Vec<temzdf_scene::Cell> = text
                .chars()
                .map(|c| temzdf_scene::Cell { text: c.to_string(), ..temzdf_scene::Cell::blank() })
                .collect();
            row.resize_with(usize::from(d.cols), temzdf_scene::Cell::blank);
            rows.push(row);
        }
        alt.push(TextGrid { col: d.col, row: d.row, rows });
    }
    (layers, holes, alt)
}
