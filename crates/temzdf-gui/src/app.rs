//! The window: shows the session's frames, acknowledges them, follows the
//! window's size, and carries out effects. Input comes in `input`.

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use anyhow::{Context, Result};
use glyphon::cosmic_text::FontSystem;
use temzdf_protocol::{ClientMsg, ExitReason, VERSION};
use temzdf_scene::{Effect, Hints, Pointer};
use termpix_core::geom::Geometry;
use winit::application::ApplicationHandler;
use winit::cursor::CursorIcon;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::connection::{Connection, FromServer};
use crate::fonts::FontSpec;
use crate::render::Renderer;
use crate::view::{Applied, View, exit_message};

/// The window's size before the system says otherwise (logical pixels).
const INITIAL_SIZE: (u32, u32) = (900, 600);

struct Gpu {
    window: Arc<dyn Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    view: View,
    geometry: Geometry,
}

pub struct App {
    name: String,
    conn: Connection,
    rx: Receiver<FromServer>,
    gpu: Option<Gpu>,
    /// The frame drawn last and not acknowledged yet.
    unacked: Option<u64>,
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
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        let caps = surface.get_capabilities(&adapter);
        // Blending on the stored values: a non-sRGB format (see render.rs).
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let size = window.surface_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);
        let scale = window.scale_factor();
        let font = FontSpec::parse(&temzdf_scene::Appearance::default().font);
        let renderer = Renderer::new(device, queue, format, FontSystem::new(), font, scale);
        let view = View::new(renderer, scale);
        let geometry = view.geometry((config.width, config.height));
        crate::input::enable_ime(window.as_ref());
        self.gpu = Some(Gpu { window, surface, config, view, geometry });
        let hints = Hints { exact_scaling: true, fast_uploads: true };
        self.send(ClientMsg::Hello { version: VERSION, geometry, hints });
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
                    self.unacked = Some(seq);
                    g.window.request_redraw();
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

    fn redraw(&mut self) {
        let Some(g) = &mut self.gpu else { return };
        let texture = match g.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                g.surface.configure(g.view.renderer.device(), &g.config);
                g.window.request_redraw();
                return;
            }
            _ => return,
        };
        let target = texture.texture.create_view(&Default::default());
        let list = g.view.draw_list(g.geometry);
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
        if let Some(seq) = self.unacked.take() {
            self.send(ClientMsg::FrameAck(seq));
        }
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

    fn window_event(&mut self, event_loop: &dyn ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // Closing the window detaches; the session goes on.
                self.send(ClientMsg::Detach);
                self.end(format!("[detached from session {}]", self.name));
                event_loop.exit();
            }
            WindowEvent::SurfaceResized(size) => {
                if let Some(g) = &mut self.gpu {
                    g.config.width = size.width.max(1);
                    g.config.height = size.height.max(1);
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
    let app = App { name, conn, rx, gpu: None, unacked: None, pointer: Pointer::Default, ended: ended.clone(), input };
    event_loop.run_app(app)?;
    let line = ended.lock().unwrap_or_else(|p| p.into_inner()).take();
    Ok(line.unwrap_or_default())
}
