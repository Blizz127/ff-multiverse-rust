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
    // exactly one exit is open for each state of the lift and the story up to moment 32; from 33 on NONE is: the lift of
    // the later story leads to `smkin_1` and `smkin_2`, fields nobody has reviewed, and a script exit of a served field
    // that leads out of the reviewed fields is closed whatever the story says (`exit_gate`, "Where it leads")
    for (moment, lift_down, want) in [
        (1, false, Some("nmkin_1")),
        (11, false, Some("nmkin_1")),
        (12, false, Some("nmkin_1")),
        (32, false, Some("nmkin_1")),
        (33, false, None),
        (500, false, None),
        (1, true, Some("nmkin_2")),
        (12, true, Some("nmkin_2")),
        (13, true, Some("nmkin_2")),
        (32, true, Some("nmkin_2")),
        (33, true, None),
        (500, true, None),
    ] {
        let open = open_target(area.as_ref(), &state(moment, lift_down));
        assert_eq!(open, want.map(str::to_string).into_iter().collect::<Vec<_>>(), "moment {moment}, lift {}", if lift_down { "down" } else { "up" });
    }
    // what the data says (the survey's loader keeps it): from 33 on the lift leads to the other reactor, one exit for each
    // state of the lift. It is the policy that closes them in a served field, not the guards.
    let surveyed = adapter.survey_field("elevtr1").unwrap();
    for (moment, lift_down, want) in [(32, false, "nmkin_1"), (32, true, "nmkin_2"), (33, false, "smkin_1"), (500, false, "smkin_1"), (33, true, "smkin_2"), (500, true, "smkin_2")] {
        assert_eq!(open_target(&surveyed, &state(moment, lift_down)), [want], "the data, moment {moment}, lift {}", if lift_down { "down" } else { "up" });
    }
    assert!(!fmv_ff7::is_reviewed_field("smkin_1") && !fmv_ff7::is_reviewed_field("smkin_2"), "the two fields the later story's lift leads to");
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
    // the timers' automatic jumps to nrthmk and the director's jump to md8_4 are nobody's walk-on exit (nmkin_5 is not
    // reviewed: what its data holds is read with the survey's loader)
    for field in ["md1_1", "nmkin_1", "elevtr1", "nmkin_2", "nmkin_5"] {
        let area = adapter.survey_field(field).unwrap();
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
        StoryTrigger::Press(c) => format!("press({})", c.len()),
        StoryTrigger::Talk { .. } => "talk".to_string(),
        StoryTrigger::Line(..) => "line".to_string(),
        StoryTrigger::Enter => "entry".to_string(),
        _ => "other".to_string(),
    };
    // what the field's scripts say (every field is loaded with the survey's loader; what a server offers of a field that
    // is not reviewed is the next test's)
    let points = |field: &str| -> Vec<(String, String)> {
        let area = adapter.survey_field(field).unwrap();
        let served = adapter.load_area(field).unwrap().story_points();
        assert_eq!(served.is_empty(), !fmv_ff7::is_reviewed_field(field) || area.story_points().is_empty(), "{field}: a server offers the points of the reviewed fields only");
        area.story_points().iter().map(|p| (kind(&p.trigger), p.label.clone())).collect()
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
    // nrthmk: the way in (8 -> 10) and the finale of the mission (the explosion's cutscene, which a party that walks out of
    // the reactor at 27 or later is moved to 33 by); the loop that plays the late party's game over wants the timeout flag
    // SET, which nothing in the model sets, and is not a point
    assert_eq!(
        points("nrthmk"),
        [
            ("entry".to_string(), "entry: moment <10 -> 10".to_string()),
            (
                "entry".to_string(),
                "entry: moment >=27 and Var[1][225] bit 7 clear -> 33; moment >=27 and Var[1][225] bit 7 clear -> Var[1][225] bit 0 on".to_string()
            )
        ]
    );
    assert_eq!(points("nmkin_1"), one("line", "line: moment <11 -> 11"));
    // the lift's panel waits for the OK button: presses, not regions walked into: the ride down, and the ride back that
    // the panel's own script makes
    assert_eq!(
        points("elevtr1"),
        [
            (
                "press(3)".to_string(),
                "triangle 8 (OK): Var[1][225] bit 0 clear -> Var[1][225] bit 0 on; Var[1][225] bit 0 clear and moment <12 -> 12".to_string()
            ),
            (
                "press(3)".to_string(),
                "triangle 8 (OK): the panel's own script runs, what it does to a tracked bit besides a step forward (the ride back)".to_string()
            )
        ]
    );
    assert_eq!(points("nmkin_2"), one("entry", "entry: moment <13 -> 13"));
    // the woman who moves the story to 14 is talked to (entity 4 of the field)
    assert_eq!(points("nmkin_3"), one("talk", "talk to entity 4: the talk script moves the story"));
    assert!(points("nmkin_4").is_empty());
    assert_eq!(
        points("nmkin_5"),
        [
            // the guard scorpion: the triangle of the core starts battle 324 first and the step waits for the win
            // ... and the ten minutes of the bomb that its block sets
            (
                "region(3)".to_string(),
                "triangle 2: moment <27 -> 27 (starts battle 324 first, the step is paid when it is won) (starts a countdown of 600 s)".to_string()
            ),
            // the two points of the escape (moments 120 and 123: no account of the first mission is there)
            ("region(3)".to_string(), "triangle 2: moment ==123 -> 127".to_string()),
            ("entry".to_string(), "entry: moment ==120 -> 123".to_string()),
            // the materia on the floor (entity 6), taken by talking to it
            ("talk".to_string(), "talk to entity 6: the talk script moves the story".to_string())
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
/// how a new account's story judges them, and how many story points there are of each kind. This is what the DATA
/// holds (every field loaded with the survey's loader); what a server offers is the reviewed fields' share of it
/// (`the_fields_that_are_not_reviewed_offer_their_gateways_and_nothing_else_of_the_story`).
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
    let (mut battle_first, mut walk_on_links, mut ok_links, mut fields_with_links) = (0usize, 0usize, 0usize, 0usize);
    for info in &areas {
        let area = adapter.survey_field(&info.id.key).unwrap();
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
        let links = area.links();
        walk_on_links += links.iter().filter(|l| !l.needs_ok).count();
        ok_links += links.iter().filter(|l| l.needs_ok).count();
        fields_with_links += usize::from(!links.is_empty());
        battle_first += p.iter().filter(|point| area.story_battle(point.id).is_some()).count();
        for point in &p {
            *points.entry(match point.trigger {
                StoryTrigger::Region(_) => "region",
                StoryTrigger::Press(_) => "press",
                StoryTrigger::Talk { .. } => "talk",
                StoryTrigger::Line(..) => "line",
                StoryTrigger::Enter => "entry",
                _ => "other",
            }
            .to_string())
            .or_default() += 1;
            ok_points += usize::from(point.label.contains("(OK)"));
        }
    }
    println!("{with_exits} fields with exits, {script} script exits (most {max_script} in {max_field}): new account open {open}, closed {closed}, unjudged {unjudged}; points {points:?} in {fields_with_points} fields, {ok_points} wanting OK, {battle_first} starting a battle first; links {walk_on_links} walk-on and {ok_links} on OK in {fields_with_links} fields");
    assert_eq!((with_exits, script, max_script, max_field.as_str()), (558, 917, 88, "move_f"));
    assert_eq!((open, closed, unjudged), (324, 56, 537));
    // (a lift's panel is a press and a talk that moves the story is a talk point: 49 characters in the archive have one;
    // two points start a fight first, and the ladders and jumps are 141 walk-on lines and 49 that wait for OK)
    assert_eq!(
        (points.get("region").copied(), points.get("press").copied(), points.get("talk").copied(), points.get("line").copied(), points.get("entry").copied()),
        (Some(6), Some(2), Some(49), Some(20), Some(74))
    );
    assert_eq!((fields_with_points, ok_points, battle_first), (95, 2, 2), "the lift's two presses want OK");
    assert_eq!((walk_on_links, ok_links, fields_with_links), (141, 49, 33));
}

/// The script exits of an area (the gateways are data and not counted).
fn script_exit_count(area: &dyn AreaRuntime) -> usize {
    area.exits().iter().filter(|e| matches!(e.kind, ExitKind::Script { .. })).count()
}

/// The gateways of an area.
fn gateways(area: &dyn AreaRuntime) -> Vec<Exit> {
    area.exits().into_iter().filter(|e| e.kind == ExitKind::Gateway).collect()
}

/// **The reviewed list** (`fmv_ff7::REVIEWED_FIELDS`). Only the ten fields of the mission offer their script exits,
/// story points and links; every other field offers its gateways exactly as ever and leaves the rest off, saying what it
/// left off and why. Over the whole archive: gateways are the same everywhere, a field that is not reviewed has no script
/// exit, no point, no link and no step a point could take for any story, its phases are what they were, and what it says
/// it left off is exactly what its data holds. The data holds 917 script exits, 151 points and 190 links (133 points
/// before the writes of a requested script were read: 18 more entry points, one of them the mission's finale in `nrthmk`
/// and the other seventeen in fields that are left off); the ten fields offer 4, 15 and 11 of them (the lift's four
/// exits, the mission's fifteen points, and the ladders and jumps of `nmkin_2`, `nmkin_3` and `nmkin_4`), the other fields
/// that have any leave off the rest (see the numbers asserted at the end).
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_fields_that_are_not_reviewed_offer_their_gateways_and_nothing_else_of_the_story() {
    let Some(adapter) = adapter() else { return };
    // (the list itself is pinned by `the_reviewed_list_is_the_first_reactor_mission_and_nothing_else` in the crate's own
    // tests; a field that joins it changes the numbers asserted at the end, which is the review's last step)
    // (script exits, points, links)
    let (mut offered, mut left_off, mut fields_left_off) = ((0usize, 0usize, 0usize), (0usize, 0usize, 0usize), 0usize);
    let blobs = [Vec::new(), StoryState::at(13).encode(), StoryState::at(33).encode(), StoryState::at(120).encode()];
    for info in adapter.list_areas() {
        let key = info.id.key.as_str();
        let served = adapter.load_area(key).unwrap();
        let surveyed = adapter.survey_field(key).unwrap();
        // gateways everywhere behave exactly as before
        assert_eq!(gateways(served.as_ref()), gateways(&surveyed), "{key}: gateways");
        // phases are not part of what is left off
        for blob in &blobs {
            assert_eq!(served.phase_key(blob), surveyed.phase_key(blob), "{key}: phases");
        }
        let (exits, points, links) = (script_exit_count(&surveyed), surveyed.story_points().len(), surveyed.links().len());
        if fmv_ff7::is_reviewed_field(key) {
            assert_eq!(served.exits(), surveyed.exits(), "{key}");
            assert_eq!(served.story_points(), surveyed.story_points(), "{key}");
            assert_eq!(served.links(), surveyed.links(), "{key}");
            assert_eq!(served.story_withheld(), None, "{key}: a reviewed field leaves nothing off");
            offered = (offered.0 + exits, offered.1 + points, offered.2 + links);
        } else {
            assert_eq!(script_exit_count(served.as_ref()), 0, "{key}: no script exit is offered");
            assert!(served.story_points().is_empty(), "{key}: no story point is offered");
            assert!(served.links().is_empty(), "{key}: no link is offered");
            assert!((0..8).all(|id| served.story_battle(id).is_none()), "{key}: no scripted battle is offered");
            // no step either, whichever point and whichever story (a server only asks about the points it was told of)
            for blob in &blobs {
                for id in 0..8 {
                    assert_eq!(served.story_advance(blob, id), None, "{key}: point {id}");
                }
            }
            match served.story_withheld() {
                Some(w) => {
                    assert_eq!((w.reason.as_str(), w.exits.len(), w.points.len(), w.links.len()), ("unreviewed field", exits, points, links), "{key}");
                    left_off = (left_off.0 + exits, left_off.1 + points, left_off.2 + links);
                    fields_left_off += 1;
                }
                None => assert_eq!((exits, points, links), (0, 0, 0), "{key}: it says nothing only when there was nothing to leave off"),
            }
        }
    }
    println!("offered by the reviewed fields: {offered:?} (script exits, points, links); left off in {fields_left_off} other fields: {left_off:?}");
    assert_eq!(offered, (4, 15, 11));
    assert_eq!((left_off, fields_left_off), ((913, 136, 179), 310));
}

/// The cases that matter, named: the entry point of a Sector 8 field that raises a story from 33 to 36 the moment a
/// player stands there, the three points of the reactor's core, and the debug field with 88 script exits. A server
/// offers none of them; the survey still sees them.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn an_unreviewed_entry_point_does_not_fire_and_the_fields_beyond_the_mission_say_what_they_left_off() {
    let Some(adapter) = adapter() else { return };
    let at_33 = StoryState::at(33).encode();

    // md8_1: one entry point, `moment ==33 -> 36`. The survey sees it and it would apply; the server's field has none
    let surveyed = adapter.survey_field("md8_1").unwrap();
    assert_eq!(surveyed.story_advance(&at_33, 0).map(|s| s.summary), Some("moment=33->36".to_string()), "the data holds it");
    let served = adapter.load_area("md8_1").unwrap();
    assert!(served.story_points().is_empty());
    assert_eq!(served.story_advance(&at_33, 0), None, "a story at 33 stays at 33 in a field nobody has reviewed");
    let w = served.story_withheld().expect("md8_1 leaves its point off");
    assert_eq!((w.reason.as_str(), w.exits.len(), w.points.as_slice()), ("unreviewed field", 0, &["entry: moment ==33 -> 36".to_string()][..]));
    assert_eq!(gateways(served.as_ref()).iter().map(|e| e.target.as_str()).collect::<Vec<_>>(), ["md8_2"], "its gateway is as ever");

    // md8_4 (the field nrthmk's gateway leads to) has neither exits nor points: nothing is left off
    let md8_4 = adapter.load_area("md8_4").unwrap();
    assert_eq!(md8_4.story_withheld(), None);
    assert_eq!(md8_4.exits().iter().map(|e| e.target.as_str()).collect::<Vec<_>>(), ["nrthmk"]);

    // ujunon2: one entry point that starts a scripted fight first (battle 480, the step paid on the win). The survey
    // sees it, with its battle; the server's field has neither the point nor the fight
    let surveyed = adapter.survey_field("ujunon2").unwrap();
    assert_eq!(surveyed.story_points().len(), 1);
    assert_eq!(surveyed.story_battle(0), Some(480), "the data holds the fight");
    let served = adapter.load_area("ujunon2").unwrap();
    assert!(served.story_points().is_empty());
    assert_eq!(served.story_battle(0), None, "a fight nobody reviewed is not started");
    let w = served.story_withheld().expect("ujunon2 leaves its point off");
    assert_eq!(w.points, ["entry: moment <388 -> 388 (starts battle 480 first, the step is paid when it is won)"]);

    // the reactor's core and the stair before it are reviewed: nothing of them is left off
    for field in ["nmkin_4", "nmkin_5"] {
        assert_eq!(adapter.load_area(field).unwrap().story_withheld(), None, "{field}");
    }

    // move_f: 88 script exits, none offered, all named (the log lists the first few and counts the rest)
    let move_f = adapter.load_area("move_f").unwrap();
    assert_eq!(script_exit_count(move_f.as_ref()), 0);
    assert_eq!(move_f.story_withheld().map(|w| (w.exits.len(), w.points.len())), Some((88, 0)));
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


/// The triangles a link's line touches AT ITS LEVEL: a sample along the line lies in the triangle, at a height within
/// `Link::LEVEL` of the line's there.
fn link_triangles(mesh: &fmv_ff7::field::Walkmesh, link: &fmv_adapter_api::Link) -> std::collections::BTreeSet<usize> {
    use fmv_adapter_api::Link;
    let (a, b) = link.line;
    let mut found = std::collections::BTreeSet::new();
    for k in 0..=24 {
        let t = k as f32 / 24.0;
        let (x, y, z) = (a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, a.z + (b.z - a.z) * t);
        let (px, py) = (x.round() as i32, y.round() as i32);
        for (tri, vertices) in mesh.triangles.iter().enumerate() {
            if fmv_ff7::script::triangle_contains_xy(vertices, px, py)
                && fmv_ff7::script::triangle_height_at(vertices, px, py).is_some_and(|h| (h as f32 - z).abs() <= Link::LEVEL)
            {
                found.insert(tri);
            }
        }
    }
    found
}

/// The triangles of `mesh` that a walker can get to from the triangles of `start`, over the access words and over the
/// links (ladders and jumps) of `area` whose line the walker can touch (see [`link_triangles`]). A link puts the walker on
/// the triangle its destination names, from which it walks on. (OK links count: a press is a key, not a place.)
fn reach_with_links(area: &fmv_ff7::Ff7FieldArea, start: usize) -> std::collections::BTreeSet<usize> {
    let mesh = area.walkmesh();
    let mut reach = walkable_from(mesh, start);
    let links = area.links();
    let touched: Vec<std::collections::BTreeSet<usize>> = links.iter().map(|l| link_triangles(mesh, l)).collect();
    loop {
        let before = reach.len();
        for (link, tris) in links.iter().zip(&touched) {
            if tris.iter().any(|t| reach.contains(t)) {
                let to = usize::try_from(link.to_hint.expect("a link names the triangle it leads to")).unwrap();
                reach.extend(walkable_from(mesh, to));
            }
        }
        if reach.len() == before {
            return reach;
        }
    }
}

/// **How far the walkmesh and the links carry the party.** From the triangle each hop of the chain puts it on, the
/// triangles of the next hop's line are reachable on foot through the access words (which is the rule every move the
/// server accepts follows), along the whole first mission: the panel and the lift's line in `elevtr1`, the way on from
/// `nmkin_2`, and, once the ladders and jumps are counted, the way down from `nmkin_3` (`JUMP`s over a gap and two
/// ladders) and from `nmkin_4` (a ladder) to the reactor's core. WITHOUT the links the walkmesh alone ends at `nmkin_3`:
/// its arrival is on the upper level and only 6 of the field's 46 triangles can be walked to from it, and `nmkin_4`'s
/// arrival is a pocket too (22 of its 40 triangles; the gateway to `nmkin_5` is not among them). (The access words are
/// the truth for walking. The client's grid planner (`nav.rs`) keeps the level with every node and agrees with this.)
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_walkmesh_and_the_links_carry_the_party_down_to_the_reactors_core() {
    let Some(adapter) = adapter() else { return };
    let load = |field: &str| adapter.load_field(field).unwrap();
    let exit_to = |area: &fmv_ff7::Ff7FieldArea, target: &str| area.exits().into_iter().find(|e| e.target == target).unwrap_or_else(|| panic!("no exit to {target}"));
    // (field the party stands in, the field it came from, the exit it walks to, whether it can on foot alone)
    let hops: [(&str, &str, &str, bool); 9] = [
        ("md1stin", "", "md1_1", true),
        ("md1_1", "md1stin", "md1_2", true),
        ("md1_2", "md1_1", "nrthmk", true),
        ("nrthmk", "md1_2", "nmkin_1", true),
        ("nmkin_1", "nrthmk", "elevtr1", true),
        ("nmkin_2", "elevtr1", "nmkin_3", true),
        ("nmkin_3", "nmkin_2", "nmkin_4", false),
        ("nmkin_4", "nmkin_3", "nmkin_5", false),
        // (the core has no onward gateway: it is walked to the way back, nmkin_4, which is where the party arrived)
        ("nmkin_5", "nmkin_4", "nmkin_4", true),
    ];
    for (field, from, to, on_foot) in hops {
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
        let reached_on_foot = wanted.iter().any(|t| reach.contains(t));
        let with_links = reach_with_links(&area, start);
        let reached = wanted.iter().any(|t| with_links.contains(t));
        println!("{field}: from triangle {start} ({} on foot, {} with the links) to the line to {to} (triangles {wanted:?}): on foot {reached_on_foot}, with links {reached}", reach.len(), with_links.len());
        assert_eq!(reached_on_foot, on_foot, "{field}: from where the party arrives to the exit to {to}, walking only");
        assert!(reached, "{field}: with the ladders and jumps the party gets to the exit to {to}");
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
    assert!(matches!(&panel.trigger, StoryTrigger::Press(c) if c.len() == 3), "the panel waits for OK");
    // nmkin_3: the woman is on the upper level the jumps lead to; nmkin_5: the chest and the core's triangle are walked to
    // from the arrival without leaving the one component
    let nmkin_3 = load("nmkin_3");
    let arrival = exit_to(&load("nmkin_2"), "nmkin_3").arrival_hint.unwrap() as usize;
    assert!(!walkable_from(nmkin_3.walkmesh(), arrival).contains(&26), "the woman's triangle (26) is not on foot from the arrival");
    assert!(reach_with_links(&nmkin_3, arrival).contains(&26), "... but it is once the first jump is made");
    let nmkin_5 = load("nmkin_5");
    let arrival = exit_to(&load("nmkin_4"), "nmkin_5").arrival_hint.unwrap() as usize;
    let walkable = walkable_from(nmkin_5.walkmesh(), arrival);
    assert!(walkable.contains(&1) && walkable.contains(&2), "the chest (triangle 1) and the core's triangle (2) are on foot from the arrival");
    // and the chest is reached without crossing the core's triangle (which would start the fight before the materia is taken)
    let mut without_core = std::collections::BTreeSet::from([arrival]);
    let mut queue = std::collections::VecDeque::from([arrival]);
    while let Some(t) = queue.pop_front() {
        for &next in nmkin_5.walkmesh().access.get(t).into_iter().flatten() {
            let next = usize::from(next);
            if next != 0xFFFF && next != 2 && next < nmkin_5.walkmesh().triangles.len() && without_core.insert(next) {
                queue.push_back(next);
            }
        }
    }
    assert!(without_core.contains(&1), "the chest can be reached without standing on the core's triangle");
}

/// The ladders and jumps of the first mission, as the scripts of the lines make them (`fmv_ff7::links`): which lines
/// move the player's character, which of them wait for OK, and the triangle each leads to. The fields before `nmkin_2`
/// and `nmkin_5` have none a server can judge. Every destination is on the triangle its script names, and the arming rule
/// (a player put within reach of a walk-on line does not fire it until it has left) is what keeps a landing next to a line
/// from sending the player back.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_ladders_and_jumps_of_the_mission_are_the_ones_the_lines_scripts_make() {
    use fmv_adapter_api::Link;
    let Some(adapter) = adapter() else { return };
    // (what the character does, whether the line waits for OK, the triangle it leads to)
    let links = |field: &str| -> Vec<(String, bool, Option<u32>)> {
        let surveyed = adapter.survey_field(field).unwrap();
        let served = adapter.load_area(field).unwrap().links();
        assert_eq!(served.is_empty(), !fmv_ff7::is_reviewed_field(field) || surveyed.links().is_empty(), "{field}: a server offers the links of the reviewed fields only");
        surveyed.links().iter().map(|l| (l.label.clone(), l.needs_ok, l.to_hint)).collect()
    };
    let l = |label: &str, ok: bool, to: u32| (label.to_string(), ok, Some(to));
    for field in ["md1stin", "md1_1", "md1_2", "nrthmk", "nmkin_1", "elevtr1", "nmkin_5"] {
        assert!(links(field).is_empty(), "{field}");
    }
    assert_eq!(links("nmkin_2"), [l("ladder", true, 102), l("ladder", true, 79), l("jump", false, 34)]);
    assert_eq!(
        links("nmkin_3"),
        [l("jump", false, 42), l("jump", false, 40), l("ladder", true, 28), l("ladder", true, 7), l("ladder", true, 2), l("ladder", true, 44)]
    );
    assert_eq!(links("nmkin_4"), [l("ladder", false, 27), l("ladder", true, 18)]);

    for field in ["nmkin_2", "nmkin_3", "nmkin_4"] {
        let area = adapter.load_field(field).unwrap();
        let all = area.links();
        let mesh = area.walkmesh();
        let mut near: Vec<(usize, usize, i32)> = Vec::new();
        for (i, link) in all.iter().enumerate() {
            let hint = usize::try_from(link.to_hint.unwrap()).unwrap();
            // the destination is on the triangle the script names, and the area puts the player on that floor
            let (x, y) = (link.to.x.round() as i32, link.to.y.round() as i32);
            assert!(fmv_ff7::script::triangle_contains_xy(&mesh.triangles[hint], x, y), "{field}#{i}: the destination is in its triangle");
            let placed = area.place_arrival(link.to, link.to_hint);
            let floor = fmv_ff7::script::triangle_height_at(&mesh.triangles[hint], x, y).unwrap() as f32;
            assert!((placed.z - floor).abs() < 1.0, "{field}#{i}: placed at {} on a floor of {floor}", placed.z);
            assert!(matches!(area.validate_move(placed, placed), fmv_adapter_api::MoveResult::Accepted(p) if p.dist_xy(placed) < 1.0), "{field}#{i}: the destination can be stood on");
            // the lines this landing is within reach of: the walk-on ones are the ones the arming rule is for
            for (j, other) in all.iter().enumerate().filter(|(j, _)| *j != i) {
                let d = other.distance_xy(placed);
                if d <= Link::REACH {
                    near.push((i, j, d.round() as i32));
                }
            }
        }
        println!("{field}: landings within reach of another link's line (from, line, units): {near:?}");
        let expected: &[(usize, usize, i32)] = match field {
            "nmkin_2" => &[(0, 1, 24), (1, 0, 3)],
            "nmkin_3" => &[(0, 1, 23), (2, 3, 12), (3, 2, 13), (4, 5, 15), (5, 4, 10)],
            _ => &[(0, 1, 1)],
        };
        assert_eq!(near, expected, "{field}");
        // a landing next to a line that waits for OK is harmless; the one next to a walk-on line (the first jump of
        // nmkin_3 lands 23 units from the second) is what the arming rule is for: that line starts disarmed
        let walk_on: Vec<_> = near.iter().filter(|(i, j, _)| !all[*i].needs_ok && !all[*j].needs_ok).collect();
        assert_eq!(walk_on.len(), usize::from(field == "nmkin_3"), "{field}: {walk_on:?}");
    }
}

/// The two characters of the first mission whose talk moves the story, as talk points: the woman of `nmkin_3` (14) and
/// the materia on the floor of `nmkin_5` (15). What a talk does is what the character's own script writes for the story
/// the player has (the dialogue walker runs it), so the step is judged on that story: once it has moved nothing is left,
/// and a character that switches itself off when the story has moved is gone.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_woman_and_the_materia_are_talk_points_and_the_talk_scripts_decide_their_steps() {
    let Some(adapter) = adapter() else { return };
    let at = |m: u16| StoryState::at(m).encode();
    let step = |area: &dyn AreaRuntime, id: u32, m: u16| area.story_advance(&at(m), id).map(|s| s.summary);

    // the woman: a point whose centre is where one of the field's NPCs stands, in her talk radius
    let nmkin_3 = adapter.load_area("nmkin_3").unwrap();
    let points = nmkin_3.story_points();
    assert_eq!(points.len(), 1);
    let StoryTrigger::Talk { center, radius } = points[0].trigger else { panic!("{:?}", points[0]) };
    assert_eq!(radius, 80.0, "the radius a character without its own has");
    let npcs = nmkin_3.npcs();
    assert_eq!(npcs.iter().filter(|n| n.pos == center).count(), 1, "one NPC stands at the point's centre: the woman");
    for (moment, want) in [(1, Some("moment=1->14")), (13, Some("moment=13->14")), (14, None), (15, None), (27, Some("var1.225=0->32")), (40, Some("var1.225=0->32"))] {
        assert_eq!(step(nmkin_3.as_ref(), 0, moment).as_deref(), want, "nmkin_3's woman at moment {moment}");
    }
    assert_eq!(nmkin_3.story_battle(0), None);

    // the materia: talkable until the story is at 15, and what the talk does is take it (the story is 15)
    let nmkin_5 = adapter.load_area("nmkin_5").unwrap();
    let points = nmkin_5.story_points();
    let materia = points.iter().find(|p| matches!(p.trigger, StoryTrigger::Talk { .. })).expect("the materia's point");
    assert_eq!(materia.id, 3);
    let StoryTrigger::Talk { center, radius } = materia.trigger else { unreachable!() };
    assert_eq!(radius, 80.0);
    assert_eq!(nmkin_5.npcs().iter().filter(|n| n.pos == center).count(), 1, "one NPC stands at the point's centre: the materia");
    for (moment, want) in [(13, Some("moment=13->15")), (14, Some("moment=14->15")), (15, None), (27, None), (40, None)] {
        assert_eq!(step(nmkin_5.as_ref(), 3, moment).as_deref(), want, "nmkin_5's materia at moment {moment}");
    }
}

/// The guard scorpion: the core of the reactor starts battle 324 when the party comes to its triangle, and the story
/// steps to 27 when it is WON (the point's step is judged on the story the party has then: 14 or 15 both go to 27).
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_guard_scorpion_is_the_battle_the_core_starts_before_its_step() {
    let Some(adapter) = adapter() else { return };
    let area = adapter.load_area("nmkin_5").unwrap();
    let points = area.story_points();
    assert_eq!(points.len(), 4);
    assert_eq!(area.story_battle(0), Some(324), "the triangle of the core");
    for id in 1..4 {
        assert_eq!(area.story_battle(id), None, "point {id} starts no fight");
    }
    let at = |m: u16| StoryState::at(m).encode();
    for (moment, want) in [(13, Some("moment=13->27")), (14, Some("moment=14->27")), (15, Some("moment=15->27")), (26, Some("moment=26->27")), (27, None), (40, None)] {
        assert_eq!(area.story_advance(&at(moment), 0).map(|s| s.summary).as_deref(), want, "the core at moment {moment}");
    }
    // no other reviewed field starts a fight first
    for field in fmv_ff7::REVIEWED_FIELDS.iter().filter(|f| **f != "nmkin_5") {
        let area = adapter.load_area(field).unwrap();
        assert!(area.story_points().iter().all(|p| area.story_battle(p.id).is_none()), "{field}");
    }
}

/// The whole first mission as a story, now to the end of the reactor: a new account takes the steps the real scripts make
/// and ends at moment 27 with the lift down: the woman (14), the materia (15) and the guard scorpion (27) after the
/// seven steps of the first chain.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn walking_the_whole_mission_moves_a_new_account_from_1_to_27() {
    let Some(adapter) = adapter() else { return };
    let steps = [
        ("md1stin", 0, "moment=1->6"),
        ("md1_1", 0, "moment=6->7"),
        ("md1_2", 0, "moment=7->8"),
        ("nrthmk", 0, "moment=8->10"),
        ("nmkin_1", 0, "moment=10->11"),
        ("elevtr1", 0, "moment=11->12 var1.225=0->1"),
        ("nmkin_2", 0, "moment=12->13"),
        ("nmkin_3", 0, "moment=13->14"),
        ("nmkin_5", 3, "moment=14->15"),
        ("nmkin_5", 0, "moment=15->27"),
    ];
    let mut blob: Vec<u8> = Vec::new();
    for (field, point, summary) in steps {
        let area = adapter.load_area(field).unwrap();
        let step = area.story_advance(&blob, point).unwrap_or_else(|| panic!("{field}#{point} does not apply to {blob:02x?}"));
        assert_eq!(step.summary, summary, "{field}");
        blob = step.progress;
        assert_eq!(area.story_advance(&blob, point), None, "{field}#{point} a second time");
    }
    assert_eq!(blob, vec![2, 27, 0, 1, 1, 225, 1], "moment 27, the lift bit on");
}

