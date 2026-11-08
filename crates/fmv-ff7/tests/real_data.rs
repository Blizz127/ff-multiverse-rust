//! Checks against the owner's REAL FF7 data (FF7 Steam Edition, English; verified 2026-11-08).
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` is unset, so a normal
//! `cargo test` never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-ff7 --test real_data -j8 -- --ignored
//! ```
//!
//! `FMV_FF7_DATA` is the game's `data/` directory (the one containing `field/flevel.lgp` and
//! `field/char.lgp`). The files are only ever opened read-only, and the assertions record
//! counts and sizes: no game bytes are stored in this repository or printed.

// `chunks_exact(n)` with a constant n would be `as_chunks` on Rust 1.88+; the workspace keeps its
// MSRV lower (see `src/lgp.rs`), so the lint is allowed here.
#![allow(clippy::chunks_exact_to_as_chunks)]

use fmv_adapter_api::{AreaRuntime, GameAdapter, MoveResult};
use fmv_core::{EditionProvenance, GameId, Platform, Pos};
use fmv_ff7::anim::FieldAnimation;
use fmv_ff7::background::{decode_background, decode_background_layers, decode_background_with, BackgroundOptions, GroupFilter, Projection};
use fmv_ff7::field::{FieldFile, NO_ACCESS};
use fmv_ff7::hrc::Hrc;
use fmv_ff7::lgp::{LgpArchive, MAX_ENTRY_LEN};
use fmv_ff7::model::{load_field_animation, load_field_model, mat4_identity, model_to_field, transform_point, transform_vector};
use fmv_ff7::pmodel::PModel;
use fmv_ff7::rsd::Rsd;
use fmv_ff7::tex::parse_tex;
use fmv_ff7::{lzss, resolve_appearance, Ff7Adapter};
use std::collections::{BTreeMap, HashSet};
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

fn flevel(dir: &std::path::Path) -> PathBuf {
    dir.join("field").join("flevel.lgp")
}

/// Field entries are the ones whose names have no '.', excluding the name list. The Steam
/// archive also holds `*.tex`, `*.tut` and the console-port `*.NX` / `*.XOne` extras.
fn is_field_name(name: &str) -> bool {
    !name.contains('.') && !name.eq_ignore_ascii_case("maplist")
}

