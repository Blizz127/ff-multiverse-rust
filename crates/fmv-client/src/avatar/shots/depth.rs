//! Offscreen checks of the depth-tested scenery, through the real renderer (`Gpu::draw_field`).
//!
//! They need a GPU or a software Vulkan/GL driver, so they only run on request:
//! `cargo test -p fmv-client -- --ignored offscreen`.
//!
//! * The *synthetic* tests (no game data) put hand-made tiles in front of and behind a stick figure
//!   and check pixels: a tile hides a model only where it is nearer, to the unit; layer 4 and
//!   the ids below the near plane are in front of everything; transparent texels hide nothing; blended
//!   tiles follow the paint order and the depth test even when they and the tiles they must not
//!   cover are all beyond the models (the case a depth mapping fitted to the models would get wrong);
//!   every blend mode is the game's; the marks on the ground are hidden exactly when the character
//!   would be; the tile picture is the flat picture at a scale that lines the pixels up; a silhouette
//!   has no fringe; the scenery is uploaded once per field.
//! * The *real-data* tests draw the owner's own `md1stin` and `md1_1` and write PNGs
//!   (`70-depth-*.png` in `$FMV_SHOT_DIR`, default `~/.cache/claude-tmp/fmv-client-shots`). Cloud
//!   is put at the walkmesh spots of the software composites of `fmv-ff7`
//!   (`examples/depth_composite.rs`, PNGs in `~/.cache/claude-tmp/ffmv-ff7-dump/depth_md1stin_*`):
//!   where the flat front layer used to hide him although he is nearer than the wall piece (2, 544);
//!   where he is behind that wall piece, hidden and partly hidden; behind the posts and the train.
//!   The hidden fraction of his silhouette is measured on the GPU render and printed next to the
//!   software composite's.

use super::real;
use super::*;
use crate::avatar::frame::SkinnedModel;
use crate::avatar::scenery::{tests as scenery_tests, Scenery, DEPTH_FAR};
use crate::avatar::stage::Stage;
use fmv_ff7::background::depth::DepthTile;
use fmv_ff7::background::{BlendMode, ScreenRect};

// ---------------------------------------------------------------------------------------------
// Synthetic scenes
// ---------------------------------------------------------------------------------------------

/// The picture of the synthetic scenes: field-screen -100..100 on both axes, the window rectangle
/// of [`bounds`].
fn canvas() -> ScreenRect {
    ScreenRect { min_x: -100, min_y: -100, max_x: 100, max_y: 100 }
}

/// A tile of one opaque colour at camera depth `depth` (any layer: the depth is what counts).
fn solid(layer: u8, at: (i32, i32), size: i32, depth: f32, rgb: [u8; 3], blend: Option<BlendMode>) -> DepthTile {
    let mut t = scenery_tests::tile(layer, 0, at, size, rgb, blend);
    t.depth = depth;
    t
}

fn scenery_of(tiles: &[DepthTile], canvas: ScreenRect) -> Arc<Scenery> {
    let (scenery, note) = Scenery::choose(tiles, canvas);
    Arc::new(scenery.expect(&note))
}

/// Field-screen position (x right, y down) of a camera-space point of the synthetic camera.
fn field_screen(p: [f32; 3]) -> (f32, f32) {
    (p[0] * 400.0 / p[2], p[1] * 400.0 / p[2])
}

/// The window pixel of a field-screen position in the synthetic window.
fn field_to_pixel(sx: f32, sy: f32) -> (u32, u32) {
    let c = fitted().project(sx, -sy);
    ((W as f32 / 2.0 + c[0]).round() as u32, (H as f32 / 2.0 - c[1]).round() as u32)
}

/// The red figure's body: where it is on the screen (field-screen centre of its body triangle) and
/// its camera depth range.
struct Figure {
    centre: (f32, f32),
    pixel: (u32, u32),
    near: f32,
    far: f32,
}

fn figure(frame: &AvatarFrame) -> Figure {
    let body = body(frame, 0);
    let pts = body.map(field_screen);
    let centre = ((pts[0].0 + pts[1].0 + pts[2].0) / 3.0, (pts[0].1 + pts[1].1 + pts[2].1) / 3.0);
    let (near, far) = frame.models[0].positions.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[2]), b.max(p[2])));
    Figure { centre, pixel: field_to_pixel(centre.0, centre.1), near, far }
}

/// A pixel of the 32 x 32 tile around the figure's body where the figure is not: the clear colour in
/// the render without scenery.
fn clear_probe(gpu: &mut Offscreen, frame: &AvatarFrame, f: &Figure) -> (u32, u32) {
    let models = frame.models.iter().map(|m| SkinnedModel { asset: m.asset.clone(), positions: m.positions.clone() }).collect();
    let bare = AvatarFrame { models, tints: Vec::new(), identities: Vec::new(), zoom: frame.zoom, overlay_bias: frame.overlay_bias, ground: Vec::new(), scenery: None };
    let img = draw(gpu, &bare);
    for (dx, dy) in [(13.0, 13.0), (-13.0, 13.0), (13.0, -13.0), (-13.0, -13.0), (14.0, 0.0), (-14.0, 0.0)] {
        let (x, y) = field_to_pixel(f.centre.0 + dx, f.centre.1 + dy);
        if close(pixel(&img, W, x, y), CLEAR_RGB, 2) {
            return (x, y);
        }
    }
    panic!("no clear pixel around the figure's body");
}

/// A 32 x 32 tile centred on a field-screen point.
fn tile_over(centre: (f32, f32), layer: u8, depth: f32, rgb: [u8; 3], blend: Option<BlendMode>) -> DepthTile {
    solid(layer, (centre.0.round() as i32 - 16, centre.1.round() as i32 - 16), 32, depth, rgb, blend)
}

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLACK: [u8; 3] = [0, 0, 0];

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_a_tile_hides_a_model_only_where_it_is_nearer_to_the_unit() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let f = figure(&frame);
    eprintln!("the figure's body is at pixel {:?}, depth {:.1}..{:.1}", f.pixel, f.near, f.far);
    let pixel_at = |img: &[u8]| pixel(img, W, f.pixel.0, f.pixel.1);
    // Without scenery the body shows (the flat picture is the scene: none here, the clear colour).
    assert!(close(pixel_at(&draw(&mut gpu, &frame)), RED, 2));

    // A tile a unit nearer than the nearest vertex of the figure hides the body; one a unit farther
    // than the farthest vertex does not; the pixels of the tile where nothing is drawn show it.
    let probe = clear_probe(&mut gpu, &frame, &f);
    for (depth, hides) in [(f.near - 1.0, true), (f.far + 1.0, false), (f.near - 400.0, true), (f.far + 4000.0, false)] {
        frame.scenery = Some(scenery_of(&[tile_over(f.centre, 2, depth, GREEN, None)], canvas()));
        let img = draw(&mut gpu, &frame);
        let (got, want) = (pixel_at(&img), if hides { GREEN } else { RED });
        assert!(close(got, want, 2), "tile at depth {depth:.1} (figure {:.1}..{:.1}): body pixel {got:?}, want {want:?}", f.near, f.far);
        assert!(close(pixel(&img, W, probe.0, probe.1), GREEN, 2), "the tile is drawn where the figure is not: {:?}", pixel(&img, W, probe.0, probe.1));
        // Beyond the tile: the base, black inside the picture's rectangle.
        assert!(close(pixel(&img, W, W / 2 - 60, H / 2 + 80), BLACK, 2));
    }
    // The picture is black inside its rectangle and the clear colour outside it.
    let img = draw(&mut gpu, &frame);
    assert!(close(pixel(&img, W, 20, 20), CLEAR_RGB, 2), "outside the picture: {:?}", pixel(&img, W, 20, 20));
    save("70x-depth-synthetic-nearer-and-farther", W, H, &img);
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_layer_four_and_the_ids_below_the_near_plane_are_in_front_of_everything_and_holes_hide_nothing() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let f = figure(&frame);
    let at = |img: &[u8]| pixel(img, W, f.pixel.0, f.pixel.1);
    // Depth 0 (layer 4) and depth 48 (layer-2 id 12): both below the near plane of 50, both in front.
    for depth in [0.0f32, 4.0, 48.0] {
        frame.scenery = Some(scenery_of(&[tile_over(f.centre, 4, depth, GREEN, None)], canvas()));
        assert!(close(at(&draw(&mut gpu, &frame)), GREEN, 2), "depth {depth}");
    }
    // Layer 1 and 3 (16 380 and 16 384) are behind the figure: a tile of layer 2 elsewhere keeps the
    // scenery in use.
    let elsewhere = solid(2, (-96, -96), 16, 100.0, [9, 9, 9], None);
    for depth in [16_380.0f32, 16_384.0, DEPTH_FAR + 5000.0] {
        frame.scenery = Some(scenery_of(&[tile_over(f.centre, 1, depth, GREEN, None), elsewhere.clone()], canvas()));
        assert!(close(at(&draw(&mut gpu, &frame)), RED, 2), "depth {depth}");
    }
    // Transparent texels hide nothing: a tile in front with a hole where the body is shows the body
    // through the hole (the hole's texels carry a loud colour that must not leak) and itself around it.
    let mut holed = tile_over(f.centre, 2, f.near - 100.0, GREEN, None);
    let side = 32usize;
    for y in 8..24 {
        for x in 8..24 {
            holed.rgba[(y * side + x) * 4..(y * side + x) * 4 + 4].copy_from_slice(&[255, 0, 255, 0]);
        }
    }
    frame.scenery = Some(scenery_of(&[holed], canvas()));
    let img = draw(&mut gpu, &frame);
    assert!(close(at(&img), RED, 2), "through the hole: {:?}", at(&img));
    let probe = clear_probe(&mut gpu, &frame, &f);
    assert!(close(pixel(&img, W, probe.0, probe.1), GREEN, 2), "around it");
    save("70x-depth-synthetic-hole", W, H, &img);
}

