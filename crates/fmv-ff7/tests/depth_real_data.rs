//! Checks of the per-tile depth rule (`fmv_ff7::background::depth`) against the owner's REAL FF7 data
//! (FF7 Steam Edition, English; verified 2026-11-08).
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` is unset, so a normal
//! `cargo test` never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test --release -p fmv-ff7 --test depth_real_data -j16 -- --ignored --nocapture
//! ```
//!
//! The calibration itself lives in `examples/depth_survey.rs` (engine constants and walkmesh
//! geometry over all 702 fields) and `examples/depth_composite.rs` (Cloud in `md1stin` with the flat
//! front layer and with per-tile depth); this file includes both as modules and asserts robust
//! aggregates of what they measure. The files are only opened read-only and nothing from them is
//! stored or printed beyond counts and ratios.

// `chunks_exact(n)` with a constant n would be `as_chunks` on Rust 1.88+; the workspace keeps its
// MSRV lower (see `src/lgp.rs`), so the lint is allowed here.
#![allow(clippy::chunks_exact_to_as_chunks)]

#[allow(dead_code)]
#[path = "../examples/depth_composite.rs"]
mod composite;
#[allow(dead_code)]
#[path = "../examples/depth_survey.rs"]
mod survey;

use fmv_ff7::background::depth::{pack_atlas, LAYER1_DEPTH, LAYER3_DEPTH, LAYER4_DEPTH};
use fmv_ff7::background::{
    decode_background, decode_background_tiles_with_depth, depth_of_id, depth_of_point, tile_hides_point, DepthTile,
    Projection,
};
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn data_dir() -> Option<PathBuf> {
    match std::env::var_os("FMV_FF7_DATA") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => {
            eprintln!("FMV_FF7_DATA is not set; skipping");
            None
        }
    }
}

fn open_flevel(dir: &std::path::Path) -> LgpArchive<std::fs::File> {
    LgpArchive::open_path(dir.join("field").join("flevel.lgp")).unwrap()
}

fn load(lgp: &mut LgpArchive<std::fs::File>, name: &str) -> FieldFile {
    FieldFile::parse(lzss::decompress(&lgp.read(name).unwrap()).unwrap()).unwrap()
}

/// The tiles painted in their order over an empty canvas of the size of `decode_background`'s:
/// the same arithmetic as the compositor.
fn paint(tiles: &[DepthTile], width: usize, height: usize, origin: (i32, i32)) -> Vec<u8> {
    let mut canvas = vec![0u8; width * height * 4];
    for t in tiles {
        let size = t.size() as i32;
        for y in 0..size {
            for x in 0..size {
                let px = t.pixel(x as u32, y as u32).unwrap();
                if px[3] == 0 {
                    continue;
                }
                let (cx, cy) = (t.rect.min_x + x + origin.0, t.rect.min_y + y + origin.1);
                assert!(cx >= 0 && cy >= 0 && (cx as usize) < width && (cy as usize) < height);
                let at = (cy as usize * width + cx as usize) * 4;
                let back = if canvas[at + 3] != 0 { [canvas[at], canvas[at + 1], canvas[at + 2]] } else { [0, 0, 0] };
                let out = match t.blend {
                    Some(mode) => [mode.apply(back[0], px[0]), mode.apply(back[1], px[1]), mode.apply(back[2], px[2])],
                    None => [px[0], px[1], px[2]],
                };
                canvas[at..at + 4].copy_from_slice(&[out[0], out[1], out[2], 255]);
            }
        }
    }
    canvas
}

