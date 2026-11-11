//! Movie presentation is an explicit base pass. These tests do not register playback,
//! advance a movie clock or treat command submission as display acknowledgement.

use super::*;
use crate::ui::draw::DrawList;
use crate::ui::testkit::Offscreen;
use fmv_media::{DecodedFrame, FrameTiming, TimeBase, VideoMetadata};

fn video_metadata(width: u32, height: u32) -> VideoMetadata {
    VideoMetadata {
        width, height, time_base: TimeBase { numerator: 1, denominator: 1000 },
        frames: vec![FrameTiming { pts: 0, duration: 41 }, FrameTiming { pts: 41, duration: 58 },
            FrameTiming { pts: 99, duration: 19 }, FrameTiming { pts: 118, duration: 57 }],
    }
}

fn decoded_frame(metadata: &VideoMetadata, ordinal: usize, rgba: [u8; 4]) -> DecodedFrame {
    DecodedFrame { ordinal, timing: metadata.frames[ordinal], rgba: rgba.repeat((metadata.width * metadata.height) as usize) }
}

fn identity(stream: u64, metadata: &VideoMetadata, ordinal: usize) -> MovieFrameId {
    MovieFrameId { stream, ordinal, timing: metadata.frames[ordinal], time_base: metadata.time_base }
}

fn policy(width: u32, height: u32, fit: MovieFit) -> DisplayPolicy {
    DisplayPolicy { aspect: DisplayAspect::new(width, height).unwrap(), fit }
}

fn pixel(image: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    image[at..at + 4].try_into().unwrap()
}

fn close(actual: [u8; 4], expected: [u8; 4]) -> bool {
    actual.into_iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1)
}

/// Interpolate a quad's texture coordinate at a viewport point. Cover may use either
/// oversized positions or cropped UVs; this checks its result without choosing that detail.
fn uv_at(quad: &[crate::render::QuadVertex; 6], point: [f32; 2]) -> Option<[f32; 2]> {
    for triangle in quad.as_chunks::<3>().0 {
        let [a, b, c] = [triangle[0], triangle[1], triangle[2]];
        let denominator = (b.pos[1] - c.pos[1]) * (a.pos[0] - c.pos[0])
            + (c.pos[0] - b.pos[0]) * (a.pos[1] - c.pos[1]);
        let wa = ((b.pos[1] - c.pos[1]) * (point[0] - c.pos[0])
            + (c.pos[0] - b.pos[0]) * (point[1] - c.pos[1])) / denominator;
        let wb = ((c.pos[1] - a.pos[1]) * (point[0] - c.pos[0])
            + (a.pos[0] - c.pos[0]) * (point[1] - c.pos[1])) / denominator;
        let wc = 1.0 - wa - wb;
        if [wa, wb, wc].into_iter().all(|weight| (-0.00001..=1.00001).contains(&weight)) {
            return Some([0, 1].map(|axis| wa * a.uv[axis] + wb * b.uv[axis] + wc * c.uv[axis]));
        }
    }
    None
}

fn near(actual: [f32; 2], expected: [f32; 2]) {
    assert!(actual.into_iter().zip(expected).all(|(a, b)| (a - b).abs() < 0.00001), "{actual:?} / {expected:?}");
}

#[test]
fn coded_pixel_aspect_and_an_explicit_display_aspect_are_different_policies() {
    // 1280x896 is 10:7 pixel geometry. Selecting 4:3 is a separate explicit decision,
    // never something inferred by the renderer from a game's name or the target size.
    let coded = movie_quad((1280, 960), policy(1280, 896, MovieFit::Contain)).unwrap();
    let four_three = movie_quad((1280, 960), policy(4, 3, MovieFit::Contain)).unwrap();
    assert_ne!(coded, four_three);
    assert!(uv_at(&coded, [0.0, 1.0]).is_none(), "coded geometry leaves 32 rows above and below");
    near(uv_at(&coded, [-1.0, 14.0 / 15.0]).unwrap(), [0.0, 0.0]);
    near(uv_at(&coded, [1.0, -14.0 / 15.0]).unwrap(), [1.0, 1.0]);
    near(uv_at(&four_three, [-1.0, 1.0]).unwrap(), [0.0, 0.0]);
    near(uv_at(&four_three, [1.0, -1.0]).unwrap(), [1.0, 1.0]);
}

