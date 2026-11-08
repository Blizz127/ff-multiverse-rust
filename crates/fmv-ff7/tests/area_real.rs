//! Checks of the FF7 area hooks (exits, NPCs, arrival placement, encounters) against the owner's
//! REAL FF7 data (FF7 Steam Edition, English).
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` is unset, so a normal
//! `cargo test` never touches game data. Run them (release is much faster for the whole-archive
//! ones) with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-ff7 --test area_real --release -- --ignored --nocapture
//! ```
//!
//! `FMV_FF7_DATA` is the game's `data/` directory (the one containing `field/flevel.lgp`). The
//! files are only ever opened read-only, and what the tests print are counts, field names, entity
//! names, model stems and numbers: no game bytes and no dialog text.

use fmv_adapter_api::{AreaRuntime, GameAdapter, MoveResult};
use fmv_core::{EditionProvenance, GameId, Platform, Pos};
use fmv_ff7::background::Projection;
use fmv_ff7::field::{FieldFile, Walkmesh};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::script::{
    parse_model_list, resolve_gateways, resolve_map_jumps, triangle_contains_xy, triangle_height_at, Arg, EntityInit, FieldScript, Maplist,
    Phase,
};
use fmv_ff7::{decode_encounter_section, field_direction_to_facing, Ff7Adapter, Ff7FieldArea};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

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

fn adapter(dir: &std::path::Path) -> Ff7Adapter {
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
    Ff7Adapter::from_flevel_path(flevel(dir), provenance).unwrap()
}

/// Wall-clock cost of `Ff7Adapter::load_field` for `md1stin`: the first call of a fresh adapter
/// and then the steady state. Prints one line; asserts nothing about speed (the box is shared).
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn load_field_timing_for_md1stin() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    // Warm the page cache with one throw-away read of the entry (the archive is on a shared
    // disk), then measure a fresh adapter's first load and the repeats.
    a.load_field("md1stin").unwrap();
    let fresh = adapter(&dir);
    let started = Instant::now();
    fresh.load_field("md1stin").unwrap();
    let first = started.elapsed();
    let mut runs: Vec<Duration> = (0..30)
        .map(|_| {
            let started = Instant::now();
            fresh.load_field("md1stin").unwrap();
            started.elapsed()
        })
        .collect();
    runs.sort();
    let median = runs[runs.len() / 2];
    let mean = runs.iter().sum::<Duration>() / runs.len() as u32;
    println!(
        "load_field(md1stin): first call of a fresh adapter {:.2} ms; steady state min {:.2} ms, median {:.2} ms, mean {:.2} ms, max {:.2} ms over {} loads",
        first.as_secs_f64() * 1e3,
        runs[0].as_secs_f64() * 1e3,
        median.as_secs_f64() * 1e3,
        mean.as_secs_f64() * 1e3,
        runs[runs.len() - 1].as_secs_f64() * 1e3,
        runs.len()
    );
}

/// The fields `load_area` fails on, by name (the baseline the new hooks must not change).
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn baseline_which_fields_fail_to_load() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    let areas = a.list_areas();
    let started = Instant::now();
    let failed: Vec<String> =
        areas.iter().filter(|info| a.load_area(&info.id.key).is_err()).map(|info| info.id.key.clone()).collect();
    println!("{} areas listed, {} fail to load: {failed:?} ({:.1} s)", areas.len(), failed.len(), started.elapsed().as_secs_f64());
}

// ---------------------------------------------------------------------------------------
// The DIR byte -> heading convention
// ---------------------------------------------------------------------------------------

const TAU: f64 = std::f64::consts::TAU;

/// Angle wrapped into `(-pi, pi]`.
fn wrap(a: f64) -> f64 {
    let r = a.rem_euclid(TAU);
    if r > std::f64::consts::PI {
        r - TAU
    } else {
        r
    }
}

fn is_field_name(name: &str) -> bool {
    !name.contains('.') && !name.eq_ignore_ascii_case("maplist")
}

fn read_field(lgp: &mut LgpArchive<std::fs::File>, name: &str) -> Option<FieldFile> {
    let stored = lgp.read(name).ok()?;
    FieldFile::parse(lzss::decompress(&stored).ok()?).ok()
}

/// Area-weighted centroid of the walkmesh in x and y (z by the same weights).
fn mesh_centroid(mesh: &Walkmesh) -> Option<[f64; 3]> {
    let (mut w, mut cx, mut cy, mut cz) = (0.0, 0.0, 0.0, 0.0);
    for t in &mesh.triangles {
        let p = |i: usize| [f64::from(t[i].x), f64::from(t[i].y), f64::from(t[i].z)];
        let (a, b, c) = (p(0), p(1), p(2));
        let area = ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() / 2.0;
        w += area;
        cx += area * (a[0] + b[0] + c[0]) / 3.0;
        cy += area * (a[1] + b[1] + c[1]) / 3.0;
        cz += area * (a[2] + b[2] + c[2]) / 3.0;
    }
    (w > 0.0).then(|| [cx / w, cy / w, cz / w])
}

/// World heading `atan2(wy, wx)` of the ground direction that appears as "up the screen" through
/// `projection` at the world point `at` (the same inversion `FieldView::screen_to_world_dir` of the
/// client does: the local slope of the mapping, inverted).
fn screen_up_heading(projection: &Projection, at: [f32; 3]) -> Option<f64> {
    const H: f32 = 16.0;
    let o = projection.project(at)?;
    let ax = projection.project([at[0] + H, at[1], at[2]])?;
    let ay = projection.project([at[0], at[1] + H, at[2]])?;
    // field-screen displacement per unit of world x and of world y (screen y points DOWN)
    let (a, c) = (f64::from(ax.x - o.x) / f64::from(H), f64::from(ax.y - o.y) / f64::from(H));
    let (b, d) = (f64::from(ay.x - o.x) / f64::from(H), f64::from(ay.y - o.y) / f64::from(H));
    let det = a * d - b * c;
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    // solve [a b; c d] (wx, wy) = (0, -1): no sideways motion, one unit up
    let (wx, wy) = (b / det, -a / det);
    (wx.hypot(wy) > 1e-12).then(|| wy.atan2(wx))
}

