//! Camera-range bars are part of the scene, before filtering; native UI can cover them afterward.

use super::{AntiAliasing, RenderScale};
use crate::camera::{CameraMode, FieldCamera};
use crate::render::{Scene, CLEAR_RGB};
use crate::ui::draw::DrawList;
use crate::ui::testkit::Offscreen;
use fmv_adapter_api::Bounds;
use fmv_core::Pos;

const SIZE: (u32, u32) = (256, 192);

fn bounds(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Bounds {
    Bounds { min: Pos::new(min_x, min_y, 0.0), max: Pos::new(max_x, max_y, 0.0) }
}

fn range() -> Bounds { bounds(-64.0, -96.0, 64.0, 96.0) }

fn invalid_ranges() -> [Bounds; 4] {
    [bounds(f32::NAN, -96.0, 64.0, 96.0), bounds(-64.0, -96.0, f32::INFINITY, 96.0),
        bounds(64.0, -96.0, -64.0, 96.0), bounds(-64.0, 0.0, 64.0, 0.0)]
}

fn pixel(image: &[u8], x: u32, y: u32) -> &[u8] {
    let at = ((y * SIZE.0 + x) * 4) as usize;
    &image[at..at + 4]
}

fn under_hud(x: u32, y: u32) -> bool {
    ((4..60).contains(&x) && (8..32).contains(&y)) || ((196..252).contains(&x) && (160..184).contains(&y))
}

#[test]
fn only_a_valid_follow_camera_range_clips_the_scene() {
    let mut camera = FieldCamera::default();
    assert_eq!(camera.clip_range(Some(range())), Some(range()));
    assert_eq!(camera.clip_range(None), None);
    for invalid in invalid_ranges() {
        assert_eq!(camera.clip_range(Some(invalid)), None, "invalid range fails open: {invalid:?}");
    }
    assert_eq!(camera.toggle(), CameraMode::Whole);
    assert_eq!(camera.clip_range(Some(range())), None, "F2 whole-field view keeps the overview");
    assert_eq!(camera.toggle(), CameraMode::Follow);
    assert_eq!(camera.clip_range(Some(range())), Some(range()));
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_camera_bars_exclude_adjacent_geometry_before_2x_and_fxaa_but_keep_native_hud() {
    for format in [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Rgba8UnormSrgb] {
        let mut gpu = Offscreen::with_format(format).expect("GPU or software adapter");
        // Camera::fit at this native size and its 24px margin has scale 1. The range therefore
        // starts at output x=64 and ends at x=192, covering the full window vertically.
        let fitted = bounds(-104.0, -72.0, 104.0, 72.0);
        let inside = [Pos::new(-35.0, -30.0, 0.0), Pos::new(25.0, -15.0, 0.0), Pos::new(0.0, 45.0, 0.0)];
        let base = [inside];
        let with_outside = [
            inside,
            // Walkmesh edges are stroked 1.25 native pixels wide: a centreline at 64.64 puts
            // the nearest painted edge at 64.015, immediately outside rather than inside the range.
            [Pos::new(64.64, -80.0, 0.0), Pos::new(116.0, 0.0, 0.0), Pos::new(64.64, 80.0, 0.0)],
            [Pos::new(-64.64, -80.0, 0.0), Pos::new(-64.64, 80.0, 0.0), Pos::new(-116.0, 0.0, 0.0)],
        ];
        let mut overlay = DrawList::new();
        overlay.fill_rect(4, 8, 56, 24, [80, 20, 40, 255]);
        overlay.draw_text(8, 16, 1, [255, 255, 255, 255], "UI L");
        overlay.fill_rect(196, 160, 56, 24, [20, 60, 90, 255]);
        overlay.draw_text(200, 168, 1, [255, 255, 255, 255], "UI R");
        let mut render = |scale, aa, triangles: &[[Pos; 3]], clip: Option<Bounds>| {
            let scene = Scene { bounds: fitted, triangles, players: &[], fixture_banner: false, backdrop: None };
            gpu.render_with(SIZE.0, SIZE.1, |gpu, target, srgb| {
                gpu.set_render_scale(scale);
                gpu.set_anti_aliasing(aa);
                gpu.draw_field_clipped(target, SIZE, &scene, None, srgb, clip);
                gpu.draw_overlay(target, &overlay.vertices(SIZE.0, SIZE.1, srgb));
            })
        };
        let native = render(RenderScale::Native, AntiAliasing::Off, &base, Some(range()));
        let clear = [CLEAR_RGB[0], CLEAR_RGB[1], CLEAR_RGB[2], 255];
        assert!(pixel(&native, 8, 10) != clear && pixel(&native, 200, 162) != clear, "HUD panels are actually visible in both bars");
        assert!((16..24).any(|y| (8..56).any(|x| pixel(&native, x, y) == [255, 255, 255, 255])), "HUD text is actually drawn");
        for scale in [RenderScale::Native, RenderScale::Double] {
            for aa in [AntiAliasing::Off, AntiAliasing::Fxaa] {
                let clipped_base = render(scale, aa, &base, Some(range()));
                let clipped_outside = render(scale, aa, &with_outside, Some(range()));
                assert!(clipped_outside == clipped_base,
                    "off-range geometry must not leak into any filtered pixel: {format:?}, {scale:?}, {aa:?}");
                for y in 0..SIZE.1 { for x in 0..SIZE.0 {
                    if under_hud(x, y) {
                        assert_eq!(pixel(&clipped_outside, x, y), pixel(&native, x, y), "native HUD at {x},{y}");
                    } else if !(64..192).contains(&x) {
                        assert_eq!(pixel(&clipped_outside, x, y), clear, "clear camera bar at {x},{y}");
                    }
                } }
                let unclipped = render(scale, aa, &with_outside, None);
                assert!(unclipped != clipped_outside, "None keeps the whole-field overview");
                assert!(pixel(&unclipped, 201, 80) != clear && pixel(&unclipped, 55, 80) != clear,
                    "the test's outside triangles paint both bars without clipping");
                for invalid in invalid_ranges() {
                    assert!(render(scale, aa, &with_outside, Some(invalid)) == unclipped, "invalid range fails open: {invalid:?}");
                }
                let mut camera = FieldCamera::default();
                camera.toggle();
                assert!(render(scale, aa, &with_outside, camera.clip_range(Some(range()))) == unclipped,
                    "the whole-field camera passes no range to the renderer");
            }
        }
    }
}
