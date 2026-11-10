//! Authored field fades over the completed scene, before dialogue and HUD.
//!
//! The blend is addition or subtraction of display RGB, not a translucent colour laid over
//! the field. The CPU interpreter supplies the already timed colour. A separate native-size
//! target keeps the operation after supersampling/FXAA and avoids reading a surface texture.
//! sRGB targets are decoded by texture loads: convert back to display RGB for the original
//! arithmetic, then back to linear for the attachment's sRGB encoding.
//!
//! Primary implementation evidence: FFNx's scene compositor, commit
//! [f9100649](https://github.com/julianxhokaxhiu/FFNx/commit/f9100649b2b5140f172a6b43d3e23b7aca52e771),
//! and the PS1 updater in Q-Gears' [system_fade.txt](https://github.com/q-gears/q-gears-reversing-data/blob/2b155a8c5455f5fc8addd2eda9e5cd4c226abe34/reversing/ffvii/ffvii_system/system_fade.txt).
//! Both composite the scene before dialogue; this pass does not change UI colours.

use fmv_ff7::script::scene::{FadeBlend, FadeColor};

const SHADER: &str = r#"
const SRGB_TARGET: bool = false;
struct Fade { rgb: vec3<f32>, mode: f32 };
@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var<uniform> fade: Fade;

@vertex
fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(corners[i], 0.0, 1.0);
}
fn display_rgb(linear: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(linear, vec3(1.0 / 2.4)) - vec3(0.055), linear * 12.92,
        linear <= vec3(0.0031308));
}
fn linear_rgb(display: vec3<f32>) -> vec3<f32> {
    return select(pow((display + vec3(0.055)) / 1.055, vec3(2.4)), display / 12.92,
        display <= vec3(0.04045));
}
@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = textureLoad(scene, vec2<i32>(position.xy), 0);
    var colour = pixel.rgb;
    if SRGB_TARGET { colour = display_rgb(colour); }
    if fade.mode < 0.5 {
        colour += fade.rgb;
    } else if fade.mode < 1.5 {
        colour -= fade.rgb;
    } else {
        colour = vec3(0.0);
    }
    colour = clamp(colour, vec3(0.0), vec3(1.0));
    if SRGB_TARGET { colour = linear_rgb(colour); }
    return vec4(colour, pixel.a);
}
"#;

