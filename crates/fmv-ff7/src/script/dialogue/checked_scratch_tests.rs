//! Opt-in exact scratch execution. A supplied visit image is never implicitly cleared,
//! and its afterimage remains separate from persistent story assignments.

use super::{run_script_exact, run_script_exact_with_scratch, run_talk_exact, run_talk_exact_with_scratch,
    ExactBattle, ExactError, ExactRun, ExactScript};
use crate::script::{synth::{dialog, SynthScript}, FieldScratch, FieldScratchError, FieldScript};
use crate::story::{NativePartyState, StoryState};

fn byte(bank: u8, address: u8, value: u8) -> Vec<u8> { vec![0x80, bank << 4, address, value] }

fn word(bank: u8, address: u8, value: u16) -> Vec<u8> {
    let [lo, hi] = value.to_le_bytes();
    vec![0x81, bank << 4, address, lo, hi]
}

fn copy_byte(destination: u8, address: u8, source: u8, from: u8) -> Vec<u8> {
    vec![0x80, (destination << 4) | source, address, from]
}

fn if_byte(bank: u8, address: u8, op: u8, right: u8, body: &[u8]) -> Vec<u8> {
    [vec![0x14, bank << 4, address, right, op, u8::try_from(body.len() + 1).unwrap()], body.to_vec()].concat()
}

fn if_word(opcode: u8, bank: u8, address: u8, op: u8, right: u16, body: &[u8]) -> Vec<u8> {
    let [lo, hi] = right.to_le_bytes();
    [vec![opcode, bank << 4, address, 0, lo, hi, op, u8::try_from(body.len() + 1).unwrap()], body.to_vec()].concat()
}

fn section(init: &[u8], main: &[u8], talk: &[u8]) -> Vec<u8> {
    SynthScript::default().entity("npc", vec![
        (0, [init.to_vec(), vec![0], main.to_vec(), vec![0]].concat()),
        (1, [talk.to_vec(), vec![0]].concat()),
    ]).build()
}

fn state() -> StoryState { StoryState::at(36).into_extended().unwrap() }

fn run(body: &[u8], story: &StoryState, scratch: &FieldScratch) -> Result<ExactRun, ExactError> {
    let bytes = section(&[], &[], body);
    run_script_exact_with_scratch(&FieldScript::parse(&bytes).unwrap(), story, scratch, 7,
        &ExactScript { entity: 0, slot: 1, ..ExactScript::default() }, &[])
}

fn after(result: &ExactRun) -> &FieldScratch { result.scratch.as_ref().unwrap().after() }

fn writes(result: &ExactRun) -> Vec<(u8, u8, u8, u8)> {
    result.writes.iter().map(|write| (write.bank, write.address, write.value, write.mask)).collect()
}

fn legacy_run(body: &[u8], story: &StoryState) -> Result<ExactRun, ExactError> {
    let bytes = section(&[], &[], body);
    run_script_exact(&FieldScript::parse(&bytes).unwrap(), story, 7,
        &ExactScript { entity: 0, slot: 1, ..ExactScript::default() }, &[])
}

fn story_after(story: &StoryState, result: &ExactRun) -> StoryState {
    let mut after = story.clone();
    for write in &result.writes { after.write_masked(write.bank, write.address, write.value, write.mask).unwrap(); }
    after
}

fn story_word(story: &StoryState, bank: u8, address: u8) -> Option<u16> {
    Some(u16::from_le_bytes([story.known_byte(bank, address)?, story.known_byte(bank, address.checked_add(1)?)?]))
}

#[test]
fn explicit_unknown_is_not_the_legacy_zero_baseline_or_an_entry_clear() {
    let story = state();
    let body = copy_byte(3, 70, 5, 20);
    let unknown = FieldScratch::unknown();
    assert_eq!(run(&body, &story, &unknown), Err(ExactError::Scratch(
        FieldScratchError::Unknown { address: 20, missing_mask: 255 })));
    let cleared = FieldScratch::cleared_for_entry();
    let result = run(&body, &story, &cleared).unwrap();
    assert_eq!(writes(&result), vec![(3, 70, 0, 255)]);
    assert_eq!(after(&result), &cleared);
    assert!(!result.scratch.as_ref().unwrap().changed());
    assert_eq!(unknown, FieldScratch::unknown());

    let bytes = section(&[], &[], &body);
    let legacy = run_script_exact(&FieldScript::parse(&bytes).unwrap(), &story, 7,
        &ExactScript { entity: 0, slot: 1, ..ExactScript::default() }, &[]).unwrap();
    assert_eq!(writes(&legacy), vec![(3, 70, 0, 255)], "the old API keeps its established policy");
    assert!(legacy.scratch.is_none() && legacy.battle_scratch.is_none());
}

