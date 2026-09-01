//! wgpu drawing pipeline tuned for minimum touch-to-photon latency:
//! no VSync wait when the platform allows it, a single frame of latency,
//! and dynamically-resized vertex/index buffers so a whole frame's stroke
//! geometry can be pushed to the GPU in one write.
//!
//! Two pipelines share one render pass: a standard alpha-blended one for
//! ink/UI, and a Max-blended one for the highlighter brush — Max blend
//! means overlapping highlighter geometry (self-overlap at stroke joints,
//! or two highlighter strokes crossing) never accumulates past one layer's
//! opacity, so the marked text underneath never gets progressively darker.

use std::collections::HashMap;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::Vec2;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::board::PdfImage;
use crate::stroke::Vertex;

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct Uniforms {
    screen_size: [f32; 2],
    _padding: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct TexVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

/// Matches `shader.wgsl`'s `MagnifierUniforms` — `center`+`radius` in
/// screen pixels, `zoom` the same factor `magnifier.rs` uses.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct MagnifierUniforms {
    center: [f32; 2],
    radius: f32,
    zoom: f32,
}

impl TexVertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TexVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

/// Growth factor applied when a dynamic buffer needs to be reallocated, to
/// amortize reallocation cost across frames instead of resizing every time
/// stroke geometry grows by one vertex.
const BUFFER_GROWTH_FACTOR: u64 = 2;

/// A GPU buffer that grows (never shrinks) to fit whatever's written to it
/// each frame — used for both the vertex and index streams, for both the
/// normal and highlighter draw batches.
struct DynamicBuffer {
    buffer: wgpu::Buffer,
    capacity_bytes: u64,
    usage: wgpu::BufferUsages,
    label: &'static str,
}

impl DynamicBuffer {
    fn new(device: &wgpu::Device, label: &'static str, usage: wgpu::BufferUsages, initial_capacity_bytes: u64) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: initial_capacity_bytes,
            usage,
            mapped_at_creation: false,
        });
        Self { buffer, capacity_bytes: initial_capacity_bytes, usage, label }
    }

    fn ensure_capacity(&mut self, device: &wgpu::Device, needed_bytes: u64) {
        if needed_bytes <= self.capacity_bytes {
            return;
        }
        self.capacity_bytes = needed_bytes * BUFFER_GROWTH_FACTOR;
        self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size: self.capacity_bytes,
            usage: self.usage,
            mapped_at_creation: false,
        });
    }
}

/// One draw batch's GPU-side geometry buffers.
struct Batch {
    vertex: DynamicBuffer,
    index: DynamicBuffer,
}

