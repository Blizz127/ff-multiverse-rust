//! The optional 4x raster changes texture detail, never the field's logical geometry or depth.
//! Synthetic checks need only a GPU. The owned-data check additionally needs the original FF7
//! data and an explicit `FMV_BACKGROUND_TEST_DIR` containing the private nmkin_1 PNG pages.

use super::*;
use crate::backdrop::FieldView;
use crate::background_pack::PngFieldPack;
use fmv_ff7::background::raster::{compose_tile_pixels, TilePixels};
use std::path::PathBuf;

fn scaled_scenery(tiles: &[DepthTile], rgba: &[Vec<u8>], canvas: ScreenRect) -> Arc<Scenery> {
    let pixels: Vec<_> = tiles.iter().zip(rgba).map(|(tile, rgba)| TilePixels { side: tile.size() * 4, rgba }).collect();
    let (scenery, note) = Scenery::choose_with_pixels(tiles, &pixels, canvas, 4);
    Arc::new(scenery.expect(&note))
}

fn assert_geometry(native: &Scenery, scaled: &Scenery) {
    assert_eq!((native.counts.tiles, native.counts.opaque, native.counts.blended, native.counts.in_front),
        (scaled.counts.tiles, scaled.counts.opaque, scaled.counts.blended, scaled.counts.in_front));
    assert_eq!(native.runs, scaled.runs, "blend modes and paint order stay native");
    let geometry = |vertices: &[crate::avatar::scenery::TileVertex]| vertices.iter().map(|v| (v.screen, v.depth)).collect::<Vec<_>>();
    assert_eq!(geometry(&native.opaque), geometry(&scaled.opaque), "opaque positions/depths, including the black base");
    assert_eq!(geometry(&native.blended), geometry(&scaled.blended), "blended positions/depths");
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_upscale_foreground_keeps_native_depth_and_four_times_finer_holes() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let figure = figure(&frame);
    let sample = |image: &[u8]| pixel(image, W, figure.pixel.0, figure.pixel.1);
    let probe = clear_probe(&mut gpu, &frame, &figure);
    for (depth, expected) in [(figure.near - 1.0, GREEN), (figure.far + 1.0, RED)] {
        let tiles = [tile_over(figure.centre, 2, depth, GREEN, None)];
        let rgba = vec![[0, 255, 0, 255].repeat(128 * 128)];
        let scaled = scaled_scenery(&tiles, &rgba, canvas());
        assert_geometry(&scenery_of(&tiles, canvas()), &scaled);
        frame.scenery = Some(scaled);
        assert!(close(sample(&draw(&mut gpu, &frame)), expected, 2), "native depth {depth}");
    }
    let tiles = [tile_over(figure.centre, 2, figure.near - 1.0, GREEN, None)];
    let mut rgba = vec![[0, 255, 0, 255].repeat(128 * 128)];
    // The physical 64x64 hole is still only 16x16 field units. Alpha 127 must not become an
    // occluder during hidden-texel baking, and its magenta RGB must not leak through filtering.
    for y in 32..96usize {
        for x in 32..96usize {
            rgba[0][(y * 128 + x) * 4..(y * 128 + x) * 4 + 4].copy_from_slice(&[255, 0, 255, 127]);
        }
    }
    frame.scenery = Some(scaled_scenery(&tiles, &rgba, canvas()));
    let image = draw(&mut gpu, &frame);
    assert!(close(sample(&image), RED, 2), "the body remains visible through the scaled hole");
    assert!(close(pixel(&image, W, probe.0, probe.1), GREEN, 2), "the foreground still hides around the hole");
    save("75-upscale-synthetic-foreground-hole", W, H, &image);
    // Half-alpha is exactly the opaque threshold used by both scaled paths.
    for texel in rgba[0].chunks_exact_mut(4) { texel.copy_from_slice(&[0, 255, 0, 128]); }
    frame.scenery = Some(scaled_scenery(&tiles, &rgba, canvas()));
    assert!(close(sample(&draw(&mut gpu, &frame)), GREEN, 2), "alpha 128 hides the body");
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_upscale_flat_and_depth_paths_preserve_every_physical_texel() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let (tiles, _, canvas) = gradient_picture();
    let mut rgba = Vec::new();
    for tile in &tiles {
        let mut pixels = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64usize {
            for x in 0..64usize {
                let fx = (tile.rect.min_x - canvas.min_x) as usize * 4 + x;
                let fy = (tile.rect.min_y - canvas.min_y) as usize * 4 + y;
                pixels.extend_from_slice(&[fx as u8, fy as u8, ((fx + fy) / 2) as u8, 255]);
            }
        }
        rgba.push(pixels);
    }
    let pixels: Vec<_> = rgba.iter().map(|rgba| TilePixels { side: 64, rgba }).collect();
    let raster = compose_tile_pixels(&tiles, &pixels, canvas, 4).expect("the 4x composition");
    assert_eq!((raster.width, raster.height), (256, 192));
    assert_eq!(raster.logical_bounds, canvas);
    let picture = Backdrop::new(raster.width, raster.height, raster.rgba, [-32.0, -24.0], [32.0, 24.0]).unwrap();
    let mut frame = off_screen();
    frame.scenery = Some(scaled_scenery(&tiles, &rgba, canvas));
    assert_geometry(&scenery_of(&tiles, canvas), frame.scenery.as_ref().unwrap());
    let scene = Scene { bounds: Bounds { min: Pos::new(-32.0, -24.0, 0.0), max: Pos::new(32.0, 24.0, 0.0) },
        triangles: &[], players: &[], fixture_banner: false, backdrop: Some(&picture) };
    // Four pixels per field unit, one output pixel per replacement texel, plus the fit margin.
    let (w, h) = (304, 240);
    let tiled = gpu.render_field(w, h, &scene, Some(&frame), &DrawList::new());
    let state = gpu.scenery_state();
    assert_eq!(tiled, gpu.render_field(w, h, &scene, Some(&frame), &DrawList::new()));
    assert_eq!(state, gpu.scenery_state(), "an unchanged frame reuses its GPU atlas");
    frame.scenery = None;
    let flat = gpu.render_field(w, h, &scene, Some(&frame), &DrawList::new());
    for (index, (a, b)) in tiled.chunks_exact(4).zip(flat.chunks_exact(4)).enumerate() {
        assert!(a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= 1), "flat/depth pixel {index}: {a:?} / {b:?}");
    }
    // Read all physical texels against the independent source equation. A native-sized copy
    // stretched to 4x would pass a self-comparison but fails these quarter-unit samples.
    for y in 0..192u32 {
        for x in 0..256u32 {
            assert!(close(pixel(&tiled, w, x + 24, y + 24), [x as u8, y as u8, ((x + y) / 2) as u8], 1), "replacement texel ({x},{y})");
        }
    }
    save("75-upscale-synthetic-physical-texels", w, h, &tiled);
}

