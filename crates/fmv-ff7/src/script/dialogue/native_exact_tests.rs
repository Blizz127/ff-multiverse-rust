//! Synthetic native-party regressions for the authoritative dialogue walker.
//! No owned data or persistence is involved; raw slots, main slots and PHS masks
//! deliberately disagree so a legacy moment-derived party cannot satisfy these tests.

use super::{run_panel_exact, run_script_exact, run_talk_exact, ExactBattle, ExactError, ExactRun, ExactScript};
use crate::script::{synth::SynthScript, FieldScript};
use crate::story::{NativePartyState, StoryState};

fn byte(bank: u8, address: u8, value: u8) -> Vec<u8> {
    vec![0x80, bank << 4, address, value]
}

fn word(bank: u8, address: u8, value: u16) -> Vec<u8> {
    let [lo, hi] = value.to_le_bytes();
    vec![0x81, bank << 4, address, lo, hi]
}

fn getpc(slot: u8, bank: u8, address: u8) -> Vec<u8> {
    vec![0x74, bank, slot, address]
}

fn party_if(opcode: u8, character: u8, body: &[u8]) -> Vec<u8> {
    let mut code = vec![opcode, character, u8::try_from(body.len() + 1).unwrap()];
    code.extend_from_slice(body);
    code
}

fn section(init: &[u8], main: &[u8], talk: &[u8], dialogs: Vec<Vec<u8>>) -> Vec<u8> {
    let mut setup = init.to_vec();
    setup.push(0); // init RET
    setup.extend_from_slice(main);
    setup.push(0); // main RET
    let mut talk = talk.to_vec();
    talk.push(0);
    SynthScript { dialogs, ..Default::default() }.entity("npc", vec![(0, setup), (1, talk)]).build()
}

fn native(raw: [u8; 3], main: [u8; 3]) -> StoryState {
    let mut state = StoryState::at(36).into_native_party().unwrap();
    for (address, member) in [9, 10, 11].into_iter().zip(raw) {
        state.write_byte(3, address, member).unwrap();
    }
    *state.native_party_mut().unwrap() = NativePartyState::from_parts(
        main, 7, (0x8001, u16::MAX), (0x8000, u16::MAX),
    ).unwrap();
    state
}

fn run(body: &[u8], state: &StoryState) -> Result<ExactRun, ExactError> {
    let bytes = section(&[], &[], body, vec![]);
    run_script_exact(&FieldScript::parse(&bytes).unwrap(), state, 7,
        &ExactScript { entity: 0, slot: 1, ..ExactScript::default() }, &[])
}

fn after(result: &ExactRun) -> &StoryState {
    result.native.as_ref().expect("v4 evaluation carries its complete native journal").after()
}

fn raw(state: &StoryState) -> [u8; 3] {
    [9, 10, 11].map(|address| state.known_byte(3, address).unwrap())
}

#[test]
fn getpc_raw_membership_and_phs_membership_read_three_independent_resources() {
    let mut before = native([1, 0, 255], [2, 255, 0]);
    *before.native_party_mut().unwrap() = NativePartyState::from_parts(
        [2, 255, 0], 7, (0x8004, u16::MAX), (0x8000, u16::MAX),
    ).unwrap();
    let saved = before.encode();
    let body = [
        getpc(0, 3, 70), getpc(1, 4, 71), getpc(2, 3, 72),
        byte(3, 73, 0), byte(3, 74, 0), byte(3, 75, 0), byte(3, 76, 0),
        party_if(0xCB, 1, &byte(3, 73, 1)),
        party_if(0xCB, 2, &byte(3, 74, 1)),
        party_if(0xCC, 2, &byte(3, 75, 1)),
        party_if(0xCC, 0, &byte(3, 76, 1)),
    ].concat();
    let result = run(&body, &before).unwrap();
    assert_eq!([70, 71, 72, 73, 74, 75, 76].map(|address| after(&result).known_byte(3, address)),
        [Some(2), Some(255), Some(0), Some(1), Some(0), Some(1), Some(0)]);
    assert_eq!(after(&result).native_party(), before.native_party());
    assert_eq!(raw(after(&result)), [1, 0, 255]);
    assert_eq!(before.encode(), saved, "evaluation never mutates its predecessor");
}