/// A figure far off the screen, so that the frame has a model (and so draws the scenery) without a
/// model in the picture.
fn off_screen() -> AvatarFrame {
    built(&[facing_camera(1, "ff7/cloud", Pos::new(30_000.0, 0.0, 0.0))], false)
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_blended_tiles_follow_the_paint_order_and_the_depth_test_beyond_the_models_too() {
    let Some(mut gpu) = Offscreen::new() else { return };
    // The failure a depth mapping fitted to the models would have: a blended tile (drawn after the
    // opaque ones) BEHIND a nearer opaque tile, both far beyond any model. They must not be
    // confused: the blended tile must not show over the opaque one.
    let mut frame = off_screen();
    let red_tile_depth = 15_000.0; // layer 2 id 3750
    let blue_tile_depth = 16_000.0; // id 4000, behind it
    let tiles = [
        // paint order: the farther first
        solid(2, (-64, -16), 32, blue_tile_depth, [0, 0, 100], Some(BlendMode::Additive)),
        solid(2, (-64, -16), 32, red_tile_depth, [200, 0, 0], None),
        // the blended tile alone, and the opaque one alone, for reference
        solid(2, (16, -16), 32, blue_tile_depth, [0, 0, 100], Some(BlendMode::Additive)),
        solid(2, (-16, 32), 32, red_tile_depth, [200, 0, 0], None),
    ];
    frame.scenery = Some(scenery_of(&tiles, canvas()));
    let img = draw(&mut gpu, &frame);
    let at = |sx: f32, sy: f32| {
        let (x, y) = field_to_pixel(sx, sy);
        pixel(&img, W, x, y)
    };
    // Opaque over blended: the blended tile behind it does not show.
    assert!(close(at(-48.0, 0.0), [200, 0, 0], 2), "behind a nearer opaque tile: {:?}", at(-48.0, 0.0));
    // The blended tile alone adds its colour to the black base; the opaque one alone is itself.
    assert!(close(at(32.0, 0.0), [0, 0, 100], 2), "alone: {:?}", at(32.0, 0.0));
    assert!(close(at(0.0, 48.0), [200, 0, 0], 2));

    // Now a blended tile NEARER than the opaque one: it blends over it (additive: 200 + 55 red).
    let near = [solid(2, (-64, -16), 32, red_tile_depth, [200, 0, 0], None), solid(2, (-64, -16), 32, red_tile_depth - 400.0, [55, 0, 0], Some(BlendMode::Additive))];
    frame.scenery = Some(scenery_of(&near, canvas()));
    let img = draw(&mut gpu, &frame);
    let (x, y) = field_to_pixel(-48.0, 0.0);
    assert!(close(pixel(&img, W, x, y), [255, 0, 0], 2), "a nearer blended tile blends: {:?}", pixel(&img, W, x, y));
    save("70x-depth-synthetic-blended-order", W, H, &img);
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_every_blend_mode_is_the_games_and_blended_tiles_pass_a_model_only_when_nearer() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut frame = off_screen();
    // One opaque backdrop colour, then each mode over it with a front colour, side by side.
    let (back, front) = ([200u8, 100, 60], [90u8, 140, 30]);
    let modes = [BlendMode::Average, BlendMode::Additive, BlendMode::Subtractive, BlendMode::AddQuarter];
    let mut tiles = Vec::new();
    for (i, mode) in modes.iter().enumerate() {
        let x = -96 + 48 * i as i32;
        tiles.push(solid(1, (x, -16), 32, 16_380.0, back, None));
        tiles.push(solid(2, (x, -16), 32, 1_000.0, front, Some(*mode)));
    }
    frame.scenery = Some(scenery_of(&tiles, canvas()));
    let img = draw(&mut gpu, &frame);
    for (i, mode) in modes.iter().enumerate() {
        let (x, y) = field_to_pixel(-80.0 + 48.0 * i as f32, 0.0);
        let got = pixel(&img, W, x, y);
        let want = [0, 1, 2].map(|k| mode.apply(back[k], front[k]));
        assert!(close(got, want, 2), "{mode:?}: {got:?}, the game's arithmetic gives {want:?}");
    }
    save("70x-depth-synthetic-blend-modes", W, H, &img);

    // A blended tile and a model: the figure is nearer than the tile (the tile is farther: no
    // blend over it) or the tile is nearer (it blends over the figure).
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let f = figure(&frame);
    for (depth, over_model) in [(f.far + 50.0, false), (f.near - 50.0, true)] {
        frame.scenery = Some(scenery_of(&[tile_over(f.centre, 2, depth, [0, 90, 0], Some(BlendMode::Additive))], canvas()));
        let img = draw(&mut gpu, &frame);
        let got = pixel(&img, W, f.pixel.0, f.pixel.1);
        let want = if over_model { [255, 90, 0] } else { RED };
        assert!(close(got, want, 3), "blended tile at {depth:.0} (figure {:.0}..{:.0}): {got:?}, want {want:?}", f.near, f.far);
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_the_marks_on_the_ground_are_hidden_exactly_when_the_character_would_be() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], true);
    // The middle of the tint ring's band at its rightmost point, in field-screen units (view y is up).
    let ring = &frame.ground[1];
    assert_eq!(ring.rgb, [10, 200, 30]);
    let feet = ring.depth;
    let at = |outline: &[[f32; 2]]| outline.iter().copied().max_by(|a, b| a[0].total_cmp(&b[0])).unwrap();
    let (outer, inner) = (at(&ring.outer), at(&ring.inner));
    let mid = ((outer[0] + inner[0]) / 2.0, -(outer[1] + inner[1]) / 2.0);
    let (px, py) = field_to_pixel(mid.0, mid.1);
    eprintln!("the ring's band is at pixel ({px}, {py}); the feet are at depth {feet:.1}");
    let backdrop = [200u8, 0, 200];
    // A tile nearer than the feet covers the ring: the ring is not drawn over it. A farther one does
    // not: the ring (alpha 0.85) shows over it.
    for (depth, hidden) in [(feet - 30.0, true), (feet + 30.0, false)] {
        frame.scenery = Some(scenery_of(&[tile_over(mid, 2, depth, backdrop, None)], canvas()));
        let img = draw(&mut gpu, &frame);
        let got = pixel(&img, W, px, py);
        if hidden {
            assert!(close(got, backdrop, 2), "a nearer tile hides the ring: {got:?}");
        } else {
            assert!(got[1] > 120 && got[0] < 120, "the ring shows over a farther tile: {got:?}");
        }
        save(&format!("70x-depth-synthetic-ring-{}", if hidden { "hidden" } else { "shown" }), W, H, &img);
    }
    // And the model is still drawn over its own ring and shadow.
    frame.scenery = Some(scenery_of(&[solid(2, (-96, -96), 16, 100.0, [9, 9, 9], None)], canvas()));
    let img = draw(&mut gpu, &frame);
    let f = figure(&frame);
    assert!(close(pixel(&img, W, f.pixel.0, f.pixel.1), RED, 2));
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_the_walkmesh_and_the_markers_are_over_the_tiles_and_under_the_models() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let f = figure(&frame);
    // A floor triangle (a see-through wash on a picture, opaque here) around the figure's body and
    // a tile in front of the figure everywhere else.
    frame.scenery = Some(scenery_of(&[tile_over((f.centre.0 + 40.0, f.centre.1), 2, 100.0, GREEN, None)], canvas()));
    let c = (f.centre.0, -f.centre.1);
    let floor = [[Pos::new(c.0 - 30.0, c.1 - 30.0, 0.0), Pos::new(c.0 + 80.0, c.1 - 30.0, 0.0), Pos::new(c.0 + 30.0, c.1 + 60.0, 0.0)]];
    let scene = Scene { triangles: &floor, ..scene() };
    let img = gpu.render_field(W, H, &scene, Some(&frame), &DrawList::new());
    // The floor (52, 70, 96) is drawn over the tile where the figure is not (a point of the triangle
    // inside the tile's square, away from the body)...
    let (x, y) = field_to_pixel(f.centre.0 + 30.0, f.centre.1 - 10.0);
    let got = pixel(&img, W, x, y);
    assert!(close(got, crate::render::FLOOR_RGB, 3) || close(got, crate::render::EDGE_RGB, 40), "the walkmesh over a tile: {got:?}");
    // ...and the model over the floor.
    assert!(close(pixel(&img, W, f.pixel.0, f.pixel.1), RED, 2), "the model over the walkmesh: {:?}", pixel(&img, W, f.pixel.0, f.pixel.1));
    save("70x-depth-synthetic-shapes", W, H, &img);
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_the_scenery_is_uploaded_once_per_field_and_replaced_by_the_next() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let f = figure(&frame);
    let add = Some(BlendMode::Additive);
    let tiles = [tile_over(f.centre, 2, 100.0, GREEN, None), solid(2, (40, 40), 16, 100.0, [9, 9, 9], add), solid(2, (60, 40), 16, 100.0, [9, 9, 9], Some(BlendMode::Average))];
    let first = scenery_of(&tiles, canvas());
    frame.scenery = Some(first.clone());
    assert_eq!(gpu.scenery_state(), (0, None, 0), "nothing before the first frame");
    let mut previous = None;
    for i in 0..4 {
        let img = draw(&mut gpu, &frame);
        // one upload; the draw calls of a frame: the opaque tiles' one and one per blended run
        assert_eq!(gpu.scenery_state(), (1, Some(first.id), 3), "frame {i}");
        assert!(previous.get_or_insert_with(|| img.clone()) == &img, "frame {i} differs");
    }
    // Another field's scenery is uploaded once, and replaces the first.
    let second = scenery_of(&tiles[..1], canvas());
    frame.scenery = Some(second.clone());
    for _ in 0..3 {
        draw(&mut gpu, &frame);
        assert_eq!(gpu.scenery_state(), (2, Some(second.id), 1));
    }
    // Without scenery (or without models) the flat path is used and nothing is uploaded.
    frame.scenery = None;
    draw(&mut gpu, &frame);
    assert_eq!(gpu.scenery_state().0, 2);
}