/// Circular mean and mean resultant length of angles.
fn circular(angles: &[f64]) -> (f64, f64) {
    let n = angles.len().max(1) as f64;
    let (c, s) = angles.iter().fold((0.0, 0.0), |(c, s), a| (c + a.cos(), s + a.sin()));
    (s.atan2(c), c.hypot(s) / n)
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    sorted.get(((sorted.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(f64::NAN)
}

/// One field's camera-derived facts.
struct UpFacts {
    name: String,
    control_direction: u8,
    /// Heading of screen-up at the walkmesh centroid (radians).
    up_at_centroid: f64,
    /// Heading of the camera's forward axis projected onto the ground (radians).
    camera_forward: f64,
    /// Length of that projection for a unit forward axis: 1 for a level camera, 0 for one looking
    /// straight down (whose forward heading means nothing).
    forward_ground_len: f64,
}

fn up_facts(lgp: &mut LgpArchive<std::fs::File>) -> Vec<UpFacts> {
    let names: Vec<String> = lgp.list().iter().filter(|e| is_field_name(&e.name)).map(|e| e.name.clone()).collect();
    let mut out = Vec::new();
    for name in names {
        let Some(field) = read_field(lgp, &name) else { continue };
        let (Ok(cameras), Ok(mesh), Ok(triggers)) = (field.cameras(), field.walkmesh(), field.triggers()) else { continue };
        let (Some(camera), Some(c)) = (cameras.first(), mesh_centroid(&mesh)) else { continue };
        let projection = Projection::from_camera(camera);
        let Some(up) = screen_up_heading(&projection, [c[0] as f32, c[1] as f32, c[2] as f32]) else { continue };
        let (fx, fy) = (f64::from(camera.axes[2][0]), f64::from(camera.axes[2][1]));
        out.push(UpFacts {
            name,
            control_direction: triggers.control_direction,
            up_at_centroid: up,
            camera_forward: fy.atan2(fx),
            forward_ground_len: fx.hypot(fy) / 4096.0,
        });
    }
    out
}

/// A field's name, its `control_direction` and a measured heading in radians.
type HeadingRow<'a> = (&'a str, u8, f64);

/// Print how the heading `rows` relate to the byte, free fits first and then the shipped
/// conversion with its worst fields. Returns the share of rows (in percent) within 5 and 20
/// degrees of the shipped conversion's prediction.
fn fit_report(label: &str, rows: &[HeadingRow]) -> (f64, f64) {
    println!("--- {label}: {} fields", rows.len());
    for sign in [1.0f64, -1.0] {
        let residuals: Vec<f64> = rows.iter().map(|(_, cd, h)| wrap(h - sign * TAU * f64::from(*cd) / 256.0)).collect();
        let (offset, resultant) = circular(&residuals);
        let mut spread: Vec<f64> = residuals.iter().map(|r| wrap(r - offset).abs().to_degrees()).collect();
        spread.sort_by(f64::total_cmp);
        let within = |deg: f64| spread.iter().filter(|s| **s <= deg).count() as f64 / spread.len() as f64 * 100.0;
        println!(
            "  free fit rad = {:+}*dir/256*tau + {:7.2} deg ({:6.2}/256 turn): resultant {:.4}; |residual| median {:.1} p90 {:.1} deg; within 5/20 deg: {:.1}/{:.1} %",
            sign,
            offset.to_degrees(),
            offset / TAU * 256.0,
            resultant,
            percentile(&spread, 0.5),
            percentile(&spread, 0.9),
            within(5.0),
            within(20.0)
        );
    }
    // the shipped conversion
    let mut worst: Vec<(f64, &str, u8, f64, f64)> = rows
        .iter()
        .map(|(name, cd, h)| {
            let predicted = f64::from(field_direction_to_facing(*cd));
            (wrap(h - predicted).abs().to_degrees(), *name, *cd, h.to_degrees(), predicted.to_degrees())
        })
        .collect();
    worst.sort_by(|a, b| a.0.total_cmp(&b.0));
    let spread: Vec<f64> = worst.iter().map(|w| w.0).collect();
    let within = |deg: f64| spread.iter().filter(|s| **s <= deg).count() as f64 / spread.len() as f64 * 100.0;
    println!(
        "  SHIPPED field_direction_to_facing(control_direction): |residual| median {:.1} p75 {:.1} p90 {:.1} p99 {:.1} max {:.1} deg; within 2/5/10/20/45 deg: {:.1}/{:.1}/{:.1}/{:.1}/{:.1} %",
        percentile(&spread, 0.5),
        percentile(&spread, 0.75),
        percentile(&spread, 0.9),
        percentile(&spread, 0.99),
        spread.last().copied().unwrap_or(f64::NAN),
        within(2.0),
        within(5.0),
        within(10.0),
        within(20.0),
        within(45.0)
    );
    println!("  worst fields (name, control_direction, measured deg, predicted deg, residual deg):");
    for (res, name, cd, measured, predicted) in worst.iter().rev().take(12) {
        println!("    {name:10} {cd:3} {measured:8.1} {predicted:8.1} {res:7.1}");
    }
    (within(5.0), within(20.0))
}

/// Which linear map from the DIR byte to the world heading reproduces the field's
/// `control_direction` (the direction of "up" for the movement controls) as the heading of
/// "up the screen" through the field camera? Prints the evidence and checks that the shipped
/// conversion ([`field_direction_to_facing`]) is the quarter-turn map the data point at.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn dir_convention_against_control_direction_and_the_camera() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = LgpArchive::open_path(flevel(&dir)).unwrap();
    let facts = up_facts(&mut lgp);
    println!("{} fields with a camera, a walkmesh and a triggers section", facts.len());

    // the camera's forward axis on the ground means nothing for a camera looking (nearly) straight down
    let level: Vec<HeadingRow> =
        facts.iter().filter(|f| f.forward_ground_len >= 0.25).map(|f| (f.name.as_str(), f.control_direction, f.camera_forward)).collect();
    let (within5, within20) = fit_report("heading of the camera's forward axis on the ground (cameras not near-vertical)", &level);
    println!("  ({} fields have a near-vertical camera and are left out of this variant)", facts.len() - level.len());
    // 128 is the most common value by far; split the fields on it to see whether it is a default
    // that says nothing about the camera
    let mut histogram: BTreeMap<u8, usize> = BTreeMap::new();
    for f in &facts {
        *histogram.entry(f.control_direction).or_default() += 1;
    }
    let mut common: Vec<(u8, usize)> = histogram.into_iter().collect();
    common.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("  most common control_direction values (value, fields): {:?}", &common[..common.len().min(8)]);
    let (is_128, other): (Vec<HeadingRow>, Vec<HeadingRow>) = level.iter().partition(|r| r.1 == 128);
    fit_report("camera forward axis, fields whose control_direction is NOT 128", &other);
    fit_report("camera forward axis, fields whose control_direction is 128", &is_128);
    let at_centroid: Vec<HeadingRow> = facts.iter().map(|f| (f.name.as_str(), f.control_direction, f.up_at_centroid)).collect();
    fit_report("heading of 'up the screen' at the walkmesh centroid (all fields)", &at_centroid);
    // the screen-up measure comes out about opposite for some fields: does the forward axis fit them?
    let off_by = |heading: f64, cd: u8| wrap(heading - f64::from(field_direction_to_facing(cd))).abs().to_degrees();
    let flipped: Vec<&UpFacts> = facts.iter().filter(|f| off_by(f.up_at_centroid, f.control_direction) > 90.0).collect();
    let forward_fits = flipped.iter().filter(|f| f.forward_ground_len >= 0.25 && off_by(f.camera_forward, f.control_direction) <= 20.0).count();
    let near_vertical = flipped.iter().filter(|f| f.forward_ground_len < 0.25).count();
    println!(
        "  {} fields are more than 90 deg off under the screen-up measure; the forward axis is within 20 deg for {forward_fits} of them ({near_vertical} have a near-vertical camera); {} of them have control_direction 128",
        flipped.len(),
        flipped.iter().filter(|f| f.control_direction == 128).count()
    );

    println!("--- a handful of fields (control_direction 124 is {:.1} deg of a turn)", 124.0 / 256.0 * 360.0);
    for want in ["md1stin", "md1_1", "md1_2", "mds7st1", "kuro_4", "ancnt1", "nrthmk", "elevtr1", "bwhlin"] {
        if let Some(f) = facts.iter().find(|f| f.name == want) {
            println!(
                "  {:9} control_direction {:3} -> shipped {:7.1} deg; screen-up at centroid {:7.1} deg; camera forward {:7.1} deg (ground length {:.2})",
                f.name,
                f.control_direction,
                f64::from(field_direction_to_facing(f.control_direction)).to_degrees(),
                f.up_at_centroid.to_degrees(),
                f.camera_forward.to_degrees(),
                f.forward_ground_len
            );
        }
    }
    // md1stin, the field the brief names, is within two degrees (control_direction 124)
    let md1stin = facts.iter().find(|f| f.name == "md1stin").unwrap();
    assert!(wrap(md1stin.camera_forward - f64::from(field_direction_to_facing(124))).abs().to_degrees() < 2.0);
    // the shipped map describes most fields (the measured shares are in the doc of the function)
    assert!(within5 > 45.0 && within20 > 70.0, "{within5} {within20}");
}

/// A second, independent look at the same convention: a `MAPJUMP` arrives at a point with a
/// direction byte, and the player usually faces into the field. Over all literal `MAPJUMP`s whose
/// arrival lies inside the triangle they name, how well does each linear map from the byte to the
/// world heading point from the arrival toward the middle of the target's walkmesh?
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn dir_convention_against_mapjump_arrivals() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = LgpArchive::open_path(flevel(&dir)).unwrap();
    let maplist = Maplist::from_archive(&mut lgp).unwrap();
    let names: Vec<String> = lgp.list().iter().filter(|e| is_field_name(&e.name)).map(|e| e.name.clone()).collect();
    // (arrival x, y, direction byte, centroid x, y) per usable MAPJUMP
    let mut samples: Vec<(f64, f64, u8, f64, f64)> = Vec::new();
    let mut meshes: HashMap<String, Option<Walkmesh>> = HashMap::new();
    for name in &names {
        let Some(field) = read_field(&mut lgp, name) else { continue };
        let Ok(script) = FieldScript::from_field(&field) else { continue };
        for j in resolve_map_jumps(&script.map_jumps(), &maplist) {
            let Some(target) = j.target_field else { continue };
            if j.jump.conditional || (j.jump.x, j.jump.y, j.jump.triangle) == (0, 0, 0) {
                continue;
            }
            let mesh = meshes.entry(target.clone()).or_insert_with(|| {
                let real = lgp.find(&target).map(|e| e.name.clone())?;
                read_field(&mut lgp, &real)?.walkmesh().ok()
            });
            let Some(mesh) = mesh.as_ref() else { continue };
            let Some(tri) = usize::try_from(j.jump.triangle).ok().and_then(|t| mesh.triangles.get(t)) else { continue };
            if !triangle_contains_xy(tri, i32::from(j.jump.x), i32::from(j.jump.y)) {
                continue;
            }
            let Some(c) = mesh_centroid(mesh) else { continue };
            // an arrival at the centroid itself says nothing about "inward"
            if (c[0] - f64::from(j.jump.x)).hypot(c[1] - f64::from(j.jump.y)) < 50.0 {
                continue;
            }
            samples.push((f64::from(j.jump.x), f64::from(j.jump.y), j.jump.direction, c[0], c[1]));
        }
    }
    println!(
        "{} MAPJUMPs with literal operands (not in script 0's conditional part) land inside the triangle they name, away from the target's middle",
        samples.len()
    );
    let cosines = |heading_of: &dyn Fn(u8) -> f64| -> Vec<f64> {
        samples.iter().map(|&(x, y, d, cx, cy)| (heading_of(d) - (cy - y).atan2(cx - x)).cos()).collect()
    };
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let score = |sign: f64, offset_units: f64| mean(&cosines(&|d| sign * TAU * f64::from(d) / 256.0 + offset_units / 256.0 * TAU));
    let mut best = (f64::MIN, 0.0, 0.0);
    for sign in [1.0, -1.0] {
        for offset in 0..256 {
            let s = score(sign, f64::from(offset));
            if s > best.0 {
                best = (s, sign, f64::from(offset));
            }
        }
    }
    println!(
        "best of the 512 linear maps: rad = {:+}*dir/256*tau + {:.0}/256 turn ({:.1} deg): mean cos(heading - inward) = {:.3}",
        best.1,
        best.2,
        best.2 / 256.0 * 360.0,
        best.0
    );
    for sign in [1.0f64, -1.0] {
        let by_offset: Vec<String> =
            [0.0, 32.0, 64.0, 96.0, 128.0, 160.0, 192.0, 224.0].iter().map(|o| format!("{o:.0}:{:+.3}", score(sign, *o))).collect();
        println!("sign {sign:+}: mean cos by offset (in 1/256 turns) {}", by_offset.join(" "));
    }
    let shipped = cosines(&|d| f64::from(field_direction_to_facing(d)));
    let m = mean(&shipped);
    let sd = (shipped.iter().map(|c| (c - m).powi(2)).sum::<f64>() / shipped.len() as f64).sqrt();
    let facing_inward = shipped.iter().filter(|c| **c > 0.0).count() as f64 / shipped.len() as f64 * 100.0;
    println!(
        "SHIPPED field_direction_to_facing: mean cos {:.3} (standard error {:.3}; chance would be 0, a perfect map 1); {:.1} % of arrivals face within 90 degrees of inward",
        m,
        sd / (shipped.len() as f64).sqrt(),
        facing_inward
    );
    // the shipped quarter-turn map scores within 0.01 of the best of all 512 and far above chance
    assert!(m > 0.35 && best.0 - m < 0.01, "shipped {m}, best {}", best.0);
    assert_eq!(best.1, 1.0, "counter-clockwise bytes");
}

