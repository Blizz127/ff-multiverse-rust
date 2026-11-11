//! Checked field-scratch scene contracts over invented scripts, separate from a field visit's runtime.

use super::*;
use crate::field::FieldFile;
use crate::script::dialogue::End;
use crate::script::synth::{dialog, SynthScript};
use crate::script::{FieldScratch, FieldScratchError};
use crate::story::StoryState;
use crate::testutil::FieldSpec;

fn set_byte(bank: u8, address: u8, value: u8) -> Vec<u8> { vec![0x80, bank << 4, address, value] }
fn bit(bank: u8, address: u8, index: u8, on: bool) -> Vec<u8> { vec![if on { 0x82 } else { 0x83 }, bank << 4, address, index] }
fn message() -> Vec<u8> { vec![0x40, 0, 0] }
fn battle() -> Vec<u8> { vec![0x70, 0, 72, 1] }

fn if_byte(bank: u8, address: u8, value: u8, body: Vec<u8>) -> Vec<u8> {
    let mut code = vec![0x14, bank << 4, address, value, 0, u8::try_from(body.len() + 1).unwrap()];
    code.extend(body);
    code
}

fn source(setup: Vec<Vec<u8>>, body: Vec<Vec<u8>>) -> (FieldFile, Vec<u8>) {
    let section = SynthScript { dialogs: vec![dialog("A scratch scene.")], ..Default::default() }
        .entity("actor", vec![(0, [setup.concat(), vec![0]].concat()), (1, [body.concat(), vec![0]].concat())]).build();
    let field = FieldFile::parse(FieldSpec { overrides: vec![(0, section.clone())], ..Default::default() }.build()).unwrap();
    (field, section)
}

fn definition() -> SceneDef {
    SceneDef {
        id: "test/scratch-outcome".into(), game: "ff7".into(), field: "fixture".into(),
        trigger: Trigger { entity: 0, name: "actor".into(), slot: 1, talk_radius: 80, moments: 0..=u16::MAX },
        setup: vec![], given: vec![], root: vec![Step::Await(ScriptRef::new(0, 1))], cast: vec![],
        speakers: vec![], witness: vec![], arrival: Spot { x: 0, y: 0, triangle: 0, direction: 0 },
        presents: None, presents_bit: None, resume_after_battle: None,
    }
}

fn story() -> StoryState {
    let mut state = StoryState::at(69).into_extended().unwrap();
    state.write_byte(1, 4, 10).unwrap();
    state
}

fn resources() -> Resources { Resources { gil: Some(100) } }

fn checked(field: &FieldFile, def: &SceneDef, story: &StoryState, scratch: &FieldScratch) -> Outcome {
    run_to_end_with_scratch_strict(field, def, story, 123, resources(), scratch, &[]).unwrap()
}

fn applied(outcome: &Outcome, before: &StoryState) -> StoryState {
    assert!(outcome.is_complete(), "{outcome:?}");
    let mut after = before.clone();
    outcome.writes.apply_extended_to(&mut after, before).unwrap();
    after
}

#[test]
fn an_explicit_scratch_image_controls_the_scene_without_changing_legacy_constructor_behavior() {
    let before = story();
    let (field, _) = source(vec![], vec![if_byte(5, 31, 0, set_byte(1, 4, 7))]);
    let def = definition();
    let mut scratch = FieldScratch::cleared_for_entry();
    scratch.write_u8(6, 31, 1).unwrap();
    let outcome = checked(&field, &def, &before, &scratch);
    assert_eq!(applied(&outcome, &before), before, "the checked scene reads the retained latch");
    let result = outcome.scratch.as_ref().unwrap().as_ref().unwrap();
    assert_eq!(result.before(), &scratch);
    assert_eq!(result.after(), &scratch);
    assert!(!result.changed());
    let legacy = run_to_end_with_resources_strict(&field, &def, &before, 123, resources(), &[]).unwrap();
    assert_eq!(applied(&legacy, &before).var(1, 4), 7, "the existing constructor retains its documented scratch fallback");
    assert!(legacy.scratch.is_none());
}

