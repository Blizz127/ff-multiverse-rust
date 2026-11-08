//! The GPU side of the [`Scenery`]: the atlas and the two static vertex lists uploaded once per
//! field, the opaque pipeline (depth tested and written) and one blended pipeline per
//! [`BlendMode`] (depth tested, not written).
//!
//! The pipelines share the avatar renderer's globals (the fitted camera's terms and the depth
//! mapping, see [`super::gpu`]) and its depth attachment, so a tile and a model are compared in one
//! buffer. A tile fragment is discarded below half alpha, so the transparent texels of a tile (the
//! palette's index 0) write neither colour nor depth.
//!
//! The four blend modes are the PlayStation modes of [`BlendMode::apply`], done by the blend unit
//! with the blend constant where a factor is a fraction:
//!
//! | mode | result | factors |
//! |---|---|---|
//! | `Average` | `(back + front) / 2` | constant 0.5 on both |
//! | `Additive` | `back + front` | one, one |
//! | `Subtractive` | `back - front` | one, one, reverse subtract |
//! | `AddQuarter` | `back + front / 4` | constant 0.25 on the front, one on the back |
//!
//! On an sRGB target the unit blends in linear space, which is not the byte arithmetic the game (and
//! [`fmv_ff7::background::decode_background`]) uses; the window prefers a non-sRGB surface, where
//! they agree to the rounding of the unit.

use super::gpu::{create_buffer_with, globals_wgsl, texture_bind_group, upload_texture, DEPTH_FORMAT};
use super::scenery::{BlendRun, Scenery, TileVertex};
use fmv_ff7::background::BlendMode;

/// WGSL of the tiles: field-screen quads placed by the fitted camera, at their own depth.
pub const TILE_SHADER_WGSL: &str = concat!(
    globals_wgsl!(),
    r#"
@group(1) @binding(0) var tile_tex: texture_2d<f32>;
@group(1) @binding(1) var tile_samp: sampler;

struct TileOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) bounds: vec4<f32>,
};

@vertex
fn vs_tile(
    @location(0) screen: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) bounds: vec4<f32>,
    @location(3) depth: f32,
) -> TileOut {
    var out: TileOut;
    // Field-screen units to NDC through the fitted camera (y is down in field-screen space); the
    // depth is the tile's own, in the buffer's mapping: the same function the models use.
    out.position = vec4<f32>(g.tile.x * screen.x + g.tile.y, g.tile.z * screen.y + g.tile.w, buffer_depth(depth), 1.0);
    out.uv = uv;
    out.bounds = bounds;
    return out;
}

@fragment
fn fs_tile(in: TileOut) -> @location(0) vec4<f32> {
    // The linear filter, but never beyond the tile's own edge texels.
    let texel = textureSample(tile_tex, tile_samp, clamp(in.uv, in.bounds.xy, in.bounds.zw));
    if (texel.a < 0.5) {
        discard;
    }
    // The atlas is premultiplied: the filtered colour of a silhouette is that of its opaque texels.
    return vec4<f32>(texel.rgb / texel.a, 1.0);
}
"#
);

const TILE_ATTRIBUTES: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32];

/// The tile vertex layout (one buffer: screen position, texture coordinate, clamp rectangle, depth).
pub fn tile_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<TileVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &TILE_ATTRIBUTES,
    }
}

/// The blend unit's setting for a mode (see the module docs).
pub fn blend_state(mode: BlendMode) -> wgpu::BlendState {
    use wgpu::{BlendFactor as F, BlendOperation as Op};
    let (src_factor, dst_factor, operation) = match mode {
        BlendMode::Average => (F::Constant, F::Constant, Op::Add),
        BlendMode::Additive => (F::One, F::One, Op::Add),
        BlendMode::Subtractive => (F::One, F::One, Op::ReverseSubtract),
        BlendMode::AddQuarter => (F::Constant, F::One, Op::Add),
    };
    wgpu::BlendState {
        color: wgpu::BlendComponent { src_factor, dst_factor, operation },
        // the target's alpha is left as it is
        alpha: wgpu::BlendComponent { src_factor: F::Zero, dst_factor: F::One, operation: Op::Add },
    }
}

