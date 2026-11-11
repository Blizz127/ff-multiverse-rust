//! Owned original train talk/lesson-return evidence. These definitions remain unregistered.
//! Reads FMV_FF7_DATA/field/flevel.lgp; records only numeric script/state observations.
//! Run with `cargo test -p fmv-ff7 --test train_lesson_real -- --ignored --nocapture`.

mod script_common;

use fmv_core::Pos;
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::{run_talk_exact_with_scratch, End, ExactError, ExactRun};
use fmv_ff7::script::scene::{
    jessie_train_talk_scene, run_to_end_with_resources_strict, scenes, train_map_return_scene,
    with_trigger_position, Limits, Outcome, SceneBoundary, SceneDef, SceneError, SceneJump, ScenePlay,
    SceneResources, SceneSnapshot, SceneVars, Stage, Step, Then, Vm, WindowAck,
    JESSIE_TRAIN_TALK_SCENE_ID, TRAIN_DEPARTURE_SCENE_ID,
    TRAIN_MAP_RETURN_SCENE_ID, TRAIN_MAP_SCENE_ID,
};
use fmv_ff7::script::{FieldScratch, FieldScratchError, FieldScript};
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

/// Replace only the preview's invented visit latch with the checked caller's complete image.
/// Every original initializer, actor, ambient root and talk instruction remains unchanged.
fn checked_talk(field: &FieldFile) -> SceneDef {
    let mut def = talk(field, false);
    assert_eq!(def.given.len(), 1);
    assert_eq!((def.given[0].bank, def.given[0].address, def.given[0].value), (5, 31, 0));
    def.given.clear();
    def
}

/// An explicit entry clear followed by this definition's actual original initializer roots.
/// Normal execution permits their assignments; existing-visit quiet setup must not invent them.
/// This derives the reviewed initialization image, not continuous ambient/model/RNG history.
fn initialized_train_scratch(field: &FieldFile, def: &SceneDef, state: &StoryState) -> FieldScratch {
    let script = FieldScript::from_field(field).unwrap();
    let clear = FieldScratch::cleared_for_entry();
    let vars = SceneVars::with_scratch(state.clone(), &def.given, clear.clone()).unwrap();
    let mut vm = Vm::with_resources(vars, 1, Limits::default(), RESOURCES);
    vm.begin(def.setup.iter().copied().map(Step::Await).collect());
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Finished, .. }),
        "the original initializer roots have no dialogue, transfer or battle");
    let stats = vm.stats();
    assert!(!stats.setup_incomplete && stats.unknown.is_empty() && stats.unresolved == 0
        && stats.dropped_requests == 0, "{stats:?}");
    assert!(vm.vars().unknown_persistent_reads().is_empty());
    assert_eq!(vm.writes().iter().map(|(key, value)| (*key, *value)).collect::<Vec<_>>(),
        [((5, 0), 0), ((5, 4), 0), ((5, 6), 10)], "the initializer roots write only these scratch bytes");
    assert!(vm.party_ops().is_empty());
    let effect = vm.scratch_outcome().unwrap().unwrap();
    assert_eq!(effect.before(), &clear);
    assert_eq!(effect.assigned_masks().iter().enumerate().filter(|(_, mask)| **mask != 0)
        .map(|(address, &mask)| (address, mask)).collect::<Vec<_>>(), [(0, 255), (4, 255), (6, 255)]);
    assert_eq!((effect.after().read_u8(5, 0), effect.after().read_u8(5, 4), effect.after().read_u8(5, 6)),
        (Ok(0), Ok(0), Ok(10)), "observed authored assignments, never input seeds");
    assert_eq!(effect.after().read_u8(5, 31), Ok(0), "entry cleared byte31; no initializer assigned it");
    effect.after().clone()
}

fn apply_exact_story(state: &StoryState, result: &ExactRun) -> StoryState {
    assert!(result.native.is_none() && result.party.is_empty() && result.battle.is_none());
    let mut after = state.clone();
    for write in &result.writes {
        assert_eq!((write.bank, write.address), (1, 4), "only the authored affection assignment is persistent");
        after.write_masked(write.bank, write.address, write.value, write.mask).unwrap();
    }
    after
}

