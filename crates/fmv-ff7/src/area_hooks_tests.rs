//! Unit tests of the area hooks (`exits`, `npcs`, `place_arrival`, `encounters`) through the
//! adapter's public path (`load_field` / `load_area`), over small SYNTHETIC archives. Nothing
//! here is game data; the scripts, names and numbers are invented.

use crate::area::Ff7Adapter;
use crate::area_content::{field_direction_to_facing, npc_spawns, npc_spawns_capped, MAX_NPC_LABEL_CHARS};
use crate::field::FieldFile;
use crate::testutil::{asm, build_lgp, maplist_bytes, pack_literals, script_section, FieldSpec, GatewaySpec, LoaderModelSpec};
use fmv_adapter_api::{AdapterError, AreaRuntime, EncounterEntry, EncounterTable, ExitKind, GameAdapter, NpcSpawn};
use fmv_core::{EditionProvenance, GameId, Platform, Pos, SourceRef};
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn provenance() -> EditionProvenance {
    EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Synthetic,
        edition: "synthetic test edition".into(),
        region: "none".into(),
        language: "none".into(),
        revision: "test".into(),
        sources: vec![SourceRef::Fixture],
        modded: false,
    }
}

fn entry(spec: &FieldSpec) -> Vec<u8> {
    pack_literals(&spec.build())
}

fn adapter_over(entries: &[(&str, Vec<u8>)]) -> Ff7Adapter {
    let refs: Vec<(&str, &[u8])> = entries.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    Ff7Adapter::from_flevel_reader(Cursor::new(build_lgp(&refs)), provenance()).unwrap()
}

fn gateway(exit_a: [i16; 3], exit_b: [i16; 3], destination: [i16; 3], target_field_id: u16) -> GatewaySpec {
    GatewaySpec { exit_a, exit_b, destination, target_field_id }
}

const UNUSED: u16 = 0x7FFF;

// ---------------------------------------------------------------------------------------
// exits
// ---------------------------------------------------------------------------------------

/// Maplist ids of the exits archive: 0 `src`, 1 `tgt1`, 2 `TGT2` (the archive spells it `tgt2`),
/// 3 `ghost` (no such entry), 4 `bad.name` (an entry, but not offered as an area), 5 `maplist`
/// (the name list is not an area), 6 the empty name, 7 `SRC` (another spelling of the first).
const EXIT_NAMES: [&str; 8] = ["src", "tgt1", "TGT2", "ghost", "bad.name", "maplist", "", "SRC"];

fn exits_archive() -> Vec<(&'static str, Vec<u8>)> {
    let src = FieldSpec {
        gateways: Some(vec![
            gateway([0, 0, 1], [0, 100, 2], [10, 20, 1], 1),
            gateway([9, 9, 9], [9, 9, 9], [9, 9, 9], UNUSED),
            gateway([100, 0, 3], [100, 100, 4], [-5, 40, 0], 2),
            gateway([1, 1, 1], [2, 2, 2], [1, 1, 1], 3),
            gateway([1, 1, 1], [2, 2, 2], [1, 1, 1], 4),
            gateway([1, 1, 1], [2, 2, 2], [1, 1, 1], 5),
            gateway([1, 1, 1], [2, 2, 2], [1, 1, 1], 99),
            gateway([1, 1, 1], [2, 2, 2], [1, 1, 1], 6),
            gateway([-7, 8, -9], [7, -8, 9], [3, 4, -1], 0),
            gateway([5, 5, 5], [6, 6, 6], [30, 31, 1], 7),
        ]),
        ..FieldSpec::default()
    };
    let plain = entry(&FieldSpec::default());
    vec![
        ("maplist", maplist_bytes(&EXIT_NAMES)),
        ("src", entry(&src)),
        ("tgt1", plain.clone()),
        ("tgt2", plain.clone()),
        ("bad.name", plain),
    ]
}

