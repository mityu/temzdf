//! Experiment: can a GPU rasteriser beat MuPDF's CPU rasteriser for temzdf?
//!
//! 1. Record a page's display list as vector commands (paths and glyph
//!    outlines) through a custom MuPDF device.
//! 2. Render the recording with vello_cpu (CPU, 1 and N threads), vello (GPU
//!    compute) and vello_hybrid (CPU strips, GPU raster), the GPU ones including
//!    the readback temzdf would need to send pixels to a terminal.
//!
//! Only fills, strokes and text are recorded; clips, images, shadings, dashes,
//! caps/joins and blend groups are not (counted as "skipped").
//! 3. Compare times and pixels with MuPDF's own rasteriser.
//!
//! Usage: gpu-raster FILE.pdf [PAGE] [WIDTH_PX] [OUT_DIR] (see doc/gpu.md).
//! `MUPDF_AA=0..8` sets MuPDF's anti-aliasing level.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mupdf::{
    ColorParams, Colorspace, Device, DisplayList, Document, Matrix, NativeDevice, Path, PathWalker, Pixmap,
    StrokeState, Text,
};

#[derive(Clone, Copy, Debug)]
enum El {
    M(f64, f64),
    L(f64, f64),
    C(f64, f64, f64, f64, f64, f64),
    Z,
}

#[derive(Clone, Debug)]
enum Kind {
    Fill { even_odd: bool },
    Stroke { width: f64 },
}

#[derive(Clone, Debug)]
struct Cmd {
    path: Rc<Vec<El>>,
    /// x' = a x + c y + e, y' = b x + d y + f (MuPDF's and kurbo's layout).
    transform: [f64; 6],
    kind: Kind,
    rgba: [f32; 4],
}

#[derive(Default)]
struct Recording {
    cmds: Vec<Cmd>,
    glyphs: HashMap<(String, i32), Rc<Vec<El>>>,
    skipped: HashMap<&'static str, usize>,
}

struct Walk(Vec<El>);

impl PathWalker for Walk {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(El::M(x as f64, y as f64));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(El::L(x as f64, y as f64));
    }
    fn curve_to(&mut self, a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) {
        self.0.push(El::C(a as f64, b as f64, c as f64, d as f64, e as f64, f as f64));
    }
    fn close(&mut self) {
        self.0.push(El::Z);
    }
}

fn path_els(p: &Path) -> Vec<El> {
    let mut w = Walk(Vec::new());
    p.walk(&mut w).unwrap();
    w.0
}

fn m6(m: &Matrix) -> [f64; 6] {
    [m.a as f64, m.b as f64, m.c as f64, m.d as f64, m.e as f64, m.f as f64]
}

/// `first` then `then` (row-vector convention: p * first * then).
fn concat(first: [f64; 6], then: [f64; 6]) -> [f64; 6] {
    let [a, b, c, d, e, f] = first;
    let [a2, b2, c2, d2, e2, f2] = then;
    [a * a2 + b * c2, a * b2 + b * d2, c * a2 + d * c2, c * b2 + d * d2, e * a2 + f * c2 + e2, e * b2 + f * d2 + f2]
}

fn rgb(cs: &Colorspace, color: &[f32], alpha: f32) -> [f32; 4] {
    let rgb = cs.convert_color(color, &Colorspace::device_rgb(), None, ColorParams::default()).unwrap_or(vec![0.0; 3]);
    [rgb[0], rgb[1], rgb[2], alpha]
}

struct Recorder(Rc<RefCell<Recording>>);

impl NativeDevice for Recorder {
    fn fill_path(
        &mut self,
        path: &Path,
        even_odd: bool,
        ctm: Matrix,
        cs: &Colorspace,
        color: &[f32],
        alpha: f32,
        _: ColorParams,
    ) {
        self.0.borrow_mut().cmds.push(Cmd {
            path: Rc::new(path_els(path)),
            transform: m6(&ctm),
            kind: Kind::Fill { even_odd },
            rgba: rgb(cs, color, alpha),
        });
    }

    fn stroke_path(
        &mut self,
        path: &Path,
        stroke: &StrokeState,
        ctm: Matrix,
        cs: &Colorspace,
        color: &[f32],
        alpha: f32,
        _: ColorParams,
    ) {
        self.0.borrow_mut().cmds.push(Cmd {
            path: Rc::new(path_els(path)),
            transform: m6(&ctm),
            kind: Kind::Stroke { width: stroke.line_width() as f64 },
            rgba: rgb(cs, color, alpha),
        });
    }

