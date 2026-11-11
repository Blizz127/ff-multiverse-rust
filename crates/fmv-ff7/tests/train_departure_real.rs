//! Original passenger-train departure and Sector 7 arrival, evaluated read-only.
//!
//! The owned-data tests require `FMV_FF7_DATA` and `--ignored`. They retain numeric script
//! evidence only. These definitions remain offline: reaching MAPJUMP while MOVIE is skipped
//! is interpreter evidence, not acceptance of the original transition's presentation.

mod script_common;

use fmv_core::Pos;
use fmv_ff7::background::BackgroundOp;
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::{End, MessageOrigin};
use fmv_ff7::script::scene::{
    run_to_end_with_resources_strict, sector7_station_scene, train_departure_scene,
    with_trigger_position, Ground, MoveHow, Resources, SceneBoundary, SceneJump, ScenePlay,
    ScriptRef, Stage, Then, WindowAck,
};
use fmv_ff7::script::FieldScript;
use fmv_ff7::story::StoryState;
use script_common::{data_dir, flevel_path, load_field};

fn field(key: &str) -> FieldFile {
    let dir = data_dir().expect("FMV_FF7_DATA is required for the explicitly requested owned-data test");
    let mut archive = LgpArchive::open_path(flevel_path(&dir)).unwrap();
    load_field(&mut archive, key).unwrap()
}

/// An explicit test input, not a new-game default: Jessie has been heard, and Biggs's
/// independent facing bit is known clear. The other six bits deliberately remain unknown.
fn departure_story() -> StoryState {
    let mut story = StoryState::at(51).into_extended().unwrap();
    story.write_masked(3, 223, 1 << 2, (1 << 0) | (1 << 2)).unwrap();
    story
}

/// Acknowledge each exact non-choice window. No guessed answers or cursor defaults are used.
fn finish(play: &mut ScenePlay) -> (End, Vec<Stage>, Vec<(u8, usize)>) {
    let mut events = play.initial_stage_events().to_vec();
    let mut windows = Vec::new();
    let mut ack = None;
    for _ in 0..64 {
        let beat = play.next_strict(ack.take()).unwrap();
        events.extend(beat.stage.into_iter().map(|event| event.what));
        match beat.then {
            Then::End { how, .. } => return (how, events, windows),
            Then::Window(window) => {
                assert!(window.message.choice.is_none(), "this departure and arrival contain no ASK");
                let MessageOrigin::Canon { dialog, .. } = window.message.origin else {
                    panic!("the original field supplies each line")
                };
                windows.push((window.opener, dialog));
                let Some(SceneBoundary::Window(checkpoint)) = play.boundary() else {
                    panic!("a shown window has an exact acknowledgement boundary")
                };
                ack = Some(WindowAck { checkpoint, answer: None });
            }
        }
    }
    panic!("the authored sequence did not finish within its window budget")
}

