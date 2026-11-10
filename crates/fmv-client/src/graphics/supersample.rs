//! Optional 2x scene resolution: four rendered samples per output pixel, before FXAA and UI.
//! Only the raster targets grow. Geometry and camera layout are still built for the native
//! window, so the field's fit margins, battle framing and marker sizes do not change.

use super::RenderScale;

/// With four-byte RGBA8/BGRA8 colour targets, current colour plus Depth32Float targets use at
/// most 128 MiB at this limit (wider colour formats need more), excluding native output, optional
/// native FXAA, assets and older targets still in flight during resize. wgpu exposes dimensions,
/// not free VRAM;
/// this additional conservative allocation bound permits 2x 2560x1440 but falls back at 4K.
pub const MAX_SCENE_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub size: (u32, u32),
    pub effective: RenderScale,
}

impl Plan {
    pub fn new(native: (u32, u32), requested: RenderScale, dimension_limit: u32) -> Self {
        if requested == RenderScale::Double {
            if let Some((width, height)) = native.0.checked_mul(2).zip(native.1.checked_mul(2)) {
                if width > 0 && height > 0 && width <= dimension_limit && height <= dimension_limit
                    && u64::from(width) * u64::from(height) <= MAX_SCENE_PIXELS {
                    return Self { size: (width, height), effective: RenderScale::Double };
                }
            }
        }
        Self { size: native, effective: RenderScale::Native }
    }

    pub fn factor(self) -> f32 { if self.effective == RenderScale::Double { 2.0 } else { 1.0 } }
}

const SHADER: &str = r#"
@group(0) @binding(0) var scene: texture_2d<f32>;
@vertex
fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(corners[i], 0.0, 1.0);
}
@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // Exactly 2x on both axes, including odd native dimensions. Texture reads from sRGB
    // targets decode to linear and the output attachment encodes the averaged result again.
    let at = vec2<i32>(position.xy) * 2;
    return (textureLoad(scene, at, 0) + textureLoad(scene, at + vec2(1, 0), 0)
          + textureLoad(scene, at + vec2(0, 1), 0) + textureLoad(scene, at + vec2(1, 1), 0)) * 0.25;
}
"#;

