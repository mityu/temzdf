//! `--render-frame`: attach to a session as the window would, draw its
//! frame offscreen and save it as a PNG (to compare the window against, and
//! to look at without a display).

use std::path::Path;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glyphon::cosmic_text::FontSystem;
use temzdf_protocol::{ClientMsg, VERSION};
use temzdf_scene::Hints;
use termpix_core::raster::RgbaImage;

use crate::connection::{Connection, FromServer};
use crate::fonts::FontSpec;
use crate::render::{Renderer, offscreen_device};
use crate::view::{Applied, View, grid};

/// How long the frame may keep changing after it was first complete
/// (newer frames replace it: the resize to this geometry, late tiles).
const SETTLE: Duration = Duration::from_millis(500);
/// Longest wait for a complete frame.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Render session `name` on a `size` (pixels) window at `scale` into `out`.
pub fn render_frame(name: &str, out: &Path, size: (u32, u32), scale: f64) -> Result<()> {
    let (device, queue) = offscreen_device()?;
    let font = FontSpec::parse(&temzdf_scene::Appearance::default().font);
    let renderer = Renderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm, FontSystem::new(), font, scale);
    let mut view = View::new(renderer, scale);
    let (conn, rx) = Connection::open(name, || {})?;
    let mut geometry = view.geometry(size);
    conn.send(&ClientMsg::Hello {
        version: VERSION,
        geometry,
        hints: Hints { exact_scaling: true, fast_uploads: true },
    })?;
    let deadline = Instant::now() + TIMEOUT;
    // When the latest frame was complete: settle from there.
    let mut complete_at: Option<Instant> = None;
    loop {
        let wait = match complete_at {
            Some(t) => (t + SETTLE).saturating_duration_since(Instant::now()),
            None => deadline.saturating_duration_since(Instant::now()),
        };
        let msg = match rx.recv_timeout(wait) {
            Ok(FromServer::Msg(m)) => m,
            Ok(FromServer::Gone) => bail!("the session closed the connection"),
            Err(RecvTimeoutError::Timeout) if complete_at.is_some() => break,
            Err(_) => bail!("no complete frame from session {name:?} within {} s", TIMEOUT.as_secs()),
        };
        match view.apply(msg) {
            Applied::Frame(seq, complete) => {
                conn.send(&ClientMsg::FrameAck(seq))?;
                complete_at = complete.then(Instant::now);
            }
            Applied::Appearance { resized: true } => {
                geometry = view.geometry(size);
                conn.send(&ClientMsg::Resize(geometry))?;
                complete_at = None;
            }
            Applied::Exit(reason) => bail!("{}", crate::view::exit_message(&reason, name)),
            _ => {}
        }
    }
    let list = view.draw_list(geometry);
    let pixels = view.renderer.draw_offscreen(&list, grid(geometry), size)?;
    let _ = conn.send(&ClientMsg::Detach);
    RgbaImage { width: size.0, height: size.1, data: pixels }
        .save_png(out)
        .with_context(|| format!("writing {}", out.display()))
}