fn finish_checked_talk(mut play: ScenePlay, answers: &[u8]) -> (SceneSnapshot, Vec<u16>) {
    let mut answers = answers.iter().copied();
    let mut ack = None;
    let mut dialogs = Vec::new();
    loop {
        match play.next_strict(ack.take()).unwrap().then {
            Then::Window(_) => {
                let Some(SceneBoundary::Window(checkpoint)) = play.boundary() else { panic!("window boundary") };
                dialogs.push(checkpoint.dialog.unwrap());
                assert!(dialogs.len() <= 2, "a later Jessie talk has at most two windows");
                let answer = checkpoint.choices.map(|_| answers.next().expect("an explicit answer"));
                ack = Some(WindowAck { checkpoint, answer });
            }
            Then::End { how, .. } => { assert_eq!(how, End::Finished); break; }
        }
    }
    assert_eq!(answers.next(), None, "all answers were consumed");
    let snapshot = play.snapshot().unwrap();
    assert!(snapshot.unknown_persistent_reads.is_empty() && snapshot.party.is_empty());
    for stats in [&snapshot.setup_stats, &snapshot.stats] {
        assert!(!stats.setup_incomplete && stats.unknown.is_empty() && stats.unresolved == 0
            && stats.dropped_requests == 0, "{stats:?}");
    }
    assert_eq!((snapshot.resources.gil_before, snapshot.resources.gil_after), (Some(240), Some(240)));
    assert!(snapshot.resources.gil_ops.is_empty() && snapshot.jump.is_none());
    assert!(snapshot.writes.untracked.keys().all(|(bank, _)| !matches!(bank, 5 | 6)),
        "checked scratch must not leak into persistent/legacy write summaries");
    assert!(snapshot.scratch.as_ref().unwrap().is_ok());
    (snapshot, dialogs)
}

#[test]
#[ignore = "needs original FF7 data (FMV_FF7_DATA); checked scratch remains interpreter-only"]
fn owned_jessie_checked_talk_retains_its_latch_until_an_explicit_new_entry() {
    let field = train().expect("FMV_FF7_DATA must identify the owned original field archive");
    let script = FieldScript::from_field(&field).unwrap();
    let def = checked_talk(&field);
    for affection in [0u8, 2, 30] {
        for answer in [2, 3] {
            let mut original = story(false, true);
            original.write_byte(1, 4, affection).unwrap();
            let before = original.encode();
            let entry = initialized_train_scratch(&field, &def, &original);
            assert_eq!(run_talk_exact_with_scratch(&script, 34, &original, &entry, 1, &[]),
                Err(ExactError::MissingAnswer), "the unlatched branch requires its authored choice");
            let first = run_talk_exact_with_scratch(&script, 34, &original, &entry, 1, &[answer]).unwrap();
            let scratch = first.scratch.as_ref().unwrap();
            assert_eq!(scratch.before(), &entry);
            assert_eq!(scratch.after().read_u8(5, 31), Ok(1));
            assert_eq!(scratch.assigned_masks()[31], 255);
            let after = apply_exact_story(&original, &first);
            let wanted = if answer == 3 { affection.saturating_sub(3) } else { affection };
            let mut expected = original.clone();
            expected.write_byte(1, 4, wanted).unwrap();
            assert_eq!(after.encode(), expected.encode());
            assert_eq!(original.encode(), before);
            assert_eq!(entry.read_u8(5, 31), Ok(0), "the caller's input remains unchanged");

            // Simulate restoring the exact visit image, preserving values AND knowledge.
            let retained = FieldScratch::from_parts(*scratch.after().values(), *scratch.after().known_masks()).unwrap();
            let repeat = run_talk_exact_with_scratch(&script, 34, &after, &retained, 1, &[]).unwrap();
            assert!(repeat.writes.is_empty());
            let repeat_scratch = repeat.scratch.as_ref().unwrap();
            assert_eq!(repeat_scratch.before(), &retained);
            assert_eq!(repeat_scratch.after(), &retained);
            assert!(!repeat_scratch.any_change());
            assert_eq!(repeat_scratch.assigned_masks()[31], 0, "the repeated talk only reads the latch");

            // A distinct real entry may clear the latch. Run the original setup again;
            // do not rewrite byte31 or manufacture byte6=10 as a test input.
            let next_entry = initialized_train_scratch(&field, &def, &after);
            assert_ne!(next_entry, retained);
            let again = run_talk_exact_with_scratch(&script, 34, &after, &next_entry, 1, &[answer]).unwrap();
            let next_story = apply_exact_story(&after, &again);
            expected.write_byte(1, 4, if answer == 3 { wanted.saturating_sub(3) } else { wanted }).unwrap();
            assert_eq!(next_story.encode(), expected.encode());
            assert_eq!(again.scratch.as_ref().unwrap().after().read_u8(5, 31), Ok(1));
            assert_eq!(retained.read_u8(5, 31), Ok(1), "new entry never mutates the old visit");
        }
    }
}

