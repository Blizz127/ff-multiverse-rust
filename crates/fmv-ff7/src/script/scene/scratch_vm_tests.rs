//! Opt-in checked field scratch: no entry reset, guessed zero or implicit Given value.

use super::*;
use crate::script::dialogue::End;
use crate::script::synth::SynthScript;
use crate::script::{FieldScratch, FieldScratchError, FieldScratchOutcome, FieldScript};
use crate::story::StoryState;

fn story() -> StoryState { StoryState::at(33).into_extended().unwrap() }
fn code(body: Vec<Vec<u8>>) -> Vec<u8> { [body.concat(), vec![0]].concat() }
fn section(body: Vec<Vec<u8>>) -> Vec<u8> {
    SynthScript::default().entity("a", vec![(1, code(body))]).build()
}
fn vm(story: StoryState, scratch: FieldScratch) -> Vm {
    Vm::with_resources(SceneVars::with_scratch(story, &[], scratch).unwrap(), 9, Limits::default(), SceneResources { gil: Some(50) })
}
fn run_with(story: StoryState, scratch: FieldScratch, body: Vec<Vec<u8>>) -> (Then, Vm) {
    let section = section(body);
    let script = FieldScript::parse(&section).unwrap();
    let mut vm = vm(story, scratch);
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    let beat = vm.resume(&script, None);
    (beat.then, vm)
}
fn run(scratch: FieldScratch, body: Vec<Vec<u8>>) -> (Then, Vm) { run_with(story(), scratch, body) }
fn set(bank: u8, address: u8, value: u8) -> Vec<u8> { vec![0x80, bank << 4, address, value] }
fn wide(op: u8, bank: u8, address: u8, value: u16) -> Vec<u8> {
    let [lo, hi] = value.to_le_bytes();
    vec![op, bank << 4, address, lo, hi]
}
fn word(bank: u8, address: u8, value: u16) -> Vec<u8> { wide(0x81, bank, address, value) }
fn copy_byte(to: u8, at: u8, from: u8, address: u8) -> Vec<u8> { vec![0x80, (to << 4) | from, at, address] }
fn copy_word(to: u8, at: u8, from: u8, address: u8) -> Vec<u8> { vec![0x81, (to << 4) | from, at, address, 0] }
fn outcome(vm: &Vm) -> FieldScratchOutcome {
    assert!(vm.scratch_valid(), "{:?}", vm.scratch_outcome());
    assert!(vm.stats().unknown.is_empty(), "{:?}", vm.stats());
    assert_eq!(vm.stats().unresolved, 0);
    vm.scratch_outcome().unwrap().unwrap()
}
fn finished(then: &Then) { assert!(matches!(then, Then::End { how: End::Finished, .. }), "{then:?}"); }

#[test]
fn checked_scratch_rejects_legacy_stories_and_given_overrides_even_after_constructor_mutation() {
    use crate::script::dialogue::StoryVars;
    let checked = SceneVars::with_scratch(story(), &[], FieldScratch::cleared_for_entry()).unwrap();
    assert!(!checked.execution_supported(), "the permissive walker cannot consume a checked visit without its outcome contract");
    assert!(SceneVars::new(story(), &[]).execution_supported());
    assert!(matches!(SceneVars::with_scratch(StoryState::at(1), &[], FieldScratch::unknown()), Err(SceneError::ScratchStory)));
    let mut changed_story = checked;
    changed_story.story = StoryState::at(1);
    assert_eq!(Vm::new(changed_story, 1, Limits::default()).scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome)));
    for bank in [5, 6] {
        let given = Given { bank, address: 31, value: 1 };
        assert!(matches!(SceneVars::with_scratch(story(), &[given], FieldScratch::unknown()), Err(SceneError::ScratchGiven { bank: b, address: 31 }) if b == bank));
        let mut vars = SceneVars::with_scratch(story(), &[], FieldScratch::unknown()).unwrap();
        vars.given.push(given);
        let vm = Vm::new(vars, 1, Limits::default());
        assert!(!vm.scratch_valid());
        assert_eq!(vm.scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome)));
    }
}

