//! Synthetic semantic decisions only: these fixtures neither admit a field visit nor
//! restore an anchor, run a field-entry profile, write a store, or contact game clients.

use super::IdleVisitSource;
use fmv_core::{AccountId, GameId, InventoryEntry};
use fmv_ff7::field::FieldFile;
use fmv_ff7::script::scene::{SceneDef, ScriptRef, Spot, Step, Trigger};
use fmv_ff7::script::FieldScratch;
use fmv_ff7::story::{NativePartyState, StoryState};
use fmv_persist::{CharacterProgress, FieldVisitEntryV1, FieldVisitOperationKind, FieldVisitV1,
    ProgressPair, Progression, SceneCheckpoint, ScenePhase, SceneReservation, SceneState};

const KEY: &str = "fixture";
const PROFILE: &[u8] = b"synthetic-existing-visit-v1";
const OPERATION: [u8; 16] = [19; 16];
const NOW: u64 = 900;

fn byte(bank: u8, address: u8, value: u8) -> Vec<u8> { vec![0x80, bank << 4, address, value] }
fn word(bank: u8, address: u8, value: u16) -> Vec<u8> {
    [vec![0x81, bank << 4, address], value.to_le_bytes().to_vec()].concat()
}
fn copy(destination: u8, address: u8, source: u8, from: u8) -> Vec<u8> {
    vec![0x80, (destination << 4) | source, address, from]
}
fn gold(opcode: u8, amount: u32) -> Vec<u8> { [vec![opcode, 0], amount.to_le_bytes().to_vec()].concat() }
fn branch(bank: u8, address: u8, value: u8, body: &[u8]) -> Vec<u8> {
    [vec![0x14, bank << 4, address, value, 0, u8::try_from(body.len() + 1).unwrap()], body.to_vec()].concat()
}

/// The small subset of the FF7 crate's private SynthScript/FieldSpec fixtures used here:
/// one entity, an explicit init/main RET pair, one talk, one dialog, and one flat triangle.
/// Keep these original synthetic bytes local instead of exposing test builders in production.
fn field(init: &[u8], body: &[u8]) -> FieldFile { field_with_tag(init, body, 0) }

fn field_with_tag(init: &[u8], body: &[u8], tag: u8) -> FieldFile {
    const CODE_START: usize = 0x20 + 8 + 32 * 2;
    let mut code = Vec::new();
    let mut entries = [0u16; 32];
    for (slot, entry) in entries.iter_mut().enumerate() {
        *entry = u16::try_from(CODE_START + code.len()).unwrap();
        match slot {
            0 => { code.extend_from_slice(init); code.extend_from_slice(&[0, 0]); },
            1 => { code.extend_from_slice(body); code.push(0); },
            _ => code.push(0),
        }
    }
    let text_at = CODE_START + code.len();
    let mut script = vec![0; 0x20];
    script[..4].copy_from_slice(&[2, 5, 1, 0]);
    script[4..6].copy_from_slice(&u16::try_from(text_at).unwrap().to_le_bytes());
    script[8..10].copy_from_slice(&512u16.to_le_bytes());
    script.extend_from_slice(b"npc\0\0\0\0\0");
    for entry in entries { script.extend_from_slice(&entry.to_le_bytes()); }
    script.extend_from_slice(&code);
    script.extend_from_slice(&[1, 0, 4, 0]); // One dialog, relative offset four.
    script.extend(b"Yes".iter().map(|b| b - 0x20));
    script.push(0xE7);
    script.extend(b"No".iter().map(|b| b - 0x20));
    script.push(0xFF);

    let mut mesh = 1u32.to_le_bytes().to_vec();
    for (x, y) in [(-100i16, -100i16), (100, -100), (0, 100)] {
        for value in [x, y, 0, 0] { mesh.extend_from_slice(&value.to_le_bytes()); }
    }
    mesh.extend_from_slice(&[255; 6]);
    let sections = [script, vec![], vec![0, 0, 0, 0, 0, 2], vec![], mesh,
        vec![tag], vec![], vec![], vec![]];
    let mut encoded = vec![0, 0];
    encoded.extend_from_slice(&9u32.to_le_bytes());
    let mut offset = 2 + 4 + 9 * 4;
    for section in &sections {
        encoded.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
        offset += 4 + section.len();
    }
    for section in sections {
        encoded.extend_from_slice(&u32::try_from(section.len()).unwrap().to_le_bytes());
        encoded.extend_from_slice(&section);
    }
    FieldFile::parse(encoded).unwrap()
}