#[test]
fn script_talk_and_panel_entries_all_execute_native_reads() {
    let mut before = native([1, 255, 255], [2, 255, 255]);
    before.native_party_mut().unwrap().write_availability_masked(4, 4);
    let body = [getpc(0, 3, 70), party_if(0xCB, 1, &byte(3, 71, 1)),
        party_if(0xCC, 2, &byte(3, 72, 1))].concat();
    let talk_bytes = section(&[], &[], &body, vec![]);
    let talk_script = FieldScript::parse(&talk_bytes).unwrap();
    let panel_bytes = section(&[], &body, &[], vec![]);
    let panel_script = FieldScript::parse(&panel_bytes).unwrap();
    let results = [
        run_script_exact(&talk_script, &before, 7, &ExactScript { entity: 0, slot: 1, ..ExactScript::default() }, &[]),
        run_talk_exact(&talk_script, 0, &before, 7, &[]),
        run_panel_exact(&panel_script, 0, &before, 7, 8, &[]),
    ];
    for result in results {
        let result = result.unwrap();
        assert_eq!([70, 71, 72].map(|address| after(&result).known_byte(3, address)), [Some(2), Some(1), Some(1)]);
        assert_eq!(after(&result).native_party(), before.native_party());
    }
}

#[test]
fn direct_raw_bank_writes_change_field_membership_without_syncing_main_or_phs() {
    let before = native([0, 2, 255], [0, 2, 255]);
    let body = [byte(4, 9, 1), getpc(0, 3, 70), byte(3, 71, 0), byte(3, 72, 0),
        party_if(0xCB, 1, &byte(3, 71, 1)), party_if(0xCB, 0, &byte(3, 72, 1))].concat();
    let result = run(&body, &before).unwrap();
    assert_eq!(raw(after(&result)), [1, 2, 255]);
    assert_eq!(after(&result).native_party(), before.native_party());
    assert_eq!([70, 71, 72].map(|address| after(&result).known_byte(3, address)), [Some(0), Some(1), Some(0)]);
}

#[test]
fn ordered_raw_write_party_sync_and_restore_preserve_the_intermediate_main_change() {
    use crate::story::NativeStoryEvent;
    let before = native([0, 1, 255], [0, 1, 255]);
    let body = [byte(3, 10, 2), vec![0xC8, 0], byte(4, 10, 1)].concat();
    let result = run(&body, &before).unwrap();
    let journal = result.native.as_ref().unwrap();
    assert_eq!(raw(journal.after()), raw(&before), "the raw tuple ends exactly where it began");
    assert_eq!(journal.after().native_party().unwrap().known_main(), Some([0, 2, 255]),
        "duplicate Add reconciles main against the intermediate raw tuple");
    assert_eq!(journal.after().native_party().unwrap().availability_bits(), before.native_party().unwrap().availability_bits());
    assert!(matches!(journal.events(), [NativeStoryEvent::Assignment(first), NativeStoryEvent::Party(_), NativeStoryEvent::Assignment(last)]
        if first.key() == (3, 10) && first.value() == 2 && last.key() == (3, 10) && last.value() == 1));
    let mut published = before.clone();
    assert!(journal.apply_to(&mut published, &before).unwrap());
    assert_eq!(&published, journal.after(), "publication replays the complete ordered result, not just the final raw map");
    assert_eq!(before.native_party().unwrap().known_main(), Some([0, 1, 255]));
}