fn load_field_file(lgp: &mut LgpArchive<std::fs::File>, name: &str) -> FieldFile {
    let stored = lgp.read(name).unwrap();
    FieldFile::parse(lzss::decompress(&stored).unwrap()).unwrap()
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn flevel_archive_structure() {
    let Some(dir) = data_dir() else { return };
    let path = flevel(&dir);
    let mut lgp = LgpArchive::open_path(&path).unwrap();
    assert_eq!(lgp.len(), 753);
    assert_eq!(lgp.archive_len(), 131_431_170);
    assert!(lgp.list().iter().all(|e| e.check_code == 0x0E && e.path_index == 0));
    let fields = lgp.list().iter().filter(|e| is_field_name(&e.name)).count();
    assert_eq!(fields, 702);
    // md1stin: 201 335 stored bytes, LZSS header = stored - 4, 414 915 bytes once expanded
    let stored = lgp.read("md1stin").unwrap();
    assert_eq!(stored.len(), 201_335);
    assert_eq!(u32::from_le_bytes(stored[..4].try_into().unwrap()), 201_331);
    let expanded = lzss::decompress(&stored).unwrap();
    assert_eq!(expanded.len(), 414_915);
    // the nine section lengths + the 42-byte header + nine length fields tile the file exactly
    let field = FieldFile::parse(expanded).unwrap();
    let declared = [18_944u32, 38, 1_314, 6_668, 1_174, 42, 48, 740, 385_869];
    for (i, want) in declared.iter().enumerate() {
        assert_eq!(field.declared_section_len(i), Some(*want), "section {i}");
        assert_eq!(field.section(i).unwrap().len(), *want as usize);
    }
    assert_eq!(declared.iter().sum::<u32>() as usize + 42 + 9 * 4, field.bytes().len());
    // the archive is read-only data: opening it again still works and nothing was modified
    assert!(LgpArchive::open_path(&path).is_ok());
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn md1stin_decodes_and_the_walkmesh_lines_up_with_the_background() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = LgpArchive::open_path(flevel(&dir)).unwrap();
    let field = load_field_file(&mut lgp, "md1stin");

    let walk = field.walkmesh().unwrap();
    assert_eq!(walk.len(), 39);
    let all = || walk.triangles.iter().flatten();
    assert_eq!(all().map(|v| v.x).min(), Some(3490));
    assert_eq!(all().map(|v| v.x).max(), Some(4055));
    assert_eq!(all().map(|v| v.y).min(), Some(27_244));
    assert_eq!(all().map(|v| v.y).max(), Some(29_405));
    assert_eq!(all().map(|v| v.z).min(), Some(310));
    assert_eq!(all().map(|v| v.z).max(), Some(432));
    assert_eq!(walk.access.iter().flatten().filter(|&&e| e == NO_ACCESS).count(), 41);

    let cameras = field.cameras().unwrap();
    assert_eq!(cameras.len(), 1);
    assert_eq!((cameras[0].position, cameras[0].zoom), ([-365, 3279, -27_031], 386));

    let triggers = field.triggers().unwrap();
    assert_eq!(triggers.name, "md1stin");
    assert_eq!(triggers.camera_range.bounds(), (-256, -256, 256, 256));
    let gateways: Vec<_> = triggers.used_gateways().collect();
    assert_eq!(gateways.len(), 1);
    assert_eq!(gateways[0].target_field_id, 117); // `md1_1` in the archive's maplist

    let palette = field.palette().unwrap();
    assert_eq!((palette.page_count(), palette.colors_per_page), (13, 256));
    let bg = field.background().unwrap();
    // md1stin is one of the 121 fields whose declared length includes the 14-byte `FINAL FANTASY7`
    // string after the `END` tag
    assert!(bg.texture_error.is_none() && bg.end_tag && bg.trailing_bytes == 14);
    assert_eq!(bg.tiles.len(), 1_115);
    assert_eq!(bg.discarded_tiles, 0);
    let slots: Vec<u8> = bg.textures.iter().flatten().map(|t| t.slot).collect();
    assert_eq!(slots, [0, 1, 2, 3, 4]);

    let image = decode_background(&field).unwrap();
    assert_eq!((image.width, image.height, image.origin_x, image.origin_y), (512, 512, 256, 256));
    assert_eq!(image.view_bounds.width(), 512);
    let drawn: Vec<usize> = image.layers.iter().map(|l| l.tiles_drawn).collect();
    assert_eq!(drawn, [795, 320]);
    assert!(image.layers.iter().all(|l| l.tiles_unrenderable == 0 && l.tiles_hidden == 0));
    // every pixel of layer 1 is covered or transparent-by-index: the picture is mostly opaque
    let opaque = image.rgba.chunks_exact(4).filter(|p| p[3] == 255).count();
    assert!(opaque > 512 * 512 * 9 / 10, "{opaque}");

    // The camera puts the walkmesh on the platform: 115 of the 117 vertex references (all but
    // one corner at the near left, which lands at x = -275, 19 units past the left edge of the
    // 512-wide background) project inside the camera range, and a larger world y (further
    // along the platform) is higher on screen.
    let proj = Projection::from_camera(&cameras[0]);
    let bounds = image.view_bounds;
    let (mut ys, mut inside) = (Vec::new(), 0);
    for v in all() {
        let p = proj.project_i16([v.x, v.y, v.z]).expect("in front of the camera");
        assert!(p.depth > 0.0);
        if p.x >= bounds.min_x as f32 && p.x < bounds.max_x as f32 && p.y >= bounds.min_y as f32 && p.y < bounds.max_y as f32 {
            inside += 1;
        }
        ys.push((v.y, p.y));
    }
    assert_eq!((inside, ys.len()), (115, 117));
    let far = ys.iter().max_by_key(|(y, _)| *y).unwrap().1;
    let near = ys.iter().min_by_key(|(y, _)| *y).unwrap().1;
    assert!(far < near, "larger y is further away, so higher on screen: far {far} near {near}");
    let exit = proj.project_i16(gateways[0].exit_a).unwrap();
    assert!(exit.x >= bounds.min_x as f32 && exit.x < bounds.max_x as f32);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn every_field_entry_decodes_with_the_recorded_totals() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = LgpArchive::open_path(flevel(&dir)).unwrap();
    let names: Vec<String> = lgp.list().iter().map(|e| e.name.clone()).collect();
    let (mut fields, mut triangles, mut tiles, mut discarded) = (0usize, 0usize, 0usize, 0usize);
    let (mut unrenderable, mut unrenderable_all_groups, mut hidden_by_default) = (0usize, 0usize, 0usize);
    let mut scales: BTreeMap<u16, usize> = BTreeMap::new();
    let (mut zero_page_palettes, mut trailer, mut gateways_in_use) = (0usize, 0usize, 0usize);
    for (i, name) in names.iter().enumerate() {
        if !is_field_name(name) {
            continue;
        }
        fields += 1;
        let stored = lgp.read_index(i).unwrap();
        let field = FieldFile::parse(lzss::decompress(&stored).unwrap()).unwrap();
        let declared: usize = (0..9).map(|k| field.declared_section_len(k).unwrap() as usize).sum::<usize>() + 78;
        match field.bytes().len() - declared {
            0 => {}
            14 => {
                assert_eq!(&field.bytes()[declared..], b"FINAL FANTASY7", "{name}");
                trailer += 1;
            }
            other => panic!("{name}: {other} unexplained bytes after the last section"),
        }
        // field scale: the model-loader header and the script header (offset 8) carry the same value
        let scale = field.field_scale().unwrap();
        assert_eq!(u16::from_le_bytes(field.section(0).unwrap()[8..10].try_into().unwrap()), scale, "{name}");
        assert_eq!(field.model_loader_header().unwrap().scale, scale);
        *scales.entry(scale).or_default() += 1;
        let walk = field.walkmesh().unwrap();
        triangles += walk.len();
        for (t, acc) in walk.access.iter().enumerate() {
            for (e, &link) in acc.iter().enumerate() {
                if link == NO_ACCESS {
                    continue;
                }
                let target = usize::from(link);
                assert!(target < walk.len() && target != t, "{name}: bad link {t}.{e} -> {link}");
                // the link shares the two vertices of the edge it is stored under
                let (a, b) = (walk.triangles[t][e], walk.triangles[t][(e + 1) % 3]);
                assert!(walk.triangles[target].contains(&a) && walk.triangles[target].contains(&b), "{name}: link {t}.{e}");
            }
        }
        assert!(!field.cameras().unwrap().is_empty());
        let triggers = field.triggers().unwrap();
        gateways_in_use += triggers.used_gateways().count();
        if field.palette().unwrap().page_count() == 0 {
            zero_page_palettes += 1;
        }
        let bg = field.background().unwrap();
        assert!(bg.texture_error.is_none() && bg.end_tag, "{name}");
        tiles += bg.tiles.len();
        discarded += bg.discarded_tiles;
        let image = decode_background(&field).unwrap();
        unrenderable += image.layers.iter().map(|l| l.tiles_unrenderable).sum::<usize>();
        hidden_by_default += image.layers.iter().map(|l| l.tiles_hidden).sum::<usize>();
        assert!(image.width > 0 && image.height > 0 || bg.tiles.is_empty(), "{name}");
        let every_group = BackgroundOptions { groups: GroupFilter::All, ..BackgroundOptions::default() };
        let image = decode_background_with(&field, &every_group).unwrap();
        unrenderable_all_groups += image.layers.iter().map(|l| l.tiles_unrenderable).sum::<usize>();
    }
    assert_eq!(fields, 702);
    // ... and every value equals the FF7 Speedrun Wiki's per-field "Field Scale" list (checked once,
    // field by field through the maplist ids, 702 of 702)
    let expected: BTreeMap<u16, usize> = [
        (400, 4),
        (448, 1),
        (480, 2),
        (512, 643),
        (576, 5),
        (600, 5),
        (640, 2),
        (650, 2),
        (700, 6),
        (720, 1),
        (768, 12),
        (1024, 9),
        (2048, 7),
        (4096, 2),
        (5120, 1),
    ]
    .into();
    assert_eq!(scales, expected);
    assert_eq!(triangles, 81_542);
    assert_eq!(trailer, 582);
    assert_eq!(gateways_in_use, 1_036);
    assert_eq!(zero_page_palettes, 14);
    assert_eq!((tiles, discarded), (646_881, 650));
    // Only the blended tiles of `lastmap` (187, in an animation group) and `fr_e` (125, always
    // visible) name a palette page past the end of the palette section. With the default options
    // the `lastmap` group is not drawn at all, so only the `fr_e` ones are counted.
    assert_eq!((unrenderable, unrenderable_all_groups), (125, 312));
    // tiles in animation groups are the ones the default options leave out (about 19%)
    assert_eq!(hidden_by_default, 124_937, "animation-group tiles hidden by the default options");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn adapter_lists_and_loads_real_fields_and_validates_real_moves() {
    let Some(dir) = data_dir() else { return };
    let provenance = EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Pc,
        edition: "steam".into(),
        region: "any".into(),
        language: "en".into(),
        revision: "test".into(),
        sources: vec![],
        modded: false,
    };
    let adapter = Ff7Adapter::from_flevel_path(flevel(&dir), provenance).unwrap();
    let areas = adapter.list_areas();
    assert_eq!(areas.len(), 702, "skipped: {:?}", adapter.skipped_entries().len());
    assert!(areas.iter().all(|a| !a.fixture));
    assert!(areas.iter().any(|a| a.id.key == "md1stin"));
    assert!(!areas.iter().any(|a| a.id.key.contains('.') || a.id.key == "maplist"));

    let area = adapter.load_field("md1stin").unwrap();
    assert_eq!(area.walkmesh().len(), 39);
    // the speed follows the field scale: md1stin is 512 (the base), md1_1 is 1024 (twice as fast)
    assert_eq!((area.field_scale(), area.max_speed()), (512, fmv_ff7::FF7_FIELD_MAX_SPEED_PLACEHOLDER));
    let md1_1 = adapter.load_field("md1_1").unwrap();
    assert_eq!((md1_1.field_scale(), md1_1.max_speed()), (1024, 2.0 * fmv_ff7::FF7_FIELD_MAX_SPEED_PLACEHOLDER));
    let walk = area.walkmesh().clone();

    // ground-plane centroid of a triangle, at its own height
    let centroid = |t: usize| -> Pos {
        let v = walk.triangles[t];
        let avg = |f: fn(&fmv_ff7::field::Vertex) -> i16| v.iter().map(|p| f64::from(f(p))).sum::<f64>() / 3.0;
        Pos::new(avg(|p| p.x) as f32, avg(|p| p.y) as f32, avg(|p| p.z) as f32)
    };
    // Walking between the centroids of two linked triangles stays on the mesh.
    let (a, b) = (0..walk.len()).find_map(|t| (0..3).find_map(|e| walk.neighbor(t, e).map(|n| (t, n)))).unwrap();
    match area.validate_move(centroid(a), centroid(b)) {
        MoveResult::Accepted(p) => assert!((p.z - centroid(b).z).abs() < 5.0, "{p:?}"),
        other => panic!("expected Accepted, got {other:?}"),
    }
    // A step far off the platform is refused: the entity stays where it was.
    let from = centroid(a);
    let off = Pos::new(from.x + 3000.0, from.y, from.z);
    let same_spot = |r: MoveResult, at: Pos| matches!(r, MoveResult::Corrected(p) if (p.x - at.x).abs() < 0.01 && (p.y - at.y).abs() < 0.01);
    assert!(same_spot(area.validate_move(from, off), from));
    // Stepping out through a blocked edge onto empty ground is refused too.
    let inside = |x: f64, y: f64| {
        walk.triangles.iter().any(|tri| {
            let s = |p: &fmv_ff7::field::Vertex, q: &fmv_ff7::field::Vertex| {
                (f64::from(q.x) - f64::from(p.x)) * (y - f64::from(p.y)) - (f64::from(q.y) - f64::from(p.y)) * (x - f64::from(p.x))
            };
            let d = [s(&tri[0], &tri[1]), s(&tri[1], &tri[2]), s(&tri[2], &tri[0])];
            d.iter().all(|v| *v >= -1e-6) || d.iter().all(|v| *v <= 1e-6)
        })
    };
    let mut refused_exits = 0;
    for t in 0..walk.len() {
        for e in 0..3 {
            if walk.neighbor(t, e).is_some() {
                continue;
            }
            let (p, q) = (walk.triangles[t][e], walk.triangles[t][(e + 1) % 3]);
            let mid = ((f64::from(p.x) + f64::from(q.x)) / 2.0, (f64::from(p.y) + f64::from(q.y)) / 2.0);
            let c = centroid(t);
            let beyond = (mid.0 + (mid.0 - f64::from(c.x)) * 0.4, mid.1 + (mid.1 - f64::from(c.y)) * 0.4);
            if inside(beyond.0, beyond.1) {
                continue; // that edge borders another part of the mesh
            }
            let to = Pos::new(beyond.0 as f32, beyond.1 as f32, c.z);
            assert!(same_spot(area.validate_move(c, to), c), "triangle {t} edge {e}");
            refused_exits += 1;
        }
    }
    assert!(refused_exits >= 20, "{refused_exits}");
    // every spawn point is on the mesh
    for spawn in area.spawn_points() {
        assert!(matches!(area.validate_move(spawn, spawn), MoveResult::Accepted(_)));
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn char_archive_structure() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = LgpArchive::open_path(dir.join("field").join("char.lgp")).unwrap();
    assert_eq!(lgp.len(), 12_649);
    assert!(lgp.list().iter().all(|e| e.check_code == 0x0E && e.path_index == 0));
    let lower: HashSet<String> = lgp.list().iter().map(|e| e.name.to_ascii_lowercase()).collect();
    assert_eq!(lower.len(), 12_649, "no duplicate bare names, so first-match lookup is unambiguous");
    let ext = |e: &str| lgp.list().iter().filter(|x| x.name.to_ascii_lowercase().ends_with(e)).count();
    assert_eq!((ext(".hrc"), ext(".a"), ext(".p"), ext(".rsd"), ext(".tex")), (385, 3_209, 4_180, 4_180, 695));
    // every entry is readable and below the reader's cap (the largest is 124 236 bytes)
    let mut largest = 0usize;
    for i in 0..lgp.len() {
        largest = largest.max(lgp.read_index(i).unwrap().len());
    }
    assert_eq!(largest, 124_236);
    assert!(largest < MAX_ENTRY_LEN as usize);
}

// ---------------------------------------------------------------------------------------
// Field character models (char.lgp) and the playable table
// ---------------------------------------------------------------------------------------

fn char_lgp(dir: &std::path::Path) -> LgpArchive<std::fs::File> {
    LgpArchive::open_path(dir.join("field").join("char.lgp")).unwrap()
}

/// Every `.hrc`, `.rsd`, `.p`, `.tex` and `.a` entry of `char.lgp` parses, every skeleton loads as
/// a complete model and nothing is left dangling.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn char_models_all_parse() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = char_lgp(&dir);
    let names: Vec<String> = lgp.list().iter().map(|e| e.name.clone()).collect();
    let by_lower: HashSet<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();

    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut bones_min, mut bones_max, mut bones_total, mut null_skeletons) = (usize::MAX, 0usize, 0usize, 0usize);
    let mut rsd_refs = 0usize;
    let mut dangling: Vec<String> = Vec::new();
    // P statistics
    let (mut p_polys, mut p_groups, mut p_textured, mut p_blend, mut p_dropped) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut p_no_normal, mut p_no_uv, mut p_nn_lt_nv, mut p_nn_gt_nv) = (0usize, 0usize, 0usize, 0usize);
    let mut group_types: BTreeMap<u32, usize> = BTreeMap::new();
    let mut tex_sizes: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    let (mut tex_first_transparent, mut tex_total) = (0usize, 0usize);
    // animations
    let (mut frames_min, mut frames_max, mut zero_bone_anims) = (usize::MAX, 0usize, 0usize);
    let mut anim_bone_counts: BTreeMap<usize, usize> = BTreeMap::new();
    let mut anim_frame_counts: BTreeMap<usize, usize> = BTreeMap::new();
    let (mut root_rotation_frames, mut root_rotation_files) = (0usize, 0usize);
    let (mut rot_min, mut rot_max, mut trans_max) = (f32::MAX, f32::MIN, 0.0f32);
    let mut absurd_files: Vec<String> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let lower = name.to_ascii_lowercase();
        let bytes = lgp.read_index(i).unwrap();
        let ext = lower.rsplit('.').next().unwrap_or("");
        let kind = match ext {
            "hrc" => "hrc",
            "rsd" => "rsd",
            "p" => "p",
            "tex" => "tex",
            "a" => "a",
            _ => "other",
        };
        *count.entry(kind).or_default() += 1;
        match ext {
            "hrc" => {
                let hrc = Hrc::parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
                bones_min = bones_min.min(hrc.bones.len());
                bones_max = bones_max.max(hrc.bones.len());
                bones_total += hrc.bones.len();
                if hrc.declared_bones == 0 {
                    null_skeletons += 1;
                    assert_eq!((hrc.bones.len(), hrc.bones[0].name.as_str()), (1, "null"), "{name}");
                } else {
                    assert_eq!(hrc.declared_bones, hrc.bones.len(), "{name}");
                }
                for rsd in hrc.rsd_names() {
                    rsd_refs += 1;
                    if !by_lower.contains(&format!("{}.rsd", rsd.to_ascii_lowercase())) {
                        dangling.push(format!("{name} -> {rsd}.rsd"));
                    }
                }
            }
            "rsd" => {
                let rsd = Rsd::parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
                // PLY, MAT and GRP each appear once and name the same stem, which is the mesh
                let text: String = bytes.iter().map(|&b| char::from(b)).collect();
                let stem_of = |prefix: &str| -> Vec<String> {
                    text.lines()
                        .map(str::trim)
                        .filter_map(|l| l.strip_prefix(prefix))
                        .map(|v| v.rsplit_once('.').map_or(v, |(s, _)| s).to_ascii_lowercase())
                        .collect()
                };
                let want = vec![rsd.p_name.clone()];
                assert_eq!((stem_of("PLY="), stem_of("MAT="), stem_of("GRP=")), (want.clone(), want.clone(), want), "{name}");
                assert!(text.starts_with("@RSD940102"), "{name}");
                if !by_lower.contains(&format!("{}.p", rsd.p_name)) {
                    dangling.push(format!("{name} -> {}.p", rsd.p_name));
                }
                for t in &rsd.textures {
                    if !by_lower.contains(&format!("{t}.tex")) {
                        dangling.push(format!("{name} -> {t}.tex"));
                    }
                }
            }
            "p" => {
                let p = PModel::parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
                p_polys += p.polygons.len();
                p_groups += p.groups.len();
                p_nn_lt_nv += usize::from(p.normals.len() < p.vertices.len());
                p_nn_gt_nv += usize::from(p.normals.len() > p.vertices.len());
                for (g, group) in p.groups.iter().enumerate() {
                    *group_types.entry(group.primitive_type).or_default() += 1;
                    p_textured += usize::from(group.textured);
                    p_blend += usize::from(p.render_state(g).blend);
                    let t = p.group_triangles(group);
                    p_dropped += t.dropped;
                    p_no_normal += t.triangles.iter().flatten().filter(|c| c.normal.is_none()).count();
                    if group.textured {
                        p_no_uv += t.triangles.iter().flatten().filter(|c| c.texcoord.is_none()).count();
                    }
                }
            }
            "tex" => {
                let img = parse_tex(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
                // the colour-key flag (header 0x08) is set exactly when palette entry 0 is (0, 0, 0, alpha 0)
                let keyed = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) != 0;
                assert_eq!(keyed, bytes[0xEC..0xF0] == [0, 0, 0, 0], "{name}");
                *tex_sizes.entry((img.width, img.height)).or_default() += 1;
                tex_total += 1;
                tex_first_transparent += usize::from(img.rgba.get(3).copied() == Some(0));
            }
            "a" => {
                let a = FieldAnimation::parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
                frames_min = frames_min.min(a.frame_count());
                frames_max = frames_max.max(a.frame_count());
                zero_bone_anims += usize::from(a.bone_count() == 0);
                *anim_bone_counts.entry(a.bone_count()).or_default() += 1;
                *anim_frame_counts.entry(a.frame_count()).or_default() += 1;
                let turned = a.frames().iter().filter(|f| f.root_rotation != [0.0; 3]).count();
                root_rotation_frames += turned;
                root_rotation_files += usize::from(turned > 0);
                assert_eq!((bytes.len(), a.rotation_order, bytes[15]), (36 + a.frame_count() * (24 + 12 * a.bone_count()), [1, 0, 2], 0), "{name}");
                // raw floats: angles are degrees in [0, 360) except in the one file with absurd values
                let raw = |at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                let per_frame = 6 + 3 * a.bone_count();
                let all: Vec<f32> = (0..a.frame_count() * per_frame).map(|k| raw(36 + 4 * k)).collect();
                if all.iter().any(|v| !v.is_finite() || v.abs() > fmv_ff7::anim::MAX_ANGLE) {
                    absurd_files.push(name.clone());
                } else {
                    for (k, v) in all.iter().enumerate() {
                        match k % per_frame {
                            3..=5 => trans_max = trans_max.max(v.abs()),
                            _ => {
                                rot_min = rot_min.min(*v);
                                rot_max = rot_max.max(*v);
                            }
                        }
                    }
                }
                // every angle is finite and within the sanitising limit
                let sane = a.frames().iter().all(|f| f.rotations.iter().flatten().all(|v| v.is_finite() && v.abs() <= fmv_ff7::anim::MAX_ANGLE));
                assert!(sane, "{name}");
            }
            _ => {}
        }
    }
    eprintln!("entries by kind: {count:?}");
    eprintln!("hrc: bones min {bones_min} max {bones_max} total {bones_total}, null skeletons {null_skeletons}; rsd references {rsd_refs}; dangling {}", dangling.len());
    eprintln!("p: polygons {p_polys} groups {p_groups} textured {p_textured} blend {p_blend} dropped {p_dropped} corners without normal {p_no_normal} textured corners without uv {p_no_uv}; nn<nv in {p_nn_lt_nv} files, nn>nv in {p_nn_gt_nv}; group types {group_types:?}");
    eprintln!("tex: {tex_total} sizes {tex_sizes:?}; first texel transparent in {tex_first_transparent}");
    eprintln!("anim: frames {frames_min}..={frames_max}; bone counts {anim_bone_counts:?}; zero-bone {zero_bone_anims}");
    assert_eq!(count.get("hrc"), Some(&385));
    assert_eq!(count.get("rsd"), Some(&4180));
    assert_eq!(count.get("p"), Some(&4180));
    assert_eq!(count.get("tex"), Some(&695));
    assert_eq!(count.get("a"), Some(&3209));
    assert!(dangling.is_empty(), "{dangling:?}");
    assert_eq!(p_dropped, 0);
    // the totals the module docs quote
    assert_eq!((bones_min, bones_max, bones_total, null_skeletons, rsd_refs), (1, 29, 5_713, 105, 4_195));
    assert_eq!((p_polys, p_groups, p_textured, p_blend, p_no_normal, p_no_uv), (156_551, 4_875, 695, 23, 0, 0));
    assert_eq!((p_nn_lt_nv, p_nn_gt_nv), (1_567, 190));
    assert_eq!(group_types, BTreeMap::from([(0, 67), (1, 4_113), (2, 687), (4, 8)]));
    assert_eq!((tex_total, tex_sizes.get(&(32, 32)).copied()), (695, Some(603)));
    assert_eq!((frames_min, frames_max, zero_bone_anims), (1, 480, 130));
    assert_eq!(anim_bone_counts.get(&21), Some(&1_840));
    assert_eq!((anim_frame_counts.get(&30), anim_frame_counts.get(&15)), (Some(&987), Some(&294)));
    eprintln!("anim raw: rotation range {rot_min}..{rot_max}, max |root translation| {trans_max}, absurd files {absurd_files:?}; non-zero root rotation in {root_rotation_frames} frames of {root_rotation_files} files");
    assert_eq!(absurd_files, ["fghe.a"]);
    assert_eq!((root_rotation_frames, root_rotation_files), (23_357, 577));
    assert!(rot_min >= 0.0 && rot_max < 360.0, "angles are degrees in [0, 360): {rot_min}..{rot_max}");
    assert!((1000.0..1200.0).contains(&trans_max), "{trans_max}");

    // every skeleton loads as a complete model
    let hrcs: Vec<String> = names.iter().filter(|n| n.to_ascii_lowercase().ends_with(".hrc")).cloned().collect();
    let (mut parts, mut triangles, mut textured_parts, mut dropped, mut missing) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for hrc in &hrcs {
        let model = load_field_model(&mut lgp, hrc).unwrap_or_else(|e| panic!("{hrc}: {e}"));
        parts += model.parts.len();
        triangles += model.triangle_count();
        textured_parts += model.parts.iter().filter(|p| p.texture.is_some()).count();
        dropped += model.dropped_triangles;
        missing += model.missing_textures.len();
        let sound = model.parts.iter().all(|p| p.indices.iter().all(|&i| (i as usize) < p.positions.len()) && p.indices.len() % 3 == 0);
        assert!(sound, "{hrc}");
        assert!(model.parts.iter().all(|p| p.bone_index < model.bones.len()), "{hrc}");
    }
    eprintln!("models: {} loaded; parts {parts} (textured {textured_parts}); triangles {triangles}; dropped {dropped}; missing textures {missing}", hrcs.len());
    assert_eq!((dropped, missing), (0, 0));
    assert_eq!((hrcs.len(), parts, textured_parts, triangles), (385, 4_890, 695, 156_891));
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn len3(a: [f32; 3]) -> f32 {
    dot3(a, a).sqrt()
}