fn definition() -> SceneDef {
    let spot = Spot { x: 0, y: 0, triangle: 0, direction: 64 };
    SceneDef {
        id: "ff7/fixture/idle-action".into(), game: "ff7".into(), field: KEY.into(),
        trigger: Trigger { entity: 0, name: "npc".into(), slot: 1, talk_radius: 80, moments: 36..=36 },
        setup: Vec::new(), given: Vec::new(), root: vec![Step::Await(ScriptRef::new(0, 1))],
        cast: Vec::new(), speakers: Vec::new(), witness: Vec::new(), arrival: spot,
        presents: None, presents_bit: None, resume_after_battle: None,
    }
}

fn story() -> StoryState {
    let mut story = StoryState::at(36).into_extended().unwrap();
    story.write_byte(1, 4, 50).unwrap();
    story.write_masked(13, 101, 0x80, 0x80).unwrap();
    story
}

fn state(source: &IdleVisitSource<'_>, story: &StoryState, scratch: &FieldScratch) -> SceneState {
    let account = AccountId(7);
    let character = CharacterProgress {
        schema_version: 4, exp: 400, ap: 17, gil: 240, limit: 137,
        inventory: Some(vec![InventoryEntry::new(8, 3), InventoryEntry::new(1, 19)]),
        ..CharacterProgress::new(account, GameId::Ff7, 2, 8, 41)
    };
    SceneState {
        pair: ProgressPair { story: Progression::new(account, GameId::Ff7, story.encode(), 40), character: Some(character) },
        checkpoint: None, reservation: None,
        field_visit: Some(FieldVisitV1 {
            schema_version: 1, visit_id: [7; 16], revision: 12, field: KEY.into(), source_identity: source.identity(),
            entry: FieldVisitEntryV1 { operation: [8; 16], arrival: vec![0xA5, 0, 255, 37] },
            anchor: vec![0xC3, 255, 0, 12], scratch_values: *scratch.values(), scratch_known_masks: *scratch.known_masks(),
            active_continuation: None,
        }),
    }
}

fn next_scratch(state: &SceneState) -> FieldScratch {
    let visit = state.field_visit.as_ref().unwrap();
    FieldScratch::from_parts(visit.scratch_values, visit.scratch_known_masks).unwrap()
}

#[test]
fn identity_binds_the_full_field_key_initializers_and_profile() {
    let original = field(&[], &byte(5, 31, 1));
    let script_change = field(&[], &byte(5, 31, 2));
    let other_section = field_with_tag(&[], &byte(5, 31, 1), 1);
    let source = IdleVisitSource::new(KEY, &original, &[], PROFILE).unwrap();
    assert_eq!(source.identity(), IdleVisitSource::new(KEY, &original, &[], PROFILE).unwrap().identity());
    assert_ne!(source.identity(), [0; 32]);
    let init = [ScriptRef::new(0, 0)];
    for changed in [
        IdleVisitSource::new(KEY, &script_change, &[], PROFILE).unwrap(),
        IdleVisitSource::new(KEY, &other_section, &[], PROFILE).unwrap(),
        IdleVisitSource::new("other", &original, &[], PROFILE).unwrap(),
        IdleVisitSource::new(KEY, &original, &init, PROFILE).unwrap(),
        IdleVisitSource::new(KEY, &original, &[], b"other-profile").unwrap(),
    ] {
        assert_ne!(changed.identity(), source.identity());
        let before = state(&source, &story(), &FieldScratch::unknown());
        assert!(changed.scene_job(&definition(), &before, OPERATION, 1, &[], NOW).is_err());
        assert!(changed.talk_job(0, &before, OPERATION, 1, &[], NOW).is_err());
    }
}