#[test]
fn trigger_seeds_guide_authored_writes_without_becoming_durable_party_or_bank_knowledge() {
    let before = native([0, 1, 255], [0, 1, 255]);
    let stored = byte(3, 81, 42);
    let mut body = vec![0x14, 0x30, 80, 7, 0, u8::try_from(stored.len() + 1).unwrap()];
    body.extend(stored);
    let bytes = section(&[], &[], &body, vec![]);
    let script = FieldScript::parse(&bytes).unwrap();
    let entry = ExactScript { entity: 0, slot: 1, seeds: vec![((3, 80), 7)], ..ExactScript::default() };
    let result = run_script_exact(&script, &before, 7, &entry, &[]).unwrap();
    assert_eq!(after(&result).known_byte(3, 80), None, "a trigger observation is not a persistent assignment");
    assert_eq!(after(&result).known_byte(3, 81), Some(42));
    assert_eq!(result.native.as_ref().unwrap().events().len(), 1);

    let bytes = section(&[], &[], &[], vec![]);
    let script = FieldScript::parse(&bytes).unwrap();
    for bank in [3, 4] {
        for address in 9..=11 {
            let entry = ExactScript { entity: 0, slot: 1, seeds: vec![((bank, address), 2)], ..ExactScript::default() };
            assert!(run_script_exact(&script, &before, 7, &entry, &[]).is_err(),
                "trigger seeds cannot substitute for durable raw party bytes");
        }
    }
}

#[test]
fn native_party_mutators_preserve_main_order_and_keep_phs_resources_independent() {
    let mut before = native([0, 1, 255], [1, 255, 0]);
    before.native_party_mut().unwrap().write_availability_masked(7, 7);
    let body = [
        vec![0xC8, 2], // raw [0,1,2], main [1,2,0]
        vec![0xC9, 0], // raw [255,1,2], main [1,2,255]; availability of Cloud stays set
        vec![0xCA, 2, 255, 254], // raw [2,1,255], retained main order [1,2,255]
        getpc(0, 3, 70), getpc(1, 3, 71), getpc(2, 3, 72),
        vec![0xCD, 0, 2], // remove Tifa from PHS/raw, without synchronizing main
        getpc(1, 3, 73), byte(3, 74, 0), byte(3, 75, 0),
        party_if(0xCB, 2, &byte(3, 74, 1)), party_if(0xCC, 2, &byte(3, 75, 1)),
        vec![0xCD, 1, 0], vec![0xCE, 1], vec![0xCE, 2], vec![0xCF, 1],
    ].concat();
    let result = run(&body, &before).unwrap();
    assert_eq!(raw(after(&result)), [255, 1, 255]);
    let party = after(&result).native_party().unwrap();
    assert_eq!(party.known_main(), Some([1, 2, 255]));
    assert_eq!(party.availability_bits(), (0x8003, u16::MAX));
    assert_eq!(party.locking_bits(), (0x8004, u16::MAX));
    assert_eq!([70, 71, 72, 73, 74, 75].map(|address| after(&result).known_byte(3, address)),
        [Some(1), Some(2), Some(255), Some(2), Some(0), Some(0)]);
}

#[test]
fn duplicate_add_and_absent_remove_still_reconcile_main_without_granting_membership() {
    let before = native([2, 255, 0], [0, 1, 255]);
    for opcode in [vec![0xC8, 2], vec![0xC9, 8]] {
        let result = run(&opcode, &before).unwrap();
        assert_eq!(raw(after(&result)), [2, 255, 0]);
        let party = after(&result).native_party().unwrap();
        assert_eq!(party.known_main(), Some([0, 2, 255]));
        assert_eq!(party.availability_bits(), before.native_party().unwrap().availability_bits());
        assert_eq!(party.known_available(2), Ok(Some(false)), "duplicate PRTYP does not run ADD_PARTY_MEMBER");
    }
}

#[test]
fn availability_removal_clears_every_raw_duplicate_but_prtym_removes_only_the_first() {
    let before = native([2, 0, 2], [2, 0, 255]);
    let removed = run(&[0xC9, 2], &before).unwrap();
    assert_eq!(raw(after(&removed)), [255, 0, 2]);
    assert_eq!(after(&removed).native_party().unwrap().known_main(), Some([2, 0, 255]));
    let unavailable = run(&[0xCD, 0, 2], &before).unwrap();
    assert_eq!(raw(after(&unavailable)), [255, 0, 255]);
    assert_eq!(after(&unavailable).native_party().unwrap().known_main(), Some([2, 0, 255]));
}

#[test]
fn sptye_snapshots_every_source_before_replacing_the_raw_party() {
    let before = native([0, 1, 2], [0, 1, 2]);
    // Read raw[1], raw[0], literal forced-empty. Writing the first slot before
    // reading the second source would incorrectly manufacture a duplicate Barret.
    let result = run(&[0x0A, 0x33, 0, 10, 9, 254], &before).unwrap();
    assert_eq!(raw(after(&result)), [1, 0, 255]);
    assert_eq!(after(&result).native_party().unwrap().known_main(), Some([0, 1, 255]));
}