fn owned_pack() -> Option<PngFieldPack> {
    let Some(root) = std::env::var_os("FMV_BACKGROUND_TEST_DIR").map(PathBuf::from) else {
        eprintln!("FMV_BACKGROUND_TEST_DIR is needed for the private nmkin_1 comparison; skipping");
        return None;
    };
    Some(PngFieldPack::open(&root).expect("the explicitly supplied private background pack"))
}

fn scenery(view: &FieldView) -> &Scenery {
    view.stage.as_ref().and_then(|stage| stage.scenery.as_deref()).expect("the field has depth scenery")
}

fn ids(view: &FieldView) -> (u64, u64) {
    (view.backdrop.as_ref().expect("the field backdrop").id, scenery(view).id)
}

fn field_scene(view: &FieldView) -> Scene<'_> {
    Scene { bounds: view.bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: view.backdrop.as_ref() }
}

fn cloud(pos: Pos, forward: [f32; 3]) -> AvatarPlayer<'static> {
    AvatarPlayer { who: Who(1), pos, facing: real::toward_camera(forward.map(f64::from)), anim: AnimState::Idle,
        scene_frame: None, scene_clip: None, scene_return: None, appearance: Some("ff7/cloud"), npc: None,
        tint: [90, 125, 245], own: true, party: false }
}

