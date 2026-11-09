//! Checks of the world a server offers on the owner's REAL FF7 data (FF7 Steam Edition, English; measured
//! 2026-11-09): the exits the fields' scripts decide and the story points they hold, along the first reactor mission
//! (`md1stin` down to `nmkin_5`).
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` is unset, so a normal `cargo test` never
//! touches game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-ff7 --test chain_real -- --ignored --nocapture
//! ```
//!
//! The files are only opened read-only; the assertions record field names, entity-free labels and numbers (no game
//! bytes and no game text). `examples/world_survey.rs` prints the same facts field by field.

mod script_common;

use fmv_adapter_api::{AreaRuntime, Exit, ExitKind, ExitState, GameAdapter, StoryTrigger};
use fmv_core::{EditionProvenance, GameId, Platform};
use fmv_ff7::story::StoryState;
use fmv_ff7::Ff7Adapter;
use script_common::{data_dir, flevel_path};
use std::collections::BTreeMap;

fn adapter() -> Option<Ff7Adapter> {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return None;
    };
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
    Some(Ff7Adapter::from_flevel_path(flevel_path(&dir), provenance).unwrap())
}

/// The state with the lift bit set (what the elevator's ride leaves).
fn state(moment: u16, lift_down: bool) -> Vec<u8> {
    let mut s = StoryState::at(moment);
    if lift_down {
        assert!(s.set_bit(1, 225, 0, true));
    }
    s.encode()
}

/// The target of the one exit of `area` that is open for `progress`, among those that share `line` (a script's).
fn open_target(area: &dyn AreaRuntime, progress: &[u8]) -> Vec<String> {
    area.exits().iter().filter(|e| area.exit_state(e, progress) == ExitState::Open).map(|e| e.target.clone()).collect()
}

/// The lift of the first reactor: four exits on one line; the story picks which one is open.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_lift_of_the_first_reactor_routes_by_its_bit_and_the_moment() {
    let Some(adapter) = adapter() else { return };
    let area = adapter.load_area("elevtr1").unwrap();
    let exits = area.exits();
    assert_eq!(exits.len(), 4, "no gateway, four exits of the one line: {exits:#?}");
    assert!(exits.iter().all(|e| matches!(e.kind, ExitKind::Script { .. }) && e.same_line_as(&exits[0])), "one line");
    let by_target: BTreeMap<&str, &Exit> = exits.iter().map(|e| (e.target.as_str(), e)).collect();
    assert_eq!(by_target.keys().copied().collect::<Vec<_>>(), ["nmkin_1", "nmkin_2", "smkin_1", "smkin_2"]);
    // where they put the player (the data's own arrivals), and the line they are on
    assert_eq!((by_target["nmkin_2"].arrival.x, by_target["nmkin_2"].arrival.y, by_target["nmkin_2"].arrival_hint), (-682.0, 308.0, Some(92)));
    assert_eq!((by_target["nmkin_1"].arrival.x, by_target["nmkin_1"].arrival.y, by_target["nmkin_1"].arrival_hint), (-1485.0, 4298.0, Some(60)));
    assert_eq!((exits[0].line.0.x, exits[0].line.0.y, exits[0].line.1.x, exits[0].line.1.y), (-185.0, -68.0, -185.0, 56.0));
    // exactly one exit is open for each state of the lift and the story
    for (moment, lift_down, want) in [
        (1, false, "nmkin_1"),
        (11, false, "nmkin_1"),
        (12, false, "nmkin_1"),
        (32, false, "nmkin_1"),
        (33, false, "smkin_1"),
        (500, false, "smkin_1"),
        (1, true, "nmkin_2"),
        (12, true, "nmkin_2"),
        (13, true, "nmkin_2"),
        (32, true, "nmkin_2"),
        (33, true, "smkin_2"),
        (500, true, "smkin_2"),
    ] {
        assert_eq!(open_target(area.as_ref(), &state(moment, lift_down)), [want], "moment {moment}, lift {}", if lift_down { "down" } else { "up" });
    }
    // a new account's empty blob is the lift up at moment 1
    assert_eq!(open_target(area.as_ref(), &[]), ["nmkin_1"]);
    // the exits of the later story are closed because their guards say so, not because they are unknown
    for e in &exits {
        let s = area.exit_state(e, &state(12, true));
        assert!(matches!(s, ExitState::Open | ExitState::Closed), "{} is judged: {s:?}", e.target);
    }
    // a progression nobody can read opens nothing the script decides
    assert!(open_target(area.as_ref(), &[7, 7]).is_empty());
    assert!(exits.iter().all(|e| matches!(area.exit_state(e, &[7, 7]), ExitState::Unknown(_))));
}