/// A picture of 4 x 3 tiles of 16 texels whose every texel is its own colour, as tiles (layer 2, so
/// that the scenery is used) and as the flat picture of the same pixels.
fn gradient_picture() -> (Vec<DepthTile>, Backdrop, ScreenRect) {
    let canvas = ScreenRect { min_x: -32, min_y: -24, max_x: 32, max_y: 24 };
    let mut flat = vec![0u8; 64 * 48 * 4];
    let mut tiles = Vec::new();
    for ty in 0..3i32 {
        for tx in 0..4i32 {
            let mut t = solid(2, (-32 + 16 * tx, -24 + 16 * ty), 16, 800.0 - 10.0 * (tx + 4 * ty) as f32, [0; 3], None);
            for y in 0..16usize {
                for x in 0..16usize {
                    // smooth across the whole picture (a few levels a texel), so that only a seam
                    // artefact, not the picture, can make the two paths differ
                    let (fx, fy) = (tx as usize * 16 + x, ty as usize * 16 + y);
                    let rgba = [(fx * 4) as u8, (fy * 5) as u8, ((fx + fy) * 2 + 30) as u8, 255];
                    t.rgba[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4].copy_from_slice(&rgba);
                    flat[(fy * 64 + fx) * 4..(fy * 64 + fx) * 4 + 4].copy_from_slice(&rgba);
                }
            }
            tiles.push(t);
        }
    }
    let picture = Backdrop::new(64, 48, flat, [-32.0, -24.0], [32.0, 24.0]).unwrap();
    (tiles, picture, canvas)
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_the_tile_picture_is_the_flat_picture_when_pixels_line_up_and_has_no_seams_when_they_do_not() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let (tiles, picture, canvas) = gradient_picture();
    let bounds = Bounds { min: Pos::new(-32.0, -24.0, 0.0), max: Pos::new(32.0, 24.0, 0.0) };
    let mut frame = off_screen();
    frame.scenery = Some(scenery_of(&tiles, canvas));
    // 112 x 96 with 24 px margins fits the 64 x 48 picture at exactly one pixel per texel, centred on
    // a pixel boundary: every pixel centre is a texel centre, so both paths must give the same bytes.
    for (w, h, exact) in [(112u32, 96u32, true), (1100, 680, false), (333, 211, false)] {
        let scene = Scene { bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: Some(&picture) };
        let tiled = gpu.render_field(w, h, &scene, Some(&frame), &DrawList::new());
        let mut flat_frame = off_screen();
        flat_frame.scenery = None;
        let flat = gpu.render_field(w, h, &scene, Some(&flat_frame), &DrawList::new());
        let mut worst = 0u8;
        let mut differing = 0usize;
        for (a, b) in tiled.chunks_exact(4).zip(flat.chunks_exact(4)) {
            let d = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
            worst = worst.max(d);
            differing += usize::from(d > 1);
        }
        eprintln!("{w}x{h}: tile path against flat path: {differing} of {} pixels differ by more than 1 (worst {worst})", tiled.len() / 4);
        if exact {
            assert_eq!(differing, 0, "same pixels, same bytes (worst {worst})");
        } else {
            // A smooth picture: the per-tile clamp only moves the colours at the seams by the
            // difference between neighbouring texels (here 4 to 5 levels).
            assert!(worst <= 8, "{w}x{h}: worst {worst}");
        }
        if w == 112 {
            save("70x-depth-synthetic-flat-equals-tiles", w, h, &tiled);
        }
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_a_silhouette_has_no_dark_or_coloured_fringe() {
    let Some(mut gpu) = Offscreen::new() else { return };
    // A 32 x 32 tile, its left half opaque white, its right half transparent with a LOUD colour in
    // the texels (what a straight-alpha filter would bleed into the edge), over the black base at
    // twice the size: every pixel must be white or black, nothing between.
    let canvas = ScreenRect { min_x: -16, min_y: -16, max_x: 16, max_y: 16 };
    let mut t = solid(2, (-16, -16), 32, 500.0, [255, 255, 255], None);
    for y in 0..32usize {
        for x in 16..32usize {
            t.rgba[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4].copy_from_slice(&[255, 0, 0, 0]);
        }
    }
    let mut frame = off_screen();
    frame.scenery = Some(scenery_of(&[t], canvas));
    let bounds = Bounds { min: Pos::new(-16.0, -16.0, 0.0), max: Pos::new(16.0, 16.0, 0.0) };
    let scene = Scene { bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: None };
    let (w, h) = (112u32, 112u32); // 64 px for 32 units: scale 2
    let img = gpu.render_field(w, h, &scene, Some(&frame), &DrawList::new());
    let (mut white, mut black) = (0i32, 0i32);
    for y in 24..88u32 {
        for x in 24..88u32 {
            let p = pixel(&img, w, x, y);
            match p {
                [255, 255, 255] => white += 1,
                [0, 0, 0] => black += 1,
                other => panic!("fringe pixel at ({x}, {y}): {other:?}"),
            }
        }
    }
    // The opaque half is 32 of the 64 pixel columns, give or take the half-texel the edge sits at.
    assert!((white - 64 * 32).abs() <= 64 * 2 && white + black == 64 * 64, "{white} white, {black} black");
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_an_srgb_target_gets_the_tile_colours_as_authored() {
    let Some(mut gpu) = Offscreen::with_format(wgpu::TextureFormat::Rgba8UnormSrgb) else { return };
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let f = figure(&frame);
    let colour = [200, 100, 50];
    frame.scenery = Some(scenery_of(&[tile_over(f.centre, 2, f.near - 50.0, colour, None)], canvas()));
    let img = draw(&mut gpu, &frame);
    let got = pixel(&img, W, f.pixel.0, f.pixel.1);
    assert!(close(got, colour, 3), "an opaque tile on an sRGB target: {got:?}");
    // the base is black, the outside the clear colour, as on a plain target
    assert!(close(pixel(&img, W, W / 2 - 60, H / 2 + 80), BLACK, 2));
    assert!(close(pixel(&img, W, 20, 20), CLEAR_RGB, 2));
}

// ---------------------------------------------------------------------------------------------
// The owner's own game data
// ---------------------------------------------------------------------------------------------

/// What the GPU frames say about one character at one spot, in a window where one window pixel is
/// one field-screen pixel (see [`Measure::window`]).
#[derive(Debug, Clone)]
struct Seen {
    /// Pixels of the character's silhouette.
    silhouette: usize,
    /// Of them, visible with the depth-tested scenery.
    visible: usize,
    /// Of them, under an opaque pixel of the flat front layer (what the old client hid).
    flat_hidden: usize,
}

impl Seen {
    fn hidden_fraction(&self) -> f32 {
        1.0 - self.visible as f32 / self.silhouette.max(1) as f32
    }

    fn flat_fraction(&self) -> f32 {
        self.flat_hidden as f32 / self.silhouette.max(1) as f32
    }
}

/// One field prepared for the measurements.
struct Measure<'a> {
    view: &'a crate::backdrop::FieldView,
    stage: &'a Stage,
    flat_front: &'a Backdrop,
    toward: f32,
    /// The field camera's x axis in field space (a unit vector): along it a point moves sideways on
    /// the screen without changing its depth.
    right: [f32; 3],
    /// Window size: the picture at one pixel per field-screen pixel plus 24 px margins.
    window: (u32, u32),
    /// The window pixel of the picture's top left corner.
    origin: (i64, i64),
    /// The flat picture alone, in the window.
    base: Vec<u8>,
}

impl<'a> Measure<'a> {
    fn new(gpu: &mut Offscreen, view: &'a crate::backdrop::FieldView, stage: &'a Stage, flat_front: &'a Backdrop, camera: &fmv_ff7::field::Camera) -> Self {
        let axes = camera.axes_f32();
        let forward = [f64::from(axes[2][0]), f64::from(axes[2][1]), f64::from(axes[2][2])];
        let toward = real::toward_camera(forward);
        let picture = view.backdrop.as_ref().expect("the picture");
        let window = (picture.width + 48, picture.height + 48);
        let cam = Camera::fit(&view.bounds, window.0 as f32, window.1 as f32, MARGIN_PX);
        assert!((cam.pixels_per_unit() - 1.0).abs() < 1e-4, "one window pixel is one field-screen pixel");
        let tl = cam.project(picture.min[0], picture.max[1]);
        let origin = ((window.0 as f32 / 2.0 + tl[0]).round() as i64, (window.1 as f32 / 2.0 - tl[1]).round() as i64);
        let mut me = Self { view, stage, flat_front, toward, right: axes[0], window, origin, base: Vec::new() };
        me.base = gpu.render_field(window.0, window.1, &me.scene(), None, &DrawList::new());
        me
    }

    fn scene(&self) -> Scene<'_> {
        Scene { bounds: self.view.bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: self.view.backdrop.as_ref() }
    }

    /// Cloud standing at `pos`, facing the camera, idle pose.
    fn cloud(&self, pos: Pos) -> AvatarPlayer<'static> {
        self.cloud_facing(pos, self.toward)
    }

    fn cloud_facing(&self, pos: Pos, facing: f32) -> AvatarPlayer<'static> {
        AvatarPlayer { who: Who(1), pos, facing, anim: AnimState::Idle, scene_frame: None, scene_clip: None, scene_return: None, appearance: Some("ff7/cloud"), npc: None, tint: [90, 125, 245], own: true, party: false }
    }

    /// A place far to the side of `pos` on the screen at the same depth: a character there is in the
    /// frame (so the frame draws the scenery) but nowhere in the picture.
    fn away(&self, pos: Pos) -> Pos {
        Pos::new(pos.x + 30_000.0 * self.right[0], pos.y + 30_000.0 * self.right[1], pos.z + 30_000.0 * self.right[2])
    }

    fn frame(&self, avatars: &mut Avatars, players: &[AvatarPlayer<'_>], marks: bool, scenery: bool) -> AvatarFrame {
        let mut frame = avatars.frame(self.view, self.stage, players, 0.0).frame;
        if !marks {
            frame.ground.clear();
        }
        if !scenery {
            frame.scenery = None;
        }
        frame
    }

    fn render(&self, gpu: &mut Offscreen, frame: &AvatarFrame) -> Vec<u8> {
        gpu.render_field(self.window.0, self.window.1, &self.scene(), Some(frame), &DrawList::new())
    }

    /// The window pixel of the flat front layer's pixel `(x, y)` is `(x + origin.0, y + origin.1)`.
    fn front_covers(&self, px: u32, py: u32) -> bool {
        let (fx, fy) = (i64::from(px) - self.origin.0, i64::from(py) - self.origin.1);
        if fx < 0 || fy < 0 || fx >= i64::from(self.flat_front.width) || fy >= i64::from(self.flat_front.height) {
            return false;
        }
        self.flat_front.rgba.get(((fy as usize * self.flat_front.width as usize + fx as usize) * 4) + 3).is_some_and(|a| *a != 0)
    }

    /// Cloud at `pos`: the silhouette (his pixels over the flat picture with no scenery), how many of
    /// them show with the depth-tested scenery, how many the flat front layer would have hidden.
    /// `images` gets the frames rendered (flat path, tile path) for a picture.
    fn look(&self, gpu: &mut Offscreen, avatars: &mut Avatars, pos: Pos) -> (Seen, Vec<u8>, Vec<u8>) {
        self.look_facing(gpu, avatars, pos, self.toward)
    }

    fn look_facing(&self, gpu: &mut Offscreen, avatars: &mut Avatars, pos: Pos, facing: f32) -> (Seen, Vec<u8>, Vec<u8>) {
        let cloud = self.cloud_facing(pos, facing);
        let alone = self.render(gpu, &self.frame(avatars, &[cloud], false, false));
        let tiled = self.render(gpu, &self.frame(avatars, &[cloud], false, true));
        let (w, _) = self.window;
        let mut seen = Seen { silhouette: 0, visible: 0, flat_hidden: 0 };
        for (i, ((a, b), t)) in alone.chunks_exact(4).zip(self.base.chunks_exact(4)).zip(tiled.chunks_exact(4)).enumerate() {
            if a[..3].iter().zip(&b[..3]).all(|(x, y)| x.abs_diff(*y) <= 6) {
                continue;
            }
            seen.silhouette += 1;
            seen.visible += usize::from(a[..3].iter().zip(&t[..3]).all(|(x, y)| x.abs_diff(*y) <= 3));
            seen.flat_hidden += usize::from(self.front_covers(i as u32 % w, i as u32 / w));
        }
        (seen, alone, tiled)
    }

    /// The old client's picture: Cloud over the picture, then the flat front layer over him.
    fn flat_picture(&self, alone: &[u8]) -> Vec<u8> {
        let mut out = alone.to_vec();
        let (w, h) = self.window;
        for y in 0..h {
            for x in 0..w {
                if self.front_covers(x, y) {
                    let (fx, fy) = ((i64::from(x) - self.origin.0) as usize, (i64::from(y) - self.origin.1) as usize);
                    let from = (fy * self.flat_front.width as usize + fx) * 4;
                    let at = ((y * w + x) * 4) as usize;
                    out[at..at + 3].copy_from_slice(&self.flat_front.rgba[from..from + 3]);
                }
            }
        }
        out
    }
}