struct Target {
    size: (u32, u32),
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

pub struct FadeGpu {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    format: wgpu::TextureFormat,
    target: Option<Target>,
}

impl FadeGpu {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let source = SHADER.replace("const SRGB_TARGET: bool = false;", &format!("const SRGB_TARGET: bool = {};", format.is_srgb()));
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fmv-field-fade"), source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fmv-field-fade"), layout: None,
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vertex"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &shader, entry_point: Some("fragment"), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: Default::default(), multiview_mask: None, cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fmv-field-fade-colour"), size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        Self { pipeline, uniform, format, target: None }
    }

    /// The entire scene, including its optional resolution/edge filters, is drawn here first.
    /// The target is replaced on resize and reused otherwise.
    pub fn target(&mut self, device: &wgpu::Device, size: (u32, u32)) -> wgpu::TextureView {
        let size = (size.0.max(1), size.1.max(1));
        if self.target.as_ref().is_none_or(|target| target.size != size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fmv-scene-before-field-fade"),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("fmv-field-fade-scene"), layout: &self.pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: self.uniform.as_entire_binding() },
                ],
            });
            self.target = Some(Target { size, _texture: texture, view, bind });
        }
        self.target.as_ref().expect("target was created").view.clone()
    }

    pub fn resolve(&self, device: &wgpu::Device, queue: &wgpu::Queue, output: &wgpu::TextureView, colour: FadeColor) {
        let Some(target) = &self.target else { return };
        let mode = match colour.blend { FadeBlend::Add => 0.0, FadeBlend::Subtract => 1.0, FadeBlend::Black => 2.0 };
        let rgba = [f32::from(colour.rgb[0]) / 255.0, f32::from(colour.rgb[1]) / 255.0, f32::from(colour.rgb[2]) / 255.0, mode];
        queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&rgba));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fmv-field-fade-resolve") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fmv-field-fade-resolve"),
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
    use crate::backdrop::Backdrop;
    use crate::graphics::{AntiAliasing, RenderScale};
    use crate::render::Scene;
    use crate::ui::draw::DrawList;
    use crate::ui::testkit::Offscreen;
    use fmv_adapter_api::Bounds;
    use fmv_core::Pos;

    #[test]
    #[allow(clippy::chunks_exact_to_as_chunks)] // as_chunks needs Rust 1.88; retain the workspace's older MSRV.
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    fn offscreen_authored_fade_composites_display_rgb_after_scene_filters_and_before_ui() {
        for format in [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Bgra8UnormSrgb] {
            let mut gpu = Offscreen::with_format(format).expect("GPU or software adapter");
            // Distinct channels and an oblique foreground edge expose multiply/alpha mistakes,
            // swapped channels, flipped sampling and any fade applied before scene filtering.
            let picture = Backdrop::new(2, 2, vec![
                24, 180, 220, 255, 208, 40, 112, 255,
                160, 220, 8, 255, 112, 64, 224, 255,
            ], [-2.0, -2.0], [2.0, 2.0]).unwrap();
            let bounds = Bounds { min: Pos::new(-1.0, -1.0, 0.0), max: Pos::new(1.0, 1.0, 0.0) };
            let triangles = [[Pos::new(-0.9, -0.8, 0.0), Pos::new(0.8, -0.3, 0.0), Pos::new(-0.2, 0.9, 0.0)]];
            let scene = Scene { bounds, triangles: &triangles, players: &[], fixture_banner: false, backdrop: Some(&picture) };
            let mut overlay = DrawList::new();
            overlay.fill_rect(0, 0, 96, 24, [8, 24, 80, 255]);
            overlay.draw_text(4, 8, 1, [255, 255, 255, 255], "CLEAR UI");
            let mut render = |scale, aa, fade, size: (u32, u32)| {
                let mut pixels = gpu.render_with(size.0, size.1, |gpu, target, srgb| {
                    gpu.set_render_scale(scale);
                    gpu.set_anti_aliasing(aa);
                    gpu.draw_field_effects(target, size, &scene, None, srgb, None, fade);
                    gpu.draw_overlay(target, &overlay.vertices(size.0, size.1, srgb));
                });
                // Readback is native byte order; desktop surfaces often use BGRA.
                if matches!(format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb) {
                    for pixel in pixels.chunks_exact_mut(4) { pixel.swap(0, 2); }
                }
                pixels
            };
            for scale in [RenderScale::Native, RenderScale::Double] {
                for aa in [AntiAliasing::Off, AntiAliasing::Fxaa] {
                    let size = (160, 120);
                    let original = render(scale, aa, None, size);
                    for blend in [FadeBlend::Add, FadeBlend::Subtract, FadeBlend::Black] {
                        let colour = FadeColor { blend, rgb: [71, 133, 29] };
                        let faded = render(scale, aa, Some(colour), size);
                        let mut changed = 0;
                        for y in 0..size.1 { for x in 0..size.0 {
                            let i = ((y * size.0 + x) * 4) as usize;
                            if x < 96 && y < 24 {
                                assert_eq!(&original[i..i + 4], &faded[i..i + 4], "native UI at {x},{y}");
                                continue;
                            }
                            for channel in 0..3 {
                                let expected = match blend {
                                    FadeBlend::Add => original[i + channel].saturating_add(colour.rgb[channel]),
                                    FadeBlend::Subtract => original[i + channel].saturating_sub(colour.rgb[channel]),
                                    FadeBlend::Black => 0,
                                };
                                assert!(faded[i + channel].abs_diff(expected) <= 1,
                                    "{format:?} {scale:?} {aa:?} {blend:?}, pixel {x},{y}, channel {channel}: {} expected {expected}", faded[i + channel]);
                            }
                            assert_eq!(faded[i + 3], original[i + 3], "fade keeps scene alpha");
                            changed += usize::from(original[i..i + 3] != faded[i..i + 3]);
                        } }
                        assert!(changed > 10_000, "fade changes the scene, not merely an edge");
                        // Resize while active, then return. No previous image or colour survives.
                        let resized = render(scale, aa, Some(colour), (173, 131));
                        assert_eq!(resized.len(), 173 * 131 * 4);
                        assert_eq!(render(scale, aa, Some(colour), size), faded, "resize restores exact sampling");
                    }
                    assert_eq!(render(scale, aa, None, size), original, "clearing a fade restores the untouched scene path");
                    let identity = render(scale, aa, Some(FadeColor { blend: FadeBlend::Add, rgb: [0; 3] }), size);
                    for (a, b) in original.iter().zip(identity.iter()) {
                        assert!(a.abs_diff(*b) <= 1, "zero colour is an identity in either target format");
                    }
                }
            }
        }
    }
}