#[test]
fn a_latch_survives_two_executions_only_when_the_caller_reuses_the_afterimage() {
    let story = state();
    let before = FieldScratch::cleared_for_entry();
    let bytes = section(&[], &[], &[
        if_byte(6, 20, 0, 0, &byte(3, 70, 1)), byte(5, 20, 1),
    ].concat());
    let script = FieldScript::parse(&bytes).unwrap();
    let first = run_talk_exact_with_scratch(&script, 0, &story, &before, 7, &[]).unwrap();
    assert_eq!(writes(&first), vec![(3, 70, 1, 255)]);
    let second = run_talk_exact_with_scratch(&script, 0, &story, after(&first), 7, &[]).unwrap();
    assert!(second.writes.is_empty(), "the latched branch is not replayed on the next conversation");
    assert_eq!(after(&second).read_u8(6, 20), Ok(1));
    assert!(!second.scratch.as_ref().unwrap().any_change());
    assert_eq!(before.read_u8(5, 20), Ok(0));
    assert_eq!(story, state(), "scratch never enters the persistent predecessor");
}

#[test]
fn an_unknown_read_cannot_be_repaired_by_a_later_assignment() {
    let scratch = FieldScratch::unknown();
    let body = [byte(3, 70, 9), copy_byte(3, 71, 5, 20), byte(6, 20, 7)].concat();
    assert_eq!(run(&body, &state(), &scratch), Err(ExactError::Scratch(
        FieldScratchError::Unknown { address: 20, missing_mask: 255 })));
    assert_eq!(scratch, FieldScratch::unknown(), "neither a prefix nor the later repair is published");
}

#[test]
fn masked_assignments_and_guards_preserve_unrequested_unknown_bits() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_masked_u8(5, 20, 0x80, 0x80).unwrap();
    let body = [
        vec![0x82, 0x50, 20, 1], vec![0x83, 0x60, 20, 2], vec![0x84, 0x60, 20, 1],
        if_byte(5, 20, 10, 1, &byte(3, 70, 42)),
    ].concat();
    let result = run(&body, &state(), &scratch).unwrap();
    assert_eq!(writes(&result), vec![(3, 70, 42, 255)]);
    assert_eq!((after(&result).values()[20], after(&result).known_masks()[20]), (0x80, 0x86));
    assert_eq!(result.scratch.as_ref().unwrap().assigned_masks()[20], 6);
    assert_eq!(after(&result).known_masks()[21], 0);
    assert!(after(&result).read_u8(5, 20).is_err());
    assert_eq!(scratch.known_masks()[20], 0x80);
}

#[test]
fn xor_numeric_guards_and_arithmetic_cannot_guess_missing_scratch_bits() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_masked_u8(5, 20, 2, 2).unwrap();
    for operation in [
        vec![0x84, 0x50, 20, 3], // Unknown bit, unlike the known bit1.
        vec![0x85, 0x50, 20, 1], // PLUS reads the full current byte.
        if_byte(6, 20, 0, 2, &byte(3, 70, 1)),
    ] {
        assert!(run(&operation, &state(), &scratch).is_err());
    }
    assert_eq!((scratch.values()[20], scratch.known_masks()[20]), (2, 2));
}