#[test]
fn exits_are_the_used_gateways_with_archive_spelled_targets() {
    let a = adapter_over(&exits_archive());
    let area = a.load_field("src").unwrap();
    let exits = area.exits();
    // unused slot, unknown id, missing entry, key-rule failure, the name list and the empty name are dropped
    let targets: Vec<&str> = exits.iter().map(|e| e.target.as_str()).collect();
    assert_eq!(targets, ["tgt1", "tgt2", "src", "src"], "slot order, archive spelling");
    // the first one in full
    let first = &exits[0];
    assert_eq!(first.line, (Pos::new(0.0, 0.0, 1.0), Pos::new(0.0, 100.0, 2.0)));
    assert_eq!(first.arrival, Pos::new(10.0, 20.0, 0.0), "x, y from the gateway; z is the placeholder");
    assert_eq!(first.arrival_hint, Some(1), "the target triangle index");
    assert_eq!(first.arrival_facing, None, "gateways carry no heading");
    assert_eq!(first.kind, ExitKind::Gateway);
    // negative coordinates and a damaged (negative) triangle: no hint
    let own = &exits[2];
    assert_eq!(own.line, (Pos::new(-7.0, 8.0, -9.0), Pos::new(7.0, -8.0, 9.0)));
    assert_eq!(own.arrival, Pos::new(3.0, 4.0, 0.0));
    assert_eq!(own.arrival_hint, None);
    // the second spelling of the same field resolves to the same key
    assert_eq!((exits[3].target.as_str(), exits[3].arrival_hint), ("src", Some(1)));
    // through the trait object, as the server loads areas
    assert_eq!(a.load_area("src").unwrap().exits(), exits);
    // every target is something the adapter lists, and loads
    let listed: Vec<String> = a.list_areas().into_iter().map(|i| i.id.key).collect();
    for e in &exits {
        assert!(listed.contains(&e.target), "{} is not listed in {listed:?}", e.target);
        assert!(a.load_area(&e.target).is_ok());
    }
    // the other fields' only gateway is the default synthetic one, which names id 291: past this
    // eight-name maplist, so they have no exits
    assert!(a.load_field("tgt1").unwrap().exits().is_empty());
}

#[test]
fn a_gateway_past_the_maplist_is_dropped_and_the_default_field_has_no_exits_without_one() {
    // the default synthetic field's only gateway targets id 0x0123 = 291
    let plain = entry(&FieldSpec::default());
    let short = adapter_over(&[("maplist", maplist_bytes(&["a", "b"])), ("fld", plain.clone())]);
    assert!(short.load_field("fld").unwrap().exits().is_empty());
    // a maplist long enough to name id 291 finds the target
    let names: Vec<String> = (0..300).map(|i| if i == 291 { "fld".to_string() } else { format!("n{i}") }).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let long = adapter_over(&[("maplist", maplist_bytes(&refs)), ("fld", plain)]);
    let exits = long.load_field("fld").unwrap().exits();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].target, "fld");
    assert_eq!((exits[0].arrival, exits[0].arrival_hint), (Pos::new(7.0, 8.0, 0.0), Some(9)), "the legacy gateway's destination 7, 8, 9");
}

#[test]
fn a_missing_or_damaged_maplist_means_no_exits_and_never_an_error() {
    let src = entry(&FieldSpec {
        gateways: Some(vec![gateway([0, 0, 0], [0, 10, 0], [1, 2, 0], 0)]),
        ..FieldSpec::default()
    });
    // no maplist entry at all
    let none = adapter_over(&[("src", src.clone())]);
    assert!(none.maplist().is_none());
    assert!(none.load_field("src").unwrap().exits().is_empty());
    assert!(none.load_area("src").is_ok());
    // a maplist that promises more names than it holds, one that is empty, and one that is junk
    for bad in [b"\x05\x00abc".to_vec(), Vec::new(), vec![0xFF; 9]] {
        let a = adapter_over(&[("maplist", bad), ("src", src.clone())]);
        assert!(a.maplist().is_none());
        assert!(a.load_field("src").unwrap().exits().is_empty());
    }
    // a good one next to it works (the gateway above names id 0 = the field itself)
    let good = adapter_over(&[("maplist", maplist_bytes(&["src"])), ("src", src)]);
    assert_eq!(good.load_field("src").unwrap().exits().len(), 1);
}

#[test]
fn a_damaged_triggers_section_means_no_exits() {
    let spec = FieldSpec { overrides: vec![(7, vec![1, 2, 3])], ..FieldSpec::default() };
    let a = adapter_over(&[("maplist", maplist_bytes(&["f"])), ("f", entry(&spec))]);
    let area = a.load_field("f").unwrap();
    assert!(area.exits().is_empty());
    assert!(area.triggers().is_err());
}

#[test]
fn the_maplist_is_read_once_per_adapter() {
    let a = adapter_over(&exits_archive());
    let first = a.maplist().unwrap();
    assert_eq!(first.len(), EXIT_NAMES.len());
    for _ in 0..3 {
        a.load_field("src").unwrap();
        assert!(Arc::ptr_eq(&first, &a.maplist().unwrap()), "one parsed list, shared");
    }
}

/// A reader whose reads can be made to fail, like a flaky network share.
struct Flaky {
    inner: Cursor<Vec<u8>>,
    fail: Arc<AtomicBool>,
}
impl Read for Flaky {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "share went away"));
        }
        self.inner.read(buf)
    }
}
impl Seek for Flaky {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.inner.seek(from)
    }
}