// ---------------------------------------------------------------------------------------
// Exits
// ---------------------------------------------------------------------------------------

/// The adapter's key rule, restated: 1 to 20 ASCII letters, digits, `_` or `-`.
fn plausible_key(name: &str) -> bool {
    !name.is_empty() && name.len() <= 20 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Lower-cased entry name -> the archive's own spelling; the first entry of a name wins (as in the
/// archive's look-up and in the adapter's index).
fn archive_spelling(lgp: &LgpArchive<std::fs::File>) -> HashMap<String, String> {
    let mut spelled = HashMap::new();
    for e in lgp.list().iter().filter(|e| !e.name.is_empty()) {
        spelled.entry(e.name.to_ascii_lowercase()).or_insert_with(|| e.name.clone());
    }
    spelled
}

/// Every exit of every field arrives at a point inside the walkmesh triangle it names in the
/// target field, `place_arrival` puts it at that triangle's height there, and the exits are exactly
/// the used gateways whose target is an area the archive offers.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn exits_of_every_field_arrive_on_the_hinted_target_triangle() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    let mut lgp = LgpArchive::open_path(flevel(&dir)).unwrap();
    let maplist = Maplist::from_archive(&mut lgp).unwrap();
    let spelled = archive_spelling(&lgp);
    let keys: Vec<String> = a.list_areas().into_iter().map(|i| i.id.key).collect();
    let listed: HashSet<&str> = keys.iter().map(String::as_str).collect();
    assert_eq!(keys.len(), 702);

    let started = Instant::now();
    let mut targets: HashMap<String, Option<Ff7FieldArea>> = HashMap::new();
    let (mut loaded, mut load_failures) = (0usize, Vec::new());
    let (mut used, mut unresolved, mut not_in_archive, mut not_offered) = (0usize, 0usize, 0usize, 0usize);
    let (mut exits_total, mut zero_exit_fields, mut max_exits) = (0usize, 0usize, 0usize);
    let (mut target_load_failures, mut outside_triangle, mut hint_changes_z) = (Vec::new(), Vec::new(), 0usize);
    let mut self_exits = 0usize;
    let (mut accepted_by_the_rules, mut not_accepted) = (0usize, Vec::<String>::new());
    let mut by_count: BTreeMap<usize, usize> = BTreeMap::new();
    for key in &keys {
        let Ok(area) = a.load_field(key) else {
            load_failures.push(key.clone());
            continue;
        };
        loaded += 1;
        let triggers = area.triggers().unwrap();
        let gateways = resolve_gateways(&triggers, &maplist);
        used += gateways.len();
        // the exits as the rule says: used gateway, id in the maplist, name an offered archive entry
        let mut expected = Vec::new();
        for g in &gateways {
            let Some(name) = g.target_field.as_deref() else {
                unresolved += 1;
                continue;
            };
            let Some(spelling) = spelled.get(&name.to_ascii_lowercase()) else {
                not_in_archive += 1;
                continue;
            };
            if spelling.eq_ignore_ascii_case("maplist") || !plausible_key(spelling) {
                not_offered += 1;
                continue;
            }
            expected.push((spelling.clone(), g.clone()));
        }
        let exits = area.exits();
        assert_eq!(exits.len(), expected.len(), "{key}: exits");
        exits_total += exits.len();
        max_exits = max_exits.max(exits.len());
        zero_exit_fields += usize::from(exits.is_empty());
        *by_count.entry(exits.len()).or_default() += 1;
        for (e, (spelling, g)) in exits.iter().zip(&expected) {
            assert_eq!(&e.target, spelling, "{key}");
            assert_eq!(e.line.0, Pos::new(f32::from(g.exit_a[0]), f32::from(g.exit_a[1]), f32::from(g.exit_a[2])), "{key}");
            assert_eq!(e.line.1, Pos::new(f32::from(g.exit_b[0]), f32::from(g.exit_b[1]), f32::from(g.exit_b[2])), "{key}");
            assert_eq!(e.arrival, Pos::new(f32::from(g.destination_x), f32::from(g.destination_y), 0.0), "{key}");
            assert_eq!(e.arrival_hint, g.destination_triangle.map(|t| t as u32), "{key}");
            assert_eq!(e.arrival_facing, None);
            assert!(listed.contains(e.target.as_str()), "{key}: {} is not listed", e.target);
            self_exits += usize::from(e.target.eq_ignore_ascii_case(key));

            let target = targets.entry(e.target.clone()).or_insert_with(|| a.load_field(&e.target).ok());
            let Some(target) = target else {
                target_load_failures.push(format!("{key} -> {}", e.target));
                continue;
            };
            let hint = e.arrival_hint.unwrap();
            let tri = target.walkmesh().triangles.get(hint as usize).unwrap_or_else(|| panic!("{key} -> {}: triangle {hint}", e.target));
            let (x, y) = (e.arrival.x as i32, e.arrival.y as i32);
            if !triangle_contains_xy(tri, x, y) {
                outside_triangle.push(format!("{key} -> {} ({x}, {y}) triangle {hint}", e.target));
                continue;
            }
            let height = triangle_height_at(tri, x, y).unwrap() as f32;
            let placed = target.place_arrival(e.arrival, e.arrival_hint);
            assert_eq!((placed.x, placed.y), (e.arrival.x, e.arrival.y), "{key} -> {}", e.target);
            assert!((placed.z - height).abs() < 1e-3, "{key} -> {}: placed z {} against the triangle's {height}", e.target, placed.z);
            // the trait promises a position the area accepts: the movement rules take the placed
            // point as it is (at most the small wall nudge) and keep it on the hinted floor
            match target.validate_move(placed, placed) {
                MoveResult::Accepted(p)
                    if (p.x - placed.x).abs() <= 0.05 && (p.y - placed.y).abs() <= 0.05 && (p.z - placed.z).abs() <= 1.0 =>
                {
                    accepted_by_the_rules += 1;
                }
                other => not_accepted.push(format!("{key} -> {} at {placed:?}: {other:?}", e.target)),
            }
            // where the default rule (no hint) would have put it, on a layered mesh
            let plain = target.place_arrival(e.arrival, None);
            hint_changes_z += usize::from((plain.z - placed.z).abs() > 1.0);
        }
    }
    println!(
        "{loaded} fields loaded ({} failed: {load_failures:?}) in {:.1} s; {used} used gateways: {exits_total} exits, dropped {unresolved} \
         past the maplist, {not_in_archive} naming no archive entry, {not_offered} naming an entry that is not an area",
        load_failures.len(),
        started.elapsed().as_secs_f64()
    );
    println!("fields with zero exits {zero_exit_fields}, most exits in one field {max_exits}, exits to the field itself {self_exits}; fields by exit count {by_count:?}");
    println!(
        "{} distinct targets loaded ({} failed: {target_load_failures:?}); {} arrivals outside their hinted triangle {outside_triangle:?}; \
         the hint changes the arrival height by more than a unit for {hint_changes_z} exits",
        targets.len(),
        target_load_failures.len(),
        outside_triangle.len()
    );
    println!(
        "{accepted_by_the_rules} placed arrivals are accepted by validate_move as they are (xy within 0.05, z within 1); {} are not: {not_accepted:?}",
        not_accepted.len()
    );
    assert!(not_accepted.is_empty(), "place_arrival returned positions the area does not accept: {not_accepted:?}");
    assert!(load_failures.is_empty(), "load_area failed where it used to succeed: {load_failures:?}");
    assert_eq!(used, 1036, "used gateways of the 702 fields");
    assert_eq!(used, exits_total + unresolved + not_in_archive + not_offered);
    assert_eq!(unresolved, 0, "every gateway id is in the maplist");
    assert!(target_load_failures.is_empty() && outside_triangle.is_empty());
}