#[test]
fn wide_selector_five_writes_one_byte_and_six_writes_two_in_the_same_image() {
    let scratch = FieldScratch::unknown();
    let body = [word(6, 20, 0xABCD), word(5, 20, 0x1234), word(5, 255, 0x89EF),
        vec![0x86, 0x50, 255, 1, 0]].concat(); // PLUS2 also reads/writes selector5 as one byte.
    let result = run(&body, &state(), &scratch).unwrap();
    assert_eq!(after(&result).read_u16(6, 20), Ok(0xAB34));
    assert_eq!(after(&result).read_u16(5, 255), Ok(0xF0));
    assert_eq!(after(&result).known_masks()[254], 0);
    assert!(result.writes.is_empty(), "scratch assignments are not ExactWrite save assignments");
    let invalid = [byte(5, 20, 9), word(6, 255, 0x1234)].concat();
    assert!(run(&invalid, &state(), &scratch).is_err(), "a late crossing word exposes no afterimage");
    assert_eq!(scratch, FieldScratch::unknown());
}

#[test]
fn wide_reads_preserve_selector_width_and_signedness() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_u16(6, 20, 0x80FF).unwrap();
    scratch.write_u8(5, 255, 0xFE).unwrap();
    let body = [
        if_word(0x16, 5, 20, 0, 255, &byte(3, 70, 1)),
        if_word(0x16, 6, 20, 0, 0x80FF, &byte(3, 71, 2)),
        if_word(0x18, 6, 20, 0, 0x80FF, &byte(3, 72, 3)),
        if_word(0x16, 5, 255, 0, 254, &byte(3, 73, 4)),
    ].concat();
    assert_eq!(writes(&run(&body, &state(), &scratch).unwrap()), vec![
        (3, 70, 1, 255), (3, 71, 2, 255), (3, 72, 3, 255), (3, 73, 4, 255),
    ]);
    assert!(run(&if_word(0x18, 6, 255, 0, 254, &byte(3, 70, 1)), &state(), &scratch).is_err());
}

#[test]
fn wide_bit_guards_use_selector_five_zero_extension_and_selector_six_bounds() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_masked_u8(6, 21, 2, 2).unwrap();
    let body = [
        if_word(0x18, 5, 20, 10, 9, &byte(3, 70, 1)),
        if_word(0x18, 6, 20, 9, 9, &byte(3, 71, 2)),
        if_word(0x18, 5, 255, 10, 15, &byte(3, 72, 3)),
    ].concat();
    assert_eq!(writes(&run(&body, &state(), &scratch).unwrap()), vec![
        (3, 70, 1, 255), (3, 71, 2, 255), (3, 72, 3, 255),
    ]);
    scratch.write_masked_u8(5, 255, 1, 1).unwrap();
    assert!(run(&if_word(0x18, 6, 255, 9, 0, &byte(3, 70, 1)), &state(), &scratch).is_err(),
        "even a low-bit guard cannot authorize a word that crosses scratch memory");
}

#[test]
fn hbyte_uses_the_original_wide_source_selector() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_u8(5, 255, 0xFF).unwrap();
    scratch.write_u16(6, 20, 0x80FF).unwrap();
    let body = [vec![0x9B, 0x35, 70, 255, 0], vec![0x9B, 0x36, 71, 20, 0],
        vec![0x9A, 0x36, 72, 255]].concat();
    assert_eq!(writes(&run(&body, &state(), &scratch).unwrap()),
        vec![(3, 70, 0, 255), (3, 71, 0x80, 255), (3, 72, 255, 255)]);
    assert!(run(&[0x9B, 0x36, 70, 255, 0], &state(), &scratch).is_err(),
        "LBYTE6 at255 reads one byte, while HBYTE6 requires a crossing word");
}

#[test]
fn hbyte_requires_only_meaningful_high_bits_and_preserves_unknown_low_bits() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_u8(5, 21, 0x80).unwrap();
    let mut story = state();
    story.write_byte(1, 81, 0xAB).unwrap();
    let body = [
        vec![0x9B, 0x36, 70, 20, 0], // Scratch word: only its high byte is known.
        vec![0x9B, 0x35, 71, 255, 0], // Scratch byte: high bits are structurally zero.
        vec![0x9B, 0x32, 72, 80, 0], // Persistent word with an unknown low byte.
        vec![0x9B, 0x31, 73, 255, 0], // Persistent byte with no known value.
    ].concat();
    let result = run(&body, &story, &scratch).unwrap();
    assert_eq!(writes(&result), vec![(3, 70, 0x80, 255), (3, 71, 0, 255), (3, 72, 0xAB, 255), (3, 73, 0, 255)]);
    assert_eq!(after(&result), &scratch, "discarded source bits remain unknown");
    assert_eq!(story_after(&story, &result).known_byte(1, 80), None);
    assert!(run(&[0x9B, 0x36, 70, 255, 0], &story, &scratch).is_err(), "masked reads still validate the word boundary");
    assert!(run(&[0x9B, 0x32, 70, 255, 0], &story, &scratch).is_err());
    assert!(run(&[0x9B, 0x36, 70, 22, 0], &story, &scratch).is_err(), "meaningful unknown high bits still refuse");
}