/// **The ride back up.** The panel of the lift (`elevtr1`, triangle 8) is two presses of one triangle: the ride down (the
/// lift bit on, and the story to 12 the first time) and the ride back, which the panel's OWN script makes when the lift is
/// down (it asks the director's script to clear the bit), run for the story the player has. A press is one ride: down,
/// back, down again, and the lift's exits follow the bit (up to `nmkin_1` while it is up, down to `nmkin_2` while it is
/// down) with the story's moment untouched. The model only carries a story forward, so the saved story keeps the bit
/// (the join of the two), and the player's story in the world is what the lift is for this session.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_lift_rides_back_up_with_every_press_and_never_flaps() {
    let Some(adapter) = adapter() else { return };
    let area = adapter.load_area("elevtr1").unwrap();
    let points = area.story_points();
    assert_eq!(points.iter().filter(|p| matches!(&p.trigger, StoryTrigger::Press(c) if c.len() == 3)).count(), 2, "{points:?}");
    let mut blob: Vec<u8> = state(11, false);
    // (the point the press reaches, what it did, where the lift's exit leads afterwards)
    for (point, summary, leads) in [
        (0, "moment=11->12 var1.225=0->1", "nmkin_2"),
        (1, "var1.225=1->0", "nmkin_1"),
        (0, "var1.225=0->1", "nmkin_2"),
        (1, "var1.225=1->0", "nmkin_1"),
        (1, "", "nmkin_1"),
    ] {
        match area.story_advance(&blob, point) {
            Some(step) => {
                assert_eq!(step.summary, summary, "point {point} at {blob:02x?}");
                blob = step.progress;
            }
            None => assert_eq!(summary, "", "point {point} does not apply to {blob:02x?}"),
        }
        assert_eq!(open_target(area.as_ref(), &blob), [leads], "the lift's exit after point {point}");
        // exactly one of the two applies in any state: a press is one ride, never both
        let both = (0..2).filter(|&p| area.story_advance(&blob, p).is_some()).count();
        assert_eq!(both, 1, "{blob:02x?}: one ride at a time");
    }
    // later in the story the ride back still works (the lift is the same lift) and the moment stays where it was
    let late = state(33, true);
    let back = area.story_advance(&late, 1).expect("a ride back at moment 33");
    assert_eq!((back.summary.as_str(), StoryState::decode(&back.progress).unwrap().moment), ("var1.225=1->0", 33));
    // the saved story is the join: the lift stays down in the store while the story the player holds says up
    assert_eq!(adapter.merge_progression(&late, &back.progress), late);
}