impl Batch {
    fn new(device: &wgpu::Device, name: &'static str) -> Self {
        Self {
            vertex: DynamicBuffer::new(
                device,
                name,
                wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                4096 * std::mem::size_of::<Vertex>() as u64,
            ),
            index: DynamicBuffer::new(
                device,
                name,
                wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                8192 * std::mem::size_of::<u32>() as u64,
            ),
        }
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, vertices: &[Vertex], indices: &[u32]) {
        self.vertex.ensure_capacity(device, (vertices.len() * std::mem::size_of::<Vertex>()) as u64);
        self.index.ensure_capacity(device, (indices.len() * std::mem::size_of::<u32>()) as u64);
        if !vertices.is_empty() {
            queue.write_buffer(&self.vertex.buffer, 0, bytemuck::cast_slice(vertices));
        }
        if !indices.is_empty() {
            queue.write_buffer(&self.index.buffer, 0, bytemuck::cast_slice(indices));
        }
    }

    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, pipeline: &'a wgpu::RenderPipeline, bind_group: &'a wgpu::BindGroup, index_count: u32) {
        self.draw_range(pass, pipeline, bind_group, 0..index_count);
    }

    /// Like `draw`, but only `range` of the index buffer — used to split
    /// the normal batch into its "page content" prefix (magnifiable) and
    /// "UI chrome" suffix (see `render`'s `content_index_count` param)
    /// without needing two separate GPU buffers.
    fn draw_range<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, pipeline: &'a wgpu::RenderPipeline, bind_group: &'a wgpu::BindGroup, range: std::ops::Range<u32>) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex.buffer.slice(..));
        pass.set_index_buffer(self.index.buffer.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(range, 0, 0..1);
    }
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    normal_pipeline: wgpu::RenderPipeline,
    highlighter_pipeline: wgpu::RenderPipeline,
    page_pipeline: wgpu::RenderPipeline,

    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,

    normal_batch: Batch,
    highlighter_batch: Batch,

    page_texture_bgl: wgpu::BindGroupLayout,
    page_sampler: wgpu::Sampler,
    /// One GPU texture per distinct PDF page seen so far, keyed by
    /// `PdfImage::id` — paging back and forth never re-uploads.
    page_textures: HashMap<u64, (wgpu::Texture, wgpu::BindGroup)>,
    page_quad_vertex: wgpu::Buffer,
    page_quad_index: wgpu::Buffer,

    /// The embedded browser panel's texture — unlike `page_textures`,
    /// there's only ever one, it's rewritten every time Servo paints a
    /// new frame (see `webengine::WebPanel::tick`), and it's recreated
    /// outright if the panel's pixel size changes (resize, not just pan).
    web_texture: Option<(wgpu::Texture, wgpu::BindGroup, u32, u32)>,
    web_quad_vertex: wgpu::Buffer,

    /// Offscreen render target the whole "page content" (background/page
    /// image/ink, not UI chrome) is re-rendered into every frame the
    /// magnifier is open, so its lens can sample real pixels — see
    /// `magnifier.rs` and `shader.wgsl::fs_magnifier`. Recreated on
    /// resize (sized to match the swapchain).
    content_texture: wgpu::Texture,
    content_view: wgpu::TextureView,
    content_bind_group: wgpu::BindGroup,

    magnifier_pipeline: wgpu::RenderPipeline,
    magnifier_uniform_buffer: wgpu::Buffer,
    magnifier_uniform_bind_group: wgpu::BindGroup,
    magnifier_quad_vertex: wgpu::Buffer,
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(window)
            .expect("failed to create wgpu surface");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("no suitable GPU adapter found");

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("tahta_device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .expect("failed to acquire wgpu device");

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        // Prefer Mailbox (low-latency triple buffering, no tearing), then
        // Immediate (no wait at all, tearing possible), falling back to
        // whatever the platform actually supports.
        let present_mode = [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
            .into_iter()
            .find(|mode| surface_caps.present_modes.contains(mode))
            .unwrap_or(surface_caps.present_modes[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tahta_uniform_buffer"),
            contents: bytemuck::bytes_of(&Uniforms {
                screen_size: [config.width as f32, config.height as f32],
                _padding: [0.0; 2],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("tahta_uniform_bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // VERTEX for the position transform every pipeline
                    // does; FRAGMENT too because `fs_magnifier` also reads
                    // `screen_size` to convert a source pixel position
                    // into the content texture's UV space.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tahta_uniform_bind_group"),
            layout: &uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tahta_pipeline_layout"),
            bind_group_layouts: &[&uniform_bind_group_layout],
            push_constant_ranges: &[],
        });

        let primitive = wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        };

        let make_pipeline = |label: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    buffers: &[Vertex::layout()],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive,
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            })
        };

        let normal_pipeline = make_pipeline("tahta_normal_pipeline", wgpu::BlendState::ALPHA_BLENDING);

        let max_component = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let highlighter_pipeline = make_pipeline(
            "tahta_highlighter_pipeline",
            wgpu::BlendState { color: max_component, alpha: max_component },
        );

        let normal_batch = Batch::new(&device, "tahta_normal_batch");
        let highlighter_batch = Batch::new(&device, "tahta_highlighter_batch");

        let page_texture_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tahta_page_texture_bgl"),
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

        let page_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tahta_page_pipeline_layout"),
            bind_group_layouts: &[&uniform_bind_group_layout, &page_texture_bgl],
            push_constant_ranges: &[],
        });

        let page_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tahta_page_pipeline"),
            layout: Some(&page_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_tex",
                buffers: &[TexVertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_tex",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive,
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let page_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tahta_page_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let page_quad_vertex = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tahta_page_quad_vertex"),
            size: 4 * std::mem::size_of::<TexVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let page_quad_index = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tahta_page_quad_index"),
            contents: bytemuck::cast_slice(&[0u32, 1, 2, 2, 1, 3]),
            usage: wgpu::BufferUsages::INDEX,
        });

        let web_quad_vertex = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tahta_web_quad_vertex"),
            size: 4 * std::mem::size_of::<TexVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (content_texture, content_view, content_bind_group) =
            Self::create_content_texture(&device, &page_texture_bgl, &page_sampler, surface_format, config.width, config.height);

        let magnifier_uniform_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tahta_magnifier_uniform_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let magnifier_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tahta_magnifier_uniform_buffer"),
            size: std::mem::size_of::<MagnifierUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let magnifier_uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tahta_magnifier_uniform_bind_group"),
            layout: &magnifier_uniform_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: magnifier_uniform_buffer.as_entire_binding() }],
        });

        let magnifier_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tahta_magnifier_pipeline_layout"),
            bind_group_layouts: &[&uniform_bind_group_layout, &page_texture_bgl, &magnifier_uniform_bgl],
            push_constant_ranges: &[],
        });
        let magnifier_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tahta_magnifier_pipeline"),
            layout: Some(&magnifier_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_tex",
                buffers: &[TexVertex::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_magnifier",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive,
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        let magnifier_quad_vertex = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tahta_magnifier_quad_vertex"),
            size: 4 * std::mem::size_of::<TexVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            surface,
            device,
            queue,
            config,
            normal_pipeline,
            highlighter_pipeline,
            page_pipeline,
            uniform_buffer,
            uniform_bind_group,
            normal_batch,
            highlighter_batch,
            page_texture_bgl,
            page_sampler,
            page_textures: HashMap::new(),
            page_quad_vertex,
            page_quad_index,
            web_texture: None,
            web_quad_vertex,
            content_texture,
            content_view,
            content_bind_group,
            magnifier_pipeline,
            magnifier_uniform_buffer,
            magnifier_uniform_bind_group,
            magnifier_quad_vertex,
        }
    }

    /// Creates (or, on resize, recreates) the offscreen "content" texture
    /// the magnifier lens samples from — sized to match the swapchain so
    /// its pixel coordinates line up 1:1 with `Uniforms::screen_size`.
    fn create_content_texture(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView, wgpu::BindGroup) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tahta_content_texture"),
            size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tahta_content_bind_group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        });
        (texture, view, bind_group)
    }

    /// Uploads `image` to the GPU the first time it's seen; subsequent
    /// calls with the same `PdfImage::id` reuse the cached texture.
    fn ensure_page_texture(&mut self, image: &PdfImage) -> &wgpu::BindGroup {
        if !self.page_textures.contains_key(&image.id) {
            let size = wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 };
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tahta_page_texture"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &image.rgba,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * image.width),
                    rows_per_image: Some(image.height),
                },
                size,
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("tahta_page_bind_group"),
                layout: &self.page_texture_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.page_sampler) },
                ],
            });
            self.page_textures.insert(image.id, (texture, bind_group));
        }
        &self.page_textures[&image.id].1
    }

    /// Writes a freshly-painted Servo frame into the panel's texture,
    /// recreating it if the pixel size changed since the last frame
    /// (resize, or the panel just opened).
    fn upload_web_texture(&mut self, rgba: &[u8], width: u32, height: u32) {
        let needs_new = !matches!(&self.web_texture, Some((_, _, w, h)) if *w == width && *h == height);
        if needs_new {
            let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tahta_web_texture"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("tahta_web_bind_group"),
                layout: &self.page_texture_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.page_sampler) },
                ],
            });
            self.web_texture = Some((texture, bind_group, width, height));
        }
        let (texture, ..) = self.web_texture.as_ref().unwrap();
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(4 * width), rows_per_image: Some(height) },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }

    /// Largest centered rect of `image`'s aspect ratio that fits within
    /// `screen_size` (letterboxed), in screen-space pixels.
    fn letterbox_rect(image: &PdfImage, screen_size: Vec2) -> (Vec2, Vec2) {
        let image_aspect = image.width as f32 / image.height as f32;
        let screen_aspect = screen_size.x / screen_size.y;
        let size = if image_aspect > screen_aspect {
            Vec2::new(screen_size.x, screen_size.x / image_aspect)
        } else {
            Vec2::new(screen_size.y * image_aspect, screen_size.y)
        };
        let top_left = (screen_size - size) / 2.0;
        (top_left, size)
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);

        let (texture, view, bind_group) = Self::create_content_texture(
            &self.device,
            &self.page_texture_bgl,
            &self.page_sampler,
            self.config.format,
            self.config.width,
            self.config.height,
        );
        self.content_texture = texture;
        self.content_view = view;
        self.content_bind_group = bind_group;
    }

    /// Uploads and draws one frame: `normal` covers ink/UI (standard alpha
    /// blend, drawn first) and `highlighter` covers Highlighter-brush
    /// strokes (Max blend, drawn on top so marker strokes never darken the
    /// ink or each other underneath). `content_index_count` is how many of
    /// `normal`'s indices (from the start) are page content rather than UI
    /// chrome — see `input_handler.rs::collect_geometry`. When
    /// `magnifier_lens` is `Some((center, radius, zoom))`, the page content
    /// (page image + highlighter + that content prefix) is re-rendered into
    /// an offscreen texture first, then sampled into the lens circle
    /// between the content and chrome portions of the main pass — real
    /// pixels, not re-scaled vectors (see `magnifier.rs`).
    pub fn render(
        &mut self,
        normal: (&[Vertex], &[u32]),
        highlighter: (&[Vertex], &[u32]),
        content_index_count: usize,
        page_image: Option<&PdfImage>,
        web_frame: Option<(&[u8], u32, u32)>,
        web_bounds: Option<(Vec2, Vec2)>,
        magnifier_lens: Option<(Vec2, f32, f32)>,
    ) -> Result<(), wgpu::SurfaceError> {
        let output = self.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        self.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&Uniforms {
                screen_size: [self.config.width as f32, self.config.height as f32],
                _padding: [0.0; 2],
            }),
        );

        self.normal_batch.upload(&self.device, &self.queue, normal.0, normal.1);
        self.highlighter_batch.upload(&self.device, &self.queue, highlighter.0, highlighter.1);

        if let Some(image) = page_image {
            self.ensure_page_texture(image);
            let screen_size = Vec2::new(self.config.width as f32, self.config.height as f32);
            let (top_left, size) = Self::letterbox_rect(image, screen_size);
            let verts = [
                TexVertex { position: [top_left.x, top_left.y], uv: [0.0, 0.0] },
                TexVertex { position: [top_left.x, top_left.y + size.y], uv: [0.0, 1.0] },
                TexVertex { position: [top_left.x + size.x, top_left.y], uv: [1.0, 0.0] },
                TexVertex { position: [top_left.x + size.x, top_left.y + size.y], uv: [1.0, 1.0] },
            ];
            self.queue.write_buffer(&self.page_quad_vertex, 0, bytemuck::cast_slice(&verts));
        }

        if let Some((rgba, w, h)) = web_frame {
            self.upload_web_texture(rgba, w, h);
        }
        if let Some((top_left, size)) = web_bounds {
            let verts = [
                TexVertex { position: [top_left.x, top_left.y], uv: [0.0, 0.0] },
                TexVertex { position: [top_left.x, top_left.y + size.y], uv: [0.0, 1.0] },
                TexVertex { position: [top_left.x + size.x, top_left.y], uv: [1.0, 0.0] },
                TexVertex { position: [top_left.x + size.x, top_left.y + size.y], uv: [1.0, 1.0] },
            ];
            self.queue.write_buffer(&self.web_quad_vertex, 0, bytemuck::cast_slice(&verts));
        }

        if let Some((center, radius, zoom)) = magnifier_lens {
            self.queue.write_buffer(
                &self.magnifier_uniform_buffer,
                0,
                bytemuck::bytes_of(&MagnifierUniforms { center: [center.x, center.y], radius, zoom }),
            );
            let verts = [
                TexVertex { position: [center.x - radius, center.y - radius], uv: [0.0, 0.0] },
                TexVertex { position: [center.x - radius, center.y + radius], uv: [0.0, 1.0] },
                TexVertex { position: [center.x + radius, center.y - radius], uv: [1.0, 0.0] },
                TexVertex { position: [center.x + radius, center.y + radius], uv: [1.0, 1.0] },
            ];
            self.queue.write_buffer(&self.magnifier_quad_vertex, 0, bytemuck::cast_slice(&verts));
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tahta_encoder"),
            });

        if magnifier_lens.is_some() {
            let mut content_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tahta_content_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.content_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.05, g: 0.05, b: 0.07, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            if let Some(image) = page_image {
                let bind_group = &self.page_textures[&image.id].1;
                content_pass.set_pipeline(&self.page_pipeline);
                content_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                content_pass.set_bind_group(1, bind_group, &[]);
                content_pass.set_vertex_buffer(0, self.page_quad_vertex.slice(..));
                content_pass.set_index_buffer(self.page_quad_index.slice(..), wgpu::IndexFormat::Uint32);
                content_pass.draw_indexed(0..6, 0, 0..1);
            }
            self.highlighter_batch.draw(&mut content_pass, &self.highlighter_pipeline, &self.uniform_bind_group, highlighter.1.len() as u32);
            self.normal_batch.draw_range(&mut content_pass, &self.normal_pipeline, &self.uniform_bind_group, 0..content_index_count as u32);
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tahta_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.07,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Page image first (if any) so ink/UI drawn after it sits on top.
            if let Some(image) = page_image {
                let bind_group = &self.page_textures[&image.id].1;
                pass.set_pipeline(&self.page_pipeline);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_bind_group(1, bind_group, &[]);
                pass.set_vertex_buffer(0, self.page_quad_vertex.slice(..));
                pass.set_index_buffer(self.page_quad_index.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..6, 0, 0..1);
            }

            // Highlighter next (Max-blended) so it sits "under" — visually
            // behind — freshly drawn ink and UI chrome painted after it.
            self.highlighter_batch.draw(&mut pass, &self.highlighter_pipeline, &self.uniform_bind_group, highlighter.1.len() as u32);
            self.normal_batch.draw_range(&mut pass, &self.normal_pipeline, &self.uniform_bind_group, 0..content_index_count as u32);

            // Magnifier lens (real pixel zoom of the content pass above)
            // between page content and UI chrome, so its own rim/handle
            // (drawn as part of the chrome range right after) sits on top
            // of the zoomed circle instead of under it.
            if magnifier_lens.is_some() {
                pass.set_pipeline(&self.magnifier_pipeline);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_bind_group(1, &self.content_bind_group, &[]);
                pass.set_bind_group(2, &self.magnifier_uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, self.magnifier_quad_vertex.slice(..));
                pass.set_index_buffer(self.page_quad_index.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..6, 0, 0..1);
            }

            self.normal_batch.draw_range(&mut pass, &self.normal_pipeline, &self.uniform_bind_group, content_index_count as u32..normal.1.len() as u32);

            // Browser panel last: it's a floating overlay above the
            // canvas/toolbar, not page content, so it draws on top of
            // everything else — same spot other panels' own chrome would
            // occupy if drawn as vertex geometry instead of a texture.
            if web_bounds.is_some() {
                if let Some((_, bind_group, ..)) = &self.web_texture {
                    pass.set_pipeline(&self.page_pipeline);
                    pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                    pass.set_bind_group(1, bind_group, &[]);
                    pass.set_vertex_buffer(0, self.web_quad_vertex.slice(..));
                    pass.set_index_buffer(self.page_quad_index.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..6, 0, 0..1);
                }
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        Ok(())
    }
}