#[test]
fn a_transient_read_error_is_not_remembered_as_a_missing_maplist() {
    let refs: Vec<(&str, Vec<u8>)> = exits_archive();
    let slices: Vec<(&str, &[u8])> = refs.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let fail = Arc::new(AtomicBool::new(false));
    let reader = Flaky { inner: Cursor::new(build_lgp(&slices)), fail: Arc::clone(&fail) };
    let a = Ff7Adapter::from_flevel_reader(reader, provenance()).unwrap();
    // the share goes away: no maplist now, and that is not cached
    fail.store(true, Ordering::SeqCst);
    assert!(a.maplist().is_none());
    // it comes back: the next call reads again
    fail.store(false, Ordering::SeqCst);
    let list = a.maplist().expect("read again");
    assert_eq!(list.name(1), Some("tgt1"));
    // and a good read is cached: breaking the reader again changes nothing
    fail.store(true, Ordering::SeqCst);
    assert!(Arc::ptr_eq(&list, &a.maplist().unwrap()));
    fail.store(false, Ordering::SeqCst);
    assert_eq!(a.load_field("src").unwrap().exits().len(), 4);
}

// ---------------------------------------------------------------------------------------
// place_arrival
// ---------------------------------------------------------------------------------------

/// Two floors over the same ground (triangles 0 and 2 overlap, at heights 0 and 100) plus a
/// second ground triangle; every edge blocked, so nothing is linked.
fn layered_area() -> Box<dyn AreaRuntime> {
    let spec = FieldSpec::with_mesh(
        vec![
            [(0, 0, 0), (200, 0, 0), (200, 200, 0)],
            [(0, 0, 0), (200, 200, 0), (0, 200, 0)],
            [(0, 0, 100), (200, 0, 100), (200, 200, 100)],
        ],
        vec![[0xFFFF; 3]; 3],
    );
    adapter_over(&[("layers", entry(&spec))]).load_area("layers").unwrap()
}

#[test]
fn place_arrival_puts_the_arrival_on_the_hinted_triangle() {
    let area = layered_area();
    let at = |x: f32, y: f32| Pos::new(x, y, 0.0);
    // (150, 50) is under both floors: the hint chooses, whatever the arrival z says
    assert_eq!(area.place_arrival(at(150.0, 50.0), Some(0)), Pos::new(150.0, 50.0, 0.0));
    assert_eq!(area.place_arrival(at(150.0, 50.0), Some(2)), Pos::new(150.0, 50.0, 100.0));
    // without a hint the default rule applies: the surface nearest the arrival's own z
    assert_eq!(area.place_arrival(at(150.0, 50.0), None), Pos::new(150.0, 50.0, 0.0));
    assert_eq!(area.place_arrival(Pos::new(150.0, 50.0, 100.0), None), Pos::new(150.0, 50.0, 100.0));
    // a point on the shared edge belongs to both triangles (borders count)
    assert_eq!(area.place_arrival(at(100.0, 100.0), Some(1)), Pos::new(100.0, 100.0, 0.0));
    // fractional coordinates are rounded for the containment test and kept in the result
    assert_eq!(area.place_arrival(at(150.4, 49.6), Some(2)), Pos::new(150.4, 49.6, 100.0));
}

#[test]
fn place_arrival_interpolates_the_height_of_a_sloped_triangle() {
    // the default field: two triangles on the plane z = x + y
    let area = adapter_over(&[("slope", entry(&FieldSpec::default()))]).load_area("slope").unwrap();
    assert_eq!(area.place_arrival(Pos::new(30.0, 10.0, 0.0), Some(0)), Pos::new(30.0, 10.0, 40.0));
    assert_eq!(area.place_arrival(Pos::new(10.0, 30.0, 0.0), Some(1)), Pos::new(10.0, 30.0, 40.0));
}