    fn fill_text(&mut self, text: &Text, ctm: Matrix, cs: &Colorspace, color: &[f32], alpha: f32, _: ColorParams) {
        let rgba = rgb(cs, color, alpha);
        let mut rec = self.0.borrow_mut();
        for span in text.spans() {
            let font = span.font();
            let name = font.name().to_owned();
            let trm = span.trm();
            for item in span.items() {
                let gid = item.gid();
                if gid < 0 {
                    continue;
                }
                let key = (name.clone(), gid);
                let path = match rec.glyphs.get(&key) {
                    Some(p) => p.clone(),
                    None => {
                        let p =
                            Rc::new(font.outline_glyph(gid).ok().flatten().map(|p| path_els(&p)).unwrap_or_default());
                        rec.glyphs.insert(key, p.clone());
                        p
                    }
                };
                let mut glyph = m6(&trm);
                glyph[4] = item.x() as f64;
                glyph[5] = item.y() as f64;
                rec.cmds.push(Cmd {
                    path,
                    transform: concat(glyph, m6(&ctm)),
                    kind: Kind::Fill { even_odd: false },
                    rgba,
                });
            }
        }
    }

    fn fill_image(&mut self, _: &mupdf::Image, _: Matrix, _: f32, _: ColorParams) {
        *self.0.borrow_mut().skipped.entry("image").or_default() += 1;
    }
    fn fill_shade(&mut self, _: &mupdf::Shade, _: Matrix, _: f32, _: ColorParams) {
        *self.0.borrow_mut().skipped.entry("shade").or_default() += 1;
    }
    fn clip_path(&mut self, _: &Path, _: bool, _: Matrix, _: mupdf::Rect) {
        *self.0.borrow_mut().skipped.entry("clip").or_default() += 1;
    }
    fn clip_text(&mut self, _: &Text, _: Matrix, _: mupdf::Rect) {
        *self.0.borrow_mut().skipped.entry("clip").or_default() += 1;
    }
}

fn record(list: &DisplayList) -> Recording {
    let rec = Rc::new(RefCell::new(Recording::default()));
    {
        let dev = Device::from_native(Recorder(rec.clone())).unwrap();
        list.run(&dev, &Matrix::IDENTITY, mupdf::Rect::INF).unwrap();
    }
    Rc::try_unwrap(rec).ok().expect("device dropped").into_inner()
}

/// MuPDF's own rendering: RGBA8 (opaque white background).
fn mupdf_render(list: &DisplayList, scale: f32, w: u32, h: u32) -> Vec<u8> {
    let rect = mupdf::IRect::new(0, 0, w as i32, h as i32);
    let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), rect, true).unwrap();
    pixmap.clear_with(255).unwrap();
    {
        let dev = Device::from_pixmap(&pixmap).unwrap();
        list.run(&dev, &Matrix::new_scale(scale, scale), mupdf::Rect::new(0.0, 0.0, w as f32, h as f32)).unwrap();
    }
    pixmap.samples()[..(w * h * 4) as usize].to_vec()
}

mod cpu {
    use super::*;
    use vello_cpu::kurbo::{Affine, BezPath, Stroke};
    use vello_cpu::peniko::Fill;
    use vello_cpu::peniko::color::{AlphaColor, Srgb};
    use vello_cpu::{Level, Pixmap, RenderContext, RenderSettings, Resources};

    pub fn bez(els: &[El]) -> BezPath {
        let mut p = BezPath::new();
        for e in els {
            match *e {
                El::M(x, y) => p.move_to((x, y)),
                El::L(x, y) => p.line_to((x, y)),
                El::C(a, b, c, d, e, f) => p.curve_to((a, b), (c, d), (e, f)),
                El::Z => p.close_path(),
            }
        }
        p
    }

    pub fn render(rec: &Recording, scale: f64, w: u32, h: u32, threads: u16) -> Vec<u8> {
        let settings = RenderSettings { level: Level::try_detect().unwrap_or(Level::baseline()), num_threads: threads };
        let mut ctx = RenderContext::new_with(w as u16, h as u16, settings);
        let mut resources = Resources::new();
        ctx.set_paint(AlphaColor::<Srgb>::new([1.0, 1.0, 1.0, 1.0]));
        ctx.fill_rect(&vello_cpu::kurbo::Rect::new(0.0, 0.0, w as f64, h as f64));
        let page = Affine::scale(scale);
        for cmd in &rec.cmds {
            let path = bez(&cmd.path);
            ctx.set_transform(page * Affine::new(cmd.transform));
            ctx.set_paint(AlphaColor::<Srgb>::new(cmd.rgba));
            match cmd.kind {
                Kind::Fill { even_odd } => {
                    ctx.set_fill_rule(if even_odd { Fill::EvenOdd } else { Fill::NonZero });
                    ctx.fill_path(&path);
                }
                Kind::Stroke { width } => {
                    ctx.set_stroke(Stroke::new(width));
                    ctx.stroke_path(&path);
                }
            }
        }
        let mut pixmap = Pixmap::new(w as u16, h as u16);
        ctx.flush();
        ctx.render(&mut pixmap, &mut resources);
        pixmap.data().iter().flat_map(|p| [p.r, p.g, p.b, p.a]).collect()
    }
}