#[test]
fn existing_visit_setup_refuses_value_or_knowledge_changes_even_when_restored() {
    let story = state();
    for (scratch, init) in [
        (FieldScratch::unknown(), byte(5, 20, 0)),
        (FieldScratch::cleared_for_entry(), [byte(5, 20, 1), byte(6, 20, 0)].concat()),
    ] {
        let bytes = section(&init, &[], &byte(3, 70, 1));
        let script = FieldScript::parse(&bytes).unwrap();
        assert!(run_talk_exact_with_scratch(&script, 0, &story, &scratch, 7, &[]).is_err());
        assert!(run_script_exact_with_scratch(&script, &story, &scratch, 7,
            &ExactScript { entity: 0, slot: 1, setup: true, ..ExactScript::default() }, &[]).is_err());
    }
    let bytes = section(&[], &byte(5, 20, 1), &byte(3, 70, 1));
    assert!(run_talk_exact_with_scratch(&FieldScript::parse(&bytes).unwrap(), 0, &story,
        &FieldScratch::cleared_for_entry(), 7, &[]).is_err(), "the quiet main prelude is also an audit");
}

#[test]
fn exact_setup_remembers_v3_persistent_changes_after_aliases_restore_the_final_image() {
    let mut story = state();
    story.write_byte(3, 70, 0).unwrap();
    story.write_word(4, 80, 0x1234).unwrap();
    story.write_masked(3, 90, 0, 2).unwrap();
    let scratch = FieldScratch::unknown();
    for (init, address) in [
        ([byte(3, 70, 1), byte(4, 70, 0)].concat(), 70),
        ([word(4, 80, 0x1235), word(4, 80, 0x1234)].concat(), 80),
        (vec![0x82, 0x30, 90, 1, 0x83, 0x40, 90, 1], 90),
    ] {
        let bytes = section(&init, &[], &byte(3, 71, 9));
        let script = FieldScript::parse(&bytes).unwrap();
        let entry = ExactScript { entity: 0, slot: 1, setup: true, ..ExactScript::default() };
        let expected = Err(ExactError::SetupWrite { bank: 3, address });
        assert_eq!(run_script_exact(&script, &story, 7, &entry, &[]), expected);
        assert_eq!(run_talk_exact(&script, 0, &story, 7, &[]), expected);
        assert_eq!(run_script_exact_with_scratch(&script, &story, &scratch, 7, &entry, &[]), expected);
        assert_eq!(run_talk_exact_with_scratch(&script, 0, &story, &scratch, 7, &[]), expected);
        assert!(super::run_talk(&script, 0, &story, 7).is_ok(), "permissive preview keeps its prior behavior");

        let authored_entry = ExactScript { entity: 0, slot: 0, through_init: true, ..ExactScript::default() };
        let result = run_script_exact_with_scratch(&script, &story, &scratch, 7, &authored_entry, &[]).unwrap();
        assert_eq!(story_after(&story, &result), story, "an explicit root may author changes; setup is only an audit");
    }
    let bytes = section(&[], &[byte(3, 70, 1), byte(4, 70, 0)].concat(), &byte(3, 71, 9));
    let script = FieldScript::parse(&bytes).unwrap();
    assert_eq!(run_talk_exact_with_scratch(&script, 0, &story, &scratch, 7, &[]),
        Err(ExactError::SetupWrite { bank: 3, address: 70 }), "the talk-time main audit also remembers transient changes");
    assert_eq!(scratch, FieldScratch::unknown());
}

