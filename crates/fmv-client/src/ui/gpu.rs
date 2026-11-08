//! The GPU side of the UI overlay: the font atlas texture and one textured, alpha-blended
//! pipeline that draws a [`DrawList`](crate::ui::draw::DrawList)'s vertices over whatever is
//! already in the target.
//!
//! The overlay is a second render pass with `LoadOp::Load`, run after the scene's own pass, so the
//! scene renderer needs no change beyond calling it. Sampling is NEAREST (never linear): every
//! font pixel stays a sharp square at any integer scale.

use super::draw::UiVertex;
use super::font::{Atlas, ATLAS_H, ATLAS_W};

/// WGSL of the overlay: position is already NDC; the atlas alpha (glyph coverage, or 1 inside the
/// solid block) multiplies the vertex alpha, the colour is the vertex colour.
pub const UI_SHADER_WGSL: &str = r#"
struct UiOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@group(0) @binding(0) var atlas_tex: texture_2d<f32>;
@group(0) @binding(1) var atlas_samp: sampler;

@vertex
fn vs_ui(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> UiOut {
    var out: UiOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_ui(in: UiOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas_tex, atlas_samp, in.uv);
    return vec4<f32>(in.color.rgb, in.color.a * texel.a);
}
"#;

const ATTRIBUTES: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<UiVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

/// Upper bound on overlay vertices per frame (about 32 MB); more is cut off, never an error.
pub const MAX_UI_VERTICES: usize = 1_000_000;
const MIN_VERTEX_BYTES: u64 = 32 * 1024;

/// Pipeline, atlas and vertex buffer of the overlay.
pub struct UiGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    vertex_buf: wgpu::Buffer,
    vertex_cap: u64,
    /// Owns the pixels the bind group's view looks at.
    _atlas: wgpu::Texture,
}

impl UiGpu {
    /// Build the pipeline for targets of `format` and upload the font atlas.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fmv-ui-shader"),
            source: wgpu::ShaderSource::Wgsl(UI_SHADER_WGSL.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fmv-ui-pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_ui"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(vertex_layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_ui"),
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