/// All 702 fields: the depth tiles are exactly the tiles `decode_background` draws, in its order
/// (painting them reproduces its picture bit for bit, blended tiles included), every depth follows
/// the rule, and the atlas of every field holds all its tiles.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn depth_tiles_reproduce_the_flat_background_in_every_field() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = open_flevel(&dir);
    let names = survey::field_names(&lgp);
    assert_eq!(names.len(), 702);
    let (mut total_tiles, mut blended_fields, mut compared) = (0usize, 0usize, 0usize);
    for name in &names {
        let field = load(&mut lgp, name);
        let image = decode_background(&field).unwrap();
        let tiles = decode_background_tiles_with_depth(&field).unwrap();
        total_tiles += tiles.len();
        if tiles.iter().any(|t| t.blend.is_some()) {
            blended_fields += 1;
        }
        for t in &tiles {
            let want = match t.layer {
                1 => LAYER1_DEPTH,
                3 => LAYER3_DEPTH,
                4 => LAYER4_DEPTH,
                _ => depth_of_id(t.id),
            };
            assert_eq!(t.depth, want, "{name}: layer {} id {}", t.layer, t.id);
            assert!(t.depth >= 0.0 && t.depth <= LAYER3_DEPTH);
            assert_eq!(t.rgba.len(), (t.size() * t.size() * 4) as usize);
        }
        let painted = paint(&tiles, image.width as usize, image.height as usize, (image.origin_x, image.origin_y));
        assert_eq!(painted, image.rgba, "{name}: painting the depth tiles in order must give decode_background's picture");
        compared += 1;
        let atlas = pack_atlas(&tiles, 2048).unwrap_or_else(|| panic!("{name}: no atlas"));
        assert_eq!(atlas.rects.len(), tiles.len());
        assert!(atlas.width <= 2048 && atlas.height <= 8192);
    }
    eprintln!("{compared} fields compared, {total_tiles} depth tiles, {blended_fields} fields with blended tiles");
    assert_eq!(compared, 702);
    assert!(total_tiles > 200_000, "{total_tiles} tiles");
    assert!(blended_fields > 50, "{blended_fields} fields have blended tiles, so the blended path is exercised");
}

/// `md1stin`: six layer-2 ids with the depths the rule gives, one projection for tiles and points.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn md1stin_depth_tiles_and_the_walkmesh_share_one_depth() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = open_flevel(&dir);
    let field = load(&mut lgp, "md1stin");
    let tiles = decode_background_tiles_with_depth(&field).unwrap();
    let layer2: Vec<&DepthTile> = tiles.iter().filter(|t| t.layer == 2).collect();
    assert_eq!(layer2.len(), 320);
    let ids: BTreeSet<u16> = layer2.iter().map(|t| t.id).collect();
    assert_eq!(ids, BTreeSet::from([1, 152, 294, 544, 4087, 4088]));
    let depths: BTreeSet<i32> = layer2.iter().map(|t| t.depth as i32).collect();
    assert_eq!(depths, BTreeSet::from([4, 608, 1176, 2176, 16_348, 16_352]));
    assert!(tiles.iter().all(|t| t.layer != 4), "md1stin has no layer 4");
    assert!(tiles.iter().all(|t| t.blend.is_none()), "md1stin has no blended tiles");

    // the walkmesh in the same units: from 275 to 2385 in front of the camera, so the left wall
    // piece (id 544, depth 2176) has walkable points both nearer and farther than it
    let camera = field.cameras().unwrap()[0];
    let projection = Projection::from_camera(&camera);
    let walk = field.walkmesh().unwrap();
    let vertex_depths: Vec<f32> = walk
        .triangles
        .iter()
        .flatten()
        .filter_map(|v| depth_of_point(&projection, [f32::from(v.x), f32::from(v.y), f32::from(v.z)]))
        .collect();
    let (near, far) = (vertex_depths.iter().copied().fold(f32::MAX, f32::min), vertex_depths.iter().copied().fold(0.0, f32::max));
    assert!((274.0..276.0).contains(&near) && (2384.0..2386.0).contains(&far), "walkmesh depth {near}..{far}");
    let wall = depth_of_id(544);
    assert!(tile_hides_point(wall, far), "the farthest walkmesh vertex is behind the wall piece");
    assert!(!tile_hides_point(wall, near), "the nearest is in front of it");
    // it is the same camera depth the survey measures: the layer-2 depths sit inside 0.2x..8x the walkmesh range
    assert!(layer2.iter().filter(|t| t.id < 4000 && t.id > 12).all(|t| t.depth > near * 0.5 && t.depth < far * 1.5));

    // the atlas holds every tile pixel
    let atlas = pack_atlas(&tiles, 2048).unwrap();
    for (t, r) in tiles.iter().zip(&atlas.rects) {
        for (x, y) in [(0, 0), (5, 7), (t.size() - 1, t.size() - 1)] {
            let at = (((r.y + y) * atlas.width + r.x + x) * 4) as usize;
            assert_eq!(atlas.rgba[at..at + 4], t.pixel(x, y).unwrap());
        }
    }
}

