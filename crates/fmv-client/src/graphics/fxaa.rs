//! A compact FXAA pass over the scene, before UI. The diagonal luminance gradient estimates the
//! edge direction; two bounded line filters are compared against the local luminance range to
//! avoid introducing an edge halo. No history, geometry, game clock or game data is changed.
//!
//! Algorithm background: Timothy Lottes, NVIDIA, *FXAA* (2009),
//! https://developer.download.nvidia.com/assets/gamedev/files/sdk/11/FXAA_WhitePaper.pdf .
//! This is an independently written WGSL implementation of the spatial filter, not a claim of
//! temporal reconstruction. Texture detail can soften; the unfiltered path remains selectable.

const SHADER: &str = r#"
const SRGB_TARGET: bool = false;
@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;

@vertex
fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(corners[i], 0.0, 1.0);
}
fn sample_at(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(scene, scene_sampler, uv, 0.0).rgb;
}
fn luma(rgb: vec3<f32>) -> f32 {
    // sRGB targets decode their sampled texture to linear. Detect edges in approximately
    // perceptual brightness in either format; filtering and writing still use the target space.
    let brightness = dot(rgb, vec3(0.299, 0.587, 0.114));
    if SRGB_TARGET { return pow(max(brightness, 0.0), 1.0 / 2.2); }
    return brightness;
}
@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(scene));
    let uv = position.xy * texel;
    let centre = sample_at(uv);
    let c = luma(centre);
    let nw = luma(sample_at(uv + vec2(-1.0, -1.0) * texel));
    let ne = luma(sample_at(uv + vec2( 1.0, -1.0) * texel));
    let sw = luma(sample_at(uv + vec2(-1.0,  1.0) * texel));
    let se = luma(sample_at(uv + vec2( 1.0,  1.0) * texel));
    let low = min(c, min(min(nw, ne), min(sw, se)));
    let high = max(c, max(max(nw, ne), max(sw, se)));
    if high - low < max(1.0 / 32.0, high / 8.0) { return vec4(centre, 1.0); }
    let direction = vec2(sw + se - nw - ne, nw + sw - ne - se);
    let damping = max((nw + ne + sw + se) / 32.0, 1.0 / 128.0);
    let step = clamp(direction / (min(abs(direction.x), abs(direction.y)) + damping), vec2(-8.0), vec2(8.0)) * texel;
    let narrow = (sample_at(uv - step / 6.0) + sample_at(uv + step / 6.0)) * 0.5;
    let wide = narrow * 0.5 + (sample_at(uv - step * 0.5) + sample_at(uv + step * 0.5)) * 0.25;
    let filtered = luma(wide);
    return vec4(select(wide, narrow, filtered < low || filtered > high), 1.0);
}
"#;