fn exit_targets(area: &dyn AreaRuntime) -> Vec<String> {
    area.exits().iter().map(|e| e.target.clone()).collect()
}

/// `md1stin` has the one gateway to `md1_1`; `md1_1` has two, back to `md1stin` and on to `md1_2`.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn md1stin_and_md1_1_exits() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    let md1stin = a.load_field("md1stin").unwrap();
    let md1_1 = a.load_field("md1_1").unwrap();
    for (name, area) in [("md1stin", &md1stin), ("md1_1", &md1_1)] {
        for e in area.exits() {
            println!(
                "{name}: line ({}, {}, {}) - ({}, {}, {}) -> {} arriving at ({}, {}) triangle {:?}",
                e.line.0.x, e.line.0.y, e.line.0.z, e.line.1.x, e.line.1.y, e.line.1.z, e.target, e.arrival.x, e.arrival.y, e.arrival_hint
            );
        }
    }
    assert_eq!(exit_targets(&md1stin), ["md1_1"]);
    assert_eq!(exit_targets(&md1_1), ["md1stin", "md1_2"]);
    // md1stin's exit arrives in md1_1 inside the triangle it names, at that triangle's height
    let exit = md1stin.exits().remove(0);
    let hint = exit.arrival_hint.unwrap() as usize;
    let tri = md1_1.walkmesh().triangles[hint];
    let (x, y) = (exit.arrival.x as i32, exit.arrival.y as i32);
    assert!(triangle_contains_xy(&tri, x, y));
    let placed = md1_1.place_arrival(exit.arrival, exit.arrival_hint);
    assert_eq!((placed.x, placed.y), (exit.arrival.x, exit.arrival.y));
    assert!((f64::from(placed.z) - triangle_height_at(&tri, x, y).unwrap()).abs() < 1e-3);
    println!("md1stin -> md1_1: triangle {hint} of md1_1, arrival z {}", placed.z);
    // and the way back lands on md1stin's own walkmesh
    let back = md1_1.exits().remove(0);
    assert_eq!(back.target, "md1stin");
    let placed = md1stin.place_arrival(back.arrival, back.arrival_hint);
    let tri = md1stin.walkmesh().triangles[back.arrival_hint.unwrap() as usize];
    assert!(triangle_contains_xy(&tri, back.arrival.x as i32, back.arrival.y as i32));
    println!("md1_1 -> md1stin: triangle {:?} of md1stin, arrival {:?}", back.arrival_hint, placed);
}

