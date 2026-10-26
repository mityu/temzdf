//! Executes a [`DrawList`] with wgpu: image layers and colour fills as
//! textured quads, glyphs with glyphon, in the draw list's order. Renders
//! into a window's surface or offscreen (tests, `--render-frame`).
//!
//! Colours are blended as the stored (sRGB-encoded) values, like the mock
//! terminal and most UI toolkits: textures and targets are non-sRGB formats
//! and glyphon runs in its "web" colour mode.

use std::collections::HashMap;

use anyhow::{Context, Result};
use glyphon::cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, Style, Weight};
use glyphon::{
    Cache, Color as GlyphColor, ColorMode, Resolution, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer,
    Viewport,
};
use temzdf_scene::{Image, ImageKey, Rect};
use wgpu::util::DeviceExt;

use crate::drawlist::{DrawList, Fill, Glyph, Grid, Image as Layer};
use crate::fonts::{CellMetrics, FontSpec};

const SHADER: &str = r"
struct Screen { size: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> screen: Screen;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct Quad {
    @location(0) dst: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) color: vec4<f32>,
};
struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32, q: Quad) -> Out {
    let corner = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    let p = q.dst.xy + corner * q.dst.zw;
    var out: Out;
    out.pos = vec4<f32>(p.x / screen.size.x * 2.0 - 1.0, 1.0 - p.y / screen.size.y * 2.0, 0.0, 1.0);
    out.uv = q.uv.xy + corner * q.uv.zw;
    out.color = q.color;
    return out;
}

// Images and colours have straight alpha; the target blends premultiplied.
@fragment
fn fs(v: Out) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, v.uv) * v.color;
    return vec4<f32>(c.rgb * c.a, c.a);
}
";

/// One quad's instance data: destination (pixels), texture coordinates,
/// colour multiplier.
#[repr(C)]
#[derive(Clone, Copy)]
struct Quad {
    dst: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
}

impl Quad {
    fn bytes(quads: &[Quad]) -> Vec<u8> {
        quads.iter().flat_map(|q| q.dst.iter().chain(&q.uv).chain(&q.color).flat_map(|f| f.to_ne_bytes())).collect()
    }
}

struct Texture {
    _texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    size: (u32, u32),
}

/// What to draw next: quads of one texture (`None`: plain colour), or text.
enum Step {
    Quads { texture: Option<ImageKey>, range: std::ops::Range<u32> },
    Text(usize),
}

/// The glyph cache key: what a shaped buffer depends on.
type GlyphKey = (String, bool, bool, u8);

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    screen: wgpu::Buffer,
    screen_bind: wgpu::BindGroup,
    white: Texture,
    textures: HashMap<ImageKey, Texture>,
    fonts: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    /// Glyphs, and the character over a block cursor.
    text: [TextRenderer; 2],
    glyphs: HashMap<GlyphKey, Buffer>,
    font: FontSpec,
    font_wide: Option<FontSpec>,
    metrics: CellMetrics,
}