#[test]
fn checked_scratch_exports_actual_masks_and_known_zeroes_separately_from_story() {
    for before in [story(), story().into_native_party().unwrap()] {
        let (field, _) = source(vec![], vec![bit(6, 20, 0, true), bit(5, 20, 1, false)]);
        let scratch = FieldScratch::unknown();
        let outcome = checked(&field, &definition(), &before, &scratch);
        assert_eq!(applied(&outcome, &before), before);
        assert!(outcome.writes.is_story_neutral());
        let result = outcome.scratch.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(result.before(), &scratch);
        assert_eq!((result.after().values()[20], result.after().known_masks()[20]), (1, 3));
        assert_eq!(result.after().known_masks()[21], 0);
        assert_eq!(result.assigned_masks()[20], 3);
        assert!(result.assigned_masks().iter().enumerate().all(|(address, &mask)| address == 20 || mask == 0));
        assert!(result.changed());
    }
    let before = story();
    let scratch = FieldScratch::cleared_for_entry();
    let (written, _) = source(vec![], vec![set_byte(5, 20, 0)]);
    let (empty, _) = source(vec![], vec![]);
    let assignment = checked(&written, &definition(), &before, &scratch);
    let untouched = checked(&empty, &definition(), &before, &scratch);
    let result = assignment.scratch.as_ref().unwrap().as_ref().unwrap();
    assert_eq!(result.after(), &scratch);
    assert!(!result.changed());
    assert_eq!(result.assigned_masks()[20], 255, "an unchanged authored assignment is still an assignment");
    assert!(!assignment.same_story_as(&untouched) && !untouched.same_story_as(&assignment));
}

#[test]
fn unknown_scratch_reads_remain_faults_after_an_overwrite_and_cannot_prove_equivalence() {
    let before = story();
    let scratch = FieldScratch::unknown();
    let (field, _) = source(vec![], vec![if_byte(5, 30, 0, set_byte(1, 4, 7)), set_byte(6, 30, 0)]);
    let def = definition();
    assert!(matches!(run_to_end_with_scratch_strict(&field, &def, &before, 123, resources(), &scratch, &[]),
        Err(SceneError::Scratch(FieldScratchError::Unknown { address: 30, .. }))));
    let mut play = ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch).unwrap();
    play.next(None);
    assert_eq!(play.current_story(), None);
    assert!(matches!(play.scratch_outcome(), Some(Err(FieldScratchError::Unknown { address: 30, .. }))));
    let (empty, _) = source(vec![], vec![]);
    let mut invalid = checked(&empty, &def, &before, &scratch);
    invalid.scratch = play.scratch_outcome();
    assert!(!invalid.is_complete());
    assert!(!invalid.same_story_as(&invalid), "a later overwrite cannot validate an earlier guessed branch");
}

#[test]
fn checked_v3_persistent_unknown_reads_refuse_strict_progress_and_replacement_proofs() {
    let before = story();
    assert!(!before.supports_native_party());
    assert_eq!(before.known_mask(1, 100), 0);
    let scratch = FieldScratch::cleared_for_entry();
    let (field, _) = source(vec![], vec![if_byte(1, 100, 0, set_byte(1, 4, 7)), set_byte(1, 100, 0)]);
    let def = definition();
    assert!(matches!(run_to_end_with_scratch_strict(&field, &def, &before, 123, resources(), &scratch, &[]),
        Err(SceneError::UnknownPersistentRead)));
    let mut play = ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch).unwrap();
    play.next(None);
    assert_eq!(play.current_story(), None);
    let snapshot = play.snapshot().unwrap();
    assert!(snapshot.unknown_persistent_reads.contains(&(1, 100)), "the later assignment cannot erase the unknown branch input");
    assert!(matches!(snapshot.scratch, Some(Err(FieldScratchError::InvalidOutcome))),
        "an unknown persistent input invalidates the checked visit outcome without erasing its mode");
    let (empty, _) = source(vec![], vec![]);
    let mut invalid = checked(&empty, &def, &before, &scratch);
    invalid.unknown_persistent_reads = snapshot.unknown_persistent_reads;
    assert!(!invalid.is_complete());
    assert!(!invalid.same_story_as(&invalid));

    let (field, _) = source(vec![if_byte(1, 100, 0, vec![])], vec![]);
    let mut setup = def;
    setup.setup = vec![ScriptRef::new(0, 0)];
    assert!(matches!(ScenePlay::with_scratch(&field, &setup, &before, 123, resources(), &scratch),
        Err(SceneError::UnknownPersistentRead)));
}

