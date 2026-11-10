//! Owned original train talk/lesson-return evidence. These definitions remain unregistered.
//! Reads FMV_FF7_DATA/field/flevel.lgp; records only numeric script/state observations.
//! Run with `cargo test -p fmv-ff7 --test train_lesson_real -- --ignored --nocapture`.

mod script_common;

use fmv_core::Pos;
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{
    jessie_train_talk_scene, run_to_end_with_resources_strict, scenes, train_map_return_scene,
    with_trigger_position, Outcome, SceneBoundary, SceneDef, SceneJump, ScenePlay, SceneResources,
    Stage, Then, WindowAck, JESSIE_TRAIN_TALK_SCENE_ID, TRAIN_DEPARTURE_SCENE_ID,
    TRAIN_MAP_RETURN_SCENE_ID, TRAIN_MAP_SCENE_ID,
};
use fmv_ff7::script::FieldScript;
use fmv_ff7::story::StoryState;
use script_common::{data_dir, flevel_path, load_field};

const RESOURCES: SceneResources = SceneResources { gil: Some(240) };

fn train() -> Option<FieldFile> {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return None;
    };
    let mut archive = LgpArchive::open_path(flevel_path(&dir)).unwrap();
    Some(load_field(&mut archive, "tin_1").unwrap())
}

fn story(bit0: bool, bit2: bool) -> StoryState {
    let mut story = StoryState::at(51).into_extended().unwrap();
    // Independent authored inputs: Biggs's facing flag and Jessie's lesson flag only.
    story.write_masked(3, 223, u8::from(bit0), 1).unwrap();
    story.write_masked(3, 223, u8::from(bit2) << 2, 1 << 2).unwrap();
    story
}

fn talk(field: &FieldFile, talked_this_visit: bool) -> SceneDef {
    with_trigger_position(field, &jessie_train_talk_scene(talked_this_visit),
        Pos::new(72.0, -319.0, -53.0), fmv_ff7::field_direction_to_facing(64), Some(47)).unwrap()
}

fn run(field: &FieldFile, def: &SceneDef, story: &StoryState, answers: &[usize]) -> Outcome {
    run_to_end_with_resources_strict(field, def, story, 1, RESOURCES, answers).unwrap()
}

fn complete_without_resources_changed(outcome: &Outcome) {
    assert!(outcome.is_complete() && outcome.unknown_persistent_reads.is_empty(), "{outcome:?}");
    assert!(outcome.party.is_empty() && outcome.resources.gil_ops.is_empty());
    assert_eq!((outcome.resources.gil_before, outcome.resources.gil_after), (Some(240), Some(240)));
}

#[test]
fn train_lesson_definitions_are_not_production_registrations() {
    let registered = scenes();
    for id in [JESSIE_TRAIN_TALK_SCENE_ID, TRAIN_MAP_SCENE_ID, TRAIN_MAP_RETURN_SCENE_ID, TRAIN_DEPARTURE_SCENE_ID] {
        assert!(!registered.iter().any(|def| def.id == id), "{id}: interpreter evidence does not enable the route");
    }
    let close_up = fmv_ff7::script::scene::train_map_scene();
    assert!(close_up.hides_participants(), "the authored close-ups replace participant bodies");
    assert!(!close_up.requires_continuation() && !close_up.requires_landing(),
        "presentation policy does not grant durable route capabilities");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); train lesson is not enabled"]