#[test]
fn a_scene_freezes_story_gil_and_scratch_as_one_exact_afterimage() {
    let body = [word(2, 0, 37), byte(13, 100, 42), gold(0x3A, 7), vec![0x82, 0x50, 31, 0]].concat();
    let field = field(&[], &body);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let mut scratch = FieldScratch::unknown();
    scratch.write_masked_u8(5, 31, 0x80, 0x80).unwrap();
    let original_story = story();
    let before = state(&source, &original_story, &scratch);
    let job = source.scene_job(&definition(), &before, OPERATION, 91, &[], NOW).unwrap();
    assert_eq!(job.expected(), &before);
    assert_eq!(job.operation().id, OPERATION);
    assert_eq!(job.operation().kind, FieldVisitOperationKind::Advance);

    let mut expected = before.clone();
    let mut after_story = original_story.clone();
    after_story.moment = 37;
    after_story.write_byte(13, 100, 42).unwrap();
    expected.pair.story.blob = after_story.encode();
    expected.pair.story.updated_at = NOW;
    let character = expected.pair.character.as_mut().unwrap();
    character.gil = 233;
    character.updated_at = NOW;
    let visit = expected.field_visit.as_mut().unwrap();
    visit.revision += 1;
    visit.scratch_values[31] = 0x81;
    visit.scratch_known_masks[31] = 0x81;
    assert_eq!(job.next(), &expected, "all unrelated resources, knowledge, entry and anchor are unchanged");
    assert_eq!(state(&source, &original_story, &scratch), before, "the supplied predecessor is immutable");
    assert_eq!(job.clone(), job, "retry retains these exact timestamps and images");
}

#[test]
fn a_neutral_action_keeps_unknown_scratch_and_both_inventory_presence_cases() {
    let field = field(&[], &[]);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    for inventory in [None, Some(Vec::new())] {
        let mut before = state(&source, &story(), &FieldScratch::unknown());
        before.pair.character.as_mut().unwrap().inventory = inventory;
        let mut expected = before.clone();
        expected.pair.story.updated_at = NOW;
        expected.pair.character.as_mut().unwrap().updated_at = NOW;
        expected.field_visit.as_mut().unwrap().revision += 1;
        assert_eq!(source.scene_job(&definition(), &before, OPERATION, 1, &[], NOW).unwrap().next(), &expected);
        assert_eq!(source.talk_job(0, &before, OPERATION, 1, &[], NOW).unwrap().next(), &expected);
    }
}

#[test]
fn a_second_talk_reuses_the_latch_without_changing_story_or_reinitializing_scratch() {
    let body = [branch(5, 31, 0, &byte(1, 4, 47)), byte(6, 31, 1)].concat();
    let field = field(&[], &body);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let mut scratch = FieldScratch::unknown();
    scratch.write_u8(5, 31, 0).unwrap();
    let before = state(&source, &story(), &scratch);
    let first = source.talk_job(0, &before, OPERATION, 1, &[], NOW).unwrap();
    assert_eq!(StoryState::decode(&first.next().pair.story.blob).unwrap().known_byte(1, 4), Some(47));
    assert_eq!(next_scratch(first.next()).read_u8(5, 31), Ok(1));
    let second = source.talk_job(0, first.next(), [20; 16], 1, &[], NOW + 1).unwrap();
    assert_eq!(second.next().pair.story.blob, first.next().pair.story.blob);
    assert_eq!(second.next().pair.character.as_ref().unwrap().gil, 240);
    assert_eq!(next_scratch(second.next()), next_scratch(first.next()));
    assert_eq!(second.next().field_visit.as_ref().unwrap().revision, 14);
}

#[test]
fn unknown_scratch_or_story_reads_refuse_the_entire_decision_even_after_a_repair() {
    for body in [
        [byte(1, 4, 47), copy(3, 80, 5, 21), byte(6, 21, 0)].concat(),
        [byte(5, 21, 1), copy(6, 22, 1, 99), byte(1, 99, 0)].concat(),
    ] {
        let field = field(&[], &body);
        let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
        let before = state(&source, &story(), &FieldScratch::unknown());
        let unchanged = before.clone();
        assert!(source.scene_job(&definition(), &before, OPERATION, 1, &[], NOW).is_err());
        assert!(source.talk_job(0, &before, OPERATION, 1, &[], NOW).is_err());
        assert_eq!(before, unchanged);
    }
}

#[test]
fn setup_cannot_clear_or_temporarily_change_an_existing_visit() {
    let init_refs = [ScriptRef::new(0, 0)];
    for (scratch, init) in [
        (FieldScratch::unknown(), byte(5, 31, 0)),
        (FieldScratch::cleared_for_entry(), [byte(5, 31, 1), byte(6, 31, 0)].concat()),
    ] {
        let field = field(&init, &byte(1, 4, 47));
        let source = IdleVisitSource::new(KEY, &field, &init_refs, PROFILE).unwrap();
        let before = state(&source, &story(), &scratch);
        let mut def = definition();
        def.setup = init_refs.to_vec();
        assert!(source.scene_job(&def, &before, OPERATION, 1, &[], NOW).is_err());
        assert!(source.talk_job(0, &before, OPERATION, 1, &[], NOW).is_err());
    }
}