/// Facts about the P files that `pmodel`'s docs quote: index spaces, normals, winding and the
/// render-state words.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn p_models_index_spaces_normals_winding_and_render_state() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = char_lgp(&dir);
    let names: Vec<String> = lgp.list().iter().filter(|e| e.name.to_ascii_lowercase().ends_with(".p")).map(|e| e.name.clone()).collect();
    assert_eq!(names.len(), 4180);
    let (mut files_nn_eq_nv, mut groups_in_those, mut groups_normal_follows_vertex) = (0usize, 0usize, 0usize);
    let (mut polys, mut cw, mut ccw, mut degenerate) = (0usize, 0usize, 0usize, 0usize);
    let (mut normals_total, mut normals_unit, mut normals_zero) = (0usize, 0usize, 0usize);
    let (mut blend_groups, mut blend_src_dst) = (0usize, BTreeMap::<(u32, u32), usize>::new());
    let (mut local_vertex_ok, mut groups_total) = (0usize, 0usize);
    for name in &names {
        let bytes = lgp.read(name).unwrap();
        let p = PModel::parse(&bytes).unwrap();
        assert_eq!(p.vertex_colors.len(), p.vertices.len(), "{name}: one colour per vertex");
        // An independent statement of the layout from the raw bytes (header words, section sizes):
        // no "unknown" entries, a zero first u16 in every polygon, zero `u` words in every group,
        // and a file that is the documented sections plus 28 bytes per bounding box.
        {
            let word = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()) as usize;
            let half = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
            let (nv, nn, nu1, ntc, nvc, ne, np, nu2, nu3, nh, ng, nbb) = (word(3), word(4), word(5), word(6), word(7), word(8), word(9), word(10), word(11), word(12), word(13), word(14));
            assert_eq!((word(0), word(1), word(2)), (1, 1, 1), "{name}");
            assert_eq!((nu1, nu2, nu3), (0, 0, 0), "{name}: no unknown entries");
            let polygons_at = 128 + 12 * nv + 12 * nn + 8 * ntc + 4 * nvc + 4 * np + 4 * ne;
            let groups_at = polygons_at + 24 * np + 100 * nh;
            let groups_end = groups_at + 56 * ng;
            assert!(nbb <= 1 && bytes.len() == groups_end + 28 * nbb + 4 * nv, "{name}: tail of {} bytes", bytes.len() - groups_end);
            assert!((0..np).all(|i| half(polygons_at + 24 * i) == 0), "{name}: first u16 of every polygon is zero");
            for g in 0..ng {
                let at = groups_at + 56 * g;
                assert!((7..=10).all(|k| word(at / 4 + k) == 0), "{name}: group {g}'s u words are zero");
                let (primitive_type, textured) = (word(at / 4), word(at / 4 + 12));
                assert_eq!(textured == 1, primitive_type >= 2, "{name}: group {g}: type {primitive_type} textured {textured}");
            }
        }
        normals_total += p.normals.len();
        for n in &p.normals {
            let l = len3(*n);
            normals_unit += usize::from((l - 1.0).abs() < 0.01);
            normals_zero += usize::from(l < 1e-6);
        }
        let eq = p.normals.len() == p.vertices.len();
        files_nn_eq_nv += usize::from(eq);
        for (g, group) in p.groups.iter().enumerate() {
            groups_total += 1;
            let start = group.polygon_start as usize;
            let ps = &p.polygons[start..start + group.num_polygons as usize];
            // vertex indices are group-relative
            local_vertex_ok += usize::from(ps.iter().all(|q| q.vertex.iter().all(|&v| u32::from(v) < group.num_vertices)));
            if eq {
                groups_in_those += 1;
                let follows = ps.iter().all(|q| (0..3).all(|k| usize::from(q.normal[k]) == group.vertex_start as usize + usize::from(q.vertex[k])));
                groups_normal_follows_vertex += usize::from(follows);
            }
            let state = p.render_state(g);
            assert_eq!(state.blend, state.alpha < 255, "{name} group {g}: blending is on exactly for the partly transparent groups");
            if state.blend {
                blend_groups += 1;
                *blend_src_dst.entry((state.src_blend, state.dst_blend)).or_default() += 1;
            }
            // winding of the raw triangles against the first corner's stored normal
            for q in ps {
                let v = |k: usize| p.vertices[group.vertex_start as usize + usize::from(q.vertex[k])];
                let geometric = cross3(sub3(v(1), v(0)), sub3(v(2), v(0)));
                let stored = p.normals[usize::from(q.normal[0])];
                polys += 1;
                let d = dot3(geometric, stored);
                if len3(geometric) < 1e-9 || len3(stored) < 1e-6 {
                    degenerate += 1;
                } else if d < 0.0 {
                    cw += 1;
                } else {
                    ccw += 1;
                }
            }
        }
    }
    eprintln!("files with as many normals as vertices {files_nn_eq_nv}; their groups {groups_in_those}, of which normal index = vertex_start + vertex index: {groups_normal_follows_vertex}");
    eprintln!("normals {normals_total}: unit length {normals_unit}, zero {normals_zero}");
    eprintln!("polygons {polys}: clockwise against the stored normal {cw}, counter-clockwise {ccw}, degenerate or normal-less {degenerate}");
    eprintln!("blend groups {blend_groups} {blend_src_dst:?}; groups with every local vertex index below num_vertices {local_vertex_ok} of {groups_total}");
    assert_eq!(local_vertex_ok, groups_total);
    assert_eq!(polys, 156_551);
    assert!(cw * 100 >= polys * 97, "the files store triangles clockwise seen from the normal's side");
    assert_eq!((files_nn_eq_nv, groups_in_those, groups_normal_follows_vertex), (2_423, 2_426, 5));
    assert_eq!((normals_total, normals_unit, normals_zero), (96_701, 96_110, 268));
    assert_eq!((cw, ccw, degenerate), (152_710, 3_393, 448));
    assert_eq!(blend_groups, 23);
    assert_eq!(blend_src_dst, BTreeMap::from([((5, 2), 13), ((5, 6), 10)]));
}