mod gpu {
    use super::*;
    use vello::kurbo::{Affine, BezPath, Stroke};
    use vello::peniko::Fill;
    use vello::peniko::color::{AlphaColor, Srgb};
    use vello::wgpu;
    use vello::{AaConfig, RenderParams, Renderer, RendererOptions, Scene};

    pub struct Gpu {
        device: wgpu::Device,
        queue: wgpu::Queue,
        renderer: Renderer,
        target: Option<(wgpu::Texture, wgpu::Buffer, (u32, u32))>,
        pub adapter: String,
    }

    impl Gpu {
        pub fn new() -> Option<Self> {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            }))
            .ok()?;
            let info = adapter.get_info();
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
            let renderer = Renderer::new(&device, RendererOptions::default()).ok()?;
            Some(Self { device, queue, renderer, target: None, adapter: format!("{} ({:?})", info.name, info.backend) })
        }

        pub fn scene(rec: &Recording, scale: f64, w: u32, h: u32) -> Scene {
            let mut scene = Scene::new();
            let white = AlphaColor::<Srgb>::new([1.0, 1.0, 1.0, 1.0]);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                white,
                None,
                &vello::kurbo::Rect::new(0.0, 0.0, w as f64, h as f64),
            );
            let page = Affine::scale(scale);
            for cmd in &rec.cmds {
                let mut path = BezPath::new();
                for e in cmd.path.iter() {
                    match *e {
                        El::M(x, y) => path.move_to((x, y)),
                        El::L(x, y) => path.line_to((x, y)),
                        El::C(a, b, c, d, e, f) => path.curve_to((a, b), (c, d), (e, f)),
                        El::Z => path.close_path(),
                    }
                }
                let t = page * Affine::new(cmd.transform);
                let color = AlphaColor::<Srgb>::new(cmd.rgba);
                match cmd.kind {
                    Kind::Fill { even_odd } => {
                        scene.fill(if even_odd { Fill::EvenOdd } else { Fill::NonZero }, t, color, None, &path)
                    }
                    Kind::Stroke { width } => scene.stroke(&Stroke::new(width), t, color, None, &path),
                }
            }
            scene
        }

        /// Render and read back RGBA8, reusing the target texture and buffer.
        /// Returns the pixels, the time until the GPU finished rendering, and
        /// the time to copy the image back into CPU memory.
        pub fn render(&mut self, scene: &Scene, w: u32, h: u32) -> (Vec<u8>, Duration, Duration) {
            let row = (w * 4).div_ceil(256) * 256;
            if self.target.as_ref().is_none_or(|t| t.2 != (w, h)) {
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: (row * h) as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                self.target = Some((texture, buffer, (w, h)));
            }
            let (texture, buffer, _) = self.target.as_ref().unwrap();
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let start = Instant::now();
            self.renderer
                .render_to_texture(
                    &self.device,
                    &self.queue,
                    scene,
                    &view,
                    &RenderParams {
                        base_color: AlphaColor::<Srgb>::new([1.0, 1.0, 1.0, 1.0]),
                        width: w,
                        height: h,
                        antialiasing_method: AaConfig::Area,
                    },
                )
                .unwrap();
            self.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let rendered = start.elapsed();
            let start = Instant::now();
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            enc.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            self.queue.submit([enc.finish()]);
            let slice = buffer.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            self.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let data = slice.get_mapped_range().unwrap();
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h as usize {
                out.extend_from_slice(&data[y * row as usize..y * row as usize + w as usize * 4]);
            }
            drop(data);
            buffer.unmap();
            (out, rendered, start.elapsed())
        }
    }
}

mod hybrid {
    use super::*;
    use vello_common::kurbo::{Affine, BezPath, Stroke};
    use vello_common::peniko::Fill;
    use vello_common::peniko::color::{AlphaColor, Srgb};
    use wgpu29 as wgpu;