/// Which exits the fields of the chain offer: the gateways the party walks through, and no other script exit.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_chain_is_gateways_and_the_lift_and_every_exit_is_open_or_closed_never_unjudged() {
    let Some(adapter) = adapter() else { return };
    let chain: [(&str, &[&str]); 8] = [
        ("md1stin", &["md1_1"]),
        ("md1_1", &["md1stin", "md1_2"]),
        ("md1_2", &["md1_1", "nrthmk"]),
        ("nrthmk", &["md1_2", "md8_4", "nmkin_1"]),
        ("nmkin_1", &["nrthmk", "elevtr1"]),
        ("nmkin_2", &["elevtr1", "nmkin_3"]),
        ("nmkin_3", &["nmkin_2", "nmkin_4"]),
        ("nmkin_4", &["nmkin_3", "nmkin_5"]),
    ];
    for (field, targets) in chain {
        let area = adapter.load_area(field).unwrap();
        let exits = area.exits();
        assert_eq!(exits.iter().map(|e| e.target.as_str()).collect::<Vec<_>>(), targets, "{field}");
        assert!(exits.iter().all(|e| e.kind == ExitKind::Gateway), "{field}: no script exit but the lift's");
        // gateways are data: open for every story, a new account's and a late one's
        for blob in [vec![], state(12, true), state(40, false)] {
            assert!(exits.iter().all(|e| area.exit_state(e, &blob) == ExitState::Open), "{field}");
        }
    }
    let nmkin_5 = adapter.load_area("nmkin_5").unwrap();
    assert_eq!(nmkin_5.exits().iter().map(|e| e.target.as_str()).collect::<Vec<_>>(), ["nmkin_4"]);
    // the timers' automatic jumps to nrthmk and the director's jump to md8_4 are nobody's walk-on exit
    for field in ["md1_1", "nmkin_1", "elevtr1", "nmkin_2", "nmkin_5"] {
        let area = adapter.load_area(field).unwrap();
        assert!(!area.exits().iter().any(|e| e.target == "nrthmk" && matches!(e.kind, ExitKind::Script { .. })), "{field}");
    }
}