struct Target {
    size: (u32, u32),
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

pub struct Fxaa {
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    target: Option<Target>,
}

impl Fxaa {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let source = SHADER.replace("const SRGB_TARGET: bool = false;", &format!("const SRGB_TARGET: bool = {};", format.is_srgb()));
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fmv-fxaa"), source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fmv-fxaa"), layout: None,
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vertex"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &shader, entry_point: Some("fragment"), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: Default::default(), multiview_mask: None, cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fmv-fxaa-clamped"),
            mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Self { pipeline, sampler, format, target: None }
    }

    /// The scene goes here first. Replace it on resize, never allocate per frame.
    pub fn target(&mut self, device: &wgpu::Device, size: (u32, u32)) -> wgpu::TextureView {
        if self.target.as_ref().is_none_or(|target| target.size != size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fmv-scene-before-fxaa"),
                size: wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("fmv-fxaa-scene"), layout: &self.pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            self.target = Some(Target { size, _texture: texture, view, bind });
        }
        self.target.as_ref().expect("target was created").view.clone()
    }

    pub fn resolve(&self, device: &wgpu::Device, queue: &wgpu::Queue, output: &wgpu::TextureView) {
        let Some(target) = &self.target else { return };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fmv-fxaa-resolve") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fmv-fxaa-resolve"),
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
    use crate::avatar::AvatarFrame;
    use crate::battle::scene::BattleFrame;
    use crate::graphics::AntiAliasing;
    use crate::render::{Camera, Scene, Vertex};
    use crate::ui::draw::DrawList;
    use crate::ui::testkit::Offscreen;
    use fmv_adapter_api::Bounds;
    use fmv_core::Pos;

    #[test]
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    #[allow(clippy::chunks_exact_to_as_chunks)] // as_chunks requires Rust 1.88; keep the workspace's older MSRV.
    fn offscreen_fxaa_smooths_field_and_battle_edges_keeps_ui_exact_and_can_be_disabled() {
        // The window can negotiate either a plain or an sRGB surface. Both must preserve solid
        // colours, orientation and UI while producing intermediate coverage along a jagged edge.
        for format in [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Rgba8UnormSrgb] {
            let mut gpu = Offscreen::with_format(format).expect("GPU or software adapter");
            let bounds = Bounds { min: Pos::new(-1.0, -1.0, 0.0), max: Pos::new(1.0, 1.0, 0.0) };
            let triangles = [[Pos::new(-0.9, -0.8, 0.0), Pos::new(0.8, -0.3, 0.0), Pos::new(-0.2, 0.9, 0.0)]];
            let scene = Scene { bounds, triangles: &triangles, players: &[], fixture_banner: false, backdrop: None };
            let battle = BattleFrame {
                clear_rgb: [0, 0, 0],
                stage: vec![
                    Vertex { pos: [-0.9, -0.8], color: [1.0; 4] },
                    Vertex { pos: [0.8, -0.3], color: [1.0; 4] },
                    Vertex { pos: [-0.2, 0.9], color: [1.0; 4] },
                ],
                location: None,
                models: AvatarFrame { models: vec![], tints: vec![], identities: vec![], zoom: 1.0, overlay_bias: 0.0, ground: vec![], scenery: None },
                tints: vec![], identities: vec![], camera: Camera::fit(&bounds, 256.0, 192.0, 0.0),
            };
            let mut overlay = DrawList::new();
            overlay.fill_rect(0, 0, 128, 24, [8, 24, 80, 255]);
            overlay.draw_text(4, 8, 1, [255, 255, 255, 255], "SHARP UI");
            let mut render = |mode, battle_mode, size: (u32, u32)| gpu.render_with(size.0, size.1, |gpu, target, srgb| {
                gpu.set_anti_aliasing(mode);
                match battle_mode {
                    0 => gpu.draw_field(target, size, &scene, None, srgb),
                    1 => gpu.draw_battle(target, size, &battle, srgb),
                    _ => gpu.draw_flat(target, size, &battle.stage, srgb),
                }
                gpu.draw_overlay(target, &overlay.vertices(size.0, size.1, srgb));
            });
            for battle_mode in 0..3 {
                let size = (256, 192);
                let original = render(AntiAliasing::Off, battle_mode, size);
                let smooth = render(AntiAliasing::Fxaa, battle_mode, size);
                let changed = original.chunks_exact(4).zip(smooth.chunks_exact(4)).filter(|(a, b)| a != b).count();
                assert!(changed > 100 && changed < 4_000, "only the edges change: {changed} pixels, {format:?}, battle={battle_mode}");
                // The opaque panel, its boundaries and every font pixel stay byte-for-byte exact.
                for y in 0..24 { for x in 0..128 {
                    let i = (y * size.0 as usize + x) * 4;
                    assert_eq!(&original[i..i+4], &smooth[i..i+4], "UI at {x},{y}");
                } }
                // Resize with the filter enabled, then restore. No previous frame or texture size leaks.
                let resized = render(AntiAliasing::Fxaa, battle_mode, (319, 241));
                assert_eq!(resized.len(), 319 * 241 * 4);
                assert_eq!(render(AntiAliasing::Fxaa, battle_mode, size), smooth);
                assert_eq!(render(AntiAliasing::Off, battle_mode, size), original, "off restores the exact original path");
                if battle_mode == 1 {
                    let partial = |image: &[u8]| image.chunks_exact(4).skip(256 * 24)
                        .filter(|p| p[0] > 0 && p[0] < 255 && p[0] == p[1] && p[1] == p[2]).count();
                    assert_eq!(partial(&original), 0, "unfiltered triangle is black or white");
                    assert!(partial(&smooth) > 100, "filter provides intermediate edge coverage");
                    let pixel = |image: &[u8], x: usize, y: usize| { let i = (y * 256 + x) * 4; image[i..i+4].to_vec() };
                    assert_eq!(pixel(&smooth, 0, 191), [0, 0, 0, 255], "flat background stays black");
                    assert_eq!(pixel(&smooth, 110, 110), [255, 255, 255, 255], "solid interior stays white");
                }
            }
        }
    }
}