/// The model-loader sections of all 702 fields decode with no byte left over, and the playable
/// table agrees with them: for each character, how many loader records use its skeleton with its
/// idle, walk and run animations first, and that the animations fit the skeleton.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn playable_models_match_the_field_loaders() {
    let Some(dir) = data_dir() else { return };
    let mut flevel_lgp = LgpArchive::open_path(flevel(&dir)).unwrap();
    let mut char_archive = char_lgp(&dir);
    let char_names: HashSet<String> = char_archive.list().iter().map(|e| e.name.to_ascii_lowercase()).collect();
    let field_names: Vec<String> = flevel_lgp.list().iter().filter(|e| is_field_name(&e.name)).map(|e| e.name.clone()).collect();
    assert_eq!(field_names.len(), 702);

    let (mut records, mut trailing, mut missing_hrc, mut missing_anim) = (0usize, 0usize, 0usize, 0usize);
    // (field scale, per-model scale) -> records
    let mut scale_pairs: BTreeMap<(u16, Option<u16>), usize> = BTreeMap::new();
    // skeleton stem -> (idle, walk, run) -> records
    let mut by_hrc: BTreeMap<String, BTreeMap<Vec<String>, usize>> = BTreeMap::new();
    for name in &field_names {
        let field = load_field_file(&mut flevel_lgp, name);
        let loader = field.model_loader().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(loader.field_scale, field.field_scale().unwrap(), "{name}");
        assert_eq!(field.model_loader_header().unwrap().model_count as usize, loader.models.len(), "{name}");
        trailing += loader.trailing_bytes;
        for m in &loader.models {
            records += 1;
            missing_hrc += usize::from(!char_names.contains(&format!("{}.hrc", m.hrc_stem())));
            missing_anim += m.animations.iter().filter(|a| !char_names.contains(&format!("{}.a", a.stem()))).count();
            assert!(m.name.ends_with(".char"), "{name}: {:?}", m.name);
            *scale_pairs.entry((loader.field_scale, m.scale)).or_default() += 1;
            let first_three: Vec<String> = m.animations.iter().take(3).map(|a| a.stem()).collect();
            *by_hrc.entry(m.hrc_stem()).or_default().entry(first_three).or_default() += 1;
        }
    }
    eprintln!("loader records {records}, trailing bytes {trailing}, skeletons not in char.lgp {missing_hrc}, animations not in char.lgp {missing_anim}");
    assert_eq!((records, trailing), (5_454, 0));
    assert_eq!((missing_hrc, missing_anim), (0, 0));
    // the per-model scale (ASCII) is not the field's scale in general; for the playable characters'
    // records it is, except in a few fields
    let differing: Vec<_> = scale_pairs.iter().filter(|((field, model), _)| Some(*field) != *model).collect();
    eprintln!("(field scale, per-model scale) pairs: {} distinct, {} differ", scale_pairs.len(), differing.len());
    assert_eq!((scale_pairs.len(), differing.len()), (71, 56));
    let model_scales: Vec<u16> = scale_pairs.keys().filter_map(|(_, model)| *model).collect();
    assert_eq!((model_scales.iter().min(), model_scales.iter().max()), (Some(&32), Some(&5120)));
    for (who, hrc, equal_expected) in [("cloud", "aaaa", 712usize), ("tifa", "aagb", 519), ("barret", "acgd", 310), ("aerith", "auff", 215)] {
        let (mut equal, mut other) = (0usize, BTreeMap::<(u16, Option<u16>), usize>::new());
        for name in &field_names {
            let loader = load_field_file(&mut flevel_lgp, name).model_loader().unwrap();
            for m in loader.models.iter().filter(|m| m.hrc_stem() == hrc) {
                if m.scale == Some(loader.field_scale) {
                    equal += 1;
                } else {
                    *other.entry((loader.field_scale, m.scale)).or_default() += 1;
                }
            }
        }
        eprintln!("{who}: per-model scale equals the field scale in {equal} records; (field, model) otherwise: {other:?}");
        assert_eq!(equal, equal_expected, "{who}");
    }

    // md1stin: ten records, the first is Cloud
    let md1stin = load_field_file(&mut flevel_lgp, "md1stin").model_loader().unwrap();
    assert_eq!(md1stin.models.len(), 10);
    let cloud = &md1stin.models[0];
    assert_eq!((cloud.name.as_str(), cloud.hrc.as_str(), cloud.scale, cloud.unknown), ("md1stinmain_n_cloud.char", "AAAA.HRC", Some(512), 0));
    let stems: Vec<String> = cloud.animations.iter().map(|a| a.stem()).collect();
    assert_eq!(stems, ["acfe", "aaff", "aaga", "bvjf"]);
    assert_eq!(cloud.animations[0].name, "ACFE.aki");
    assert_eq!(md1stin.models[1].hrc, "ACGD.HRC", "Barret (named ballet) is the second record of md1stin");

    // (records using the skeleton, of them records opening with idle, walk, run) as quoted in the
    // `playable` module docs
    let expected: BTreeMap<&str, (usize, usize)> = BTreeMap::from([
        ("ff7/cloud", (728, 610)),
        ("ff7/tifa", (529, 529)),
        ("ff7/barret", (315, 310)),
        ("ff7/aerith", (220, 219)),
        ("ff7/red_xiii", (251, 235)),
        ("ff7/cid", (472, 472)),
        ("ff7/yuffie", (242, 242)),
        ("ff7/vincent", (197, 194)),
    ]);
    assert_eq!(expected.len(), fmv_ff7::playable::PLAYABLE_FIELD_MODELS.len());
    // the 118 other records of Cloud's skeleton: 115 open with the generic standing clip `aafe`
    let cloud_records = by_hrc.get("aaaa").unwrap();
    let aafe_first = ["aafe", "aaff", "aaga"].map(String::from).to_vec();
    assert_eq!(cloud_records.get(&aafe_first), Some(&115));
    assert_eq!(cloud_records.values().sum::<usize>() - 610 - 115, 3);
    for m in fmv_ff7::playable::PLAYABLE_FIELD_MODELS {
        let wanted: Vec<String> = [m.idle_anim, m.walk_anim, m.run_anim].iter().map(|s| s.to_string()).collect();
        let records_for = by_hrc.get(m.hrc).map_or(0, |anims| anims.get(&wanted).copied().unwrap_or(0));
        let all_for: usize = by_hrc.get(m.hrc).map_or(0, |anims| anims.values().sum());
        eprintln!("{}: skeleton {} used by {all_for} loader records, {records_for} of them open with {wanted:?}", m.appearance_ref, m.hrc);
        assert_eq!(Some(&(all_for, records_for)), expected.get(m.appearance_ref), "{}", m.appearance_ref);
        assert_eq!(resolve_appearance(m.appearance_ref), Some(*m));
        // the whole character loads in one call, and the skeleton and all three animations fit
        let character = fmv_ff7::load_playable(&mut char_archive, m).unwrap();
        assert_eq!(character.appearance, *m);
        assert_eq!(m.animation_for(fmv_core::AnimState::Walk), m.walk_anim);
        assert!(character.pose(fmv_core::AnimState::Run, 5).is_some());
        let model = load_field_model(&mut char_archive, m.hrc).unwrap();
        for stem in [m.idle_anim, m.walk_anim, m.run_anim] {
            let a = load_field_animation(&mut char_archive, stem).unwrap();
            assert!(model.fits(&a), "{}: {stem} has {} bones, the skeleton {}", m.appearance_ref, a.bone_count(), model.bones.len());
        }
        let frames = |l: &mut LgpArchive<std::fs::File>, stem: &str| load_field_animation(l, stem).unwrap().frame_count();
        let (idle, walk, run) = (frames(&mut char_archive, m.idle_anim), frames(&mut char_archive, m.walk_anim), frames(&mut char_archive, m.run_anim));
        eprintln!("{}: frames idle {idle}, walk {walk}, run {run}", m.appearance_ref);
        // the idle clip is a pose or a very short loop (Cloud's has 1 frame, Aerith's 2); walk and run are loops
        assert!(idle <= 4, "{}: idle has {idle} frames", m.appearance_ref);
        assert!(walk >= 8 && run >= 8, "{}: walk {walk} run {run}", m.appearance_ref);
    }
}