impl Renderer {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        mut fonts: FontSystem,
        font: FontSpec,
        scale: f64,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quads"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let screen_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("screen"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quads"),
            bind_group_layouts: &[Some(&screen_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quads"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Quad>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("images"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let screen = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screen"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let screen_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("screen"),
            layout: &screen_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: screen.as_entire_binding() }],
        });
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::with_color_mode(&device, &queue, &cache, format, ColorMode::Web);
        let text = [
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None),
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None),
        ];
        let metrics = crate::fonts::measure(&mut fonts, &font, scale);
        let mut r = Self {
            white: Texture {
                _texture: dummy(&device),
                bind: dummy_bind(&device, &texture_layout, &sampler),
                size: (1, 1),
            },
            device,
            queue,
            format,
            pipeline,
            texture_layout,
            sampler,
            screen,
            screen_bind,
            textures: HashMap::new(),
            fonts,
            swash: SwashCache::new(),
            atlas,
            viewport,
            text,
            glyphs: HashMap::new(),
            font,
            font_wide: None,
            metrics,
        };
        r.white = r.texture(1, 1, &[255, 255, 255, 255]);
        r
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The cell grid the font makes.
    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    /// Use `font` (and `wide` for wide characters) at `scale`.
    pub fn set_font(&mut self, font: FontSpec, wide: Option<FontSpec>, scale: f64) -> CellMetrics {
        self.metrics = crate::fonts::measure(&mut self.fonts, &font, scale);
        (self.font, self.font_wide) = (font, wide);
        self.glyphs.clear();
        self.metrics
    }

    fn texture(&self, width: u32, height: u32, pixels: &[u8]) -> Texture {
        let texture = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some("image"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            pixels,
        );
        let view = texture.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        Texture { _texture: texture, bind, size: (width, height) }
    }

    /// An image of the scene's store, uploaded (again when replaced).
    pub fn set_image(&mut self, key: ImageKey, image: &Image) {
        if image.width == 0 || image.height == 0 {
            self.textures.remove(&key);
            return;
        }
        let t = self.texture(image.width, image.height, &image.pixels);
        self.textures.insert(key, t);
    }

    pub fn remove_image(&mut self, key: ImageKey) {
        self.textures.remove(&key);
    }

    /// The shaped buffer for a glyph (cached).
    fn glyph_buffer(&mut self, g: &Glyph) -> GlyphKey {
        let key = (g.text.clone(), g.bold, g.italic, g.width);
        if !self.glyphs.contains_key(&key) {
            let m = self.metrics;
            let spec = if g.width > 1 { self.font_wide.as_ref().unwrap_or(&self.font) } else { &self.font };
            let mut attrs: Attrs<'_> = spec.attrs();
            if g.bold {
                attrs = attrs.weight(Weight::BOLD);
            }
            if g.italic {
                attrs = attrs.style(Style::Italic);
            }
            let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(m.font_px, m.height as f32));
            buffer.set_size(Some((m.width * g.width as u32 * 4) as f32), Some(m.height as f32));
            buffer.set_text(&g.text, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut self.fonts, false);
            self.glyphs.insert(key.clone(), buffer);
        }
        key
    }

    /// Draw `list` on a target of `size` pixels, the cell grid at `origin`.
    pub fn draw(
        &mut self,
        list: &DrawList,
        grid: Grid,
        origin: (i32, i32),
        size: (u32, u32),
        target: &wgpu::TextureView,
    ) {
        let mut quads: Vec<Quad> = Vec::new();
        let mut steps: Vec<Step> = Vec::new();
        let (ox, oy) = (origin.0 as f32, origin.1 as f32);
        let push = |quads: &mut Vec<Quad>, steps: &mut Vec<Step>, texture: Option<ImageKey>, q: Quad| {
            let i = quads.len() as u32;
            quads.push(q);
            match steps.last_mut() {
                Some(Step::Quads { texture: t, range }) if *t == texture && range.end == i => range.end = i + 1,
                _ => steps.push(Step::Quads { texture, range: i..i + 1 }),
            }
        };
        let dst = |r: Rect| [r.x as f32 + ox, r.y as f32 + oy, r.w as f32, r.h as f32];
        let images =
            |quads: &mut Vec<Quad>, steps: &mut Vec<Step>, layers: &[Layer], textures: &HashMap<ImageKey, Texture>| {
                for l in layers {
                    let Some(t) = textures.get(&l.image) else { continue };
                    let (w, h) = (t.size.0 as f32, t.size.1 as f32);
                    let uv = [l.src.x as f32 / w, l.src.y as f32 / h, l.src.w as f32 / w, l.src.h as f32 / h];
                    push(quads, steps, Some(l.image), Quad { dst: dst(l.dst), uv, color: [1.0; 4] });
                }
            };
        let fills = |quads: &mut Vec<Quad>, steps: &mut Vec<Step>, fills: &[Fill]| {
            for f in fills {
                let color = f.color.map(|c| c as f32 / 255.0);
                push(quads, steps, None, Quad { dst: dst(f.rect), uv: [0.0, 0.0, 1.0, 1.0], color });
            }
        };
        images(&mut quads, &mut steps, &list.under, &self.textures);
        fills(&mut quads, &mut steps, &list.cells);
        images(&mut quads, &mut steps, &list.between, &self.textures);
        steps.push(Step::Text(0));
        fills(&mut quads, &mut steps, &list.marks);
        fills(&mut quads, &mut steps, list.cursor.as_slice());
        steps.push(Step::Text(1));
        images(&mut quads, &mut steps, &list.over, &self.textures);

        // Glyphs: shaped once per character and style, placed in their cells.
        let m = self.metrics;
        let glyph_sets: [Vec<(GlyphKey, &Glyph)>; 2] = [
            list.glyphs.iter().map(|g| (self.glyph_buffer(g), g)).collect(),
            list.cursor_glyph.iter().map(|g| (self.glyph_buffer(g), g)).collect(),
        ];
        self.viewport.update(&self.queue, Resolution { width: size.0, height: size.1 });
        for (n, set) in glyph_sets.iter().enumerate() {
            let areas = set.iter().map(|(key, g)| {
                let left = ox + (g.col as u32 * grid.cell_w) as f32;
                let top = oy + (g.row as u32 * grid.cell_h) as f32;
                TextArea {
                    buffer: &self.glyphs[key],
                    left,
                    top,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: left as i32,
                        top: top as i32,
                        right: left as i32 + (g.width as u32 * grid.cell_w) as i32,
                        bottom: top as i32 + grid.cell_h as i32,
                    },
                    default_color: GlyphColor::rgb(g.color[0], g.color[1], g.color[2]),
                    custom_glyphs: &[],
                }
            });
            let _ = self.text[n].prepare(
                &self.device,
                &self.queue,
                &mut self.fonts,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash,
            );
        }
        let _ = m;

        self.queue.write_buffer(
            &self.screen,
            0,
            &[size.0 as f32, size.1 as f32, 0.0, 0.0].map(f32::to_ne_bytes).concat(),
        );
        let instances = (!quads.is_empty()).then(|| {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("quads"),
                contents: &Quad::bytes(&quads),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let bg = list.background.map(|c| c as f64 / 255.0);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: bg[0], g: bg[1], b: bg[2], a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for step in &steps {
                match step {
                    Step::Quads { texture, range } => {
                        let Some(instances) = &instances else { continue };
                        let bind = match texture {
                            Some(k) => match self.textures.get(k) {
                                Some(t) => &t.bind,
                                None => continue,
                            },
                            None => &self.white.bind,
                        };
                        pass.set_pipeline(&self.pipeline);
                        pass.set_bind_group(0, &self.screen_bind, &[]);
                        pass.set_bind_group(1, bind, &[]);
                        pass.set_vertex_buffer(0, instances.slice(..));
                        pass.draw(0..4, range.clone());
                    }
                    Step::Text(n) => {
                        let _ = self.text[*n].render(&self.atlas, &self.viewport, &mut pass);
                    }
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        self.atlas.trim();
    }

    /// Draw `list` offscreen and read the pixels back (RGBA, `size`).
    pub fn draw_offscreen(&mut self, list: &DrawList, grid: Grid, size: (u32, u32)) -> Result<Vec<u8>> {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        self.draw(list, grid, (0, 0), size, &view);
        let row = size.0 * 4;
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * size.1) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
            },
            wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).context("waiting for the GPU")?;
        rx.recv().context("mapping the frame")?.context("mapping the frame")?;
        let data = buffer.slice(..).get_mapped_range().context("reading the frame")?;
        let mut out = Vec::with_capacity((row * size.1) as usize);
        for y in 0..size.1 as usize {
            out.extend_from_slice(&data[y * padded as usize..y * padded as usize + row as usize]);
        }
        drop(data);
        buffer.unmap();
        if self.format == wgpu::TextureFormat::Bgra8Unorm {
            for px in out.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }
        Ok(out)
    }
}

/// Placeholders while the renderer is built (replaced at once).
fn dummy(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn dummy_bind(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler) -> wgpu::BindGroup {
    let view = dummy(device).create_view(&Default::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

/// A device for offscreen rendering (tests, `--render-frame`), on whatever
/// adapter wgpu picks (lavapipe when `VK_DRIVER_FILES` points at it).
pub fn offscreen_device() -> Result<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .context("no GPU adapter (for a software one, VK_DRIVER_FILES=$TEMZDF_LAVAPIPE_ICD)")?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
    Ok((device, queue))
}