/// **The links trap nobody and stay off the way that was walked before they were offered.** Every reviewed field's links
/// are live for every player in it, so two things are pinned. (1) From where each link puts a player, walking and the links
/// lead to an exit of the field or to the triangles players arrive on: no link drops anybody in a pocket. (2) In `nmkin_2`,
/// which was walked on foot from the lift to the way on before it had links (and has a walk-on jump), the shortest walk
/// over the triangles touches no walk-on line of the field: the jump is not on the way, so the route that worked still
/// works with no help from the bots' avoidance of lines.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_links_trap_nobody_and_the_walk_that_worked_before_does_not_touch_them() {
    let Some(adapter) = adapter() else { return };
    let chain = ["elevtr1", "nmkin_1", "nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5"];
    for field in ["nmkin_2", "nmkin_3", "nmkin_4"] {
        let area = adapter.load_field(field).unwrap();
        let mesh = area.walkmesh();
        let links = area.links();
        assert!(!links.is_empty(), "{field}");
        // where a player can end up on its own: the exits' triangles, and the triangles the other fields arrive on
        let mut safe: std::collections::BTreeSet<usize> = area.exits().iter().flat_map(|e| triangles_at(mesh, e)).collect();
        for other in chain {
            let from = adapter.load_field(other).unwrap();
            safe.extend(from.exits().iter().filter(|e| e.target == field).filter_map(|e| e.arrival_hint).map(|h| h as usize));
        }
        for (i, link) in links.iter().enumerate() {
            let dest = usize::try_from(link.to_hint.unwrap()).unwrap();
            let reach = reach_with_links(&area, dest);
            let out = reach.iter().filter(|t| safe.contains(t)).count();
            println!("{field}#{i}: from triangle {dest}: {} triangles on foot and by links, {out} of them exits or arrivals", reach.len());
            assert!(out > 0, "{field}#{i}: the destination (triangle {dest}) is a pocket: nothing from there reaches an exit or an arrival");
        }
    }

    // nmkin_2: the shortest way on foot from the lift's arrival to the gateway to nmkin_3 touches no walk-on line
    let area = adapter.load_field("nmkin_2").unwrap();
    let mesh = area.walkmesh();
    let start = adapter.load_field("elevtr1").unwrap().exits().iter().find(|e| e.target == "nmkin_2").and_then(|e| e.arrival_hint).unwrap() as usize;
    let goal: std::collections::BTreeSet<usize> = area.exits().iter().filter(|e| e.target == "nmkin_3").flat_map(|e| triangles_at(mesh, e)).collect();
    let mut parent = vec![usize::MAX; mesh.triangles.len()];
    let mut queue = std::collections::VecDeque::from([start]);
    let mut seen = std::collections::BTreeSet::from([start]);
    let mut found = None;
    while let Some(t) = queue.pop_front() {
        if goal.contains(&t) {
            found = Some(t);
            break;
        }
        for &next in mesh.access.get(t).into_iter().flatten() {
            let next = usize::from(next);
            if next != 0xFFFF && next < mesh.triangles.len() && seen.insert(next) {
                parent[next] = t;
                queue.push_back(next);
            }
        }
    }
    let mut path = vec![found.expect("nmkin_2 is walked on foot from the lift to nmkin_3")];
    while let Some(&last) = path.last() {
        if parent[last] == usize::MAX {
            break;
        }
        path.push(parent[last]);
    }
    let path: std::collections::BTreeSet<usize> = path.into_iter().collect();
    for (i, link) in area.links().iter().enumerate().filter(|(_, l)| !l.needs_ok) {
        let touched = link_triangles(mesh, link);
        println!("nmkin_2 walk-on link #{i}: touches {touched:?}; the shortest way is {} triangles", path.len());
        assert!(touched.is_disjoint(&path), "the shortest way on foot passes over walk-on link #{i} of nmkin_2");
    }
}