#[test]
fn departure_requires_jessies_flag_known_set_at_the_train_moment() {
    let def = train_departure_scene();
    assert!(!def.offered_to(&StoryState::at(51)), "legacy absence is not a known flag");
    let mut story = StoryState::at(51).into_extended().unwrap();
    assert!(!def.offered_to(&story));
    story.write_masked(3, 223, 0, 1 << 2).unwrap();
    assert!(!def.offered_to(&story), "the field disables this line before Jessie is heard");
    story.write_masked(3, 223, 1 << 2, 1 << 2).unwrap();
    assert!(def.offered_to(&story));
    assert_eq!(story.known_byte(3, 223), None, "one known guard does not invent other bits");
    for moment in [48, 50, 52, 63, 108] {
        story.moment = moment;
        assert!(!def.offered_to(&story), "moment {moment} uses another field script path");
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); departure is not registered"]
fn departure_uses_the_guarded_original_line_and_requires_movie_24_before_the_station() {
    let f = field("tin_1");
    let def = train_departure_scene();
    assert_eq!((def.trigger.entity, def.trigger.slot, def.origin_actor()), (27, 2, Some(30)));
    let line = def.line_trigger().unwrap();
    assert_eq!((line.a, line.b), ([-55, -210, -53], [52, -220, -53]));
    let script = FieldScript::from_field(&f).unwrap();
    let border = script.disassemble(27, 0).unwrap();
    let authored_line = border.iter().find(|ins| ins.offset == 0x13F7).unwrap();
    assert_eq!(authored_line.mnemonic, "LINE");
    assert_eq!(["x1", "y1", "z1", "x2", "y2", "z2"].map(|name| authored_line.literal(name)),
        [Some(-55), Some(-210), Some(-53), Some(52), Some(-220), Some(-53)]);
    let guard = border.iter().find(|ins| ins.offset == 0x1411).unwrap();
    assert_eq!(guard.mnemonic, "IFUB");
    assert_eq!(guard.operand("left").map(|value| (value.bank, value.value)), Some((3, 223)));
    assert_eq!((guard.literal("right"), guard.literal("op")), (Some(2), Some(10)));
    assert_eq!(border.iter().find(|ins| ins.offset == 0x1417).unwrap().literal("enabled"), Some(0));
    let crossing = script.disassemble(27, 2).unwrap();
    assert!(!crossing.iter().any(|ins| ins.operand("left").is_some_and(|value|
        (value.bank, value.value) == (3, 223))), "slot2 relies on the main's line gate; the host must retain it");
    for (offset, entity, slot) in [(0x1434, 31, 8), (0x1437, 34, 8), (0x143A, 30, 7)] {
        let request = crossing.iter().find(|ins| ins.offset == offset).unwrap();
        assert_eq!((request.literal("entity"), request.literal("script")), (Some(entity), Some(slot)));
    }
    let director = script.disassemble(17, 1).unwrap();
    assert_eq!(director.iter().map(|ins| ins.mnemonic).collect::<Vec<_>>(),
        ["PMVIE", "REQSW", "MOVIE", "AKAO", "MAPJUMP", "RET"]);
    assert_eq!(director[0].literal("movie"), Some(24));
    assert_eq!((director[1].literal("entity"), director[1].literal("script")), (Some(19), Some(3)));
    assert_eq!(["field", "x", "y", "triangle", "direction"].map(|name| director[4].literal(name)),
        [Some(146), Some(-2891), Some(3392), Some(26), Some(0)]);
    // The movie helper's delayed fade/audio cannot be discarded merely because MAPJUMP
    // follows MOVIE in the producer. A real movie boundary must keep these threads alive.
    let movie_helper = script.disassemble(19, 3).unwrap();
    assert_eq!((movie_helper[0].mnemonic, movie_helper[0].literal("frames")), ("WAIT", Some(1)));
    assert_eq!((movie_helper[1].mnemonic, movie_helper[1].literal("op")), ("AKAO2", Some(202)));
    assert_eq!((movie_helper[2].mnemonic, movie_helper[2].literal("type")), ("FADE", Some(1)));
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); movie playback is not asserted"]
fn departure_preserves_its_crossing_all_ten_windows_and_persistent_resources() {
    let f = field("tin_1");
    let story = departure_story();
    let def = with_trigger_position(&f, &train_departure_scene(), Pos::new(-1.0, -215.0, -53.0), 0.0, Some(43)).unwrap();
    let resources = Resources { gil: Some(240) };
    let outcome = run_to_end_with_resources_strict(&f, &def, &story, 1, resources, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::MapJump { field: 146 }, 10, 0));
    assert_eq!(outcome.jump, Some(SceneJump { field: 146, x: -2891, y: 3392, triangle: 26, direction: 0 }));
    assert!(outcome.is_complete() && outcome.unknown_persistent_reads.is_empty(), "{outcome:?}");
    assert!(outcome.writes.is_story_neutral() && outcome.party.is_empty() && outcome.resources.gil_ops.is_empty());
    assert_eq!((outcome.resources.gil_before, outcome.resources.gil_after), (Some(240), Some(240)));
    let mut after = story.clone();
    assert_eq!(outcome.writes.apply_extended_to(&mut after, &story), Some(false));
    assert_eq!(after.encode(), story.encode(), "scratch animation state does not become persistent state");

    let mut play = ScenePlay::with_resources(&f, &def, &story, 1, resources).unwrap();
    let (end, events, windows) = finish(&mut play);
    assert_eq!(end, End::MapJump { field: 146 });
    assert_eq!(windows, [(31, 2), (30, 3), (31, 4), (31, 5), (31, 7), (31, 8), (30, 9), (31, 10), (31, 11), (30, 12)]);
    assert!(events.contains(&Stage::Place { entity: 30, x: -1, y: -215, z: Some(-53), triangle: Some(43) }));
    assert!(events.iter().any(|event| matches!(event,
        Stage::Move { entity: 30, from: Some((-1, -215)), x: -55, y: -125, .. })), "Cloud starts from the accepted crossing");
    for (entity, x, y, triangle) in [(33, -95, -292, 50), (34, 110, -337, 55), (35, 85, -292, 46)] {
        assert!(events.contains(&Stage::Place { entity, x, y, z: Some(-53), triangle: Some(triangle) }),
            "actor {entity} is placed by the original moment51 main");
    }
    assert_eq!(play.off_cast(), 0);
    let snapshot = play.snapshot().unwrap();
    assert_eq!(snapshot.boundary, SceneBoundary::End(End::MapJump { field: 146 }));
    assert!(snapshot.writes.is_story_neutral() && snapshot.unknown_persistent_reads.is_empty());
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); station arrival is not registered"]
fn station_arrival_needs_no_inherited_scratch_and_returns_cloud_to_the_authored_floor_at_63() {
    let f = field("mds7st3");
    let def = sector7_station_scene();
    assert_eq!(def.setup, (0..23).map(|entity| ScriptRef::new(entity, 0)).collect::<Vec<_>>());
    let story = StoryState::at(51).into_extended().unwrap();
    assert_eq!(story.known_byte(3, 223), None);
    let resources = Resources { gil: Some(240) };
    let outcome = run_to_end_with_resources_strict(&f, &def, &story, 1, resources, &[]).unwrap();
    assert_eq!((outcome.end, outcome.windows, outcome.questions, outcome.writes.moment), (End::Finished, 1, 0, Some(63)));
    assert!(outcome.is_complete() && outcome.unknown_persistent_reads.is_empty(), "{outcome:?}");
    assert!(outcome.writes.persistent.is_empty() && outcome.writes.tracked.is_empty());
    assert!(outcome.party.is_empty() && outcome.resources.gil_ops.is_empty());
    assert_eq!((outcome.resources.gil_before, outcome.resources.gil_after), (Some(240), Some(240)));
    let mut after = story.clone();
    assert_eq!(outcome.writes.apply_extended_to(&mut after, &story), Some(true));
    assert_eq!((after.moment, after.known_byte(3, 223)), (63, None), "the arrival invents no train flags");

    let mut play = ScenePlay::with_resources(&f, &def, &story, 1, resources).unwrap();
    let (end, events, windows) = finish(&mut play);
    assert_eq!((end, windows), (End::Finished, vec![(12, 1)]));
    assert!(events.contains(&Stage::Background(BackgroundOp::Clear { group: 1 })));
    assert!(events.iter().any(|event| matches!(event, Stage::Move {
        entity: 11, x: -2783, y: 3102, z: Some(101), triangle: Some(52), frames: 19, how: MoveHow::Jump, ..
    })), "Cloud's authored JUMP has17 motion steps plus completion and script updates");
    let final_move = events.iter().rev().find_map(|event| match event {
        Stage::Move { entity: 11, x, y, z, .. } => Some((*x, *y, *z)), _ => None,
    });
    assert_eq!(final_move, Some((-2738, 3017, Some(101))));
    let ground = Ground::new(f.walkmesh().unwrap());
    assert_eq!(ground.triangle_at(-2738, 3017), Some(9));
    assert_eq!(ground.height(-2738, 3017, Some(9), Some(101)), Some(101.0));
    let heading = events.iter().rev().find_map(|event| match event {
        Stage::Face { entity: 11, heading, .. } => Some(*heading), _ => None,
    }).unwrap();
    assert!((heading - fmv_ff7::field_direction_to_facing(16)).abs() < 1e-6);
    assert_eq!(play.off_cast(), 0);

    let script = FieldScript::from_field(&f).unwrap();
    let cloud = script.disassemble(11, 3).unwrap();
    assert_eq!(cloud.iter().find(|ins| ins.offset == 0x096F).unwrap().literal("direction"), Some(16));
    let control_tail: Vec<_> = cloud.iter().filter(|ins| ins.offset >= 0x0978).collect();
    assert_eq!(control_tail.iter().map(|ins| ins.mnemonic).collect::<Vec<_>>(),
        ["MOVE", "REQSW", "SETWORD", "IDLCK", "UC", "MENU2", "RET"]);
    assert_eq!(control_tail[2].literal("value"), Some(63));
    assert_eq!((control_tail[3].literal("triangle"), control_tail[3].literal("locked")), (Some(64), Some(1)));
    assert_eq!((control_tail[4].literal("disabled"), control_tail[5].literal("disabled")), (Some(0), Some(0)));
    // This pins the original lock/control contract; scene completion alone does not implement
    // the dynamic walkmesh lock or prove the port releases control on the original frame.
}