/// Measure the actual character twice against matching baselines. Comparing a depth render to
/// a flat baseline would mistake different background filtering for an occluded body pixel.
fn rail_picture(gpu: &mut Offscreen, avatars: &mut Avatars, view: &FieldView, player: AvatarPlayer<'_>, right: [f32; 3])
    -> (Vec<u8>, Vec<Vec<[f32; 3]>>, usize, usize) {
    let (w, h) = (1100, 680);
    let stage = view.stage.as_ref().unwrap();
    let scene = field_scene(view);
    let mut frame = avatars.frame(view, stage, &[player], 0.0).frame;
    assert_eq!(frame.models.len(), 1, "Cloud comes from the owned character archive");
    frame.ground.clear();
    let geometry = frame.models.iter().map(|model| model.positions.clone()).collect();
    let tiled = gpu.render_field(w, h, &scene, Some(&frame), &DrawList::new());
    frame.scenery = None;
    let flat = gpu.render_field(w, h, &scene, Some(&frame), &DrawList::new());
    let bare_flat = gpu.render_field(w, h, &scene, None, &DrawList::new());
    let away = AvatarPlayer { pos: Pos::new(player.pos.x + 30_000.0 * right[0], player.pos.y + 30_000.0 * right[1],
        player.pos.z + 30_000.0 * right[2]), ..player };
    let mut away = avatars.frame(view, stage, &[away], 0.0).frame;
    away.ground.clear();
    assert_eq!(away.models.len(), 1, "an offscreen model keeps the tiled render path active");
    let bare_tiled = gpu.render_field(w, h, &scene, Some(&away), &DrawList::new());
    let different = |a: &[u8], b: &[u8], tolerance| a[..3].iter().zip(&b[..3]).any(|(x, y)| x.abs_diff(*y) > tolerance);
    let (mut visible, mut hidden) = (0, 0);
    for (((a, b), c), d) in flat.chunks_exact(4).zip(bare_flat.chunks_exact(4)).zip(tiled.chunks_exact(4)).zip(bare_tiled.chunks_exact(4)) {
        if different(a, b, 12) {
            if different(c, d, 12) { visible += 1; }
            else { hidden += 1; }
        }
    }
    (tiled, geometry, visible, hidden)
}

#[test]
#[ignore = "needs original FF7 data, FMV_BACKGROUND_TEST_DIR and a GPU or software Vulkan/GL driver"]
fn offscreen_upscale_owned_nmkin_1_preserves_rails_models_and_cached_palette_fallback() {
    use crate::scene::SceneBackground;
    use fmv_adapter_api::AreaRuntime;
    use fmv_ff7::background::{BackgroundOp, PaletteOp};
    use fmv_ff7::script::{parse_model_list, FieldScript};

    let (Some(real), Some(pack)) = (real::open(), owned_pack()) else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let area = real.adapter.load_field("nmkin_1").expect("the owned reactor field");
    let field = area.field_file();
    // These are model doors, not background animation groups. Preserve that source contract:
    // replacement pages must not introduce invented door masks or bake the moving models in.
    let background = field.background().unwrap();
    assert_eq!(background.tiles.len(), 1025);
    assert!(background.tiles.iter().all(|tile| tile.param == 0 && tile.state == 0));
    let script = FieldScript::from_field(field).unwrap();
    let models = parse_model_list(field.section(fmv_ff7::field::section::MODEL_LOADER).unwrap()).unwrap();
    let inits = script.entity_inits(Some(&models));
    let loader = field.model_loader().unwrap();
    for (entity, hrc, offset, movement) in [(5, "byba", 0x0633, [150, 0, -22]), (6, "bybf", 0x06B6, [0, 184, 0]),
        (7, "bycd", 0x0756, [-184, 0, 0]), (8, "bycd", 0x07C2, [154, 0, 0])] {
        let init = inits.iter().find(|init| init.entity == entity).unwrap();
        assert_eq!(loader.models[init.model_index.unwrap()].hrc_stem(), hrc);
        let code = script.disassemble_partial(entity, 3).unwrap();
        assert!(code.error.is_none(), "door {entity}: {:?}", code.error);
        let motion = code.instructions.iter().find(|ins| ins.offset == offset).expect("the authored door motion");
        assert_eq!(motion.mnemonic, "OFST");
        assert_eq!([motion.literal("x"), motion.literal("y"), motion.literal("z")], movement.map(Some));
        assert_eq!((motion.literal("move_type"), motion.literal("speed")), (Some(1), Some(30)));
        assert!(code.instructions.iter().all(|ins| !matches!(ins.mnemonic, "BGON" | "BGOFF" | "BGCLR" | "BGROL" | "BGROL2")));
    }
    let mut native = FieldView::ff7_with_pack(&real.adapter, "nmkin_1", true, None).unwrap();
    let mut scaled = FieldView::ff7_with_pack(&real.adapter, "nmkin_1", true, Some(&pack)).unwrap();
    // Before the window hands over a device, the conservative cap keeps both paths
    // native. An artificially small GPU must never accept only one replacement texture.
    scaled.set_background_texture_limit(2048).unwrap();
    assert_eq!(scaled.backdrop.as_ref().unwrap().rgba, native.backdrop.as_ref().unwrap().rgba,
        "a 2048 texture cap selects the entire native picture");
    assert_eq!(scenery(&scaled).atlas, scenery(&native).atlas,
        "the same cap also selects the entire native depth atlas");
    scaled.set_background_texture_limit(gpu.max_texture_dimension_2d()).unwrap();
    let a = native.backdrop.as_ref().unwrap();
    let b = scaled.backdrop.as_ref().unwrap();
    assert_eq!((b.width, b.height), (a.width * 4, a.height * 4), "the provided pack is active, not a silent fallback");
    assert_eq!((a.min, a.max), (b.min, b.max), "field-screen dimensions stay native");
    assert_eq!((native.bounds, native.whole, native.range), (scaled.bounds, scaled.whole, scaled.range));
    assert_geometry(scenery(&native), scenery(&scaled));
    let axes = area.cameras().unwrap()[0].axes_f32();
    let triangles = area.walk_triangles();
    let mut avatars = real::avatars(&real);
    let mut totals = [(0usize, 0usize); 2];
    let initial_ids = [ids(&native), ids(&scaled)];
    // Authored walkmesh triangles 20/23 lie behind the lower foreground rail (layer-2 tiles
    // 306/291, depth 4). The feet are covered while the head clears it; use original centroids.
    for index in [20usize, 23] {
        let t = triangles[index];
        let pos = Pos::new((t[0].x + t[1].x + t[2].x) / 3.0, (t[0].y + t[1].y + t[2].y) / 3.0,
            (t[0].z + t[1].z + t[2].z) / 3.0);
        let mut previous_geometry = None;
        for (which, (name, view)) in [("native", &native), ("syw4x", &scaled)].into_iter().enumerate() {
            let (image, geometry, visible, hidden) = rail_picture(&mut gpu, &mut avatars, view, cloud(pos, axes[2]), axes[0]);
            assert!(visible + hidden > 20, "{name} triangle {index}: Cloud has a measurable silhouette");
            if let Some(previous) = &previous_geometry { assert_eq!(previous, &geometry, "texture resolution cannot move model vertices"); }
            previous_geometry = Some(geometry);
            totals[which].0 += visible;
            totals[which].1 += hidden;
            eprintln!("nmkin_1 {name}, triangle {index}: {visible} visible, {hidden} hidden character pixels");
            save(&format!("75-nmkin_1-{name}-rail-{index}"), 1100, 680, &image);
        }
    }
    for (which, (visible, hidden)) in totals.into_iter().enumerate() {
        assert!(visible > 20 && hidden > 5, "path {which}: the authored foreground partially hides Cloud ({visible} visible, {hidden} hidden)");
    }
    assert_eq!([ids(&native), ids(&scaled)], initial_ids, "drawing models does not replace background resources");

    // A visible palette upload selects native pixels for the ENTIRE frame, then restoring the
    // original palette retrieves the cached replacement and native frame identities. Scratch
    // work alone must keep both GPU resource identities unchanged.
    let event = |op| SceneBackground { field: "nmkin_1".into(), op: BackgroundOp::Palette(op) };
    for view in [&mut native, &mut scaled] {
        view.apply_background_events([event(PaletteOp::Store { palette: 0, position: 0, count: 256 })]);
    }
    assert_eq!([ids(&native), ids(&scaled)], initial_ids);
    for view in [&mut native, &mut scaled] {
        view.apply_background_events([event(PaletteOp::Load { position: 16, palette: 0, count: 256 })]);
    }
    assert_ne!([ids(&native), ids(&scaled)], initial_ids, "the visible palette changed");
    let a = native.backdrop.as_ref().unwrap();
    let b = scaled.backdrop.as_ref().unwrap();
    assert_eq!((a.width, a.height, &a.rgba), (b.width, b.height, &b.rgba), "flat paths fall back together");
    assert_eq!(scenery(&native).atlas, scenery(&scaled).atlas, "depth paths use the same whole native fallback");
    assert_geometry(scenery(&native), scenery(&scaled));
    for view in [&mut native, &mut scaled] {
        view.apply_background_events([event(PaletteOp::Load { position: 0, palette: 0, count: 256 })]);
    }
    assert_eq!([ids(&native), ids(&scaled)], initial_ids, "returning to the cached selection reuses both resource identities");
    assert_eq!(scaled.backdrop.as_ref().unwrap().width, native.backdrop.as_ref().unwrap().width * 4);
    eprintln!("nmkin_1 door source offsets verified; door ScenePlayer playback pictures are a separate check, not covered here");
}