#[test]
fn place_arrival_falls_back_to_the_default_rule_when_the_hint_does_not_fit() {
    let area = layered_area();
    let spawns = area.spawn_points();
    let at = |x: f32, y: f32| Pos::new(x, y, 0.0);
    // a hint past the mesh, and a triangle that does not hold the point: the point is still on the
    // mesh (triangle 0), so the default rule settles it there
    assert_eq!(area.place_arrival(at(150.0, 50.0), Some(99)), Pos::new(150.0, 50.0, 0.0));
    assert_eq!(area.place_arrival(at(150.0, 50.0), Some(1)), Pos::new(150.0, 50.0, 0.0));
    assert_eq!(area.place_arrival(at(150.0, 50.0), Some(u32::MAX)), Pos::new(150.0, 50.0, 0.0));
    // a point off the mesh goes where the area puts such an entity, hint or not
    for hint in [None, Some(0), Some(2), Some(99)] {
        let p = area.place_arrival(at(900.0, 900.0), hint);
        assert!(spawns.contains(&p), "{p:?} is not a spawn point of {spawns:?}");
    }
    // garbage never panics and still lands on a spawn point
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30] {
        for hint in [None, Some(0), Some(2)] {
            let p = area.place_arrival(Pos::new(bad, 50.0, 0.0), hint);
            assert!(p.is_finite(), "{p:?}");
            let p = area.place_arrival(Pos::new(150.0, bad, bad), hint);
            assert!(p.is_finite(), "{p:?}");
        }
    }
}

// ---------------------------------------------------------------------------------------
// npcs
// ---------------------------------------------------------------------------------------

/// The script of an entity in the NPC archive: `CHAR n` first (every entity of the list that has
/// one takes the next loader record), then the pieces.
fn char_then(model: u8, parts: &[Vec<u8>]) -> Vec<u8> {
    asm::cat(&[asm::char_model(model), asm::cat(parts)])
}

/// Entities in script order, each with the bytes of its script 0, and what each is expected to be.
/// The default synthetic mesh is the plane z = x + y over the square 0..100, triangle 0 below the
/// diagonal.
fn npc_entities() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        // a player: has CHAR and PC and a placement, and is not an NPC
        ("hero", char_then(0, &[asm::player(0), asm::xyzi(50, 20, 70, 0), asm::ret()])),
        // returned: literal z (91 is not the plane's 90), DIR 64, shown
        ("guard1", char_then(1, &[asm::xyzi(60, 30, 91, 0), asm::direction(64), asm::visible(1), asm::ret()])),
        // returned: XYI takes the height of the triangle it names (z = 40 + 10)
        ("guard2", char_then(2, &[asm::xyi(40, 10, 0), asm::direction(192), asm::ret()])),
        // hidden by an unconditional VISI 0 in the init part
        ("late", char_then(3, &[asm::xyzi(10, 5, 15, 0), asm::visible(0), asm::ret()])),
        // placed only on some paths
        ("maybe", char_then(4, &[asm::skipped(&asm::xyzi(20, 10, 30, 0)), asm::ret()])),
        // placed only by the main part (after the first RET)
        ("mainly", char_then(5, &[asm::ret(), asm::xyzi(30, 10, 40, 0), asm::idle()])),
        // XYI on a point no triangle holds: no height, no NPC
        ("offmesh", char_then(6, &[asm::xyi(500, 500, 0), asm::ret()])),
        // a placement whose x names a variable
        ("varpos", char_then(7, &[asm::xyzi_var_x(3, 10, 20, 0), asm::ret()])),
        // returned: XYZ, no DIR, no VISI
        ("plain", char_then(8, &[asm::xyz(70, 20, 12), asm::ret()])),
        // a director: no CHAR, so not a field model
        ("nochar", asm::cat(&[asm::xyzi(1, 1, 2, 0), asm::ret()])),
        // hidden at first, shown for good by the main part: shown
        ("shown", char_then(9, &[asm::xyzi(80, 10, 90, 0), asm::visible(0), asm::ret(), asm::visible(1), asm::idle()])),
        // shown by the init part, hidden for good by the main part: hidden
        ("hidemain", char_then(10, &[asm::xyzi(80, 20, 100, 0), asm::ret(), asm::visible(0), asm::idle()])),
        // returned, with a variable DIR (the byte is not known: direction 0), and a later
        // unconditional placement that wins
        ("moved", char_then(11, &[asm::xyzi(1, 1, 2, 0), asm::xyzi(75, 25, 100, 0), asm::direction_var(9), asm::ret()])),
        // a CHAR beyond the loader's records: no model, no NPC
        ("nomodel", char_then(12, &[asm::xyzi(10, 2, 12, 0), asm::ret()])),
    ]
}

