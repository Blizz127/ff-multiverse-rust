//! Checks of `FieldScript::script_exits` against the owner's REAL FF7 data (FF7 Steam Edition,
//! English; measured 2026-11-08).
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` is unset, so a normal
//! `cargo test` never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-ff7 --test exits_real -- --ignored --nocapture
//! ```
//!
//! `FMV_FF7_DATA` is the game's `data/` directory (the one containing `field/flevel.lgp`). The
//! files are only ever opened read-only; the assertions record counts, field names, entity names,
//! variable addresses and numbers: no game bytes and no game text are stored in this repository.
//! `examples/exits_dump.rs` prints the same facts field by field (`--gated --all` is the list of
//! walk-on exits that depend on the game state).

mod script_common;

use fmv_adapter_api::GameAdapter;
use fmv_core::{EditionProvenance, GameId, Platform};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::{ExitTrigger, FieldScript, Maplist, ScriptExit};
use fmv_ff7::Ff7Adapter;
use script_common::{data_dir, flevel_path, is_field_name, load_field};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The end points of a line.
type LineKey = ([i16; 3], [i16; 3]);
/// Where a jump puts the player: x, y, triangle, direction.
type Arrival = (i16, i16, i16, u8);

fn open() -> Option<(LgpArchive<std::fs::File>, Maplist, std::path::PathBuf)> {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return None;
    };
    let path = flevel_path(&dir);
    let mut lgp = LgpArchive::open_path(&path).unwrap();
    let maplist = Maplist::from_archive(&mut lgp).unwrap();
    Some((lgp, maplist, path))
}

fn exits_of(lgp: &mut LgpArchive<std::fs::File>, maplist: &Maplist, name: &str) -> Vec<ScriptExit> {
    let field = load_field(lgp, name).unwrap();
    FieldScript::from_field(&field).unwrap().script_exits(maplist)
}

fn texts(e: &ScriptExit) -> Vec<String> {
    e.guards.iter().map(ToString::to_string).collect()
}

/// Totals over the 702 fields, and the invariant that makes them trustworthy: every `MAPJUMP` the
/// older `map_jumps()` finds is described at least once, and nothing else is.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_whole_archive_in_numbers() {
    let Some((mut lgp, maplist, _)) = open() else { return };
    let names: Vec<String> = lgp.list().iter().filter(|e| is_field_name(&e.name)).map(|e| e.name.clone()).collect();
    assert_eq!(names.len(), 702);
    let mut by_trigger: BTreeMap<String, usize> = BTreeMap::new();
    let (mut total, mut walk_on, mut gated, mut placeholders, mut disabled, mut shared_lines) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for name in &names {
        let field = load_field(&mut lgp, name).unwrap();
        let script = FieldScript::from_field(&field).unwrap();
        let exits = script.script_exits(&maplist);
        // the same MAPJUMPs as `map_jumps()`: no more, no fewer
        let described: BTreeSet<usize> = exits.iter().map(|e| e.offset).collect();
        let found: BTreeSet<usize> = script.map_jumps().iter().map(|j| j.offset).collect();
        assert_eq!(described, found, "{name}: script_exits and map_jumps disagree");
        let mut lines: BTreeMap<LineKey, BTreeSet<Option<String>>> = BTreeMap::new();
        for e in &exits {
            total += 1;
            *by_trigger.entry(e.trigger.to_string()).or_default() += 1;
            // the path starts at the root and ends in the script that holds the jump
            assert!(!e.path.is_empty() && e.path.first().is_some_and(|r| r.entity == e.entity), "{name}: {e:?}");
            if e.trigger.is_walk_on() {
                walk_on += 1;
                gated += usize::from(e.is_story_gated());
                placeholders += usize::from(e.arrival_is_placeholder());
                let line = e.line.expect("a line root has its line");
                disabled += usize::from(line.enabled == Some(false));
                lines.entry((line.a, line.b)).or_default().insert(e.target.clone());
            }
            // exactly the line roots (the `[OK]` slot included) carry their line
            assert_eq!(matches!(e.trigger, ExitTrigger::Line { .. }), e.line.is_some(), "{name}: {e:?}");
        }
        shared_lines += lines.values().filter(|targets| targets.len() > 1).count();
    }
    println!("{total} script exits {by_trigger:?}; walk-on {walk_on} (story-gated {gated}, placeholder arrival {placeholders}, on disabled lines {disabled}); {shared_lines} lines with more than one destination");
    assert_eq!(total, 2511);
    let slot = |s: &str| by_trigger.get(s).copied().unwrap_or(0);
    assert_eq!((slot("talk"), slot("contact"), slot("main"), slot("orphan"), slot("init")), (547, 84, 629, 96, 0));
    assert_eq!(
        (slot("line slot 1 ([OK])"), slot("line slot 2 (Move)"), slot("line slot 3 (Move)"), slot("line slot 4 (Go)"), slot("line slot 5 (Go 1x)"), slot("line slot 6 (Go away)")),
        (41, 522, 123, 329, 140, 0)
    );
    assert_eq!((walk_on, gated, placeholders, disabled, shared_lines), (1114, 707, 94, 11, 78));
}