#[test]
fn idle_decisions_refuse_battles_travel_and_unimplemented_state_effects() {
    for effect in [
        vec![0x70, 0, 44, 1], // BATTLE300.
        vec![0x60, 1, 0, 0, 0, 0, 0, 0, 0, 0], // MAPJUMP1.
        vec![0x3C], vec![0x3D], vec![0x3E], vec![0x3F], // Full HP/MP restores are not redundant here.
        vec![0x58, 0, 1, 0, 1], // STITM item1 x1.
        vec![0x38, 0, 0, 0, 0, 1], // STTIM.
        vec![0x49, 0, 0, 0], // Unreviewed MENU.
        vec![0x7F, 0, 5], // Unknown RDMSD.
    ] {
        let field = field(&[], &[byte(5, 31, 1), byte(1, 4, 47), effect.clone()].concat());
        let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
        let before = state(&source, &story(), &FieldScratch::cleared_for_entry());
        assert!(source.scene_job(&definition(), &before, OPERATION, 1, &[], NOW).is_err(), "scene effect {effect:?}");
        assert!(source.talk_job(0, &before, OPERATION, 1, &[], NOW).is_err(), "talk effect {effect:?}");
    }
}

#[test]
fn talk_cannot_discard_gil_effects_and_scene_gil_must_fit_the_existing_resource_boundary() {
    let debit_field = field(&[], &gold(0x3A, 1));
    let source = IdleVisitSource::new(KEY, &debit_field, &[], PROFILE).unwrap();
    let before = state(&source, &story(), &FieldScratch::unknown());
    assert!(source.talk_job(0, &before, OPERATION, 1, &[], NOW).is_err());
    let field = field(&[], &gold(0x39, 1));
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let mut before = state(&source, &story(), &FieldScratch::unknown());
    before.pair.character.as_mut().unwrap().gil = fmv_ff7_battle::kernel::MAX_GIL;
    assert!(source.scene_job(&definition(), &before, OPERATION, 1, &[], NOW).is_err());
}

#[test]
fn strict_answers_never_default_or_ignore_trailing_choices() {
    let field = field(&[], &[0x48, 5, 0, 0, 0, 1, 19]);
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let mut scratch = FieldScratch::unknown();
    // Scene playback opens ASK using the existing cursor before an answer arrives.
    // Supply that one known input so these refusals test the answer transcript itself.
    scratch.write_u8(5, 19, 0).unwrap();
    let before = state(&source, &story(), &scratch);
    for answers in [vec![], vec![2], vec![0, 1]] {
        assert!(source.scene_job(&definition(), &before, OPERATION, 1, &answers, NOW).is_err());
        assert!(source.talk_job(0, &before, OPERATION, 1, &answers, NOW).is_err());
    }
    for job in [
        source.scene_job(&definition(), &before, OPERATION, 1, &[1], NOW).unwrap(),
        source.talk_job(0, &before, OPERATION, 1, &[1], NOW).unwrap(),
    ] {
        let after = next_scratch(job.next());
        assert_eq!(after.read_u8(5, 19), Ok(1));
        assert_eq!(after.known_masks(), scratch.known_masks(), "the answer establishes no unrelated scratch knowledge");
    }
}

#[test]
fn legacy_and_malformed_story_never_gain_knowledge_from_a_visit() {
    let field = field(&[], &byte(5, 31, 1));
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    for blob in [Vec::new(), vec![1, 36, 0], vec![2, 36, 0, 1, 1, 225, 1], vec![99]] {
        let mut before = state(&source, &story(), &FieldScratch::unknown());
        before.pair.story.blob = blob.clone();
        assert!(source.scene_job(&definition(), &before, OPERATION, 1, &[], NOW).is_err());
        assert!(source.talk_job(0, &before, OPERATION, 1, &[], NOW).is_err());
        assert_eq!(before.pair.story.blob, blob);
    }
}

#[test]
fn native_story_remains_native_and_unknown_party_resources_are_not_inferred() {
    let field = field(&[], &byte(5, 31, 1));
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let native = story().into_native_party().unwrap();
    let before = state(&source, &native, &FieldScratch::unknown());
    for job in [
        source.scene_job(&definition(), &before, OPERATION, 1, &[], NOW).unwrap(),
        source.talk_job(0, &before, OPERATION, 1, &[], NOW).unwrap(),
    ] { assert_eq!(job.next().pair.story.blob, native.encode()); }
}