    pub struct Hybrid {
        device: wgpu::Device,
        queue: wgpu::Queue,
        target: Option<(wgpu::Texture, wgpu::Buffer, vello_hybrid::Renderer, vello_hybrid::Resources, (u32, u32))>,
        pub adapter: String,
    }

    impl Hybrid {
        pub fn new() -> Option<Self> {
            let instance = wgpu::Instance::default();
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            }))
            .ok()?;
            let info = adapter.get_info();
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
            Some(Self { device, queue, target: None, adapter: format!("{} ({:?})", info.name, info.backend) })
        }

        /// Build the scene (CPU: flattening and sparse strips happen here).
        pub fn scene(rec: &Recording, scale: f64, w: u32, h: u32) -> vello_hybrid::Scene {
            let mut scene = vello_hybrid::Scene::new(w as u16, h as u16);
            scene.set_paint(AlphaColor::<Srgb>::new([1.0, 1.0, 1.0, 1.0]));
            scene.fill_rect(&vello_common::kurbo::Rect::new(0.0, 0.0, w as f64, h as f64));
            let page = Affine::scale(scale);
            for cmd in &rec.cmds {
                let mut path = BezPath::new();
                for e in cmd.path.iter() {
                    match *e {
                        El::M(x, y) => path.move_to((x, y)),
                        El::L(x, y) => path.line_to((x, y)),
                        El::C(a, b, c, d, e, f) => path.curve_to((a, b), (c, d), (e, f)),
                        El::Z => path.close_path(),
                    }
                }
                scene.set_transform(page * Affine::new(cmd.transform));
                scene.set_paint(AlphaColor::<Srgb>::new(cmd.rgba));
                match cmd.kind {
                    Kind::Fill { even_odd } => {
                        scene.set_fill_rule(if even_odd { Fill::EvenOdd } else { Fill::NonZero });
                        scene.fill_path(&path);
                    }
                    Kind::Stroke { width } => {
                        scene.set_stroke(Stroke::new(width));
                        scene.stroke_path(&path);
                    }
                }
            }
            scene
        }

        /// GPU part: render and read back. Returns pixels, render time, readback time.
        pub fn render(&mut self, scene: &vello_hybrid::Scene, w: u32, h: u32) -> (Vec<u8>, Duration, Duration) {
            let row = (w * 4).div_ceil(256) * 256;
            if self.target.as_ref().is_none_or(|t| t.4 != (w, h)) {
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: (row * h) as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let (renderer, resources) = vello_hybrid::Renderer::new(
                    &self.device,
                    &vello_hybrid::RenderTargetConfig { format: texture.format(), width: w, height: h },
                );
                self.target = Some((texture, buffer, renderer, resources, (w, h)));
            }
            let (texture, buffer, renderer, resources, _) = self.target.as_mut().unwrap();
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let start = Instant::now();
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            renderer
                .render(
                    scene,
                    resources,
                    &self.device,
                    &self.queue,
                    &mut enc,
                    &vello_hybrid::RenderSize { width: w, height: h },
                    &view,
                    &vello_hybrid::TextureBindings::new(),
                )
                .unwrap();
            self.queue.submit([enc.finish()]);
            self.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let rendered = start.elapsed();
            let start = Instant::now();
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            enc.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            self.queue.submit([enc.finish()]);
            let slice = buffer.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            self.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let data = slice.get_mapped_range();
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h as usize {
                out.extend_from_slice(&data[y * row as usize..y * row as usize + w as usize * 4]);
            }
            drop(data);
            buffer.unmap();
            (out, rendered, start.elapsed())
        }
    }
}

/// Mean absolute difference per channel (RGB), and the share of pixels whose
/// largest channel difference exceeds 32.
fn compare(a: &[u8], b: &[u8]) -> (f64, f64) {
    let (mut sum, mut big, mut n) = (0u64, 0u64, 0u64);
    for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let d = (0..3).map(|i| p[i].abs_diff(q[i]) as u64);
        let m = d.clone().max().unwrap_or(0);
        sum += d.sum::<u64>();
        big += (m > 32) as u64;
        n += 1;
    }
    (sum as f64 / (n * 3) as f64, big as f64 / n as f64)
}

fn save(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}

fn best<T>(n: usize, mut f: impl FnMut() -> T) -> (T, Duration) {
    let mut best = Duration::MAX;
    let mut last = None;
    for _ in 0..n {
        let t = Instant::now();
        let v = f();
        best = best.min(t.elapsed());
        last = Some(v);
    }
    (last.unwrap(), best)
}

