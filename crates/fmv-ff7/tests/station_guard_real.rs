//! The owned station's guard approach, separate from the movie opening. The asynchronous
//! victory return remains an unregistered survey until its field lifecycle can be preserved.
//!
//! Real-data checks are ignored unless explicitly requested with `FMV_FF7_DATA`. They only
//! read the original field archive and report numeric script/geometry evidence, never text.
//! MOVE durations remain the interpreter's estimates; these checks pin authored paths,
//! request ordering and boundaries, not an invented original-engine elapsed time.

mod script_common;

use fmv_core::Pos;
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{
    run_to_end_with_resources, scenes, station_guard_scene, station_guard_victory_scene,
    with_trigger_position, Resources, SceneBoundary, SceneDef, ScenePlay, Stage, Then,
};
use fmv_ff7::script::{FieldScript, Instruction};
use fmv_ff7::story::StoryState;
use script_common::{data_dir, flevel_path, load_field};

fn field() -> Option<FieldFile> {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping station guard data checks");
        return None;
    };
    let mut archive = LgpArchive::open_path(flevel_path(&dir)).unwrap();
    Some(load_field(&mut archive, "md1stin").unwrap())
}

fn instruction(instructions: &[Instruction], offset: usize, mnemonic: &str) -> Instruction {
    let instruction = instructions.iter().find(|instruction| instruction.offset == offset)
        .unwrap_or_else(|| panic!("missing authored {mnemonic} at {offset}"));
    assert_eq!(instruction.mnemonic, mnemonic, "instruction at {offset}");
    instruction.clone()
}

fn variable(instruction: &Instruction, name: &str) -> (u8, i32) {
    let operand = instruction.operand(name).expect("named variable operand");
    (operand.bank, operand.value)
}

/// An offline preview point derived from the actual floor. Production supplies its own
/// accepted crossing position; this helper does not assert that movement occurred.
fn at_triangle(field: &FieldFile, definition: &SceneDef, triangle: u16) -> SceneDef {
    let mesh = field.walkmesh().unwrap();
    let vertices = mesh.triangles[usize::from(triangle)];
    let x = (vertices.iter().map(|v| f32::from(v.x)).sum::<f32>() / 3.0).round();
    let y = (vertices.iter().map(|v| f32::from(v.y)).sum::<f32>() / 3.0).round();
    let z = vertices.iter().map(|v| f32::from(v.z)).sum::<f32>() / 3.0;
    assert!(fmv_ff7::script::triangle_contains_xy(&vertices, x as i32, y as i32));
    with_trigger_position(field, definition, Pos::new(x, y, z), 0.0, Some(u32::from(triangle))).unwrap()
}

fn stories() -> [StoryState; 2] {
    [StoryState::at(1).into_extended().unwrap(), StoryState::fresh_native_first_control()]
}