#[test]
fn a_sticky_scratch_fault_blocks_checked_v3_battle_rebase_without_mutating_the_boundary() {
    let before = story();
    assert!(!before.supports_native_party());
    let scratch = FieldScratch::unknown();
    let (field, _) = source(vec![], vec![if_byte(5, 30, 0, set_byte(1, 4, 7)), set_byte(6, 30, 0), vec![0x72, 8, 0], battle()]);
    let mut clean = ScenePlay::with_scratch(&field, &definition(), &before, 123, resources(), &FieldScratch::cleared_for_entry()).unwrap();
    clean.next_strict(None).unwrap();
    let Some(SceneBoundary::Battle(checkpoint)) = clean.boundary() else { panic!("the known-input control reaches its authored battle") };
    let committed = clean.current_story().unwrap();
    let mut play = ScenePlay::with_scratch(&field, &definition(), &before, 123, resources(), &scratch).unwrap();
    assert!(matches!(play.next_strict(None), Err(SceneError::Scratch(FieldScratchError::Unknown { address: 30, .. }))));
    assert_eq!(play.boundary(), Some(SceneBoundary::End(End::Broken)), "a checked fault must never offer a battle");
    assert_eq!(play.current_story(), None);
    let held = play.snapshot();
    let transcript = play.transcript().to_vec();
    assert!(matches!(play.resume_battle_checked(BattleResume {
        checkpoint, resolution: BattleResolution::Victory, story: committed, resources: Resources { gil: Some(125) },
    }), Err(SceneError::Scratch(FieldScratchError::Unknown { address: 30, .. }))));
    assert_eq!(play.snapshot(), held);
    assert_eq!(play.transcript(), transcript);
    assert_eq!((play.resources().gil_before, play.resources().gil_after), (Some(100), Some(100)));
    assert!(matches!(play.scratch_outcome(), Some(Err(FieldScratchError::Unknown { address: 30, .. }))));
}

#[test]
fn checked_scene_setup_must_be_neutral_in_values_knowledge_and_every_intermediate_write() {
    let before = story();
    let mut def = definition();
    def.setup = vec![ScriptRef::new(0, 0)];
    for (scratch, setup) in [
        (FieldScratch::cleared_for_entry(), vec![set_byte(5, 31, 1)]),
        (FieldScratch::cleared_for_entry(), vec![set_byte(5, 31, 1), set_byte(6, 31, 0)]),
        (FieldScratch::unknown(), vec![set_byte(6, 31, 0)]),
    ] {
        let held = scratch.clone();
        let (field, section) = source(setup, vec![]);
        assert!(matches!(ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch), Err(SceneError::ScratchSetup)));
        assert!(matches!(ScenePlay::with_script_section_and_scratch(&field, &def, &before, 123, &section, resources(), &scratch),
            Err(SceneError::ScratchSetup)));
        assert_eq!(scratch, held, "a refused constructor cannot mutate its caller's visit");
        assert!(ScenePlay::with_resources(&field, &def, &before, 123, resources()).is_ok(), "legacy setup is unchanged");
    }
    let scratch = FieldScratch::cleared_for_entry();
    let (field, _) = source(vec![set_byte(5, 31, 0)], vec![]);
    let outcome = checked(&field, &def, &before, &scratch);
    assert_eq!(outcome.scratch.as_ref().unwrap().as_ref().unwrap().after(), &scratch);
    let (field, _) = source(vec![if_byte(5, 31, 0, vec![])], vec![]);
    assert!(matches!(ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &FieldScratch::unknown()),
        Err(SceneError::Scratch(FieldScratchError::Unknown { address: 31, .. }))));
}

#[test]
fn checked_setup_refuses_persistent_changes_restoration_and_new_knowledge_in_both_editions() {
    let scratch = FieldScratch::cleared_for_entry();
    let mut def = definition();
    def.setup = vec![ScriptRef::new(0, 0)];
    for before in [story(), story().into_native_party().unwrap()] {
        for setup in [
            vec![set_byte(1, 4, 11)],
            vec![set_byte(1, 4, 11), set_byte(1, 4, 10)],
            vec![set_byte(1, 100, 0)],
        ] {
            let held = before.clone();
            let (field, section) = source(setup, vec![]);
            assert!(matches!(ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch),
                Err(SceneError::SetupState)));
            assert!(matches!(ScenePlay::with_script_section_and_scratch(&field, &def, &before, 123, &section, resources(), &scratch),
                Err(SceneError::SetupState)));
            assert!(ScenePlay::with_resources(&field, &def, &before, 123, resources()).is_ok(),
                "legacy setup keeps its existing effects");
            assert_eq!(before, held);
        }
        let (field, _) = source(vec![set_byte(1, 4, 10)], vec![set_byte(1, 4, 7)]);
        let outcome = checked(&field, &def, &before, &scratch);
        assert_eq!(applied(&outcome, &before).known_byte(1, 4), Some(7),
            "an unchanged setup assignment is allowed and ordinary root changes remain meaningful");
    }
}

