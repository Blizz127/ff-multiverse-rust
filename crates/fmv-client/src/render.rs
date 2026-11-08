//! Renderer: a tiny vertex-coloured wgpu pipeline that draws an orthographic debug view of the
//! area (walkable triangles, players, FIXTURE banner).
//!
//! * CPU side ([`Camera`], [`build_vertices`]): world -> pixels -> NDC, pure and unit-tested.
//! * GPU side ([`Gpu`]): one pipeline, one growing vertex buffer, renders into any texture view,
//!   so it can also be exercised offscreen.
//! * Surface side ([`Renderer`]): window surface lifecycle (resize, minimise, suboptimal,
//!   outdated, lost) without panics.

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
pub const BANNER_YELLOW: [u8; 3] = [255, 196, 0];
pub const BANNER_BLACK: [u8; 3] = [24, 24, 24];

// Sizes in pixels.
const MARGIN_PX: f32 = 24.0;
const EDGE_PX: f32 = 1.25;
const PLAYER_HALF_PX: f32 = 9.0;
const OWN_HALF_PX: f32 = 10.0;
const OUTLINE_PX: f32 = 2.5;
pub const BANNER_PX: f32 = 18.0;

/// One player marker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerSprite {
    pub x: f32,
    pub y: f32,
    pub tint: [u8; 3],
    /// The local player: drawn as an outlined, brightened diamond on top of everyone else.
    pub own: bool,
}

/// Everything needed to draw one frame.
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    pub bounds: Bounds,
    pub triangles: &'a [[Pos; 3]],
    pub players: &'a [PlayerSprite],
    /// Draw the hazard-stripe FIXTURE banner across the top.
    pub fixture_banner: bool,
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

    #[cfg(test)]
    pub fn scale(&self) -> f32 {
        self.scale
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
        if self.out.len() + 3 > MAX_VERTICES || ![a, b, c].iter().all(|p| p[0].is_finite() && p[1].is_finite()) {
            return;
        }
        let col = color(rgb, self.srgb);
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
/// banner. Non-finite input is skipped, never propagated to the GPU.
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
    for t in scene.triangles {
        b.tri(px(&t[0]), px(&t[1]), px(&t[2]), FLOOR_RGB);
    }
    for t in scene.triangles {
        for (i, j) in [(0, 1), (1, 2), (2, 0)] {
            b.line(px(&t[i]), px(&t[j]), EDGE_PX, EDGE_RGB);
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
    }
    if scene.fixture_banner {
        b.banner();
    }
    b.out
}

// ---------------------------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------------------------

/// Pipeline + vertex buffer, independent of any window surface.
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    vertex_buf: wgpu::Buffer,
    vertex_cap: u64,
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
        Self {
            device,
            queue,
            pipeline,
            vertex_buf,
            vertex_cap: MIN_VERTEX_BYTES,
        }
    }

    fn create_vertex_buffer(device: &wgpu::Device, bytes: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fmv-vertices"),
            size: bytes,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Clear `target` and draw `vertices` into it.
    pub fn draw(&mut self, target: &wgpu::TextureView, vertices: &[Vertex], clear_rgb: [u8; 3], srgb: bool) {
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
        let c = color(clear_rgb, srgb);
        let clear = wgpu::Color {
            r: f64::from(c[0]),
            g: f64::from(c[1]),
            b: f64::from(c[2]),
            a: 1.0,
        };

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
            // `set_vertex_buffer` panics on an empty slice, so only draw when there is geometry.
            if !bytes.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.vertex_buf.slice(0..need));
                pass.draw(0..u32::try_from(vertices.len()).unwrap_or(u32::MAX), 0..1);
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
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
        })
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

    pub fn render(&mut self, scene: &Scene<'_>) -> FrameResult {
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
        let vertices = build_vertices(scene, w, h, self.srgb);
        self.gpu.draw(&view, &vertices, CLEAR_RGB, self.srgb);
        self.window.pre_present_notify();
        self.gpu.queue.present(frame);
        FrameResult::Presented
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            },
            PlayerSprite {
                x: 10.0,
                y: 10.0,
                tint: [50, 200, 50],
                own: true,
            },
        ];
        let scene = Scene {
            bounds: bounds(-400.0, -300.0, 400.0, 300.0),
            triangles: &t,
            players: &players,
            fixture_banner: false,
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
            },
            PlayerSprite {
                x: 1.0,
                y: nan,
                tint: [1, 2, 3],
                own: true,
            },
        ];
        let scene = Scene {
            bounds: bounds(0.0, 0.0, 1.0, 1.0),
            triangles: &t,
            players: &players,
            fixture_banner: false,
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
        };
        let v = build_vertices(&scene, 640, 480, false);
        assert!(v.len() <= MAX_VERTICES);
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

    /// Renders a scene into an offscreen texture and checks real pixels. Needs a GPU or a software
    /// Vulkan/GL driver (e.g. lavapipe), so it only runs on request:
    /// `cargo test -p fmv-client -- --ignored offscreen`.
    #[test]
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    fn offscreen_render_shows_floor_players_and_banner() {
        const W: u32 = 128;
        const H: u32 = 96;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("skipped: no GPU adapter available ({e})");
                return;
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

        let t = tris();
        let players = [PlayerSprite {
            x: 0.0,
            y: 0.0,
            tint: [200, 40, 40],
            own: false,
        }];
        let scene = Scene {
            bounds: bounds(-400.0, -300.0, 400.0, 300.0),
            triangles: &t,
            players: &players,
            fixture_banner: true,
        };
        let vertices = build_vertices(&scene, W, H, false);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        gpu.draw(&view, &vertices, CLEAR_RGB, false);

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(W * 4 * H),
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
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(W * 4),
                    rows_per_image: Some(H),
                },
            },
            wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| tx.send(r).expect("send"));
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        rx.recv().expect("map callback").expect("map");
        let data = readback.slice(..).get_mapped_range().expect("mapped range");
        let px = |x: u32, y: u32| -> [u8; 3] {
            let i = ((y * W + x) * 4) as usize;
            [data[i], data[i + 1], data[i + 2]]
        };
        let close = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2);

        // Margin corner (below the banner): clear colour.
        assert!(close(px(2, H - 3), CLEAR_RGB), "clear: {:?}", px(2, H - 3));
        // A point on the floor away from the player and the diagonal edge: floor colour.
        assert!(
            close(px(W / 2 + 30, H / 2 + 10), FLOOR_RGB) || close(px(W / 2 - 30, H / 2 - 10), FLOOR_RGB),
            "floor: {:?}",
            px(W / 2 + 30, H / 2 + 10)
        );
        // The player marker at the centre: its tint.
        assert!(close(px(W / 2, H / 2), [200, 40, 40]), "player: {:?}", px(W / 2, H / 2));
        // The top row belongs to the hazard banner: yellow or black stripes.
        let top = px(0, 0);
        let mut seen_yellow = false;
        let mut seen_black = false;
        for x in 0..W {
            seen_yellow |= close(px(x, 1), BANNER_YELLOW);
            seen_black |= close(px(x, 1), BANNER_BLACK);
        }
        assert!(seen_yellow && seen_black, "banner stripes: first pixel {top:?}");
    }
}