#[test]
fn only_the_exact_station_step_selects_the_prelude_and_the_victory_remains_unregistered() {
    let prelude = station_guard_scene();
    let victory = station_guard_victory_scene();
    assert_eq!(prelude.id, "ff7/md1stin/station-guards");
    assert_eq!(victory.id, "ff7/md1stin/station-guards-victory");
    let registered = scenes();
    assert_eq!(registered.iter().filter(|definition| definition.id == prelude.id).count(), 1);
    assert!(!registered.iter().any(|definition| definition.id == victory.id), "the asynchronous victory return is still a survey");
    let identities: std::collections::BTreeSet<_> = registered.iter().map(|definition| &definition.id).collect();
    assert_eq!(identities.len(), registered.len(), "every registered scene has one stable identity");
    for definition in [&prelude, &victory] {
        assert_eq!((definition.field.as_str(), definition.origin_actor()), ("md1stin", Some(1)));
        assert!(definition.requires_presentation_origin());
        assert_eq!(definition.presentation_player_actor(), Some(1));
        assert_eq!(definition.presents, Some(6));
        assert!(definition.applies_at(1));
        assert!(!definition.applies_at(0) && !definition.applies_at(6));
        assert!(!definition.starts_on_arrival(), "entering the station does not trigger the fight");
        assert!(definition.member(1).unwrap().start.is_none(), "the actual trigger supplies Cloud's origin");
        assert!(!definition.hides_participants(), "the station keeps the existing participant presentation");
    }
    assert_eq!(prelude.resume_after_battle, None);
    assert_eq!(victory.resume_after_battle, Some(300));
    for before in stories() {
        let mut after = before.clone();
        after.moment = 6;
        assert!(prelude.presents_step("md1stin", &before, &after));
        assert!(!victory.presents_step("md1stin", &before, &after));
        assert!(victory.follows_battle_step("md1stin", &before, &after, 300));
        assert!(!victory.follows_battle_step("md1stin", &before, &after, 303));
        assert!(!prelude.follows_battle_step("md1stin", &before, &after, 300));
        assert!(!prelude.presents_step("md1_1", &before, &after));
        assert!(!prelude.presents_step("md1stin", &after, &after));
        assert!(!prelude.offered_to(&before) && !victory.offered_to(&before));
        let mut opening = before.clone();
        opening.moment = 0;
        assert!(!prelude.presents_step("md1stin", &opening, &after), "the movie opening cannot force this encounter");
    }
    let mut legacy_v2 = StoryState::at(1);
    legacy_v2.set_bit(1, 225, 0, true);
    for before in [StoryState::at(1), legacy_v2] {
        assert!(!before.supports_extended());
        let mut after = before.clone();
        after.moment = 6;
        assert!(!prelude.presents_step("md1stin", &before, &after), "legacy progression retains its existing battle path");
        assert_eq!(prelude.prepare_story(&before).unwrap(), before, "presentation registration does not promote a save");
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_original_director_requires_triangle_twelve_and_moment_one_before_the_guard_requests() {
    let Some(field) = field() else { return };
    let script = FieldScript::from_field(&field).unwrap();
    let director = script.disassemble(0, 0).unwrap();
    let threshold = instruction(&director, 949, "SETWORD");
    assert_eq!((variable(&threshold, "var"), threshold.literal("value")), ((6, 9), Some(12)));
    assert_eq!(instruction(&director, 954, "BTLMD").literal("flags"), Some(32));
    let triangle = instruction(&director, 1187, "GETAI");
    assert_eq!((triangle.literal("entity"), variable(&triangle, "var_triangle")), (Some(1), (6, 4)));
    let on_floor = instruction(&director, 1191, "IFSW");
    assert_eq!((variable(&on_floor, "left"), variable(&on_floor, "right"), on_floor.literal("op")), ((6, 4), (6, 9), Some(0)));
    let at_moment = instruction(&director, 1199, "IFSW");
    assert_eq!((variable(&at_moment, "left"), at_moment.literal("right"), at_moment.literal("op")), ((2, 0), Some(1), Some(0)));
    let advance = instruction(&director, 1207, "SETWORD");
    assert_eq!((variable(&advance, "var"), advance.literal("value")), ((2, 0), Some(6)));
    for (offset, mnemonic) in [(1212, "UC"), (1214, "MENU2")] {
        assert_eq!(instruction(&director, offset, mnemonic).literal("disabled"), Some(1));
    }
    for (offset, mnemonic, entity) in [(1216, "REQ", 4), (1219, "REQEW", 5)] {
        let request = instruction(&director, offset, mnemonic);
        assert_eq!(["entity", "priority", "script"].map(|name| request.literal(name)), [Some(entity), Some(2), Some(3)]);
    }
    assert_eq!(instruction(&director, 1222, "BATTLE").literal("battle"), Some(300));
    let hide = instruction(&director, 1226, "REQ");
    assert_eq!(["entity", "script"].map(|name| hide.literal(name)), [Some(5), Some(4)]);
    for (offset, mnemonic) in [(1229, "UC"), (1231, "MENU2")] {
        assert_eq!(instruction(&director, offset, mnemonic).literal("disabled"), Some(0));
    }
    assert_eq!(instruction(&director, 1233, "JMPB").jump_target, Some(1187));
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn both_guards_start_hidden_at_the_authored_platform_and_cloud_keeps_the_accepted_origin() {
    let Some(field) = field() else { return };
    let script = FieldScript::from_field(&field).unwrap();
    let definition = at_triangle(&field, &station_guard_scene(), 12);
    let origin = definition.member(1).unwrap().start.unwrap();
    assert_eq!((origin.x, origin.y, origin.triangle), (3783, 28446, 12));
    for (entity, offset) in [(4, 1424), (5, 1472)] {
        let init = script.disassemble(entity, 0).unwrap();
        let place = instruction(&init, offset, "XYZI");
        assert_eq!(["x", "y", "z", "triangle"].map(|name| place.literal(name)), [Some(3634), Some(29284), Some(367), Some(36)]);
        let member = definition.member(entity as u8).unwrap();
        let fmv_ff7::script::scene::Look::Model { hrc, .. } = &member.look else { panic!("an original guard model") };
        assert_eq!(hrc, "bwab");
    }
    let play = ScenePlay::with_resources(&field, &definition, &stories()[1], 1, Resources { gil: Some(600) }).unwrap();
    let initial = play.initial_stage_events();
    assert!(initial.contains(&Stage::Place { entity: 1, x: origin.x, y: origin.y, z: Some(310), triangle: Some(12) }));
    for entity in [4, 5] {
        assert!(initial.contains(&Stage::Place { entity, x: 3634, y: 29284, z: Some(367), triangle: Some(36) }));
        assert!(initial.contains(&Stage::Show { entity, visible: false }));
    }
    assert_eq!(play.off_cast(), 0);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_approach_reaches_only_battle_300_and_changes_only_the_moment() {
    let Some(field) = field() else { return };
    let script = FieldScript::from_field(&field).unwrap();
    let definition = at_triangle(&field, &station_guard_scene(), 12);
    for (entity, speed, path) in [(4, 2432, [(3794, 29268), (3732, 28752)]), (5, 2560, [(3897, 29257), (3873, 28840)])] {
        let body = script.disassemble(entity, 3).unwrap();
        assert_eq!(body.iter().find(|ins| ins.mnemonic == "MSPED").unwrap().literal("speed"), Some(speed));
        let moves: Vec<_> = body.iter().filter(|ins| ins.mnemonic == "MOVE")
            .map(|ins| (ins.literal("x").unwrap(), ins.literal("y").unwrap())).collect();
        assert_eq!(moves, path);
    }
    for before in stories() {
        let resources = Resources { gil: Some(123) };
        let mut play = ScenePlay::with_resources(&field, &definition, &before, 1, resources).unwrap();
        let beat = play.next_strict(None).unwrap();
        assert!(matches!(beat.then, Then::End { how: End::Battle, .. }), "the approach contains no window");
        let Some(SceneBoundary::Battle(checkpoint)) = play.boundary() else { panic!("an exact battle boundary") };
        assert_eq!((checkpoint.entity, checkpoint.instruction_offset, checkpoint.formation, checkpoint.flags), (0, 1222, 300, Some(32)));
        for (entity, path) in [(4, [(3794, 29268), (3732, 28752)]), (5, [(3897, 29257), (3873, 28840)])] {
            let moves: Vec<_> = beat.stage.iter().filter_map(|event| match event.what {
                Stage::Move { entity: actor, x, y, .. } if actor == entity => Some((x, y)), _ => None,
            }).collect();
            assert_eq!(moves, path);
            assert!(beat.stage.iter().any(|event| event.what == Stage::Show { entity, visible: true }));
        }
        assert!(!beat.stage.iter().any(|event| event.what == Stage::Show { entity: 5, visible: false }), "the awaited guard remains until the victory tail");
        assert!(!beat.stage.iter().any(|event| matches!(event.what, Stage::Move { entity: 1, .. })), "the approach never moves the player");
        assert_eq!(play.off_cast(), 0);
        let mut expected = before.clone();
        expected.moment = 6;
        assert_eq!(play.current_story(), Some(expected.clone()));
        let outcome = run_to_end_with_resources(&field, &definition, &before, 1, resources, &[]).unwrap();
        assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::Battle, 0, 0));
        assert!(outcome.is_complete() && outcome.unknown_persistent_reads.is_empty(), "{outcome:?}");
        assert!(outcome.party.is_empty() && outcome.resources.gil_ops.is_empty() && outcome.jump.is_none());
        assert_eq!((outcome.resources.gil_before, outcome.resources.gil_after), (Some(123), Some(123)));
        let mut after = before.clone();
        assert_eq!(outcome.writes.apply_extended_to(&mut after, &before), Some(true));
        assert_eq!(after, expected, "every unrelated persistent value and known mask survives");
        for mnemonic in ["PMVIE", "MOVIE", "MVIEF"] {
            assert!(!outcome.stats.ignored.contains_key(mnemonic), "the opening movie is outside this segment");
        }
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_unregistered_victory_source_releases_control_without_awaiting_the_guard_tails() {
    let Some(field) = field() else { return };
    let survey = station_guard_victory_scene();
    assert!(!scenes().iter().any(|definition| definition.id == survey.id));
    let script = FieldScript::from_field(&field).unwrap();
    let director = script.disassemble(0, 0).unwrap();
    let returned: Vec<_> = director.iter().filter(|instruction| (1226..=1233).contains(&instruction.offset)).collect();
    assert_eq!(returned.iter().map(|instruction| (instruction.offset, instruction.mnemonic)).collect::<Vec<_>>(),
        [(1226, "REQ"), (1229, "UC"), (1231, "MENU2"), (1233, "JMPB")]);
    assert_eq!(["entity", "priority", "script"].map(|name| returned[0].literal(name)), [Some(5), Some(2), Some(4)]);
    assert_eq!(returned[1].literal("disabled"), Some(0));
    assert_eq!(returned[2].literal("disabled"), Some(0));
    assert_eq!(returned[3].jump_target, Some(1187));
    assert!(!returned.iter().any(|instruction| matches!(instruction.mnemonic, "REQEW" | "REQSW" | "WAIT")),
        "neither the first guard's outstanding approach nor the second guard's hide is awaited before control returns");
    let second_guard = script.disassemble(5, 4).unwrap();
    assert_eq!(instruction(&second_guard, 1516, "VISI").literal("visible"), Some(0));
    // The director keeps polling after releasing controls. A presentation cannot replace
    // that live-field lifecycle with either a synthetic stop or an added wait for all actors.
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn a_different_valid_floor_never_forces_the_encounter() {
    let Some(field) = field() else { return };
    let definition = at_triangle(&field, &station_guard_scene(), 25);
    for before in stories() {
        let outcome = run_to_end_with_resources(&field, &definition, &before, 1, Resources { gil: Some(123) }, &[]).unwrap();
        assert_ne!(outcome.end, End::Battle);
        assert_eq!(outcome.windows, 0);
        assert!(outcome.writes.is_story_neutral(), "the native trigger did not fire");
        assert!(outcome.unknown_persistent_reads.is_empty());
        assert_eq!((outcome.stats.unresolved, outcome.stats.dropped_requests), (0, 0));
    }
}