/// **The bomb's countdown.** The Guard Scorpion's block sets a timer of ten minutes (`STTIM 0:10:00`, just before the
/// moment is written), which the point carries as the step's countdown. It is live while the story's moment is still 27
/// (every reactor field's timer window and timeout test `Var[2][0] == 27`): the lift going up or down does not end it
/// and the finale's step to 33 does, and a story nobody can read has none.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_guard_scorpion_starts_a_ten_minute_countdown_that_the_finale_makes_moot() {
    let Some(adapter) = adapter() else { return };
    let core = adapter.load_area("nmkin_5").unwrap();
    for moment in [13, 14, 15, 26] {
        let step = core.story_advance(&state(moment, true), 0).unwrap_or_else(|| panic!("the core at {moment}"));
        assert_eq!(step.countdown, Some(600), "ten minutes at moment {moment}");
    }
    let won = core.story_advance(&state(15, true), 0).unwrap();
    assert_eq!((won.summary.as_str(), won.progress.as_slice()), ("moment=15->27", &[2, 27, 0, 1, 1, 225, 1][..]), "the blob the smoke pins: 021b000101e101");
    // nothing else of the mission starts a countdown (the other points of this field and of every reviewed field)
    for field in fmv_ff7::REVIEWED_FIELDS {
        let area = adapter.load_area(field).unwrap();
        for point in area.story_points() {
            for blob in [vec![], state(10, false), state(15, true), state(27, true), state(27, false), state(33, false), state(120, true), state(123, true)] {
                if let Some(step) = area.story_advance(&blob, point.id) {
                    assert_eq!(step.countdown.is_some(), field == "nmkin_5" && point.id == 0, "{field}#{}: {step:?}", point.id);
                }
            }
        }
    }
    // what the countdown is for, judged by the adapter
    let at_27 = state(27, true);
    assert!(core.countdown_live(&at_27, &at_27));
    assert!(core.countdown_live(&at_27, &state(27, false)), "the lift riding up does not end it");
    assert!(core.countdown_live(&state(27, false), &at_27), "nor does it riding down");
    assert!(!core.countdown_live(&at_27, &state(33, true)), "the finale ends it");
    assert!(!core.countdown_live(&at_27, &state(28, true)));
    assert!(!core.countdown_live(&at_27, &state(15, true)));
    assert!(!core.countdown_live(&at_27, &[9, 9]), "a story nobody can read has none");
    assert!(!core.countdown_live(&[9, 9], &at_27));
    // it is the same for every reviewed field (the world asks the area the account stood in)
    for field in fmv_ff7::REVIEWED_FIELDS {
        let area = adapter.load_area(field).unwrap();
        assert!(area.countdown_live(&at_27, &at_27) && !area.countdown_live(&at_27, &state(33, false)), "{field}");
    }
}

