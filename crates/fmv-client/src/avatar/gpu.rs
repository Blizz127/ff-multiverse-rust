//! The GPU side of the avatars: depth-tested 3D models through the field camera, the scenery as
//! depth-tested tiles, and the marks on the ground.
//!
//! [`AvatarGpu`] belongs to the renderer's `Gpu`. A frame with models and a [`super::scenery::Scenery`] is drawn as
//! three render passes over one `Depth32Float` attachment (see `Gpu::draw_field`):
//!
//! 1. **Scenery** ([`AvatarGpu::draw_scenery`]): clear the colour and the depth (to 1.0), draw the
//!    black base quad and every opaque tile of the field in paint order, each at its own depth,
//!    depth tested and written ([`super::scenery_gpu`]). The tile atlas and the vertex lists were
//!    uploaded once for the field.
//! 2. The renderer's flat shapes (the walkmesh overlay, the markers of players without a model),
//!    with no depth test.
//! 3. **Models** ([`AvatarGpu::draw_models`]): the marks on the ground (depth tested at the feet's
//!    depth, not written), then every skinned model of the frame, opaque batches of all players
//!    first, then the semi-transparent ones (depth-tested but not written), then the blended tiles
//!    in paint order (depth tested, not written).
//!
//! Without a scenery (a field with nothing in front of the characters, or a device that cannot hold
//! the atlas) pass 3 alone draws over the flat picture with a fresh depth buffer. Models are uploaded
//! once per character: static vertex data (texture coordinates, vertex colours, the overlay flag),
//! indices and textures stay on the GPU; only the skinned positions are written per frame, with what
//! belongs to the DRAW and not to the character: a [`Tint`] (a colour factor and an opacity) and an
//! [`Identity`] (the colour of the player the model shows, see [`super::identity`]) per model, each one
//! 16-byte instance-rate slice of a buffer of its own. A character is never copied per player.
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
//! the very mapping the picture and the walkmesh go through. A tile quad goes through the same
//! affine map from its field-screen corners (`ndc = tile.xz * screen + tile.yw`).
//!
//! # The depth
//!
//! Models, tiles and ground marks are all written with [`super::scenery::buffer_depth`] of their
//! camera depth, the engine's perspective z-buffer value `A - B / z` (near 50, far 32 000), clamped to
//! 0..1. The mapping is fixed, not fitted to the models of the frame (which would put every tile
//! outside the models' depth range at 0 or 1 and so lose the order of blended tiles), and it is
//! exact per pixel because `1 / z` is affine in screen space; see [`super::scenery`]. A model
//! vertex's depth is that of its camera-space `z` (overlay vertices pulled `overlay_bias` towards the
//! camera); a vertex nearer than the near plane is clamped to 0, nothing is clipped by depth.
//!
//! The surface may be sRGB or not (the window prefers not): vertex colours and textures are
//! authored in sRGB bytes, so for an sRGB target the textures are sampled as sRGB and the vertex
//! colours converted in the shader, and either way the colours come out as authored.

use super::frame::AvatarFrame;
use super::ground::GroundVertex;
use super::identity::{identity_wgsl, Identity};
use super::library::AvatarAsset;
use super::mesh::BatchKind;
use super::scenery::depth_terms;
use super::scenery_gpu::SceneryGpu;
use crate::render::Camera;
use bytemuck::{Pod, Zeroable};
use fmv_ff7::RgbaImage;
use std::collections::HashMap;
use std::sync::Arc;

