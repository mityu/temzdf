//! Smallest end-to-end use of termpix: a tall generated image scrolled with the
//! wheel (kinetic), by dragging (pixel-precise), or with keys.
//!
//! Keys: j/k or arrows scroll, space/b page, g/G top/bottom, q or Esc quit.
//! `TERMPIX_BACKEND` / `TERMPIX_MEDIUM` override detection (see `termpix::probe`).

use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use termpix::compositor::{Compositor, Layer};
use termpix::geom::{Geometry, Rect};
use termpix::input::{CoordUnit, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind, ScrollDirection};
use termpix::scroll::KineticScroll;
use termpix::tty::{self, Modes, TermEvent, Tty};

const IMAGE_HEIGHT: u32 = 6000;
const FRAME: Duration = Duration::from_millis(16);

fn main() -> io::Result<()> {
    let mut tty = Tty::open(Modes::default())?;
    let caps = termpix::probe::probe(&mut tty, Duration::from_secs(1))?;
    if !caps.kitty_graphics {
        drop(tty);
        eprintln!("this terminal does not support the kitty graphics protocol");
        std::process::exit(1);
    }
    if caps.pixel_mouse {
        tty.enable_pixel_mouse()?;
    }
    let mut geometry = caps.geometry;
    let mut comp = Compositor::new(caps.backend(), caps.medium(), geometry);

    let width = (geometry.width_px() * 4 / 5).max(64);
    let image = comp.add_image(width, IMAGE_HEIGHT, make_image(width, IMAGE_HEIGHT));
    let mut scroll = KineticScroll::default();
    let set_bounds = |scroll: &mut KineticScroll, g: &Geometry| {
        scroll.set_bounds(0.0, IMAGE_HEIGHT.saturating_sub(g.height_px()) as f64)
    };
    set_bounds(&mut scroll, &geometry);

    let (tx, rx) = mpsc::channel();
    tty::spawn_input(tty.reader()?, caps.pixel_mouse, tx, |e| e)?;

    let mut drag: Option<(f64, f64)> = None;
    let mut dirty = true;
    let mut out = Vec::new();
    loop {
        if dirty {
            let offset = scroll.offset().round() as i32;
            let x = (geometry.width_px() - width) as i32 / 2;
            let layer = Layer {
                key: 1,
                image,
                src: Rect::new(0, offset, width, geometry.height_px().min(IMAGE_HEIGHT)),
                dst: Rect::new(x, 0, width, geometry.height_px().min(IMAGE_HEIGHT)),
                z: -1_073_741_825,
            };
            out.clear();
            comp.present(&[layer], &mut out)?;
            tty.write_all(&out)?;
            dirty = false;
        }
        let timeout = if scroll.is_animating() { FRAME } else { Duration::from_secs(3600) };
        let event = match rx.recv_timeout(timeout) {
            Ok(e) => Some(e),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let now = Instant::now();
        let page = geometry.height_px() as f64 * 0.9;
        match event {
            Some(TermEvent::Terminate) => break,
            Some(TermEvent::WindowChanged) | Some(TermEvent::Input(Event::Resize(_))) => {
                if let Some(g) = tty.winsize()?.geometry() {
                    geometry = g;
                    comp.set_geometry(g);
                    set_bounds(&mut scroll, &geometry);
                    dirty = true;
                }
            }
            Some(TermEvent::Input(Event::Graphics(reply))) => dirty |= comp.handle_reply(&reply),
            Some(TermEvent::Input(Event::Key(k))) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Char('q') | KeyCode::Escape => break,
                KeyCode::Char('j') | KeyCode::Down => scroll.wheel(1.0, now),
                KeyCode::Char('k') | KeyCode::Up => scroll.wheel(-1.0, now),
                KeyCode::Char(' ') | KeyCode::PageDown => scroll.scroll_by(page, now),
                KeyCode::Char('b') | KeyCode::PageUp => scroll.scroll_by(-page, now),
                KeyCode::Char('g') | KeyCode::Home => scroll.scroll_by(-f64::INFINITY, now),
                KeyCode::Char('G') | KeyCode::End => scroll.scroll_by(f64::INFINITY, now),
                _ => {}
            },
            Some(TermEvent::Input(Event::Mouse(m))) => {
                let to_px = |v: i32, cell: u32| match m.unit {
                    CoordUnit::Pixel => v as f64,
                    CoordUnit::Cell => (v as f64 + 0.5) * cell as f64,
                };
                let y = to_px(m.y, geometry.cell.height);
                match m.kind {
                    MouseEventKind::Scroll(ScrollDirection::Down) => scroll.wheel(1.0, now),
                    MouseEventKind::Scroll(ScrollDirection::Up) => scroll.wheel(-1.0, now),
                    MouseEventKind::Press(MouseButton::Left) => drag = Some((y, scroll.offset())),
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some((y0, offset0)) = drag {
                            scroll.jump_to(offset0 - (y - y0));
                            dirty = true;
                        }
                    }
                    MouseEventKind::Release(_) => drag = None,
                    _ => {}
                }
            }
            _ => {}
        }
        dirty |= scroll.tick(Instant::now());
    }
    Ok(())
}

/// Horizontal bands with a 100 px grid, so scrolling is easy to judge by eye.
fn make_image(width: u32, height: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let band = (y / 500) as f32;
        let hue = (band * 0.13).fract();
        let (r, g, b) = hsv(hue, 0.35, 0.95);
        for x in 0..width {
            let px = if y % 1000 < 8 {
                [20, 20, 20, 255]
            } else if y % 100 < 2 || x % 100 < 2 {
                [90, 90, 90, 255]
            } else {
                [r, g, b, 255]
            };
            data.extend_from_slice(&px);
        }
    }
    data
}

fn hsv(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}