// ---------------------------------------------------------------------------------------
// NPCs
// ---------------------------------------------------------------------------------------

fn arg_text<T: std::fmt::Display + Copy>(a: &Arg<T>) -> String {
    match a {
        Arg::Literal(v) => v.to_string(),
        Arg::Variable { bank, address } => format!("var[{bank}][{address}]"),
        Arg::Absent => "-".into(),
    }
}

fn phase_text(p: Phase) -> &'static str {
    match p {
        Phase::Init => "init",
        Phase::Main => "main",
    }
}

/// What the script of one entity says, in one line (counts and numbers only).
fn facts(e: &EntityInit) -> String {
    let tracked = |t: &Option<fmv_ff7::script::Tracked<Arg<u8>>>| {
        t.as_ref().map_or("-".to_string(), |t| {
            format!("{}{}/{}", arg_text(&t.value), if t.conditional { " (conditional)" } else { "" }, phase_text(t.phase))
        })
    };
    let placements: Vec<String> = e
        .placements
        .iter()
        .map(|p| {
            format!(
                "{:?}({}, {}, {}, tri {}){}{}",
                p.kind,
                arg_text(&p.x),
                arg_text(&p.y),
                arg_text(&p.z),
                arg_text(&p.triangle),
                if p.conditional { " conditional" } else { "" },
                if p.phase == Phase::Main { " main" } else { "" }
            )
        })
        .collect();
    format!(
        "{:8} CHAR {:5} PC {:5} model {:12} VISI {:22} DIR {:22} placements [{}]",
        e.name,
        e.char_id.is_some(),
        e.player.map_or("-".to_string(), |p| arg_text(&p.value)),
        e.model_ref.as_ref().map_or("-".to_string(), |m| m.hrc.clone()),
        tracked(&e.visible),
        tracked(&e.direction),
        placements.join("; ")
    )
}

