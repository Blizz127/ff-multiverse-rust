//! Renderer: a tiny wgpu renderer that draws an orthographic view of the area (an optional
//! background picture, the walkable triangles over it, players, the FIXTURE banner).
//!
//! Everything is in *view space* (+x right, +y up, any units); how an area's own coordinates
//! become view space (top-down, or through a game's field camera) is decided by
//! [`crate::backdrop::FieldView`], not here.
//!
//! * CPU side ([`Camera`], [`build_vertices`], [`backdrop_quad`]): view space -> pixels -> NDC,
//!   pure and unit-tested.
//! * GPU side ([`Gpu`]): a flat-colour pipeline with one growing vertex buffer and a textured
//!   quad pipeline for the backdrop; renders into any texture view, so it can also be exercised
//!   offscreen.
//! * Surface side ([`Renderer`]): window surface lifecycle (resize, minimise, suboptimal,
//!   outdated, lost) without panics.

use crate::backdrop::Backdrop;
use bytemuck::{Pod, Zeroable};
use fmv_adapter_api::Bounds;
use fmv_core::Pos;
use std::sync::{Arc, Mutex};
use winit::event_loop::OwnedDisplayHandle;
use winit::window::Window;

/// WGSL for the whole renderer: position is already in NDC, colour is passed through.
pub const SHADER_WGSL: &str = r#"
struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) position: vec2<f32>, @location(1) color: vec4<f32>) -> VsOut {
    var out: VsOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

/// WGSL of the backdrop quad: position is already in NDC, the picture is sampled as is (it is
/// opaque; pixels no tile touched were flattened onto black when the picture was made).
pub const QUAD_SHADER_WGSL: &str = r#"
struct QuadOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var backdrop_tex: texture_2d<f32>;
@group(0) @binding(1) var backdrop_samp: sampler;

@vertex
fn vs_quad(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> QuadOut {
    var out: QuadOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_quad(in: QuadOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(backdrop_tex, backdrop_samp, in.uv).rgb, 1.0);
}
"#;

/// One corner of the backdrop quad: NDC position and texture coordinate (`(0,0)` = top left).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct QuadVertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
}

impl QuadVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<QuadVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub color: [f32; 4],
}

impl Vertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// Upper bound on vertices per frame (24 MB), far above any walkmesh this client will see.
pub const MAX_VERTICES: usize = 1_000_000;

// Palette (sRGB bytes).
pub const CLEAR_RGB: [u8; 3] = [14, 16, 22];
pub const FLOOR_RGB: [u8; 3] = [52, 70, 96];
pub const EDGE_RGB: [u8; 3] = [92, 120, 160];
const OUTLINE_RGB: [u8; 3] = [8, 8, 12];
const OWN_OUTLINE_RGB: [u8; 3] = [255, 255, 255];
/// Over a backdrop the floor is a see-through wash and the edges are brighter, so the scenery
/// stays visible with the walkable area readable on top of it.
pub const BACKDROP_FLOOR_ALPHA: f32 = 0.30;
pub const BACKDROP_EDGE_RGB: [u8; 3] = [200, 230, 255];
pub const BANNER_YELLOW: [u8; 3] = [255, 196, 0];
pub const BANNER_BLACK: [u8; 3] = [24, 24, 24];
/// The ring drawn around every member of the player's party.
pub const RING_RGB: [u8; 3] = [255, 208, 64];

// Sizes in pixels.
/// Margin around the fitted area, in pixels (the camera every drawn thing is placed by).
pub const MARGIN_PX: f32 = 24.0;
const EDGE_PX: f32 = 1.25;
const PLAYER_HALF_PX: f32 = 9.0;
const OWN_HALF_PX: f32 = 10.0;
const OUTLINE_PX: f32 = 2.5;
pub const BANNER_PX: f32 = 18.0;
/// A party ring is a closed band (a 24-sided polygon outline) between these radii around the
/// marker's centre: a few pixels outside the furthest corner of any marker's outline (about 16.3
/// px for a player, 15.6 px for the own diamond), and thin.
const RING_INNER_PX: f32 = 18.0;
const RING_OUTER_PX: f32 = 20.5;
const RING_SEGMENTS: usize = 24;

/// One player marker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerSprite {
    pub x: f32,
    pub y: f32,
    pub tint: [u8; 3],
    /// The local player: drawn as an outlined, brightened diamond on top of everyone else.
    pub own: bool,
    /// A member of the local player's party (the local player included when in a party): drawn
    /// with a gold ring around the marker.
    pub party: bool,
}

/// Everything needed to draw one frame.
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    pub bounds: Bounds,
    pub triangles: &'a [[Pos; 3]],
    pub players: &'a [PlayerSprite],
    /// Draw the hazard-stripe FIXTURE banner across the top.
    pub fixture_banner: bool,
    /// A picture drawn behind everything, covering its own rectangle of view space.
    pub backdrop: Option<&'a Backdrop>,
}

/// Orthographic camera fitting the area bounds into the viewport, +y up, aspect preserved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    cx: f32,
    cy: f32,
    /// Pixels per world unit.
    scale: f32,
}

impl Camera {
    pub fn fit(bounds: &Bounds, vw: f32, vh: f32, margin_px: f32) -> Self {
        let (min, max) = (bounds.min, bounds.max);
        let ok = [min.x, min.y, max.x, max.y].iter().all(|v| v.is_finite());
        let (minx, miny, maxx, maxy) = if ok {
            (min.x, min.y, max.x, max.y)
        } else {
            (-1.0, -1.0, 1.0, 1.0)
        };
        let w = (maxx - minx).abs().max(1e-3);
        let h = (maxy - miny).abs().max(1e-3);
        let aw = (vw - 2.0 * margin_px).max(1.0);
        let ah = (vh - 2.0 * margin_px).max(1.0);
        Self {
            cx: (minx + maxx) * 0.5,
            cy: (miny + maxy) * 0.5,
            scale: (aw / w).min(ah / h),
        }
    }

    /// World position -> pixels from the viewport centre (+y up).
    pub fn project(&self, x: f32, y: f32) -> [f32; 2] {
        [(x - self.cx) * self.scale, (y - self.cy) * self.scale]
    }

    /// The view-space point at the viewport centre.
    pub fn centre(&self) -> [f32; 2] {
        [self.cx, self.cy]
    }

    /// Pixels per view-space unit.
    pub fn pixels_per_unit(&self) -> f32 {
        self.scale
    }

    /// Raster density changes without changing the logical camera or its framing. A 2x target
    /// needs twice the pixel offsets, including a battle camera's native-window offsets.
    fn at_raster_scale(self, factor: f32) -> Self { Self { scale: self.scale * factor, ..self } }