#[test]
fn contain_cover_and_stretch_have_explicit_distinct_sampling() {
    let contain = movie_quad((1600, 900), policy(4, 3, MovieFit::Contain)).unwrap();
    assert!(uv_at(&contain, [-1.0, 0.0]).is_none(), "contain has side bars");
    near(uv_at(&contain, [-0.75, 1.0]).unwrap(), [0.0, 0.0]);
    near(uv_at(&contain, [0.75, -1.0]).unwrap(), [1.0, 1.0]);
    let cover = movie_quad((1600, 900), policy(4, 3, MovieFit::Cover)).unwrap();
    near(uv_at(&cover, [0.0, 1.0]).unwrap(), [0.5, 0.125]);
    near(uv_at(&cover, [0.0, -1.0]).unwrap(), [0.5, 0.875]);
    let stretch = movie_quad((1600, 900), policy(4, 3, MovieFit::Stretch)).unwrap();
    near(uv_at(&stretch, [-1.0, 1.0]).unwrap(), [0.0, 0.0]);
    near(uv_at(&stretch, [1.0, -1.0]).unwrap(), [1.0, 1.0]);
    for quad in [&contain, &cover, &stretch] { near(uv_at(quad, [0.0, 0.0]).unwrap(), [0.5, 0.5]); }
    assert!(DisplayAspect::new(0, 3).is_err());
    assert!(DisplayAspect::new(4, 0).is_err());
    assert!(movie_quad((0, 900), policy(4, 3, MovieFit::Contain)).is_err());
    assert!(movie_quad((1600, 0), policy(4, 3, MovieFit::Contain)).is_err());
}

#[test]
fn an_explicit_field_viewport_does_not_become_the_full_window() {
    let viewport = MovieViewport { x: 160, y: 0, width: 960, height: 720 };
    let quad = movie_quad_in((1280, 720), viewport, policy(10, 7, MovieFit::Contain)).unwrap();
    // Full source image:960x672, placed inside the explicitly supplied960x720 field.
    // The1280x720 target contributes its own side margins, rather than stretching it.
    near(uv_at(&quad, [-0.75, 14.0 / 15.0]).unwrap(), [0.0, 0.0]);
    near(uv_at(&quad, [0.75, -14.0 / 15.0]).unwrap(), [1.0, 1.0]);
    assert!(uv_at(&quad, [-0.8, 0.0]).is_none());
    assert!(uv_at(&quad, [0.0, 0.95]).is_none());
    for invalid in [
        MovieViewport { width: 0, ..viewport }, MovieViewport { height: 0, ..viewport },
        MovieViewport { x: 321, ..viewport }, MovieViewport { y: 1, ..viewport },
        MovieViewport { x: u32::MAX, width: u32::MAX, ..viewport },
    ] { assert!(movie_quad_in((1280, 720), invalid, policy(10, 7, MovieFit::Contain)).is_err()); }
}