/// **The finale of the mission.** The party that walks out of the reactor at moment 27 or later is moved to 33 by the
/// loop of a nameless entity of `nrthmk`, which runs the director's cutscene script (the explosion's movie, the moment
/// written, the lift's bit turned on, then a jump to `md8_4`, a field with no player-controlled character whose own
/// scripts jump on to `md8_1`: the end of what a server can judge). The point is the second of `nrthmk` (after the way in,
/// `entry <10`), fires when a player comes to stand there, and applies only while the story is not past it:
///
/// * 27 to 33, whichever way the lift's bit stands (the finale turns it on: `var1.225=0->1` when it was up), and any
///   moment between (28 to 32) goes to 33 too;
/// * 26 and before: not yet (the party has not set the bomb), and 33 is only a step where it turns the lift's bit on;
/// * the late party's game over (the loop that wants the timeout flag set) is not a point at all: nothing sets the flag.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_finale_moves_a_party_that_walks_out_at_27_to_33_and_applies_to_a_late_party_too() {
    let Some(adapter) = adapter() else { return };
    let area = adapter.load_area("nrthmk").unwrap();
    let points = area.story_points();
    assert_eq!(points.len(), 2, "the way in, and the finale: {points:?}");
    assert!(points[1].is_on_entry(), "the finale fires on coming to stand in nrthmk");
    assert_eq!(area.story_battle(1), None);
    let step = |blob: &[u8]| area.story_advance(blob, 1);
    // 27 to 33 with the lift down (what the descent left): the moment, and nothing else changed (the bit is on)
    let done = step(&state(27, true)).expect("the finale at 27, lift down");
    assert_eq!((done.summary.as_str(), done.progress.as_slice(), done.countdown), ("moment=27->33", &[2, 33, 0, 1, 1, 225, 1][..], None));
    // with the lift up (the party rode back up first): the bit is turned on again by the cutscene
    let done = step(&state(27, false)).expect("the finale at 27, lift up");
    assert_eq!((done.summary.as_str(), done.progress.as_slice()), ("moment=27->33 var1.225=0->1", &[2, 33, 0, 1, 1, 225, 1][..]));
    for moment in [28u16, 30, 32] {
        assert_eq!(step(&state(moment, true)).map(|s| s.summary), Some(format!("moment={moment}->33")), "{moment}");
    }
    // not before the bomb is set
    for moment in [0u16, 1, 11, 15, 26] {
        assert_eq!(step(&state(moment, true)), None, "moment {moment}");
        assert_eq!(step(&state(moment, false)), None, "moment {moment}");
    }
    assert_eq!(step(&[]), None, "a new account");
    // at 33 and later it is a step only where it turns the lift's bit on, and never moves the moment back
    assert_eq!(step(&state(33, true)), None);
    assert_eq!(step(&state(33, false)).map(|s| s.summary).as_deref(), Some("var1.225=0->1"));
    assert_eq!(step(&state(40, true)), None);
    // the step is once: after it the story is at 33 with the bit on
    assert_eq!(step(&done.progress), None);
    // nobody can have the timeout flag set, so the late party's loop is not a point and a story that does has none
    let mut timed_out = StoryState::at(27);
    assert!(!timed_out.set_bit(1, 225, 7, true), "the model refuses to turn the flag on");
    let blob = vec![2, 27, 0, 1, 1, 225, 0x80];
    assert_eq!(step(&blob), None, "a story with the flag set (nobody makes one) is not moved by the finale");
    // the other reviewed fields have no finale
    for field in fmv_ff7::REVIEWED_FIELDS.iter().filter(|f| **f != "nrthmk") {
        let other = adapter.load_area(field).unwrap();
        for id in 0..8 {
            if let Some(s) = other.story_advance(&state(27, true), id) {
                assert_ne!(StoryState::decode(&s.progress).map(|st| st.moment), Some(33), "{field}#{id}: {s:?}");
            }
        }
    }
}

