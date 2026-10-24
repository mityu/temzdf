//! Interactive mode: terminal setup, the event loop and input bindings.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use termpix::compositor::Compositor;
use termpix::input::{
    CoordUnit, Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
    ScrollDirection,
};
use termpix::tty::{self, Modes, TermEvent, Tty};

use crate::viewer::{Need, Viewer};
use crate::workers::{self, AppEvent, DocCmd, RasterPool};

const FRAME: Duration = Duration::from_millis(16);
const ZOOM_STEP: f32 = 1.1;

pub struct Options {
    /// 0-based page to show first.
    pub page: Option<usize>,
    pub scale: Option<f32>,
}

pub fn run(path: &Path, opts: Options) -> Result<()> {
    let mut tty = Tty::open(Modes::default())?;
    let caps = termpix::probe::probe(&mut tty, Duration::from_secs(1))?;
    if !caps.kitty_graphics {
        drop(tty);
        bail!("this terminal does not support the kitty graphics protocol");
    }
    if caps.pixel_mouse {
        tty.enable_pixel_mouse()?;
    }
    let mut geometry = caps.geometry;
    let mut comp = Compositor::new(caps.backend(), caps.medium(), geometry);

    let (tx, rx) = mpsc::channel::<AppEvent>();
    tty::spawn_input(tty.reader()?, caps.pixel_mouse, tx.clone(), AppEvent::Term)?;
    let doc = workers::spawn_document(path.to_owned(), tx.clone())?;
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4));
    let pool = RasterPool::new(threads, tx.clone())?;

    // Wait for the document before drawing anything.
    let bounds = loop {
        match rx.recv()? {
            AppEvent::Opened(Ok(b)) => break b,
            AppEvent::Opened(Err(e)) => {
                drop(tty);
                bail!("{e}");
            }
            AppEvent::Term(TermEvent::Terminate) => return Ok(()),
            _ => {}
        }
    };
    if bounds.is_empty() {
        drop(tty);
        bail!("the document has no pages");
    }
    let mut viewer = Viewer::new(bounds, geometry);
    if let Some(scale) = opts.scale {
        viewer.set_scale(scale, (0.0, 0.0));
    }
    if let Some(page) = opts.page {
        viewer.goto_page(page, false, Instant::now());
    }

    let mut state = Input::default();
    let mut dirty = true;
    let mut out = Vec::new();
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let mut title = Title::default();
    loop {
        if dirty {
            let (layers, needs) = viewer.frame(&mut comp);
            dispatch(&mut viewer, &doc, &pool, needs);
            out.clear();
            title.update(&mut out, &name, viewer.current_page() + 1, viewer.page_count());
            comp.present(&layers, &mut out)?;
            if !out.is_empty() {
                tty.write_all(&out)?;
            }
            dirty = false;
        }
        let timeout = if viewer.is_animating() { FRAME } else { Duration::from_secs(3600) };
        let event = match rx.recv_timeout(timeout) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let now = Instant::now();
        match event {
            None => {}
            Some(AppEvent::Term(TermEvent::Terminate)) => break,
            Some(AppEvent::Term(TermEvent::WindowChanged))
            | Some(AppEvent::Term(TermEvent::Input(Event::Resize(_)))) => {
                if let Some(g) = tty.winsize()?.geometry()
                    && g != geometry
                {
                    geometry = g;
                    comp.set_geometry(g);
                    viewer.resize(g);
                    dirty = true;
                }
            }
            Some(AppEvent::Term(TermEvent::Input(Event::Graphics(reply)))) => dirty |= comp.handle_reply(&reply),
            Some(AppEvent::Term(TermEvent::Input(Event::Key(key)))) => match state.key(&mut viewer, &key, now) {
                Action::Quit => break,
                Action::Redraw => dirty = true,
                Action::None => {}
            },
            Some(AppEvent::Term(TermEvent::Input(Event::Mouse(m)))) => {
                dirty |= state.mouse(&mut viewer, &m, geometry.cell.width, geometry.cell.height, now)
            }
            Some(AppEvent::Term(_)) => {}
            Some(AppEvent::Opened(_)) => {}
            Some(AppEvent::PageLoaded(p, list)) => {
                viewer.page_loaded(p, list);
                dirty = true;
            }
            Some(AppEvent::PageFailed(p, e)) => {
                viewer.page_failed(p);
                title.error = Some(format!("page {}: {e}", p + 1));
                dirty = true;
            }
            Some(AppEvent::Rendered(r)) => {
                viewer.rendered(&mut comp, r);
                dirty = true;
            }
            Some(AppEvent::RenderFailed(key, e)) => {
                viewer.render_cancelled(key);
                title.error = Some(format!("render: {e}"));
                dirty = true;
            }
        }
        dirty |= viewer.tick(Instant::now());
    }
    Ok(())
}

fn dispatch(viewer: &mut Viewer, doc: &Sender<DocCmd>, pool: &RasterPool, needs: Vec<Need>) {
    let mut jobs = Vec::new();
    for need in needs {
        viewer.mark_requested(&need);
        match need {
            Need::LoadPage(p) => {
                let _ = doc.send(DocCmd::LoadPage(p));
            }
            Need::Render(job) => jobs.push(job),
        }
    }
    for key in pool.replace(jobs) {
        viewer.render_cancelled(key);
    }
}

/// Window title (OSC 2) as a stand-in status line until the UI kit lands.
#[derive(Default)]
struct Title {
    shown: String,
    error: Option<String>,
}