    #[cfg(test)]
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// A camera whose view space is the window in pixels (one unit a pixel, `+y` up), with its
    /// origin `dx` pixels right of and `dy` pixels above the window's centre: the camera a
    /// battle frame is drawn with (see [`crate::battle::scene`]), where the perspective is the
    /// scene's own and this only places it in the window.
    pub fn pixels(dx: f32, dy: f32) -> Self {
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        Self { cx: -finite(dx), cy: -finite(dy), scale: 1.0 }
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB bytes -> shader colour. Our palette is authored in sRGB; an sRGB render target converts
/// shader output from linear, so the colour is linearised first for those targets.
pub fn color(rgb: [u8; 3], srgb_target: bool) -> [f32; 4] {
    let f = |b: u8| {
        let c = f32::from(b) / 255.0;
        if srgb_target {
            srgb_to_linear(c)
        } else {
            c
        }
    };
    [f(rgb[0]), f(rgb[1]), f(rgb[2]), 1.0]
}

/// The clear colour for `rgb` (sRGB bytes) on a target that is `srgb` or not (see [`color`]).
pub fn clear_color(rgb: [u8; 3], srgb: bool) -> wgpu::Color {
    let c = color(rgb, srgb);
    wgpu::Color { r: f64::from(c[0]), g: f64::from(c[1]), b: f64::from(c[2]), a: 1.0 }
}

/// Cover only the area outside the authored camera range. The same NDC vertices fit native
/// and supersampled targets: layout is always calculated with the native camera and margins.
fn field_margin_vertices(bounds: &Bounds, range: Option<Bounds>, size: (u32, u32), srgb: bool) -> Vec<Vertex> {
    let Some(range) = range else { return Vec::new() };
    if size.0 == 0 || size.1 == 0 || ![range.min.x, range.min.y, range.max.x, range.max.y].iter().all(|v| v.is_finite())
        || range.min.x >= range.max.x || range.min.y >= range.max.y { return Vec::new(); }
    let (w, h) = (size.0 as f32, size.1 as f32);
    let cam = Camera::fit(bounds, w, h, MARGIN_PX);
    let (lo, hi) = (cam.project(range.min.x, range.min.y), cam.project(range.max.x, range.max.y));
    let (hw, hh) = (w * 0.5, h * 0.5);
    let (left, right, bottom, top) = (lo[0].clamp(-hw, hw), hi[0].clamp(-hw, hw), lo[1].clamp(-hh, hh), hi[1].clamp(-hh, hh));
    let mut out = Builder { out: Vec::new(), hw, hh, srgb };
    for [x0, y0, x1, y1] in [[-hw, -hh, left, hh], [right, -hh, hw, hh], [-hw, -hh, hw, bottom], [-hw, top, hw, hh]] {
        if x1 > x0 && y1 > y0 { out.quad([x0, y0], [x1, y0], [x1, y1], [x0, y1], CLEAR_RGB); }
    }
    out.out
}

fn brighten(rgb: [u8; 3]) -> [u8; 3] {
    rgb.map(|c| (f32::from(c) + (255.0 - f32::from(c)) * 0.4).round() as u8)
}

/// Accumulates triangles given in pixels from the viewport centre and emits NDC vertices.
struct Builder {
    out: Vec<Vertex>,
    hw: f32,
    hh: f32,
    srgb: bool,
}

impl Builder {
    fn vertex(&mut self, p: [f32; 2], c: [f32; 4]) {
        self.out.push(Vertex {
            pos: [p[0] / self.hw, p[1] / self.hh],
            color: c,
        });
    }

    fn tri(&mut self, a: [f32; 2], b: [f32; 2], c: [f32; 2], rgb: [u8; 3]) {
        self.tri_alpha(a, b, c, rgb, 1.0);
    }

    fn tri_alpha(&mut self, a: [f32; 2], b: [f32; 2], c: [f32; 2], rgb: [u8; 3], alpha: f32) {
        if self.out.len() + 3 > MAX_VERTICES || ![a, b, c].iter().all(|p| p[0].is_finite() && p[1].is_finite()) {
            return;
        }
        let mut col = color(rgb, self.srgb);
        col[3] = alpha;
        self.vertex(a, col);
        self.vertex(b, col);
        self.vertex(c, col);
    }

    fn quad(&mut self, a: [f32; 2], b: [f32; 2], c: [f32; 2], d: [f32; 2], rgb: [u8; 3]) {
        self.tri(a, b, c, rgb);
        self.tri(a, c, d, rgb);
    }

    fn square(&mut self, c: [f32; 2], half: f32, rgb: [u8; 3]) {
        self.quad(
            [c[0] - half, c[1] - half],
            [c[0] + half, c[1] - half],
            [c[0] + half, c[1] + half],
            [c[0] - half, c[1] + half],
            rgb,
        );
    }

    fn diamond(&mut self, c: [f32; 2], half: f32, rgb: [u8; 3]) {
        // A diamond is a square rotated by 45 degrees, so use a slightly larger reach.
        let r = half * 1.25;
        self.quad(
            [c[0], c[1] - r],
            [c[0] + r, c[1]],
            [c[0], c[1] + r],
            [c[0] - r, c[1]],
            rgb,
        );
    }

    /// A closed band between the radii `r_in` and `r_out` around `c`: a polygon outline of
    /// [`RING_SEGMENTS`] segments, each a quad, so neighbouring segments share their corners and
    /// the band has no gaps.
    fn ring(&mut self, c: [f32; 2], r_in: f32, r_out: f32, rgb: [u8; 3]) {
        let at = |i: usize, r: f32| {
            let angle = i as f32 / RING_SEGMENTS as f32 * std::f32::consts::TAU;
            [c[0] + r * angle.cos(), c[1] + r * angle.sin()]
        };
        for i in 0..RING_SEGMENTS {
            self.quad(at(i, r_in), at(i, r_out), at(i + 1, r_out), at(i + 1, r_in), rgb);
        }
    }

    fn line(&mut self, a: [f32; 2], b: [f32; 2], width: f32, rgb: [u8; 3]) {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy);
        if !(len.is_finite() && len > 1e-3) {
            return;
        }
        let (nx, ny) = (-dy / len * width * 0.5, dx / len * width * 0.5);
        self.quad(
            [a[0] + nx, a[1] + ny],
            [b[0] + nx, b[1] + ny],
            [b[0] - nx, b[1] - ny],
            [a[0] - nx, a[1] - ny],
            rgb,
        );
    }

    fn banner(&mut self) {
        let (hw, hh) = (self.hw, self.hh);
        let (y0, y1) = (hh - BANNER_PX, hh);
        self.quad([-hw, y0], [hw, y0], [hw, y1], [-hw, y1], BANNER_YELLOW);
        // 45 degree hazard stripes; the parts beyond the viewport are clipped by the rasteriser.
        let stripe = BANNER_PX;
        let count = ((hw / stripe).ceil() as usize + 2).min(512);
        for i in 0..count {
            let x0 = -hw - stripe + i as f32 * 2.0 * stripe;
            self.quad(
                [x0, y0],
                [x0 + stripe, y0],
                [x0 + stripe + BANNER_PX, y1],
                [x0 + BANNER_PX, y1],
                BANNER_BLACK,
            );
        }
    }
}

/// Build the frame's vertex list. Painter's order: floor, edges, remote players, own player,
/// banner; each marker is followed by its party ring, if it has one. Non-finite input is skipped,
/// never propagated to the GPU.
pub fn build_vertices(scene: &Scene<'_>, vw: u32, vh: u32, srgb: bool) -> Vec<Vertex> {
    let (fw, fh) = (vw.max(1) as f32, vh.max(1) as f32);
    let cam = Camera::fit(&scene.bounds, fw, fh, MARGIN_PX);
    let capacity = (scene.triangles.len().saturating_mul(21) + 256).min(MAX_VERTICES);
    let mut b = Builder {
        out: Vec::with_capacity(capacity),
        hw: fw * 0.5,
        hh: fh * 0.5,
        srgb,
    };

    let px = |p: &Pos| cam.project(p.x, p.y);
    let (floor_alpha, edge_rgb) = if scene.backdrop.is_some() {
        (BACKDROP_FLOOR_ALPHA, BACKDROP_EDGE_RGB)
    } else {
        (1.0, EDGE_RGB)
    };
    for t in scene.triangles {
        b.tri_alpha(px(&t[0]), px(&t[1]), px(&t[2]), FLOOR_RGB, floor_alpha);
    }
    for t in scene.triangles {
        for (i, j) in [(0, 1), (1, 2), (2, 0)] {
            b.line(px(&t[i]), px(&t[j]), EDGE_PX, edge_rgb);
        }
    }
    let remote = scene.players.iter().filter(|p| !p.own);
    let own = scene.players.iter().filter(|p| p.own);
    for p in remote.chain(own) {
        let c = cam.project(p.x, p.y);
        if p.own {
            b.diamond(c, OWN_HALF_PX + OUTLINE_PX, OWN_OUTLINE_RGB);
            b.diamond(c, OWN_HALF_PX, brighten(p.tint));
        } else {
            b.square(c, PLAYER_HALF_PX + OUTLINE_PX, OUTLINE_RGB);
            b.square(c, PLAYER_HALF_PX, p.tint);
        }
        if p.party {
            b.ring(c, RING_INNER_PX, RING_OUTER_PX, RING_RGB);
        }
    }
    if scene.fixture_banner {
        b.banner();
    }
    b.out
}

/// The backdrop's two triangles in NDC for a `vw` x `vh` viewport, placed by the same camera as
/// everything else, so the picture and the walkmesh drawn over it stay aligned. The picture's
/// top row is at the top of its rectangle (view-space `max.y`). `None` without a backdrop, or if
/// a corner does not come out finite.
pub fn backdrop_quad(scene: &Scene<'_>, vw: u32, vh: u32) -> Option<[QuadVertex; 6]> {
    let backdrop = scene.backdrop?;
    let (fw, fh) = (vw.max(1) as f32, vh.max(1) as f32);
    let cam = Camera::fit(&scene.bounds, fw, fh, MARGIN_PX);
    let corner = |x: f32, y: f32, u: f32, v: f32| {
        let p = cam.project(x, y);
        QuadVertex { pos: [p[0] / (fw * 0.5), p[1] / (fh * 0.5)], uv: [u, v] }
    };
    let (min, max) = (backdrop.min, backdrop.max);
    let top_left = corner(min[0], max[1], 0.0, 0.0);
    let top_right = corner(max[0], max[1], 1.0, 0.0);
    let bottom_right = corner(max[0], min[1], 1.0, 1.0);
    let bottom_left = corner(min[0], min[1], 0.0, 1.0);
    let quad = [top_left, bottom_left, bottom_right, top_left, bottom_right, top_right];
    quad.iter().all(|v| v.pos.iter().all(|c| c.is_finite())).then_some(quad)
}

/// Where a view-space point lands in a `vw` x `vh` viewport drawn by [`build_vertices`], in window
/// pixels from the TOP LEFT (+y down): how the HUD places a name label over a player's marker.
pub fn view_to_window(bounds: &Bounds, vw: u32, vh: u32, x: f32, y: f32) -> [f32; 2] {
    let (fw, fh) = (vw.max(1) as f32, vh.max(1) as f32);
    let p = Camera::fit(bounds, fw, fh, MARGIN_PX).project(x, y);
    [fw * 0.5 + p[0], fh * 0.5 - p[1]]
}

// ---------------------------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------------------------

/// A backdrop picture uploaded to the GPU, with the bind group that samples it.
struct GpuBackdrop {
    id: u64,
    bind_group: wgpu::BindGroup,
    /// Owns the pixels the bind group's view looks at.
    _texture: wgpu::Texture,
}

/// Pipelines + buffers, independent of any window surface.
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    vertex_buf: wgpu::Buffer,
    vertex_cap: u64,
    /// The colour format of every target this renderer draws into.
    format: wgpu::TextureFormat,
    quad_pipeline: wgpu::RenderPipeline,
    quad_sampler: wgpu::Sampler,
    quad_buf: wgpu::Buffer,
    /// The picture currently on the GPU, replaced when a scene brings a different one.
    backdrop: Option<GpuBackdrop>,
    /// The text/panel overlay (menus, HUD), drawn over the scene by [`Gpu::draw_overlay`].
    ui: crate::ui::gpu::UiGpu,
    /// The players as models, drawn over the scene by [`Gpu::draw_field`] (see [`crate::avatar`]).
    avatars: crate::avatar::gpu::AvatarGpu,
    /// Allocated only while optional edge smoothing is selected.
    fxaa: Option<crate::graphics::fxaa::Fxaa>,
    supersample: Option<crate::graphics::supersample::Supersample>,
    /// Created on the first authored fade, after scene filtering and before UI.
    fade: Option<crate::scene::fade::FadeGpu>,
    /// Say a requested 2x fallback once per window size, rather than every frame.
    last_scale_fallback: Option<(u32, u32)>,
}

struct SceneTarget {
    draw: wgpu::TextureView,
    downsample_to: Option<wgpu::TextureView>,
    size: (u32, u32),
    factor: f32,
}

const MIN_VERTEX_BYTES: u64 = 64 * 1024;

