//! The GPU side of the avatars: depth-tested 3D models through the field camera, and the scenery
//! in front of them.
//!
//! [`AvatarGpu`] belongs to the renderer's `Gpu` and is drawn after the scene's own pass
//! (picture, ground marks), as two more render passes over what the target already holds:
//!
//! 1. **Models.** A pass with a `Depth32Float` attachment (cleared to 1.0, discarded afterwards):
//!    every skinned model of the frame, opaque batches of all players first, then the
//!    semi-transparent ones (depth-tested but not written). Models are uploaded once per
//!    character: static vertex data (texture coordinates, vertex colours, the overlay flag),
//!    indices and textures stay on the GPU; only the skinned positions are written per frame.
//! 2. **Front layer.** The scenery in front of the characters (background layers 2 and 4) as a
//!    quad over the same rectangle as the picture, alpha-blended (premultiplied: it is already
//!    zero where it is transparent), so a character standing behind a pillar is covered by it.
//!
//! # The projection
//!
//! Skinned positions arrive in *camera space* (see [`super::stage::StageCamera`]). The vertex
//! shader needs only the field camera's perspective divide and the window fit the rest of the
//! scene uses (`render::Camera::fit`):
//!
//! ```text
//! clip.x = (zoom * cam.x - cx * cam.z) * scale / (width / 2)
//! clip.y = (-zoom * cam.y - cy * cam.z) * scale / (height / 2)       w = cam.z
//! ```
//!
//! where `(cx, cy)` and `scale` are the fitted camera's centre (view space) and pixels per unit:
//! the very mapping the picture and the walkmesh go through. The depth written is the camera
//! depth mapped linearly onto the frame's own near..far range (see
//! [`super::frame::AvatarFrame::near`]), which is about the size of one character, so the 32-bit
//! float depth buffer has far finer steps than the parts of a model are apart.
//!
//! The surface may be sRGB or not (the window prefers not): vertex colours and textures are
//! authored in sRGB bytes, so for an sRGB target the textures are sampled as sRGB and the vertex
//! colours converted in the shader, and either way the colours come out as authored.

use super::frame::AvatarFrame;
use super::library::AvatarAsset;
use super::mesh::BatchKind;
use crate::backdrop::Backdrop;
use crate::render::{Camera, QuadVertex};
use bytemuck::{Pod, Zeroable};
use fmv_ff7::RgbaImage;
use std::collections::HashMap;
use std::sync::Arc;

/// WGSL of the models: skinned camera-space positions in, the field camera's projection applied.
pub const MODEL_SHADER_WGSL: &str = r#"
struct Globals {
    // clip.x = proj.x * cam.x + proj.y * cam.z; clip.y = proj.z * cam.y + proj.w * cam.z
    proj: vec4<f32>,
    // near, 1 / (far - near), overlay depth bias (camera units), 1.0 for an sRGB target
    depth: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var model_tex: texture_2d<f32>;
@group(1) @binding(1) var model_samp: sampler;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_model(
    @location(0) cam: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) overlay: f32,
) -> VsOut {
    var out: VsOut;
    let w = cam.z;
    // Overlays (faces) are pulled a hair towards the camera so they win over the skin below them.
    let depth = clamp((cam.z - overlay * g.depth.z - g.depth.x) * g.depth.y, 0.0, 1.0);
    out.position = vec4<f32>(
        g.proj.x * cam.x + g.proj.y * cam.z,
        g.proj.z * cam.y + g.proj.w * cam.z,
        depth * w,
        w,
    );
    out.uv = uv;
    out.color = color;
    return out;
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4)), c / 12.92, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_model(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(model_tex, model_samp, in.uv);
    if (texel.a < 0.5) {
        discard;
    }
    var rgb = in.color.rgb;
    if (g.depth.w > 0.5) {
        rgb = to_linear(rgb);
    }
    return vec4<f32>(rgb * texel.rgb, in.color.a);
}
"#;