fn npc_loader_models() -> Vec<LoaderModelSpec> {
    let long_name = "n".repeat(MAX_NPC_LABEL_CHARS + 20);
    vec![
        LoaderModelSpec::new("md.hero", "AAAA.HRC", &["IDLE.aki", "WALK.aki"]), // 0 hero
        LoaderModelSpec::new("md.guard", "Bbbb.Hrc", &["STAND.aki", "WALK.aki"]), // 1 guard1
        LoaderModelSpec::new("md.guard2", "CCCC.HRC", &[]),                    // 2 guard2: no animation
        LoaderModelSpec::new("md.late", "DDDD.HRC", &["X.aki"]),               // 3
        LoaderModelSpec::new("md.maybe", "EEEE.HRC", &["X.aki"]),              // 4
        LoaderModelSpec::new("md.mainly", "FFFF.HRC", &["X.aki"]),             // 5
        LoaderModelSpec::new("md.offmesh", "GGGG.HRC", &["X.aki"]),            // 6
        LoaderModelSpec::new("md.varpos", "HHHH.HRC", &["X.aki"]),             // 7
        LoaderModelSpec::new(&long_name, "IIII.HRC", &["PLAIN.aki"]),          // 8 plain
        LoaderModelSpec::new("md.shown", "JJJJ.HRC", &["X.aki"]),              // 9
        LoaderModelSpec::new("md.hidemain", "KKKK.HRC", &["X.aki"]),           // 10
        LoaderModelSpec::new("md.moved", "LLLL.HRC", &["X.aki"]),              // 11
        // record 12 does not exist: `nomodel` is the thirteenth CHAR
    ]
}

fn npc_field_spec() -> FieldSpec {
    let entities = npc_entities();
    let refs: Vec<(&str, Vec<u8>)> = entities.iter().map(|(n, c)| (*n, c.clone())).collect();
    FieldSpec { models: npc_loader_models(), overrides: vec![(0, script_section(&refs, 512))], ..FieldSpec::default() }
}

fn npc_area() -> Box<dyn AreaRuntime> {
    adapter_over(&[("npcs", entry(&npc_field_spec()))]).load_area("npcs").unwrap()
}

fn find<'a>(npcs: &'a [NpcSpawn], name: &str) -> Option<&'a NpcSpawn> {
    npcs.iter().find(|n| n.entity_name == name)
}

#[test]
fn npcs_are_the_unconditionally_placed_visible_non_player_models() {
    let npcs = npc_area().npcs();
    let names: Vec<&str> = npcs.iter().map(|n| n.entity_name.as_str()).collect();
    // `hero` (player), `late` (hidden), `maybe` (conditional), `mainly` (main part only), `offmesh`
    // (no height), `varpos` (variable), `nochar` (no CHAR), `hidemain` (hidden by the main part)
    // and `nomodel` (no loader record) are not reported
    assert_eq!(names, ["guard1", "guard2", "plain", "shown", "moved"]);
    assert!(npcs.iter().all(|n| n.unconditional));
}

#[test]
fn npc_positions_use_the_literal_z_else_the_walkmesh() {
    let npcs = npc_area().npcs();
    assert_eq!(find(&npcs, "guard1").unwrap().pos, Pos::new(60.0, 30.0, 91.0), "the literal z, not the plane's 90");
    assert_eq!(find(&npcs, "guard2").unwrap().pos, Pos::new(40.0, 10.0, 50.0), "XYI: the height of its triangle");
    assert_eq!(find(&npcs, "plain").unwrap().pos, Pos::new(70.0, 20.0, 12.0));
    assert_eq!(find(&npcs, "shown").unwrap().pos, Pos::new(80.0, 10.0, 90.0));
    assert_eq!(find(&npcs, "moved").unwrap().pos, Pos::new(75.0, 25.0, 100.0), "the last unconditional placement of the init part wins");
}

#[test]
fn npc_facing_goes_through_the_one_conversion() {
    let npcs = npc_area().npcs();
    assert_eq!(find(&npcs, "guard1").unwrap().facing, field_direction_to_facing(64));
    assert_eq!(find(&npcs, "guard2").unwrap().facing, field_direction_to_facing(192));
    // no DIR, and a DIR that names a variable: the engine's initial direction, byte 0
    assert_eq!(find(&npcs, "plain").unwrap().facing, field_direction_to_facing(0));
    assert_eq!(find(&npcs, "moved").unwrap().facing, field_direction_to_facing(0));
}

#[test]
fn npc_appearance_comes_from_the_model_loader_record() {
    let npcs = npc_area().npcs();
    let guard1 = find(&npcs, "guard1").unwrap();
    assert_eq!(guard1.appearance.model, "bbbb", "lower-case stem of the skeleton entry");
    assert_eq!(guard1.appearance.idle_animation.as_deref(), Some("stand"), "the first animation's stem");
    assert_eq!(guard1.appearance.label, "md.guard");
    // a record without animations has no idle clip
    let guard2 = find(&npcs, "guard2").unwrap();
    assert_eq!((guard2.appearance.model.as_str(), guard2.appearance.idle_animation.as_deref()), ("cccc", None));
    // a very long record name is cut
    let plain = find(&npcs, "plain").unwrap();
    assert_eq!(plain.appearance.label.chars().count(), MAX_NPC_LABEL_CHARS);
    assert_eq!(plain.appearance.model, "iiii");
    // the record is the one the entity's place among the CHAR entities names, not the operand
    assert_eq!(find(&npcs, "moved").unwrap().appearance.model, "llll");
    assert_eq!(find(&npcs, "shown").unwrap().appearance.model, "jjjj");
}