/// The arrival points of `field`: the arrival of every exit of every field of the archive that leads to it (the hint
/// triangle the server puts the player on), and, for `home`, the triangles under the spawn points.
fn arrivals_of(adapter: &Ff7Adapter, field: &str, home: &str) -> std::collections::BTreeSet<usize> {
    let area = adapter.load_field(field).unwrap();
    let mesh = area.walkmesh();
    let mut found = std::collections::BTreeSet::new();
    if field == home {
        for spawn in area.spawn_points() {
            let at = |x: f32, y: f32| mesh.triangles.iter().position(|t| fmv_ff7::script::triangle_contains_xy(t, x.round() as i32, y.round() as i32));
            found.extend(at(spawn.x, spawn.y));
        }
    }
    for info in adapter.list_areas() {
        let from = adapter.load_field(&info.id.key).unwrap();
        for exit in from.exits().iter().filter(|e| e.target == field) {
            let placed = area.place_arrival(exit.arrival, exit.arrival_hint);
            if let Some(t) = mesh.triangles.iter().enumerate().find(|(_, t)| fmv_ff7::script::triangle_contains_xy(t, placed.x.round() as i32, placed.y.round() as i32)) {
                found.insert(exit.arrival_hint.map_or(t.0, |h| h as usize));
            }
        }
    }
    found
}