/// WGSL of the front layer: the picture's quad, sampled with its alpha (premultiplied).
pub const FRONT_SHADER_WGSL: &str = r#"
struct FrontOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var front_tex: texture_2d<f32>;
@group(0) @binding(1) var front_samp: sampler;

@vertex
fn vs_front(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> FrontOut {
    var out: FrontOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_front(in: FrontOut) -> @location(0) vec4<f32> {
    return textureSample(front_tex, front_samp, in.uv);
}
"#;

/// Per-vertex data that never changes for a character.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct StaticVertex {
    pub uv: [f32; 2],
    /// Vertex colour in sRGB bytes; alpha is a semi-transparent part's constant alpha.
    pub color: [u8; 4],
    /// 1.0 for the vertices of overlay parts (see `MergedMesh::overlay`).
    pub overlay: f32,
}

const STATIC_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![1 => Float32x2, 2 => Unorm8x4, 3 => Float32];
const POSITION_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x3];

fn position_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<[f32; 3]>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &POSITION_ATTRIBUTES,
    }
}

fn static_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<StaticVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &STATIC_ATTRIBUTES,
    }
}

/// The uniform block of [`MODEL_SHADER_WGSL`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Globals {
    pub proj: [f32; 4],
    pub depth: [f32; 4],
}

/// The projection terms of the shader's `Globals::proj` for the fitted window camera `cam`, the
/// field camera's focal length `zoom` and a `vw` x `vh` window (see the module docs).
pub fn projection_terms(cam: &Camera, zoom: f32, vw: u32, vh: u32) -> [f32; 4] {
    let (hw, hh) = (vw.max(1) as f32 * 0.5, vh.max(1) as f32 * 0.5);
    let (centre, scale) = (cam.centre(), cam.pixels_per_unit());
    [zoom * scale / hw, -centre[0] * scale / hw, -zoom * scale / hh, -centre[1] * scale / hh]
}

impl Globals {
    pub fn new(cam: &Camera, frame: &AvatarFrame, vw: u32, vh: u32, srgb: bool) -> Self {
        let range = (frame.far - frame.near).max(1e-3);
        Self {
            proj: projection_terms(cam, frame.zoom, vw, vh),
            depth: [frame.near, 1.0 / range, frame.overlay_bias, if srgb { 1.0 } else { 0.0 }],
        }
    }
}

/// Most characters kept on the GPU (the table has eight); beyond it the cache starts over.
const MAX_GPU_MODELS: usize = 32;
const MIN_POSITION_BYTES: u64 = 64 * 1024;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

struct GpuBatch {
    kind: BatchKind,
    first_index: u32,
    index_count: u32,
    bind_group: wgpu::BindGroup,
}

struct GpuModel {
    static_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    vertex_count: u32,
    batches: Vec<GpuBatch>,
    /// Own the pixels the bind groups look at.
    _textures: Vec<wgpu::Texture>,
}

struct GpuFront {
    id: u64,
    bind_group: wgpu::BindGroup,
    _texture: wgpu::Texture,
}

struct DepthTarget {
    size: (u32, u32),
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// Pipelines, buffers and the per-character GPU copies.
pub struct AvatarGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    texture_format: wgpu::TextureFormat,
    srgb: bool,
    texture_layout: wgpu::BindGroupLayout,
    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    opaque: wgpu::RenderPipeline,
    alpha: wgpu::RenderPipeline,
    additive: wgpu::RenderPipeline,
    front_pipeline: wgpu::RenderPipeline,
    model_sampler: wgpu::Sampler,
    front_sampler: wgpu::Sampler,
    quad_buf: wgpu::Buffer,
    white: wgpu::BindGroup,
    _white_texture: wgpu::Texture,
    models: HashMap<u64, GpuModel>,
    front: Option<GpuFront>,
    position_buf: wgpu::Buffer,
    position_cap: u64,
    depth: Option<DepthTarget>,
}