#[test]
fn frame_validation_keeps_exact_pts_and_bounds_geometry_before_upload() {
    let metadata = video_metadata(4, 2);
    let frame = decoded_frame(&metadata, 2, [12, 34, 56, 255]);
    assert_eq!(validate_frame(73, &metadata, &frame, 4).unwrap().0, identity(73, &metadata, 2));
    assert!(validate_frame(73, &metadata, &frame, 3).is_err(), "a smaller GPU cannot hold the frame");
    assert!(validate_frame(73, &metadata, &frame, 0).is_err());
    let mut broken = frame;
    broken.rgba.pop();
    assert!(validate_frame(73, &metadata, &broken, 4).is_err(), "partial RGBA must not upload");
    broken.rgba.push(255);
    broken.timing.pts += 1;
    assert!(validate_frame(73, &metadata, &broken, 4).is_err(), "identity is exact metadata, not nominal frame rate");
    broken.timing = metadata.frames[2];
    broken.ordinal = metadata.frames.len();
    assert!(validate_frame(73, &metadata, &broken, 4).is_err());
    let valid = decoded_frame(&metadata, 0, [1, 2, 3, 255]);
    for changed in [
        VideoMetadata { width: 0, ..metadata.clone() },
        VideoMetadata { width: 4097, height: 1, ..metadata.clone() },
        VideoMetadata { width: u32::MAX, height: u32::MAX, ..metadata.clone() },
        VideoMetadata { time_base: TimeBase { numerator: 0, denominator: 1000 }, ..metadata.clone() },
        VideoMetadata { time_base: TimeBase { numerator: 1, denominator: 0 }, ..metadata.clone() },
        VideoMetadata { frames: Vec::new(), ..metadata.clone() },
        VideoMetadata { frames: vec![FrameTiming { pts: 0, duration: 1 }; 100_001], ..metadata.clone() },
    ] { assert!(validate_frame(73, &changed, &valid, u32::MAX).is_err()); }
    for frames in [
        vec![FrameTiming { pts: 1, duration: 1 }],
        vec![FrameTiming { pts: 0, duration: 0 }],
        vec![FrameTiming { pts: 0, duration: 1 }, FrameTiming { pts: 0, duration: 1 }],
        vec![FrameTiming { pts: 0, duration: 1 }, FrameTiming { pts: -1, duration: 1 }],
        vec![FrameTiming { pts: 0, duration: 1 }, FrameTiming { pts: i64::MAX, duration: 1 }],
    ] {
        let changed = VideoMetadata { frames, ..metadata.clone() };
        assert!(validate_frame(73, &changed, &valid, 4).is_err(), "the entire timing contract must be valid");
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_movie_texture_reuses_allocation_and_requires_explicit_stream_replacement() {
    let Some(mut offscreen) = Offscreen::new() else { return };
    let metadata = video_metadata(2, 2);
    let fit = policy(1, 1, MovieFit::Contain);
    let first = decoded_frame(&metadata, 0, [15, 45, 90, 255]);
    let second = decoded_frame(&metadata, 1, [90, 45, 15, 255]);
    let mut state = None;
    let image = offscreen.render_with(2, 2, |gpu, target, _| {
        assert_eq!(gpu.upload_movie_frame(7, &metadata, &first).unwrap(), identity(7, &metadata, 0));
        let submitted = gpu.draw_movie_base(target, (2, 2), fit).unwrap();
        assert_eq!(submitted.frame, identity(7, &metadata, 0));
        let got = gpu.movie_state();
        assert_eq!((got.allocations, got.uploads, got.geometry, got.frame), (1, 1, Some((2, 2)), Some(submitted.frame)));
        state = Some(got);
    });
    assert!(image.as_chunks::<4>().0.iter().all(|p| p == [15, 45, 90, 255].as_slice()));
    offscreen.render_with(12, 8, |gpu, target, _| {
        assert_eq!(gpu.draw_movie_base(target, (12, 8), fit).unwrap().frame, identity(7, &metadata, 0));
        assert_eq!(&gpu.movie_state(), state.as_ref().unwrap(), "viewport resize and redraw do not upload or allocate a movie frame");
        gpu.upload_movie_frame(7, &metadata, &second).unwrap();
        assert_eq!((gpu.movie_state().allocations, gpu.movie_state().uploads), (1, 2));
    });
    let same_geometry = decoded_frame(&metadata, 0, [22, 44, 66, 255]);
    offscreen.render_with(2, 2, |gpu, target, _| {
        gpu.replace_movie_stream(8);
        let got = gpu.movie_state();
        assert_eq!((got.allocations, got.uploads, got.geometry, got.frame), (1, 2, Some((2, 2)), None));
        assert!(gpu.draw_movie_base(target, (2, 2), fit).is_err(), "replacement cannot display the preceding stream's pixels");
        gpu.upload_movie_frame(8, &metadata, &same_geometry).unwrap();
        assert_eq!(gpu.draw_movie_base(target, (2, 2), fit).unwrap().frame, identity(8, &metadata, 0));
        assert_eq!((gpu.movie_state().allocations, gpu.movie_state().uploads), (1, 3), "same geometry reuses its allocation");
    });
    let wide = video_metadata(3, 1);
    let changed_geometry = decoded_frame(&wide, 0, [33, 66, 99, 255]);
    offscreen.render_with(3, 1, |gpu, target, _| {
        gpu.replace_movie_stream(9);
        gpu.upload_movie_frame(9, &wide, &changed_geometry).unwrap();
        assert_eq!((gpu.movie_state().allocations, gpu.movie_state().uploads, gpu.movie_state().geometry), (2, 4, Some((3, 1))));
        assert_eq!(gpu.draw_movie_base(target, (3, 1), policy(3, 1, MovieFit::Contain)).unwrap().frame, identity(9, &wide, 0));
        gpu.clear_movie();
        assert_eq!((gpu.movie_state().allocations, gpu.movie_state().uploads, gpu.movie_state().geometry, gpu.movie_state().frame),
            (2, 4, None, None), "clear releases the current allocation but retains lifetime counters");
        assert!(gpu.draw_movie_base(target, (3, 1), fit).is_err());
    });
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_movie_rejects_stale_wrong_stream_and_malformed_frames_without_mutation() {
    let Some(mut offscreen) = Offscreen::new() else { return };
    let metadata = video_metadata(4, 2);
    let fit = policy(2, 1, MovieFit::Contain);
    let first = decoded_frame(&metadata, 0, [10, 20, 30, 255]);
    let current = decoded_frame(&metadata, 1, [40, 50, 60, 255]);
    let next = decoded_frame(&metadata, 2, [255, 0, 255, 255]);
    // The readback drains the first upload before submitting a later decoded frame.
    offscreen.render_with(4, 2, |gpu, target, _| {
        gpu.upload_movie_frame(17, &metadata, &first).unwrap();
        gpu.draw_movie_base(target, (4, 2), fit).unwrap();
    });
    let mut before = None;
    let reference = offscreen.render_with(4, 2, |gpu, target, _| {
        gpu.upload_movie_frame(17, &metadata, &current).unwrap();
        gpu.draw_movie_base(target, (4, 2), fit).unwrap();
        before = Some(gpu.movie_state());
    });
    let image = offscreen.render_with(4, 2, |gpu, target, _| {
        assert!(gpu.upload_movie_frame(17, &metadata, &current).is_err(), "a redraw is not a second upload of the same ordinal");
        assert!(gpu.upload_movie_frame(17, &metadata, &first).is_err(), "an older ordinal cannot replace the current one");
        assert!(gpu.upload_movie_frame(18, &metadata, &next).is_err(), "stream changes need explicit replacement");
        let mut bad_timing = decoded_frame(&metadata, 2, [255, 0, 255, 255]);
        bad_timing.timing.pts += 1;
        assert!(gpu.upload_movie_frame(17, &metadata, &bad_timing).is_err());
        let mut truncated = decoded_frame(&metadata, 2, [255, 0, 255, 255]);
        truncated.rgba.truncate(truncated.rgba.len() - 1);
        assert!(gpu.upload_movie_frame(17, &metadata, &truncated).is_err());
        let mut changed_timeline = metadata.clone();
        changed_timeline.frames[3].duration += 1;
        assert!(gpu.upload_movie_frame(17, &changed_timeline, &next).is_err(), "future metadata also belongs to the bound stream");
        let other_geometry = video_metadata(3, 2);
        let other = decoded_frame(&other_geometry, 2, [255, 0, 255, 255]);
        assert!(gpu.upload_movie_frame(17, &other_geometry, &other).is_err(), "geometry cannot change inside a bound stream");
        assert_eq!(&gpu.movie_state(), before.as_ref().unwrap(), "all rejected inputs preserve allocation, upload count and identity");
        assert_eq!(gpu.draw_movie_base(target, (4, 2), fit).unwrap().frame, identity(17, &metadata, 1));
        let outside = MovieViewport { x: 1, y: 0, width: 4, height: 2 };
        assert!(gpu.draw_movie_base_in(target, (4, 2), outside, fit).is_err());
        assert_eq!(&gpu.movie_state(), before.as_ref().unwrap(), "invalid draw geometry cannot mutate identity or counters");
    });
    assert_eq!(image, reference, "rejected bytes never reach the texture, and an invalid viewport cannot clear the already queued base image");
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_movie_upload_backpressure_survives_stream_replacement_and_clear() {
    use std::sync::atomic::Ordering;
    let Some(mut offscreen) = Offscreen::new() else { return };
    let metadata = video_metadata(2, 2);
    let fit = policy(1, 1, MovieFit::Contain);
    let first = decoded_frame(&metadata, 0, [21, 42, 63, 255]);
    let next = decoded_frame(&metadata, 1, [255, 0, 255, 255]);
    let reference = offscreen.render_with(2, 2, |gpu, target, _| {
        gpu.upload_movie_frame(70, &metadata, &first).unwrap();
        gpu.draw_movie_base(target, (2, 2), fit).unwrap();
    });
    let actual = offscreen.render_with(2, 2, |gpu, target, _| {
        gpu.draw_movie_base(target, (2, 2), fit).unwrap();
        // The preceding readback completed all real upload callbacks. Hold the guard
        // explicitly so this checks the refusal deterministically on fast and slow GPUs.
        gpu.movie.upload_in_flight.store(true, Ordering::Release);
        let before = gpu.movie_state();
        assert_eq!(gpu.upload_movie_frame(70, &metadata, &next), Err(MovieError::UploadBusy));
        assert_eq!(gpu.movie_state(), before);
        gpu.replace_movie_stream(71);
        let replaced = gpu.movie_state();
        assert_eq!(gpu.upload_movie_frame(71, &metadata, &first), Err(MovieError::UploadBusy));
        assert_eq!(gpu.movie_state(), replaced, "replacement cannot bypass outstanding staging work");
        assert!(gpu.movie.upload_in_flight.load(Ordering::Acquire));
        gpu.clear_movie();
        let cleared = gpu.movie_state();
        assert_eq!(gpu.upload_movie_frame(72, &metadata, &first), Err(MovieError::UploadBusy));
        assert_eq!(gpu.movie_state(), cleared, "clear cannot bypass outstanding staging work");
        assert!(gpu.movie.upload_in_flight.load(Ordering::Acquire));
        gpu.movie.upload_in_flight.store(false, Ordering::Release);
    });
    assert_eq!(actual, reference, "busy uploads cannot replace the previously submitted image");
    offscreen.render_with(2, 2, |gpu, target, _| {
        gpu.upload_movie_frame(72, &metadata, &first).unwrap();
        assert_eq!((gpu.movie_state().allocations, gpu.movie_state().uploads), (2, 2),
            "after progress, a cleared texture is allocated and uploaded once");
        assert_eq!(gpu.draw_movie_base(target, (2, 2), fit).unwrap().frame, identity(72, &metadata, 0));
    });
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_movie_preserves_row_orientation_color_and_opaque_output_at_one_pixel_per_texel() {
    for format in [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Rgba8UnormSrgb] {
        let Some(mut offscreen) = Offscreen::with_format(format) else { return };
        let metadata = video_metadata(2, 2);
        let source = [[255, 0, 0, 0], [0, 255, 0, 127], [0, 0, 255, 128], [64, 128, 192, 255]];
        let frame = DecodedFrame { ordinal: 0, timing: metadata.frames[0], rgba: source.concat() };
        let image = offscreen.render_with(2, 2, |gpu, target, _| {
            gpu.upload_movie_frame(31, &metadata, &frame).unwrap();
            assert_eq!(gpu.draw_movie_base(target, (2, 2), policy(1, 1, MovieFit::Contain)).unwrap().frame, identity(31, &metadata, 0));
        });
        for y in 0..2 { for x in 0..2 {
            let [r, g, b, _] = source[(y * 2 + x) as usize];
            assert!(close(pixel(&image, 2, x, y), [r, g, b, 255]), "{format:?}, ({x},{y}): {:?}", pixel(&image, 2, x, y));
        } }
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_movie_draws_black_bars_below_explicit_overlays_and_keeps_field_backdrop_separate() {
    let Some(mut offscreen) = Offscreen::new() else { return };
    let metadata = video_metadata(2, 2);
    let movie = decoded_frame(&metadata, 0, [17, 33, 65, 255]);
    let field = crate::backdrop::Backdrop::new(1, 1, vec![0, 255, 0, 255], [-1.0, -1.0], [1.0, 1.0]).unwrap();
    let mut overlay = DrawList::new();
    overlay.fill_rect(0, 0, 16, 2, [255, 0, 255, 255]);
    let image = offscreen.render_with(16, 8, |gpu, target, srgb| {
        assert!(gpu.ensure_backdrop(&field));
        gpu.upload_movie_frame(53, &metadata, &movie).unwrap();
        gpu.draw_movie_base(target, (16, 8), policy(1, 1, MovieFit::Contain)).unwrap();
        // An explicit non-clearing field overlay covers x6..10,y0..5. UI subsequently
        // covers its first two rows. All other movie pixels and black bars survive.
        let color = crate::render::color([0, 255, 255], srgb);
        let corner = |pos| crate::render::Vertex { pos, color };
        let shapes = [corner([-0.25, 1.0]), corner([-0.25, -0.25]), corner([0.25, -0.25]),
            corner([-0.25, 1.0]), corner([0.25, -0.25]), corner([0.25, 1.0])];
        gpu.draw_shapes(target, &shapes);
        gpu.draw_overlay(target, &overlay.vertices(16, 8, srgb));
        assert_eq!(gpu.backdrop.as_ref().map(|backdrop| backdrop.id), Some(field.id), "movie uploads do not evict the field texture");
        gpu.clear_movie();
        assert_eq!(gpu.backdrop.as_ref().map(|backdrop| backdrop.id), Some(field.id), "movie cleanup is independent too");
    });
    for y in 0..8 { for x in 0..16 {
        let expected = if y < 2 { [255, 0, 255, 255] }
            else if y < 5 && (6..10).contains(&x) { [0, 255, 255, 255] }
            else if !(4..12).contains(&x) { [0, 0, 0, 255] } else { [17, 33, 65, 255] };
        assert!(close(pixel(&image, 16, x, y), expected), "({x},{y}): {:?} / {expected:?}", pixel(&image, 16, x, y));
    } }
}