/// **Nobody is stranded at the new moments.** A re-login always joins `md1stin`, and a party at 27 or 33 walks the
/// mission's fields in the order its story lets it. In every reviewed field, from every place a player arrives (the
/// arrival of every exit of the archive that leads there, `md1stin`'s spawn points), walking and the ladders and
/// jumps reach a line of an exit that is OPEN for the story, at 27 with the lift up and down and at 33 with the lift up
/// and down. (Open exits exist in every state: what is pinned is that one of them can be WALKED to.) The cast does not
/// change with the moment (the server keeps every field's characters as the data places them: a known gap), and
/// the phases of the fields are the intervals their scripts compare the moment with.
///
/// **The one place that is a dead end: the lift at 33.** Its way on from moment 33 leads to the second reactor
/// (`smkin_1`, `smkin_2`), which nobody has reviewed, so a served field closes it (the data says it is open: the survey's
/// loader shows it) and its way back is the first mission's, closed by the story from 33 on. The story never takes a party
/// there (the finale ends in `nrthmk`, the lift is behind it); only a party that walks back into the reactor AFTER the
/// finale stands there, and a member that comes to the lift after its leader's finale is taken through by the leader's
/// way out instead (the server's `follow` module). Reviewing `smkin_1` and `smkin_2` opens it.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn nobody_is_stranded_at_27_or_33_in_any_reviewed_field() {
    let Some(adapter) = adapter() else { return };
    let stories = [("27, lift up", state(27, false)), ("27, lift down", state(27, true)), ("33, lift up", state(33, false)), ("33, lift down", state(33, true))];
    for field in fmv_ff7::REVIEWED_FIELDS {
        let area = adapter.load_field(field).unwrap();
        let mesh = area.walkmesh();
        let arrivals = arrivals_of(&adapter, field, "md1stin");
        assert!(!arrivals.is_empty(), "{field} is arrived at from somewhere");
        for (name, story) in &stories {
            let open: Vec<Exit> = area.exits().into_iter().filter(|e| area.exit_state(e, story) == ExitState::Open).collect();
            if field == "elevtr1" && name.starts_with("33") {
                // the one dead end (see above): closed by the policy, open in the data, and nowhere else
                assert!(open.is_empty(), "{field} at {name}: the lift's way on leads out of the reviewed fields: {open:?}");
                let surveyed = adapter.survey_field(field).unwrap();
                let data: Vec<String> = surveyed.exits().iter().filter(|e| surveyed.exit_state(e, story) == ExitState::Open).map(|e| e.target.clone()).collect();
                assert!(data.len() == 1 && data[0].starts_with("smkin_"), "the data's way on is the second reactor's: {data:?}");
                continue;
            }
            assert!(!open.is_empty(), "{field} at {name}: some exit is open");
            let open_triangles: std::collections::BTreeSet<usize> = open.iter().flat_map(|e| triangles_at(mesh, e)).collect();
            for &start in &arrivals {
                let reach = reach_with_links(&area, start);
                assert!(reach.iter().any(|t| open_triangles.contains(t)), "{field} at {name}: from the arrival on triangle {start} no open exit ({:?}) can be walked to", open.iter().map(|e| e.target.as_str()).collect::<Vec<_>>());
            }
        }
        // Only the reviewed changes to NPC visibility follow these late moments.
        for (name, story) in &stories {
            let phase = area.phase_key(story).expect("a story the adapter reads");
            let mut expected = area.npcs();
            if field == "nmkin_5" {
                expected.iter_mut().filter(|npc| npc.entity_name == "mtr").for_each(|npc| npc.unconditional = false);
            }
            assert_eq!(area.npcs_in_phase(&phase), expected, "{field} at {name}: only reviewed cast changes");
        }
    }
    // nothing of the mission re-fires at 33: the points of the reviewed fields apply to the story before them only, except the
    // lift's panel (it rides back up as often as it is pressed) and the woman of nmkin_3 (her talk frees her once, from moment
    // 27 on, and moves no moment)
    for field in fmv_ff7::REVIEWED_FIELDS {
        let area = adapter.load_area(field).unwrap();
        for point in area.story_points() {
            let step = area.story_advance(&state(33, true), point.id);
            assert_eq!(step.is_some(), (field == "elevtr1" && point.id == 1) || (field == "nmkin_3" && point.id == 0), "{field}#{}: {step:?}", point.id);
        }
    }
}

/// **What each field is at 27 and after: its phase.** The phase of a field for a story is the interval of the moment that its
/// own scripts compare the counter with, so two accounts in the same interval share one instance and see each other. The
/// first three fields only compare the moment against numbers of the way in (7 and 8 at the most): they are one phase for every
/// story from 8 on, the escape's and the later ones. The six reactor fields and `nrthmk` compare it with 27: moment 27 (the
/// bomb is set and the countdown runs) is a phase of its own in each, 28 on (the party has got out) another (the lift's own
/// field has one more edge, at 33, where its way leads to the second reactor), and the finale's step from 27 to 33 moves
/// the party from the first to the second.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn each_reviewed_field_is_one_phase_at_27_of_its_own_and_one_from_28_on() {
    let Some(adapter) = adapter() else { return };
    let phase = |area: &dyn AreaRuntime, moment: u16| area.phase_key(&state(moment, false)).expect("a story the adapter reads");
    for field in ["md1stin", "md1_1", "md1_2"] {
        let area = adapter.load_area(field).unwrap();
        assert!([8, 26, 27, 28, 33, 120].iter().all(|&m| phase(area.as_ref(), m) == phase(area.as_ref(), 8)), "{field}: one phase from 8 on");
    }
    for field in ["nrthmk", "elevtr1", "nmkin_1", "nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5"] {
        let area = adapter.load_area(field).unwrap();
        let (before, bomb, out) = (phase(area.as_ref(), 26), phase(area.as_ref(), 27), phase(area.as_ref(), 28));
        assert_ne!(before, bomb, "{field}: 27 is not 26");
        assert_ne!(bomb, out, "{field}: 27 is not 28");
        assert!([28, 30, 32].iter().all(|&m| phase(area.as_ref(), m) == out), "{field}: 28 on is one phase (up to the next comparison: the lift's own, at 33)");
        // The lift never changes the cast; rescuing Jessie changes only her own field phase.
        let mut variant = StoryState::at(27);
        assert!(variant.set_bit(1, 225, 0, true) && variant.set_bit(1, 225, 5, true));
        if field == "nmkin_3" {
            assert_ne!(area.phase_key(&variant.encode()), Some(bomb.clone()), "Jessie has left");
        } else {
            assert_eq!(area.phase_key(&variant.encode()), Some(bomb.clone()), "{field}");
        }
    }
}