/// The survey over all 702 fields: the engine constants and the walkmesh geometry both give the
/// rule `depth = 4 * id`.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_rule_holds_over_all_fields() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = open_flevel(&dir);
    let names = survey::field_names(&lgp);
    let totals = survey::survey_fields(&mut lgp, &names, false);
    survey::print_totals(&totals);
    assert_eq!(totals.fields, 702);

    // 1. IDBig is the Direct3D z-buffer value of a point at depth 4 * id: near plane 50 (and 512 in
    // some fields), far plane about 32 000, residuals of 1e-7..1e-5
    let fits = totals.fits.len();
    assert!(fits >= 440, "{fits} fields with at least four usable ids");
    let near50 = totals.fits_with_near(50.0, 0.5);
    assert!(near50 >= 390, "{near50} fields fit the engine's near plane of 50");
    assert!(totals.fits_with_near(512.0, 1.0) >= 20, "the fields with the 512 near plane");
    assert!(totals.fits_with_rms_below(1e-4) >= 400, "{} fits below 1e-4", totals.fits_with_rms_below(1e-4));
    let far_ok = totals.fits.iter().filter(|f| f.3 < 1e-4 && (f.2 - 32_000.0).abs() < 1_500.0).count();
    assert!(far_ok >= 300, "{far_ok} fits give a far plane within 1 500 of 32 000");

    // 2. the objects stand where their depth says: the floor just below the base of a layer-2 object is
    // 1.0 times 4 * id (median), within 0.8..1.25 for most
    let objects = totals.ratio_samples.len();
    assert!(objects >= 3500, "{objects} objects with floor below their base");
    let median = totals.contact_ratio_quantile(0.5).unwrap();
    assert!((0.95..=1.05).contains(&median), "median contact ratio {median}");
    assert!(totals.contact_ratio_quantile(0.25).unwrap() >= 0.9);
    assert!(totals.contact_ratio_quantile(0.75).unwrap() <= 1.15);
    let (in_band, all) = totals.contact_ratio_band(0.8, 1.25);
    assert!(in_band * 100 >= all * 75, "{in_band} of {all} objects within 0.8..1.25");

    // 3. and only the scale 4 puts them there: the floor under objects is farther than the object and the
    // floor below their bases nearer, and the two statements flip between k = 3, 4 and 5
    let share = |t: (u64, u64)| t.0 as f64 / t.1.max(1) as f64;
    let (k3, k4, k5) = (totals.scale_stats(3.0).unwrap(), totals.scale_stats(4.0).unwrap(), totals.scale_stats(5.0).unwrap());
    assert!(share(k4.behind) > 0.6 && share(k5.behind) < 0.35, "behind: k4 {:.2} k5 {:.2}", share(k4.behind), share(k5.behind));
    assert!(share(k3.front) < 0.2 && share(k4.front) > 0.6, "front: k3 {:.2} k4 {:.2}", share(k3.front), share(k4.front));
    let balanced = |s: &survey::ScaleStats| (share(s.objects_behind) + share(s.objects_front)) / 2.0;
    for k in [1.0, 2.0, 3.0, 5.0, 6.0, 8.0, 16.0] {
        assert!(balanced(k4) > balanced(totals.scale_stats(k).unwrap()) + 0.1, "k = 4 is the best scale, not {k}");
    }

    // 4. layer 1 (depth 16 380) is behind every walkable point of every field
    assert!(totals.max_walk_depth.0 < LAYER1_DEPTH, "deepest walkmesh vertex {:?}", totals.max_walk_depth);
    assert!(totals.fields_deeper_than_layer1.is_empty());
}