/// The story points of the chain: what the first mission's own scripts say moves the story, as the server reads it.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_story_points_of_the_chain_are_the_ones_the_scripts_make() {
    let Some(adapter) = adapter() else { return };
    let kind = |t: &StoryTrigger| match t {
        StoryTrigger::Region(c) => format!("region({})", c.len()),
        StoryTrigger::Line(..) => "line".to_string(),
        StoryTrigger::Enter => "entry".to_string(),
        _ => "other".to_string(),
    };
    let points = |field: &str| -> Vec<(String, String)> {
        adapter.load_area(field).unwrap().story_points().iter().map(|p| (kind(&p.trigger), p.label.clone())).collect()
    };
    let one = |trigger: &str, label: &str| vec![(trigger.to_string(), label.to_string())];
    assert_eq!(
        points("md1stin"),
        [
            ("region(3)".to_string(), "triangle 12: moment ==1 -> 6 (the script then starts battle 300, which the server does not run)".to_string()),
            ("entry".to_string(), "entry: moment ==0 -> 1".to_string())
        ]
    );
    assert_eq!(points("md1_1"), one("region(3)", "triangle 62: moment <7 -> 7"));
    assert_eq!(points("md1_2"), one("entry", "entry: moment <8 -> 8"));
    assert_eq!(points("nrthmk"), one("entry", "entry: moment <10 -> 10"));
    assert_eq!(points("nmkin_1"), one("line", "line: moment <11 -> 11"));
    assert_eq!(
        points("elevtr1"),
        one("region(3)", "triangle 8 (OK): Var[1][225] bit 0 clear -> Var[1][225] bit 0 on; Var[1][225] bit 0 clear and moment <12 -> 12")
    );
    assert_eq!(points("nmkin_2"), one("entry", "entry: moment <13 -> 13"));
    assert!(points("nmkin_3").is_empty(), "the woman who moves the story to 14 is talked to: nothing the server can see");
    assert!(points("nmkin_4").is_empty());
    assert_eq!(
        points("nmkin_5"),
        [
            ("region(3)".to_string(), "triangle 2: moment <27 -> 27".to_string()),
            ("region(3)".to_string(), "triangle 2: moment ==123 -> 127".to_string()),
            ("entry".to_string(), "entry: moment ==120 -> 123".to_string())
        ]
    );
}

/// The whole first mission as a story, in the order the party meets the points: a new account takes the steps the
/// real scripts make and ends with the lift down at moment 13, which opens the lift's way to `nmkin_2`.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn walking_the_chain_moves_a_new_account_from_1_to_13_with_the_lift_down_and_that_opens_the_way_to_nmkin_2() {
    let Some(adapter) = adapter() else { return };
    // (field, point id, what the server says it did)
    let steps = [
        ("md1stin", 0, "moment=1->6"),
        ("md1_1", 0, "moment=6->7"),
        ("md1_2", 0, "moment=7->8"),
        ("nrthmk", 0, "moment=8->10"),
        ("nmkin_1", 0, "moment=10->11"),
        ("elevtr1", 0, "moment=11->12 var1.225=0->1"),
        ("nmkin_2", 0, "moment=12->13"),
    ];
    let mut blob: Vec<u8> = Vec::new();
    let elevtr1 = adapter.load_area("elevtr1").unwrap();
    for (field, point, summary) in steps {
        let area = adapter.load_area(field).unwrap();
        // before the ride the lift's way leads up; the moment decides nothing yet
        if field == "elevtr1" {
            assert_eq!(open_target(elevtr1.as_ref(), &blob), ["nmkin_1"]);
        }
        let step = area.story_advance(&blob, point).unwrap_or_else(|| panic!("{field}#{point} does not apply to {blob:02x?}"));
        assert_eq!(step.summary, summary, "{field}");
        blob = step.progress;
        // a point applies once
        assert_eq!(area.story_advance(&blob, point), None, "{field}#{point} a second time");
    }
    assert_eq!(blob, vec![2, 13, 0, 1, 1, 225, 1], "moment 13, the lift bit on");
    assert_eq!(open_target(elevtr1.as_ref(), &blob), ["nmkin_2"], "the ride opened the way down");
    // the stored story round-trips through the adapter's merge (a late older save cannot take it back)
    assert_eq!(adapter.merge_progression(&blob, &state(7, false)), blob);
    assert_eq!(adapter.merge_progression(&state(12, false), &blob), blob);
    // the dialogue lane reads the same state
    let story = StoryState::decode(&blob).unwrap();
    use fmv_ff7::script::dialogue::StoryVars;
    assert_eq!((story.byte(2, 0), story.byte(1, 225)), (13, 1));
}