/// The posing conventions of `anim`'s docs, asserted on Cloud: upright, standing on the ground,
/// face and toes pointing the same way, the left side on +x, flat soles, feet planted through the
/// walk and triangles wound counter-clockwise from outside.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn cloud_poses_are_physically_plausible() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = char_lgp(&dir);
    let cloud = resolve_appearance("ff7/cloud").unwrap();
    let model = load_field_model(&mut lgp, cloud.hrc).unwrap();
    assert_eq!((model.skeleton_name.as_str(), model.bones.len(), model.parts.len(), model.triangle_count()), ("n_cloud_sk", 21, 18, 528));
    let idle = load_field_animation(&mut lgp, cloud.idle_anim).unwrap();
    let walk = load_field_animation(&mut lgp, cloud.walk_anim).unwrap();
    let run = load_field_animation(&mut lgp, cloud.run_anim).unwrap();
    assert_eq!((idle.frame_count(), walk.frame_count(), run.frame_count()), (1, 30, 15));
    assert!([&idle, &walk, &run].iter().all(|a| model.fits(a) && a.rotation_order == [1, 0, 2]));

    let bone = |name: &str| model.bones.iter().position(|b| b.name == name).unwrap();
    let identity = mat4_identity();
    // every bone's mesh lies on the -z side of its own frame (the bone axis), on average
    for part in &model.parts {
        let mean_z: f32 = part.positions.iter().map(|p| p[2]).sum::<f32>() / part.positions.len() as f32;
        assert!(mean_z < 0.0, "{}: mean z {mean_z}", model.bones[part.bone_index].name);
    }
    // the limb bones' meshes span about their length along -z (forearm: length 4.601, z -4.60..0)
    for (name, length) in [("l_foarm", 4.6006985f32), ("r_foarm", 4.6006985), ("l_hand", 2.2786045)] {
        let b = bone(name);
        assert!((model.bones[b].length - length).abs() < 1e-5);
        let zs: Vec<f32> = model.parts.iter().filter(|p| p.bone_index == b).flat_map(|p| p.positions.iter().map(|v| v[2])).collect();
        let (lo, hi) = (zs.iter().cloned().fold(f32::MAX, f32::min), zs.iter().cloned().fold(f32::MIN, f32::max));
        assert!((lo + length).abs() < 0.05 && hi.abs() < 0.05, "{name}: z {lo}..{hi} against length {length}");
    }

    // ---- the idle pose
    let pose0 = model.pose(&idle, 0).unwrap();
    let (lo, hi) = model.posed_bounds(&pose0, &identity).unwrap();
    eprintln!("idle bounds {lo:?} {hi:?}");
    assert!(lo[1] < -25.0 && lo[1] > -45.0, "head top {lo:?}: upright along -y");
    assert!(hi[1] > -0.5 && hi[1] < 2.0, "soles {hi:?}: the lowest point is at the ground");
    let hip_y = transform_point(&pose0[bone("hip")], [0.0; 3])[1];
    assert!((hip_y + 13.54).abs() < 0.1, "hip height {hip_y}");
    // legs point away from the head; the chest is above the hip
    let down = |name: &str| transform_vector(&pose0[bone(name)], [0.0, 0.0, -1.0]);
    assert!(down("l_femur")[1] > 0.9 && down("r_femur")[1] > 0.9, "thighs point down (+y)");
    assert!(down("hip")[1] < -0.9, "the hip bone points up (-y) towards the chest");
    // the left side is +x
    let origin = |name: &str| transform_point(&pose0[bone(name)], [0.0; 3]);
    assert!(origin("l_uparm")[0] > 3.0 && origin("r_uparm")[0] < -3.0);
    assert!(origin("l_femur")[0] > 1.0 && origin("r_femur")[0] < -1.0);
    // the face (the textured polygons of the head) is in front of the head's centre, towards -z, and
    // the feet point the same way
    let centroid = |parts: &[&fmv_ff7::MeshPart]| -> [f32; 3] {
        let mut sum = [0.0f32; 3];
        let mut n = 0.0;
        for p in parts {
            for v in &p.positions {
                let w = transform_point(&pose0[p.bone_index], *v);
                sum = [sum[0] + w[0], sum[1] + w[1], sum[2] + w[2]];
                n += 1.0;
            }
        }
        [sum[0] / n, sum[1] / n, sum[2] / n]
    };
    let head = bone("head");
    let head_all: Vec<&fmv_ff7::MeshPart> = model.parts.iter().filter(|p| p.bone_index == head).collect();
    let head_face: Vec<&fmv_ff7::MeshPart> = head_all.iter().copied().filter(|p| p.texture.is_some()).collect();
    assert_eq!(head_face.len(), 3, "eyes, mouth and brows are three textured groups");
    let face_dir = sub3(centroid(&head_face), centroid(&head_all));
    assert!(face_dir[2] < -1.0, "the face is towards -z: {face_dir:?}");
    let foot = bone("l_foot");
    let foot_parts: Vec<&fmv_ff7::MeshPart> = model.parts.iter().filter(|p| p.bone_index == foot).collect();
    let toe_dir = sub3(centroid(&foot_parts), origin("l_foot"));
    let horizontal = |v: [f32; 3]| [v[0], 0.0, v[2]];
    let cos = dot3(horizontal(face_dir), horizontal(toe_dir)) / (len3(horizontal(face_dir)) * len3(horizontal(toe_dir)));
    eprintln!("face direction {face_dir:?}, toe direction {toe_dir:?}, horizontal cosine {cos}");
    assert!(cos > 0.5, "the toes point where the face does (cosine {cos})");
    // soles flat and level: the four lowest vertices of each foot
    let lowest = |name: &str| -> Vec<f32> {
        let b = bone(name);
        let mut ys: Vec<f32> = model.parts.iter().filter(|p| p.bone_index == b).flat_map(|p| p.positions.iter().map(|v| transform_point(&pose0[b], *v)[1])).collect();
        ys.sort_by(|a, b| b.total_cmp(a)); // y points down: the lowest points have the largest y
        ys.truncate(4);
        ys
    };
    let (left, right) = (lowest("l_foot"), lowest("r_foot"));
    eprintln!("lowest foot vertices (y): left {left:?} right {right:?}");
    assert!(left[0] - left[3] < 1.2 && right[0] - right[3] < 1.2, "flat soles");
    assert!((left[0] - right[0]).abs() < 0.3, "level feet");

    // ---- the walk: the lowest foot point stays near the ground through all 30 frames while the
    // hip bobs between 12.8 and 13.7 above it; the planted foot is in front of the root on average
    let lowest_foot = |animation: &FieldAnimation| -> (Vec<f32>, Vec<f32>) {
        let (mut ys, mut zs) = (Vec::new(), Vec::new());
        for f in 0..animation.frame_count() {
            let pose = model.pose(animation, f).unwrap();
            let mut best = (f32::MIN, 0.0f32);
            for name in ["l_foot", "r_foot"] {
                let b = bone(name);
                for p in model.parts.iter().filter(|p| p.bone_index == b) {
                    for v in &p.positions {
                        let w = transform_point(&pose[b], *v);
                        if w[1] > best.0 {
                            best = (w[1], w[2]);
                        }
                    }
                }
            }
            ys.push(best.0);
            zs.push(best.1);
        }
        (ys, zs)
    };
    let range = |v: &[f32]| (v.iter().cloned().fold(f32::MAX, f32::min), v.iter().cloned().fold(f32::MIN, f32::max));
    let (lowest_y, lowest_z) = lowest_foot(&walk);
    for f in 0..walk.frame_count() {
        let hip = walk.frame(f).unwrap().root_translation;
        assert!(hip[0] == 0.0 && (hip[1] > 12.5 && hip[1] < 14.0), "frame {f}: root translation {hip:?}");
    }
    let ((y_lo, y_hi), (z_lo, z_hi)) = (range(&lowest_y), range(&lowest_z));
    let spread = y_hi - y_lo;
    let mean_z = lowest_z.iter().sum::<f32>() / lowest_z.len() as f32;
    eprintln!("walk: lowest foot point y {y_lo:.2}..{y_hi:.2} (spread {spread:.2}), z {z_lo:.2}..{z_hi:.2}, mean z {mean_z:.2}");
    assert!(spread < 1.0, "feet stay planted through the walk: spread {spread}");
    assert!(lowest_y.iter().all(|&y| y > 0.0 && y < 1.6), "the soles are at the ground through the walk");
    assert!(mean_z < 0.0, "the planted foot is in front of the root (forward is -z)");
    // the planted foot sweeps back by about 9 model units per step (15 frames) in the walk and
    // about 15 per step (7.5 frames) in the run: roughly 0.5-0.6 and 2.0-2.5 model units per frame,
    // which at the default model scale of 4 is about 2-2.4 and 8-10 walkmesh units per frame, the
    // hypothesised walk and run speeds (a rough consistency check, see `area`)
    let walk_sweep = z_hi - z_lo;
    // the run leaves the ground and comes back: the lowest point swings but stays bounded
    let (run_y, run_z) = lowest_foot(&run);
    let run_sweep = range(&run_z).1 - range(&run_z).0;
    eprintln!("run: lowest point y per frame {run_y:?}; sweep {run_sweep:.2}; walk sweep {walk_sweep:.2}");
    assert!((8.8..9.4).contains(&walk_sweep) && (14.5..15.2).contains(&run_sweep), "walk {walk_sweep} run {run_sweep}");
    assert!(run_y.iter().all(|y| y.is_finite() && *y > -3.0 && *y < 3.0));

    // ---- winding: triangles are counter-clockwise seen from outside, i.e. their geometric normal
    // agrees with the stored vertex normals (checked in bone-local space: the pose is rigid)
    let (mut agree, mut total) = (0usize, 0usize);
    for part in &model.parts {
        for t in part.indices.chunks_exact(3) {
            let p = |k: usize| part.positions[t[k] as usize];
            let geometric = cross3(sub3(p(1), p(0)), sub3(p(2), p(0)));
            let n = |k: usize| part.normals[t[k] as usize];
            let stored = [n(0)[0] + n(1)[0] + n(2)[0], n(0)[1] + n(1)[1] + n(2)[1], n(0)[2] + n(1)[2] + n(2)[2]];
            if len3(geometric) > 1e-9 && len3(stored) > 0.1 {
                total += 1;
                agree += usize::from(dot3(geometric, stored) > 0.0);
            }
        }
    }
    eprintln!("winding: {agree} of {total} triangles agree with their stored normals");
    assert!(agree * 100 >= total * 95, "{agree} of {total}");

    // ---- the field mapping: forward -> +x at facing 0, left -> +y, up -> +z, a proper rotation
    let place = model_to_field([0.0; 3], 0.0, 1.0);
    let forward = transform_vector(&place, face_dir);
    assert!(forward[0] > 0.0 && forward[0].abs() > forward[1].abs(), "the face looks along +x at facing 0: {forward:?}");
    let left_arm = transform_point(&place, origin("l_uparm"));
    assert!(left_arm[1] > 3.0 && left_arm[0].abs() < 2.0, "the left shoulder is on the +y side: {left_arm:?}");
    let (field_lo, field_hi) = model.posed_bounds(&pose0, &place).unwrap();
    assert!(field_lo[2] > -2.0 && field_lo[2] < 0.0 && field_hi[2] > 25.0, "standing on z = 0, upright along +z: {field_lo:?} {field_hi:?}");
}