/// The uniform block every avatar shader reads, and the depth mapping they share. As a macro so
/// that the model, tile and ground shaders spell it once.
macro_rules! globals_wgsl {
    () => {
        r#"
struct Globals {
    // clip.x = proj.x * cam.x + proj.y * cam.z; clip.y = proj.z * cam.y + proj.w * cam.z
    proj: vec4<f32>,
    // A and B of the depth mapping (buffer depth = A - B / z), overlay depth bias (camera units),
    // 1.0 for an sRGB target
    depth: vec4<f32>,
    // tiles: ndc.x = tile.x * screen.x + tile.y, ndc.y = tile.z * screen.y + tile.w
    tile: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;

// The depth-buffer value of a camera-space depth: the engine's perspective z (near 50, far 32 000),
// clamped, so a depth below the near plane is "in front of everything", not culled.
fn buffer_depth(z: f32) -> f32 {
    return clamp(g.depth.x - g.depth.y / max(z, 1.0), 0.0, 1.0);
}
"#
    };
}
pub(super) use globals_wgsl;

/// WGSL of the models: skinned camera-space positions in, the field camera's projection applied.
pub const MODEL_SHADER_WGSL: &str = concat!(
    globals_wgsl!(),
    identity_wgsl!(),
    r#"
@group(1) @binding(0) var model_tex: texture_2d<f32>;
@group(1) @binding(1) var model_samp: sampler;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    // The draw's own colour factor and opacity (per instance, one instance per model): `flat` so that
    // the value reaches the fragment as it is, bit for bit (a white tint changes nothing).
    @location(2) @interpolate(flat) tint: vec4<f32>,
    // The draw's identity colour and its strength (per instance too; a strength of 0 is no identity colour), and
    // whether this triangle keeps its own colours (a face decal, or hair: bit 1 of the overlay attribute).
    @location(3) @interpolate(flat) identity: vec4<f32>,
    @location(4) @interpolate(flat) face: f32,
};

@vertex
fn vs_model(
    @location(0) cam: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) overlay: f32,
    @location(4) tint: vec4<f32>,
    @location(5) identity: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    let w = cam.z;
    // The attribute packs two flags (see `overlay_flags`): bit 1 keeps the vertex's own colours under an identity colour
    // (a face decal, hair), bit 0 pulls it a hair towards the camera so a face wins over the skin below it.
    let keep = step(1.5, overlay);
    let pull = overlay - 2.0 * keep;
    let depth = buffer_depth(cam.z - pull * g.depth.z);
    out.position = vec4<f32>(
        g.proj.x * cam.x + g.proj.y * cam.z,
        g.proj.z * cam.y + g.proj.w * cam.z,
        depth * w,
        w,
    );
    out.uv = uv;
    out.color = color;
    out.tint = tint;
    out.identity = identity;
    out.face = keep;
    return out;
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4)), c / 12.92, c <= vec3<f32>(0.04045));
}

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055), c * 12.92, c <= vec3<f32>(0.0031308));
}

@fragment
fn fs_model(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(model_tex, model_samp, in.uv);
    if (texel.a < 0.5) {
        discard;
    }
    var rgb = in.color.rgb;
    let srgb = g.depth.w > 0.5;
    if (srgb) {
        rgb = to_linear(rgb);
    }
    var lit = rgb * texel.rgb;
    // The player's identity colour repaints the cloth (a strength of 0 leaves the product exactly as it is). It is
    // worked out on the authored (sRGB-encoded) colours, so an sRGB target shows the picture a plain one does.
    if (in.identity.w > 0.0) {
        if (srgb) {
            lit = to_linear(identity_look(to_srgb(lit), in.identity.rgb, in.identity.w, in.face));
        } else {
            lit = identity_look(lit, in.identity.rgb, in.identity.w, in.face);
        }
    }
    // The draw's tint scales the colours (above 1 it brightens) and its alpha the part's own.
    return vec4<f32>(lit * in.tint.rgb, in.color.a * in.tint.a);
}
"#
);

/// WGSL of the ground marks: flat-colour triangles placed in NDC, at the depth of the feet.
pub const GROUND_SHADER_WGSL: &str = concat!(
    globals_wgsl!(),
    r#"
struct GroundOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_ground(@location(0) ndc: vec2<f32>, @location(1) color: vec4<f32>, @location(2) depth: f32) -> GroundOut {
    var out: GroundOut;
    out.position = vec4<f32>(ndc, buffer_depth(depth), 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_ground(in: GroundOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#
);

/// Per-vertex data that never changes for a character.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct StaticVertex {
    pub uv: [f32; 2],
    /// Vertex colour in sRGB bytes; alpha is a semi-transparent part's constant alpha.
    pub color: [u8; 4],
    /// Two flags in one float, [`overlay_flags`]: 3.0 on the vertices of overlay parts (see `MergedMesh::overlay`: a
    /// face decal is pulled towards the camera and keeps its colours), 2.0 on hair (`MergedMesh::own`: keeps its
    /// colours), 0.0 elsewhere.
    pub overlay: f32,
}

/// The value of the static `overlay` attribute for a vertex with these mesh flags: bit 0 (+1) pulls it a hair towards the
/// camera, bit 1 (+2) keeps its own colours under a player's identity colour. A decal does both (3.0), hair only the second
/// (2.0). The vertex shader takes it apart again, so the vertex stays 16 bytes.
pub fn overlay_flags(overlay: f32, own: f32) -> f32 {
    let decal = overlay > 0.5;
    f32::from(u8::from(decal)) + if decal || own > 0.5 { 2.0 } else { 0.0 }
}

const STATIC_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![1 => Float32x2, 2 => Unorm8x4, 3 => Float32];
const POSITION_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x3];
const TINT_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![4 => Float32x4];
const IDENTITY_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![5 => Float32x4];

/// The per-draw tint of one model: a colour factor (`rgb`, above 1 it brightens) and an opacity
/// (`alpha`). A model whose alpha is under 1 is drawn blended (all its batches, the opaque ones
/// too), after the opaque models; an additive part of it stays additive and fades the same way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tint {
    pub rgb: [f32; 3],
    pub alpha: f32,
}