#[test]
fn gtpye_reads_each_raw_slot_after_prior_aliased_destination_writes() {
    let before = native([0, 1, 2], [2, 255, 0]);
    let result = run(&[0x0B, 0x34, 0x30, 10, 11, 9], &before).unwrap();
    assert_eq!(raw(after(&result)), [0, 0, 0], "GTPYE is a sequential copy, not a simultaneous rotation");
    assert_eq!(after(&result).native_party(), before.native_party(), "raw writes neither synchronize main nor grant PHS membership");

    let mut partial = StoryState::at(36).into_native_party().unwrap();
    partial.write_byte(3, 9, 2).unwrap();
    partial.write_byte(3, 11, 0).unwrap();
    // The first assignment establishes raw[1], so the second source is now known.
    let result = run(&[0x0B, 0x33, 0x30, 10, 70, 71], &partial).unwrap();
    assert_eq!(raw(after(&result)), [2, 2, 0]);
    assert_eq!([70, 71].map(|address| after(&result).known_byte(3, address)), [Some(2), Some(0)]);
    assert_eq!(after(&result).native_party(), partial.native_party());
}

#[test]
fn phs_assignments_establish_only_their_bits_without_requiring_party_tuples() {
    let before = StoryState::at(36).into_native_party().unwrap();
    let body = [vec![0xCD, 1, 2], vec![0xCE, 0], vec![0xCF, 1],
        party_if(0xCC, 2, &byte(3, 70, 1))].concat();
    let result = run(&body, &before).unwrap();
    let party = after(&result).native_party().unwrap();
    assert_eq!(party.main_slots(), ([0; 3], 0));
    assert_eq!(party.availability_bits(), (4, 4));
    assert_eq!(party.locking_bits(), (1, 3));
    assert_eq!(party.known_available(0), Ok(None));
    assert_eq!([9, 10, 11].map(|address| after(&result).known_mask(3, address)), [0; 3]);
    assert_eq!(after(&result).known_byte(3, 70), Some(1));
}

#[test]
fn unknown_native_reads_and_late_invalid_gtpye_return_no_committable_prefix() {
    let unknown = StoryState::at(36).into_native_party().unwrap();
    for operation in [
        getpc(0, 3, 70),
        party_if(0xCB, 0, &byte(3, 71, 1)),
        party_if(0xCC, 0, &byte(3, 71, 1)),
        vec![0xC8, 1], vec![0xCA, 0, 254, 254],
        vec![0x0A, 0x30, 0, 70, 254, 254],
    ] {
        let saved = unknown.encode();
        let body = [byte(3, 80, 9), operation].concat();
        assert!(run(&body, &unknown).is_err());
        assert_eq!(unknown.encode(), saved);
    }

    let known = native([0, 1, 2], [0, 1, 2]);
    let mut missing_later_source = StoryState::at(36).into_native_party().unwrap();
    missing_later_source.write_byte(3, 9, 0).unwrap();
    missing_later_source.write_byte(3, 11, 2).unwrap();
    for (before, operation) in [
        (&known, vec![0x0B, 0x34, 0, 70, 71, 72]), // invalid third destination, after two valid writes
        (&missing_later_source, vec![0x0B, 0x33, 0x30, 70, 71, 72]),
    ] {
        let saved = before.encode();
        assert!(run(&[byte(3, 80, 9), operation].concat(), before).is_err());
        assert_eq!(before.encode(), saved, "a failed opcode exposes no partial story");
    }
}

#[test]
fn getpc_outside_the_three_main_slots_is_a_native_no_op() {
    let before = StoryState::at(36).into_native_party().unwrap();
    for slot in [3, 255] {
        let result = run(&[getpc(slot, 3, 70), byte(3, 71, 9)].concat(), &before).unwrap();
        assert_eq!(after(&result).known_byte(3, 70), None);
        assert_eq!(after(&result).known_byte(3, 71), Some(9));
        assert_eq!(after(&result).native_party(), before.native_party());
    }
}