#[test]
fn exact_setup_accepts_known_persistent_no_ops_but_refuses_new_knowledge() {
    let mut story = state();
    story.write_byte(3, 70, 5).unwrap();
    story.write_word(4, 80, 0xAB34).unwrap();
    story.write_masked(3, 90, 2, 2).unwrap();
    let scratch = FieldScratch::unknown();
    let init = [byte(4, 70, 5), word(4, 80, 0xAB34), vec![0x82, 0x30, 90, 1]].concat();
    let bytes = section(&init, &[], &byte(3, 71, 9));
    let script = FieldScript::parse(&bytes).unwrap();
    for result in [run_talk_exact(&script, 0, &story, 7, &[]),
        run_talk_exact_with_scratch(&script, 0, &story, &scratch, 7, &[])] {
        assert_eq!(writes(&result.unwrap()), vec![(3, 71, 9, 255)]);
    }
    assert_eq!(story.known_mask(3, 90), 2, "unrelated unknown bits are not required or established");
    for init in [byte(3, 91, 0), vec![0x83, 0x30, 91, 1]] {
        let bytes = section(&init, &[], &byte(3, 71, 9));
        let script = FieldScript::parse(&bytes).unwrap();
        assert_eq!(run_talk_exact_with_scratch(&script, 0, &story, &scratch, 7, &[]),
            Err(ExactError::SetupWrite { bank: 3, address: 91 }));
    }
    assert_eq!(story.known_mask(3, 91), 0);
}

#[test]
fn identical_setup_assignments_do_not_reinitialize_or_export_a_second_scratch_write() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_masked_u8(5, 20, 2, 2).unwrap();
    let bytes = section(&[0x82, 0x60, 20, 1], &[], &byte(3, 70, 1));
    let result = run_talk_exact_with_scratch(&FieldScript::parse(&bytes).unwrap(), 0, &state(), &scratch, 7, &[]).unwrap();
    assert_eq!(after(&result), &scratch);
    assert!(!result.scratch.as_ref().unwrap().any_change());
    assert!(result.scratch.as_ref().unwrap().assigned_masks().iter().all(|&mask| mask == 0));
    assert_eq!(writes(&result), vec![(3, 70, 1, 255)]);
}

#[test]
fn an_explicit_root_entry_can_execute_init_without_clearing_other_scratch() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_u8(5, 22, 77).unwrap();
    let bytes = section(&byte(5, 20, 1), &copy_byte(3, 70, 6, 20), &[]);
    let result = run_script_exact_with_scratch(&FieldScript::parse(&bytes).unwrap(), &state(), &scratch, 7,
        &ExactScript { entity: 0, slot: 0, through_init: true, ..ExactScript::default() }, &[]).unwrap();
    assert_eq!(writes(&result), vec![(3, 70, 1, 255)]);
    assert_eq!(after(&result).read_u8(5, 20), Ok(1));
    assert_eq!(after(&result).read_u8(6, 22), Ok(77));
    assert_eq!(after(&result).known_masks()[21], 0, "explicit execution still does not imply entry clear");
}

#[test]
fn scratch_trigger_seeds_and_ambiguous_triangle_observations_are_refused() {
    let bytes = section(&[], &[], &byte(3, 70, 1));
    let script = FieldScript::parse(&bytes).unwrap();
    for bank in [5, 6] {
        let entry = ExactScript { entity: 0, slot: 1, seeds: vec![((bank, 20), 0)], ..ExactScript::default() };
        assert!(run_script_exact_with_scratch(&script, &state(), &FieldScratch::unknown(), 7, &entry, &[]).is_err());
        let entry = ExactScript { entity: 0, slot: 1, triangle_sources: vec![(bank, 20, 0)], ..ExactScript::default() };
        assert!(run_script_exact_with_scratch(&script, &state(), &FieldScratch::cleared_for_entry(), 7, &entry, &[]).is_err());
        assert!(run(&[0xB9, bank, 0, 20], &state(), &FieldScratch::cleared_for_entry()).is_err());
    }
    for bank in [1, 2, 7, 15] {
        assert!(run(&[0xB9, bank, 0, 20], &state(), &FieldScratch::cleared_for_entry()).is_err(),
            "persistent GETAI still requires an explicit source-bound observation");
    }
}

