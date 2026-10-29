//! The window: shows the session's frames, acknowledges them, follows the
//! window's size, and carries out effects. Input comes in `input`.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use temzdf_protocol::{ClientMsg, ExitReason, VERSION};
use temzdf_scene::{Effect, Hints, Pointer};
use termpix_core::geom::Geometry;
use winit::application::ApplicationHandler;
use winit::cursor::CursorIcon;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::connection::{Connection, FromServer};
use crate::render::Renderer;
use crate::view::{Applied, View, exit_message};

/// The window's size before the system says otherwise (logical pixels).
const INITIAL_SIZE: (u32, u32) = (900, 600);

struct Gpu {
    window: Arc<dyn Window>,
    /// For a new surface when the old one is lost.
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    view: View,
    geometry: Geometry,
}

/// When frames are acknowledged: once drawn, or at once while the window
/// cannot show them (macOS: covered, minimised, on another Space; wgpu then
/// has no surface texture). The session sends the next frame only after the
/// acknowledgement, so a frame left waiting for a draw that never comes stops
/// the window for good.
#[derive(Default)]
struct Acks {
    /// The frame received last and not acknowledged yet.
    pending: Option<u64>,
    hidden: bool,
}

impl Acks {
    /// Frame `seq` came; returns it if it is to be acknowledged now.
    fn arrived(&mut self, seq: u64) -> Option<u64> {
        self.pending = Some(seq);
        if self.hidden { self.pending.take() } else { None }
    }

    /// A redraw ran, whether it could draw or not.
    fn redrawn(&mut self) -> Option<u64> {
        self.pending.take()
    }

    /// The window was hidden or shown again; returns a frame to acknowledge.
    fn set_hidden(&mut self, hidden: bool) -> Option<u64> {
        self.hidden = hidden;
        if hidden { self.pending.take() } else { None }
    }
}

/// A redraw that got no surface texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Skip {
    Timeout,
    Occluded,
    Outdated,
    Lost,
    /// A validation error (reported by wgpu's handler).
    Other,
}

/// What the surface needs before the next try.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rebuild {
    None,
    Configure,
    /// `Lost`: configuring the old surface does not help (wgpu).
    Recreate,
}

/// What a skipped redraw does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OnSkip {
    /// Acknowledge the frame waiting for this draw.
    ack: bool,
    rebuild: Rebuild,
    /// Redraw again after this long.
    retry: Option<Duration>,
}

/// Tries at an outdated or lost surface before the frame is let go.
const SURFACE_TRIES: u32 = 3;
/// How long a retry that could fail again waits: a timeout comes after
/// wgpu waited a second (a minimised window on X11 with FIFO), and back to
/// back they would leave input and the session's messages a turn a second.
const RETRY_AFTER: Duration = Duration::from_millis(100);

/// What to do about `skip`, the `failures`th in a row (from 1).
fn on_skip(skip: Skip, failures: u32) -> OnSkip {
    match skip {
        // No event follows a timeout: try again, a little later.
        Skip::Timeout => OnSkip { ack: true, rebuild: Rebuild::None, retry: Some(RETRY_AFTER) },
        // `Occluded(false)` redraws later.
        Skip::Occluded | Skip::Other => OnSkip { ack: true, rebuild: Rebuild::None, retry: None },
        Skip::Outdated | Skip::Lost => {
            let rebuild = if skip == Skip::Lost { Rebuild::Recreate } else { Rebuild::Configure };
            // Drawn after the rebuild; while that keeps failing, the session
            // must not wait for it, nor this loop spin.
            if failures < SURFACE_TRIES {
                OnSkip { ack: false, rebuild, retry: Some(Duration::ZERO) }
            } else {
                OnSkip { ack: true, rebuild, retry: Some(RETRY_AFTER) }
            }
        }
    }
}

pub struct App {
    name: String,
    conn: Connection,
    rx: Receiver<FromServer>,
    gpu: Option<Gpu>,
    acks: Acks,
    /// Redraws in a row that got no texture.
    skipped: u32,
    /// A redraw to try then (see `on_skip`).
    retry_at: Option<Instant>,
    pointer: Pointer,
    /// Why the window closes, for the message after.
    ended: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    pub input: crate::input::Input,
}