struct Target {
    size: (u32, u32),
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

pub struct Supersample {
    pipeline: wgpu::RenderPipeline,
    format: wgpu::TextureFormat,
    target: Option<Target>,
}

impl Supersample {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fmv-2x-scene-resolve"), source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fmv-2x-scene-resolve"), layout: None,
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vertex"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &shader, entry_point: Some("fragment"), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: Default::default(), depth_stencil: None, multisample: Default::default(), multiview_mask: None, cache: None,
        });
        Self { pipeline, format, target: None }
    }

    /// Reuse one target until the window changes size. A fallback releases its old large target.
    pub fn target(&mut self, device: &wgpu::Device, plan: Plan) -> Option<wgpu::TextureView> {
        if plan.effective != RenderScale::Double { self.target = None; return None; }
        if self.target.as_ref().is_none_or(|target| target.size != plan.size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fmv-2x-scene"),
                size: wgpu::Extent3d { width: plan.size.0, height: plan.size.1, depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("fmv-2x-scene"), layout: &self.pipeline.get_bind_group_layout(0),
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }],
            });
            self.target = Some(Target { size: plan.size, _texture: texture, view, bind });
        }
        self.target.as_ref().map(|target| target.view.clone())
    }

    pub fn resolve(&self, device: &wgpu::Device, queue: &wgpu::Queue, output: &wgpu::TextureView) {
        let Some(target) = &self.target else { return };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fmv-2x-resolve") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fmv-2x-resolve"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output, depth_slice: None, resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &target.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avatar::AvatarFrame;
    use crate::battle::scene::BattleFrame;
    use crate::graphics::AntiAliasing;
    use crate::render::{Camera, Scene, Vertex};
    use crate::ui::draw::DrawList;
    use crate::ui::testkit::Offscreen;
    use fmv_adapter_api::Bounds;
    use fmv_core::Pos;

    #[test]
    fn doubled_targets_preserve_aspect_and_respect_dimensions_and_allocation_budget() {
        for native in [(1, 1), (319, 241), (1100, 680), (2560, 1440)] {
            let p = Plan::new(native, RenderScale::Double, 8192);
            assert_eq!(p.size, (native.0 * 2, native.1 * 2));
            assert_eq!(p.factor(), 2.0);
            assert_eq!(u64::from(p.size.0) * u64::from(native.1), u64::from(p.size.1) * u64::from(native.0));
            assert_eq!(Plan::new(native, RenderScale::Native, 8192).size, native);
        }
        for (native, limit) in [((0, 1), 8192), ((1, 0), 8192), ((1100, 680), 2048), ((3840, 2160), 16384), ((u32::MAX, 4), u32::MAX)] {
            assert_eq!(Plan::new(native, RenderScale::Double, limit), Plan { size: native, effective: RenderScale::Native });
        }
        assert_eq!(Plan::new((2048, 2048), RenderScale::Double, 4096).effective, RenderScale::Double, "both inclusive bounds");
    }

    fn pixel(image: &[u8], size: (u32, u32), x: u32, y: u32) -> &[u8] {
        let at = ((y * size.0 + x) * 4) as usize;
        &image[at..at + 4]
    }

    fn assert_native_hud(original: &[u8], rendered: &[u8], size: (u32, u32)) {
        for y in 0..24 { for x in 0..128 {
            assert_eq!(pixel(original, size, x, y), pixel(rendered, size, x, y), "native HUD at {x},{y}");
        } }
    }

    fn assert_changes_stay_at_edges(original: &[u8], rendered: &[u8], size: (u32, u32)) {
        let mut changed = 0;
        for y in 0..size.1 { for x in 0..size.0 {
            let before = pixel(original, size, x, y);
            if before == pixel(rendered, size, x, y) { continue; }
            changed += 1;
            // A coverage change must touch a boundary of the native image. This also catches
            // shifted margins, inverted coordinates, shrunken shapes, and changed solid colours.
            let boundary = (y.saturating_sub(1)..=(y + 1).min(size.1 - 1)).any(|ny|
                (x.saturating_sub(1)..=(x + 1).min(size.0 - 1)).any(|nx| pixel(original, size, nx, ny) != before));
            assert!(boundary, "changed solid interior at {x},{y}: {before:?} -> {:?}", pixel(rendered, size, x, y));
        } }
        assert!(changed > 40, "the extra samples must measurably change edge coverage ({changed} pixels)");
    }

    #[test]
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    fn offscreen_supersample_smooths_scene_edges_preserves_native_hud_and_composes_with_fxaa() {
        for format in [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Bgra8UnormSrgb] {
            let mut gpu = Offscreen::with_format(format).expect("GPU or software adapter");
            let bounds = Bounds { min: Pos::new(-1.0, -1.0, 0.0), max: Pos::new(1.0, 1.0, 0.0) };
            let triangles = [[Pos::new(-0.9, -0.8, 0.0), Pos::new(0.8, -0.3, 0.0), Pos::new(-0.2, 0.9, 0.0)]];
            let scene = Scene { bounds, triangles: &triangles, players: &[], fixture_banner: false, backdrop: None };
            // Exercise the 3D battle's wrapper too. Model projection/occlusion is checked by the
            // avatar shots; this asymmetric stage isolates raster coverage from model assets.
            let battle = BattleFrame {
                clear_rgb: [0, 0, 0],
                stage: vec![
                    Vertex { pos: [-0.9, -0.8], color: [1.0; 4] },
                    Vertex { pos: [0.8, -0.3], color: [1.0; 4] },
                    Vertex { pos: [-0.2, 0.9], color: [1.0; 4] },
                ],
                location: None,
                models: AvatarFrame { models: vec![], tints: vec![], identities: vec![], zoom: 1.0, overlay_bias: 0.0, ground: vec![], scenery: None },
                tints: vec![], identities: vec![], camera: Camera::pixels(0.0, 0.0),
            };
            let mut overlay = DrawList::new();
            overlay.fill_rect(0, 0, 128, 24, [8, 24, 80, 255]);
            overlay.draw_text(4, 8, 1, [255, 255, 255, 255], "NATIVE UI");
            let mut render = |scale, aa, kind, size: (u32, u32)| gpu.render_with(size.0, size.1, |gpu, target, srgb| {
                gpu.set_render_scale(scale);
                gpu.set_anti_aliasing(aa);
                match kind {
                    0 => gpu.draw_field(target, size, &scene, None, srgb),
                    1 => gpu.draw_battle(target, size, &battle, srgb),
                    _ => gpu.draw_flat(target, size, &battle.stage, srgb),
                }
                gpu.draw_overlay(target, &overlay.vertices(size.0, size.1, srgb));
            });
            for kind in 0..3 {
                let size = (256, 192);
                let original = render(RenderScale::Native, AntiAliasing::Off, kind, size);
                let double = render(RenderScale::Double, AntiAliasing::Off, kind, size);
                assert_changes_stay_at_edges(&original, &double, size);
                assert_native_hud(&original, &double, size);
                let combined = render(RenderScale::Double, AntiAliasing::Fxaa, kind, size);
                assert_native_hud(&original, &combined, size);
                assert!(combined != double, "FXAA still contributes after downsampling: {format:?}, path {kind}");
                let native_fxaa = render(RenderScale::Native, AntiAliasing::Fxaa, kind, size);
                assert!(combined != native_fxaa, "2x still contributes before FXAA: {format:?}, path {kind}");
                let odd_size = (319, 241);
                let odd_original = render(RenderScale::Native, AntiAliasing::Off, kind, odd_size);
                let odd_double = render(RenderScale::Double, AntiAliasing::Off, kind, odd_size);
                assert_changes_stay_at_edges(&odd_original, &odd_double, odd_size);
                assert_native_hud(&odd_original, &odd_double, odd_size);
                let odd_combined = render(RenderScale::Double, AntiAliasing::Fxaa, kind, odd_size);
                assert_native_hud(&odd_original, &odd_combined, odd_size);
                assert!(render(RenderScale::Double, AntiAliasing::Fxaa, kind, size) == combined, "resize back restores both passes");
                assert!(render(RenderScale::Double, AntiAliasing::Off, kind, size) == double, "turning off FXAA preserves 2x");
                assert!(render(RenderScale::Native, AntiAliasing::Off, kind, size) == original, "both off restore exact native pixels");
            }
        }
    }

    #[test]
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    fn offscreen_supersample_over_the_allocation_limit_falls_back_to_native_with_or_without_fxaa() {
        let mut gpu = Offscreen::new().expect("GPU or software adapter");
        let large = (2049, 2049);
        assert_eq!(Plan::new(large, RenderScale::Double, u32::MAX).effective, RenderScale::Native);
        let vertices = [
            Vertex { pos: [-0.9, -0.8], color: [1.0; 4] },
            Vertex { pos: [0.8, -0.3], color: [1.0; 4] },
            Vertex { pos: [-0.2, 0.9], color: [1.0; 4] },
        ];
        let mut render = |scale, aa, size: (u32, u32)| gpu.render_with(size.0, size.1, |gpu, target, srgb| {
            gpu.set_render_scale(scale);
            gpu.set_anti_aliasing(aa);
            gpu.draw_flat(target, size, &vertices, srgb);
        });
        for aa in [AntiAliasing::Off, AntiAliasing::Fxaa] {
            let native = render(RenderScale::Native, aa, large);
            let warm = render(RenderScale::Double, aa, (319, 241));
            assert!(render(RenderScale::Double, aa, large) == native, "fallback keeps the selected FXAA mode");
            assert!(render(RenderScale::Double, aa, (319, 241)) == warm, "returning within the cap restores 2x");
        }
    }
}