impl Gpu {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fmv-flat-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_WGSL.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fmv-flat-pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(Vertex::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
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
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let vertex_buf = Self::create_vertex_buffer(&device, MIN_VERTEX_BYTES);

        let quad_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fmv-quad-shader"),
            source: wgpu::ShaderSource::Wgsl(QUAD_SHADER_WGSL.into()),
        });
        let quad_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fmv-quad-pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &quad_shader,
                entry_point: Some("vs_quad"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(QuadVertex::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &quad_shader,
                entry_point: Some("fs_quad"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
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
        let quad_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fmv-backdrop-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });
        let quad_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fmv-backdrop-quad"),
            size: (6 * std::mem::size_of::<QuadVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ui = crate::ui::gpu::UiGpu::new(&device, &queue, format);
        let avatars = crate::avatar::gpu::AvatarGpu::new(&device, &queue, format);
        Self {
            device,
            queue,
            pipeline,
            vertex_buf,
            vertex_cap: MIN_VERTEX_BYTES,
            format,
            quad_pipeline,
            quad_sampler,
            quad_buf,
            backdrop: None,
            ui,
            avatars,
            fxaa: None,
            supersample: None,
            fade: None,
            last_scale_fallback: None,
        }
    }

    pub fn set_anti_aliasing(&mut self, mode: crate::graphics::AntiAliasing) {
        match mode {
            crate::graphics::AntiAliasing::Off => self.fxaa = None,
            crate::graphics::AntiAliasing::Fxaa if self.fxaa.is_none() => {
                self.fxaa = Some(crate::graphics::fxaa::Fxaa::new(&self.device, self.format));
            }
            _ => {}
        }
    }

    pub fn set_render_scale(&mut self, scale: crate::graphics::RenderScale) {
        match scale {
            crate::graphics::RenderScale::Native => { self.supersample = None; self.last_scale_fallback = None; }
            crate::graphics::RenderScale::Double if self.supersample.is_none() => {
                self.supersample = Some(crate::graphics::supersample::Supersample::new(&self.device, self.format));
            }
            _ => {}
        }
    }

    fn render_plan(&self, size: (u32, u32)) -> crate::graphics::supersample::Plan {
        use crate::graphics::{supersample::Plan, RenderScale};
        Plan::new(size, if self.supersample.is_some() { RenderScale::Double } else { RenderScale::Native }, self.device.limits().max_texture_dimension_2d)
    }

    /// Scene -> optional 2x resolve -> optional native FXAA -> native UI. Building layout at
    /// native size and scaling only raster coordinates keeps every overlay aligned to the scene.
    fn scene_target(&mut self, output: &wgpu::TextureView, size: (u32, u32)) -> SceneTarget {
        let native = self.fxaa.as_mut().map_or_else(|| output.clone(), |fxaa| fxaa.target(&self.device, size));
        let plan = self.render_plan(size);
        if self.supersample.is_some() && plan.effective == crate::graphics::RenderScale::Native {
            if self.last_scale_fallback != Some(size) {
                eprintln!("graphics: 2x scene rendering requested at {}x{}; using native resolution within GPU dimension and scene allocation limits", size.0, size.1);
                self.last_scale_fallback = Some(size);
            }
        } else { self.last_scale_fallback = None; }
        let high = self.supersample.as_mut().and_then(|supersample| supersample.target(&self.device, plan));
        match high {
            Some(draw) => SceneTarget { draw, downsample_to: Some(native), size: plan.size, factor: plan.factor() },
            None => SceneTarget { draw: native, downsample_to: None, size, factor: 1.0 },
        }
    }

    fn resolve_scene(&self, scene: &SceneTarget, output: &wgpu::TextureView) {
        if let Some(target) = &scene.downsample_to {
            self.supersample.as_ref().expect("2x scene target").resolve(&self.device, &self.queue, target);
        }
        if let Some(fxaa) = &self.fxaa { fxaa.resolve(&self.device, &self.queue, output); }
    }

    /// Characters the avatar renderer has uploaded so far (one entry per character, not per frame).
    #[cfg(test)]
    pub fn cached_avatar_models(&self) -> usize {
        self.avatars.cached_models()
    }

    /// Draw one frame of a field view into `target` (`size` pixels). Without `avatars` this is
    /// exactly [`Gpu::draw_scene`] over the scene's backdrop and vertices.
    ///
    /// With `avatars` the players are models, and the frame is drawn in one of two ways:
    ///
    /// * **Depth-tested scenery**, when the frame has models and a scenery
    ///   ([`crate::avatar::scenery`]) the device can hold: pass 1 draws the picture as tiles with
    ///   their depth (the flat picture is not drawn), pass 2 the flat shapes (walkmesh overlay,
    ///   markers) over it, and pass 3 the marks on the ground, the models and the blended tiles, all
    ///   tested against the tiles' depth ([`crate::avatar::gpu`]).
    /// * **Flat picture**, otherwise: the scene as it always was (picture, flat shapes), then the
    ///   marks and the models over it with a fresh depth buffer.
    pub fn draw_field(
        &mut self,
        target: &wgpu::TextureView,
        size: (u32, u32),
        scene: &Scene<'_>,
        avatars: Option<&crate::avatar::AvatarFrame>,
        srgb: bool,
    ) {
        self.draw_field_clipped(target, size, scene, avatars, srgb, None);
    }

    /// A follow-camera field keeps the scene inside its authored range. Clear the bars before
    /// downsampling/FXAA so off-range actors cannot bleed into the visible edge, and restore
    /// their exact clear colour afterward. The caller draws native HUD/text after this method.
    pub fn draw_field_clipped(
        &mut self, target: &wgpu::TextureView, size: (u32, u32), scene: &Scene<'_>,
        avatars: Option<&crate::avatar::AvatarFrame>, srgb: bool, range: Option<Bounds>,
    ) {
        self.draw_field_effects(target, size, scene, avatars, srgb, range, None);
    }

    /// Apply an authored display-RGB fade to the completed field, before native dialogue/HUD.
    /// A frame without a fade follows the unchanged render path and needs no fade target.
    #[allow(clippy::too_many_arguments)] // Keep the existing field-render inputs plus its optional authored fade.
    pub fn draw_field_effects(
        &mut self, target: &wgpu::TextureView, size: (u32, u32), scene: &Scene<'_>,
        avatars: Option<&crate::avatar::AvatarFrame>, srgb: bool, range: Option<Bounds>,
        fade: Option<fmv_ff7::script::scene::FadeColor>,
    ) {
        let before_fade = fade.map(|_| {
            let compositor = self.fade.get_or_insert_with(|| crate::scene::fade::FadeGpu::new(&self.device, self.format));
            compositor.target(&self.device, size)
        });
        let scene_target = before_fade.as_ref().unwrap_or(target);
        let raster = self.scene_target(scene_target, size);
        let margins = field_margin_vertices(&scene.bounds, range, size, srgb);
        self.draw_field_raw(&raster, size, scene, avatars, srgb);
        self.draw_shapes(&raster.draw, &margins);
        self.resolve_scene(&raster, scene_target);
        if raster.downsample_to.is_some() || self.fxaa.is_some() { self.draw_shapes(scene_target, &margins); }
        if let Some(colour) = fade {
            self.fade.as_ref().expect("fade target was created").resolve(&self.device, &self.queue, target, colour);
        }
    }

    fn draw_field_raw(
        &mut self, raster: &SceneTarget, size: (u32, u32), scene: &Scene<'_>,
        avatars: Option<&crate::avatar::AvatarFrame>, srgb: bool,
    ) {
        let (w, h) = size;
        let target = &raster.draw;
        // The one fitted camera everything of the frame is placed by.
        let cam = Camera::fit(&scene.bounds, w.max(1) as f32, h.max(1) as f32, MARGIN_PX).at_raster_scale(raster.factor);
        let vertices = build_vertices(scene, w, h, srgb);
        let tiled = avatars.filter(|f| !f.models.is_empty() && self.avatars.prepare_scenery(f));
        if let Some(frame) = tiled {
            self.avatars.draw_scenery(target, raster.size, &cam, frame, clear_color(CLEAR_RGB, srgb));
            self.draw_shapes(target, &vertices);
            self.avatars.draw_models(target, raster.size, &cam, frame, true);
            return;
        }
        // The picture goes to the GPU once (again only if it changes); without one, or if the
        // device cannot hold it, the walkmesh is drawn on its own.
        let quad = match scene.backdrop {
            Some(b) if self.ensure_backdrop(b) => backdrop_quad(scene, w, h),
            _ => None,
        };
        self.draw_scene(target, &vertices, quad.as_ref(), CLEAR_RGB, srgb);
        if let Some(frame) = avatars {
            self.avatars.draw_models(target, raster.size, &cam, frame, false);
        }
    }

    /// What the avatar renderer holds of the scenery: uploads so far, the identity of the current
    /// one and the draw calls it takes a frame (tests).
    #[cfg(test)]
    pub fn scenery_state(&self) -> (u32, Option<u64>, usize) {
        self.avatars.scenery_state()
    }

    /// Draw the UI overlay `vertices` (see [`crate::ui::draw::DrawList::vertices`]) over what
    /// `target` holds, after [`Gpu::draw_scene`]. An empty list draws nothing.
    pub fn draw_overlay(&mut self, target: &wgpu::TextureView, vertices: &[crate::ui::draw::UiVertex]) {
        self.ui.draw(&self.device, &self.queue, target, vertices);
    }

    pub fn draw_creator_preview(&mut self, target: &wgpu::TextureView, size: (u32, u32), frame: &crate::avatar::AvatarFrame) {
        let offset = size.0 as f32 * 0.5 - crate::ui::preview::margin(size) * 0.5;
        self.avatars.draw_models(target, size, &Camera::pixels(offset, 0.0), frame, false);
    }

    /// Make sure `backdrop` is the picture on the GPU (uploading it if it is a different one).
    /// Returns false if it cannot be used (larger than the device's texture limit): the scene is
    /// then drawn without it.
    pub fn ensure_backdrop(&mut self, backdrop: &Backdrop) -> bool {
        if self.backdrop.as_ref().is_some_and(|g| g.id == backdrop.id) {
            return true;
        }
        self.backdrop = None;
        let limit = self.device.limits().max_texture_dimension_2d;
        if backdrop.width > limit || backdrop.height > limit {
            return false;
        }
        // The pixels are sRGB bytes. An sRGB target converts what the shader writes from linear,
        // so the texture is sampled as sRGB (decoded to linear) for those targets: either way the
        // colours come out as authored.
        let format = if self.format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let size = wgpu::Extent3d {
            width: backdrop.width,
            height: backdrop.height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fmv-backdrop"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &backdrop.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(backdrop.width * 4),
                rows_per_image: Some(backdrop.height),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let layout = self.quad_pipeline.get_bind_group_layout(0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fmv-backdrop-bind"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.quad_sampler),
                },
            ],
        });
        self.backdrop = Some(GpuBackdrop {
            id: backdrop.id,
            bind_group,
            _texture: texture,
        });
        true
    }

    fn create_vertex_buffer(device: &wgpu::Device, bytes: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fmv-vertices"),
            size: bytes,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Upload `vertices` (at most [`MAX_VERTICES`]) to the vertex buffer, growing it when needed.
    /// Returns the vertices kept and the bytes they take.
    fn upload_vertices<'v>(&mut self, vertices: &'v [Vertex]) -> (&'v [Vertex], u64) {
        let vertices = &vertices[..vertices.len().min(MAX_VERTICES)];
        let bytes: &[u8] = bytemuck::cast_slice(vertices);
        let need = bytes.len() as u64;
        if need > self.vertex_cap {
            let cap = need.next_power_of_two().max(MIN_VERTEX_BYTES);
            self.vertex_buf = Self::create_vertex_buffer(&self.device, cap);
            self.vertex_cap = cap;
        }
        if !bytes.is_empty() {
            self.queue.write_buffer(&self.vertex_buf, 0, bytes);
        }
        (vertices, need)
    }

    /// Draw `vertices` (flat shapes, painter's order) over what `target` holds: no clear, no
    /// picture, no depth. The shapes of a frame whose picture the avatar renderer drew as tiles.
    pub fn draw_shapes(&mut self, target: &wgpu::TextureView, vertices: &[Vertex]) {
        let (vertices, need) = self.upload_vertices(vertices);
        // `set_vertex_buffer` panics on an empty slice, so only draw when there is geometry.
        if need == 0 {
            return;
        }
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("fmv-shapes"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fmv-shapes-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.vertex_buf.slice(0..need));
            pass.draw(0..u32::try_from(vertices.len()).unwrap_or(u32::MAX), 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Clear `target`, draw the backdrop `quad` first when there is one and a picture was
    /// uploaded with [`Gpu::ensure_backdrop`], then `vertices` over it.
    pub fn draw_scene(
        &mut self,
        target: &wgpu::TextureView,
        vertices: &[Vertex],
        quad: Option<&[QuadVertex; 6]>,
        clear_rgb: [u8; 3],
        srgb: bool,
    ) {
        let (vertices, need) = self.upload_vertices(vertices);
        let no_shapes = need == 0;
        if let Some(quad) = quad {
            self.queue.write_buffer(&self.quad_buf, 0, bytemuck::cast_slice(quad));
        }
        let clear = clear_color(clear_rgb, srgb);

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("fmv-frame"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fmv-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let (Some(_), Some(backdrop)) = (quad, &self.backdrop) {
                pass.set_pipeline(&self.quad_pipeline);
                pass.set_bind_group(0, &backdrop.bind_group, &[]);
                pass.set_vertex_buffer(0, self.quad_buf.slice(..));
                pass.draw(0..6, 0..1);
            }
            // `set_vertex_buffer` panics on an empty slice, so only draw when there is geometry.
            if !no_shapes {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.vertex_buf.slice(0..need));
                pass.draw(0..u32::try_from(vertices.len()).unwrap_or(u32::MAX), 0..1);
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// A flat fallback battle gets the same optional filter as the 3D scene.
    pub fn draw_flat(&mut self, target: &wgpu::TextureView, size: (u32, u32), vertices: &[Vertex], srgb: bool) {
        let raster = self.scene_target(target, size);
        self.draw_scene(&raster.draw, vertices, None, CLEAR_RGB, srgb);
        self.resolve_scene(&raster, target);
    }

    /// Draw one frame of a battle ([`crate::battle::scene`]) into `target` (`size` pixels): clear it,
    /// the stage (flat triangles in NDC, painter's order), the location's model if there is one, then
    /// the models and the marks on the ground under them through the avatar renderer, depth-tested
    /// with a fresh depth buffer. The UI overlay is the caller's to draw after.
    pub fn draw_battle(&mut self, target: &wgpu::TextureView, size: (u32, u32), frame: &crate::battle::scene::BattleFrame, srgb: bool) {
        let raster = self.scene_target(target, size);
        self.draw_battle_raw(&raster, frame, srgb);
        self.resolve_scene(&raster, target);
    }

    fn draw_battle_raw(&mut self, raster: &SceneTarget, frame: &crate::battle::scene::BattleFrame, srgb: bool) {
        let target = &raster.draw;
        let cam = frame.camera.at_raster_scale(raster.factor);
        self.draw_scene(target, &frame.stage, None, frame.clear_rgb, srgb);
        // The location, when there is one, in a pass of its own: the actors' pass then starts with a
        // fresh depth buffer, so the marks under the actors are not covered by the location's floor.
        if let Some(location) = &frame.location {
            self.avatars.draw_models(target, raster.size, &cam, location, false);
        }
        self.avatars.draw_models_tinted(target, raster.size, &cam, &frame.models, &frame.tints, &frame.identities, false);
    }
}

// ---------------------------------------------------------------------------------------------
// Window surface
// ---------------------------------------------------------------------------------------------

/// Result of drawing one frame.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameResult {
    Presented,
    /// Nothing drawn this time (minimised, occluded, reconfiguring); try again next frame.
    Skipped,
    /// The renderer cannot continue.
    Fatal(String),
}

pub struct Renderer {
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    window: Arc<Window>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    srgb: bool,
    size: (u32, u32),
    needs_reconfigure: bool,
    description: String,
    gpu_errors: Arc<Mutex<Vec<String>>>,
    graphics: crate::graphics::Settings,
}

impl Renderer {
    /// Create the GPU context for `window`. Every failure is a message, never a panic: wgpu's
    /// default uncaptured-error handler (which panics) is replaced by one that logs.
    pub fn new(window: Arc<Window>, display: OwnedDisplayHandle) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(
            display,
        )));
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("cannot create the GPU surface for the window: {e}"))?;

        let request = |force_fallback: bool| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::from_env().unwrap_or_default(),
                force_fallback_adapter: force_fallback,
                compatible_surface: Some(&surface),
                ..wgpu::RequestAdapterOptions::default()
            }))
        };
        let adapter = match request(false) {
            Ok(a) => a,
            // No hardware adapter can present to this window: try a software one.
            Err(first) => request(true).map_err(|second| {
                format!("no usable GPU adapter ({first}); software fallback also failed ({second})")
            })?,
        };
        let info = adapter.get_info();
        let description = format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type);

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("fmv-client"),
            required_features: wgpu::Features::empty(),
            // Minimal limits (nothing here needs more) but the adapter's own texture size cap so
            // very large windows can still be configured.
            required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..wgpu::DeviceDescriptor::default()
        }))
        .map_err(|e| format!("cannot open the GPU device on {description}: {e}"))?;

        let gpu_errors = Arc::new(Mutex::new(Vec::new()));
        {
            let sink = Arc::clone(&gpu_errors);
            device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
                eprintln!("gpu error: {e}");
                if let Ok(mut v) = sink.lock() {
                    if v.len() < 16 {
                        v.push(e.to_string());
                    }
                }
            }));
        }

        let caps = surface.get_capabilities(&adapter);
        // Prefer a non-sRGB format: the palette is authored in sRGB bytes and is shown as is.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| format!("the window surface reports no usable pixel format on {description}"))?;
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let mut config = surface
            .get_default_config(&adapter, w, h)
            .ok_or_else(|| format!("{description} cannot present to this window"))?;
        config.format = format;
        config.present_mode = wgpu::PresentMode::Fifo; // required to be available everywhere
        surface.configure(&device, &config);

        let gpu = Gpu::new(device, queue, format);
        Ok(Self {
            instance,
            surface,
            window,
            config,
            gpu,
            srgb: format.is_srgb(),
            size: (size.width, size.height),
            needs_reconfigure: false,
            description,
            gpu_errors,
            graphics: crate::graphics::Settings::default(),
        })
    }

    pub fn set_graphics(&mut self, settings: crate::graphics::Settings) {
        self.gpu.set_anti_aliasing(settings.anti_aliasing);
        self.gpu.set_render_scale(settings.render_scale);
        self.graphics = settings;
    }

    /// The persisted choices and the resolution this window can actually allocate now.
    pub fn graphics_status(&self) -> crate::graphics::panel::Status {
        crate::graphics::panel::Status {
            anti_aliasing: self.graphics.anti_aliasing,
            render_scale: self.graphics.render_scale,
            effective_scale: self.gpu.render_plan(self.size).effective,
        }
    }

    /// Restore the original render path immediately, with one attempt to remember both choices.
    pub fn restore_graphics_defaults(&mut self) -> String {
        let saved = self.graphics.restore_defaults();
        self.gpu.set_anti_aliasing(self.graphics.anti_aliasing);
        self.gpu.set_render_scale(self.graphics.render_scale);
        match saved {
            Ok(()) => "Graphics defaults restored".to_string(),
            Err(e) => {
                eprintln!("graphics: {e}");
                "Graphics defaults restored for this session; settings could not be saved".to_string()
            }
        }
    }

    /// The choice applies immediately even if the local settings file cannot be written.
    pub fn toggle_anti_aliasing(&mut self) -> &'static str {
        let saved = self.graphics.toggle();
        self.gpu.set_anti_aliasing(self.graphics.anti_aliasing);
        if let Err(e) = saved {
            eprintln!("graphics: {e}");
            return "Graphics changed for this session; settings could not be saved";
        }
        self.graphics.anti_aliasing.notice()
    }

    pub fn toggle_render_scale(&mut self) -> String {
        let saved = self.graphics.toggle_render_scale();
        self.gpu.set_render_scale(self.graphics.render_scale);
        let notice = self.graphics.render_scale.notice(self.gpu.render_plan(self.size).effective);
        match saved {
            Ok(_) => notice.to_string(),
            Err(e) => { eprintln!("graphics: {e}"); format!("{notice}; not saved") }
        }
    }

    /// "adapter name (backend, device type)" for logs.
    pub fn describe(&self) -> &str {
        &self.description
    }

    /// The first GPU error reported through the uncaptured-error handler, if any. Such errors
    /// mean a bug or a lost device; the window loop turns them into a clean failure.
    pub fn first_gpu_error(&self) -> Option<String> {
        self.gpu_errors.lock().ok().and_then(|v| v.first().cloned())
    }

    /// The size of the window surface in pixels, as last reported by [`Renderer::resize`].
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Window size changed (a zero size means minimised: drawing is skipped until it grows).
    pub fn resize(&mut self, width: u32, height: u32) {
        self.size = (width, height);
        self.needs_reconfigure = true;
    }

    fn reconfigure(&mut self) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return; // `Surface::configure` panics on a zero dimension
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.gpu.device, &self.config);
        self.needs_reconfigure = false;
    }

    fn recreate_surface(&mut self) -> Result<(), String> {
        self.surface = self
            .instance
            .create_surface(self.window.clone())
            .map_err(|e| format!("cannot recreate the lost GPU surface: {e}"))?;
        self.needs_reconfigure = true;
        Ok(())
    }

    /// Draw `scene` alone (no overlay).
    #[allow(dead_code)] // the window always draws an overlay now; kept as the plain entry point
    pub fn render(&mut self, scene: &Scene<'_>) -> FrameResult {
        self.render_with(scene, None)
    }

    /// Draw `scene`, then the UI `overlay` (text, panels) over it.
    pub fn render_with(&mut self, scene: &Scene<'_>, overlay: Option<&crate::ui::draw::DrawList>) -> FrameResult {
        self.render_layers(scene, None, overlay)
    }

    /// Draw `scene` and, when `avatars` is given, the players as models with the picture as
    /// depth-tested tiles around them ([`Gpu::draw_field`]), then the UI `overlay`. Without avatars it
    /// is exactly [`Renderer::render_with`].
    pub fn render_layers(
        &mut self,
        scene: &Scene<'_>,
        avatars: Option<&crate::avatar::AvatarFrame>,
        overlay: Option<&crate::ui::draw::DrawList>,
    ) -> FrameResult {
        self.render_layers_clipped(scene, avatars, overlay, None)
    }

    /// Draw a follow-camera field inside its authored range, with native UI over the result.
    pub fn render_layers_clipped(
        &mut self, scene: &Scene<'_>, avatars: Option<&crate::avatar::AvatarFrame>,
        overlay: Option<&crate::ui::draw::DrawList>, range: Option<Bounds>,
    ) -> FrameResult {
        self.render_layers_effects(scene, avatars, overlay, range, None)
    }

    /// Draw an authored field fade after scene filtering, with native UI over the result.
    pub fn render_layers_effects(
        &mut self, scene: &Scene<'_>, avatars: Option<&crate::avatar::AvatarFrame>,
        overlay: Option<&crate::ui::draw::DrawList>, range: Option<Bounds>, fade: Option<fmv_ff7::script::scene::FadeColor>,
    ) -> FrameResult {
        self.render_field_layers(scene, avatars, overlay, None, range, fade)
    }

    /// Render a character preview after the menu overlay, using pixel-space projection.
    pub fn render_layers_with_preview(
        &mut self,
        scene: &Scene<'_>,
        avatars: Option<&crate::avatar::AvatarFrame>,
        overlay: Option<&crate::ui::draw::DrawList>,
        preview: Option<&crate::avatar::AvatarFrame>,
    ) -> FrameResult {
        self.render_field_layers(scene, avatars, overlay, preview, None, None)
    }

    fn render_field_layers(
        &mut self, scene: &Scene<'_>, avatars: Option<&crate::avatar::AvatarFrame>,
        overlay: Option<&crate::ui::draw::DrawList>, preview: Option<&crate::avatar::AvatarFrame>, range: Option<Bounds>,
        fade: Option<fmv_ff7::script::scene::FadeColor>,
    ) -> FrameResult {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return FrameResult::Skipped;
        }
        // No SurfaceTexture is alive here (every frame is presented or dropped below), which
        // `Surface::configure` requires.
        if self.needs_reconfigure {
            self.reconfigure();
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                // Draw and present this frame; reconfigure before the next one.
                self.needs_reconfigure = true;
                f
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return FrameResult::Skipped
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.needs_reconfigure = true;
                return FrameResult::Skipped;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return match self.recreate_surface() {
                    Ok(()) => FrameResult::Skipped,
                    Err(e) => FrameResult::Fatal(e),
                };
            }
            // The error itself was already logged by the uncaptured-error handler.
            wgpu::CurrentSurfaceTexture::Validation => return FrameResult::Skipped,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        match (range, fade) {
            (_, Some(fade)) => self.gpu.draw_field_effects(&view, (w, h), scene, avatars, self.srgb, range, Some(fade)),
            (Some(range), None) => self.gpu.draw_field_clipped(&view, (w, h), scene, avatars, self.srgb, Some(range)),
            (None, None) => self.gpu.draw_field(&view, (w, h), scene, avatars, self.srgb),
        }
        if let Some(overlay) = overlay {
            self.gpu.draw_overlay(&view, &overlay.vertices(w, h, self.srgb));
        }
        if let Some(preview) = preview {
            self.gpu.draw_creator_preview(&view, (w, h), preview);
        }
        self.window.pre_present_notify();
        self.gpu.queue.present(frame);
        FrameResult::Presented
    }
}