impl App {
    /// Why the window closes (the first reason counts).
    fn end(&self, why: String) {
        self.ended.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert(why);
    }

    fn send(&mut self, msg: ClientMsg) {
        if self.conn.send(&msg).is_err() {
            self.end(format!("[lost the connection to session {}]", self.name));
        }
    }

    fn start(&mut self, event_loop: &dyn ActiveEventLoop) -> Result<()> {
        let attrs = WindowAttributes::default()
            .with_title("temzdf")
            .with_surface_size(LogicalSize::new(INITIAL_SIZE.0, INITIAL_SIZE.1));
        let window: Arc<dyn Window> = Arc::from(event_loop.create_window(attrs)?);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(
            event_loop.owned_display_handle(),
        )));
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .context("no GPU adapter for the window")?;
        let (device, queue) = crate::render::device_for(&adapter)?;
        let caps = surface.get_capabilities(&adapter);
        // Blending on the stored values: a non-sRGB format (see render.rs).
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let size = window.surface_size();
        let limit = device.limits().max_texture_dimension_2d;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.clamp(1, limit),
            height: size.height.clamp(1, limit),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);
        let scale = window.scale_factor();
        let renderer = Renderer::new(device, queue, format, crate::fonts::system(), scale);
        let view = View::new(renderer, scale);
        let geometry = view.geometry((config.width, config.height));
        crate::input::enable_ime(window.as_ref());
        self.gpu = Some(Gpu { window, instance, surface, config, view, geometry });
        let hints = Hints { exact_scaling: true, fast_uploads: true };
        let shared_memory = temzdf_protocol::shared_memory_wanted();
        self.send(ClientMsg::Hello { version: VERSION, geometry, hints, inside: None, shared_memory });
        Ok(())
    }

    /// The window or the cell size changed: tell the session.
    fn refit(&mut self) {
        let Some(g) = &mut self.gpu else { return };
        let geometry = g.view.geometry((g.config.width, g.config.height));
        if geometry != g.geometry {
            g.geometry = geometry;
            self.send(ClientMsg::Resize(geometry));
        }
    }

    fn handle_server(&mut self, event_loop: &dyn ActiveEventLoop) {
        while let Ok(msg) = self.rx.try_recv() {
            let FromServer::Msg(msg) = msg else {
                self.end(format!("[lost the connection to session {}]", self.name));
                event_loop.exit();
                return;
            };
            let Some(g) = &mut self.gpu else { continue };
            match g.view.apply(msg) {
                Applied::Frame(seq, _) => {
                    // Drawn when the window is ready; acknowledged after.
                    g.window.request_redraw();
                    let ack = self.acks.arrived(seq);
                    self.ack(ack);
                }
                Applied::Appearance { resized } => {
                    g.window.request_redraw();
                    if resized {
                        self.refit();
                    }
                }
                Applied::Effect(e) => self.effect(e),
                Applied::Exit(reason) => {
                    self.end(exit_message(&reason, &self.name));
                    if matches!(reason, ExitReason::Error(_)) {
                        eprintln!("{}", exit_message(&reason, &self.name));
                    }
                    event_loop.exit();
                    return;
                }
                Applied::None => {}
            }
        }
    }

    fn effect(&mut self, e: Effect) {
        let conn = self.conn.clone();
        let report = move |m: String| {
            let _ = conn.send(&ClientMsg::Report(m));
        };
        match e {
            Effect::OpenUri { uri, command } => {
                let command = command.filter(|c| !c.is_empty()).unwrap_or_else(temzdf_client::default_opener);
                temzdf_client::open_uri(&command, &uri, report);
            }
            Effect::RunShell(cmd) => temzdf_client::run_shell(&cmd),
            Effect::Copy(text) => {
                if let Err(e) = self.input.clipboard_set(&text, false) {
                    report(format!("cannot copy: {e}"));
                }
            }
            Effect::Paste { primary } => match self.input.clipboard_get(primary) {
                Ok(text) => self.send(ClientMsg::Paste(text)),
                Err(e) => report(format!("cannot paste: {e}")),
            },
        }
    }

    fn ack(&mut self, seq: Option<u64>) {
        if let Some(seq) = seq {
            self.send(ClientMsg::FrameAck(seq));
        }
    }

    fn redraw(&mut self) {
        let Some(g) = &mut self.gpu else { return };
        let skip = match g.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => Ok(t),
            wgpu::CurrentSurfaceTexture::Outdated => Err(Skip::Outdated),
            wgpu::CurrentSurfaceTexture::Lost => Err(Skip::Lost),
            wgpu::CurrentSurfaceTexture::Timeout => Err(Skip::Timeout),
            wgpu::CurrentSurfaceTexture::Occluded => Err(Skip::Occluded),
            _ => Err(Skip::Other),
        };
        let texture = match skip {
            Ok(t) => t,
            Err(skip) => {
                self.skipped += 1;
                let what = on_skip(skip, self.skipped);
                match what.rebuild {
                    Rebuild::None => {}
                    Rebuild::Configure => g.surface.configure(g.view.renderer.device(), &g.config),
                    Rebuild::Recreate => match g.instance.create_surface(g.window.clone()) {
                        Ok(surface) => {
                            g.surface = surface;
                            g.surface.configure(g.view.renderer.device(), &g.config);
                        }
                        Err(e) => eprintln!("temzdf-gui: cannot make a new surface: {e}"),
                    },
                }
                match what.retry {
                    Some(Duration::ZERO) => g.window.request_redraw(),
                    Some(after) => self.retry_at = Some(Instant::now() + after),
                    None => {}
                }
                if what.ack {
                    let ack = self.acks.redrawn();
                    self.ack(ack);
                }
                return;
            }
        };
        self.skipped = 0;
        let target = texture.texture.create_view(&Default::default());
        let mut list = g.view.draw_list(g.geometry);
        if let Some((col, row)) = g.view.frame.as_ref().and_then(|(_, f)| f.cursor) {
            crate::drawlist::preedit(
                &mut list,
                &self.input.preedit,
                (col, row),
                crate::view::grid(g.geometry),
                &g.view.colors,
            );
        }
        g.view.renderer.draw(&list, crate::view::grid(g.geometry), (0, 0), (g.config.width, g.config.height), &target);
        g.window.pre_present_notify();
        g.view.renderer.queue().present(texture);
        if let Some((_, f)) = &g.view.frame {
            if f.title != g.window.title() {
                g.window.set_title(&f.title);
            }
            if f.pointer != self.pointer {
                self.pointer = f.pointer;
                g.window.set_cursor(cursor_icon(f.pointer).into());
            }
            self.input.set_cursor_cell(f.cursor, g.geometry, g.window.as_ref());
        }
        let ack = self.acks.redrawn();
        self.ack(ack);
    }
}