impl Tint {
    /// Drawn as it is.
    pub const NONE: Tint = Tint { rgb: [1.0; 3], alpha: 1.0 };

    /// The four floats the shader reads.
    pub fn to_array(self) -> [f32; 4] {
        [self.rgb[0], self.rgb[1], self.rgb[2], self.alpha]
    }
}

impl Default for Tint {
    fn default() -> Self {
        Self::NONE
    }
}

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

/// The tint: one `[f32; 4]` per INSTANCE, and a model is drawn as the one instance of a 16-byte slice
/// of the buffer (no `first_instance`, which the downlevel limits the renderer asks for do not promise).
fn tint_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<[f32; 4]>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &TINT_ATTRIBUTES,
    }
}

/// The identity colour ([`Identity`]): the same, one `[f32; 4]` per INSTANCE in a buffer of its own, a model drawn
/// as the one instance of its 16-byte slice.
fn identity_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<[f32; 4]>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &IDENTITY_ATTRIBUTES,
    }
}

/// The uniform block the avatar shaders share (see `globals_wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Globals {
    pub proj: [f32; 4],
    pub depth: [f32; 4],
    pub tile: [f32; 4],
}

/// The projection terms of the shader's `Globals::proj` for the fitted window camera `cam`, the
/// field camera's focal length `zoom` and a `vw` x `vh` window (see the module docs).
pub fn projection_terms(cam: &Camera, zoom: f32, vw: u32, vh: u32) -> [f32; 4] {
    let (hw, hh) = (vw.max(1) as f32 * 0.5, vh.max(1) as f32 * 0.5);
    let (centre, scale) = (cam.centre(), cam.pixels_per_unit());
    [zoom * scale / hw, -centre[0] * scale / hw, -zoom * scale / hh, -centre[1] * scale / hh]
}

/// The terms of the shader's `Globals::tile` for the fitted window camera `cam` and a `vw` x `vh`
/// window: the same map the picture's quad goes through, from field-screen units (x right, y DOWN,
/// so view space is `(x, -y)`) to NDC.
pub fn tile_terms(cam: &Camera, vw: u32, vh: u32) -> [f32; 4] {
    let (hw, hh) = (vw.max(1) as f32 * 0.5, vh.max(1) as f32 * 0.5);
    let (centre, scale) = (cam.centre(), cam.pixels_per_unit());
    [scale / hw, -centre[0] * scale / hw, -scale / hh, -centre[1] * scale / hh]
}

impl Globals {
    pub fn new(cam: &Camera, frame: &AvatarFrame, vw: u32, vh: u32, srgb: bool) -> Self {
        let [a, b] = depth_terms();
        Self {
            proj: projection_terms(cam, frame.zoom, vw, vh),
            depth: [a, b, frame.overlay_bias, if srgb { 1.0 } else { 0.0 }],
            tile: tile_terms(cam, vw, vh),
        }
    }
}