/// The limbs that the characters carry asymmetrically are on the sides their bone names say, which
/// is what makes the documented pose convention the anatomically right one of the mirror pair (see
/// the `anim` docs): Barret's gun arm (the game's right arm) is the `r_*` chain, whose hand is a
/// plain grey metal block against the `l_*` chain's brown fist, and Cloud's bracer (grey, so a
/// less saturated forearm) is on the `l_*` chain; both `l_*` chains stand at +x.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_asymmetric_limbs_are_on_the_sides_their_bone_names_say() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = char_lgp(&dir);
    let bone_of = |model: &fmv_ff7::FieldModel, name: &str| model.bones.iter().position(|b| b.name == name).unwrap();
    let colours = |model: &fmv_ff7::FieldModel, name: &str| -> Vec<[u8; 4]> {
        let b = bone_of(model, name);
        model.parts.iter().filter(|p| p.bone_index == b).flat_map(|p| p.colors.iter().copied()).collect()
    };
    // (mean of the largest channel difference, mean saturation) over a limb's vertex colours
    let spread = |model: &fmv_ff7::FieldModel, name: &str| -> (f32, f32) {
        let cs = colours(model, name);
        let n = cs.len() as f32;
        let diff = cs.iter().map(|c| f32::from(c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]))).sum::<f32>() / n;
        let sat = cs.iter().map(|c| f32::from(c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])) / f32::from(c[0].max(c[1]).max(c[2]).max(1))).sum::<f32>() / n;
        (diff, sat)
    };
    for who in ["ff7/barret", "ff7/cloud", "ff7/tifa", "ff7/aerith"] {
        let m = resolve_appearance(who).unwrap();
        let model = load_field_model(&mut lgp, m.hrc).unwrap();
        let idle = load_field_animation(&mut lgp, m.idle_anim).unwrap();
        let pose = model.pose(&idle, 0).unwrap();
        let x = |name: &str| transform_point(&pose[bone_of(&model, name)], [0.0; 3])[0];
        // the left chain is at +x: the left of a character facing -z with up -y in a right-handed frame
        assert!(x("l_uparm") > 0.0 && x("r_uparm") < 0.0, "{who}");
        assert!(x("l_femur") > 0.0 && x("r_femur") < 0.0, "{who}");
        // the pose is left-right symmetric about x = 0 to within the pose's own asymmetry
        assert!((x("l_uparm") + x("r_uparm")).abs() < 1.5, "{who}");
    }
    let barret = resolve_appearance("ff7/barret").unwrap();
    let model = load_field_model(&mut lgp, barret.hrc).unwrap();
    let (l_hand, r_hand) = (spread(&model, "l_hand"), spread(&model, "r_hand"));
    eprintln!("barret hand colour spread (largest channel difference, saturation): l_ {l_hand:?} r_ {r_hand:?}");
    assert!(r_hand.0 < 10.0 && l_hand.0 > 20.0, "the r_ hand is grey metal (the gun), the l_ hand a brown fist");
    let cloud = resolve_appearance("ff7/cloud").unwrap();
    let model = load_field_model(&mut lgp, cloud.hrc).unwrap();
    let (l_arm, r_arm) = (spread(&model, "l_foarm"), spread(&model, "r_foarm"));
    eprintln!("cloud forearm colour spread: l_ {l_arm:?} r_ {r_arm:?}");
    assert!(l_arm.1 + 0.1 < r_arm.1, "the l_ forearm carries the grey bracer, so it is less saturated");
    // the pauldron is part of the l_ upper arm: it has more vertices than the r_ one
    let vertices = |name: &str| -> usize { colours(&model, name).len() };
    assert!(vertices("l_uparm") > vertices("r_uparm"), "l_uparm {} r_uparm {}", vertices("l_uparm"), vertices("r_uparm"));
}