fn jessies_first_talk_sets_only_its_known_bit_and_transfers_after_two_windows() {
    let Some(field) = train() else { return };
    let def = talk(&field, false);
    assert_eq!((def.origin_actor(), def.trigger.entity, def.trigger.slot, def.trigger.talk_radius), (Some(30), 34, 1, 80));
    for bit0 in [false, true] {
        let mut original = story(bit0, false);
        original.write_masked(3, 223, 1 << 5, 1 << 5).unwrap();
        let before = original.encode();
        let outcome = run(&field, &def, &original, &[]);
        complete_without_resources_changed(&outcome);
        assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::MapJump { field: 143 }, 2, 0));
        assert_eq!(outcome.jump, Some(SceneJump { field: 143, x: 0, y: 0, triangle: 0, direction: 28 }));
        assert_eq!(outcome.writes.moment, None);
        assert_eq!(outcome.writes.persistent.iter().map(|(key, value)| (*key, *value)).collect::<Vec<_>>(),
            [((3, 223), (4, 4))]);
        let mut after = original.clone();
        assert_eq!(outcome.writes.apply_extended_to(&mut after, &original), Some(true));
        let mut expected = original.clone();
        expected.write_masked(3, 223, 4, 4).unwrap();
        assert_eq!(after.encode(), expected.encode());
        assert_eq!(original.encode(), before, "the read-only run never changes its input");
        assert_eq!(after.known_bit(3, 223, 1), None, "an unrelated unknown bit remains unknown");
        assert_eq!(after.known_bit(3, 223, 5), Some(true));
        let play = ScenePlay::with_resources(&field, &def, &original, 1, RESOURCES).unwrap();
        assert!(play.initial_stage_events().contains(&Stage::Place {
            entity: 30, x: 72, y: -319, z: Some(-53), triangle: Some(47),
        }), "the talk starts Cloud at the accepted player position");
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); train lesson is not enabled"]
fn jessies_optional_answers_change_affection_once_per_retained_train_visit() {
    let Some(field) = train() else { return };
    let first = talk(&field, false);
    let later = talk(&field, true);
    for bit0 in [false, true] {
        for affection in [0u8, 2, 30] {
            for answer in [2, 3] {
                let mut original = story(bit0, true);
                original.write_byte(1, 4, affection).unwrap();
                let outcome = run(&field, &first, &original, &[answer]);
                complete_without_resources_changed(&outcome);
                assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::Finished, 2, 1));
                assert!(outcome.jump.is_none() && outcome.writes.moment.is_none());
                assert_eq!(outcome.writes.untracked.get(&(5, 31)), Some(&1), "both answers set the visit latch");
                let mut after = original.clone();
                outcome.writes.apply_extended_to(&mut after, &original).unwrap();
                let wanted = if answer == 3 { affection.saturating_sub(3) } else { affection };
                let mut expected = original.clone();
                expected.write_byte(1, 4, wanted).unwrap();
                assert_eq!(after.encode(), expected.encode(), "answer {answer}, affection {affection}");
                // The same visit now supplies its retained local byte, instead of constructing
                // a fresh scratch bank and allowing the economic choice again.
                let repeat = run(&field, &later, &after, &[]);
                complete_without_resources_changed(&repeat);
                assert_eq!((repeat.end, repeat.windows, repeat.questions), (End::Finished, 1, 0));
                assert!(repeat.writes.is_story_neutral() && repeat.jump.is_none());
                assert_eq!(repeat.writes.untracked.get(&(5, 31)), None, "the already-set latch is only read");
                let mut unchanged = after.clone();
                assert_eq!(repeat.writes.apply_extended_to(&mut unchanged, &after), Some(false));
                assert_eq!(unchanged.encode(), after.encode());
            }
        }
    }
    let original = story(false, true);
    let identity = |def| ScenePlay::with_resources(&field, def, &original, 1, RESOURCES).unwrap().replay_identity();
    assert_ne!(identity(&first), identity(&later), "the replay binds the retained visit input");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); train lesson is not enabled"]
fn unknown_lesson_or_affection_inputs_are_reported_without_inventing_defaults() {
    let Some(field) = train() else { return };
    let def = talk(&field, false);
    let mut unknown_lesson = StoryState::at(51).into_extended().unwrap();
    unknown_lesson.write_masked(3, 223, 0, 1).unwrap(); // Biggs's independent flag is known.
    let before = unknown_lesson.encode();
    let outcome = run(&field, &def, &unknown_lesson, &[]);
    assert!(outcome.unknown_persistent_reads.contains(&(3, 223)), "unknown bit2 is not a known-clear lesson flag");
    assert_eq!(unknown_lesson.encode(), before);
    let unknown_affection = story(false, true);
    let outcome = run(&field, &def, &unknown_affection, &[3]);
    assert!(outcome.unknown_persistent_reads.contains(&(1, 4)), "the deduction needs an authoritative old value");
    assert_eq!(unknown_affection.known_byte(1, 4), None);
    let other_answer = run(&field, &def, &unknown_affection, &[2]);
    assert!(other_answer.unknown_persistent_reads.is_empty(), "answer2 never reads affection");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA); train lesson is not enabled"]
