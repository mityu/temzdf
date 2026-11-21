//! `--render-frame`: attach to a session as the window would, draw its
//! frame offscreen and save it as a PNG (to compare the window against, and
//! to look at without a display).

use std::path::Path;

use anyhow::{Context, Result};
use temzdf_client::{Connection, Settling};
use temzdf_protocol::ClientMsg;
use termpix_core::raster::RgbaImage;

use crate::render::{Renderer, offscreen_device};
use crate::view::{Applied, View, grid};

/// Render session `name` on a `size` (pixels) window at `scale` into `out`.
pub fn render_frame(name: &str, out: &Path, size: (u32, u32), scale: f64) -> Result<()> {
    let (device, queue) = offscreen_device()?;
    let renderer = Renderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm, crate::fonts::system(), scale);
    let mut view = View::new(renderer, scale);
    let (tx, rx) = std::sync::mpsc::channel();
    let conn = Connection::open(name, tx, std::convert::identity, || {})?;
    let mut geometry = view.geometry(size);
    conn.hello(geometry, crate::connection::HINTS, None)?;
    temzdf_client::settle(name, &rx, |msg| {
        Ok(match view.apply(msg) {
            Applied::Frame(seq, complete) => {
                conn.send(&ClientMsg::FrameAck(seq))?;
                Settling::Frame { complete }
            }
            Applied::Appearance { resized: true } => {
                geometry = view.geometry(size);
                conn.send(&ClientMsg::Resize(geometry))?;
                Settling::Unsettled
            }
            _ => Settling::Other,
        })
    })?;
    let list = view.draw_list(geometry);
    let pixels = view.renderer.draw_offscreen(&list, grid(geometry), size)?;
    let _ = conn.send(&ClientMsg::Detach);
    RgbaImage { width: size.0, height: size.1, data: pixels }
        .save_png(out)
        .with_context(|| format!("writing {}", out.display()))
}