#[test]
fn an_entity_whose_model_record_is_missing_is_not_an_npc() {
    // 13 CHAR entities, 12 records: `nomodel` (the thirteenth) has none
    let a = adapter_over(&[("npcs", entry(&npc_field_spec()))]);
    let area = a.load_field("npcs").unwrap();
    assert!(find(&area.npcs(), "nomodel").is_none());
}

#[test]
fn the_npc_list_is_capped() {
    let spec = npc_field_spec();
    let field = FieldFile::parse(spec.build()).unwrap();
    let mesh = field.walkmesh().unwrap();
    let all = npc_spawns_capped(&field, &mesh, |_, _| None, 100);
    assert!(all.len() >= 3, "{}", all.len());
    for cap in [0usize, 1, 2] {
        assert_eq!(npc_spawns_capped(&field, &mesh, |_, _| None, cap), all.iter().take(cap).cloned().collect::<Vec<_>>());
    }
    assert_eq!(npc_spawns(&field, &mesh, |_, _| None), all, "the default cap is far above a real field's entities");
}

#[test]
fn an_oversized_script_or_model_loader_section_reports_no_npcs() {
    let entities = npc_entities();
    let refs: Vec<(&str, Vec<u8>)> = entities.iter().map(|(n, c)| (*n, c.clone())).collect();
    let script = script_section(&refs, 512);
    // NPCs of a field with this script section, and (if given) a model-loader section padded with
    // zeros to this length; both parsers ignore the bytes after the last record
    let count = |script: Vec<u8>, loader_len: Option<usize>| {
        let mut spec = FieldSpec { models: npc_loader_models(), overrides: vec![(0, script)], ..FieldSpec::default() };
        if let Some(len) = loader_len {
            let mut loader = FieldFile::parse(spec.build()).unwrap().section(2).unwrap().to_vec();
            loader.resize(len, 0);
            spec.overrides.push((2, loader));
        }
        adapter_over(&[("f", entry(&spec))]).load_area("f").unwrap().npcs().len()
    };
    let cap = crate::MAX_NPC_SECTION_BYTES;
    let normal = count(script.clone(), None);
    assert_eq!(normal, 5);
    let padded = |len: usize| {
        let mut s = script.clone();
        s.resize(len, 0);
        s
    };
    // the script section: at the cap it is still analysed, one byte over it is not
    assert_eq!(count(padded(cap), None), normal, "script section at the cap");
    assert_eq!(count(padded(cap + 1), None), 0, "script section over the cap");
    // the same for the model-loader section
    assert_eq!(count(script.clone(), Some(cap)), normal, "loader section at the cap");
    assert_eq!(count(script, Some(cap + 1)), 0, "loader section over the cap");
}

#[test]
fn a_model_with_an_empty_skeleton_name_is_not_an_npc() {
    let script = script_section(&[("ghost", char_then(0, &[asm::xyzi(10, 10, 20, 0), asm::ret()]))], 512);
    let spec = FieldSpec {
        models: vec![LoaderModelSpec::new("md.nameless", "", &["X.aki"])],
        overrides: vec![(0, script)],
        ..FieldSpec::default()
    };
    let area = adapter_over(&[("f", entry(&spec))]).load_area("f").unwrap();
    assert!(area.npcs().is_empty());
}

#[test]
fn placements_without_a_literal_z_use_the_triangle_they_name_on_a_layered_mesh() {
    // two floors over the same ground; the XYI names the upper one
    let spec = FieldSpec {
        triangles: vec![
            [(0, 0, 0), (200, 0, 0), (200, 200, 0)],
            [(0, 0, 100), (200, 0, 100), (200, 200, 100)],
        ],
        access: vec![[0xFFFF; 3]; 2],
        models: vec![LoaderModelSpec::new("md.a", "AAAA.HRC", &["X.aki"]), LoaderModelSpec::new("md.b", "BBBB.HRC", &["X.aki"])],
        overrides: vec![(
            0,
            script_section(
                &[
                    ("up", char_then(0, &[asm::xyi(150, 50, 1), asm::ret()])),
                    ("down", char_then(1, &[asm::xyi(150, 50, 0), asm::ret()])),
                ],
                512,
            ),
        )],
        ..FieldSpec::default()
    };
    let area = adapter_over(&[("f", entry(&spec))]).load_area("f").unwrap();
    let npcs = area.npcs();
    assert_eq!(find(&npcs, "up").unwrap().pos.z, 100.0);
    assert_eq!(find(&npcs, "down").unwrap().pos.z, 0.0);
}