/// The blend constant a mode's factors use (unused modes get 1).
pub fn blend_constant(mode: BlendMode) -> f64 {
    match mode {
        BlendMode::Average => 0.5,
        BlendMode::AddQuarter => 0.25,
        BlendMode::Additive | BlendMode::Subtractive => 1.0,
    }
}

fn mode_index(mode: BlendMode) -> usize {
    match mode {
        BlendMode::Average => 0,
        BlendMode::Additive => 1,
        BlendMode::Subtractive => 2,
        BlendMode::AddQuarter => 3,
    }
}

const MODES: [BlendMode; 4] = [BlendMode::Average, BlendMode::Additive, BlendMode::Subtractive, BlendMode::AddQuarter];

/// A scenery on the GPU.
struct Uploaded {
    id: u64,
    bind_group: wgpu::BindGroup,
    /// Owns the pixels the bind group's view looks at.
    _atlas: wgpu::Texture,
    opaque: wgpu::Buffer,
    opaque_vertices: u32,
    blended: Option<wgpu::Buffer>,
    runs: Vec<BlendRun>,
}

/// The tile pipelines and the scenery currently on the GPU (the one of the field being drawn).
pub struct SceneryGpu {
    opaque: wgpu::RenderPipeline,
    blended: [wgpu::RenderPipeline; 4],
    current: Option<Uploaded>,
    /// A scenery this device cannot hold (not tried again every frame).
    rejected: Option<u64>,
    uploads: u32,
}