        let atlas = Atlas::build();
        let size = wgpu::Extent3d { width: ATLAS_W, height: ATLAS_H, depth_or_array_layers: 1 };
        // Plain (not sRGB) RGBA: the colour is white and only the alpha carries information.
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fmv-ui-atlas"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_W * 4),
                rows_per_image: Some(ATLAS_H),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fmv-ui-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        let layout = pipeline.get_bind_group_layout(0);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fmv-ui-bind"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        Self {
            pipeline,
            bind_group,
            vertex_buf: Self::create_vertex_buffer(device, MIN_VERTEX_BYTES),
            vertex_cap: MIN_VERTEX_BYTES,
            _atlas: texture,
        }
    }

    fn create_vertex_buffer(device: &wgpu::Device, bytes: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fmv-ui-vertices"),
            size: bytes,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Draw `vertices` over what `target` already holds. Nothing happens for an empty list
    /// (`set_vertex_buffer` panics on an empty slice).
    pub fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::TextureView, vertices: &[UiVertex]) {
        let vertices = &vertices[..vertices.len().min(MAX_UI_VERTICES)];
        if vertices.is_empty() {
            return;
        }
        let bytes: &[u8] = bytemuck::cast_slice(vertices);
        let need = bytes.len() as u64;
        if need > self.vertex_cap {
            let cap = need.next_power_of_two().max(MIN_VERTEX_BYTES);
            self.vertex_buf = Self::create_vertex_buffer(device, cap);
            self.vertex_cap = cap;
        }
        queue.write_buffer(&self.vertex_buf, 0, bytes);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fmv-ui-frame") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fmv-ui-pass"),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buf.slice(0..need));
            pass.draw(0..u32::try_from(vertices.len()).unwrap_or(u32::MAX), 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vertex_layout_matches_the_shader() {
        assert_eq!(std::mem::size_of::<UiVertex>(), 32);
        let l = vertex_layout();
        assert_eq!(l.array_stride, 32);
        assert_eq!(l.attributes.len(), 3);
        let at = |i: usize| (l.attributes[i].shader_location, l.attributes[i].offset);
        assert_eq!((at(0), at(1), at(2)), ((0, 0), (1, 8), (2, 16)));
        for needle in ["vs_ui", "fs_ui", "textureSample", "in.color.a * texel.a"] {
            assert!(UI_SHADER_WGSL.contains(needle), "{needle}");
        }
    }

    // ------------------------------------------------------------------------------------
    // Offscreen GPU tests (they need a GPU or a software Vulkan/GL driver such as llvmpipe, so they
    // only run on request: `cargo test -p fmv-client -- --ignored offscreen`)
    // ------------------------------------------------------------------------------------

    use crate::render::CLEAR_RGB;
    use crate::ui::draw::DrawList;
    use crate::ui::font::{glyph_rows, Special};
    use crate::ui::testkit::{empty_scene, pixel, save, Offscreen};

    const INK: [u8; 3] = [255, 240, 120];

    /// Glyphs and rectangles reach the screen pixel-exact: every font pixel is a square of
    /// `scale` x `scale` pixels, lit ones in the text colour, unlit ones untouched; plain
    /// rectangles are their exact colour; translucent ones blend with what is behind them.
    #[test]
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    fn offscreen_glyphs_are_pixel_exact_at_integer_scales() {
        let Some(mut gpu) = Offscreen::new() else { return };
        const W: u32 = 200;
        const H: u32 = 120;
        let mut list = DrawList::new();
        // The same letter at three scales, at awkward (odd) pixel positions.
        let spots = [(13_i32, 7_i32, 1_u32), (40, 7, 3), (100, 31, 5)];
        for (x, y, scale) in spots {
            list.draw_text(x, y, scale, [INK[0], INK[1], INK[2], 255], "G");
        }
        list.fill_rect(10, 90, 20, 10, [200, 40, 40, 255]);
        list.fill_rect(50, 90, 10, 10, [255, 255, 255, 128]);
        list.draw_special(70, 90, 1, [0, 255, 0, 255], Special::ArrowRight);
        let img = gpu.render(W, H, &empty_scene(), &list);

        for (x, y, scale) in spots {
            let rows = glyph_rows('G');
            for row in 0..8_u32 {
                for col in 0..8_u32 {
                    let lit = (rows[row as usize] >> col) & 1 == 1;
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let (px, py) = (x as u32 + col * scale + dx, y as u32 + row * scale + dy);
                            let want = if lit { INK } else { CLEAR_RGB };
                            assert_eq!(pixel(&img, W, px, py), want, "scale {scale}: font pixel ({col},{row}) at screen ({px},{py})");
                        }
                    }
                }
            }
        }
        assert_eq!(pixel(&img, W, 15, 95), [200, 40, 40], "an opaque rectangle is its exact colour");
        assert_eq!(pixel(&img, W, 9, 95), CLEAR_RGB, "and stops at its edge");
        assert_eq!(pixel(&img, W, 30, 95), CLEAR_RGB);
        let blended = pixel(&img, W, 55, 95);
        for c in 0..3 {
            let want = (f32::from(CLEAR_RGB[c]) * 0.5 + 255.0 * 0.5).round() as i32;
            assert!((i32::from(blended[c]) - want).abs() <= 2, "alpha blend: {blended:?}");
        }
        // The arrow's first row has one lit pixel at its left edge; its widest rows four.
        assert_eq!(pixel(&img, W, 70, 90), [0, 255, 0]);
        assert_eq!(pixel(&img, W, 71, 90), CLEAR_RGB);
        assert_eq!(pixel(&img, W, 73, 93), [0, 255, 0]);
    }

    /// All printable ASCII, a few special glyphs and the "missing character" box at 3x, written
    /// out as a PNG to look at: a wrong byte in the glyph table is visible here.
    #[test]
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    fn offscreen_glyph_sheet() {
        let Some(mut gpu) = Offscreen::new() else { return };
        const W: u32 = 620;
        const H: u32 = 330;
        let mut list = DrawList::new();
        list.fill_rect(0, 0, W as i32, H as i32, [10, 14, 40, 255]);
        let white = [255, 255, 255, 255];
        let dim = [120, 130, 170, 255];
        list.draw_text(8, 6, 2, [255, 220, 90, 255], "GLYPH SHEET - font8x8 (public domain)");
        // Four rows of 24 characters: code points 0x20.. in order.
        let chars: Vec<char> = (0x20_u8..=0x7E).map(char::from).collect();
        for (i, chunk) in chars.chunks(24).enumerate() {
            let y = 34 + i as i32 * 44;
            let line: String = chunk.iter().collect();
            list.draw_text(8, y, 3, white, &line);
            // The same characters at 1x underneath, as they appear in body text.
            list.draw_text(8, y + 28, 1, dim, &line);
        }
        let y = 34 + 4 * 44;
        list.draw_text(8, y, 3, white, "ABC xyz 0123 \u{e9}\u{4e2d}");
        list.draw_special(8 + 14 * 24, y, 3, [255, 220, 90, 255], Special::ArrowRight);
        list.draw_special(8 + 15 * 24, y, 3, [255, 220, 90, 255], Special::ArrowLeft);
        list.draw_special(8 + 16 * 24, y, 3, [255, 220, 90, 255], Special::ArrowUp);
        list.draw_special(8 + 17 * 24, y, 3, [255, 220, 90, 255], Special::ArrowDown);
        list.draw_text(8, y + 34, 2, dim, "The quick brown fox jumps over the lazy dog 0123456789");
        let img = gpu.render(W, H, &empty_scene(), &list);
        save("glyph-sheet", W, H, &img);
        assert!(img.chunks(4).any(|p| p[..3] == [255, 255, 255]), "something was drawn");
    }
}
