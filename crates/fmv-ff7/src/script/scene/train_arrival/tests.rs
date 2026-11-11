//! Invented source exercises the private entry seam; the public factory stays train-only.
use super::*;
use crate::background::BackgroundOp;
use crate::script::scene::{CastMember, SceneVars, ScriptRef, Spot, Stage, Step, Then, Trigger, Vm, WindowAck};
use crate::script::synth::{dialog, SynthScript};
use crate::testutil::FieldSpec;

fn source(init: &[u8], body: &[u8]) -> (FieldFile, Vec<u8>) {
    let section = SynthScript { dialogs: vec![dialog("An entry fixture.")], ..Default::default() }
        .entity("actor", vec![(0, [init, &[0]].concat()), (1, [body, &[0]].concat())]).build();
    let field = FieldFile::parse(FieldSpec { overrides: vec![(0, section.clone())], ..Default::default() }.build()).unwrap();
    (field, section)
}

fn def() -> SceneDef {
    SceneDef {
        id: "test/train-entry-seam".into(), game: "ff7".into(), field: "fixture".into(),
        trigger: Trigger { entity: 0, name: "actor".into(), slot: 1, talk_radius: 0, moments: 48..=48 },
        setup: vec![ScriptRef::new(0, 0)], given: vec![], root: vec![Step::Await(ScriptRef::new(0, 1))],
        cast: vec![CastMember { entity: 0, name: "actor".into(), look: Look::Appearance("ff7/cloud".into()), start: None }],
        speakers: vec![], witness: vec![], arrival: Spot { x: 0, y: 0, triangle: 0, direction: 0 },
        presents: None, presents_bit: None, resume_after_battle: None,
    }
}
fn story() -> StoryState { StoryState::at(48).into_extended().unwrap() }
fn resources() -> SceneResources { SceneResources { gil: Some(240) } }
fn entry(field: &FieldFile, def: &SceneDef) -> Result<ScenePlay, SceneError> {
    let script = FieldScript::from_field(field).unwrap();
    ScenePlay::with_train_entry(field, def, &story(), 123, resources(), TRAIN_ARRIVAL_LANDING,
        source_identity(field, &script, def))
}

#[test]
fn entry_retains_initial_assignments_rng_and_presentation_in_one_vm() {
    let init = [0x80, 0x50, 6, 10, 0x99, 5, 50, 0xE0, 0, 4, 1, 0xA4, 0];
    let body = [0x99, 5, 51, 0x40, 0, 0, 0x80, 0x50, 6, 20];
    let (field, section) = source(&init, &body);
    let def = def();
    let mut play = entry(&field, &def).unwrap();
    let before = play.train_entry().unwrap().clone();
    assert_eq!(before.initial_scratch(), &FieldScratch::cleared_for_entry());
    assert_eq!(before.initialized_scratch().read_u8(5, 6), Ok(10));
    assert!(play.initial_stage_events().contains(&Stage::Background(BackgroundOp::On { group: 4, frame: 1 })));
    assert!(play.initial_stage_events().contains(&Stage::Show { entity: 0, visible: false }));
    assert_eq!(play.setup_stats().handled.get("RANDOM"), Some(&1));
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Window(checkpoint)) = play.boundary() else { panic!("the exact window") };
    let first = play.scratch_outcome().unwrap().unwrap();
    assert_eq!(first.after().read_u8(5, 6), Ok(10));

    // Independently execute init then body as one ordinary root. Reconstructing a fresh
    // VM after init would repeat the first RANDOM instead of continuing its state.
    let script = FieldScript::parse(&section).unwrap();
    let vars = SceneVars::with_scratch(story(), &[], FieldScratch::cleared_for_entry()).unwrap();
    let mut reference = Vm::with_resources(vars, 123, Limits::default(), resources());
    reference.begin(vec![Step::Await(ScriptRef::new(0, 0)), Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(reference.resume(&script, None).then, Then::Window(_)));
    let expected = reference.scratch_outcome().unwrap().unwrap();
    for address in [6, 50, 51] {
        assert_eq!(first.after().read_u8(5, address), expected.after().read_u8(5, address));
    }
    assert_ne!(first.after().read_u8(5, 50), first.after().read_u8(5, 51));
    let beat = play.next_strict(Some(WindowAck { checkpoint, answer: None })).unwrap();
    assert!(matches!(beat.then, Then::End { how: End::Finished, .. }));
    let terminal = play.scratch_outcome().unwrap().unwrap();
    assert_eq!(terminal.before(), before.initial_scratch());
    assert_eq!(terminal.after().read_u8(5, 6), Ok(20));
    for address in [6, 50, 51] { assert_eq!(terminal.assigned_masks()[address], 255); }
    assert_eq!(play.train_entry().unwrap(), &before, "initial inputs never become the afterimage");
    assert!(train_arrival_ready(&play).is_err(), "a synthetic run is not the complete train profile");
}