/// The numbers over the archive (what `examples/world_survey.rs --all` prints): how many exits the scripts add,
/// how a new account's story judges them, and how many story points there are of each kind.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn script_exits_and_story_points_of_the_archive_in_numbers() {
    let Some(adapter) = adapter() else { return };
    let areas = adapter.list_areas();
    assert_eq!(areas.len(), 702);
    let (mut with_exits, mut script, mut max_script, mut max_field) = (0usize, 0usize, 0usize, String::new());
    let (mut open, mut closed, mut unjudged) = (0usize, 0usize, 0usize);
    let mut points: BTreeMap<String, usize> = BTreeMap::new();
    let (mut ok_points, mut fields_with_points) = (0usize, 0usize);
    for info in &areas {
        let area = adapter.load_field(&info.id.key).unwrap();
        let exits = area.exits();
        with_exits += usize::from(!exits.is_empty());
        let scripts: Vec<_> = exits.iter().filter(|e| matches!(e.kind, ExitKind::Script { .. })).collect();
        script += scripts.len();
        if scripts.len() > max_script {
            (max_script, max_field) = (scripts.len(), info.id.key.clone());
        }
        for e in scripts {
            match area.exit_state(e, &[]) {
                ExitState::Open => open += 1,
                ExitState::Closed => closed += 1,
                ExitState::Unknown(_) => unjudged += 1,
            }
        }
        let p = area.story_points();
        fields_with_points += usize::from(!p.is_empty());
        for point in &p {
            *points.entry(match point.trigger {
                StoryTrigger::Region(_) => "region",
                StoryTrigger::Line(..) => "line",
                StoryTrigger::Enter => "entry",
                _ => "other",
            }
            .to_string())
            .or_default() += 1;
            ok_points += usize::from(point.label.contains("(OK)"));
        }
    }
    println!("{with_exits} fields with exits, {script} script exits (most {max_script} in {max_field}): new account open {open}, closed {closed}, unjudged {unjudged}; points {points:?} in {fields_with_points} fields, {ok_points} wanting OK");
    assert_eq!((with_exits, script, max_script, max_field.as_str()), (558, 917, 88, "move_f"));
    assert_eq!((open, closed, unjudged), (324, 56, 537));
    assert_eq!((points.get("region").copied(), points.get("line").copied(), points.get("entry").copied(), fields_with_points, ok_points), (Some(7), Some(20), Some(56), 66, 1));
}