#[test]
fn a_field_without_usable_scripts_or_models_has_no_npcs() {
    // the default synthetic script section is arbitrary bytes, and there are no model records
    let area = adapter_over(&[("f", entry(&FieldSpec::default()))]).load_area("f").unwrap();
    assert!(area.npcs().is_empty());
    // a good script and no model-loader records: nothing can be shown
    let spec = FieldSpec {
        overrides: vec![(0, script_section(&[("npc", char_then(0, &[asm::xyzi(10, 10, 20, 0), asm::ret()]))], 512))],
        ..FieldSpec::default()
    };
    let area = adapter_over(&[("f", entry(&spec))]).load_area("f").unwrap();
    assert!(area.npcs().is_empty());
}

// ---------------------------------------------------------------------------------------
// the DIR conversion
// ---------------------------------------------------------------------------------------

#[test]
fn the_direction_conversion_is_a_full_turn_per_256_in_range_and_never_nan() {
    use std::f32::consts::{PI, TAU};
    let all: Vec<f32> = (0..=255u8).map(field_direction_to_facing).collect();
    assert!(all.iter().all(|f| f.is_finite() && *f > -PI - 1e-6 && *f <= PI + 1e-6), "{all:?}");
    // 256 steps are one full turn: consecutive bytes differ by TAU/256 (mod TAU)
    let step = TAU / 256.0;
    for d in 0..255usize {
        let diff = (all[d + 1] - all[d]).rem_euclid(TAU);
        assert!((diff - step).abs() < 1e-4 || (diff - (TAU - step)).abs() < 1e-4, "byte {d}: {diff}");
    }
    // a quarter turn is 64 bytes apart
    let quarter = (all[64] - all[0]).rem_euclid(TAU);
    assert!((quarter - TAU / 4.0).abs() < 1e-4 || (quarter - 3.0 * TAU / 4.0).abs() < 1e-4, "{quarter}");
}

#[test]
fn the_direction_conversion_is_the_documented_quarter_turn_map() {
    use std::f32::consts::{FRAC_PI_2, PI};
    // facing = (direction - 64) / 256 * tau, in (-pi, pi]: byte 64 faces +x, 128 +y, 192 -x, 0 -y
    assert_eq!(field_direction_to_facing(64), 0.0);
    assert_eq!(field_direction_to_facing(128), FRAC_PI_2);
    assert_eq!(field_direction_to_facing(192), PI, "a half turn is +pi, not -pi");
    assert_eq!(field_direction_to_facing(0), -FRAC_PI_2);
    // counter-clockwise: one byte up from 64 turns toward +y
    assert!(field_direction_to_facing(65) > 0.0 && field_direction_to_facing(63) < 0.0);
    // just past the half turn wraps to the negative side
    assert!(field_direction_to_facing(193) < 0.0 && field_direction_to_facing(193) > -PI);
    // the field's control direction 124 (md1stin) is "up the screen" at 84.4 degrees
    assert!((f64::from(field_direction_to_facing(124)).to_degrees() - 84.375).abs() < 1e-4);
}

// ---------------------------------------------------------------------------------------
// encounters through the area
// ---------------------------------------------------------------------------------------

fn table_bytes(enabled: u8, rate: u8, entries: &[(u16, u16)]) -> Vec<u8> {
    let mut t = vec![enabled, rate];
    for slot in 0..6 {
        let raw = entries.get(slot).map_or(0, |&(formation, weight)| (weight << 10) | formation);
        t.extend_from_slice(&raw.to_le_bytes());
    }
    t.resize(24, 0);
    t
}

#[test]
fn encounters_come_from_the_fields_own_section_through_the_area() {
    let mut section = table_bytes(0, 0, &[]);
    section.extend(table_bytes(1, 40, &[(301, 40), (302, 24)]));
    let with = FieldSpec { overrides: vec![(6, section)], ..FieldSpec::default() };
    let a = adapter_over(&[("with", entry(&with)), ("without", entry(&FieldSpec::default())), ("short", entry(&FieldSpec { overrides: vec![(6, vec![1, 2, 3])], ..FieldSpec::default() }))]);
    let want = EncounterTable {
        rate: 40,
        entries: vec![EncounterEntry { formation: 301, weight: 40 }, EncounterEntry { formation: 302, weight: 24 }],
    };
    // through the trait object, which is all a server holds
    assert_eq!(a.load_area("with").unwrap().encounters(), Some(want.clone()));
    assert_eq!(a.load_field("with").unwrap().encounters(), Some(want));
    // a field with an all-zero section (the synthetic default) and one with a too-short section: none
    assert_eq!(a.load_area("without").unwrap().encounters(), None);
    assert_eq!(a.load_area("short").unwrap().encounters(), None);
}