/// Pixels where the tile picture `a` differs from the flat picture `b` by more than 24 levels, and
/// those that are green in `a` (green well above red and blue) and not in `b`: the key-colour fringe.
fn compare_pictures(a: &[u8], b: &[u8]) -> (usize, usize) {
    let greenish = |p: &[u8]| p[1] > p[0].saturating_add(40) && p[1] > p[2].saturating_add(40);
    let (mut far, mut green) = (0usize, 0usize);
    for (x, y) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        far += usize::from(x[..3].iter().zip(&y[..3]).any(|(p, q)| p.abs_diff(*q) > 24));
        green += usize::from(greenish(x) && !greenish(y));
    }
    (far, green)
}

/// `n` steps per edge of a grid of points over a triangle (the example's dense sampling).
fn grid(t: &[Pos; 3], n: usize) -> Vec<Pos> {
    let mut out = Vec::new();
    for i in 0..=n {
        for j in 0..=(n - i) {
            let (u, v) = (i as f32 / n as f32, j as f32 / n as f32);
            let w = 1.0 - u - v;
            out.push(Pos::new(w * t[0].x + u * t[1].x + v * t[2].x, w * t[0].y + u * t[1].y + v * t[2].y, w * t[0].z + u * t[1].z + v * t[2].z));
        }
    }
    out
}