/// The rules for an NPC, restated from the facts of the script (a second statement of what
/// `AreaRuntime::npcs` documents): `Ok(position)` or the reason the entity is not reported.
fn verdict(e: &EntityInit, area: &Ff7FieldArea) -> Result<Pos, &'static str> {
    if !e.is_character() {
        return Err("no CHAR: not a field model");
    }
    if e.is_player() {
        return Err("PC: stands for a playable character");
    }
    if e.model_ref.is_none() {
        return Err("no model-loader record");
    }
    if let Some(v) = e.visible {
        if !v.conditional && v.value == Arg::Literal(0) {
            return Err(match v.phase {
                Phase::Init => "hidden: unconditional VISI 0 in the init part",
                Phase::Main => "hidden: unconditional VISI 0 in the main part",
            });
        }
    }
    let Some(p) = e.placements.iter().rev().find(|p| p.phase == Phase::Init && !p.conditional) else {
        return Err(if e.placements.is_empty() {
            "no placement instruction"
        } else if e.placements.iter().any(|p| p.phase == Phase::Main && !p.conditional) {
            "placed only by the main part"
        } else {
            "placed only conditionally"
        });
    };
    let Some((x, y)) = p.xy() else { return Err("placement x or y names a variable") };
    let z = match p.z.literal() {
        Some(z) => f32::from(z),
        None => {
            let named = p.triangle.literal().and_then(|t| area.walkmesh().triangles.get(usize::from(t))).filter(|t| {
                triangle_contains_xy(t, i32::from(x), i32::from(y))
            });
            match named {
                Some(t) => triangle_height_at(t, i32::from(x), i32::from(y)).ok_or("degenerate triangle")? as f32,
                None => area.height_at(f32::from(x), f32::from(y), None).ok_or("no z and the point is on no triangle")?,
            }
        }
    };
    Ok(Pos::new(f32::from(x), f32::from(y), z))
}

fn inits_of(area: &Ff7FieldArea) -> Vec<EntityInit> {
    let field = area.field_file();
    let script = FieldScript::from_field(field).unwrap();
    let models = parse_model_list(field.section(2).unwrap()).unwrap();
    script.entity_inits(Some(&models))
}

/// The NPC lists of `md1stin` and `md1_1`, and why each entity is or is not in them (the guards of
/// `md1stin` are `gu0`, `gu1` and `guadd`).
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn md1stin_and_md1_1_npcs_and_the_guards() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    for name in ["md1stin", "md1_1"] {
        let area = a.load_field(name).unwrap();
        let inits = inits_of(&area);
        println!("=== {name}: {} entities, field scale {}", inits.len(), area.field_scale());
        for e in &inits {
            println!("  {}", facts(e));
            match verdict(e, &area) {
                Ok(p) => println!("      -> NPC at ({}, {}, {})", p.x, p.y, p.z),
                Err(why) => println!("      -> not an NPC: {why}"),
            }
        }
        let npcs = area.npcs();
        println!("--- {name}: {} NPCs returned", npcs.len());
        for n in &npcs {
            println!(
                "  {:8} model {:6} idle {:8} label {:30} at ({}, {}, {}) facing {:.3} rad = {:.1} deg",
                n.entity_name,
                n.appearance.model,
                n.appearance.idle_animation.as_deref().unwrap_or("-"),
                n.appearance.label,
                n.pos.x,
                n.pos.y,
                n.pos.z,
                n.facing,
                n.facing.to_degrees()
            );
        }
        let expected: Vec<String> = inits.iter().filter(|e| verdict(e, &area).is_ok()).map(|e| e.name.clone()).collect();
        let got: Vec<String> = npcs.iter().map(|n| n.entity_name.clone()).collect();
        assert_eq!(got, expected, "{name}: npcs() against the restated rules");
        assert!(npcs.iter().all(|n| n.unconditional));
        if name == "md1stin" {
            for guard in ["gu0", "gu1", "guadd"] {
                let e = inits.iter().find(|e| e.name == guard).unwrap_or_else(|| panic!("{guard} is not an entity of md1stin"));
                match npcs.iter().find(|n| n.entity_name == guard) {
                    Some(n) => println!("GUARD {guard}: RETURNED, model {} at ({}, {}, {})", n.appearance.model, n.pos.x, n.pos.y, n.pos.z),
                    None => println!("GUARD {guard}: NOT returned: {}", verdict(e, &area).unwrap_err()),
                }
            }
        }
    }
}