#[test]
fn unknown_reads_fail_before_destination_writes_and_cannot_be_repaired_by_later_writes() {
    let initial = FieldScratch::unknown();
    let section = section(vec![copy_byte(1, 60, 5, 31), set(5, 31, 0)]);
    let script = FieldScript::parse(&section).unwrap();
    let mut vm = vm(story(), initial.clone());
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Broken, .. }));
    assert!(vm.writes().is_empty());
    assert_eq!(vm.scratch_current(), Some(&initial));
    let fault = vm.scratch_outcome();
    assert!(matches!(fault, Some(Err(FieldScratchError::Unknown { address: 31, missing_mask: 255 }))));
    vm.take_stats();
    vm.resume_battle(&story(), SceneResources { gil: Some(100) });
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Broken, .. }));
    assert_eq!(vm.scratch_outcome(), fault);
}

#[test]
fn unknown_persistent_source_cannot_initialize_checked_scratch_from_zero() {
    let initial = FieldScratch::unknown();
    for instruction in [copy_byte(5, 31, 1, 60), copy_word(6, 31, 2, 60)] {
        let (then, vm) = run(initial.clone(), vec![instruction]);
        assert!(matches!(then, Then::End { how: End::Broken, .. }));
        assert_eq!(vm.scratch_current(), Some(&initial));
        assert_eq!(vm.scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome)));
    }
}

#[test]
fn explicit_zero_establishes_only_assigned_bytes_and_keeps_scratch_out_of_story_journal() {
    let initial = FieldScratch::unknown();
    let (then, vm) = run_with(StoryState::fresh_native_first_control(), initial.clone(), vec![set(5, 31, 0), copy_byte(1, 60, 6, 31)]);
    finished(&then);
    let out = outcome(&vm);
    assert_eq!(out.before(), &initial);
    assert_eq!(out.after().read_u8(6, 31), Ok(0));
    assert_eq!(out.assigned_masks()[31], 255);
    assert_eq!(out.after().known_masks()[30], 0);
    assert_eq!(vm.native_journal().unwrap().events().len(), 1, "only the authored persistent copy is a story event");
    let mut saved = initial.clone();
    assert!(out.apply_to(&mut saved, &initial).unwrap());
    assert_eq!(&saved, out.after());
}

#[test]
fn checked_wide_scratch_selectors_alias_but_five_is_one_unsigned_byte() {
    let (then, vm) = run(FieldScratch::unknown(), vec![
        word(6, 20, 0x80FF), copy_word(6, 30, 5, 20), copy_word(6, 32, 6, 20),
        word(5, 20, 0xFFFE), word(6, 21, 0x1234), word(5, 255, 0x1234), copy_word(6, 40, 5, 255),
    ]);
    finished(&then);
    let out = outcome(&vm);
    assert_eq!(out.after().read_u16(6, 30), Ok(255));
    assert_eq!(out.after().read_i16(6, 32), Ok(-32513));
    assert_eq!(&out.after().values()[20..23], &[0xFE, 0x34, 0x12]);
    assert_eq!(out.after().read_u16(5, 255), Ok(0x34));
    assert_eq!(out.after().read_u16(6, 40), Ok(0x34));
    assert_eq!(out.assigned_masks()[254], 0);
}

#[test]
fn checked_wide_persistent_selectors_keep_their_original_width_before_aliasing() {
    for (selector, bytes) in [(1, 1), (3, 1), (11, 1), (13, 1), (15, 1), (2, 2), (4, 2), (7, 2), (12, 2), (14, 2)] {
        let mut before = story();
        before.write_byte(selector, 80, 0xFF).unwrap();
        before.write_byte(selector, 81, 0x80).unwrap();
        let (then, vm) = run_with(before, FieldScratch::unknown(), vec![copy_word(6, 20, selector, 80), word(selector, 82, 0x1234)]);
        finished(&then);
        assert_eq!(outcome(&vm).after().read_u16(6, 20), Ok(if bytes == 1 { 0xFF } else { 0x80FF }));
        let key = crate::story::canonical_persistent_bank(selector).unwrap();
        assert_eq!(vm.writes().get(&(key, 82)), Some(&0x34));
        assert_eq!(vm.writes().get(&(key, 83)), if bytes == 1 { None } else { Some(&0x12) });
    }
}