/// The three-panel picture around Cloud's silhouette, 3x: the old flat picture, the tile picture, and
/// the difference (the pixels of Cloud the scenery hides, in red over the flat picture).
fn panels(m: &Measure<'_>, alone: &[u8], tiled: &[u8], name: &str) {
    let (w, h) = m.window;
    let mut bbox = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (i, (a, b)) in alone.chunks_exact(4).zip(m.base.chunks_exact(4)).enumerate() {
        if a[..3].iter().zip(&b[..3]).any(|(x, y)| x.abs_diff(*y) > 6) {
            let (x, y) = (i as u32 % w, i as u32 / w);
            bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
        }
    }
    if bbox.0 == u32::MAX {
        return;
    }
    let half = (((bbox.2 - bbox.0).max(bbox.3 - bbox.1) / 2) + 12).clamp(40, 120);
    let (cx, cy) = ((bbox.0 + bbox.2) / 2, (bbox.1 + bbox.3) / 2);
    let (x0, y0) = (cx.saturating_sub(half).min(w - 2 * half), cy.saturating_sub(half).min(h - 2 * half));
    let flat = m.flat_picture(alone);
    let mut marked = tiled.to_vec();
    for (i, ((a, t), b)) in alone.chunks_exact(4).zip(tiled.chunks_exact(4)).zip(m.base.chunks_exact(4)).enumerate() {
        let is_cloud = !a[..3].iter().zip(&b[..3]).all(|(x, y)| x.abs_diff(*y) <= 6);
        if is_cloud && !a[..3].iter().zip(&t[..3]).all(|(x, y)| x.abs_diff(*y) <= 3) {
            marked[i * 4..i * 4 + 3].copy_from_slice(&[255, 0, 255]);
        }
    }
    let zoom = 3u32;
    let side = 2 * half;
    let mut sheet = Vec::new();
    for y in 0..side * zoom {
        for img in [&flat, tiled, &marked] {
            for x in 0..side * zoom {
                let at = (((y0 + y / zoom) * w + x0 + x / zoom) * 4) as usize;
                sheet.extend_from_slice(&img[at..at + 4]);
            }
        }
    }
    real::save_png(name, side * zoom * 3, side * zoom, &sheet);
}

/// A spot to look at: where, and what the software composite of `fmv-ff7` found there (hidden
/// fraction of Cloud's silhouette under the depth rule, and under the flat front layer).
struct Target {
    name: &'static str,
    /// Walkmesh triangle to search (the composite's), `None` for the counter-example search.
    triangle: Option<usize>,
    /// The band of hidden fractions to pick from on that triangle, and what to prefer inside it.
    band: (f32, f32),
    prefer: Prefer,
    /// Camera depth range of the spot.
    depth: (f32, f32),
    /// The software composite's numbers: Cloud's silhouette px, hidden px under the depth rule, under
    /// the flat layer (`examples/depth_composite.rs md1stin`).
    software: (usize, usize, usize),
}

/// What to prefer among the samples of a triangle that fit a band.
#[derive(Clone, Copy)]
enum Prefer {
    /// The biggest silhouette (the composite's choice for the spots behind an object).
    Silhouette,
    /// The most pixels the flat front layer would have hidden (its choice for the spots in front).
    FlatHidden,
}