#[test]
fn direct_persistent_getai_requires_original_selector_provenance_and_only_its_native_width() {
    let before = state();
    let scratch = FieldScratch::unknown();
    for (selector, address, seeds, expected) in [
        (15, 255, vec![((15, 255), 0xA5)], vec![(7, 255, 0xA5, 255)]),
        (2, 80, vec![((2, 80), 0x34), ((2, 81), 0x12)], vec![(1, 80, 0x34, 255), (1, 81, 0x12, 255)]),
    ] {
        let bytes = section(&[], &[], &[0xB9, selector, 0, address]);
        let entry = ExactScript { entity: 0, slot: 1, seeds, triangle_sources: vec![(selector, address, 0)], ..ExactScript::default() };
        let result = run_script_exact_with_scratch(&FieldScript::parse(&bytes).unwrap(), &before, &scratch, 7, &entry, &[]).unwrap();
        assert_eq!(writes(&result), expected);
        assert_eq!(after(&result), &scratch);
    }
    let bytes = section(&[], &[], &[0xB9, 2, 0, 80]);
    let mismatched = ExactScript { entity: 0, slot: 1, seeds: vec![((1, 80), 0x34), ((1, 81), 0x12)],
        triangle_sources: vec![(1, 80, 0)], ..ExactScript::default() };
    assert!(run_script_exact_with_scratch(&FieldScript::parse(&bytes).unwrap(), &before, &scratch, 7, &mismatched, &[]).is_err(),
        "canonical bank identity cannot substitute for the authored selector's width");
}

#[test]
fn every_persistent_selector_keeps_its_native_width_before_canonicalization() {
    for (byte_selector, word_selector) in [(1, 2), (3, 4), (15, 7), (11, 12), (13, 14)] {
        let mut before = state();
        before.write_word(byte_selector, 80, 0xABCD).unwrap();
        let result = run(&word(byte_selector, 80, 0x1234), &before, &FieldScratch::unknown()).unwrap();
        let after = story_after(&before, &result);
        assert_eq!(story_word(&after, word_selector, 80), Some(0xAB34), "selector {byte_selector} stores only its low byte");
        let result = run(&word(word_selector, 80, 0x1234), &before, &FieldScratch::unknown()).unwrap();
        assert_eq!(story_word(&story_after(&before, &result), byte_selector, 80), Some(0x1234),
            "selector {word_selector} stores both physical bytes");
        let result = run(&word(byte_selector, 255, 0x1234), &before, &FieldScratch::unknown()).unwrap();
        assert_eq!(story_after(&before, &result).known_byte(word_selector, 255), Some(0x34));
        assert!(run(&word(word_selector, 255, 0x1234), &before, &FieldScratch::unknown()).is_err());

        let legacy = legacy_run(&word(byte_selector, 80, 0x1234), &before).unwrap();
        assert_eq!(story_word(&story_after(&before, &legacy), byte_selector, 80), Some(0x1234),
            "the old API retains its old width policy");
        assert!(legacy_run(&word(byte_selector, 255, 0x1234), &before).is_err());
    }
}

#[test]
fn persistent_signed_queries_high_bytes_and_bit_guards_use_the_original_selector() {
    for (byte_selector, word_selector) in [(1, 2), (3, 4), (15, 7), (11, 12), (13, 14)] {
        let mut before = state();
        before.write_word(byte_selector, 80, 0x80FF).unwrap();
        before.write_byte(byte_selector, 255, 1).unwrap();
        let body = [
            if_word(0x16, byte_selector, 80, 0, 255, &byte(3, 70, 1)),
            if_word(0x16, word_selector, 80, 0, 0x80FF, &byte(3, 71, 2)),
            vec![0x9B, 0x30 | byte_selector, 72, 80, 0],
            vec![0x9B, 0x30 | word_selector, 73, 80, 0],
            if_word(0x18, byte_selector, 80, 10, 15, &byte(3, 74, 3)),
            if_word(0x18, word_selector, 80, 9, 15, &byte(3, 75, 4)),
            vec![0x9B, 0x30 | byte_selector, 76, 255, 0],
        ].concat();
        assert_eq!(writes(&run(&body, &before, &FieldScratch::unknown()).unwrap()), vec![
            (3, 70, 1, 255), (3, 71, 2, 255), (3, 72, 0, 255), (3, 73, 0x80, 255),
            (3, 74, 3, 255), (3, 75, 4, 255), (3, 76, 0, 255),
        ]);
        assert!(run(&if_word(0x18, word_selector, 255, 9, 0, &byte(3, 70, 1)),
            &before, &FieldScratch::unknown()).is_err(), "wide bounds precede bit-mask dependency checks");
    }
}