// A pure addition for the battle view (`crate::battle`): a frame made only of flat-coloured
// vertices, with the same surface handling as `Renderer::render` above (kept as its own block so
// that the existing function stays untouched).
impl Renderer {
    /// Draw one frame of flat-coloured vertices only (no backdrop, no walkmesh), then the UI
    /// `overlay` (text) over it: `build` gets the surface size in pixels and whether the target is
    /// sRGB (see [`color`]) and returns the vertices, in NDC, painter's order. The surface handling
    /// (resize, minimised, outdated, lost) is `render_with`'s.
    pub fn render_flat(
        &mut self,
        build: impl FnOnce(u32, u32, bool) -> Vec<Vertex>,
        overlay: Option<&crate::ui::draw::DrawList>,
    ) -> FrameResult {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return FrameResult::Skipped;
        }
        if self.needs_reconfigure {
            self.reconfigure();
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                self.needs_reconfigure = true;
                f
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return FrameResult::Skipped
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.needs_reconfigure = true;
                return FrameResult::Skipped;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return match self.recreate_surface() {
                    Ok(()) => FrameResult::Skipped,
                    Err(e) => FrameResult::Fatal(e),
                };
            }
            wgpu::CurrentSurfaceTexture::Validation => return FrameResult::Skipped,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let vertices = build(w, h, self.srgb);
        self.gpu.draw_flat(&view, (w, h), &vertices, self.srgb);
        if let Some(overlay) = overlay {
            self.gpu.draw_overlay(&view, &overlay.vertices(w, h, self.srgb));
        }
        self.window.pre_present_notify();
        self.gpu.queue.present(frame);
        FrameResult::Presented
    }
}