const TARGETS: [Target; 7] = [
    Target { name: "behind-wall-hidden", triangle: Some(38), band: (0.9, 1.01), prefer: Prefer::Silhouette, depth: (300.0, 6000.0), software: (150, 150, 150) },
    Target { name: "behind-wall-partly", triangle: Some(36), band: (0.25, 0.9), prefer: Prefer::Silhouette, depth: (300.0, 6000.0), software: (145, 63, 63) },
    Target { name: "in-front-of-wall", triangle: Some(25), band: (-0.01, 0.01), prefer: Prefer::FlatHidden, depth: (450.0, 1300.0), software: (3518, 0, 1347) },
    Target { name: "in-front-of-wall-near", triangle: Some(28), band: (-0.01, 0.01), prefer: Prefer::FlatHidden, depth: (300.0, 1300.0), software: (7117, 0, 2159) },
    Target { name: "partly-behind-post-152", triangle: Some(22), band: (0.25, 0.9), prefer: Prefer::Silhouette, depth: (450.0, 1300.0), software: (1769, 481, 1345) },
    Target { name: "partly-behind-post-294", triangle: Some(31), band: (0.25, 0.9), prefer: Prefer::Silhouette, depth: (450.0, 1300.0), software: (479, 120, 120) },
    Target { name: "partly-behind-train", triangle: Some(26), band: (0.25, 0.95), prefer: Prefer::Silhouette, depth: (450.0, 1300.0), software: (3319, 2817, 3311) },
];

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_ff7_md1stin_characters_are_hidden_by_depth_not_by_the_flat_front_layer() {
    use crate::backdrop::FieldView;
    use fmv_adapter_api::AreaRuntime;
    let Some(real) = real::open() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let key = "md1stin";
    let view = real::timed("field view", || FieldView::ff7_with(&real.adapter, key, true)).expect("field view");
    eprintln!("{}", view.description);
    let stage = view.stage.clone().expect("a stage");
    let scenery = stage.scenery.clone().expect("md1stin has scenery in front of the characters");
    eprintln!("scenery: {:?}", scenery.counts);
    let area = real.adapter.load_field(key).expect("field");
    let flat_front = real::flat_front(&area).expect("the flat front layer");
    let camera = area.cameras().expect("cameras")[0];
    let m = Measure::new(&mut gpu, &view, &stage, &flat_front, &camera);
    let mut avatars = real::avatars(&real);

    // ---- 0. With nobody in the picture the tile picture is the flat picture, pixel for pixel -----
    // (one window pixel is one picture pixel: the tile path with its depth test must paint every
    // layer in the order the flat picture was composed in, and show no key-colour pixel).
    let away = m.away(area.spawn_points()[0]);
    let frame = m.frame(&mut avatars, &[m.cloud(away)], false, true);
    assert_eq!(frame.models.len(), 1, "the off-screen figure keeps the frame in tile mode");
    let tiled = m.render(&mut gpu, &frame);
    assert_eq!(gpu.scenery_state().1, Some(scenery.id), "the tile path was taken");
    let (mut differing, mut worst) = (0usize, 0u8);
    for (a, b) in tiled.chunks_exact(4).zip(m.base.chunks_exact(4)) {
        let d = a[..3].iter().zip(&b[..3]).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        worst = worst.max(d);
        differing += usize::from(d > 1);
    }
    let green = |img: &[u8]| img.chunks_exact(4).filter(|p| p[..3] == [0, 255, 0]).count();
    eprintln!("tile picture against flat picture at 1:1: {differing} of {} pixels differ by more than 1 (worst {worst}); key-colour pixels {} against {}", tiled.len() / 4, green(&tiled), green(&m.base));
    assert_eq!(green(&tiled), green(&m.base), "no key-colour pixel of the back layers shows");
    assert_eq!(differing, 0, "the tile picture is the flat picture (worst {worst})");

    // What a frame costs here (software Vulkan, so only the ratio means anything): the same frame as
    // tiles and over the flat picture, in the 1100x680 window, readback included.
    let time = |gpu: &mut Offscreen, frame: Option<&AvatarFrame>| {
        let started = std::time::Instant::now();
        for _ in 0..20 {
            gpu.render_field(1100, 680, &m.scene(), frame, &DrawList::new());
        }
        started.elapsed() / 20
    };
    let (with_tiles, without) = (time(&mut gpu, Some(&frame)), time(&mut gpu, None));
    eprintln!(
        "a 1100x680 frame: {with_tiles:.2?} with the {} tiles, {without:.2?} with the flat picture; draw calls {} (scenery) + 1 (marks) + one per model batch",
        scenery.counts.tiles,
        gpu.scenery_state().2
    );

    // At other window sizes the filter reads across texels: the tile picture then differs from the flat
    // one along the objects' silhouettes (cut at half alpha instead of blended) and by the odd
    // level at tile seams, and no key-colour green runs round the objects.
    for (w, h) in [(1100u32, 680u32), (1920, 1080), (400, 300)] {
        let tiled = gpu.render_field(w, h, &m.scene(), Some(&frame), &DrawList::new());
        let flat = gpu.render_field(w, h, &m.scene(), None, &DrawList::new());
        let (far, fringe) = compare_pictures(&tiled, &flat);
        eprintln!("{w}x{h}: {far} of {} pixels differ by more than 24 levels (silhouettes), {fringe} are newly green", tiled.len() / 4);
        assert!(far * 100 < tiled.len() / 4 * 3, "{w}x{h}: {far} pixels differ");
        assert!(fringe <= 8, "{w}x{h}: {fringe} green fringe pixels");
    }

    // ---- 1. The old counter-example: in front of the wall piece, hidden by the flat layer only ----
    let mut table: Vec<(String, Pos, Seen, usize)> = Vec::new();
    let tris = area.walk_triangles();
    let candidates = real::spots(&mut avatars, &view, &stage, &area, Some(&flat_front), (450.0, 1300.0));
    // The place the old test picked: the character about half covered (by the flat layer's reckoning).
    let counter = candidates
        .iter()
        .copied()
        .filter(|s| s.covered > 0.2 && s.covered < 0.9)
        .min_by(|a, b| (a.covered - 0.5).abs().total_cmp(&(b.covered - 0.5).abs()))
        .expect("a place half under the flat front layer");
    let (seen, alone, tiled) = m.look(&mut gpu, &mut avatars, counter.pos);
    eprintln!(
        "counter-example {:?} (depth {:.0}): Cloud {} px; the flat front layer hides {} ({:.0}%); the depth-tested scenery hides {} ({:.1}%)   [software composite: 3305 px, flat 1296, depth 0]",
        counter.pos,
        counter.depth,
        seen.silhouette,
        seen.flat_hidden,
        100.0 * seen.flat_fraction(),
        seen.silhouette - seen.visible,
        100.0 * seen.hidden_fraction()
    );
    assert!(seen.silhouette > 2000, "Cloud is a good size here: {} px", seen.silhouette);
    assert!(seen.flat_fraction() > 0.25, "the flat layer hid a good part of him: {:.2}", seen.flat_fraction());
    assert!(seen.hidden_fraction() < 0.01, "he is fully visible with depth: {:.3} hidden", seen.hidden_fraction());
    panels(&m, &alone, &tiled, "70-depth-counterexample");
    table.push(("counterexample".into(), counter.pos, seen, 0));
    let full = {
        let frame = m.frame(&mut avatars, &[m.cloud(counter.pos)], true, true);
        gpu.render_field(1100, 680, &m.scene(), Some(&frame), &DrawList::new())
    };
    save("70-depth-counterexample-full", 1100, 680, &full);

    // ---- 2. The software composite's spots, by triangle and by what is hidden there --------------
    for target in &TARGETS {
        let triangle = target.triangle.expect("every listed target names a triangle");
        let mut best: Option<(Pos, Seen, Vec<u8>, Vec<u8>)> = None;
        for pos in grid(&tris[triangle], 10) {
            let Some(depth) = stage.camera.project([f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)]).map(|s| s[2] as f32) else { continue };
            if depth < target.depth.0 || depth > target.depth.1 {
                continue;
            }
            let (seen, alone, tiled) = m.look(&mut gpu, &mut avatars, pos);
            if seen.silhouette < 40 || !(target.band.0..=target.band.1).contains(&seen.hidden_fraction()) {
                continue;
            }
            let score = |s: &Seen| match target.prefer {
                Prefer::Silhouette => s.silhouette,
                Prefer::FlatHidden => s.flat_hidden,
            };
            if best.as_ref().is_none_or(|b| score(&seen) > score(&b.1)) {
                best = Some((pos, seen, alone, tiled));
            }
        }
        let (pos, seen, alone, tiled) = best.unwrap_or_else(|| panic!("{}: no sample of triangle {triangle} has Cloud hidden by {:?}", target.name, target.band));
        let (sw_px, sw_hidden, sw_flat) = target.software;
        eprintln!(
            "{:24} triangle {triangle} {:?}: Cloud {} px, depth-tested scenery hides {} ({:.1}%), flat layer hides {} ({:.0}%)   [software composite: {sw_px} px, depth {sw_hidden} ({:.1}%), flat {sw_flat} ({:.0}%)]",
            target.name,
            pos,
            seen.silhouette,
            seen.silhouette - seen.visible,
            100.0 * seen.hidden_fraction(),
            seen.flat_hidden,
            100.0 * seen.flat_fraction(),
            100.0 * sw_hidden as f32 / sw_px as f32,
            100.0 * sw_flat as f32 / sw_px as f32,
        );
        // The GPU agrees with the software composite on what is hidden, to the sample's choice.
        let want = sw_hidden as f32 / sw_px as f32;
        assert!((seen.hidden_fraction() - want).abs() < 0.2, "{}: hidden {:.2} against the software composite's {:.2}", target.name, seen.hidden_fraction(), want);
        panels(&m, &alone, &tiled, &format!("70-depth-{}", target.name));
        if matches!(target.name, "behind-wall-hidden" | "behind-wall-partly") {
            let frame = m.frame(&mut avatars, &[m.cloud(pos)], true, true);
            let full = gpu.render_field(1100, 680, &m.scene(), Some(&frame), &DrawList::new());
            save(&format!("70-depth-{}-full", target.name), 1100, 680, &full);
        }
        table.push((target.name.into(), pos, seen, sw_hidden));
    }

    // ---- 3. Over the walkable places the depth rule frees most of what the flat layer hid -------
    // (the composite's own candidate set and pose: Cloud turned to heading 0 at every place of depth
    // 450..1300, as the old search posed him)
    let (mut flat_total, mut depth_total, mut silhouettes, mut touched) = (0usize, 0usize, 0usize, 0usize);
    for s in &candidates {
        let (seen, _, _) = m.look_facing(&mut gpu, &mut avatars, s.pos, 0.0);
        flat_total += seen.flat_hidden;
        depth_total += seen.silhouette - seen.visible;
        silhouettes += seen.silhouette;
        touched += usize::from(s.covered > 0.0);
    }
    eprintln!(
        "over the {} walkable places in depth 450..1300 ({touched} where the flat layer overlaps Cloud's rectangle) Cloud has {silhouettes} px; the flat front layer hides {flat_total} of them, the depth-tested scenery {depth_total} ({:.0}% of the flat layer's)   [software composite over the same places: 59 of 73 touched, 67533 px, flat 44853, depth 15631 (35%)]",
        candidates.len(),
        100.0 * depth_total as f32 / flat_total.max(1) as f32,
    );
    assert!(depth_total * 2 < flat_total, "the depth rule frees most of what the flat layer hid");
    for (name, pos, seen, _) in &table {
        eprintln!("table: {name:24} {pos:?} silhouette {:5} hidden {:5.1}% flat {:5.1}%", seen.silhouette, 100.0 * seen.hidden_fraction(), 100.0 * seen.flat_fraction());
    }
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_ff7_md1_1_has_blended_tiles_in_front_and_its_characters_stand_among_the_scenery() {
    use crate::avatar::frame::NpcRef;
    use crate::backdrop::FieldView;
    use fmv_adapter_api::AreaRuntime;
    let Some(real) = real::open() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let key = "md1_1";
    let view = real::timed("field view", || FieldView::ff7_with(&real.adapter, key, true)).expect("field view");
    eprintln!("{}", view.description);
    let stage = view.stage.clone().expect("a stage");
    let scenery = stage.scenery.clone().expect("md1_1 has scenery in front of the characters");
    eprintln!("scenery: {:?}; {} blended runs", scenery.counts, scenery.runs.len());
    assert!(scenery.counts.blended > 0, "md1_1 has blended tiles in front (the flat split refused it)");
    let area = real.adapter.load_field(key).expect("field");
    let flat_front = real::flat_front(&area).expect("the flat front layer");
    let camera = area.cameras().expect("cameras")[0];
    let m = Measure::new(&mut gpu, &view, &stage, &flat_front, &camera);
    let mut avatars = real::avatars(&real);

    // The tile picture against the flat picture, nobody in it, at 1:1. The blended tiles go through the
    // blend unit after the opaque ones, in paint order, instead of into the composite in place, so
    // they may differ by the unit's rounding and where a blended tile shares a depth with an
    // opaque one that the file paints later.
    let spawn = area.spawn_points()[0];
    let frame = m.frame(&mut avatars, &[m.cloud(m.away(spawn))], false, true);
    assert_eq!(frame.models.len(), 1, "the off-screen figure keeps the frame in tile mode");
    let tiled = m.render(&mut gpu, &frame);
    assert_eq!(gpu.scenery_state().1, Some(scenery.id));
    let (mut differing, mut off_by_more, mut worst) = (0usize, 0usize, 0u8);
    for (a, b) in tiled.chunks_exact(4).zip(m.base.chunks_exact(4)) {
        let d = a[..3].iter().zip(&b[..3]).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        worst = worst.max(d);
        differing += usize::from(d > 1);
        off_by_more += usize::from(d > 8);
    }
    eprintln!(
        "md1_1 tile picture against flat picture at 1:1: {differing} of {} pixels differ by more than 1, {off_by_more} by more than 8 (worst {worst}); draw calls per frame {}",
        tiled.len() / 4,
        gpu.scenery_state().2
    );
    assert!(off_by_more * 1000 < tiled.len() / 4, "the blended tiles land where the composite put them: {off_by_more} pixels off by more than 8");
    save("70-depth-md1_1-no-character", m.window.0, m.window.1, &tiled);
    for (w, h) in [(1100u32, 680u32), (1920, 1080)] {
        let tiled = gpu.render_field(w, h, &m.scene(), Some(&frame), &DrawList::new());
        let flat = gpu.render_field(w, h, &m.scene(), None, &DrawList::new());
        let (far, fringe) = compare_pictures(&tiled, &flat);
        // (the glows are additive on the blend unit after the filter, the flat picture added them
        // before it: the saturated cores of the green lamps differ, and are greener; so the count of
        // newly green pixels is printed here and only asserted for md1stin, which has no green lamp)
        eprintln!("md1_1 {w}x{h}: {far} of {} pixels differ by more than 24 levels, {fringe} are newly green (lamp cores)", tiled.len() / 4);
        assert!(far * 100 < tiled.len() / 4 * 3, "{w}x{h}: {far} pixels differ");
    }

    // The area's own characters (the guards' doors, the AVALANCHE members) and Cloud among them, as
    // the window draws them: 1920x1080.
    let reported = area.npcs();
    let npcs: Vec<NpcRef<'_>> = reported
        .iter()
        .filter(|n| n.unconditional && n.pos.is_finite() && n.facing.is_finite())
        .map(|n| NpcRef { model: n.appearance.model.as_str(), idle: n.appearance.idle_animation.as_deref() })
        .collect();
    avatars.set_npcs(&npcs);
    let mut players: Vec<AvatarPlayer<'_>> = reported
        .iter()
        .filter(|n| n.unconditional && n.pos.is_finite() && n.facing.is_finite())
        .enumerate()
        .map(|(i, n)| AvatarPlayer {
            who: Who((1 << 63) + i as u64),
            pos: n.pos,
            facing: n.facing,
            anim: AnimState::Idle,
            scene_frame: None, scene_clip: None, scene_return: None,
            appearance: None,
            npc: Some(NpcRef { model: n.appearance.model.as_str(), idle: n.appearance.idle_animation.as_deref() }),
            tint: [120, 130, 140],
            own: false,
            party: false,
        })
        .collect();
    let first_npc = players.first().map_or(spawn, |p| p.pos);
    players.push(m.cloud(Pos::new(first_npc.x + 170.0, first_npc.y, first_npc.z)));
    let with = m.frame(&mut avatars, &players, true, true);
    let without = m.frame(&mut avatars, &players, true, false);
    eprintln!("{:?}: {} players, {} models", with, players.len(), with.models.len());
    let (img_tiles, img_flat) = (gpu.render_field(1920, 1080, &m.scene(), Some(&with), &DrawList::new()), gpu.render_field(1920, 1080, &m.scene(), Some(&without), &DrawList::new()));
    let differing = img_tiles.chunks_exact(4).zip(img_flat.chunks_exact(4)).filter(|(a, b)| a[..3].iter().zip(&b[..3]).any(|(x, y)| x.abs_diff(*y) > 16)).count();
    eprintln!("md1_1 with its characters at 1920x1080: {differing} pixels differ between the depth-tested scenery and the flat picture with models over it");
    save("71-depth-md1_1-npcs", 1920, 1080, &img_tiles);
    save("71-depth-md1_1-npcs-flat-path", 1920, 1080, &img_flat);
    assert!(differing > 0, "the scenery in front of the characters changes something");
}