#[test]
fn wide_division_and_remainder_use_signed_operands_before_narrowing() {
    for (left, right, quotient, remainder) in [(-7i16, 2i16, 0xFFFD, 0xFFFF), (i16::MIN, -1, 0x8000, 0)] {
        let [lo, hi] = right.to_le_bytes();
        for (opcode, expected) in [(0x8C, quotient), (0x8E, remainder)] {
            let body = [word(6, 20, left as u16), vec![opcode, 0x60, 20, lo, hi]].concat();
            assert_eq!(after(&run(&body, &state(), &FieldScratch::unknown()).unwrap()).read_u16(6, 20), Ok(expected));
        }
    }
    for opcode in [0x8C, 0x8E] {
        let body = [word(6, 20, (-7i16) as u16), vec![opcode, 0x60, 20, 0, 0]].concat();
        assert!(run(&body, &state(), &FieldScratch::unknown()).is_err(), "zero divisor cannot publish a scratch prefix");
    }
    for (opcode, expected) in [(0x8C, 0x7FFC), (0x8E, 1)] {
        let body = [word(6, 20, (-7i16) as u16), vec![opcode, 0x60, 20, 2, 0],
            vec![0x81, 0x46, 80, 20, 0]].concat();
        let legacy = legacy_run(&body, &state()).unwrap();
        assert_eq!(story_word(&story_after(&state(), &legacy), 4, 80), Some(expected),
            "the old API keeps its unsigned current-value behavior");
    }
}

#[test]
fn positive_wide_saturation_caps_only_the_upper_bound() {
    for (left, right, expected) in [(32760i16, 10i16, 0x7FFF), (-1, 0, 0xFFFF), (i16::MIN, -1, 0x7FFF)] {
        let [lo, hi] = right.to_le_bytes();
        let body = [word(6, 20, left as u16), vec![0x77, 0x60, 20, lo, hi]].concat();
        assert_eq!(after(&run(&body, &state(), &FieldScratch::unknown()).unwrap()).read_u16(6, 20), Ok(expected));
    }
    for (left, expected) in [(i16::MAX, 0x7FFF), (-1, 0), (i16::MIN, 0x8001)] {
        let body = [word(6, 20, left as u16), vec![0x7B, 6, 20]].concat();
        assert_eq!(after(&run(&body, &state(), &FieldScratch::unknown()).unwrap()).read_u16(6, 20), Ok(expected));
    }
    let legacy = legacy_run(&[word(6, 20, 32760), vec![0x77, 0x60, 20, 10, 0],
        vec![0x81, 0x46, 80, 20, 0]].concat(), &state()).unwrap();
    assert_eq!(story_word(&story_after(&state(), &legacy), 4, 80), Some(32770), "legacy upper cap remains 65535");
}

#[test]
fn native_minus_and_decrement_saturation_use_the_confirmed_positive_32768_floor() {
    for (left, right, expected) in [(1i16, 1i16, 0x8000), (10000, -30000, 0x9C40),
        (i16::MIN, 1, 0x8000), (i16::MAX, i16::MIN, 0xFFFF)] {
        let [lo, hi] = right.to_le_bytes();
        let body = [word(6, 20, left as u16), vec![0x79, 0x60, 20, lo, hi]].concat();
        assert_eq!(after(&run(&body, &state(), &FieldScratch::unknown()).unwrap()).read_u16(6, 20), Ok(expected));
    }
    for left in [i16::MIN, -1, 0, i16::MAX] {
        let body = [word(6, 20, left as u16), vec![0x7D, 6, 20]].concat();
        assert_eq!(after(&run(&body, &state(), &FieldScratch::unknown()).unwrap()).read_u16(6, 20), Ok(0x8000));
    }
    let mut before = state();
    before.write_word(15, 80, 0xAB01).unwrap();
    for operation in [vec![0x79, 0xF0, 80, 1, 0], vec![0x7D, 15, 80]] {
        let result = run(&operation, &before, &FieldScratch::unknown()).unwrap();
        assert_eq!(story_word(&story_after(&before, &result), 7, 80), Some(0xAB00),
            "selector15 stores the low byte of32768 and preserves its physical neighbour");
    }
    let legacy = legacy_run(&[word(6, 20, 1), vec![0x79, 0x60, 20, 1, 0],
        vec![0x81, 0x46, 80, 20, 0]].concat(), &state()).unwrap();
    assert_eq!(story_word(&story_after(&state(), &legacy), 4, 80), Some(0), "the old API keeps conventional unsigned saturation");
}