type M3 = [[f64; 3]; 3];

fn axis_matrix(axis: usize, degrees: f64) -> M3 {
    let (s, c) = degrees.to_radians().sin_cos();
    match axis {
        0 => [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
        1 => [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
        _ => [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
    }
}

fn m3_mul(a: &M3, b: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, slot) in row.iter_mut().enumerate() {
            *slot = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn m3_apply(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| (0..3).map(|k| m[i][k] * v[k]).sum())
}

fn v3_dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|k| a[k] * b[k]).sum()
}

fn v3_unit(a: [f64; 3]) -> [f64; 3] {
    let l = v3_dot(a, a).sqrt().max(1e-12);
    a.map(|c| c / l)
}

/// The search that fixed the posing conventions of `anim`'s docs, re-run on the owned data: for
/// every axis order (six), sign pattern of the three angles (eight) and bone direction (`-z`,
/// `+z`) pose Cloud's standing frame and score how physically plausible the standing figure is.
/// The documented convention (order y, x, z; all signs positive; bone axis `-z`) must score best,
/// tied only with its left-right mirror image (negating the y and z angles), and every other
/// candidate must be clearly worse.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_documented_pose_convention_is_the_only_physically_plausible_one() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = char_lgp(&dir);
    let cloud = resolve_appearance("ff7/cloud").unwrap();
    let model = load_field_model(&mut lgp, cloud.hrc).unwrap();
    let idle = load_field_animation(&mut lgp, cloud.idle_anim).unwrap();
    let frame = idle.frame(0).unwrap().clone();
    let bone = |name: &str| model.bones.iter().position(|b| b.name == name).unwrap();
    let (head, hip, l_foot, r_foot, l_femur, l_tibia) = (bone("head"), bone("hip"), bone("l_foot"), bone("r_foot"), bone("l_femur"), bone("l_tibia"));
    let centroid_of = |parts: Vec<&fmv_ff7::MeshPart>| -> [f64; 3] {
        let mut sum = [0.0f64; 3];
        let mut n = 0.0;
        for p in parts {
            for v in &p.positions {
                for k in 0..3 {
                    sum[k] += f64::from(v[k]);
                }
                n += 1.0;
            }
        }
        sum.map(|c| c / n)
    };
    let of_bone = |b: usize, textured_only: bool| -> Vec<&fmv_ff7::MeshPart> { model.parts.iter().filter(|p| p.bone_index == b && (!textured_only || p.texture.is_some())).collect() };
    let face_local = {
        let (all, face) = (centroid_of(of_bone(head, false)), centroid_of(of_bone(head, true)));
        [0, 1, 2].map(|k| face[k] - all[k])
    };
    let toe_local = centroid_of(of_bone(l_foot, false));

    let mut scored: Vec<(f64, String)> = Vec::new();
    let perms: [[usize; 3]; 6] = [[1, 0, 2], [2, 0, 1], [0, 1, 2], [0, 2, 1], [1, 2, 0], [2, 1, 0]];
    let names = ['x', 'y', 'z'];
    for perm in perms {
        for signs in [[1.0, 1.0, 1.0], [1.0, 1.0, -1.0], [1.0, -1.0, 1.0], [1.0, -1.0, -1.0], [-1.0, 1.0, 1.0], [-1.0, 1.0, -1.0], [-1.0, -1.0, 1.0], [-1.0, -1.0, -1.0]] {
            for axis in [[0.0, 0.0, -1.0], [0.0, 0.0, 1.0]] {
                // world rotation and origin of every bone
                let mut world: Vec<(M3, [f64; 3])> = Vec::new();
                for (i, b) in model.bones.iter().enumerate() {
                    let angles = frame.rotations[i].map(f64::from);
                    let [p0, p1, p2] = perm;
                    let local = m3_mul(&axis_matrix(p0, signs[p0] * angles[p0]), &m3_mul(&axis_matrix(p1, signs[p1] * angles[p1]), &axis_matrix(p2, signs[p2] * angles[p2])));
                    let entry = match b.parent {
                        None => (local, [0.0; 3]),
                        Some(p) => {
                            let (pr, po) = world[p];
                            let off = m3_apply(&pr, axis.map(|c| c * f64::from(model.bones[p].length)));
                            (m3_mul(&pr, &local), [po[0] + off[0], po[1] + off[1], po[2] + off[2]])
                        }
                    };
                    world.push(entry);
                }
                let up = v3_unit([0, 1, 2].map(|k| world[head].1[k] - world[hip].1[k]));
                let lowest = |b: usize| -> Vec<f64> {
                    let (r, o) = world[b];
                    let mut hs: Vec<f64> = of_bone(b, false).iter().flat_map(|p| p.positions.iter()).map(|v| {
                        let w = m3_apply(&r, v.map(f64::from));
                        v3_dot([o[0] + w[0], o[1] + w[1], o[2] + w[2]], up)
                    }).collect();
                    hs.sort_by(f64::total_cmp);
                    hs.truncate(4);
                    hs
                };
                let (low_l, low_r) = (lowest(l_foot), lowest(r_foot));
                let flat = (low_l[3] - low_l[0]).max(low_r[3] - low_r[0]);
                let level = (low_l[0] - low_r[0]).abs();
                let horizontal = |v: [f64; 3]| v3_unit([0, 1, 2].map(|k| v[k] - up[k] * v3_dot(v, up)));
                let face = horizontal(m3_apply(&world[head].0, face_local));
                let toe = horizontal(m3_apply(&world[l_foot].0, toe_local));
                let facing = v3_dot(face, toe);
                let femur = v3_unit(m3_apply(&world[l_femur].0, axis));
                let tibia = v3_unit(m3_apply(&world[l_tibia].0, axis));
                let straight = v3_dot(femur, tibia);
                let legs_down = v3_dot(femur, up);
                let score = flat + level + (1.0 - facing) + (1.0 - straight) + (legs_down + 0.9).max(0.0);
                let label = format!("{}{}{} signs {:?} bone axis {:?}", names[perm[0]], names[perm[1]], names[perm[2]], signs, axis);
                scored.push((score, label));
            }
        }
    }
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (score, label) in scored.iter().take(5) {
        eprintln!("{score:.2}  {label}");
    }
    assert_eq!(scored.len(), 96);
    let documented = "yxz signs [1.0, 1.0, 1.0] bone axis [0.0, 0.0, -1.0]";
    let mirrored = "yxz signs [1.0, -1.0, -1.0] bone axis [0.0, 0.0, -1.0]";
    let best_two: HashSet<&str> = scored.iter().take(2).map(|s| s.1.as_str()).collect();
    assert_eq!(best_two, HashSet::from([documented, mirrored]), "the two best candidates are the documented convention and its mirror image");
    assert!(scored[2].0 - scored[1].0 > 0.3, "every other candidate is clearly worse: {:?}", &scored[..3]);
    assert!((scored[0].0 - scored[1].0).abs() < 0.05, "the mirror pair scores alike");
}