// A pure addition for the 3D battle scene (`crate::battle::scene`): the same surface handling as
// `Renderer::render_flat`, drawing a [`BattleFrame`](crate::battle::scene::BattleFrame) instead.
impl Renderer {
    /// Draw one frame of the battle scene: `build` gets the surface size in pixels and whether the
    /// target is sRGB (see [`color`]) and returns the frame ([`Gpu::draw_battle`]); then the UI
    /// `overlay` over it. The surface handling (resize, minimised, outdated, lost) is
    /// `render_flat`'s.
    pub fn render_battle(
        &mut self,
        build: impl FnOnce(u32, u32, bool) -> crate::battle::scene::BattleFrame,
        overlay: Option<&crate::ui::draw::DrawList>,
    ) -> FrameResult {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return FrameResult::Skipped;
        }
        if self.needs_reconfigure {
            self.reconfigure();
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                self.needs_reconfigure = true;
                f
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return FrameResult::Skipped
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.needs_reconfigure = true;
                return FrameResult::Skipped;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return match self.recreate_surface() {
                    Ok(()) => FrameResult::Skipped,
                    Err(e) => FrameResult::Fatal(e),
                };
            }
            wgpu::CurrentSurfaceTexture::Validation => return FrameResult::Skipped,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let battle = build(w, h, self.srgb);
        self.gpu.draw_battle(&view, (w, h), &battle, self.srgb);
        if let Some(overlay) = overlay {
            self.gpu.draw_overlay(&view, &overlay.vertices(w, h, self.srgb));
        }
        self.window.pre_present_notify();
        self.gpu.queue.present(frame);
        FrameResult::Presented
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_raster_samples_preserve_field_margins_and_battle_pixel_framing() {
        let bounds = Bounds { min: Pos::new(-173.0, -65.0, 0.0), max: Pos::new(227.0, 175.0, 0.0) };
        for size in [(1100, 680), (319, 241)] {
            for camera in [Camera::fit(&bounds, size.0 as f32, size.1 as f32, MARGIN_PX), Camera::pixels(73.0, -29.0)] {
                let high = camera.at_raster_scale(2.0);
                assert_eq!(high.centre(), camera.centre());
                for point in [[-50.0, 78.0], [0.0, 0.0], [170.0, -150.0]] {
                    let a = camera.project(point[0], point[1]);
                    let b = high.project(point[0], point[1]);
                    assert_eq!(b, a.map(|v| v * 2.0));
                    assert_eq!([a[0] / size.0 as f32, a[1] / size.1 as f32], [b[0] / (size.0 * 2) as f32, b[1] / (size.1 * 2) as f32]);
                }
            }
        }
    }