/// FXAA wraps the completed tiled field, including the model/depth pass; the scenery's early
/// return must still resolve to the window, and a foreground tile must still hide the model.
#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_fxaa_keeps_tiled_field_model_occlusion() {
    use crate::graphics::AntiAliasing;
    let mut gpu = Offscreen::new().expect("GPU or software adapter");
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let figure = figure(&frame);
    for (depth, expected) in [(figure.near - 1.0, GREEN), (figure.far + 1.0, RED)] {
        frame.scenery = Some(scenery_of(&[tile_over(figure.centre, 2, depth, GREEN, None)], canvas()));
        for mode in [AntiAliasing::Off, AntiAliasing::Fxaa] {
            let image = gpu.render_with(W, H, |gpu, target, srgb| {
                gpu.set_anti_aliasing(mode);
                let scene = Scene { bounds: bounds(), triangles: &[], players: &[], fixture_banner: false, backdrop: None };
                gpu.draw_field(target, (W, H), &scene, Some(&frame), srgb);
            });
            let actual = pixel(&image, W, figure.pixel.0, figure.pixel.1);
            assert!(close(actual, expected, 2), "{mode:?}, tile depth {depth}: {actual:?}, expected {expected:?}");
        }
    }
}

/// Reviewable off/on pictures from owned FF7 data, with the same character, camera and UI. These
/// are comparison artifacts, not evidence that an optional image filter matches the original game.
#[test]
#[ignore = "needs owned FF7 data and a GPU or software Vulkan/GL driver"]
fn offscreen_fxaa_real_ff7_field_comparison() {
    real_ff7_graphics_comparison(false);
}

#[test]
#[ignore = "needs owned FF7 data and a GPU or software Vulkan/GL driver"]
fn offscreen_supersample_real_ff7_field_comparison() {
    real_ff7_graphics_comparison(true);
}

fn real_ff7_graphics_comparison(supersample: bool) {
    use crate::backdrop::FieldView;
    use crate::camera::{CameraInput, FieldCamera};
    use crate::graphics::{AntiAliasing, RenderScale};
    let real = real::open().expect("owned FF7 data");
    let mut gpu = Offscreen::new().expect("GPU or software adapter");
    let mut view = FieldView::ff7_with(&real.adapter, "md1stin", true).expect("field view");
    let stage = view.stage.clone().expect("field camera");
    let area = real.adapter.load_field("md1stin").expect("field");
    let front = real::flat_front(&area).expect("front layer");
    let camera = area.cameras().expect("cameras")[0];
    let m = Measure::new(&mut gpu, &view, &stage, &front, &camera);
    let mut avatars = real::avatars(&real);
    let candidates = real::spots(&mut avatars, &view, &stage, &area, Some(&front), (450.0, 1300.0));
    let clear: Vec<_> = candidates.into_iter().filter(|s| s.covered == 0.0 && view.range.is_none_or(|range| {
        s.view_rect.0 >= range.min.x && s.view_rect.1 >= range.min.y && s.view_rect.2 <= range.max.x && s.view_rect.3 <= range.max.y
    })).collect();
    let selected = real::spread(&clear, 1);
    let place = selected.first().expect("visible walkable place").pos;
    let mut frame = m.frame(&mut avatars, &[m.cloud(place)], true, true);
    assert_eq!(frame.models.len(), 1, "Cloud is loaded from the owned character archive");
    assert!(frame.scenery.is_some(), "the real depth-tested tile path");
    let own = view.to_view(place).map(|p| [p.x, p.y]);
    view.bounds = FieldCamera::default().update(&CameraInput {
        range: view.range, whole: view.whole, window: [1100.0, 680.0], margin: MARGIN_PX,
        own, scene: None, dt: 0.0,
    });
    let scene = Scene { bounds: view.bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: view.backdrop.as_ref() };
    let mut overlay = DrawList::new();
    overlay.fill_rect(16, 16, 520, 32, [8, 24, 64, 255]);
    overlay.draw_text(24, 28, 1, [255, 255, 255, 255], "FF7 md1stin - same camera, scene and native UI");
    let modes = if supersample {
        vec![(RenderScale::Native, AntiAliasing::Off, "graphics-md1stin-native"),
             (RenderScale::Double, AntiAliasing::Off, "graphics-md1stin-2x"),
             (RenderScale::Double, AntiAliasing::Fxaa, "graphics-md1stin-2x-fxaa")]
    } else {
        vec![(RenderScale::Native, AntiAliasing::Off, "graphics-md1stin-off"),
             (RenderScale::Native, AntiAliasing::Fxaa, "graphics-md1stin-fxaa")]
    };
    let mut images = Vec::new();
    for (scale, mode, name) in modes {
        let image = gpu.render_with(1100, 680, |gpu, target, srgb| {
            gpu.set_render_scale(scale);
            gpu.set_anti_aliasing(mode);
            gpu.draw_field(target, (1100, 680), &scene, Some(&frame), srgb);
            gpu.draw_overlay(target, &overlay.vertices(1100, 680, srgb));
        });
        save(name, 1100, 680, &image);
        images.push(image);
    }
    let different = images[0].chunks_exact(4).zip(images[1].chunks_exact(4)).filter(|(a, b)| a != b).count();
    eprintln!("optional {} changes {different} of 748000 pixels in the owned md1stin comparison", if supersample { "2x scene rendering" } else { "FXAA" });
    assert!(different > 100, "the real render path applies the option");
    for image in &images[1..] { for y in 16..48 { for x in 16..536 {
        let i = (y * 1100 + x) * 4;
        assert_eq!(&images[0][i..i+4], &image[i..i+4], "UI remains unchanged");
    } } }
    if supersample {
        // Isolate the same owned model from the busy picture to measure its actual rendered
        // footprint. Subpixel edge coverage may add one pixel; scale or framing must not change.
        frame.scenery = None;
        let bare = Scene { backdrop: None, ..scene };
        let mut footprints = Vec::new();
        for scale in [RenderScale::Native, RenderScale::Double] {
            let image = gpu.render_with(1100, 680, |gpu, target, srgb| {
                gpu.set_anti_aliasing(AntiAliasing::Off);
                gpu.set_render_scale(scale);
                gpu.draw_field(target, (1100, 680), &bare, Some(&frame), srgb);
            });
            footprints.push(model_footprint(&image, 1100, CLEAR_RGB));
        }
        assert_same_footprint(footprints[0], footprints[1]);
        eprintln!("owned Cloud footprint: native {:?}, 2x {:?}", footprints[0], footprints[1]);
    }
}