#[test]
fn native_party_and_raw_slots_change_together_with_the_scratch_afterimage() {
    let field = field(&[], &[vec![0xCA, 2, 0, 254], byte(5, 31, 1)].concat()); // PRTYE.
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let mut native = story().into_native_party().unwrap();
    *native.native_party_mut().unwrap() = NativePartyState::from_parts([0, 255, 255], 7, (1, 1), (1, 1)).unwrap();
    for (address, value) in [(9, 0), (10, 255), (11, 255)] { native.write_byte(3, address, value).unwrap(); }
    let before = state(&source, &native, &FieldScratch::unknown());
    for job in [
        source.scene_job(&definition(), &before, OPERATION, 1, &[], NOW).unwrap(),
        source.talk_job(0, &before, OPERATION, 1, &[], NOW).unwrap(),
    ] {
        let after = StoryState::decode(&job.next().pair.story.blob).unwrap();
        assert_eq!(after.native_party().unwrap().known_main(), Some([0, 2, 255]));
        assert_eq!([9, 10, 11].map(|at| after.known_byte(3, at)), [Some(2), Some(0), Some(255)]);
        assert_eq!(next_scratch(job.next()).read_u8(6, 31), Ok(1));
        assert_eq!(job.next().pair.character.as_ref().unwrap().gil, 240);
    }
}

#[test]
fn an_idle_bridge_refuses_active_checkpoints_reservations_and_visit_links() {
    let field = field(&[], &byte(5, 31, 1));
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let clean = state(&source, &story(), &FieldScratch::unknown());
    let mut checkpoint = clean.clone();
    checkpoint.checkpoint = Some(SceneCheckpoint {
        instance: [3; 32], scene_key: "ff7/fixture/active".into(), source_hash: source.identity(), replay_version: 1,
        original_story: clean.pair.story.blob.clone(), original_gil: 240, seed: 1, replay: vec![1],
        phase: ScenePhase::Resume { segment: 0 }, consumed_battles: 0,
    });
    checkpoint.field_visit.as_mut().unwrap().active_continuation = Some([3; 32]);
    let mut reservation = clean.clone();
    reservation.reservation = Some(SceneReservation { instance: [3; 32], ordinal: 0, attempt: 0, slot: 1 });
    let mut orphan_link = clean.clone();
    orphan_link.field_visit.as_mut().unwrap().active_continuation = Some([3; 32]);
    for before in [checkpoint, reservation, orphan_link] {
        assert!(source.scene_job(&definition(), &before, OPERATION, 1, &[], NOW).is_err());
        assert!(source.talk_job(0, &before, OPERATION, 1, &[], NOW).is_err());
    }
}

#[test]
fn malformed_or_missing_visit_and_revision_overflow_cannot_produce_a_job() {
    let field = field(&[], &byte(5, 31, 1));
    let source = IdleVisitSource::new(KEY, &field, &[], PROFILE).unwrap();
    let clean = state(&source, &story(), &FieldScratch::unknown());
    let mut invalid = Vec::new();
    let mut before = clean.clone(); before.field_visit = None; invalid.push(before);
    let mut before = clean.clone(); before.pair.character = None; invalid.push(before);
    let mut before = clean.clone(); before.field_visit.as_mut().unwrap().revision = u64::MAX; invalid.push(before);
    let mut before = clean.clone(); before.field_visit.as_mut().unwrap().scratch_values[20] = 1; invalid.push(before);
    let mut before = clean.clone(); before.field_visit.as_mut().unwrap().field = "other".into(); invalid.push(before);
    for before in invalid {
        assert!(source.scene_job(&definition(), &before, OPERATION, 1, &[], NOW).is_err());
        assert!(source.talk_job(0, &before, OPERATION, 1, &[], NOW).is_err());
    }
    assert!(source.scene_job(&definition(), &clean, [0; 16], 1, &[], NOW).is_err());
    assert!(source.talk_job(0, &clean, [0; 16], 1, &[], NOW).is_err());
    let mut wrong_field = definition(); wrong_field.field = "other".into();
    assert!(source.scene_job(&wrong_field, &clean, OPERATION, 1, &[], NOW).is_err());
    let mut wrong_setup = definition(); wrong_setup.setup.push(ScriptRef::new(0, 0));
    assert!(source.scene_job(&wrong_setup, &clean, OPERATION, 1, &[], NOW).is_err(),
        "the source profile binds the exact initializer list used by this scene");
}