#[test]
fn native_party_operands_and_copies_use_the_checked_scratch_image() {
    let mut story = state().into_native_party().unwrap();
    *story.native_party_mut().unwrap() = NativePartyState::from_parts([0, 255, 255], 7, (1, 1), (0, 0)).unwrap();
    let body = [byte(5, 20, 2), byte(6, 21, 0), byte(5, 22, 254),
        vec![0x0A, 0x56, 0x50, 20, 21, 22], vec![0x0B, 0x65, 0x60, 30, 31, 32]].concat();
    let result = run(&body, &story, &FieldScratch::unknown()).unwrap();
    assert_eq!([30, 31, 32].map(|address| after(&result).read_u8(5, address)), [Ok(2), Ok(0), Ok(255)]);
    let native = result.native.as_ref().unwrap().after();
    assert_eq!(native.native_party().unwrap().known_main(), Some([0, 2, 255]));
    assert_eq!([9, 10, 11].map(|address| native.known_byte(3, address)), [Some(2), Some(0), Some(255)]);
    assert!(result.writes.iter().all(|write| !matches!(write.bank, 5 | 6)));
    assert!(run(&[0x0A, 0x50, 0, 20, 254, 254], &story, &FieldScratch::unknown()).is_err());
}

#[test]
fn battle_keeps_its_prefix_scratch_while_the_successful_suffix_has_a_separate_afterimage() {
    let mut story = state().into_native_party().unwrap();
    *story.native_party_mut().unwrap() = NativePartyState::from_parts([0, 255, 255], 7, (1, 1), (0, 0)).unwrap();
    let scratch = FieldScratch::unknown();
    let body = [byte(5, 20, 1), word(6, 40, 300), byte(3, 70, 13), vec![0x70, 6, 40, 0],
        byte(6, 20, 2), byte(3, 70, 22)].concat();
    let bytes = section(&[], &[], &body);
    let result = run_script_exact_with_scratch(&FieldScript::parse(&bytes).unwrap(), &story, &scratch, 7,
        &ExactScript { entity: 0, slot: 1, after_battle: Some(300), ..ExactScript::default() }, &[]).unwrap();
    assert_eq!(result.battle, Some(ExactBattle { formation: 300, flags: 0 }));
    assert_eq!(result.battle_scratch.as_ref().unwrap().read_u8(6, 20), Ok(1));
    assert_eq!(after(&result).read_u8(5, 20), Ok(2));
    assert_eq!(result.battle_story.as_ref().unwrap().known_byte(3, 70), Some(13));
    assert_eq!(result.native.as_ref().unwrap().after().known_byte(3, 70), Some(22));
    assert_eq!(result.scratch.as_ref().unwrap().before(), &scratch);
    assert_eq!(scratch, FieldScratch::unknown());
    assert_eq!(story.known_byte(3, 70), None);
}

#[test]
fn an_explicit_answer_establishes_an_unknown_cursor_without_reading_it() {
    let mut question = dialog("Yes");
    question.pop();
    question.push(0xE7);
    question.extend(dialog("No"));
    let bytes = SynthScript { dialogs: vec![question], ..Default::default() }.entity("npc", vec![
        (0, vec![0, 0]),
        (1, [vec![0x48, 5, 0, 0, 0, 1, 19], copy_byte(3, 70, 6, 19), vec![0]].concat()),
    ]).build();
    let result = run_talk_exact_with_scratch(&FieldScript::parse(&bytes).unwrap(), 0, &state(),
        &FieldScratch::unknown(), 7, &[1]).unwrap();
    assert_eq!(after(&result).read_u8(5, 19), Ok(1));
    assert_eq!(writes(&result), vec![(3, 70, 1, 255)]);
}