fn cursor_icon(p: Pointer) -> CursorIcon {
    match p {
        Pointer::Default => CursorIcon::Default,
        Pointer::Link => CursorIcon::Pointer,
        Pointer::ResizeColumns => CursorIcon::ColResize,
        Pointer::ResizeRows => CursorIcon::RowResize,
        Pointer::Move => CursorIcon::Move,
        Pointer::ResizeNwse => CursorIcon::NwseResize,
        Pointer::ResizeNesw => CursorIcon::NeswResize,
    }
}

impl ApplicationHandler for App {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        if let Err(e) = self.start(event_loop) {
            self.end(format!("cannot open the window: {e:#}"));
            event_loop.exit();
        }
    }

    fn proxy_wake_up(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.handle_server(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        match self.retry_at {
            Some(at) if Instant::now() >= at => {
                self.retry_at = None;
                if let Some(g) = &self.gpu {
                    g.window.request_redraw();
                }
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn window_event(&mut self, event_loop: &dyn ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // Closing the window detaches; the session goes on.
                self.send(ClientMsg::Detach);
                self.end(format!("[detached from session {}]", self.name));
                event_loop.exit();
            }
            // Minimised, where the system says so with no size (Windows):
            // the session keeps its size, its terminals their lines.
            WindowEvent::SurfaceResized(size) if size.width == 0 || size.height == 0 => {}
            WindowEvent::SurfaceResized(size) => {
                if let Some(g) = &mut self.gpu {
                    // Beyond the GPU's limit (a window across monitors):
                    // the surface stays at the limit.
                    let limit = g.view.renderer.device().limits().max_texture_dimension_2d;
                    g.config.width = size.width.clamp(1, limit);
                    g.config.height = size.height.clamp(1, limit);
                    g.surface.configure(g.view.renderer.device(), &g.config);
                    g.window.request_redraw();
                }
                self.refit();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(g) = &mut self.gpu {
                    g.view.set_scale(scale_factor);
                }
                self.refit();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Occluded(hidden) => {
                let ack = self.acks.set_hidden(hidden);
                self.ack(ack);
                // The frames that came meanwhile were not drawn.
                if let (false, Some(g)) = (hidden, &self.gpu) {
                    g.window.request_redraw();
                }
            }
            other => {
                let Some(g) = &self.gpu else { return };
                let msgs = self.input.window_event(&other, g.geometry, g.window.as_ref());
                for m in msgs {
                    self.send(m);
                }
            }
        }
    }
}

/// Show session `name` in a window until it is closed or the session ends;
/// returns a line to print after.
pub fn run(name: String) -> Result<String> {
    let event_loop = EventLoop::new()?;
    let proxy = event_loop.create_proxy();
    let (conn, rx) = Connection::open(&name, move || proxy.wake_up())?;
    let input = crate::input::Input::new();
    // The event loop owns the app; the closing message comes back here.
    let ended = std::sync::Arc::new(std::sync::Mutex::new(None));
    let app = App {
        name,
        conn,
        rx,
        gpu: None,
        acks: Acks::default(),
        skipped: 0,
        retry_at: None,
        pointer: Pointer::Default,
        ended: ended.clone(),
        input,
    };
    event_loop.run_app(app)?;
    let line = ended.lock().unwrap_or_else(|p| p.into_inner()).take();
    Ok(line.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::{Acks, OnSkip, RETRY_AFTER, Rebuild, SURFACE_TRIES, Skip, on_skip};
    use std::time::Duration;

    #[test]
    fn skipped_redraws_acknowledge_and_retry_as_needed() {
        let later = Some(RETRY_AFTER);
        assert_eq!(on_skip(Skip::Timeout, 1), OnSkip { ack: true, rebuild: Rebuild::None, retry: later });
        assert_eq!(on_skip(Skip::Occluded, 1), OnSkip { ack: true, rebuild: Rebuild::None, retry: None });
        // Drawn after reconfiguring (a new surface when lost)...
        let now = Some(Duration::ZERO);
        assert_eq!(on_skip(Skip::Outdated, 1), OnSkip { ack: false, rebuild: Rebuild::Configure, retry: now });
        assert_eq!(on_skip(Skip::Lost, 1), OnSkip { ack: false, rebuild: Rebuild::Recreate, retry: now });
        // ...but not waited for for ever, nor tried as fast as it fails.
        let last = on_skip(Skip::Lost, SURFACE_TRIES);
        assert_eq!((last.ack, last.retry), (true, later));
    }

    #[test]
    fn frames_are_acknowledged_once_drawn_or_at_once_while_hidden() {
        let mut acks = Acks::default();
        assert_eq!(acks.arrived(1), None, "a shown window draws first");
        assert_eq!(acks.redrawn(), Some(1));
        assert_eq!(acks.redrawn(), None, "acknowledged once");
        // Hidden with a frame waiting for its draw: it is let go.
        assert_eq!(acks.arrived(2), None);
        assert_eq!(acks.set_hidden(true), Some(2));
        // Hidden: no draw comes, the session must not wait for one.
        assert_eq!(acks.arrived(3), Some(3));
        assert_eq!(acks.arrived(4), Some(4));
        assert_eq!(acks.set_hidden(false), None);
        assert_eq!(acks.arrived(5), None);
        assert_eq!(acks.redrawn(), Some(5));
    }
}