fn the_lesson_return_shares_its_local_latch_and_keeps_clouds_final_turn() {
    let Some(field) = train() else { return };
    let def = train_map_return_scene();
    for bit0 in [false, true] {
        let original = story(bit0, true);
        let outcome = run(&field, &def, &original, &[]);
        complete_without_resources_changed(&outcome);
        assert_eq!((outcome.end, outcome.windows, outcome.questions), (End::Finished, 3, 0));
        assert!(outcome.writes.is_story_neutral() && outcome.jump.is_none());
        assert_eq!(outcome.writes.untracked.get(&(5, 30)), Some(&1), "Jessie releases director19's polling loop in the same VM");
        assert_eq!(outcome.writes.untracked.get(&(5, 31)), None, "the optional later talk has not happened");
        let mut unchanged = original.clone();
        assert_eq!(outcome.writes.apply_extended_to(&mut unchanged, &original), Some(false));
        assert_eq!(unchanged.encode(), original.encode());

        let mut play = ScenePlay::with_resources(&field, &def, &original, 1, RESOURCES).unwrap();
        let mut events = play.initial_stage_events().to_vec();
        let mut ack = None;
        let mut marks = Vec::new();
        loop {
            let beat = play.next_strict(ack.take()).unwrap();
            events.extend(beat.stage.into_iter().map(|event| event.what));
            match beat.then {
                Then::Window(_) => {
                    let Some(SceneBoundary::Window(checkpoint)) = play.boundary() else { panic!("window boundary") };
                    marks.push((checkpoint.entity, checkpoint.instruction_offset, checkpoint.dialog));
                    assert!(marks.len() <= 3, "the return must not replay the arrival or later talk");
                    ack = Some(WindowAck { checkpoint, answer: None });
                }
                Then::End { how, .. } => { assert_eq!(how, End::Finished); break; }
            }
        }
        assert_eq!(marks, [(34, Some(9404), Some(21)), (34, Some(9426), Some(22)), (34, Some(9447), Some(23))]);
        assert_eq!(play.off_cast(), 0);
        let position = events.iter().rev().find(|event| matches!(event,
            Stage::Place { entity: 30, .. } | Stage::Move { entity: 30, .. }));
        assert_eq!(position, Some(&Stage::Place { entity: 30, x: 72, y: -319, z: Some(-53), triangle: Some(47) }));
        let heading = events.iter().rev().find_map(|event| match event {
            Stage::Face { entity: 30, heading, .. } => Some(*heading), _ => None,
        }).unwrap();
        let toward_jessie = (-18.0f32).atan2(38.0);
        assert!((heading - toward_jessie).abs() < 0.0001, "Cloud's final TURA must face Jessie: {heading}");
        assert!((heading - fmv_ff7::field_direction_to_facing(64)).abs() > 0.4,
            "the initial DIR64 is not the control-return facing");
        assert!(play.snapshot().unwrap().unknown_persistent_reads.is_empty());
    }
    // Control release belongs to the requested final Cloud slot, not merely the director's RET.
    let script = FieldScript::from_field(&field).unwrap();
    let cloud_end = script.disassemble(30, 6).unwrap();
    for (offset, mnemonic) in [(5480, "UC"), (5482, "MENU2")] {
        let instruction = cloud_end.iter().find(|ins| ins.offset == offset).unwrap();
        assert_eq!((instruction.mnemonic, instruction.literal("disabled")), (mnemonic, Some(0)));
    }
}