fn ms(d: Duration) -> String {
    format!("{:8.2} ms", d.as_secs_f64() * 1e3)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let file = &args[1];
    let page_no: i32 = args.get(2).map_or(0, |s| s.parse().unwrap());
    let width: u32 = args.get(3).map_or(1800, |s| s.parse().unwrap());
    let out = args.get(4).map(std::path::PathBuf::from);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as u16;

    if let Ok(aa) = std::env::var("MUPDF_AA") {
        let mut ctx = mupdf::Context::get();
        ctx.set_aa_level(aa.parse().unwrap());
        println!("mupdf aa level {} (text {})", ctx.aa_level(), ctx.text_aa_level());
    }
    let doc = Document::open(file).unwrap();
    let page = doc.load_page(page_no).unwrap();
    let bounds = page.bounds().unwrap();
    let list = page.to_display_list(false).unwrap();
    let scale = width as f32 / (bounds.x1 - bounds.x0);
    let h = ((bounds.y1 - bounds.y0) * scale).ceil() as u32;
    let w = width;
    println!("{file} page {page_no}: {w}x{h} px (scale {scale:.3})");

    let (reference, t_mupdf) = best(5, || mupdf_render(&list, scale, w, h));
    println!("mupdf draw          {}", ms(t_mupdf));

    let (rec, t_rec) = best(5, || record(&list));
    println!(
        "record              {}  ({} commands, {} distinct glyphs, skipped {:?})",
        ms(t_rec),
        rec.cmds.len(),
        rec.glyphs.len(),
        rec.skipped
    );

    let (cpu1, t_cpu1) = best(5, || cpu::render(&rec, scale as f64, w, h, 0));
    let (d, big) = compare(&reference, &cpu1);
    println!("vello_cpu 1 thread  {}  diff vs mupdf: mean {d:.2}, >32: {:.3}%", ms(t_cpu1), big * 100.0);
    let (cpun, t_cpun) = best(5, || cpu::render(&rec, scale as f64, w, h, threads - 1));
    println!("vello_cpu {threads:2} threads {}", ms(t_cpun));
    let _ = cpun;

    match gpu::Gpu::new() {
        None => println!("gpu: no adapter"),
        Some(mut g) => {
            println!("gpu: {}", g.adapter);
            let (scene, t_scene) = best(5, || gpu::Gpu::scene(&rec, scale as f64, w, h));
            println!("vello scene build   {}", ms(t_scene));
            let (_, r, c) = g.render(&scene, w, h);
            println!("vello first frame   {}  (pipeline setup)", ms(r + c));
            let mut renders = Vec::new();
            let mut copies = Vec::new();
            let mut px = Vec::new();
            for _ in 0..10 {
                let (p, r, c) = g.render(&scene, w, h);
                renders.push(r);
                copies.push(c);
                px = p;
            }
            renders.sort();
            copies.sort();
            let (d, big) = compare(&reference, &px);
            println!(
                "vello gpu render    {}  readback {}  (medians of 10)  diff vs mupdf: mean {d:.2}, >32: {:.3}%",
                ms(renders[5]),
                ms(copies[5]),
                big * 100.0
            );
            if let Some(dir) = &out {
                save(&dir.join("vello.png"), w, h, &px);
            }
        }
    }
    match hybrid::Hybrid::new() {
        None => println!("hybrid: no adapter"),
        Some(mut g) => {
            println!("hybrid: {}", g.adapter);
            let (scene, t_scene) = best(5, || hybrid::Hybrid::scene(&rec, scale as f64, w, h));
            println!("hybrid scene (cpu)  {}", ms(t_scene));
            let (_, r, c) = g.render(&scene, w, h);
            println!("hybrid first frame  {}  (pipeline setup)", ms(r + c));
            let mut renders = Vec::new();
            let mut copies = Vec::new();
            let mut px = Vec::new();
            for _ in 0..10 {
                let (p, r, c) = g.render(&scene, w, h);
                renders.push(r);
                copies.push(c);
                px = p;
            }
            renders.sort();
            copies.sort();
            let (d, big) = compare(&reference, &px);
            println!(
                "hybrid gpu render   {}  readback {}  (medians of 10)  diff vs mupdf: mean {d:.2}, >32: {:.3}%",
                ms(renders[5]),
                ms(copies[5]),
                big * 100.0
            );
            if let Some(dir) = &out {
                save(&dir.join("hybrid.png"), w, h, &px);
            }
        }
    }
    if let Some(dir) = &out {
        save(&dir.join("mupdf.png"), w, h, &reference);
        save(&dir.join("vello_cpu.png"), w, h, &cpu1);
    }
}