/// Diagnostic: the entity facts and verdicts of the fields named in `FMV_FF7_FIELDS`
/// (comma-separated), for looking at a field's NPCs by eye. Prints nothing without it.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA) and FMV_FF7_FIELDS"]
fn describe_fields_from_the_environment() {
    let Some(dir) = data_dir() else { return };
    let Ok(names) = std::env::var("FMV_FF7_FIELDS") else { return };
    let a = adapter(&dir);
    for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        let area = a.load_field(name).unwrap();
        println!("=== {name}");
        for e in &inits_of(&area) {
            println!("  {}", facts(e));
            match verdict(e, &area) {
                Ok(p) => println!("      -> NPC at ({}, {}, {})", p.x, p.y, p.z),
                Err(why) => println!("      -> not an NPC: {why}"),
            }
        }
    }
}

/// Over all 702 fields: `npcs()` is exactly what the restated rules select; totals.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn npcs_of_every_field_follow_the_rules() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    let keys: Vec<String> = a.list_areas().into_iter().map(|i| i.id.key).collect();
    let started = Instant::now();
    let (mut fields, mut with_npcs, mut npcs_total, mut max_npcs) = (0usize, 0usize, 0usize, 0usize);
    let mut reasons: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut dir_kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut visi_main_zero_with_placement = Vec::new();
    let (mut off_mesh, mut z_from_walkmesh, mut z_literal) = (0usize, 0usize, 0usize);
    let (mut off_mesh_names, mut conditional_dir_names): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for key in &keys {
        let Ok(area) = a.load_field(key) else { continue };
        fields += 1;
        let inits = inits_of(&area);
        let npcs = area.npcs();
        let expected: Vec<(String, Pos)> = inits.iter().filter_map(|e| Some((e.name.clone(), verdict(e, &area).ok()?))).collect();
        let got: Vec<(String, Pos)> = npcs.iter().map(|n| (n.entity_name.clone(), n.pos)).collect();
        assert_eq!(got, expected, "{key}");
        for n in &npcs {
            assert!(n.pos.is_finite() && n.facing.is_finite() && n.unconditional, "{key}: {n:?}");
            assert!(!n.appearance.model.is_empty() && n.appearance.model.len() <= 8, "{key}: {}", n.appearance.model);
            assert!(n.facing > -std::f32::consts::PI - 1e-5 && n.facing <= std::f32::consts::PI + 1e-5);
        }
        for e in &inits {
            match verdict(e, &area) {
                Ok(_) => {
                    let placement = e.placements.iter().rev().find(|p| p.phase == Phase::Init && !p.conditional).unwrap();
                    if placement.z.literal().is_some() {
                        z_literal += 1;
                    } else {
                        z_from_walkmesh += 1;
                    }
                    let (x, y) = placement.xy().unwrap();
                    if area.height_at(f32::from(x), f32::from(y), None).is_none() {
                        off_mesh += 1;
                        off_mesh_names.push(format!("{key}/{} at ({x}, {y})", e.name));
                    }
                    let kind = match e.direction {
                        None => "no DIR",
                        Some(d) if d.value.literal().is_none() => "DIR names a variable",
                        Some(d) if d.conditional => {
                            conditional_dir_names.push(format!("{key}/{}", e.name));
                            "DIR only conditional"
                        }
                        Some(_) => "DIR unconditional literal",
                    };
                    *dir_kinds.entry(kind).or_default() += 1;
                }
                Err(why) => *reasons.entry(why).or_default() += 1,
            }
            // entities the main-part VISI 0 rule hides although the init part places them
            if e.is_character() && !e.is_player() && e.model_ref.is_some() {
                if let Some(v) = e.visible {
                    let placed = e.placements.iter().any(|p| p.phase == Phase::Init && !p.conditional && p.xy().is_some());
                    if placed && !v.conditional && v.value == Arg::Literal(0) && v.phase == Phase::Main {
                        visi_main_zero_with_placement.push(format!("{key}/{}", e.name));
                    }
                }
            }
        }
        npcs_total += npcs.len();
        max_npcs = max_npcs.max(npcs.len());
        with_npcs += usize::from(!npcs.is_empty());
    }
    println!(
        "{fields} fields, {npcs_total} NPCs in {with_npcs} of them (most in one field: {max_npcs}), {:.1} s; z literal {z_literal}, z from the walkmesh {z_from_walkmesh}, NPC points on no triangle {off_mesh}",
        started.elapsed().as_secs_f64()
    );
    println!("entities not reported, by reason: {reasons:#?}");
    println!("DIR of the reported NPCs: {dir_kinds:?}");
    println!("NPCs on no walkmesh triangle (reported at their literal positions): {off_mesh_names:?}");
    println!("NPCs whose only DIR is conditional: {conditional_dir_names:?}");
    println!(
        "{} entities placed by the init part but hidden by an unconditional MAIN-part VISI 0 (the rule this adapter adds to the init-part one): {:?}",
        visi_main_zero_with_placement.len(),
        visi_main_zero_with_placement
    );
    assert_eq!(fields, 702);
}

// ---------------------------------------------------------------------------------------
// Encounters, and the whole archive
// ---------------------------------------------------------------------------------------