/// Most models kept on the GPU, whoever they are: the players' (the 8 canon characters and up to 4 placeholder
/// bodies) and the area's non-player characters (`library::MAX_NPC_MODELS`, which is what this leaves them);
/// beyond it the cache starts over.
pub(crate) const MAX_GPU_MODELS: usize = 32;
const MIN_POSITION_BYTES: u64 = 64 * 1024;
/// Room for 256 tints (and as many identity colours) to start with (a battle draws a few dozen models).
const MIN_TINT_BYTES: u64 = 256 * 16;
pub(super) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

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
    ground: wgpu::RenderPipeline,
    model_sampler: wgpu::Sampler,
    white: wgpu::BindGroup,
    _white_texture: wgpu::Texture,
    models: HashMap<u64, GpuModel>,
    scenery: SceneryGpu,
    position_buf: wgpu::Buffer,
    position_cap: u64,
    /// One tint per model of a frame (see [`tint_layout`]).
    tint_buf: wgpu::Buffer,
    tint_cap: u64,
    /// One identity colour per model of a frame (see [`identity_layout`]).
    identity_buf: wgpu::Buffer,
    identity_cap: u64,
    ground_buf: wgpu::Buffer,
    ground_cap: u64,
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
                    buffers: &[Some(position_layout()), Some(static_layout()), Some(tint_layout()), Some(identity_layout())],
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

        // The marks on the ground: flat-colour triangles with a depth of their own, blended, tested
        // against the scenery and not written.
        let ground_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fmv-avatar-ground-layout"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });
        let ground_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fmv-avatar-ground-shader"),
            source: wgpu::ShaderSource::Wgsl(GROUND_SHADER_WGSL.into()),
        });
        let ground = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fmv-avatar-ground"),
            layout: Some(&ground_layout),
            vertex: wgpu::VertexState {
                module: &ground_shader,
                entry_point: Some("vs_ground"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(ground_layout_of_vertices())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &ground_shader,
                entry_point: Some("fs_ground"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let scenery = SceneryGpu::new(device, format, &model_layout);

        let model_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fmv-avatar-sampler"),
            // Clamped, not repeated: a decal (an eye, a mouth) at the edge of its texture must not
            // blend with the texels of the opposite edge when filtered. (The models shift a decal's
            // coordinates by whole numbers, which the game's wrap-around addressing ignores: the mesh
            // merge takes those off, `mesh::whole_cell_offsets`, so that nothing is read past the edge.)
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
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
        // Untextured parts sample a 1x1 white texture (so every draw has the same bindings).
        let white_texture = upload_texture(device, queue, texture_format, 1, 1, &[255, 255, 255, 255]);
        let white = texture_bind_group(device, &texture_layout, &white_texture, &model_sampler);
        let position_buf = create_position_buffer(device, MIN_POSITION_BYTES);
        let tint_buf = create_vertex_buffer(device, "fmv-avatar-tints", MIN_TINT_BYTES);
        let identity_buf = create_vertex_buffer(device, "fmv-avatar-identities", MIN_TINT_BYTES);
        let ground_buf = create_vertex_buffer(device, "fmv-avatar-ground-vertices", MIN_POSITION_BYTES);
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
            ground,
            model_sampler,
            white,
            _white_texture: white_texture,
            models: HashMap::new(),
            scenery,
            position_buf,
            position_cap: MIN_POSITION_BYTES,
            tint_buf,
            tint_cap: MIN_TINT_BYTES,
            identity_buf,
            identity_cap: MIN_TINT_BYTES,
            ground_buf,
            ground_cap: MIN_POSITION_BYTES,
            depth: None,
        }
    }

    /// Characters currently on the GPU.
    #[cfg(test)]
    pub fn cached_models(&self) -> usize {
        self.models.len()
    }

    /// The scenery on the GPU (uploads so far, the identity of the current one, draw calls of a frame).
    #[cfg(test)]
    pub fn scenery_state(&self) -> (u32, Option<u64>, usize) {
        (self.scenery.uploads(), self.scenery.current_id(), self.scenery.draw_calls())
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
                overlay: overlay_flags(mesh.overlay.get(i).copied().unwrap_or(0.0), mesh.own.get(i).copied().unwrap_or(0.0)),
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

    /// Make sure the frame's scenery is on the GPU (uploaded once per field). `false` when the frame
    /// has none or this device cannot hold its atlas: the frame is then drawn over the flat picture.
    pub fn prepare_scenery(&mut self, frame: &AvatarFrame) -> bool {
        let Some(scenery) = frame.scenery.as_deref() else { return false };
        self.scenery.ensure(&self.device, &self.queue, self.texture_format, &self.texture_layout, &self.model_sampler, scenery)
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

    /// Write the frame's globals (the fitted camera, the depth mapping) for the passes of this frame.
    fn write_globals(&self, size: (u32, u32), cam: &Camera, frame: &AvatarFrame) {
        self.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&Globals::new(cam, frame, size.0, size.1, self.srgb)));
    }

    /// Pass 1: clear `target` to `clear` and the depth to 1.0, then draw the scenery's base and every
    /// opaque tile with depth ([`AvatarGpu::prepare_scenery`] said yes). The depth is kept for
    /// [`AvatarGpu::draw_models`], which must follow with `with_scenery`.
    pub fn draw_scenery(&mut self, target: &wgpu::TextureView, size: (u32, u32), cam: &Camera, frame: &AvatarFrame, clear: wgpu::Color) {
        if size.0 == 0 || size.1 == 0 {
            return;
        }
        self.write_globals(size, cam, frame);
        let depth_view = self.depth_view(size);
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fmv-avatar-scenery") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fmv-avatar-scenery-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.globals_bind, &[]);
            self.scenery.draw_opaque(&mut pass);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Pass 3: draw `frame` over what `target` (a `size` window fitted by `cam`) holds: the marks on
    /// the ground, the models and, with `with_scenery`, the blended tiles of the scenery over them.
    /// With `with_scenery` the depth is the one [`AvatarGpu::draw_scenery`] left (the marks, the
    /// models and the blended tiles are tested against it); without, it starts empty.
    pub fn draw_models(&mut self, target: &wgpu::TextureView, size: (u32, u32), cam: &Camera, frame: &AvatarFrame, with_scenery: bool) {
        self.draw_models_tinted(target, size, cam, frame, &frame.tints, &frame.identities, with_scenery);
    }

    /// [`AvatarGpu::draw_models`] with a [`Tint`] and an [`Identity`] for each model: `tints[i]` is the colour factor
    /// and the opacity of `frame.models[i]`, `identities[i]` the colour of the player it shows (a model without one,
    /// or every model when the list is empty, is drawn as it is, which is bit for bit what `draw_models` always
    /// drew). A model that is not fully opaque is drawn blended with everything of it, in the pass of the
    /// semi-transparent ones.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_models_tinted(
        &mut self,
        target: &wgpu::TextureView,
        size: (u32, u32),
        cam: &Camera,
        frame: &AvatarFrame,
        tints: &[Tint],
        identities: &[Identity],
        with_scenery: bool,
    ) {
        // Nothing to draw, so nothing to put the scenery in front of either.
        if size.0 == 0 || size.1 == 0 || frame.models.is_empty() {
            return;
        }
        for model in &frame.models {
            self.ensure_model(&model.asset);
        }
        self.write_globals(size, cam, frame);

        // One tint per model, in a buffer of its own, a 16-byte slice each (see `tint_layout`).
        let tint_data: Vec<[f32; 4]> = (0..frame.models.len()).map(|i| tints.get(i).copied().unwrap_or(Tint::NONE).to_array()).collect();
        let tint_need = (tint_data.len() * std::mem::size_of::<[f32; 4]>()) as u64;
        if tint_need > self.tint_cap {
            let cap = tint_need.next_power_of_two().max(MIN_TINT_BYTES);
            self.tint_buf = create_vertex_buffer(&self.device, "fmv-avatar-tints", cap);
            self.tint_cap = cap;
        }
        self.queue.write_buffer(&self.tint_buf, 0, bytemuck::cast_slice(&tint_data));
        // And one identity colour per model, the same way.
        let identity_data: Vec<[f32; 4]> = (0..frame.models.len()).map(|i| identities.get(i).copied().unwrap_or(Identity::NONE).to_array()).collect();
        let identity_need = (identity_data.len() * std::mem::size_of::<[f32; 4]>()) as u64;
        if identity_need > self.identity_cap {
            let cap = identity_need.next_power_of_two().max(MIN_TINT_BYTES);
            self.identity_buf = create_vertex_buffer(&self.device, "fmv-avatar-identities", cap);
            self.identity_cap = cap;
        }
        self.queue.write_buffer(&self.identity_buf, 0, bytemuck::cast_slice(&identity_data));

        // One buffer holds every player's skinned positions, one after the other.
        let total_vertices: usize = frame.models.iter().map(|m| m.positions.len()).sum();
        let need = (total_vertices * std::mem::size_of::<[f32; 3]>()) as u64;
        if need > self.position_cap {
            let cap = need.next_power_of_two().max(MIN_POSITION_BYTES);
            self.position_buf = create_position_buffer(&self.device, cap);
            self.position_cap = cap;
        }
        // The marks, with the depth of the feet they lie around.
        let marks = frame.ground_vertices(cam, size.0, size.1, self.srgb);
        let mark_bytes: &[u8] = bytemuck::cast_slice(&marks);
        let mark_need = mark_bytes.len() as u64;
        if mark_need > self.ground_cap {
            let cap = mark_need.next_power_of_two().max(MIN_POSITION_BYTES);
            self.ground_buf = create_vertex_buffer(&self.device, "fmv-avatar-ground-vertices", cap);
            self.ground_cap = cap;
        }
        if !mark_bytes.is_empty() {
            self.queue.write_buffer(&self.ground_buf, 0, mark_bytes);
        }
        let depth_view = self.depth_view(size);

        // (first byte, end byte, the model on the GPU, its index in the frame: that of its tint)
        let mut spans: Vec<(u64, u64, &GpuModel, usize)> = Vec::with_capacity(frame.models.len());
        let mut offset = 0u64;
        for (index, model) in frame.models.iter().enumerate() {
            let bytes: &[u8] = bytemuck::cast_slice(&model.positions);
            let end = offset + bytes.len() as u64;
            // A model whose positions do not match its uploaded mesh (cannot happen: both come from
            // the same asset) is skipped rather than drawn with the wrong vertices.
            if let Some(gpu) = self.models.get(&model.asset.id).filter(|g| g.vertex_count as usize == model.positions.len()) {
                if !bytes.is_empty() {
                    self.queue.write_buffer(&self.position_buf, offset, bytes);
                    spans.push((offset, end, gpu, index));
                }
            }
            offset = end;
        }

        let depth_load = if with_scenery { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(1.0) };
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
                    depth_ops: Some(wgpu::Operations { load: depth_load, store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.globals_bind, &[]);
            // Under the models: the marks on the ground (`set_vertex_buffer` panics on an empty slice).
            if !marks.is_empty() {
                pass.set_pipeline(&self.ground);
                pass.set_vertex_buffer(0, self.ground_buf.slice(0..mark_need));
                pass.draw(0..u32::try_from(marks.len()).unwrap_or(u32::MAX), 0..1);
            }
            let pipelines = [(BatchKind::Opaque, &self.opaque), (BatchKind::Alpha, &self.alpha), (BatchKind::Additive, &self.additive)];
            for (kind, pipeline) in pipelines {
                pass.set_pipeline(pipeline);
                for &(from, to, gpu, index) in &spans {
                    // A model that is not fully opaque is blended all over: its opaque batches go with the
                    // semi-transparent ones, over everything that is opaque.
                    let faded = tint_data[index][3] < 1.0;
                    let drawn_now = |b: &GpuBatch| b.index_count > 0 && if faded && b.kind == BatchKind::Opaque { kind == BatchKind::Alpha } else { b.kind == kind };
                    if !gpu.batches.iter().any(drawn_now) {
                        continue;
                    }
                    let tint_at = (index * std::mem::size_of::<[f32; 4]>()) as u64;
                    pass.set_vertex_buffer(0, self.position_buf.slice(from..to));
                    pass.set_vertex_buffer(1, gpu.static_buf.slice(..));
                    pass.set_vertex_buffer(2, self.tint_buf.slice(tint_at..tint_at + std::mem::size_of::<[f32; 4]>() as u64));
                    pass.set_vertex_buffer(3, self.identity_buf.slice(tint_at..tint_at + std::mem::size_of::<[f32; 4]>() as u64));
                    pass.set_index_buffer(gpu.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    for batch in gpu.batches.iter().filter(|b| drawn_now(b)) {
                        pass.set_bind_group(1, &batch.bind_group, &[]);
                        pass.draw_indexed(batch.first_index..batch.first_index + batch.index_count, 0, 0..1);
                    }
                }
            }
            // Over the models: the blended tiles (glows, shadows, glass) that are nearer than what they cover.
            if with_scenery {
                self.scenery.draw_blended(&mut pass);
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }
}

const GROUND_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4, 2 => Float32];

/// The ground marks' vertex layout: NDC position, colour, camera depth.
fn ground_layout_of_vertices() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<GroundVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &GROUND_ATTRIBUTES,
    }
}