impl Title {
    fn update(&mut self, out: &mut Vec<u8>, name: &str, page: usize, pages: usize) {
        let mut text = format!("{name} — {page}/{pages}");
        if let Some(e) = &self.error {
            text.push_str(" — ");
            text.push_str(e);
        }
        if text != self.shown {
            // Control characters would end the OSC early.
            let clean: String = text.chars().filter(|c| !c.is_control()).collect();
            out.extend_from_slice(format!("\x1b]2;{clean}\x1b\\").as_bytes());
            self.shown = text;
        }
    }
}

enum Action {
    None,
    Redraw,
    Quit,
}

#[derive(Default)]
struct Input {
    /// Numeric prefix typed so far (vim-style counts, e.g. `12G`).
    count: Option<usize>,
    /// Pointer position in screen pixels.
    pointer: (f32, f32),
    /// Drag start: pointer and scroll position.
    drag: Option<((f32, f32), (f32, f32))>,
}

impl Input {
    fn key(&mut self, v: &mut Viewer, k: &KeyEvent, now: Instant) -> Action {
        if k.kind == KeyEventKind::Release || matches!(k.code, KeyCode::Modifier(_)) {
            return Action::None;
        }
        let mods = k.modifiers.without_locks();
        let ctrl = mods.contains(Modifiers::CTRL);
        let page = v.view.screen_h as f64 * 0.9;
        let half = v.view.screen_h as f64 * 0.5;
        let count = self.count.take();
        let centre = (v.view.screen_w / 2.0, v.view.screen_h / 2.0);
        match k.code {
            KeyCode::Char(c @ '0'..='9') if !ctrl && (c != '0' || count.is_some()) => {
                self.count = Some(count.unwrap_or(0) * 10 + (c as usize - '0' as usize));
                return Action::None;
            }
            KeyCode::Char('q') | KeyCode::Escape => return Action::Quit,
            KeyCode::Char('c') if ctrl => return Action::Quit,
            KeyCode::Char('j') | KeyCode::Down => wheel_n(v, 1.0, count, now),
            KeyCode::Char('k') | KeyCode::Up => wheel_n(v, -1.0, count, now),
            KeyCode::Char('l') | KeyCode::Right => v.scroll_x.wheel(1.0, now),
            KeyCode::Char('h') | KeyCode::Left => v.scroll_x.wheel(-1.0, now),
            KeyCode::Char(' ') | KeyCode::PageDown => v.scroll_y.scroll_by(page, now),
            KeyCode::Char('f') if ctrl => v.scroll_y.scroll_by(page, now),
            KeyCode::Char('b') | KeyCode::PageUp => v.scroll_y.scroll_by(-page, now),
            KeyCode::Char('d') => v.scroll_y.scroll_by(half, now),
            KeyCode::Char('u') => v.scroll_y.scroll_by(-half, now),
            KeyCode::Char('g') | KeyCode::Home => v.goto_page(count.map_or(0, |c| c.saturating_sub(1)), true, now),
            KeyCode::Char('G') | KeyCode::End => match count {
                Some(c) => v.goto_page(c.saturating_sub(1), true, now),
                None => v.scroll_y.scroll_by(f64::INFINITY, now),
            },
            KeyCode::Char('+' | '=') => v.zoom(ZOOM_STEP, centre),
            KeyCode::Char('-') => v.zoom(1.0 / ZOOM_STEP, centre),
            KeyCode::Char('0') | KeyCode::Char('w') => v.fit_width(),
            _ => return Action::None,
        }
        Action::Redraw
    }

    fn mouse(&mut self, v: &mut Viewer, m: &MouseEvent, cell_w: u32, cell_h: u32, now: Instant) -> bool {
        let to_px = |c: i32, size: u32| match m.unit {
            CoordUnit::Pixel => c as f32,
            CoordUnit::Cell => (c as f32 + 0.5) * size as f32,
        };
        self.pointer = (to_px(m.x, cell_w), to_px(m.y, cell_h));
        let mods = m.modifiers;
        match m.kind {
            MouseEventKind::Scroll(dir) if mods.contains(Modifiers::CTRL) => {
                let f = match dir {
                    ScrollDirection::Up => ZOOM_STEP,
                    ScrollDirection::Down => 1.0 / ZOOM_STEP,
                    _ => return false,
                };
                v.zoom(f, self.pointer);
            }
            MouseEventKind::Scroll(dir) => {
                let shift = mods.contains(Modifiers::SHIFT);
                match (dir, shift) {
                    (ScrollDirection::Down, false) => v.scroll_y.wheel(1.0, now),
                    (ScrollDirection::Up, false) => v.scroll_y.wheel(-1.0, now),
                    (ScrollDirection::Right, _) | (ScrollDirection::Down, true) => v.scroll_x.wheel(1.0, now),
                    (ScrollDirection::Left, _) | (ScrollDirection::Up, true) => v.scroll_x.wheel(-1.0, now),
                }
            }
            MouseEventKind::Press(MouseButton::Left) => {
                self.drag = Some((self.pointer, (v.view.scroll_x, v.view.scroll_y)));
                return false;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some((p0, s0)) = self.drag else { return false };
                v.jump_to(s0.0 - (self.pointer.0 - p0.0), s0.1 - (self.pointer.1 - p0.1));
            }
            MouseEventKind::Release(_) => {
                self.drag = None;
                return false;
            }
            _ => return false,
        }
        true
    }
}

fn wheel_n(v: &mut Viewer, dir: f64, count: Option<usize>, now: Instant) {
    for _ in 0..count.unwrap_or(1).min(100) {
        v.scroll_y.wheel(dir, now);
    }
}
