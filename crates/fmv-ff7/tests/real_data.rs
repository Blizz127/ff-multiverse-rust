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

use fmv_adapter_api::{AreaRuntime, GameAdapter, MoveResult};
use fmv_core::{EditionProvenance, GameId, Platform, Pos};
use fmv_ff7::background::{decode_background, decode_background_with, BackgroundOptions, GroupFilter, Projection};
use fmv_ff7::field::{FieldFile, NO_ACCESS};
use fmv_ff7::lgp::{LgpArchive, MAX_ENTRY_LEN};
use fmv_ff7::{lzss, Ff7Adapter};
use std::collections::HashSet;
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