#[test]
fn entry_is_not_existing_visit_mode_even_when_both_inputs_are_clear() {
    let (field, _) = source(&[0x80, 0x50, 6, 0], &[0x40, 0, 0]);
    let def = def();
    let entry = entry(&field, &def).unwrap();
    let ordinary = ScenePlay::with_scratch(&field, &def, &story(), 123, resources(), &FieldScratch::cleared_for_entry()).unwrap();
    assert_ne!(entry.replay_identity(), ordinary.replay_identity());
    assert!(ordinary.train_entry().is_none());
    let legacy = ScenePlay::with_resources(&field, &def, &story(), 123, resources()).unwrap();
    assert!(legacy.scratch_outcome().is_none() && legacy.train_entry().is_none());
    assert!(matches!(super::super::replay_scene_with_scratch(&field, &def, &story(), resources(),
        &FieldScratch::cleared_for_entry(), entry.replay_identity(), &[]), Err(SceneError::ReplayIdentity)));
    let (changes, _) = source(&[0x80, 0x50, 6, 10], &[]);
    assert!(matches!(ScenePlay::with_scratch(&changes, &def, &story(), 123, resources(), &FieldScratch::cleared_for_entry()),
        Err(SceneError::ScratchSetup)), "existing-visit neutrality is unchanged");
}

#[test]
fn entry_never_permits_unknown_reads_skipped_state_or_persistent_setup_mutations() {
    // SETBYTE scratch6 <- unknown persistent bank3[100].
    let (field, _) = source(&[0x80, 0x53, 6, 100], &[]);
    assert!(entry(&field, &def()).is_err());
    for skipped in [&[0x3C][..], &[0x7F, 0, 5][..]] { // HMPMAX1 / unknown RDMSD
        let (field, _) = source(skipped, &[]);
        assert!(entry(&field, &def()).is_err());
        let (field, _) = source(&[], skipped);
        let mut play = entry(&field, &def()).unwrap();
        assert!(play.next_strict(None).is_err());
        assert!(train_arrival_ready(&play).is_err());
    }
    let (field, _) = source(&[0x80, 0x30, 100, 0], &[]);
    assert!(matches!(entry(&field, &def()), Err(SceneError::SetupState)), "an explicit zero cannot invent persistent knowledge");
}

#[test]
fn train_profile_refuses_time_requests_rng_and_unreviewed_initializer_operands() {
    for forbidden in [vec![0x24, 1, 0], vec![0x99, 5, 6], vec![0x03, 0, 1],
        vec![0x80, 0x50, 31, 0], vec![0x80, 0x50, 6, 9], vec![0x80, 0x53, 6, 100],
        vec![0x80, 0x30, 100, 0]] {
        let (_, section) = source(&forbidden, &[]);
        let script = FieldScript::parse(&section).unwrap();
        assert!(matches!(validate_initializers(&script, &def()), Err(SceneError::TrainEntrySetup)), "{forbidden:?}");
    }
    let (_, section) = source(&[0x80, 0x50, 6, 10, 0xE0, 0, 4, 1], &[]);
    assert_eq!(validate_initializers(&FieldScript::parse(&section).unwrap(), &def()), Ok(()));
    // The legacy VM treats an unknown comparison as false. A reviewed entry cannot
    // use that fallback to skip initializer work under an invented condition.
    for operator in [0, 10, 255] {
        let (_, section) = source(&[0x16, 0x20, 0, 0, 0, 0, operator, 1], &[]);
        assert!(matches!(validate_initializers(&FieldScript::parse(&section).unwrap(), &def()),
            Err(SceneError::TrainEntrySetup)), "unreviewed initializer comparator {operator}");
    }
    let (_, section) = source(&[0x16, 0x20, 0, 0, 0, 0, 4, 1], &[]);
    assert_eq!(validate_initializers(&FieldScript::parse(&section).unwrap(), &def()), Ok(()));
}