#[test]
fn checked_setup_refuses_temporary_gil_changes_but_allows_true_resource_noops() {
    let gold = |opcode: u8, amount: u32| [vec![opcode, 0], amount.to_le_bytes().to_vec()].concat();
    let before = story();
    let scratch = FieldScratch::cleared_for_entry();
    let mut def = definition();
    def.setup = vec![ScriptRef::new(0, 0)];
    for setup in [vec![gold(0x39, 1)], vec![gold(0x39, 1), gold(0x3A, 1)]] {
        let (field, section) = source(setup, vec![]);
        assert!(matches!(ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch),
            Err(SceneError::SetupState)));
        assert!(matches!(ScenePlay::with_script_section_and_scratch(&field, &def, &before, 123, &section, resources(), &scratch),
            Err(SceneError::SetupState)));
        assert!(ScenePlay::with_resources(&field, &def, &before, 123, resources()).is_ok());
    }
    let (field, _) = source(vec![gold(0x39, 0), gold(0x3A, 0)], vec![gold(0x3A, 1)]);
    let outcome = checked(&field, &def, &before, &scratch);
    assert_eq!((outcome.resources.gil_before, outcome.resources.gil_after), (Some(100), Some(99)),
        "only quiet setup is constrained; the root's actual debit remains in the outcome");
}

#[test]
fn checked_setup_refuses_native_party_and_knowledge_changes_even_when_restored() {
    use crate::story::NativePartyState;
    let mut before = story().into_native_party().unwrap();
    *before.native_party_mut().unwrap() = NativePartyState::from_parts(
        [0, 255, 255], 7, (1, u16::MAX), (0, u16::MAX),
    ).unwrap();
    for (address, value) in [(9, 0), (10, 255), (11, 255)] { before.write_byte(3, address, value).unwrap(); }
    let scratch = FieldScratch::cleared_for_entry();
    let mut def = definition();
    def.setup = vec![ScriptRef::new(0, 0)];
    for setup in [
        vec![vec![0xCA, 1, 254, 254]],
        vec![vec![0xCD, 1, 1], vec![0xCD, 0, 1]],
        vec![vec![0xCE, 0], vec![0xCF, 0]],
    ] {
        let (field, _) = source(setup, vec![]);
        assert!(matches!(ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch),
            Err(SceneError::SetupState)));
    }
    let (field, _) = source(vec![vec![0xCA, 0, 254, 254], vec![0xCD, 1, 0], vec![0xCF, 0]], vec![]);
    let outcome = checked(&field, &def, &before, &scratch);
    assert_eq!(applied(&outcome, &before), before, "unchanged native assignments are safe to repeat");

    let mut partial = before.clone();
    *partial.native_party_mut().unwrap() = NativePartyState::from_parts(
        [0, 255, 255], 7, (1, 1), (0, 0),
    ).unwrap();
    for setup in [vec![vec![0xCD, 0, 1]], vec![vec![0xCF, 0]]] {
        let (field, _) = source(setup, vec![]);
        assert!(matches!(ScenePlay::with_scratch(&field, &def, &partial, 123, resources(), &scratch),
            Err(SceneError::SetupState)), "establishing a known zero is a setup change");
    }
}

#[test]
fn checked_setup_cannot_change_v3_main_party_even_when_raw_bytes_already_match() {
    let mut before = StoryState::at(33).into_extended().unwrap();
    for (address, value) in [(9, 0), (10, 255), (11, 255)] { before.write_byte(3, address, value).unwrap(); }
    let scratch = FieldScratch::cleared_for_entry();
    let mut def = definition();
    def.setup = vec![ScriptRef::new(0, 0)];
    let (field, _) = source(vec![vec![0xCA, 0, 254, 254]], vec![]);
    assert!(matches!(ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch),
        Err(SceneError::SetupState)), "a native-style raw/main divergence cannot hide a v3 setup party change");
}