fn create_vertex_buffer(device: &wgpu::Device, label: &str, bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_position_buffer(device: &wgpu::Device, bytes: u64) -> wgpu::Buffer {
    create_vertex_buffer(device, "fmv-avatar-positions", bytes)
}

pub(super) fn create_buffer_with(device: &wgpu::Device, queue: &wgpu::Queue, label: &str, usage: wgpu::BufferUsages, bytes: &[u8]) -> wgpu::Buffer {
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

pub(super) fn upload_texture(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, width: u32, height: u32, rgba: &[u8]) -> wgpu::Texture {
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

pub(super) fn texture_bind_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, texture: &wgpu::Texture, sampler: &wgpu::Sampler) -> wgpu::BindGroup {
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
        // The per-draw tint: one vec4 per INSTANCE at location 4, which the vertex shader declares and passes
        // on flat (so a white tint reaches the fragment exactly).
        let t = tint_layout();
        assert_eq!((t.array_stride, t.step_mode, t.attributes[0].shader_location, t.attributes[0].offset), (16, wgpu::VertexStepMode::Instance, 4, 0));
        assert!(MODEL_SHADER_WGSL.contains("@location(4) tint: vec4<f32>") && MODEL_SHADER_WGSL.contains("@interpolate(flat) tint"));
        // The identity colour: a second vec4 per INSTANCE at location 5, in a buffer of its own (a second slice of 16
        // bytes per model), passed on flat with the face flag (the overlay attribute at location 3, which the
        // fragment stage now needs too).
        let i = identity_layout();
        assert_eq!((i.array_stride, i.step_mode, i.attributes[0].shader_location, i.attributes[0].offset), (16, wgpu::VertexStepMode::Instance, 5, 0));
        assert_eq!(i.attributes[0].format, wgpu::VertexFormat::Float32x4);
        for text in [
            "@location(5) identity: vec4<f32>",
            "@location(3) @interpolate(flat) identity: vec4<f32>",
            "@location(4) @interpolate(flat) face: f32",
            "let keep = step(1.5, overlay);",
            "let pull = overlay - 2.0 * keep;",
            "buffer_depth(cam.z - pull * g.depth.z)",
            "out.face = keep;",
            "out.identity = identity;",
        ] {
            assert!(MODEL_SHADER_WGSL.contains(text), "{text}");
        }
        // The two flags of the overlay attribute, as the vertex shader takes them apart: bit 1 (2.0) keeps the colours,
        // bit 0 (1.0) pulls the vertex towards the camera. A decal does both, hair only the first, anything else neither.
        let apart = |flags: f32| {
            let keep = if flags >= 1.5 { 1.0f32 } else { 0.0 };
            (keep, flags - 2.0 * keep)
        };
        assert_eq!(apart(overlay_flags(0.0, 0.0)), (0.0, 0.0), "cloth and skin: neither");
        assert_eq!(apart(overlay_flags(0.0, 1.0)), (1.0, 0.0), "hair keeps its colours and is not pulled towards the camera");
        assert_eq!(apart(overlay_flags(1.0, 0.0)), (1.0, 1.0), "a decal keeps its colours and is pulled");
        assert_eq!(apart(overlay_flags(1.0, 1.0)), (1.0, 1.0));
        assert_eq!((overlay_flags(0.0, 0.0), overlay_flags(0.0, 1.0), overlay_flags(1.0, 0.0)), (0.0, 2.0, 3.0));
        // Strength 0 skips the look entirely: the colour path is the old product, `rgb * texel.rgb`, then the tint.
        assert!(MODEL_SHADER_WGSL.contains("if (in.identity.w > 0.0) {") && MODEL_SHADER_WGSL.contains("vec4<f32>(lit * in.tint.rgb, in.color.a * in.tint.a)"));
        assert!(MODEL_SHADER_WGSL.contains("var lit = rgb * texel.rgb;"));
        // All of them, in the order the pipelines list the buffers (position, static, tint, identity).
        assert_eq!(Identity::NONE.to_array(), [1.0, 1.0, 1.0, 0.0]);
        assert_eq!(Tint::default().to_array(), [1.0; 4]);
        assert_eq!(Tint { rgb: [0.5, 1.0, 2.0], alpha: 0.25 }.to_array(), [0.5, 1.0, 2.0, 0.25]);
        assert_eq!(std::mem::size_of::<Globals>(), 48, "three vec4s, as the shaders' uniform block");
        for entry in ["vs_model", "fs_model", "buffer_depth"] {
            assert!(MODEL_SHADER_WGSL.contains(entry), "{entry}");
        }
        // The ground marks: NDC position, colour, camera depth.
        assert_eq!(std::mem::size_of::<GroundVertex>(), 28);
        let g = ground_layout_of_vertices();
        assert_eq!(g.array_stride, 28);
        assert_eq!(g.attributes.iter().map(|a| (a.shader_location, a.offset)).collect::<Vec<_>>(), [(0, 0), (1, 8), (2, 24)]);
        for entry in ["vs_ground", "fs_ground", "buffer_depth"] {
            assert!(GROUND_SHADER_WGSL.contains(entry), "{entry}");
        }
        // All three shaders spell the uniform block and the depth mapping the same way.
        let block = globals_wgsl!();
        for shader in [MODEL_SHADER_WGSL, GROUND_SHADER_WGSL, crate::avatar::scenery_gpu::TILE_SHADER_WGSL] {
            assert!(shader.contains(block));
        }
    }

    #[test]
    fn the_tile_terms_land_a_field_screen_point_where_the_pictures_own_quad_does() {
        use crate::backdrop::Backdrop;
        use crate::render::{backdrop_quad, Scene};
        // A picture over field-screen (-64,-48)..(64,48) is, in view space (y up), the same box.
        let picture = Backdrop::new(2, 2, vec![0; 16], [-64.0, -48.0], [64.0, 48.0]).unwrap();
        for (vw, vh) in [(1100u32, 680u32), (320, 240), (1072, 1072)] {
            let scene = Scene { bounds: Bounds { min: Pos::new(-64.0, -48.0, 0.0), max: Pos::new(64.0, 48.0, 0.0) }, triangles: &[], players: &[], fixture_banner: false, backdrop: Some(&picture) };
            let quad = backdrop_quad(&scene, vw, vh).unwrap();
            let cam = Camera::fit(&scene.bounds, vw as f32, vh as f32, crate::render::MARGIN_PX);
            let terms = tile_terms(&cam, vw, vh);
            let ndc = |sx: f32, sy: f32| [terms[0] * sx + terms[1], terms[2] * sy + terms[3]];
            // Texture corner (0,0) is the field-screen top left (-64,-48); (1,1) the bottom right (64,48).
            for (uv, screen) in [([0.0, 0.0], (-64.0, -48.0)), ([1.0, 0.0], (64.0, -48.0)), ([0.0, 1.0], (-64.0, 48.0)), ([1.0, 1.0], (64.0, 48.0))] {
                let want = quad.iter().find(|v| v.uv == uv).unwrap().pos;
                let got = ndc(screen.0, screen.1);
                assert!((want[0] - got[0]).abs() < 1e-5 && (want[1] - got[1]).abs() < 1e-5, "{vw}x{vh} {uv:?}: {want:?} vs {got:?}");
            }
        }
        // A zero-sized window does not divide by zero.
        assert!(tile_terms(&Camera::fit(&Bounds { min: Pos::default(), max: Pos::new(1.0, 1.0, 0.0) }, 0.0, 0.0, 0.0), 0, 0).iter().all(|c| c.is_finite()));
    }

    #[test]
    fn a_model_vertex_and_a_tile_at_the_same_camera_depth_get_the_same_buffer_depth() {
        use crate::avatar::scenery::buffer_depth;
        use crate::avatar::stage::StageCamera;
        use fmv_ff7::background::depth::{depth_of_point, tile_depth};
        use fmv_ff7::background::Projection;
        use fmv_ff7::field::Camera as FieldCamera;
        // The walkmesh point's depth (what a tile is compared with in `fmv_ff7`) is the camera-space z
        // the models are skinned to: the one number both go through the one mapping with.
        let camera = FieldCamera { axes: [[4096, 0, 0], [0, -3000, 2800], [0, 2800, 3000]], position: [-120, 4200, 9500], zoom: 380 };
        let (stage, projection) = (StageCamera::from_camera(&camera), Projection::from_camera(&camera));
        for p in [[0.0f32, 0.0, 0.0], [1500.0, -300.0, 40.0], [-2200.0, 900.0, 122.0], [30.0, 3000.0, 310.0]] {
            let Some(point) = depth_of_point(&projection, p) else { continue };
            let model_z = stage.to_camera(p.map(f64::from))[2] as f32;
            assert!((point - model_z).abs() < 0.01, "{p:?}: {point} vs {model_z}");
            assert!((buffer_depth(point) - buffer_depth(model_z)).abs() < 1e-6);
        }
        // A tile of id `n` hides a model fragment exactly when its depth is the smaller (`<`): in the
        // buffer a fragment a unit behind the tile is behind it, a unit before it is in front.
        let tile = buffer_depth(tile_depth(2, 544));
        assert!(buffer_depth(2176.0 + 1.0) > tile && buffer_depth(2176.0 - 1.0) < tile);
        // Layer 4 (depth 0) is in front of every model fragment, layer 1 behind every one.
        assert!(buffer_depth(tile_depth(4, 0)) < buffer_depth(60.0));
        assert!(buffer_depth(tile_depth(1, 0)) > buffer_depth(15_423.0));
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
