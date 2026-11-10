//! Field geometry must be clipped before the perspective divide. Compare the GPU's coverage
//! with an independently clipped camera-space polygon, including an edge crossing the camera.

use super::*;
use crate::graphics::{AntiAliasing, RenderScale};
use fmv_ff7::background::DepthMapping;

const SIZE: (u32, u32) = (320, 240);

fn mapping(near: f32) -> DepthMapping {
    let far = 32_000.0;
    DepthMapping { a: far / (far - near), b: near * far / (far - near), near, far, calibrated: true }
}

/// Sutherland-Hodgman in camera space, independent of the shader's homogeneous z arithmetic.
fn clipped(triangle: [[f32; 3]; 3], near: f32, far: f32) -> Vec<[f32; 3]> {
    let mut polygon = triangle.to_vec();
    for (plane, lower) in [(near, true), (far, false)] {
        let input = std::mem::take(&mut polygon);
        if input.is_empty() { break; }
        let inside = |p: [f32; 3]| if lower { p[2] >= plane } else { p[2] <= plane };
        let mut previous = *input.last().unwrap();
        for point in input {
            if inside(previous) != inside(point) {
                let t = (plane - previous[2]) / (point[2] - previous[2]);
                polygon.push(std::array::from_fn(|axis| previous[axis] + t * (point[axis] - previous[axis])));
            }
            if inside(point) { polygon.push(point); }
            previous = point;
        }
    }
    polygon
}

fn coverage(polygon: &[(f32, f32)], point: (f32, f32)) -> Option<bool> {
    if polygon.is_empty() { return Some(false); }
    let (mut positive, mut negative) = (false, false);
    for (&a, &b) in polygon.iter().zip(polygon.iter().cycle().skip(1)) {
        let cross = (b.0 - a.0) * (point.1 - a.1) - (b.1 - a.1) * (point.0 - a.0);
        let length = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        if length > 0.0 && cross.abs() / length < 4.0 { return None; } // exclude raster/filter boundary
        positive |= cross > 0.0;
        negative |= cross < 0.0;
    }
    Some(!(positive && negative))
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_field_near_and_far_crossings_match_camera_space_clipping_before_division() {
    let mut gpu = Offscreen::new().expect("GPU or software adapter");
    let camera = Camera::fit(&bounds(), SIZE.0 as f32, SIZE.1 as f32, MARGIN_PX);
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::default())], false);
    let normal_asset = Arc::clone(&frame.models[0].asset);
    let mut decal_asset = crate::avatar::library::AvatarAsset::new(stick(normal_asset.appearance, [255, 0, 0])).unwrap();
    decal_asset.mesh.overlay.fill(1.0);
    let decal_asset = Arc::new(decal_asset);
    let mut overlay = DrawList::new();
    overlay.fill_rect(0, 0, SIZE.0 as i32, 24, [8, 24, 80, 255]);
    overlay.draw_text(4, 8, 1, [255; 4], "NATIVE UI");
    for near in [16.0, 30.0] {
        let depth = mapping(near);
        let cases = [
            ("before-near", [[-1.0, -1.0, 8.0], [1.0, -1.0, 8.0], [0.0, 1.0, 8.0]], false),
            ("cross-near", [[-3.0, -3.0, 8.0], [6.0, -3.0, 48.0], [0.0, 6.0, 48.0]], true),
            ("cross-camera", [[-3.0, -3.0, -20.0], [6.0, -3.0, 48.0], [0.0, 6.0, 48.0]], true),
            ("inside", [[-3.0, -3.0, 40.0], [6.0, -3.0, 48.0], [0.0, 6.0, 48.0]], true),
            ("cross-far", [[-2_000.0, -2_000.0, 16_000.0], [2_000.0, -2_000.0, 16_000.0], [0.0, 4_000.0, 48_000.0]], true),
            ("beyond-far", [[-4_000.0, -4_000.0, 48_000.0], [4_000.0, -4_000.0, 48_000.0], [0.0, 4_000.0, 48_000.0]], false),
            ("near-decal", [[-3.0, -3.0, near * 0.5], [6.0, -3.0, near + 2.0], [0.0, 6.0, near + 2.0]], true),
        ];
        for (name, triangle, visible) in cases {
            frame.field_depth = Some(depth);
            frame.overlay_bias = if name == "near-decal" { 3.0 } else { 0.0 };
            frame.models[0].asset = Arc::clone(if name == "near-decal" { &decal_asset } else { &normal_asset });
            frame.models[0].positions.fill([0.0, 0.0, -100.0]); // the unrelated nose has zero area
            frame.models[0].positions[..3].copy_from_slice(&triangle);
            let polygon: Vec<_> = clipped(triangle, near, depth.far).into_iter()
                .map(|point| project_pixel(&camera, frame.zoom, SIZE.0, SIZE.1, point)).collect();
            let mut original = None;
            for (scale, aa) in [(RenderScale::Native, AntiAliasing::Off), (RenderScale::Double, AntiAliasing::Off),
                (RenderScale::Double, AntiAliasing::Fxaa)] {
                let image = gpu.render_with(SIZE.0, SIZE.1, |gpu, target, srgb| {
                    gpu.set_render_scale(scale);
                    gpu.set_anti_aliasing(aa);
                    gpu.draw_field(target, SIZE, &scene(), Some(&frame), srgb);
                    gpu.draw_overlay(target, &overlay.vertices(SIZE.0, SIZE.1, srgb));
                });
                let (mut inside, mut outside) = (0, 0);
                for y in 28..SIZE.1 { for x in 0..SIZE.0 {
                    let Some(expected) = coverage(&polygon, (x as f32 + 0.5, y as f32 + 0.5)) else { continue };
                    let got = pixel(&image, SIZE.0, x, y);
                    assert!(close(got, if expected { [255, 0, 0] } else { CLEAR_RGB }, 2),
                        "{name}, near {near}, {scale:?}/{aa:?}, ({x},{y}): {got:?}, inside {expected}");
                    if expected { inside += 1; } else { outside += 1; }
                } }
                assert_eq!(inside > 50, visible, "{name}: {inside} interior pixels");
                assert!(outside > 1_000, "{name}: checks clear area too");
                if let Some(before) = &original {
                    assert_eq!(&image[..SIZE.0 as usize * 24 * 4], before, "HUD is native and unchanged");
                } else {
                    original = Some(image[..SIZE.0 as usize * 24 * 4].to_vec());
                    if near == 16.0 { save(&format!("near-clip-{name}"), SIZE.0, SIZE.1, &image); }
                }
            }
            if name == "before-near" {
                frame.field_depth = None;
                let legacy = gpu.render_field(SIZE.0, SIZE.1, &scene(), Some(&frame), &overlay);
                assert!(legacy.chunks_exact(4).filter(|p| p[0] > 200 && p[1] < 10 && p[2] < 10).count() > 100,
                    "negative control: the previous clamped depth draws this out-of-frustum triangle");
                frame.field_depth = Some(DepthMapping::default());
                let fallback = gpu.render_field(SIZE.0, SIZE.1, &scene(), Some(&frame), &overlay);
                assert_eq!(fallback, legacy, "uncalibrated fields keep their existing geometry policy");
            }
        }
    }
}