#[test]
fn checked_hbyte_observes_selector_width_and_only_requires_the_retained_bits() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_u8(5, 21, 0xAB).unwrap();
    let (then, vm) = run(scratch, vec![vec![0x9B, 0x65, 30, 255, 0], vec![0x9B, 0x66, 31, 20, 0], vec![0x9B, 0x63, 32, 80, 0]]);
    finished(&then);
    let out = outcome(&vm);
    assert_eq!(&out.after().values()[30..33], &[0, 0xAB, 0]);
    assert_eq!(out.after().known_masks()[20], 0, "discarded low byte was not read or initialized");
    assert!(vm.vars().unknown_persistent_reads().is_empty(), "byte-wide persistent HBYTE has constant zero high bits");
}

#[test]
fn checked_bit_reads_and_assignments_preserve_unknown_neighbors() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_masked_u8(5, 20, 4, 4).unwrap();
    scratch.write_masked_u8(5, 22, 2, 2).unwrap();
    let body1 = set(5, 30, 7);
    let body2 = set(5, 31, 8);
    let guard1 = [vec![0x14, 0x50, 20, 2, 9, (body1.len() + 1) as u8], body1].concat();
    let guard2 = [vec![0x16, 0x60, 21, 0, 9, 0, 9, (body2.len() + 1) as u8], body2].concat();
    let (then, vm) = run(scratch, vec![guard1, guard2, vec![0x82, 0x60, 40, 1], vec![0x83, 0x50, 40, 2], vec![0x84, 0x50, 40, 1]]);
    finished(&then);
    let out = outcome(&vm);
    assert_eq!(&out.after().values()[30..32], &[7, 8]);
    assert_eq!((out.after().values()[40], out.after().known_masks()[40], out.assigned_masks()[40]), (0, 6, 6));
}

#[test]
fn checked_word_and_bit_overflows_fault_atomically_and_stay_faulted() {
    for instruction in [word(6, 255, 0x1234), copy_word(6, 40, 6, 255), vec![0x9B, 0x56, 40, 255, 0], word(2, 255, 0x1234)] {
        let initial = FieldScratch::cleared_for_entry();
        let (then, mut vm) = run(initial.clone(), vec![instruction]);
        assert!(matches!(then, Then::End { how: End::Broken, .. }));
        assert_eq!(vm.scratch_current(), Some(&initial));
        assert!(matches!(vm.scratch_outcome(), Some(Err(FieldScratchError::OutOfBounds { .. }))));
        vm.take_stats();
        assert!(!vm.scratch_valid());
    }
    let mut before = story();
    before.write_byte(1, 255, 1).unwrap();
    let guard = [vec![0x16, 0x20, 255, 0, 0, 0, 9, 5], set(5, 40, 1)].concat();
    let (then, vm) = run_with(before, FieldScratch::cleared_for_entry(), vec![guard]);
    assert!(matches!(then, Then::End { how: End::Broken, .. }));
    assert!(matches!(vm.scratch_outcome(), Some(Err(FieldScratchError::OutOfBounds { address: 255, bytes: 2 }))));
    assert_eq!(vm.scratch_current().unwrap().values()[40], 0, "even a low-bit wide guard validates the whole word's bounds");
}

#[test]
fn checked_bit_instructions_refuse_unreviewed_indices_without_touching_neighbor_bytes() {
    for op in [0x82, 0x83, 0x84] {
        for bank in [1, 2, 5, 6] {
            for address in [20, 255] {
                let initial = FieldScratch::cleared_for_entry();
                let (then, mut vm) = run(initial.clone(), vec![vec![op, bank << 4, address, 8]]);
                assert!(matches!(then, Then::End { how: End::Broken, .. }));
                assert_eq!(vm.scratch_current(), Some(&initial), "bit8 must not address the next byte");
                assert!(vm.writes().is_empty(), "persistent and scratch destinations are both untouched");
                assert_eq!(vm.scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome)));
                vm.take_stats();
                assert!(!vm.scratch_valid());
            }
        }
    }
}