fn texture_entries() -> [wgpu::BindGroupLayoutEntry; 2] {
    [
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
    ]
}

impl AvatarGpu {
    /// Build the pipelines for targets of `format`.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let srgb = format.is_srgb();
        let texture_format =
            if srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm };

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fmv-avatar-globals-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fmv-avatar-texture-layout"),
            entries: &texture_entries(),
        });
        let model_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fmv-avatar-pipeline-layout"),
            bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fmv-avatar-shader"),
            source: wgpu::ShaderSource::Wgsl(MODEL_SHADER_WGSL.into()),
        });
        let make = |label: &'static str, kind: BatchKind| {
            let blend = match kind {
                BatchKind::Opaque => None,
                BatchKind::Alpha => Some(wgpu::BlendState::ALPHA_BLENDING),
                BatchKind::Additive => Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::SrcAlpha,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent::OVER,
                }),
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&model_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_model"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(position_layout()), Some(static_layout())],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_model"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                // Some polygons of the files wind the other way round and a model is thin in
                // places: draw both sides, as the reference rasterizer of `fmv-ff7` does.
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..wgpu::PrimitiveState::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(kind == BatchKind::Opaque),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque = make("fmv-avatar-opaque", BatchKind::Opaque);
        let alpha = make("fmv-avatar-alpha", BatchKind::Alpha);
        let additive = make("fmv-avatar-additive", BatchKind::Additive);

        let front_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fmv-avatar-front-layout"),
            bind_group_layouts: &[Some(&texture_layout)],
            immediate_size: 0,
        });
        let front_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fmv-avatar-front-shader"),
            source: wgpu::ShaderSource::Wgsl(FRONT_SHADER_WGSL.into()),
        });
        let front_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fmv-avatar-front"),
            layout: Some(&front_layout),
            vertex: wgpu::VertexState {
                module: &front_shader,
                entry_point: Some("vs_front"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(quad_layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &front_shader,
                entry_point: Some("fs_front"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let model_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fmv-avatar-sampler"),
            // Clamped, not repeated: a decal (an eye, a mouth) at the edge of its texture must not
            // blend with the texels of the opposite edge when filtered.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });
        let front_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fmv-avatar-front-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fmv-avatar-globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fmv-avatar-globals-bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals_buf.as_entire_binding() }],
        });
        let quad_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fmv-avatar-front-quad"),
            size: (6 * std::mem::size_of::<QuadVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Untextured parts sample a 1x1 white texture (so every draw has the same bindings).
        let white_texture = upload_texture(device, queue, texture_format, 1, 1, &[255, 255, 255, 255]);
        let white = texture_bind_group(device, &texture_layout, &white_texture, &model_sampler);
        let position_buf = create_position_buffer(device, MIN_POSITION_BYTES);
        Self {
            device: device.clone(),
            queue: queue.clone(),
            texture_format,
            srgb,
            texture_layout,
            globals_buf,
            globals_bind,
            opaque,
            alpha,
            additive,
            front_pipeline,
            model_sampler,
            front_sampler,
            quad_buf,
            white,
            _white_texture: white_texture,
            models: HashMap::new(),
            front: None,
            position_buf,
            position_cap: MIN_POSITION_BYTES,
            depth: None,
        }
    }

    /// Characters currently on the GPU.
    #[cfg(test)]
    pub fn cached_models(&self) -> usize {
        self.models.len()
    }

    /// Make sure `asset` is on the GPU (uploaded on first use).
    fn ensure_model(&mut self, asset: &AvatarAsset) {
        if self.models.contains_key(&asset.id) {
            return;
        }
        let (device, queue) = (self.device.clone(), self.queue.clone());
        let (device, queue) = (&device, &queue);
        if self.models.len() >= MAX_GPU_MODELS {
            self.models.clear();
        }
        let mesh = &asset.mesh;
        let statics: Vec<StaticVertex> = (0..mesh.positions.len())
            .map(|i| StaticVertex {
                uv: mesh.uvs.get(i).copied().unwrap_or([0.0; 2]),
                color: mesh.colors.get(i).copied().unwrap_or([255; 4]),
                overlay: mesh.overlay.get(i).copied().unwrap_or(0.0),
            })
            .collect();
        let static_buf = create_buffer_with(device, queue, "fmv-avatar-static", wgpu::BufferUsages::VERTEX, bytemuck::cast_slice(&statics));
        let index_buf = create_buffer_with(device, queue, "fmv-avatar-indices", wgpu::BufferUsages::INDEX, bytemuck::cast_slice(&mesh.indices));
        let mut textures: Vec<wgpu::Texture> = Vec::new();
        let mut uploaded: Vec<(*const RgbaImage, wgpu::BindGroup)> = Vec::new();
        let mut batches = Vec::with_capacity(mesh.batches.len());
        for batch in &mesh.batches {
            let bind_group = match &batch.texture {
                None => None,
                Some(image) => {
                    let key = Arc::as_ptr(image);
                    match uploaded.iter().find(|(k, _)| *k == key) {
                        Some((_, group)) => Some(group.clone()),
                        None => {
                            let ok = image.width > 0
                                && image.height > 0
                                && image.rgba.len() as u64 == u64::from(image.width) * u64::from(image.height) * 4
                                && image.width.max(image.height) <= device.limits().max_texture_dimension_2d;
                            if ok {
                                let texture = upload_texture(device, queue, self.texture_format, image.width, image.height, &image.rgba);
                                let group = texture_bind_group(device, &self.texture_layout, &texture, &self.model_sampler);
                                textures.push(texture);
                                uploaded.push((key, group.clone()));
                                Some(group)
                            } else {
                                None
                            }
                        }
                    }
                }
            };
            batches.push(GpuBatch {
                kind: batch.kind,
                first_index: batch.first_index,
                index_count: batch.index_count,
                bind_group: bind_group.unwrap_or_else(|| self.white.clone()),
            });
        }
        let vertex_count = u32::try_from(mesh.positions.len()).unwrap_or(u32::MAX);
        self.models.insert(asset.id, GpuModel { static_buf, index_buf, vertex_count, batches, _textures: textures });
    }

    /// Make sure `front` is the picture on the GPU for the front layer. `false` if it cannot be used.
    fn ensure_front(&mut self, front: &Backdrop) -> bool {
        if self.front.as_ref().is_some_and(|f| f.id == front.id) {
            return true;
        }
        self.front = None;
        let (device, queue) = (&self.device, &self.queue);
        if front.width.max(front.height) > device.limits().max_texture_dimension_2d {
            return false;
        }
        let texture = upload_texture(device, queue, self.texture_format, front.width, front.height, &front.rgba);
        let bind_group = texture_bind_group(device, &self.texture_layout, &texture, &self.front_sampler);
        self.front = Some(GpuFront { id: front.id, bind_group, _texture: texture });
        true
    }

    /// The depth buffer for a `size` target (made again when the size changes).
    fn depth_view(&mut self, size: (u32, u32)) -> wgpu::TextureView {
        let depth = match self.depth.take() {
            Some(depth) if depth.size == size => depth,
            _ => {
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("fmv-avatar-depth"),
                    size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: DEPTH_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                DepthTarget { size, _texture: texture, view }
            }
        };
        let view = depth.view.clone();
        self.depth = Some(depth);
        view
    }

    /// Draw `frame` over what `target` (a `size` window fitted by `cam`) holds: the models, then the
    /// front layer through `front_quad` (the same quad as the picture's, see `render::backdrop_quad`).
    pub fn draw(
        &mut self,
        target: &wgpu::TextureView,
        size: (u32, u32),
        cam: &Camera,
        frame: &AvatarFrame,
        front_quad: Option<&[QuadVertex; 6]>,
    ) {
        // Nothing to draw, so nothing to put the scenery in front of either.
        if size.0 == 0 || size.1 == 0 || frame.models.is_empty() {
            return;
        }
        self.draw_models(target, size, cam, frame);
        if let (Some(front), Some(quad)) = (&frame.front, front_quad) {
            if self.ensure_front(front) {
                self.draw_front(target, quad);
            }
        }
    }

    fn draw_models(&mut self, target: &wgpu::TextureView, size: (u32, u32), cam: &Camera, frame: &AvatarFrame) {
        for model in &frame.models {
            self.ensure_model(&model.asset);
        }
        self.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&Globals::new(cam, frame, size.0, size.1, self.srgb)));

        // One buffer holds every player's skinned positions, one after the other.
        let total_vertices: usize = frame.models.iter().map(|m| m.positions.len()).sum();
        let need = (total_vertices * std::mem::size_of::<[f32; 3]>()) as u64;
        if need > self.position_cap {
            let cap = need.next_power_of_two().max(MIN_POSITION_BYTES);
            self.position_buf = create_position_buffer(&self.device, cap);
            self.position_cap = cap;
        }
        let depth_view = self.depth_view(size);

        let mut spans: Vec<(u64, u64, &GpuModel)> = Vec::with_capacity(frame.models.len());
        let mut offset = 0u64;
        for model in &frame.models {
            let bytes: &[u8] = bytemuck::cast_slice(&model.positions);
            let end = offset + bytes.len() as u64;
            // A model whose positions do not match its uploaded mesh (cannot happen: both come from
            // the same asset) is skipped rather than drawn with the wrong vertices.
            if let Some(gpu) = self.models.get(&model.asset.id).filter(|g| g.vertex_count as usize == model.positions.len()) {
                if !bytes.is_empty() {
                    self.queue.write_buffer(&self.position_buf, offset, bytes);
                    spans.push((offset, end, gpu));
                }
            }
            offset = end;
        }

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fmv-avatar-models") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fmv-avatar-models-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.globals_bind, &[]);
            let pipelines = [(BatchKind::Opaque, &self.opaque), (BatchKind::Alpha, &self.alpha), (BatchKind::Additive, &self.additive)];
            for (kind, pipeline) in pipelines {
                pass.set_pipeline(pipeline);
                for &(from, to, gpu) in &spans {
                    if !gpu.batches.iter().any(|b| b.kind == kind && b.index_count > 0) {
                        continue;
                    }
                    pass.set_vertex_buffer(0, self.position_buf.slice(from..to));
                    pass.set_vertex_buffer(1, gpu.static_buf.slice(..));
                    pass.set_index_buffer(gpu.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    for batch in gpu.batches.iter().filter(|b| b.kind == kind && b.index_count > 0) {
                        pass.set_bind_group(1, &batch.bind_group, &[]);
                        pass.draw_indexed(batch.first_index..batch.first_index + batch.index_count, 0, 0..1);
                    }
                }
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    fn draw_front(&mut self, target: &wgpu::TextureView, quad: &[QuadVertex; 6]) {
        let Some(front) = &self.front else { return };
        self.queue.write_buffer(&self.quad_buf, 0, bytemuck::cast_slice(quad));
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fmv-avatar-front") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fmv-avatar-front-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.front_pipeline);
            pass.set_bind_group(0, &front.bind_group, &[]);
            pass.set_vertex_buffer(0, self.quad_buf.slice(..));
            pass.draw(0..6, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }
}

const QUAD_ATTRIBUTES: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];

/// The front quad's vertex layout (the picture's own: position then texture coordinate).
fn quad_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<QuadVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &QUAD_ATTRIBUTES,
    }
}