/// `md1stin` with Cloud: the flat front layer hides him where he stands in front of the left wall
/// piece, per-tile depth does not, and he is hidden where he stands genuinely behind it.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn cloud_in_md1stin_is_hidden_only_where_he_is_behind() {
    let Some(dir) = data_dir() else { return };
    let (scene, cloud) = composite::prepare(&dir, "md1stin").unwrap();
    let f = composite::find_spots(&scene, &cloud);
    composite::print_findings(&scene, &f);
    let wall = composite::WALL;

    // the client's own figures are reproduced
    assert_eq!((f.client_places, f.client_touched), (73, 59));
    assert_eq!(f.client_blame.first(), Some(&(wall, 23)), "the wall piece is the dominant object at 23 places");
    // and the counter-example: the flat layer hides about a third of Cloud (mostly behind the wall
    // piece), per-tile depth hides none of him
    let pick = f.client_pick.as_ref().expect("a counter-example spot");
    assert!(pick.verdict.depth < 4.0 * 544.0);
    assert!(pick.verdict.flat_hidden > 500 && pick.verdict.flat(wall) > 500, "{:?}", pick.verdict);
    assert_eq!(pick.verdict.depth_hidden, 0, "Cloud stands in front of every object that overlaps him: {:?}", pick.verdict);
    // over the client's 73 places the flat layer hides 44 853 Cloud pixels and the rule 15 631
    assert!(f.client_pixels.2 * 2 < f.client_pixels.1, "{:?}", f.client_pixels);

    // the wall piece does hide Cloud where he is farther than 2176 (the walkmesh reaches depth 2385),
    // at spots linked to a spawn point's triangle, inside the camera range
    assert!(f.deepest_sample_depth > 2176.0 && f.deepest_sample_depth < 2400.0, "{}", f.deepest_sample_depth);
    assert!(f.hiders.get(&wall).copied().unwrap_or(0) > 100, "{:?}", f.hiders);
    let spot = |name: &str| f.spots.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no spot {name}"));
    let behind = spot("behind_wall_544_hidden");
    assert!(behind.triangle.is_some_and(|t| scene.reachable_triangle(t)));
    assert!(behind.verdict.depth > 2176.0);
    assert!(behind.verdict.hidden(wall) * 10 >= behind.verdict.pixels * 9, "{:?}", behind.verdict);
    assert_eq!(behind.verdict.hidden(wall), behind.verdict.flat(wall), "behind it the flat layer and the rule agree");
    let partly = spot("behind_wall_544_partly");
    assert!(partly.verdict.depth > 2176.0);
    let fraction = partly.verdict.hidden(wall) as f32 / partly.verdict.pixels as f32;
    assert!((0.25..=0.9).contains(&fraction), "partly hidden: {fraction}");
    // in front of it the flat layer hides him and the rule does not
    let front = spot("in_front_of_wall_544");
    assert!(front.verdict.depth < 2176.0 && (450.0..=1300.0).contains(&front.verdict.depth));
    assert!(front.verdict.flat(wall) > 1000, "{:?}", front.verdict);
    assert_eq!(front.verdict.hidden(wall), 0);
    assert_eq!(front.verdict.depth_hidden, 0);

    // the other objects hide him too (posts, train front, train): the layering is not just one object
    for o in [(2, 1), (2, 152), (2, 294), wall] {
        assert!(f.hiders.get(&o).copied().unwrap_or(0) > 50, "object {o:?} hides Cloud somewhere: {:?}", f.hiders);
    }
    // most of what the flat layer hides of Cloud is freed by the rule
    assert!(f.freed_pixels * 10 >= f.flat_pixels * 6, "{} of {} freed", f.freed_pixels, f.flat_pixels);
    // the spawn points are clear of the front layer
    assert!(f.spawns.iter().all(|s| s.verdict.flat_hidden == 0));
}