    fn bounds(minx: f32, miny: f32, maxx: f32, maxy: f32) -> Bounds {
        Bounds {
            min: Pos::new(minx, miny, 0.0),
            max: Pos::new(maxx, maxy, 0.0),
        }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn camera_fits_the_bounds_and_preserves_aspect() {
        // A wide area in a square viewport is limited by width.
        let cam = Camera::fit(&bounds(-400.0, -300.0, 700.0, 300.0), 1000.0, 1000.0, 20.0);
        let left = cam.project(-400.0, 0.0);
        let right = cam.project(700.0, 0.0);
        assert!(near(left[0], -480.0) && near(right[0], 480.0), "{left:?} {right:?}");
        // Same pixels per unit on both axes (no distortion), +y up.
        let top = cam.project(150.0, 300.0);
        let bottom = cam.project(150.0, -300.0);
        assert!(near(top[1] - bottom[1], 600.0 * cam.scale()));
        assert!(top[1] > bottom[1]);
        // The centre of the bounds is the centre of the viewport.
        let c = cam.project(150.0, 0.0);
        assert!(near(c[0], 0.0) && near(c[1], 0.0));
        // A tall viewport is limited by... width still; a wide viewport by height.
        let wide = Camera::fit(&bounds(-400.0, -300.0, 700.0, 300.0), 2000.0, 400.0, 20.0);
        assert!(
            near(wide.project(0.0, 300.0)[1], 180.0),
            "{:?}",
            wide.project(0.0, 300.0)
        );
    }

    #[test]
    fn camera_survives_degenerate_input() {
        for b in [
            bounds(0.0, 0.0, 0.0, 0.0),
            bounds(f32::NAN, 0.0, 1.0, 1.0),
            bounds(0.0, 0.0, f32::INFINITY, 1.0),
        ] {
            for (w, h) in [(0.0, 0.0), (1.0, 1.0), (4000.0, 3000.0)] {
                let c = Camera::fit(&b, w, h, 24.0);
                let p = c.project(0.5, 0.5);
                assert!(p[0].is_finite() && p[1].is_finite() && c.scale() > 0.0);
            }
        }
    }

    #[test]
    fn color_conversion_matches_the_target_space() {
        assert_eq!(color([255, 0, 128], false), [1.0, 0.0, 128.0 / 255.0, 1.0]);
        let lin = color([255, 255, 255], true);
        assert!(near(lin[0], 1.0) && near(lin[3], 1.0));
        let mid = color([128, 128, 128], true);
        assert!(
            mid[0] < 0.25 && mid[0] > 0.2,
            "sRGB 128 is about 0.216 linear, got {}",
            mid[0]
        );
        assert!(near(srgb_to_linear(0.0), 0.0));
    }

    fn tris() -> Vec<[Pos; 3]> {
        vec![
            [
                Pos::new(-400.0, -300.0, 0.0),
                Pos::new(400.0, -300.0, 0.0),
                Pos::new(400.0, 300.0, 0.0),
            ],
            [
                Pos::new(-400.0, -300.0, 0.0),
                Pos::new(400.0, 300.0, 0.0),
                Pos::new(-400.0, 300.0, 0.0),
            ],
        ]
    }

    #[test]
    fn vertices_are_in_ndc_and_counts_match_the_scene() {
        let t = tris();
        let players = [
            PlayerSprite {
                x: 0.0,
                y: 0.0,
                tint: [200, 50, 50],
                own: false,
                party: false,
            },
            PlayerSprite {
                x: 10.0,
                y: 10.0,
                tint: [50, 200, 50],
                own: true,
                party: false,
            },
        ];
        let scene = Scene {
            bounds: bounds(-400.0, -300.0, 400.0, 300.0),
            triangles: &t,
            players: &players,
            fixture_banner: false,
            backdrop: None,
        };
        let v = build_vertices(&scene, 800, 600, false);
        // 2 floor triangles + 6 edge quads (2 tris each) + remote (2 squares) + own (2 diamonds).
        let expected_tris = 2 + 6 * 2 + 2 * 2 + 2 * 2;
        assert_eq!(v.len(), expected_tris * 3);
        assert!(
            v.iter()
                .all(|v| v.pos.iter().all(|c| c.is_finite() && c.abs() <= 1.0 + 1e-4)),
            "everything inside the viewport"
        );
        assert!(v.iter().all(|v| v.color[3] == 1.0));
        // The first floor triangle's first corner is at the bottom-left of the fitted area.
        assert!(v[0].pos[0] < -0.9 && v[0].pos[1] < -0.9, "{:?}", v[0].pos);
        // The own player is drawn last (on top), brighter than its tint.
        let own_fill = v[v.len() - 6..].iter().map(|v| v.color).next().unwrap();
        assert!(own_fill[0] > color([50, 200, 50], false)[0], "own player is brightened");

        let with_banner = Scene {
            fixture_banner: true,
            ..scene
        };
        let vb = build_vertices(&with_banner, 800, 600, false);
        assert!(vb.len() > v.len());
        // The banner sits at the very top of the viewport and is drawn last.
        let banner = &vb[v.len()..];
        assert!(banner.iter().all(|v| v.pos[1] >= 1.0 - 2.0 * BANNER_PX / 600.0 - 1e-4));
        assert!(banner.iter().any(|v| v.pos[1] >= 1.0 - 1e-4));
        assert_eq!(banner[0].color, color(BANNER_YELLOW, false));
    }

    #[test]
    fn non_finite_input_is_skipped_not_forwarded() {
        let nan = f32::NAN;
        let t = vec![[
            Pos::new(nan, 0.0, 0.0),
            Pos::new(1.0, 0.0, 0.0),
            Pos::new(0.0, 1.0, 0.0),
        ]];
        let players = [
            PlayerSprite {
                x: f32::INFINITY,
                y: 0.0,
                tint: [1, 2, 3],
                own: false,
                party: false,
            },
            PlayerSprite {
                x: 1.0,
                y: nan,
                tint: [1, 2, 3],
                own: true,
                party: false,
            },
        ];
        let scene = Scene {
            bounds: bounds(0.0, 0.0, 1.0, 1.0),
            triangles: &t,
            players: &players,
            fixture_banner: false,
            backdrop: None,
        };
        let v = build_vertices(&scene, 640, 480, true);
        assert!(v
            .iter()
            .all(|v| v.pos.iter().chain(v.color.iter()).all(|c| c.is_finite())));
        // Zero-sized viewports do not divide by zero.
        let v = build_vertices(&scene, 0, 0, false);
        assert!(v.iter().all(|v| v.pos.iter().all(|c| c.is_finite())));
    }

    #[test]
    fn vertex_budget_is_enforced() {
        let t = vec![
            [
                Pos::new(0.0, 0.0, 0.0),
                Pos::new(1.0, 0.0, 0.0),
                Pos::new(0.0, 1.0, 0.0)
            ];
            200_000
        ];
        let scene = Scene {
            bounds: bounds(0.0, 0.0, 1.0, 1.0),
            triangles: &t,
            players: &[],
            fixture_banner: true,
            backdrop: None,
        };
        let v = build_vertices(&scene, 640, 480, false);
        assert!(v.len() <= MAX_VERTICES);
    }

    fn sprite(x: f32, y: f32, own: bool, party: bool) -> PlayerSprite {
        PlayerSprite { x, y, tint: [60, 200, 90], own, party }
    }

    /// Pixels from the viewport centre of a vertex (the vertices are in NDC).
    fn px_of(v: &Vertex, vw: u32, vh: u32) -> (f32, f32) {
        (v.pos[0] * vw as f32 * 0.5, v.pos[1] * vh as f32 * 0.5)
    }

    #[test]
    fn the_ring_clears_every_marker_and_is_thin() {
        // Furthest corner of a remote square's outline and of the own diamond's outline.
        let square = (PLAYER_HALF_PX + OUTLINE_PX) * std::f32::consts::SQRT_2;
        let diamond = (OWN_HALF_PX + OUTLINE_PX) * 1.25;
        assert!(RING_INNER_PX > square + 1.0 && RING_INNER_PX > diamond + 1.0, "{square} {diamond}");
        const { assert!(RING_OUTER_PX - RING_INNER_PX <= 3.0 && RING_OUTER_PX > RING_INNER_PX) };
        assert_eq!(RING_SEGMENTS, 24);
    }

    /// Party members get a 24-segment ring (48 triangles) around their marker, in a gold colour;
    /// others get none; the own marker gets one when it is in a party.
    #[test]
    fn party_members_are_drawn_with_a_ring_and_nobody_else() {
        let t = tris();
        let (vw, vh) = (800_u32, 600_u32);
        let build = |players: &[PlayerSprite]| {
            let scene = Scene { bounds: bounds(-400.0, -300.0, 400.0, 300.0), triangles: &t, players, fixture_banner: false, backdrop: None };
            build_vertices(&scene, vw, vh, false)
        };
        let plain = [sprite(-100.0, 0.0, false, false), sprite(100.0, 0.0, false, false), sprite(0.0, 100.0, true, false)];
        let with_party = [sprite(-100.0, 0.0, false, true), sprite(100.0, 0.0, false, false), sprite(0.0, 100.0, true, true)];
        let (a, b) = (build(&plain), build(&with_party));
        let ring_tris = RING_SEGMENTS * 2;
        assert_eq!(b.len() - a.len(), 2 * ring_tris * 3, "two rings: the remote member's and the own marker's");

        let ring_color = color(RING_RGB, false);
        let ring: Vec<&Vertex> = b.iter().filter(|v| v.color == ring_color).collect();
        assert_eq!(ring.len(), 2 * ring_tris * 3);
        assert!(a.iter().all(|v| v.color != ring_color), "no ring without party members");
        assert!(ring.iter().all(|v| v.color[3] == 1.0), "the ring is opaque");

        // Every ring vertex lies on the inner or the outer circle of one of the two markers.
        let cam = Camera::fit(&bounds(-400.0, -300.0, 400.0, 300.0), vw as f32, vh as f32, MARGIN_PX);
        let centres = [cam.project(-100.0, 0.0), cam.project(0.0, 100.0)];
        for v in &ring {
            let (x, y) = px_of(v, vw, vh);
            let on_a_circle = centres.iter().any(|c| {
                let d = (x - c[0]).hypot(y - c[1]);
                (d - RING_INNER_PX).abs() < 1e-2 || (d - RING_OUTER_PX).abs() < 1e-2
            });
            assert!(on_a_circle, "({x}, {y}) is not on a ring of a party member");
        }
        // Each ring sits right after its own marker (so a marker is never covered by another's
        // ring): the first ring vertex comes after the remote marker's fill and before the own
        // marker's outline.
        let first_ring = b.iter().position(|v| v.color == ring_color).unwrap();
        let own_outline = b.iter().position(|v| v.color == color(OWN_OUTLINE_RGB, false)).unwrap();
        assert!(first_ring < own_outline, "remote markers (and their rings) come before the own marker");
        assert_eq!(b.iter().rposition(|v| v.color == ring_color).unwrap() + 1, b.len(), "the own ring is last");
        // The geometry does not depend on the party flag: everything else is identical.
        let strip = |v: &[Vertex]| v.iter().filter(|x| x.color != ring_color).copied().collect::<Vec<_>>();
        assert_eq!(strip(&a), strip(&b));
    }

    #[test]
    fn a_ring_around_a_marker_that_cannot_be_drawn_is_skipped_not_forwarded() {
        let t = tris();
        let players = [sprite(f32::NAN, 0.0, false, true), sprite(0.0, f32::INFINITY, true, true)];
        let scene = Scene { bounds: bounds(-400.0, -300.0, 400.0, 300.0), triangles: &t, players: &players, fixture_banner: false, backdrop: None };
        let v = build_vertices(&scene, 640, 480, true);
        assert!(v.iter().all(|v| v.pos.iter().chain(v.color.iter()).all(|c| c.is_finite())));
        assert!(v.iter().all(|v| v.color != color(RING_RGB, true)), "no ring for a marker with no position");
    }

    #[test]
    fn vertex_layout_matches_the_shader() {
        assert_eq!(std::mem::size_of::<Vertex>(), 24);
        let l = Vertex::layout();
        assert_eq!(l.array_stride, 24);
        assert_eq!(l.attributes.len(), 2);
        assert_eq!((l.attributes[0].shader_location, l.attributes[0].offset), (0, 0));
        assert_eq!((l.attributes[1].shader_location, l.attributes[1].offset), (1, 8));
        assert!(SHADER_WGSL.contains("vs_main") && SHADER_WGSL.contains("fs_main"));
    }

    // ------------------------------------------------------------------------------------
    // Offscreen GPU tests (they need a GPU or a software Vulkan/GL driver, e.g. lavapipe, so they
    // only run on request: `cargo test -p fmv-client -- --ignored offscreen`)
    // ------------------------------------------------------------------------------------

    /// Render `scene` into an offscreen `w` x `h` texture exactly the way the window does
    /// (backdrop upload, quad, vertices) and read the pixels back as tightly packed RGBA8.
    /// `None` (with a note) when no adapter is available.
    fn render_offscreen(w: u32, h: u32, scene: &Scene<'_>) -> Option<Vec<u8>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("skipped: no GPU adapter available ({e})");
                return None;
            }
        };
        eprintln!("offscreen adapter: {:?}", adapter.get_info().name);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..wgpu::DeviceDescriptor::default()
        }))
        .expect("device");
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut gpu = Gpu::new(device.clone(), queue.clone(), format);

        let quad = match scene.backdrop {
            Some(b) if gpu.ensure_backdrop(b) => backdrop_quad(scene, w, h),
            _ => None,
        };
        let vertices = build_vertices(scene, w, h, false);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        gpu.draw_scene(&view, &vertices, quad.as_ref(), CLEAR_RGB, false);

        // Rows of a texture-to-buffer copy are padded to 256 bytes.
        let padded = (w * 4).next_multiple_of(256);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).expect("send"));
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        rx.recv().expect("map callback").expect("map");
        let data = readback.slice(..).get_mapped_range().expect("mapped range");
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in data.chunks(padded as usize).take(h as usize) {
            out.extend_from_slice(&row[..(w * 4) as usize]);
        }
        Some(out)
    }

    fn pixel(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2]]
    }

    fn close(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
        a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
    }

    #[test]
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    fn offscreen_render_shows_floor_players_and_banner() {
        const W: u32 = 128;
        const H: u32 = 96;
        let t = tris();
        let players = [PlayerSprite { x: 0.0, y: 0.0, tint: [200, 40, 40], own: false, party: false }];
        let scene = Scene {
            bounds: bounds(-400.0, -300.0, 400.0, 300.0),
            triangles: &t,
            players: &players,
            fixture_banner: true,
            backdrop: None,
        };
        let Some(img) = render_offscreen(W, H, &scene) else { return };
        let px = |x: u32, y: u32| pixel(&img, W, x, y);

        // Margin corner (below the banner): clear colour.
        assert!(close(px(2, H - 3), CLEAR_RGB, 2), "clear: {:?}", px(2, H - 3));
        // A point on the floor away from the player and the diagonal edge: floor colour.
        assert!(
            close(px(W / 2 + 30, H / 2 + 10), FLOOR_RGB, 2) || close(px(W / 2 - 30, H / 2 - 10), FLOOR_RGB, 2),
            "floor: {:?}",
            px(W / 2 + 30, H / 2 + 10)
        );
        // The player marker at the centre: its tint.
        assert!(close(px(W / 2, H / 2), [200, 40, 40], 2), "player: {:?}", px(W / 2, H / 2));
        // The top row belongs to the hazard banner: yellow or black stripes.
        let mut seen_yellow = false;
        let mut seen_black = false;
        for x in 0..W {
            seen_yellow |= close(px(x, 1), BANNER_YELLOW, 2);
            seen_black |= close(px(x, 1), BANNER_BLACK, 2);
        }
        assert!(seen_yellow && seen_black, "banner stripes: first pixel {:?}", px(0, 0));
    }

    /// A gold ring surrounds party members' markers (and only theirs), at the ring radius.
    #[test]
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    fn offscreen_party_ring_surrounds_party_members_only() {
        const W: u32 = 240;
        const H: u32 = 180;
        let t = tris();
        // Far enough apart that no ring touches another marker or the floor's diagonal edge.
        let players = [
            PlayerSprite { x: -200.0, y: 0.0, tint: [40, 200, 40], own: false, party: true },
            PlayerSprite { x: 200.0, y: 0.0, tint: [40, 40, 200], own: false, party: false },
            PlayerSprite { x: 0.0, y: 200.0, tint: [200, 200, 40], own: true, party: true },
        ];
        let scene = Scene {
            bounds: bounds(-400.0, -300.0, 400.0, 300.0),
            triangles: &t,
            players: &players,
            fixture_banner: false,
            backdrop: None,
        };
        let Some(img) = render_offscreen(W, H, &scene) else { return };
        let px = |x: u32, y: u32| pixel(&img, W, x, y);

        // Where the markers are, in image pixels (the viewport centre is the middle of the image).
        let cam = Camera::fit(&scene.bounds, W as f32, H as f32, MARGIN_PX);
        let at = |p: &PlayerSprite| {
            let c = cam.project(p.x, p.y);
            ((W as f32 / 2.0 + c[0]).round() as i32, (H as f32 / 2.0 - c[1]).round() as i32)
        };
        let (member, outsider, own) = (at(&players[0]), at(&players[1]), at(&players[2]));
        eprintln!("markers at {member:?} (party member), {outsider:?} (not in the party), {own:?} (own, in the party)");
        let get = |c: (i32, i32), dx: i32, dy: i32| px((c.0 + dx) as u32, (c.1 + dy) as u32);

        // The ring radius is 18..20.5 px: the pixel at +19 in each direction is on the ring.
        // (Pixel (x, y) covers x..x+1; a ring pixel's centre is 19.5 px from the marker centre.)
        for (dx, dy) in [(19, 0), (-20, 0), (0, 19), (0, -20)] {
            let got = get(member, dx, dy);
            assert!(close(got, RING_RGB, 2), "member ring at ({dx}, {dy}): {got:?}");
            let got = get(own, dx, dy);
            assert!(close(got, RING_RGB, 2), "own ring at ({dx}, {dy}): {got:?}");
        }
        // Between the marker's outline and the ring there is floor; so there is outside the ring.
        for (dx, dy) in [(15, 0), (-16, 0), (24, 0), (-25, 0)] {
            let got = get(member, dx, dy);
            assert!(close(got, FLOOR_RGB, 2), "member gap/outside at ({dx}, {dy}): {got:?}");
        }
        // The marker itself is untouched: its tint at the centre.
        assert!(close(get(member, 0, 0), [40, 200, 40], 2), "member marker: {:?}", get(member, 0, 0));
        // Someone who is not in the party has no ring at that radius.
        for (dx, dy) in [(19, 0), (-20, 0), (0, 19), (0, -20)] {
            let got = get(outsider, dx, dy);
            assert!(close(got, FLOOR_RGB, 2), "no ring around a non-member at ({dx}, {dy}): {got:?}");
        }
        assert!(close(get(outsider, 0, 0), [40, 40, 200], 2), "outsider marker: {:?}", get(outsider, 0, 0));
    }

    /// A 2x2 picture (red, green / blue, yellow) covers a wide rectangle; a small floor is drawn
    /// over its middle. The picture must be the right way up, fill exactly its rectangle (the
    /// outside stays the clear colour), and the floor must be a translucent wash on top of it.
    #[test]
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    fn offscreen_backdrop_is_drawn_behind_the_walkmesh_and_the_right_way_up() {
        const W: u32 = 200;
        const H: u32 = 100;
        let rgba: Vec<u8> = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 0, 255]].concat();
        let picture = Backdrop::new(2, 2, rgba, [-200.0, -100.0], [200.0, 100.0]).unwrap();
        // A floor of 100 x 40 units around the centre, well inside the picture.
        let floor = vec![
            [Pos::new(-50.0, -20.0, 0.0), Pos::new(50.0, -20.0, 0.0), Pos::new(50.0, 20.0, 0.0)],
            [Pos::new(-50.0, -20.0, 0.0), Pos::new(50.0, 20.0, 0.0), Pos::new(-50.0, 20.0, 0.0)],
        ];
        let scene = Scene {
            // A viewport twice as wide as the picture's 4:2 shape needs, so there are side margins.
            bounds: bounds(-200.0, -100.0, 200.0, 100.0),
            triangles: &floor,
            players: &[],
            fixture_banner: false,
            backdrop: Some(&picture),
        };
        let Some(img) = render_offscreen(W, H, &scene) else { return };
        let px = |x: u32, y: u32| pixel(&img, W, x, y);
        // The same picture without the floor, to know what is behind the floor pixels.
        let bare_scene = Scene { triangles: &[], ..scene };
        let bare = render_offscreen(W, H, &bare_scene).expect("the first render found an adapter");

        // Fit: 24 px margins, 4:2 picture in a 200 x 100 window => 104 x 52 px, centred.
        let cam = Camera::fit(&scene.bounds, W as f32, H as f32, MARGIN_PX);
        let (left, right) = (cam.project(-200.0, 0.0)[0] + W as f32 / 2.0, cam.project(200.0, 0.0)[0] + W as f32 / 2.0);
        let (top, bottom) = (-cam.project(0.0, 100.0)[1] + H as f32 / 2.0, -cam.project(0.0, -100.0)[1] + H as f32 / 2.0);
        eprintln!("picture covers x {left:.1}..{right:.1}, y {top:.1}..{bottom:.1}");
        // Quarter points of the picture are the centres of its four texels: pure colours, with the
        // top row of the picture at the top of the window.
        let at = |fx: f32, fy: f32| px((left + (right - left) * fx) as u32, (top + (bottom - top) * fy) as u32);
        assert!(close(at(0.25, 0.10), [255, 0, 0], 3), "top left is red: {:?}", at(0.25, 0.10));
        assert!(close(at(0.75, 0.10), [0, 255, 0], 3), "top right is green: {:?}", at(0.75, 0.10));
        assert!(close(at(0.25, 0.90), [0, 0, 255], 3), "bottom left is blue: {:?}", at(0.25, 0.90));
        assert!(close(at(0.75, 0.90), [255, 255, 0], 3), "bottom right is yellow: {:?}", at(0.75, 0.90));
        // Outside the picture's rectangle: the clear colour.
        assert!(close(px(1, H / 2), CLEAR_RGB, 2), "outside: {:?}", px(1, H / 2));
        assert!(close(px(W - 2, H / 2), CLEAR_RGB, 2), "outside: {:?}", px(W - 2, H / 2));
        // The floor is a translucent wash: its colour mixed with what is behind it, at every
        // sampled pixel of the floor (the picture there blends all four texels).
        // (Away from the floor's diagonal edge, which is drawn as a bright line.)
        for (x, y) in [(W / 2 - 5, H / 2 - 3), (W / 2 + 5, H / 2 + 3), (W / 2 - 8, H / 2), (W / 2 + 8, H / 2)] {
            let behind = pixel(&bare, W, x, y);
            let a = BACKDROP_FLOOR_ALPHA;
            let mix = |f: u8, b: u8| (f32::from(f) * a + f32::from(b) * (1.0 - a)).round() as u8;
            let want = [mix(FLOOR_RGB[0], behind[0]), mix(FLOOR_RGB[1], behind[1]), mix(FLOOR_RGB[2], behind[2])];
            assert!(close(px(x, y), want, 3), "floor wash at ({x},{y}): {:?}, want {want:?} over {behind:?}", px(x, y));
            assert!(!close(px(x, y), behind, 20), "the floor must be visible over the picture at ({x},{y})");
        }
    }

    /// Real FF7 data, on request: render the field the way the window does and keep a PNG to look
    /// at. `FMV_FF7_FLEVEL` names the flevel.lgp (read-only), `FMV_FF7_AREA` the field (default
    /// `md1stin`), `FMV_SHOT_DIR` where the PNG goes (default `~/.cache/claude-tmp/fmv-client-shots`).
    /// Checks that the camera, the walkmesh and the picture agree: the walkmesh must project
    /// inside the picture, and a player standing on a spawn point must show up at its tint.
    #[test]
    #[ignore = "needs the owner's real flevel.lgp (FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
    fn offscreen_real_ff7_field_through_its_camera() {
        use crate::backdrop::FieldView;
        use crate::ff7::{open_ff7, prewarm, Ff7Labels, Ff7Source};
        use fmv_adapter_api::AreaRuntime;

        let Some(path) = std::env::var_os("FMV_FF7_FLEVEL") else {
            eprintln!("skipped: FMV_FF7_FLEVEL is not set");
            return;
        };
        let key = std::env::var("FMV_FF7_AREA").unwrap_or_else(|_| "md1stin".to_string());
        let opened = open_ff7(&Ff7Source::Flevel { path: path.into(), labels: Ff7Labels::default() }).expect("open");
        prewarm(&opened.adapter);
        let view = FieldView::ff7(&opened.adapter, &key).expect("the field has a camera and a walkmesh in front of it");
        eprintln!("{}", view.description);
        let picture = view.backdrop.as_ref().expect("the field's background decodes");
        eprintln!("backdrop {:?}; {} triangles; fit {:?}", picture, view.triangles.len(), view.bounds);

        // The walkmesh lies inside the picture's rectangle (it is drawn over that scenery).
        let (mut inside, mut total) = (0, 0);
        for p in view.triangles.iter().flatten() {
            total += 1;
            inside += usize::from(
                p.x >= picture.min[0] && p.x <= picture.max[0] && p.y >= picture.min[1] && p.y <= picture.max[1],
            );
        }
        assert!(total > 0 && inside * 10 >= total * 9, "{inside}/{total} walkmesh vertices project inside the background");

        // A player on every spawn point, drawn through the same mapping as the window uses.
        let area = opened.adapter.load_field(&key).expect("field");
        let spawns = area.spawn_points();
        let players: Vec<PlayerSprite> = spawns
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let p = view.to_view(*s)?;
                Some(PlayerSprite { x: p.x, y: p.y, tint: [[255, 40, 40], [40, 255, 40], [255, 255, 40], [40, 255, 255]][i % 4], own: i == 0, party: false })
            })
            .collect();
        assert!(!players.is_empty(), "no spawn point can be drawn");
        let (w, h) = (960_u32, 720_u32);
        let scene = Scene {
            bounds: view.bounds,
            triangles: &view.triangles,
            players: &players,
            fixture_banner: false,
            backdrop: view.backdrop.as_ref(),
        };
        let Some(img) = render_offscreen(w, h, &scene) else { return };

        // The first (own) player's marker is drawn in its brightened tint at its projected spot.
        let cam = Camera::fit(&scene.bounds, w as f32, h as f32, MARGIN_PX);
        let c = cam.project(players[0].x, players[0].y);
        let (mx, my) = ((c[0] + w as f32 / 2.0) as u32, (h as f32 / 2.0 - c[1]) as u32);
        let got = pixel(&img, w, mx, my);
        assert!(close(got, brighten(players[0].tint), 3), "own marker at ({mx},{my}): {got:?}");

        let dir = std::env::var("FMV_SHOT_DIR").unwrap_or_else(|_| {
            format!("{}/.cache/claude-tmp/fmv-client-shots", std::env::var("HOME").unwrap_or_else(|_| ".".into()))
        });
        std::fs::create_dir_all(&dir).expect("shot dir");
        let file = format!("{dir}/{key}-through-camera.png");
        std::fs::write(&file, write_png(w, h, &img)).expect("write png");
        eprintln!("wrote {file}");
    }

    /// Minimal PNG encoder for the shots above (stored deflate blocks, no compression).
    fn write_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc = 0xFFFF_FFFF_u32;
            for &b in data {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
                }
            }
            !crc
        }
        fn adler32(data: &[u8]) -> u32 {
            let (mut a, mut b) = (1_u32, 0_u32);
            for &x in data {
                a = (a + u32::from(x)) % 65_521;
                b = (b + a) % 65_521;
            }
            (b << 16) | a
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc32(&body).to_be_bytes());
        }
        let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
        for row in rgba.chunks(w as usize * 4) {
            raw.push(0); // filter: none
            raw.extend_from_slice(row);
        }
        let mut z = vec![0x78, 0x01];
        let mut blocks = raw.chunks(65_535).peekable();
        while let Some(block) = blocks.next() {
            z.push(u8::from(blocks.peek().is_none()));
            z.extend_from_slice(&(block.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            z.extend_from_slice(block);
        }
        z.extend_from_slice(&adler32(&raw).to_be_bytes());
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn the_png_writer_makes_a_valid_header_and_checksums() {
        let png = write_png(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 2);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        // The well-known CRC-32 check value.
        let crc = |data: &[u8]| {
            let mut c = 0xFFFF_FFFF_u32;
            for &b in data {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        };
        assert_eq!(crc(b"123456789"), 0xCBF4_3926);
    }

    // ------------------------------------------------------------------------------------
    // CPU-side backdrop geometry
    // ------------------------------------------------------------------------------------

    #[test]
    fn the_backdrop_quad_follows_the_camera_and_the_picture_is_the_right_way_up() {
        let picture = Backdrop::new(2, 2, vec![0; 16], [-200.0, -100.0], [200.0, 100.0]).unwrap();
        let none = Scene { bounds: bounds(-1.0, -1.0, 1.0, 1.0), triangles: &[], players: &[], fixture_banner: false, backdrop: None };
        assert!(backdrop_quad(&none, 100, 100).is_none(), "no picture, no quad");

        let scene = Scene { backdrop: Some(&picture), bounds: bounds(-200.0, -100.0, 200.0, 100.0), ..none };
        let q = backdrop_quad(&scene, 200, 100).expect("quad");
        // Fit with 24 px margins: 152 x 52 px are free, the 4:2 picture is limited by the height
        // (scale 0.26) and ends up 104 x 52 px, centred: NDC +-0.52 on both axes (52/100, 26/50).
        let (xs, ys): (Vec<f32>, Vec<f32>) = (q.iter().map(|v| v.pos[0]).collect(), q.iter().map(|v| v.pos[1]).collect());
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(xs.iter().all(|x| near(x.abs(), 0.52)) && ys.iter().all(|y| near(y.abs(), 0.52)), "{q:?}");
        // The picture's top-left texel (uv 0,0) is at the top left of the screen (NDC +y is up).
        let corner = |u: f32, v: f32| q.iter().find(|c| c.uv == [u, v]).expect("corner").pos;
        assert!(corner(0.0, 0.0)[0] < 0.0 && corner(0.0, 0.0)[1] > 0.0, "top left");
        assert!(corner(1.0, 0.0)[0] > 0.0 && corner(1.0, 0.0)[1] > 0.0, "top right");
        assert!(corner(0.0, 1.0)[0] < 0.0 && corner(0.0, 1.0)[1] < 0.0, "bottom left");
        assert!(corner(1.0, 1.0)[0] > 0.0 && corner(1.0, 1.0)[1] < 0.0, "bottom right");
        // Six vertices, two triangles covering all four corners.
        assert_eq!(q.len(), 6);
        // A zero-sized viewport does not divide by zero.
        assert!(backdrop_quad(&scene, 0, 0).is_some_and(|q| q.iter().all(|v| v.pos.iter().all(|c| c.is_finite()))));
    }

    #[test]
    fn a_backdrop_turns_the_floor_into_a_see_through_wash_without_changing_the_geometry() {
        let picture = Backdrop::new(1, 1, vec![9, 9, 9, 255], [-400.0, -300.0], [400.0, 300.0]).unwrap();
        let t = tris();
        let plain = Scene { bounds: bounds(-400.0, -300.0, 400.0, 300.0), triangles: &t, players: &[], fixture_banner: false, backdrop: None };
        let over = Scene { backdrop: Some(&picture), ..plain };
        let (a, b) = (build_vertices(&plain, 800, 600, false), build_vertices(&over, 800, 600, false));
        assert_eq!(a.len(), b.len(), "same triangles either way");
        assert!(a.iter().zip(&b).all(|(x, y)| x.pos == y.pos));
        // Floor triangles (the first two) are translucent over a picture; edges are brighter.
        assert!(a[..6].iter().all(|v| v.color[3] == 1.0));
        assert!(b[..6].iter().all(|v| (v.color[3] - BACKDROP_FLOOR_ALPHA).abs() < 1e-6));
        assert_eq!(b[6].color, color(BACKDROP_EDGE_RGB, false));
        assert_eq!(a[6].color, color(EDGE_RGB, false));
        assert!(b.iter().skip(6).all(|v| v.color[3] == 1.0), "edges and markers stay opaque");
    }

    #[test]
    fn the_quad_vertex_layout_matches_its_shader() {
        assert_eq!(std::mem::size_of::<QuadVertex>(), 16);
        let l = QuadVertex::layout();
        assert_eq!(l.array_stride, 16);
        assert_eq!((l.attributes[0].shader_location, l.attributes[0].offset), (0, 0));
        assert_eq!((l.attributes[1].shader_location, l.attributes[1].offset), (1, 8));
        assert!(QUAD_SHADER_WGSL.contains("vs_quad") && QUAD_SHADER_WGSL.contains("fs_quad"));
    }
}