#[test]
fn checked_invalid_selectors_do_not_create_extra_vm_banks() {
    for selector in [8, 9, 10] {
        for instruction in [copy_byte(5, 30, selector, 60), copy_word(6, 30, selector, 60), set(selector, 60, 2), word(selector, 60, 2)] {
            let initial = FieldScratch::unknown();
            let (then, vm) = run(initial.clone(), vec![instruction]);
            assert!(matches!(then, Then::End { how: End::Broken, .. }));
            assert_eq!(vm.scratch_current(), Some(&initial));
            assert!(vm.writes().is_empty());
            assert!(matches!(vm.scratch_outcome(), Some(Err(FieldScratchError::InvalidSelector(s))) if s == selector));
        }
    }
}

#[test]
fn checked_gil_reads_respect_known_scratch_width_and_preflight_both_outputs() {
    let mut scratch = FieldScratch::unknown();
    scratch.write_u8(5, 20, 2).unwrap();
    let (then, vm) = run(scratch, vec![vec![0x39, 0x50, 20, 0, 0, 0]]);
    finished(&then);
    assert_eq!(vm.resources().gil_after, Some(52));
    outcome(&vm);
    let (then, vm) = run(FieldScratch::unknown(), vec![vec![0x39, 0x60, 20, 0, 0, 0]]);
    assert!(matches!(then, Then::End { how: End::Broken, .. }));
    assert!(vm.resources().unknown_gil);
    let initial = FieldScratch::unknown();
    let (then, vm) = run(initial.clone(), vec![vec![0x3B, 0x56, 30, 255]]);
    assert!(matches!(then, Then::End { how: End::Broken, .. }));
    assert_eq!(vm.scratch_current(), Some(&initial), "valid low output was not written before invalid high output");
}

#[test]
fn native_getpc_gtpye_and_sptye_use_the_checked_scratch_path() {
    let native = StoryState::fresh_native_first_control();
    let (then, vm) = run_with(native.clone(), FieldScratch::unknown(), vec![vec![0x74, 5, 0, 30], vec![0x0B, 0x56, 0x50, 31, 32, 33]]);
    finished(&then);
    let out = outcome(&vm);
    assert_eq!(&out.after().values()[30..34], &[0, 0, 255, 255]);
    assert_eq!(&out.assigned_masks()[30..34], &[255; 4]);
    let (then, vm) = run_with(native.clone(), FieldScratch::unknown(), vec![vec![0x0A, 0x50, 0, 30, 254, 254]]);
    assert!(matches!(then, Then::End { how: End::Broken, .. }));
    assert_eq!(vm.current_persistent_story(), Some(&native));
}

#[test]
fn malformed_checked_party_and_bit_destinations_remain_refused_after_stats_reset() {
    for state in [story(), StoryState::fresh_native_first_control()] {
        for instruction in [vec![0x74, 0, 0, 30], vec![0x82, 0, 30, 0]] {
            let initial = FieldScratch::unknown();
            let (then, mut vm) = run_with(state.clone(), initial.clone(), vec![instruction]);
            assert!(matches!(then, Then::End { how: End::Broken, .. }));
            assert_eq!(vm.scratch_current(), Some(&initial));
            vm.take_stats();
            assert_eq!(vm.scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome)));
        }
    }
    let initial = FieldScratch::unknown();
    let (then, mut vm) = run_with(StoryState::fresh_native_first_control(), initial.clone(), vec![vec![0x0B, 0x58, 0x50, 30, 31, 32]]);
    assert!(matches!(then, Then::End { how: End::Broken, .. }));
    assert_eq!(vm.scratch_current(), Some(&initial), "a later invalid GTPYE output publishes no earlier staged write");
    vm.take_stats();
    assert_eq!(vm.scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome)));
}

#[test]
fn checked_unknown_instruction_cannot_be_laundered_through_a_new_run() {
    let (then, mut vm) = run(FieldScratch::cleared_for_entry(), vec![vec![0x7F, 0, 5]]);
    assert!(matches!(then, Then::End { how: End::Broken, .. }));
    assert!(!vm.take_stats().unknown.is_empty());
    let section = section(vec![set(5, 31, 1)]);
    let script = FieldScript::parse(&section).unwrap();
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Broken, .. }));
    assert_eq!(vm.scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome)));
    assert_eq!(vm.scratch_current().unwrap().read_u8(5, 31), Ok(0));
}