#[test]
fn setup_and_body_share_the_budget_and_failure_cannot_be_reset() {
    let (_, section) = source(&[0x80, 0x50, 6, 10], &[0x80, 0x50, 6, 20]);
    let script = FieldScript::parse(&section).unwrap();
    let vars = SceneVars::with_scratch(story(), &[], FieldScratch::cleared_for_entry()).unwrap();
    let mut vm = Vm::with_resources(vars, 1, Limits { steps: 2, ..Limits::default() }, resources());
    vm.setup_train_entry(&script, &[ScriptRef::new(0, 0)]);
    assert_eq!(vm.scratch_current().unwrap().read_u8(5, 6), Ok(10));
    vm.take_stats();
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Budget, .. }));
    assert!(vm.scratch_outcome().unwrap().is_err());
    vm.take_stats();
    vm.begin(vec![]);
    assert!(vm.scratch_outcome().unwrap().is_err(), "a failed entry cannot turn into a valid empty run");
}

#[test]
fn presentation_definition_and_full_field_bytes_are_part_of_entry_source_identity() {
    let (field, section) = source(&[], &[]);
    let script = FieldScript::parse(&section).unwrap();
    let base = def();
    let original = source_identity(&field, &script, &base);
    for mutate in [
        (|def: &mut SceneDef| def.cast[0].name.push('x')) as fn(&mut SceneDef),
        |def| def.cast[0].look = Look::Model { hrc: "aaaa".into(), idle: "other".into() },
        |def| def.speakers.push(("Speaker".into(), 0)),
        |def| def.arrival.direction = 64,
        |def| def.witness.push(Spot { x: 1, y: 2, triangle: 0, direction: 0 }),
    ] {
        let mut changed = base.clone(); mutate(&mut changed);
        assert_ne!(original, source_identity(&field, &script, &changed));
    }
    let changed_field = FieldFile::parse(FieldSpec { overrides: vec![(0, section.clone()), (5, vec![1, 2, 3])],
        ..Default::default() }.build()).unwrap();
    assert_ne!(original, source_identity(&changed_field, &script, &base));
}

#[test]
fn control_release_evidence_records_executed_instructions_not_existing_bytes() {
    // JMPF skips the UC0/MENU2 enable bytes. A source-only instruction scan would
    // incorrectly treat this as release; the VM must retain the actual disabling calls.
    let (_, section) = source(&[], &[0x33, 1, 0x4A, 1, 0x10, 5, 0x33, 0, 0x4A, 0]);
    let script = FieldScript::parse(&section).unwrap();
    let vars = SceneVars::with_scratch(story(), &[], FieldScratch::cleared_for_entry()).unwrap();
    let mut vm = Vm::with_resources(vars, 1, Limits::default(), resources());
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Finished, .. }));
    assert_eq!(vm.control_instruction(false).map(|(_, _, disabled)| disabled), Some(1));
    assert_eq!(vm.control_instruction(true).map(|(_, _, disabled)| disabled), Some(1));
    let (_, section) = source(&[], &[0x33, 1, 0x4A, 1, 0x33, 0, 0x4A, 0]);
    let script = FieldScript::parse(&section).unwrap();
    let vars = SceneVars::with_scratch(story(), &[], FieldScratch::cleared_for_entry()).unwrap();
    let mut vm = Vm::with_resources(vars, 1, Limits::default(), resources());
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Finished, .. }));
    assert_eq!(vm.control_instruction(false).map(|(_, _, disabled)| disabled), Some(0));
    assert_eq!(vm.control_instruction(true).map(|(_, _, disabled)| disabled), Some(0));
}