/// (spawn y, Cloud's world height, his height on the 320 x 240 screen in pixels, depth below the camera)
type SpawnSize = (f32, f32, f32, f32);

/// The default model scale puts Cloud at a plausible size in `md1stin` (field scale 512) and in
/// `md1_1` (1024, where the factor doubles), and the speeds follow the hypothesis.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn model_scale_gives_cloud_a_plausible_size_in_md1stin_and_md1_1() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = char_lgp(&dir);
    let cloud = resolve_appearance("ff7/cloud").unwrap();
    let model = load_field_model(&mut lgp, cloud.hrc).unwrap();
    let idle = load_field_animation(&mut lgp, cloud.idle_anim).unwrap();
    let pose = model.pose(&idle, 0).unwrap();
    let provenance = EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Pc,
        edition: "steam".into(),
        region: "any".into(),
        language: "en".into(),
        revision: "test".into(),
        sources: vec![],
        modded: false,
    };
    let adapter = Ff7Adapter::from_flevel_path(flevel(&dir), provenance).unwrap();
    // Cloud's height in walkmesh units, and on the 320 x 240 screen at each spawn point of a field:
    // (spawn y, world height, screen height in pixels, depth)
    let measure = |field: &str| -> (u16, f32, Vec<SpawnSize>) {
        let area = adapter.load_field(field).unwrap();
        let projection = Projection::from_camera(&area.cameras().unwrap()[0]);
        let scale = fmv_ff7::default_model_scale(area.field_scale());
        let mut sizes = Vec::new();
        for spawn in area.spawn_points() {
            let place = model_to_field([spawn.x, spawn.y, spawn.z], -1.5, scale);
            let (lo, hi) = model.posed_bounds(&pose, &place).unwrap();
            assert!((lo[2] - spawn.z).abs() < 3.0 * scale, "{field}: the soles are on the walkmesh");
            let (mut top, mut bottom) = (f32::MAX, f32::MIN);
            for part in &model.parts {
                for v in &part.positions {
                    let w = transform_point(&place, transform_point(&pose[part.bone_index], *v));
                    let s = projection.project(w).expect("in front of the camera");
                    top = top.min(s.y);
                    bottom = bottom.max(s.y);
                }
            }
            let depth = projection.project([spawn.x, spawn.y, spawn.z]).unwrap().depth;
            sizes.push((spawn.y, hi[2] - lo[2], bottom - top, depth));
        }
        (area.field_scale(), scale, sizes)
    };
    let (field_scale, scale, sizes) = measure("md1stin");
    eprintln!("md1stin: field scale {field_scale}, model scale {scale}; (y, world height, screen height px, depth) per spawn: {sizes:?}");
    assert_eq!((field_scale, scale), (512, fmv_ff7::model::DEFAULT_MODEL_SCALE_UNITS));
    for (_, world_height, _, _) in &sizes {
        assert!((120.0..150.0).contains(world_height), "Cloud is about 33 model units = {world_height} walkmesh units tall");
    }
    // the spawn point nearest the camera: Cloud fills about a quarter of the 240-pixel-high screen
    let near = sizes.iter().min_by(|a, b| a.3.total_cmp(&b.3)).unwrap();
    assert!((45.0..70.0).contains(&near.2), "{near:?}");
    // md1_1 is built in units twice as fine: Cloud is twice as tall in them (about 272), and on the
    // screen (an elevated camera that shows more of the field) about 40 pixels, 17%
    let (field_scale, scale, sizes) = measure("md1_1");
    eprintln!("md1_1: field scale {field_scale}, model scale {scale}; per spawn: {sizes:?}");
    assert_eq!((field_scale, scale), (1024, 2.0 * fmv_ff7::model::DEFAULT_MODEL_SCALE_UNITS));
    for (_, world_height, _, _) in &sizes {
        assert!((240.0..300.0).contains(world_height), "{world_height}");
    }
    let near = sizes.iter().min_by(|a, b| a.3.total_cmp(&b.3)).unwrap();
    assert!((30.0..55.0).contains(&near.2), "{near:?}");
    // speeds: md1stin is scale 512, md1_1 is 1024
    let area = adapter.load_field("md1stin").unwrap();
    assert_eq!((area.walk_speed(), area.run_speed(), area.max_speed()), (60.0, 240.0, 600.0));
    let md1_1 = adapter.load_field("md1_1").unwrap();
    assert_eq!((md1_1.walk_speed(), md1_1.run_speed()), (120.0, 480.0));
}

/// The back/front split of every real field has the canvas of the full decode, and for `md1stin`
/// (no blended tiles) drawing the front over the back reproduces the full picture exactly.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn depth_layers_of_real_fields() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = LgpArchive::open_path(flevel(&dir)).unwrap();
    let names: Vec<String> = lgp.list().iter().filter(|e| is_field_name(&e.name)).map(|e| e.name.clone()).collect();
    let (mut checked, mut with_both, mut green_back, mut green_full) = (0usize, 0usize, 0usize, 0usize);
    for name in &names {
        let field = load_field_file(&mut lgp, name);
        let full = decode_background(&field).unwrap();
        let split = decode_background_layers(&field).unwrap();
        for half in [&split.back, &split.front] {
            assert_eq!(
                (half.width, half.height, half.origin_x, half.origin_y, half.view_bounds),
                (full.width, full.height, full.origin_x, full.origin_y, full.view_bounds),
                "{name}"
            );
        }
        checked += 1;
        let has = |layer: u8| full.layers.iter().any(|l| l.layer == layer && l.tiles_drawn > 0);
        with_both += usize::from((has(1) || has(3)) && (has(2) || has(4)));
        if name == "md1stin" {
            let blended: usize = full.layers.iter().map(|l| l.tiles_blended).sum();
            assert_eq!(blended, 0);
            let mut over = split.back.rgba.clone();
            for (dst, src) in over.chunks_exact_mut(4).zip(split.front.rgba.chunks_exact(4)) {
                if src[3] == 255 {
                    dst.copy_from_slice(src);
                }
            }
            assert_eq!(over, full.rgba, "front over back is the full composite");
            let is_key = |px: &[u8]| px[3] == 255 && px[0] == 0 && px[1] == 255 && px[2] == 0;
            green_back = split.back.rgba.chunks_exact(4).filter(|p| is_key(p)).count();
            green_full = full.rgba.chunks_exact(4).filter(|p| is_key(p)).count();
        }
    }
    eprintln!("{checked} fields checked, {with_both} have both back and front layers; md1stin pure-green pixels: back half {green_back}, full {green_full}");
    assert_eq!((checked, with_both), (702, 642));
    // the back half alone is not a finished picture: under the layer-2 objects of md1stin it holds
    // a pure-green key colour, which the front half covers completely
    assert_eq!((green_back, green_full), (6_277, 0));
}

/// Every model record of `md1stin` (Cloud, Barret, guards and passers-by) loads, and the first
/// three animations of each (the standing, walking and running clips) fit its skeleton.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn every_md1stin_loader_record_loads_and_its_first_three_animations_fit() {
    let Some(dir) = data_dir() else { return };
    let mut flevel_lgp = LgpArchive::open_path(flevel(&dir)).unwrap();
    let loader = load_field_file(&mut flevel_lgp, "md1stin").model_loader().unwrap();
    let mut lgp = char_lgp(&dir);
    let (mut fit, mut total) = (0usize, 0usize);
    for record in &loader.models {
        let model = load_field_model(&mut lgp, &record.hrc).unwrap_or_else(|e| panic!("{}: {e}", record.hrc));
        assert!(model.triangle_count() > 0, "{}", record.name);
        for (slot, animation) in record.animations.iter().enumerate() {
            let a = load_field_animation(&mut lgp, &animation.stem()).unwrap_or_else(|e| panic!("{}: {e}", animation.name));
            total += 1;
            fit += usize::from(model.fits(&a));
            if slot < 3 {
                assert!(model.fits(&a), "{} slot {slot} ({}) has {} bones, the skeleton {}", record.name, animation.name, a.bone_count(), model.bones.len());
            }
        }
    }
    eprintln!("md1stin: {} records, {fit} of {total} listed animations fit their skeleton", loader.models.len());
    assert_eq!(loader.models.len(), 10);
}