#[test]
#[ignore = "needs original FF7 data (FMV_FF7_DATA); checked scratch remains interpreter-only"]
fn owned_jessie_checked_talk_refuses_unknown_latch_without_a_zero_fallback() {
    let field = train().expect("FMV_FF7_DATA must identify the owned original field archive");
    let script = FieldScript::from_field(&field).unwrap();
    let mut original = story(false, true);
    original.write_byte(1, 4, 30).unwrap();
    let entry = initialized_train_scratch(&field, &checked_talk(&field), &original);
    for known in [0u8, 254] {
        let mut masks = *entry.known_masks();
        masks[31] = known;
        let unknown = FieldScratch::from_parts(*entry.values(), masks).unwrap();
        let before = unknown.clone();
        let story_before = original.encode();
        for answers in [&[][..], &[2][..], &[3][..]] {
            assert_eq!(run_talk_exact_with_scratch(&script, 34, &original, &unknown, 1, answers),
                Err(ExactError::Scratch(FieldScratchError::Unknown { address: 31, missing_mask: !known })));
            assert_eq!(unknown, before);
            assert_eq!(original.encode(), story_before);
        }
    }
}

#[test]
#[ignore = "needs original FF7 data (FMV_FF7_DATA); checked scratch remains interpreter-only"]
fn owned_jessie_full_scene_reuses_authored_setup_and_the_checked_visit_afterimage() {
    let field = train().expect("FMV_FF7_DATA must identify the owned original field archive");
    let def = checked_talk(&field);
    for bit0 in [false, true] {
        for answer in [2u8, 3] {
            let mut original = story(bit0, true);
            original.write_byte(1, 4, 30).unwrap();
            assert!(matches!(ScenePlay::with_scratch(&field, &talk(&field, false), &original, 1,
                RESOURCES, &FieldScratch::cleared_for_entry()), Err(SceneError::ScratchGiven { bank: 5, address: 31 })));
            assert!(matches!(ScenePlay::with_scratch(&field, &def, &original, 1,
                RESOURCES, &FieldScratch::cleared_for_entry()), Err(SceneError::ScratchSetup)),
                "existing-visit reconstruction cannot silently initialize 5:6 to 10");
            assert!(matches!(ScenePlay::with_scratch(&field, &def, &original, 1,
                RESOURCES, &FieldScratch::unknown()), Err(SceneError::ScratchSetup)),
                "reconstruction cannot silently promote unknown initializer bytes");
            let entry = initialized_train_scratch(&field, &def, &original);
            let (first, dialogs) = finish_checked_talk(
                ScenePlay::with_scratch(&field, &def, &original, 1, RESOURCES, &entry).unwrap(), &[answer]);
            assert_eq!(dialogs, [17, if answer == 2 { 18 } else { 19 }]);
            let scratch = first.scratch.as_ref().unwrap().as_ref().unwrap();
            assert_eq!(scratch.before(), &entry);
            assert_eq!(scratch.after().read_u8(5, 31), Ok(1));
            let mut after = original.clone();
            first.writes.apply_extended_to(&mut after, &original).unwrap();
            let mut expected = original.clone();
            expected.write_byte(1, 4, if answer == 3 { 27 } else { 30 }).unwrap();
            assert_eq!(after.encode(), expected.encode());
            let (repeat, dialogs) = finish_checked_talk(
                ScenePlay::with_scratch(&field, &def, &after, 1, RESOURCES, scratch.after()).unwrap(), &[]);
            assert_eq!(dialogs, [20]);
            assert!(repeat.writes.is_story_neutral());
            let repeat_scratch = repeat.scratch.as_ref().unwrap().as_ref().unwrap();
            assert_eq!(repeat_scratch.before(), scratch.after());
            assert_eq!(repeat_scratch.after().read_u8(5, 31), Ok(1));
            assert_eq!(repeat_scratch.assigned_masks()[31], 0);
            // Background mains may update their own scratch bytes; their scheduler, random
            // history and model state are not continuous across these separate ScenePlayers.
            for address in [0, 4, 6] {
                assert_eq!(repeat_scratch.after().read_u8(5, address), entry.read_u8(5, address));
            }
        }
    }
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