#[test]
fn the_setup_state_audit_survives_statistics_scene_and_battle_resets() {
    use crate::script::FieldScript;
    let before = story();
    let scratch = FieldScratch::cleared_for_entry();
    let (_, section) = source(vec![set_byte(1, 4, 11), set_byte(1, 4, 10)], vec![vec![0x72, 8, 0], battle()]);
    let script = FieldScript::parse(&section).unwrap();
    let vars = SceneVars::with_scratch(before.clone(), &[], scratch).unwrap();
    let mut vm = Vm::with_resources(vars, 123, Limits::default(), resources());
    vm.setup(&script, &[ScriptRef::new(0, 0)]);
    assert!(!vm.setup_is_state_neutral());
    assert_eq!(vm.writes().get(&(1, 4)), Some(&10), "the final value alone hides the temporary change");
    vm.take_stats();
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(!vm.setup_is_state_neutral());
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Battle, .. }));
    vm.resume_battle(&before, resources());
    assert!(!vm.setup_is_state_neutral());
    vm.take_stats();
    vm.begin(Vec::new());
    assert!(!vm.setup_is_state_neutral(), "no reset can turn replayed setup into an existing-visit proof");
}

#[test]
fn checked_constructors_refuse_legacy_stories_and_scratch_given_overrides_in_both_script_editions() {
    let (field, section) = source(vec![], vec![]);
    let scratch = FieldScratch::cleared_for_entry();
    let before = story();
    let mut def = definition();
    for bank in [5, 6] {
        def.given = vec![Given { bank, address: 31, value: 0 }];
        assert!(matches!(ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch),
            Err(SceneError::ScratchGiven { bank: actual, address: 31 }) if actual == bank));
        assert!(matches!(ScenePlay::with_script_section_and_scratch(&field, &def, &before, 123, &section, resources(), &scratch),
            Err(SceneError::ScratchGiven { bank: actual, address: 31 }) if actual == bank));
        assert!(ScenePlay::with_resources(&field, &def, &before, 123, resources()).is_ok());
    }
    def.given.clear();
    for legacy in [StoryState::at(69), StoryState::new_game()] {
        assert!(matches!(ScenePlay::with_scratch(&field, &def, &legacy, 123, resources(), &scratch), Err(SceneError::ScratchStory)));
        assert!(matches!(ScenePlay::with_script_section_and_scratch(&field, &def, &legacy, 123, &section, resources(), &scratch),
            Err(SceneError::ScratchStory)));
    }
}

#[test]
fn checked_scratch_cannot_skip_an_unrecorded_battle_prefix() {
    let (field, section) = source(vec![], vec![vec![0x72, 8, 0], battle(), set_byte(5, 31, 1)]);
    let scratch = FieldScratch::cleared_for_entry();
    let before = story();
    let mut def = definition();
    def.resume_after_battle = Some(328);
    assert!(matches!(ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch),
        Err(SceneError::UncheckedReplay)));
    assert!(matches!(ScenePlay::with_script_section_and_scratch(&field, &def, &before, 123, &section, resources(), &scratch),
        Err(SceneError::UncheckedReplay)));
    assert!(ScenePlay::with_resources(&field, &def, &before, 123, resources()).is_ok(),
        "legacy presentation retains its existing battle-skip constructor");
    assert!(ScenePlay::with_script_section_and_resources(&field, &def, &before, 123, &section, resources()).is_ok());
}

#[test]
fn checked_replacement_and_replay_bind_the_mode_and_every_initial_scratch_value_and_mask() {
    let (field, section) = source(vec![], vec![message(), set_byte(5, 31, 1)]);
    let def = definition();
    let before = story();
    let scratch = FieldScratch::cleared_for_entry();
    let mut play = ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch).unwrap();
    let identity = play.replay_identity();
    let replacement = ScenePlay::with_script_section_and_scratch(&field, &def, &before, 123, &section, resources(), &scratch).unwrap();
    assert_eq!(replacement.replay_identity(), identity);
    let legacy = ScenePlay::with_resources(&field, &def, &before, 123, resources()).unwrap();
    assert_ne!(legacy.replay_identity(), identity);
    let mut different_value = scratch.clone();
    different_value.write_u8(6, 255, 1).unwrap();
    let mut masks = *scratch.known_masks();
    masks[255] = 254;
    let different_knowledge = FieldScratch::from_parts(*scratch.values(), masks).unwrap();
    for changed in [&different_value, &different_knowledge] {
        let other = ScenePlay::with_scratch(&field, &def, &before, 123, resources(), changed).unwrap();
        assert_ne!(other.replay_identity(), identity, "even an unread input byte belongs to the replay contract");
        assert!(matches!(replay_scene_with_scratch(&field, &def, &before, resources(), changed, identity, &[]),
            Err(SceneError::ReplayIdentity)));
    }
    assert!(matches!(replay_scene_with_scratch(&field, &def, &before, resources(), &scratch, legacy.replay_identity(), &[]),
        Err(SceneError::ReplayIdentity)));
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Window(checkpoint)) = play.boundary() else { panic!("expected window") };
    play.next_strict(Some(WindowAck { checkpoint, answer: None })).unwrap();
    let replay = replay_scene_to_with_scratch(&field, &def, &before, resources(), &scratch, identity,
        play.transcript(), SceneBoundary::End(End::Finished)).unwrap();
    assert_eq!(replay.snapshot(), play.snapshot());
    let original = checked(&field, &def, &before, &scratch);
    let replaced = run_section_to_end_with_scratch_strict(&field, &def, &before, 123, &section, resources(), &scratch, &[]).unwrap();
    assert!(original.same_story_as(&replaced) && replaced.same_story_as(&original));
}