fn create_position_buffer(device: &wgpu::Device, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("fmv-avatar-positions"),
        size: bytes,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_buffer_with(device: &wgpu::Device, queue: &wgpu::Queue, label: &str, usage: wgpu::BufferUsages, bytes: &[u8]) -> wgpu::Buffer {
    // Buffer sizes must be multiples of 4; vertex and index data here always are.
    let size = (bytes.len() as u64).next_multiple_of(4).max(4);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    if !bytes.is_empty() {
        queue.write_buffer(&buffer, 0, bytes);
    }
    buffer
}

fn upload_texture(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, width: u32, height: u32, rgba: &[u8]) -> wgpu::Texture {
    let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fmv-avatar-texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        rgba,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
        size,
    );
    texture
}

fn texture_bind_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, texture: &wgpu::Texture, sampler: &wgpu::Sampler) -> wgpu::BindGroup {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fmv-avatar-texture-bind"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::Bounds;
    use fmv_core::Pos;

    #[test]
    fn the_vertex_layouts_match_the_shader() {
        assert_eq!(std::mem::size_of::<StaticVertex>(), 16);
        let s = static_layout();
        assert_eq!(s.array_stride, 16);
        assert_eq!(
            s.attributes.iter().map(|a| (a.shader_location, a.offset)).collect::<Vec<_>>(),
            [(1, 0), (2, 8), (3, 12)]
        );
        let p = position_layout();
        assert_eq!((p.array_stride, p.attributes[0].shader_location, p.attributes[0].offset), (12, 0, 0));
        assert_eq!(std::mem::size_of::<Globals>(), 32, "two vec4s, as the shader's uniform block");
        for entry in ["vs_model", "fs_model"] {
            assert!(MODEL_SHADER_WGSL.contains(entry), "{entry}");
        }
        for entry in ["vs_front", "fs_front"] {
            assert!(FRONT_SHADER_WGSL.contains(entry), "{entry}");
        }
    }

    /// The vertex shader's arithmetic on the CPU: NDC of a camera-space point.
    fn ndc(terms: [f32; 4], cam_pos: [f32; 3]) -> [f32; 2] {
        let w = cam_pos[2];
        [(terms[0] * cam_pos[0] + terms[1] * cam_pos[2]) / w, (terms[2] * cam_pos[1] + terms[3] * cam_pos[2]) / w]
    }

    #[test]
    fn the_projection_terms_reproduce_the_scenes_own_mapping() {
        // The scene maps a camera-space point to field-screen (sx, sy = y down), to view space
        // (sx, -sy) and through Camera::fit to pixels from the window centre: the shader must land
        // on exactly that pixel.
        let bounds = Bounds { min: Pos::new(-256.0, -256.0, 0.0), max: Pos::new(256.0, 256.0, 0.0) };
        for (vw, vh) in [(1100u32, 680u32), (320, 240), (1920, 1080)] {
            let cam = Camera::fit(&bounds, vw as f32, vh as f32, 24.0);
            let zoom = 380.0f32;
            let terms = projection_terms(&cam, zoom, vw, vh);
            for cam_pos in [[0.0f32, 0.0, 1000.0], [120.0, -40.0, 900.0], [-300.0, 80.0, 2500.0]] {
                let (sx, sy) = (cam_pos[0] * zoom / cam_pos[2], cam_pos[1] * zoom / cam_pos[2]);
                let px = cam.project(sx, -sy);
                let want = [px[0] / (vw as f32 * 0.5), px[1] / (vh as f32 * 0.5)];
                let got = ndc(terms, cam_pos);
                assert!((want[0] - got[0]).abs() < 1e-4 && (want[1] - got[1]).abs() < 1e-4, "{vw}x{vh} {cam_pos:?}: {want:?} vs {got:?}");
            }
        }
        // A zero-sized window does not divide by zero.
        assert!(projection_terms(&Camera::fit(&bounds, 0.0, 0.0, 24.0), 380.0, 0, 0).iter().all(|c| c.is_finite()));
    }
}