#[test]
fn setup_cannot_hide_a_temporary_native_change_by_restoring_its_final_image() {
    let before = native([0, 255, 255], [0, 255, 255]);
    let saved = before.encode();
    for init in [
        vec![0xCE, 2, 0xCF, 2], // lock then unlock, with the original known-false bit restored
        vec![0xCA, 2, 254, 254, 0xCA, 0, 254, 254, 0xCD, 0, 2],
    ] {
        let bytes = section(&init, &[], &byte(3, 70, 1), vec![]);
        let script = FieldScript::parse(&bytes).unwrap();
        for result in [
            run_script_exact(&script, &before, 7,
                &ExactScript { entity: 0, slot: 1, setup: true, ..ExactScript::default() }, &[]),
            run_talk_exact(&script, 0, &before, 7, &[]),
            run_panel_exact(&script, 0, &before, 7, 8, &[]),
        ] {
            assert!(result.is_err(), "setup is an audit, not a new opportunity to mutate native resources");
        }
        assert_eq!(before.encode(), saved);
    }
    let bytes = section(&[0xCF, 2], &[], &byte(3, 70, 1), vec![]);
    let result = run_talk_exact(&FieldScript::parse(&bytes).unwrap(), 0, &before, 7, &[]).unwrap();
    assert_eq!(after(&result).known_byte(3, 70), Some(1), "an already-established identical assignment remains allowed");
}

#[test]
fn battle_snapshot_keeps_the_prefix_party_when_the_acknowledged_suffix_restores_cloud() {
    let before = native([0, 255, 255], [0, 255, 255]);
    let body = [
        vec![0xCA, 2, 254, 254], vec![0xC8, 0], byte(3, 80, 11),
        vec![0x70, 0, 44, 1], // BATTLE 300
        vec![0xCA, 254, 254, 254], vec![0xCA, 0, 254, 254], vec![0xCD, 0, 2],
        byte(3, 80, 22), word(2, 0, 37),
    ].concat();
    let bytes = section(&[], &[], &body, vec![]);
    let script = FieldScript::parse(&bytes).unwrap();
    let entry = ExactScript { entity: 0, slot: 1, after_battle: Some(300), ..ExactScript::default() };
    let result = run_script_exact(&script, &before, 7, &entry, &[]).unwrap();
    assert_eq!(result.battle, Some(ExactBattle { formation: 300, flags: 0 }));
    let battle = result.battle_story.as_ref().expect("the battle has its exact prefix story");
    assert_eq!(raw(battle), [2, 0, 255]);
    assert_eq!(battle.native_party().unwrap().known_main(), Some([2, 0, 255]));
    assert_eq!(battle.native_party().unwrap().availability_bits(), (0x8005, u16::MAX));
    assert_eq!((battle.moment, battle.known_byte(3, 80)), (36, Some(11)));
    assert_eq!(raw(after(&result)), [0, 255, 255]);
    assert_eq!(after(&result).native_party(), before.native_party());
    assert_eq!((after(&result).moment, after(&result).known_byte(3, 80)), (37, Some(22)));
    assert!(run_script_exact(&script, &before, 7,
        &ExactScript { after_battle: None, ..entry }, &[]).is_err(), "no prefix or suffix commits without the authorized battle");
}

#[test]
fn a_party_name_window_requires_knowledge_of_the_main_slot_it_references() {
    // ExactRun deliberately carries effects rather than rendered messages, so this checks
    // refusal of an unknown substitution; the actual text is covered by message-layer tests.
    let unknown = StoryState::at(36).into_native_party().unwrap();
    let bytes = section(&[], &[], &[0x40, 0, 0], vec![vec![0xF3, 0xFF]]);
    let script = FieldScript::parse(&bytes).unwrap();
    let entry = ExactScript { entity: 0, slot: 1, ..ExactScript::default() };
    assert!(run_script_exact(&script, &unknown, 7, &entry, &[]).is_err());
    let known = native([0, 255, 255], [2, 255, 255]);
    let result = run_script_exact(&script, &known, 7, &entry, &[]).unwrap();
    assert_eq!(after(&result), &known);
}