#[test]
fn checked_ask_cursor_unknown_is_not_repaired_by_an_answer() {
    let section = SynthScript { dialogs: vec![vec![0x10, 0xE7, 0x11, 0xFF]], ..Default::default() }
        .entity("a", vec![(1, vec![0x48, 5, 0, 0, 0, 1, 31, 0])]).build();
    let script = FieldScript::parse(&section).unwrap();
    let initial = FieldScratch::unknown();
    let mut vm = vm(story(), initial.clone());
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Broken, .. }));
    assert!(matches!(vm.resume(&script, Some(1)).then, Then::End { how: End::Broken, .. }));
    assert_eq!(vm.scratch_current(), Some(&initial));
}

#[test]
fn checked_setup_restoring_a_temporary_change_is_still_rejected_after_stats_reset() {
    let section = SynthScript::default().entity("a", vec![(0, code(vec![set(5, 31, 1), set(6, 31, 0)])), (1, vec![0])]).build();
    let script = FieldScript::parse(&section).unwrap();
    let initial = FieldScratch::cleared_for_entry();
    let mut vm = vm(story(), initial.clone());
    vm.setup(&script, &[ScriptRef::new(0, 0)]);
    assert_eq!(vm.scratch_current(), Some(&initial));
    assert!(vm.scratch_setup_changed());
    vm.take_stats();
    assert_eq!(vm.scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome)));
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Broken, .. }));
}

#[test]
fn checked_signed_word_arithmetic_matches_native_wrap_and_pc_saturating_boundaries() {
    for (op, before, argument, expected) in [
        (0x8C, -7i16, 3i16, -2i16), (0x8E, -7, 3, -1),
        (0x8C, i16::MIN, -1, i16::MIN), (0x8E, i16::MIN, -1, 0),
        (0x77, 32760, 20, 32767), (0x77, -5, 1, -4),
        (0x79, 10, 1, i16::MIN), (0x79, 32767, -32768, -1),
    ] {
        let mut scratch = FieldScratch::unknown();
        scratch.write_i16(6, 20, before).unwrap();
        let (then, vm) = run(scratch, vec![wide(op, 6, 20, argument as u16)]);
        finished(&then);
        assert_eq!(outcome(&vm).after().read_i16(6, 20), Ok(expected), "opcode{op:#x}: {before}, {argument}");
    }
    for (op, initial, expected) in [(0x7B, -5i16, -4i16), (0x7B, 32767, 32767), (0x7D, -32768, -32768), (0x7D, 20, -32768)] {
        let mut scratch = FieldScratch::unknown();
        scratch.write_i16(6, 20, initial).unwrap();
        let (then, vm) = run(scratch, vec![vec![op, 6, 20]]);
        finished(&then);
        assert_eq!(outcome(&vm).after().read_i16(6, 20), Ok(expected));
    }
}

#[test]
fn checked_division_by_zero_is_an_explicit_sticky_port_refusal() {
    for op in [0x8C, 0x8E] {
        let mut initial = FieldScratch::unknown();
        initial.write_i16(6, 20, -7).unwrap();
        let (then, mut vm) = run(initial.clone(), vec![wide(op, 6, 20, 0)]);
        assert!(matches!(then, Then::End { how: End::Broken, .. }));
        assert_eq!(vm.scratch_current(), Some(&initial));
        vm.take_stats();
        assert_eq!(vm.scratch_outcome(), Some(Err(FieldScratchError::InvalidOutcome)));
    }
}

#[test]
fn unchecked_constructors_keep_the_previous_zero_and_two_byte_preview_behavior() {
    let section = section(vec![word(5, 20, 0x1234), copy_byte(1, 60, 5, 30)]);
    let script = FieldScript::parse(&section).unwrap();
    let mut vm = Vm::new(SceneVars::new(story(), &[]), 9, Limits::default());
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    finished(&vm.resume(&script, None).then);
    assert_eq!(vm.writes().get(&(5, 20)), Some(&0x34));
    assert_eq!(vm.writes().get(&(5, 21)), Some(&0x12));
    assert_eq!(vm.writes().get(&(1, 60)), Some(&0));
    assert!(!vm.checked_scratch());
    assert!(vm.scratch_outcome().is_none());
}
