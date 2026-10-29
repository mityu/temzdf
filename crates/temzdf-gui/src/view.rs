//! What a GUI client knows of its session and shows: the latest frame, the
//! images as textures, the appearance; shared by the window and
//! `--render-frame`.

use temzdf_protocol::{ExitReason, ServerMsg};
use temzdf_scene::{Appearance, Effect, Frame, ImageUpdate};
use termpix_core::geom::{CellSize, Geometry};

use crate::drawlist::{self, DrawList, Grid};
use crate::palette::Colors;
use crate::render::Renderer;

/// What a server message changed.
pub enum Applied {
    /// A frame to show (its number, whether every page in it is rendered).
    Frame(u64, bool),
    /// The look changed; the cell size too when `resized`.
    Appearance {
        resized: bool,
    },
    Effect(Effect),
    Exit(ExitReason),
    None,
}

pub struct View {
    pub renderer: Renderer,
    pub colors: Colors,
    appearance: Appearance,
    pub frame: Option<(u64, Frame)>,
    scale: f64,
}

impl View {
    pub fn new(renderer: Renderer, scale: f64) -> Self {
        Self { renderer, colors: Colors::default(), appearance: Appearance::default(), frame: None, scale }
    }

    /// The cell grid fitting `size` pixels.
    pub fn geometry(&self, size: (u32, u32)) -> Geometry {
        let m = self.renderer.metrics();
        Geometry {
            cols: (size.0 / m.width).clamp(1, u16::MAX as u32) as u16,
            rows: (size.1 / m.height).clamp(1, u16::MAX as u32) as u16,
            cell: CellSize { width: m.width, height: m.height },
        }
    }

    pub fn set_scale(&mut self, scale: f64) {
        self.scale = scale;
        self.renderer.set_font(&self.appearance.font, &self.appearance.font_wide, scale);
    }

    pub fn apply(&mut self, msg: ServerMsg) -> Applied {
        match msg {
            ServerMsg::Frame { seq, updates, frame, complete, .. } => {
                let start = std::time::Instant::now();
                let uploads = updates.len();
                for u in updates {
                    match u {
                        ImageUpdate::Set(k, image) => self.renderer.set_image(k, &image),
                        ImageUpdate::Remove(k) => self.renderer.remove_image(k),
                    }
                }
                if uploads > 0 && temzdf_protocol::timing() {
                    let ms = temzdf_protocol::ms(start.elapsed());
                    eprintln!("timing: uploaded the images of frame {seq} in {ms:.2} ms");
                }
                self.frame = Some((seq, frame));
                Applied::Frame(seq, complete)
            }
            ServerMsg::Appearance(a) => {
                let font_changed = (&a.font, &a.font_wide) != (&self.appearance.font, &self.appearance.font_wide);
                self.colors = Colors::new(&a);
                self.appearance = a;
                let before = self.renderer.metrics();
                if font_changed {
                    self.renderer.set_font(&self.appearance.font, &self.appearance.font_wide, self.scale);
                }
                Applied::Appearance { resized: self.renderer.metrics() != before }
            }
            ServerMsg::Effect(e) => Applied::Effect(e),
            ServerMsg::Exit(reason) => Applied::Exit(reason),
            _ => Applied::None,
        }
    }

    /// The draw list of the latest frame on `geometry`'s grid.
    pub fn draw_list(&self, geometry: Geometry) -> DrawList {
        let grid = grid(geometry);
        match &self.frame {
            Some((_, f)) => drawlist::build(f, grid, &self.colors),
            None => DrawList { background: self.colors.background, ..DrawList::default() },
        }
    }
}

pub fn grid(g: Geometry) -> Grid {
    Grid { cols: g.cols, rows: g.rows, cell_w: g.cell.width, cell_h: g.cell.height }
}

/// What to tell the user when the session ends the connection.
pub fn exit_message(reason: &ExitReason, name: &str) -> String {
    match reason {
        ExitReason::Detached => format!("[detached from session {name}]"),
        ExitReason::SessionEnded => format!("[session {name} ended]"),
        ExitReason::Killed => format!("[session {name} was killed]"),
        ExitReason::Error(e) => format!("[session {name}: {e}]"),
    }
}