impl SceneryGpu {
    /// Build the pipelines for targets of `format`; `layout` is the avatar renderer's globals
    /// (group 0) plus a texture and sampler (group 1).
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, layout: &wgpu::PipelineLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fmv-scenery-shader"),
            source: wgpu::ShaderSource::Wgsl(TILE_SHADER_WGSL.into()),
        });
        let make = |label: &'static str, blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_tile"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(tile_layout())],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_tile"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..wgpu::PrimitiveState::default()
                },
                // Equal depth passes: tiles of one id (and layer 4's, all at 0) paint over each
                // other in file order, exactly as the flat picture is composed. Only the opaque
                // pass writes depth; a blended tile is in front of what it blends with or it is not drawn.
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(blend.is_none()),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque = make("fmv-scenery-opaque", None);
        let blended = MODES.map(|mode| make("fmv-scenery-blended", Some(blend_state(mode))));
        Self { opaque, blended, current: None, rejected: None, uploads: 0 }
    }

    /// Scenery uploads so far (one per field, not per frame).
    #[cfg(test)]
    pub fn uploads(&self) -> u32 {
        self.uploads
    }

    /// The identity of the scenery on the GPU.
    #[cfg(test)]
    pub fn current_id(&self) -> Option<u64> {
        self.current.as_ref().map(|c| c.id)
    }

    /// Make sure `scenery` is on the GPU (uploaded on first use, replacing the last field's).
    /// `false` if this device cannot hold it: the frame is then drawn over the flat picture.
    pub fn ensure(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture_format: wgpu::TextureFormat,
        texture_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        scenery: &Scenery,
    ) -> bool {
        if self.current.as_ref().is_some_and(|c| c.id == scenery.id) {
            return true;
        }
        if self.rejected == Some(scenery.id) {
            return false;
        }
        self.current = None;
        let (w, h) = (scenery.atlas_width, scenery.atlas_height);
        let sound = w > 0
            && h > 0
            && w.max(h) <= device.limits().max_texture_dimension_2d
            && scenery.atlas.len() as u64 == u64::from(w) * u64::from(h) * 4
            && !scenery.opaque.is_empty();
        if !sound {
            self.rejected = Some(scenery.id);
            return false;
        }
        let atlas = upload_texture(device, queue, texture_format, w, h, &scenery.atlas);
        let bind_group = texture_bind_group(device, texture_layout, &atlas, sampler);
        let opaque = create_buffer_with(device, queue, "fmv-scenery-opaque", wgpu::BufferUsages::VERTEX, bytemuck::cast_slice(&scenery.opaque));
        let blended = (!scenery.blended.is_empty())
            .then(|| create_buffer_with(device, queue, "fmv-scenery-blended", wgpu::BufferUsages::VERTEX, bytemuck::cast_slice(&scenery.blended)));
        self.current = Some(Uploaded {
            id: scenery.id,
            bind_group,
            _atlas: atlas,
            opaque,
            opaque_vertices: u32::try_from(scenery.opaque.len()).unwrap_or(u32::MAX),
            blended,
            runs: scenery.runs.clone(),
        });
        self.uploads += 1;
        true
    }

    /// Draw the base quad and every opaque tile, in paint order, in one call (the pass has the
    /// globals at group 0).
    pub fn draw_opaque(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(s) = &self.current else { return };
        pass.set_pipeline(&self.opaque);
        pass.set_bind_group(1, &s.bind_group, &[]);
        pass.set_vertex_buffer(0, s.opaque.slice(..));
        pass.draw(0..s.opaque_vertices, 0..1);
    }

    /// Draw the blended tiles in paint order: one call per run of one mode.
    pub fn draw_blended(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(s) = &self.current else { return };
        let Some(buffer) = &s.blended else { return };
        pass.set_bind_group(1, &s.bind_group, &[]);
        pass.set_vertex_buffer(0, buffer.slice(..));
        for run in &s.runs {
            let c = blend_constant(run.mode);
            pass.set_pipeline(&self.blended[mode_index(run.mode)]);
            pass.set_blend_constant(wgpu::Color { r: c, g: c, b: c, a: c });
            pass.draw(run.first..run.first.saturating_add(run.count), 0..1);
        }
    }

    /// Draw calls of one frame: the opaque tiles' one and one per blended run.
    #[cfg(test)]
    pub fn draw_calls(&self) -> usize {
        self.current.as_ref().map_or(0, |s| 1 + s.runs.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tile_vertex_layout_matches_the_shader() {
        assert_eq!(std::mem::size_of::<TileVertex>(), 36);
        let l = tile_layout();
        assert_eq!(l.array_stride, 36);
        assert_eq!(l.attributes.iter().map(|a| (a.shader_location, a.offset)).collect::<Vec<_>>(), [(0, 0), (1, 8), (2, 16), (3, 32)]);
        for entry in ["vs_tile", "fs_tile", "buffer_depth", "@interpolate(flat)"] {
            assert!(TILE_SHADER_WGSL.contains(entry), "{entry}");
        }
    }

    #[test]
    fn each_mode_is_the_blend_of_the_game() {
        // Evaluate the unit's arithmetic on bytes and compare with the game's own `BlendMode::apply`.
        let eval = |mode: BlendMode, back: f32, front: f32| -> f32 {
            let s = blend_state(mode).color;
            let c = blend_constant(mode) as f32;
            let factor = |f: wgpu::BlendFactor| match f {
                wgpu::BlendFactor::One => 1.0,
                wgpu::BlendFactor::Constant => c,
                other => panic!("{other:?}"),
            };
            let (a, b) = (front * factor(s.src_factor), back * factor(s.dst_factor));
            match s.operation {
                wgpu::BlendOperation::Add => a + b,
                wgpu::BlendOperation::ReverseSubtract => b - a,
                other => panic!("{other:?}"),
            }
            .clamp(0.0, 1.0)
        };
        for mode in MODES {
            for (back, front) in [(0u8, 0u8), (200, 100), (30, 220), (255, 255), (90, 90), (0, 255), (255, 0)] {
                let got = (eval(mode, f32::from(back) / 255.0, f32::from(front) / 255.0) * 255.0).round() as i32;
                let want = i32::from(mode.apply(back, front));
                // (the game's integer divisions truncate: one level of slack)
                assert!((got - want).abs() <= 1, "{mode:?} back {back} front {front}: unit {got}, game {want}");
            }
        }
        // The alpha of the target is never touched.
        assert!(MODES.iter().all(|m| {
            let a = blend_state(*m).alpha;
            (a.src_factor, a.dst_factor, a.operation) == (wgpu::BlendFactor::Zero, wgpu::BlendFactor::One, wgpu::BlendOperation::Add)
        }));
        // Every mode has its own pipeline slot.
        let mut seen = [false; 4];
        for m in MODES {
            seen[mode_index(m)] = true;
        }
        assert!(seen.iter().all(|s| *s));
    }
}