#[test]
fn battle_rebase_preserves_the_original_scratch_preimage_and_cumulative_assignment_masks() {
    for before in [story(), story().into_native_party().unwrap()] {
        let prefix = vec![set_byte(5, 31, 1), vec![0x99, 5, 40], vec![0x72, 8, 0]];
        let suffix = vec![if_byte(6, 31, 1, set_byte(1, 4, 7)), set_byte(6, 32, 2), vec![0x99, 5, 41]];
        let (field, _) = source(vec![], [prefix.clone(), vec![battle()], suffix.clone()].concat());
        let (control, _) = source(vec![], [prefix, suffix].concat());
        let def = definition();
        let scratch = FieldScratch::cleared_for_entry();
        let mut play = ScenePlay::with_scratch(&field, &def, &before, 123, resources(), &scratch).unwrap();
        let identity = play.replay_identity();
        play.next_strict(None).unwrap();
        let Some(SceneBoundary::Battle(checkpoint)) = play.boundary() else { panic!("expected battle") };
        let committed = play.current_story().unwrap();
        let pre_battle = play.scratch_outcome().unwrap().unwrap();
        assert_eq!(pre_battle.before(), &scratch);
        assert_eq!(pre_battle.after().read_u8(6, 31), Ok(1));
        play.resume_battle_checked(BattleResume {
            checkpoint, resolution: BattleResolution::Victory, story: committed, resources: Resources { gil: Some(125) },
        }).unwrap();
        assert_eq!(play.scratch_outcome(), Some(Ok(pre_battle)), "battle return does not reset the visit's scratch run");
        play.next_strict(None).unwrap();
        assert_eq!(play.boundary(), Some(SceneBoundary::End(End::Finished)));
        assert_eq!(play.current_story().unwrap().var(1, 4), 7);
        let result = play.scratch_outcome().unwrap().unwrap();
        assert_eq!(result.before(), &scratch);
        assert_eq!(result.after().read_u8(5, 31), Ok(1));
        assert_eq!(result.after().read_u8(5, 32), Ok(2));
        for address in [31, 32, 40, 41] { assert_eq!(result.assigned_masks()[address], 255); }
        let uninterrupted = checked(&control, &def, &before, &scratch);
        assert_eq!(uninterrupted.scratch, Some(Ok(result)));
        let replay = replay_scene_to_with_scratch(&field, &def, &before, resources(), &scratch, identity,
            play.transcript(), SceneBoundary::End(End::Finished)).unwrap();
        assert_eq!(replay.snapshot(), play.snapshot());
        assert_eq!(replay.current_story(), play.current_story());
    }
}

#[test]
fn consecutive_scenes_use_the_retained_latch_instead_of_repeating_its_persistent_effect() {
    let before = story();
    let scratch = FieldScratch::cleared_for_entry();
    let body = [set_byte(1, 4, 7), set_byte(6, 31, 1)].concat();
    let (field, _) = source(vec![], vec![if_byte(5, 31, 0, body)]);
    let def = definition();
    let first = checked(&field, &def, &before, &scratch);
    let after = applied(&first, &before);
    assert_eq!(after.var(1, 4), 7);
    let retained = first.scratch.as_ref().unwrap().as_ref().unwrap().after();
    assert_eq!(retained.read_u8(5, 31), Ok(1));
    let second = checked(&field, &def, &after, retained);
    assert_eq!(applied(&second, &after), after);
    assert!(second.writes.is_story_neutral());
    let result = second.scratch.as_ref().unwrap().as_ref().unwrap();
    assert_eq!(result.before(), retained);
    assert_eq!(result.after(), retained);
    assert!(result.assigned_masks().iter().all(|mask| *mask == 0));
}