/// Bound the visible model against a uniform background, ignoring tiny colour conversion noise.
fn model_footprint(image: &[u8], width: u32, clear: [u8; 3]) -> [u32; 4] {
    let mut bounds = [u32::MAX, u32::MAX, 0, 0];
    for (i, rgba) in image.chunks_exact(4).enumerate() {
        if rgba[..3].iter().zip(clear).any(|(a, b)| a.abs_diff(b) > 2) {
            let (x, y) = (i as u32 % width, i as u32 / width);
            bounds = [bounds[0].min(x), bounds[1].min(y), bounds[2].max(x), bounds[3].max(y)];
        }
    }
    assert!(bounds[0] < bounds[2] && bounds[1] < bounds[3], "a visible model: {bounds:?}");
    bounds
}

fn assert_same_footprint(native: [u32; 4], doubled: [u32; 4]) {
    assert!(native.iter().zip(doubled).all(|(a, b)| a.abs_diff(b) <= 1),
        "2x adds edge coverage within one output pixel; it must not move or scale the model: {native:?} / {doubled:?}");
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_supersample_keeps_model_framing_and_foreground_depth() {
    use crate::graphics::{AntiAliasing, RenderScale};
    use crate::battle::scene::BattleFrame;
    let mut gpu = Offscreen::new().expect("GPU or software adapter");
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let figure = figure(&frame);
    // Depth and models use the same doubled projection, and resolving does not expose a
    // model behind a foreground tile. FXAA is independent and can run after the resolve.
    for (depth, expected) in [(figure.near - 1.0, GREEN), (figure.far + 1.0, RED)] {
        frame.scenery = Some(scenery_of(&[tile_over(figure.centre, 2, depth, GREEN, None)], canvas()));
        for scale in [RenderScale::Native, RenderScale::Double] { for mode in [AntiAliasing::Off, AntiAliasing::Fxaa] {
            let image = gpu.render_with(W, H, |gpu, target, srgb| {
                gpu.set_render_scale(scale);
                gpu.set_anti_aliasing(mode);
                let scene = Scene { bounds: bounds(), triangles: &[], players: &[], fixture_banner: false, backdrop: None };
                gpu.draw_field(target, (W, H), &scene, Some(&frame), srgb);
            });
            let actual = pixel(&image, W, figure.pixel.0, figure.pixel.1);
            assert!(close(actual, expected, 2), "{scale:?} {mode:?}, tile depth {depth}: {actual:?}, expected {expected:?}");
        } }
    }
    frame.scenery = None;
    // Put the authored range just before the model's leftmost vertex. Even its subpixel tip
    // must be cleared before FXAA/downsampling, not allowed to bleed into the valid field.
    let first_x = frame.models.iter().flat_map(|model| &model.positions).map(|&p| field_screen(p).0)
        .fold(f32::INFINITY, f32::min);
    let range = Bounds { min: Pos::new(-100.0, -100.0, 0.0), max: Pos::new(first_x - 0.01, 100.0, 0.0) };
    let scene = Scene { bounds: bounds(), triangles: &[], players: &[], fixture_banner: false, backdrop: None };
    for scale in [RenderScale::Native, RenderScale::Double] { for mode in [AntiAliasing::Off, AntiAliasing::Fxaa] {
        let mut render = |model| gpu.render_with(W, H, |gpu, target, srgb| {
            gpu.set_render_scale(scale);
            gpu.set_anti_aliasing(mode);
            gpu.draw_field_clipped(target, (W, H), &scene, model, srgb, Some(range));
        });
        let empty = render(None);
        let outside = render(Some(&frame));
        assert!(outside == empty, "{scale:?} {mode:?}: an off-range model must not leak through effects at the range edge");
    } }
    let shifted = Bounds { min: Pos::new(-80.0, -110.0, 0.0), max: Pos::new(120.0, 90.0, 0.0) };
    let scene = Scene { bounds: shifted, triangles: &[], players: &[], fixture_banner: false, backdrop: None };
    let battle = BattleFrame { clear_rgb: CLEAR_RGB, stage: vec![], location: None,
        models: frame, tints: vec![], identities: vec![], camera: Camera::pixels(73.0, -29.0) };
    for field in [true, false] {
        let camera = if field { Camera::fit(&shifted, W as f32, H as f32, MARGIN_PX) } else { battle.camera };
        let geometric = battle.models.models.iter().flat_map(|model| &model.positions)
            .map(|&p| project_pixel(&camera, battle.models.zoom, W, H, p))
            .fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)]);
        let mut coverage = Vec::new();
        for scale in [RenderScale::Native, RenderScale::Double] {
            let image = gpu.render_with(W, H, |gpu, target, srgb| {
                gpu.set_render_scale(scale);
                gpu.set_anti_aliasing(AntiAliasing::Off);
                if field { gpu.draw_field(target, (W, H), &scene, Some(&battle.models), srgb); }
                else { gpu.draw_battle(target, (W, H), &battle, srgb); }
            });
            let footprint = model_footprint(&image, W, CLEAR_RGB);
            // This fixture has needle-thin triangle tips: native centre samples can miss several
            // rows which 2x correctly covers. Check the authored projected geometry, not a larger
            // arbitrary tolerance around the less complete native silhouette.
            assert!(footprint[0] as f32 >= geometric[0].floor() && footprint[1] as f32 >= geometric[1].floor()
                && footprint[2] as f32 <= geometric[2].ceil() && footprint[3] as f32 <= geometric[3].ceil(),
                "{field} {scale:?}: rendered {footprint:?} stays inside projected vertices {geometric:?}");
            let background = pixel(&image, W, 0, 0);
            let mut sum = [0.0f64; 3];
            for (i, rgba) in image.chunks_exact(4).enumerate() {
                let weight = f64::from(rgba[..3].iter().zip(background).map(|(a, b)| a.abs_diff(b)).max().unwrap()) / 255.0;
                sum[0] += (i as u32 % W) as f64 * weight;
                sum[1] += (i as u32 / W) as f64 * weight;
                sum[2] += weight;
            }
            let measured = ([sum[0] / sum[2], sum[1] / sum[2]], sum[2]);
            eprintln!("{} {scale:?}: projected {geometric:?}, raster {footprint:?}, coverage {measured:?}", if field { "field" } else { "battle" });
            save(&format!("graphics-framing-{}-{}", if field { "field" } else { "battle" }, if scale == RenderScale::Native { "native" } else { "2x" }), W, H, &image);
            coverage.push(measured);
        }
        assert!(coverage[0].0.iter().zip(coverage[1].0).all(|(a, b)| (a - b).abs() <= 0.5),
            "coverage centre stays within half a native pixel: {coverage:?}");
        assert!((coverage[0].1 - coverage[1].1).abs() / coverage[0].1 < 0.05,
            "the visible area remains the same within raster sampling error: {coverage:?}");
    }
}