// ---------------------------------------------------------------------------------------
// damaged sections never panic
// ---------------------------------------------------------------------------------------

/// Everything the hooks offer, called on a loaded area.
fn use_every_hook(area: &dyn AreaRuntime) {
    let exits = area.exits();
    assert!(exits.len() <= crate::field::GATEWAY_SLOTS);
    for e in &exits {
        assert!(e.arrival.is_finite() && e.line.0.is_finite() && e.line.1.is_finite());
    }
    let npcs = area.npcs();
    assert!(npcs.len() <= crate::area_content::MAX_NPCS_PER_AREA);
    for n in &npcs {
        assert!(n.pos.is_finite() && n.facing.is_finite(), "{n:?}");
    }
    let _ = area.encounters();
    let _ = area.place_arrival(Pos::new(1.0, 2.0, 3.0), Some(0));
}

fn sweep_archive(spec: &FieldSpec) -> Ff7Adapter {
    adapter_over(&[("maplist", maplist_bytes(&["f", "g"])), ("f", entry(spec)), ("g", entry(&FieldSpec::default()))])
}

#[test]
fn every_truncation_of_a_section_is_harmless_through_the_adapter() {
    let base = npc_field_spec();
    let gateways = Some(vec![gateway([0, 0, 0], [0, 10, 0], [1, 2, 3], 1), gateway([0, 0, 0], [0, 10, 0], [1, 2, 3], 0)]);
    let spec = FieldSpec { gateways, ..base };
    let built = FieldFile::parse(spec.build()).unwrap();
    let mut loaded = 0;
    for section in [0usize, 2, 6, 7] {
        let bytes = built.section(section).unwrap().to_vec();
        // every cut up to a few hundred bytes (the sections are short), then a sample of the rest
        let cuts: Vec<usize> = (0..bytes.len()).filter(|c| *c < 400 || c % 7 == 0).collect();
        for cut in cuts {
            let damaged = FieldSpec { overrides: vec![(section, bytes[..cut].to_vec())], ..spec.clone() };
            let a = sweep_archive(&damaged);
            if let Ok(area) = a.load_area("f") {
                use_every_hook(area.as_ref());
                loaded += 1;
            }
        }
    }
    assert!(loaded > 400, "{loaded} damaged fields loaded");
}

/// Deterministic xorshift, so a failure can be reproduced.
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_corruption_of_the_sections_is_harmless() {
    let spec = FieldSpec {
        gateways: Some(vec![gateway([0, 0, 0], [0, 10, 0], [1, 2, 3], 1)]),
        ..npc_field_spec()
    };
    let built = FieldFile::parse(spec.build()).unwrap();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut loaded = 0usize;
    for round in 0..800 {
        let section = [0usize, 2, 6, 7][round % 4];
        let mut bytes = built.section(section).unwrap().to_vec();
        for _ in 0..1 + rng.next_u64() % 6 {
            let at = (rng.next_u64() % bytes.len() as u64) as usize;
            bytes[at] = rng.next_u64() as u8;
        }
        let damaged = FieldSpec { overrides: vec![(section, bytes)], ..spec.clone() };
        // the pure functions on the parsed field, and the whole adapter path on a sample
        let field = FieldFile::parse(damaged.build()).unwrap();
        let mesh = field.walkmesh().unwrap();
        for n in npc_spawns(&field, &mesh, |_, _| Some(1.0)) {
            assert!(n.pos.is_finite() && n.facing.is_finite());
        }
        if round % 10 == 0 {
            if let Ok(area) = sweep_archive(&damaged).load_area("f") {
                use_every_hook(area.as_ref());
                loaded += 1;
            }
        }
    }
    assert!(loaded > 50, "{loaded}");
}

#[test]
fn hooks_are_additive_for_every_other_trait_method() {
    // loading through the adapter still hosts an area whose movement rules are untouched
    let a = adapter_over(&[("f", entry(&FieldSpec::default()))]);
    assert!(matches!(a.load_area("nope"), Err(AdapterError::AreaNotFound(_))));
    let area = a.load_area("f").unwrap();
    assert_eq!(area.walk_triangles().len(), 2);
    assert!(area.exits().is_empty() && area.npcs().is_empty() && area.encounters().is_none());
}