/// The triangles of `mesh` that a walker standing on triangle `start` can get to: breadth first over the access words
/// (word `e` of a triangle is the triangle you are in once you cross its edge `e`, `0xFFFF` a wall), the rule every
/// move the server accepts follows (`walk.rs`), so what the bots and the players can do on foot.
fn walkable_from(mesh: &fmv_ff7::field::Walkmesh, start: usize) -> std::collections::BTreeSet<usize> {
    let mut seen = std::collections::BTreeSet::from([start]);
    let mut queue = std::collections::VecDeque::from([start]);
    while let Some(t) = queue.pop_front() {
        for &next in mesh.access.get(t).into_iter().flatten() {
            let next = usize::from(next);
            if next != 0xFFFF && next < mesh.triangles.len() && seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    seen
}

/// The triangles that the line of `exit` passes through in x and y (the server's crossing test ignores heights, so a
/// walker on any level that crosses the line's x and y takes the exit): where a walker can stand to cross it, sampled
/// along the line.
fn triangles_at(mesh: &fmv_ff7::field::Walkmesh, exit: &Exit) -> Vec<usize> {
    let (a, b) = exit.line;
    let mut found = std::collections::BTreeSet::new();
    for k in 0..=24 {
        let t = k as f32 / 24.0;
        let (x, y) = ((a.x + (b.x - a.x) * t).round() as i32, (a.y + (b.y - a.y) * t).round() as i32);
        for (i, tri) in mesh.triangles.iter().enumerate() {
            if fmv_ff7::script::triangle_contains_xy(tri, x, y) {
                found.insert(i);
            }
        }
    }
    found.into_iter().collect()
}

/// **How far the walkmesh carries the party.** From the triangle each hop of the chain puts it on, the triangles of
/// the next hop's line are walkable (through the access words, which is the rule every move the server accepts
/// follows), down to `nmkin_3`: the panel and the lift's line in `elevtr1`, the way on from `nmkin_2`. And no further.
/// `nmkin_3`'s arrival is on its upper level and only 6 of the field's 46 triangles can be walked to from it: the way
/// down to `nmkin_4` is scripted (a jump over a gap, then two ladders: `JUMP` and `LADER` instructions on the lines'
/// scripts, which the walkmesh does not link). `nmkin_4`'s arrival is a pocket too (22 of its 40 triangles; the
/// field has a ladder of its own, a `LADER` between two levels, and its gateway to `nmkin_5` is not among the
/// triangles walkable from the arrival). So the first mission's walkable part of the world ends at `nmkin_3` and the
/// ladders are the next gap. (The access words are the truth. The client's grid planner (`nav.rs`) used to put its
/// nodes at height 0 and so planned over the wrong level of a stacked mesh: it found no way down `nmkin_2`'s stair
/// and claimed one from `nmkin_4` to `nmkin_5`. It now keeps the level with every node and agrees with this test:
/// a plan down the stair, none past `nmkin_3`.)
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_walkmesh_carries_the_party_down_to_nmkin_3_and_no_further() {
    let Some(adapter) = adapter() else { return };
    let load = |field: &str| adapter.load_field(field).unwrap();
    let exit_to = |area: &fmv_ff7::Ff7FieldArea, target: &str| area.exits().into_iter().find(|e| e.target == target).unwrap_or_else(|| panic!("no exit to {target}"));
    // (field the party stands in, the field it came from, the exit it walks to, whether it can)
    let hops: [(&str, &str, &str, bool); 8] = [
        ("md1stin", "", "md1_1", true),
        ("md1_1", "md1stin", "md1_2", true),
        ("md1_2", "md1_1", "nrthmk", true),
        ("nrthmk", "md1_2", "nmkin_1", true),
        ("nmkin_1", "nrthmk", "elevtr1", true),
        ("nmkin_2", "elevtr1", "nmkin_3", true),
        ("nmkin_3", "nmkin_2", "nmkin_4", false),
        ("nmkin_4", "nmkin_3", "nmkin_5", false),
    ];
    for (field, from, to, can) in hops {
        let area = load(field);
        let mesh = area.walkmesh();
        // the triangle the party starts on: the arrival's own (the hint), or under the spawn in the first field
        let start = if from.is_empty() {
            let spawn = area.spawn_points()[0];
            mesh.triangles.iter().position(|t| fmv_ff7::script::triangle_contains_xy(t, spawn.x.round() as i32, spawn.y.round() as i32)).unwrap()
        } else if field == "nmkin_2" {
            exit_to(&load("elevtr1"), "nmkin_2").arrival_hint.unwrap() as usize
        } else {
            exit_to(&load(from), field).arrival_hint.unwrap() as usize
        };
        let goal = exit_to(&area, to);
        let wanted = triangles_at(mesh, &goal);
        assert!(!wanted.is_empty(), "{field}: the line to {to} is on the mesh");
        let reach = walkable_from(mesh, start);
        let reached = wanted.iter().any(|t| reach.contains(t));
        println!("{field}: from triangle {start} ({} walkable) to the line to {to} (triangles {wanted:?}): {reached}", reach.len());
        assert_eq!(reached, can, "{field}: from where the party arrives to the exit to {to}");
    }
    // elevtr1: from the arrival to the panel's triangle (the ride) and to the lift's line
    let area = load("elevtr1");
    let mesh = area.walkmesh();
    let start = exit_to(&load("nmkin_1"), "elevtr1").arrival_hint.unwrap() as usize;
    let reach = walkable_from(mesh, start);
    let lift = exit_to(&area, "nmkin_2");
    assert!(triangles_at(mesh, &lift).iter().any(|t| reach.contains(t)), "the lift's line is walkable from the arrival");
    assert!(reach.contains(&8), "the panel's triangle (8) is walkable from the arrival");
    let panel = area.story_points().into_iter().find(|p| p.label.starts_with("triangle 8")).expect("the ride");
    assert!(matches!(&panel.trigger, StoryTrigger::Region(c) if c.len() == 3));
}