/// **The woman who is stuck on the way out.** The talk point of `nmkin_3` (entity 4) was the way down's step 13 to 14; at
/// moment 27 or later its own script takes the other branch: with bit 5 of the reactor byte clear it turns the bit on (she
/// is freed and walks off), and with it set she is gone (her set-up hides her), so the talk is a step once. It moves no
/// moment and no exit or point waits for the bit; it is a story step the party takes, saved with the story (the lift's bit
/// and hers are both in the byte, whichever order the party does them in), and it never gets in the way of the lift's ride
/// back or the finale.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_woman_of_nmkin_3_is_freed_on_the_way_out_once_and_it_changes_nothing_else() {
    let Some(adapter) = adapter() else { return };
    let woman = adapter.load_area("nmkin_3").unwrap();
    let step = |blob: &[u8]| woman.story_advance(blob, 0);
    // the way out: at 27 with the lift down (what the descent left), the first talk frees her
    let freed = step(&state(27, true)).expect("a talk at 27");
    assert_eq!((freed.summary.as_str(), freed.progress.as_slice(), freed.countdown), ("var1.225=1->33", &[2, 27, 0, 1, 1, 225, 0x21][..], None));
    assert_eq!(step(&freed.progress), None, "she is gone: the talk is a step once");
    // any moment from 27 on (the escape's own, and later ones), with the lift either way
    for moment in [27u16, 28, 33, 40] {
        assert!(step(&state(moment, true)).is_some() && step(&state(moment, false)).is_some(), "moment {moment}");
    }
    // not before the bomb is set: the way down's talk stops at 14, and between 14 and 27 there is nothing to say that moves anything
    for moment in [14u16, 15, 20, 26] {
        assert_eq!(step(&state(moment, true)), None, "moment {moment}");
    }
    // the saved story keeps both bits, in whichever order the party takes the lift's ride back and the talk
    let mut s = StoryState::at(27);
    assert!(s.set_bit(1, 225, 0, true));
    let stored = s.encode();
    let mut later = StoryState::at(27);
    assert!(later.set_bit(1, 225, 5, true)); // the talk after the lift's ride back up: bit 0 off, bit 5 on
    assert_eq!(adapter.merge_progression(&stored, &later.encode()), vec![2, 27, 0, 1, 1, 225, 0x21]);
    assert_eq!(adapter.merge_progression(&later.encode(), &stored), vec![2, 27, 0, 1, 1, 225, 0x21]);
    // her bit changes nothing about the lift or the finale
    let lift = adapter.load_area("elevtr1").unwrap();
    let freed_and_down = freed.progress.clone();
    assert_eq!(open_target(lift.as_ref(), &freed_and_down), ["nmkin_2"], "the lift's exit follows its own bit only");
    let back = lift.story_advance(&freed_and_down, 1).expect("the ride back with her bit set");
    assert_eq!((back.summary.as_str(), back.progress.as_slice()), ("var1.225=33->32", &[2, 27, 0, 1, 1, 225, 0x20][..]));
    assert_eq!(open_target(lift.as_ref(), &back.progress), ["nmkin_1"]);
    let finale = adapter.load_area("nrthmk").unwrap().story_advance(&back.progress, 1).expect("the finale with her bit set");
    assert_eq!((finale.summary.as_str(), finale.progress.as_slice()), ("moment=27->33 var1.225=32->33", &[2, 33, 0, 1, 1, 225, 0x21][..]));
    // and the story the smoke ends with
    assert_eq!(adapter.merge_progression(&[2, 27, 0, 1, 1, 225, 0x21], &finale.progress), finale.progress);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn jessies_saved_rescue_changes_only_her_visibility_and_field_phase() {
    use fmv_ff7::script::scene::{jessie_rescue_scene, ScenePlay, Stage};
    let adapter = adapter().expect("FMV_FF7_DATA");
    let area = adapter.load_field("nmkin_3").unwrap();
    let base = area.npcs();
    let jessie = base.iter().position(|n| n.entity_name == "av_j").expect("Jessie");
    assert!(base.len() > 1 && base[jessie].unconditional);
    let mut archive = fmv_ff7::lgp::LgpArchive::open_path(flevel_path(&data_dir().unwrap())).unwrap();
    let field = script_common::load_field(&mut archive, "nmkin_3").unwrap();
    for moment in [1, 13, 14, 26, 27, 28, 33, 40] {
        let before = StoryState::at(moment);
        let phase = area.phase_key(&before.encode()).unwrap();
        assert_eq!(area.npcs_in_phase(&phase), base);
        let mut after = before.clone();
        after.set_bit(1, 225, 5, true);
        let rescued_phase = area.phase_key(&after.encode()).unwrap();
        let mut expected = base.clone();
        if moment >= 27 {
            assert_ne!(phase, rescued_phase, "same moment, changed cast");
            expected[jessie].unconditional = false;
            // The owned setup hides Jessie at exactly these story states.
            let scene = ScenePlay::new(&field, &jessie_rescue_scene(), &after, 1).unwrap();
            assert!(scene.initial_stage_events().iter().any(|e| matches!(e, Stage::Show { entity: 4, visible: false })));
        } else {
            assert_eq!(phase, rescued_phase, "rescue flag is irrelevant before escape");
        }
        assert_eq!(area.npcs_in_phase(&rescued_phase), expected, "slots, other NPCs and coordinates preserved");
        assert_eq!(area.phase_key(&StoryState::decode(&after.encode()).unwrap().encode()), Some(rescued_phase.clone()), "same after re-login");
        after.set_bit(1, 225, 0, true);
        assert_eq!(area.phase_key(&after.encode()), Some(rescued_phase), "lift does not change this cast");
    }
    assert_eq!(area.phase_key(&[255, 1, 2]), None, "unreadable progression is not guessed");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn reactor_materia_disappears_at_its_existing_story_step() {
    use fmv_ff7::script::scene::{guard_scorpion_scene, CastMember, Look, ScenePlay, ScriptRef, Stage};
    let adapter = adapter().expect("FMV_FF7_DATA");
    let area = adapter.load_field("nmkin_5").unwrap();
    let base = area.npcs();
    let index = base.iter().position(|npc| npc.entity_name == "mtr").expect("materia");
    let npc = &base[index];
    assert!(npc.unconditional);
    let mut archive = fmv_ff7::lgp::LgpArchive::open_path(flevel_path(&data_dir().unwrap())).unwrap();
    let field = script_common::load_field(&mut archive, "nmkin_5").unwrap();
    let mut def = guard_scorpion_scene();
    def.setup = vec![ScriptRef::new(6, 0)];
    def.cast = vec![CastMember { entity: 6, name: "mtr".into(),
        look: Look::Model { hrc: npc.appearance.model.clone(), idle: npc.appearance.idle_animation.clone().unwrap() }, start: None }];
    for moment in [0, 1, 13, 14, 15, 16, 26, 27, 28, 33, 120, 127, 32767, 32768, u16::MAX] {
        let before = StoryState::at(moment);
        let phase = area.phase_key(&before.encode()).unwrap();
        let script = ScenePlay::new(&field, &def, &before, 1).unwrap();
        let hidden = script.initial_stage_events().iter().any(|event| matches!(event, Stage::Show { entity: 6, visible: false }));
        assert_eq!(hidden, (moment as i16) >= 15, "owned setup uses a signed word at {moment}");
        let mut expected = base.clone();
        expected[index].unconditional = !hidden;
        assert_eq!(area.npcs_in_phase(&phase), expected, "same slots, positions and other actors at {moment}");
        assert_eq!(area.phase_key(&StoryState::decode(&before.encode()).unwrap().encode()), Some(phase), "same after reload");
    }
    let before = StoryState::at(14).encode();
    let step = area.story_advance(&before, 3).expect("existing materia talk point");
    assert_eq!(step.summary, "moment=14->15");
    assert_eq!(step.progress, StoryState::at(15).encode(), "unchanged saved format and effects");
    assert_ne!(area.phase_key(&before), area.phase_key(&step.progress), "immediate snapshot after step");
    assert!(area.story_advance(&step.progress, 3).is_none(), "not repeatable");
    assert_eq!(area.npcs_in_phase(&fmv_core::PhaseKey(vec![255])), base, "unknown phase is not interpreted");
}