/// The elevator: no gateway, one line, four guarded jumps (the case that needs a design decision).
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_elevator_leaves_through_one_line_with_four_guarded_destinations() {
    let Some((mut lgp, maplist, _)) = open() else { return };
    let exits = exits_of(&mut lgp, &maplist, "elevtr1");
    let walk: Vec<&ScriptExit> = exits.iter().filter(|e| e.trigger.is_walk_on()).collect();
    assert_eq!(walk.len(), 4, "{walk:#?}");
    for e in &walk {
        assert_eq!(e.trigger, ExitTrigger::Line { slot: 3 });
        assert_eq!(e.entity_name, "jp0");
        let line = e.line.unwrap();
        assert_eq!((line.a, line.b, line.enabled), ([-185, -68, 5], [-185, 56, 5], None));
        assert!(e.is_story_gated());
    }
    let summary: Vec<(String, Arrival, Vec<String>)> =
        walk.iter().map(|e| (e.target.clone().unwrap(), (e.x, e.y, e.triangle, e.direction), texts(e))).collect();
    let g = |a: &str, b: &str| vec![a.to_string(), b.to_string()];
    assert_eq!(
        summary,
        [
            ("nmkin_1".into(), (-1485, 4298, 60, 0), g("Var[1][225] lacks bit 0", "Var[2][0] < 33")),
            ("smkin_1".into(), (-1485, 4298, 52, 0), g("Var[1][225] lacks bit 0", "not (Var[2][0] < 33)")),
            ("nmkin_2".into(), (-682, 308, 92, 64), g("not (Var[1][225] lacks bit 0)", "Var[2][0] < 33")),
            ("smkin_2".into(), (-682, 308, 92, 64), g("not (Var[1][225] lacks bit 0)", "not (Var[2][0] < 33)")),
        ]
    );
    // the field has no gateway at all
    let field = load_field(&mut lgp, "elevtr1").unwrap();
    let used = field.triggers().unwrap().gateways.iter().filter(|g| g.is_used()).count();
    assert_eq!(used, 0);
}

/// The jumps of the first reactor mission that nobody walks into: cut-scene and timer jumps.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_automatic_jumps_of_the_reactor_chain() {
    let Some((mut lgp, maplist, _)) = open() else { return };
    // the countdown that ends the mission: every reactor field has the same timer entity
    for field in ["nmkin_1", "elevtr1", "nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5"] {
        let exits = exits_of(&mut lgp, &maplist, field);
        let timer: Vec<&ScriptExit> = exits.iter().filter(|e| e.entity_name == "timeo").collect();
        assert_eq!(timer.len(), 1, "{field}");
        let e = timer[0];
        assert_eq!((e.trigger, e.target.as_deref(), (e.x, e.y, e.triangle, e.direction)), (ExitTrigger::Main, Some("nrthmk"), (18, -1164, 1, 0)), "{field}");
        assert_eq!(texts(e), ["Var[2][0] == 27", "Var[1][21] == 0", "Var[1][22] == 0", "Var[1][23] == 0"], "{field}");
        assert!(e.trigger.is_automatic());
    }
    // the way on after the explosion movie is a script too: a director jump from nrthmk to md8_4
    let exits = exits_of(&mut lgp, &maplist, "nrthmk");
    assert!(exits.iter().all(|e| e.trigger.is_automatic() && e.target.as_deref() == Some("md8_4")), "{exits:#?}");
    assert_eq!(exits.len(), 2);
    // md1_1: a director's jump to md1_2 under an early story step; md8_4: one on to md8_1
    let exits = exits_of(&mut lgp, &maplist, "md1_1");
    assert_eq!(exits.len(), 1);
    assert_eq!((exits[0].trigger, exits[0].target.as_deref()), (ExitTrigger::Main, Some("md1_2")));
    let exits = exits_of(&mut lgp, &maplist, "md8_4");
    assert_eq!(exits.len(), 1);
    assert_eq!((exits[0].trigger, exits[0].target.as_deref(), texts(&exits[0])), (ExitTrigger::Main, Some("md8_1"), vec!["Var[2][0] == 33".to_string()]));
    // md1stin and md1_2 have no script exit at all
    for field in ["md1stin", "md1_2"] {
        assert!(exits_of(&mut lgp, &maplist, field).is_empty(), "{field}");
    }
}

/// What the adapter offers TODAY (gateways only): which fields of the reactor chain a player can
/// walk to from `md1stin`. The elevator is a dead end, so the reactor below it cannot be reached.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn today_the_gateway_chain_ends_at_the_elevator() {
    let Some((_, _, path)) = open() else { return };
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
    let adapter = Ff7Adapter::from_flevel_path(path, provenance).unwrap();
    let mut seen: BTreeMap<String, usize> = BTreeMap::from([("md1stin".to_string(), 0)]);
    let mut queue = VecDeque::from(["md1stin".to_string()]);
    while let Some(name) = queue.pop_front() {
        let depth = seen[&name];
        if depth >= 6 {
            continue;
        }
        let area = adapter.load_area(&name).unwrap();
        for e in area.exits() {
            if !seen.contains_key(&e.target) {
                seen.insert(e.target.clone(), depth + 1);
                queue.push_back(e.target.clone());
            }
        }
    }
    let at = |n: &str| seen.get(n).copied();
    println!("reached within six gateways of md1stin: {seen:?}");
    assert_eq!(
        [at("md1_1"), at("md1_2"), at("nrthmk"), at("nmkin_1"), at("md8_4"), at("elevtr1")],
        [Some(1), Some(2), Some(3), Some(4), Some(4), Some(5)]
    );
    // below the elevator: linked to each other by gateways, but nothing above reaches them
    for field in ["nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5", "smkin_1", "smkin_2"] {
        assert_eq!(at(field), None, "{field} should not be reachable without the elevator's line");
    }
    assert!(adapter.load_area("elevtr1").unwrap().exits().is_empty(), "the elevator has no gateway: a dead end today");
    let below: Vec<String> = ["nmkin_2", "nmkin_3", "nmkin_4"]
        .iter()
        .flat_map(|f| adapter.load_area(f).unwrap().exits().into_iter().map(|e| e.target))
        .collect();
    assert_eq!(below, ["elevtr1", "nmkin_3", "nmkin_2", "nmkin_4", "nmkin_3", "nmkin_5"]);
}