/// The table `AreaRuntime::encounters` reports is the one `decode_encounter_section` reads out of
/// the field's own section 6, for all 702 fields; the facts the decoder's docs state hold.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn encounters_through_the_area_equal_the_decoded_section_for_every_field() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    let keys: Vec<String> = a.list_areas().into_iter().map(|i| i.id.key).collect();
    let mut section_lengths: BTreeMap<usize, usize> = BTreeMap::new();
    let mut weight_sums: BTreeMap<u32, usize> = BTreeMap::new();
    let (mut fields, mut with_table, mut entries) = (0usize, 0usize, 0usize);
    for key in &keys {
        let area = a.load_field(key).unwrap();
        let bytes = area.field_file().section(6).unwrap();
        fields += 1;
        *section_lengths.entry(bytes.len()).or_default() += 1;
        let want = decode_encounter_section(bytes);
        assert_eq!(area.encounters(), want, "{key}");
        assert_eq!(a.load_area(key).unwrap().encounters(), want, "{key}: through the trait object");
        if let Some(table) = &want {
            with_table += 1;
            entries += table.entries.len();
            *weight_sums.entry(table.total_weight()).or_default() += 1;
            assert!(!table.is_empty(), "{key}");
        }
    }
    println!(
        "{fields} fields; section 6 lengths {section_lengths:?}; {with_table} with a random-encounter table ({entries} entries); table weight sums {weight_sums:?}"
    );
    let md1stin = a.load_field("md1stin").unwrap().encounters().unwrap();
    println!("md1stin: rate {}, entries {:?}", md1stin.rate, md1stin.entries);
    assert_eq!(fields, 702);
    assert_eq!(section_lengths, BTreeMap::from([(48usize, 702usize)]), "two 24-byte tables in every field");
    assert!(with_table > 50 && with_table < fields, "{with_table}");
    assert_eq!(weight_sums.keys().copied().collect::<Vec<_>>(), [64], "the weights of every real table add up to 64");
}

/// How big the script and model-loader sections of the real fields get (the NPC analysis reads
/// both; this is the headroom of `MAX_NPC_SECTION_BYTES`).
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn script_and_model_loader_sections_are_small() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    let keys: Vec<String> = a.list_areas().into_iter().map(|i| i.id.key).collect();
    let (mut max_script, mut max_loader, mut max_entities) = ((0usize, String::new()), (0usize, String::new()), 0usize);
    for key in &keys {
        let area = a.load_field(key).unwrap();
        let field = area.field_file();
        let script = field.section(0).unwrap().len();
        let loader = field.section(2).unwrap().len();
        if script > max_script.0 {
            max_script = (script, key.clone());
        }
        if loader > max_loader.0 {
            max_loader = (loader, key.clone());
        }
        max_entities = max_entities.max(FieldScript::from_field(field).unwrap().entity_count());
    }
    println!("largest script section {} bytes ({}), largest model-loader section {} bytes ({}), most entities {}", max_script.0, max_script.1, max_loader.0, max_loader.1, max_entities);
    assert!(max_script.0 < fmv_ff7::MAX_NPC_SECTION_BYTES / 8 && max_loader.0 < fmv_ff7::MAX_NPC_SECTION_BYTES / 8);
}

/// Over all 702 fields: `load_area` succeeds exactly where it did before the hooks existed (it
/// failed for none), every hook runs without a panic and returns sane values; totals.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn every_field_still_loads_and_every_hook_runs() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    let areas = a.list_areas();
    assert_eq!(areas.len(), 702);
    let started = Instant::now();
    let mut failed = Vec::new();
    let (mut unnamed, mut duplicate_names): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let (mut exits, mut npcs, mut max_npcs, mut max_exits) = (0usize, 0usize, 0usize, 0usize);
    let (mut zero_exit_fields, mut zero_npc_fields, mut tables) = (0usize, 0usize, 0usize);
    for info in &areas {
        let area = match a.load_area(&info.id.key) {
            Ok(area) => area,
            Err(e) => {
                failed.push(format!("{}: {e}", info.id.key));
                continue;
            }
        };
        let (e, n) = (area.exits(), area.npcs());
        for exit in &e {
            assert!(exit.line.0.is_finite() && exit.line.1.is_finite() && exit.arrival.is_finite(), "{}", info.id.key);
            assert!(areas.iter().any(|x| x.id.key == exit.target), "{}: {} is not listed", info.id.key, exit.target);
        }
        for npc in &n {
            assert!(npc.pos.is_finite() && npc.facing.is_finite(), "{}", info.id.key);
            if npc.entity_name.is_empty() {
                unnamed.push(info.id.key.clone());
            }
        }
        let distinct: HashSet<&str> = n.iter().map(|npc| npc.entity_name.as_str()).collect();
        if distinct.len() != n.len() {
            duplicate_names.push(format!("{} ({} NPCs, {} names)", info.id.key, n.len(), distinct.len()));
        }
        // the arrival hook never panics, hint or not
        for spawn in area.spawn_points() {
            assert!(area.place_arrival(spawn, None).is_finite() && area.place_arrival(spawn, Some(0)).is_finite());
        }
        exits += e.len();
        max_exits = max_exits.max(e.len());
        zero_exit_fields += usize::from(e.is_empty());
        npcs += n.len();
        max_npcs = max_npcs.max(n.len());
        zero_npc_fields += usize::from(n.is_empty());
        tables += usize::from(area.encounters().is_some());
    }
    println!(
        "{} fields listed, {} failed to load {failed:?} (baseline before the hooks: none); {exits} exits (most in one field {max_exits}, {zero_exit_fields} fields with none), {npcs} NPCs (most in one field {max_npcs}, {zero_npc_fields} fields with none), {tables} encounter tables; {:.1} s",
        areas.len(),
        failed.len(),
        started.elapsed().as_secs_f64()
    );
    println!("NPCs with an empty entity name: {} {unnamed:?}; fields where two NPCs share a name: {} {duplicate_names:?}", unnamed.len(), duplicate_names.len());
    assert!(failed.is_empty(), "load_area failed where it used to succeed: {failed:?}");
}
