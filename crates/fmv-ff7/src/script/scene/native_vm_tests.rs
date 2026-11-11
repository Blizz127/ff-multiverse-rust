//! Synthetic opcode tests for the independent native main/raw/PHS state.

use super::*;
use crate::script::dialogue::{End, MessageOrigin};
use crate::script::synth::SynthScript;
use crate::script::FieldScript;
use crate::story::{NativePartyOp, NativePartyState, NativeStoryEvent, StoryState};

fn state(main: [u8; 3], raw: [u8; 3]) -> StoryState {
    let mut story = StoryState::at(33).into_native_party().unwrap();
    *story.native_party_mut().unwrap() = NativePartyState::from_parts(main, 7, (3, 0xFFFF), (0x8001, 0xFFFF)).unwrap();
    for (address, value) in (9..=11).zip(raw) { story.write_byte(3, address, value).unwrap(); }
    story
}

fn section(body: Vec<Vec<u8>>, dialogs: Vec<Vec<u8>>) -> Vec<u8> {
    let code = [body.concat(), vec![0]].concat();
    SynthScript { dialogs, ..Default::default() }.entity("a", vec![(1, code)]).build()
}

fn run(story: StoryState, body: Vec<Vec<u8>>) -> Vm { run_dialogs(story, body, vec![]).1 }

fn run_dialogs(story: StoryState, body: Vec<Vec<u8>>, dialogs: Vec<Vec<u8>>) -> (Vec<Beat>, Vm) {
    let section = section(body, dialogs);
    let script = FieldScript::parse(&section).unwrap();
    let mut vm = Vm::new(SceneVars::new(story, &[]), 7, Limits::default());
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    let mut beats = Vec::new();
    for _ in 0..100 {
        let beat = vm.resume(&script, None);
        let done = matches!(beat.then, Then::End { .. });
        beats.push(beat);
        if done { return (beats, vm); }
    }
    panic!("synthetic native scene did not terminate");
}

fn set(bank: u8, address: u8, value: u8) -> Vec<u8> { vec![0x80, bank << 4, address, value] }
fn word(bank: u8, address: u8, value: u16) -> Vec<u8> {
    let [lo, hi] = value.to_le_bytes();
    vec![0x81, bank << 4, address, lo, hi]
}
fn getpc(slot: u8, address: u8) -> Vec<u8> { vec![0x74, 5, slot, address] }
fn condition(op: u8, member: u8, dialog: u8) -> Vec<u8> { vec![op, member, 4, 0x40, 0, dialog] }
fn complete(vm: &Vm) {
    assert!(vm.stats().unknown.is_empty(), "{:?}", vm.stats());
    assert_eq!(vm.stats().unresolved, 0, "{:?}", vm.stats());
    assert!(vm.vars().unknown_persistent_reads().is_empty());
}
fn dialogs(beats: &[Beat]) -> Vec<usize> {
    beats.iter().filter_map(|beat| match &beat.then {
        Then::Window(w) => match w.message.origin { MessageOrigin::Canon { dialog, .. } => Some(dialog), _ => None },
        _ => None,
    }).collect()
}

#[test]
fn native_queries_distinguish_main_raw_and_available_members_and_keep_holes() {
    let mut story = state([255, 0, 1], [2, 255, 0]);
    story.native_party_mut().unwrap().write_availability_masked(1 << 3, 0xFFFF);
    let (beats, vm) = run_dialogs(story, vec![
        getpc(0, 30), getpc(1, 31), getpc(2, 32), getpc(3, 33),
        condition(0xCB, 2, 0), condition(0xCB, 1, 1),
        condition(0xCC, 3, 2), condition(0xCC, 0, 3),
    ], vec![vec![0xFF]; 4]);
    complete(&vm);
    assert_eq!(dialogs(&beats), [0, 2]);
    for (address, value) in [(30, 255), (31, 0), (32, 1)] { assert_eq!(vm.writes().get(&(5, address)), Some(&value)); }
    assert!(!vm.writes().contains_key(&(5, 33)), "native out-of-range GETPC makes no assignment");
    assert!(!vm.native_journal().unwrap().changed());
}

#[test]
fn unknown_native_reads_never_become_empty_main_or_false_availability() {
    let mut story = StoryState::at(33).into_native_party().unwrap();
    for (address, value) in (9..=11).zip([0, 255, 255]) { story.write_byte(3, address, value).unwrap(); }
    let vm = run(story.clone(), vec![getpc(0, 30)]);
    assert!(vm.stats().unresolved > 0);
    assert!(!vm.writes().contains_key(&(5, 30)));
    let vm = run(story.clone(), vec![condition(0xCC, 0, 0)]);
    assert!(vm.stats().unresolved > 0);
    assert_eq!(vm.native_journal().unwrap().after(), &story);
}

#[test]
fn native_getpc_refuses_invalid_store_selectors_without_inventing_vm_variables() {
    for bank in [0, 8, 9, 10] {
        let before = state([2, 255, 255], [2, 255, 255]);
        let vm = run(before.clone(), vec![vec![0x74, bank, 0, 30]]);
        assert!(vm.stats().unresolved > 0, "invalid byte-store selector {bank}");
        assert!(vm.writes().is_empty());
        assert_eq!(vm.current_persistent_story(), Some(&before));
    }
}

#[test]
fn native_party_effects_preserve_holes_visibility_and_independent_locks() {
    let before = state([0, 1, 255], [0, 1, 255]);
    let vm = run(before.clone(), vec![vec![0xC9, 0], vec![0xC8, 2], vec![0xCE, 2], vec![0xCF, 0]]);
    complete(&vm);
    let journal = vm.native_journal().unwrap();
    let after = journal.after();
    assert_eq!(after.native_party().unwrap().known_main(), Some([2, 1, 255]));
    assert_eq!([9, 10, 11].map(|address| after.var(3, address)), [2, 1, 255]);
    assert_eq!(after.native_party().unwrap().availability_bits(), (7, 0xFFFF));
    assert_eq!(after.native_party().unwrap().locking_bits(), (0x8004, 0xFFFF));
    assert_eq!(journal.events().iter().filter(|event| matches!(event, NativeStoryEvent::Party(_))).count(), 4);
    assert_eq!(journal.events().len(), 4, "native raw assignments are not appended twice");
    let mut replayed = before.clone();
    assert!(journal.apply_to(&mut replayed, &before).unwrap());
    assert_eq!(&replayed, after);
}

#[test]
fn mmbud_removes_raw_members_without_syncing_main_or_touching_locks() {
    let before = state([0, 1, 255], [1, 0, 255]);
    let vm = run(before, vec![vec![0xCD, 0, 1], getpc(1, 30), vec![0xCD, 7, 2]]);
    complete(&vm);
    let after = vm.current_persistent_story().unwrap();
    assert_eq!([9, 10, 11].map(|address| after.var(3, address)), [255, 0, 255]);
    assert_eq!(after.native_party().unwrap().known_main(), Some([0, 1, 255]));
    assert_eq!(vm.writes().get(&(5, 30)), Some(&1));
    assert_eq!(after.native_party().unwrap().availability_bits(), (5, 0xFFFF));
    assert_eq!(after.native_party().unwrap().locking_bits(), (0x8001, 0xFFFF));
}

#[test]
fn raw_direct_assignments_and_native_effects_replay_in_their_actual_order() {
    let before = state([0, 1, 255], [0, 1, 255]);
    let vm = run(before.clone(), vec![
        set(4, 9, 2), vec![0xC9, 1], set(3, 9, 0),
        word(4, 8, 0x0277), vec![0x82, 0x30, 10, 0],
    ]);
    complete(&vm);
    let journal = vm.native_journal().unwrap();
    assert_eq!(journal.events().len(), 6);
    assert!(matches!(journal.events()[0], NativeStoryEvent::Assignment(write) if write.key() == (3, 9)));
    assert!(matches!(journal.events()[1], NativeStoryEvent::Party(effect) if effect.operation() == NativePartyOp::Remove(1)));
    assert_eq!(journal.after().native_party().unwrap().known_main(), Some([2, 255, 255]));
    assert_eq!([8, 9, 10, 11].map(|address| journal.after().var(3, address)), [0x77, 2, 255, 255]);
    let mut replayed = before.clone();
    journal.apply_to(&mut replayed, &before).unwrap();
    assert_eq!(&replayed, journal.after());
}

#[test]
fn full_prtye_overwrites_unknown_raw_but_does_not_invent_unknown_main() {
    let mut before = StoryState::at(33).into_native_party().unwrap();
    *before.native_party_mut().unwrap() = NativePartyState::from_parts([255, 0, 255], 7, (0, 0), (0, 0)).unwrap();
    let vm = run(before, vec![vec![0xCA, 0, 2, 254]]);
    complete(&vm);
    let after = vm.current_persistent_story().unwrap();
    assert_eq!(after.native_party().unwrap().known_main(), Some([2, 0, 255]));
    assert_eq!([9, 10, 11].map(|address| after.known_byte(3, address)), [Some(0), Some(2), Some(255)]);
    assert_eq!(after.native_party().unwrap().availability_bits(), (5, 5));
    let unknown = StoryState::at(33).into_native_party().unwrap();
    let vm = run(unknown.clone(), vec![vec![0xCA, 0, 2, 254]]);
    assert!(vm.stats().unresolved > 0);
    assert_eq!(vm.current_persistent_story(), Some(&unknown));
}

#[test]
fn masked_raw_assignments_keep_unknown_neighbors_and_dependent_party_reads_refuse() {
    let mut before = StoryState::at(33).into_native_party().unwrap();
    *before.native_party_mut().unwrap() = NativePartyState::from_parts([0, 255, 255], 7, (0, 0), (0, 0)).unwrap();
    let vm = run(before.clone(), vec![vec![0x82, 0x40, 9, 1], vec![0xC9, 0], word(4, 255, 0x1234)]);
    assert!(vm.stats().unresolved > 0);
    let journal = vm.native_journal().unwrap();
    assert_eq!(journal.events().len(), 1, "failed party and crossing-word operations publish no assignments");
    assert_eq!((journal.after().var(3, 9), journal.after().known_mask(3, 9)), (2, 2));
    assert_eq!(journal.after().native_party(), before.native_party());
    assert_eq!(journal.after().known_mask(3, 255), 0);
}

#[test]
fn availability_and_lock_assignments_do_not_require_or_invent_unknown_main_and_raw() {
    let before = StoryState::at(33).into_native_party().unwrap();
    let vm = run(before, vec![vec![0xCD, 1, 2], vec![0xCE, 2], vec![0xCF, 0]]);
    complete(&vm);
    let after = vm.current_persistent_story().unwrap();
    assert_eq!(after.native_party().unwrap().main_slots(), ([0; 3], 0));
    assert_eq!(after.native_party().unwrap().availability_bits(), (4, 4));
    assert_eq!(after.native_party().unwrap().locking_bits(), (4, 5));
    assert_eq!([9, 10, 11].map(|address| after.known_mask(3, address)), [0; 3]);
}

#[test]
fn sptye_snapshots_all_inputs_before_replacing_aliasing_raw_slots() {
    let before = state([0, 1, 255], [0, 1, 255]);
    // Operand one reads raw10; operand two reads raw9. A write during the first read would
    // corrupt the second. The final literal254 forces an empty final raw slot.
    let vm = run(before, vec![vec![0x0A, 0x33, 0, 10, 9, 254]]);
    complete(&vm);
    let after = vm.current_persistent_story().unwrap();
    assert_eq!([9, 10, 11].map(|address| after.var(3, address)), [1, 0, 255]);
    assert_eq!(after.native_party().unwrap().known_main(), Some([0, 1, 255]));
}

#[test]
fn sptye_unknown_operand_does_not_publish_a_preview_based_party() {
    let before = state([0, 1, 255], [0, 1, 255]);
    let vm = run(before.clone(), vec![vec![0x0A, 0x10, 0, 60, 2, 254]]);
    assert!(vm.stats().unresolved > 0);
    assert_eq!(vm.current_persistent_story(), Some(&before));
    assert!(vm.native_journal().unwrap().events().is_empty());
}

#[test]
fn gtpye_reads_raw_sequentially_and_aliasing_stores_affect_later_reads() {
    let before = state([0, 1, 2], [0, 1, 2]);
    // raw0→raw1; now raw1 is0, so raw1→raw2 writes0; final raw2→scratch is also0.
    let vm = run(before, vec![vec![0x0B, 0x34, 0x50, 10, 11, 30]]);
    complete(&vm);
    let after = vm.current_persistent_story().unwrap();
    assert_eq!([9, 10, 11].map(|address| after.var(3, address)), [0, 0, 0]);
    assert_eq!(after.native_party().unwrap().known_main(), Some([0, 1, 2]));
    assert_eq!(vm.writes().get(&(5, 30)), Some(&0));
    assert_eq!(vm.native_journal().unwrap().events().len(), 2);
}

#[test]
fn gtpye_stages_all_outputs_when_a_later_raw_input_is_unknown() {
    let mut before = StoryState::at(33).into_native_party().unwrap();
    before.write_byte(3, 9, 0).unwrap();
    let vm = run(before.clone(), vec![vec![0x0B, 0x15, 0x50, 60, 30, 31]]);
    assert!(vm.stats().unresolved > 0);
    assert_eq!(vm.current_persistent_story(), Some(&before));
    assert!(vm.writes().is_empty());
    assert!(vm.native_journal().unwrap().events().is_empty());
}

#[test]
fn native_message_substitutions_read_the_current_main_after_party_operations() {
    let before = state([0, 1, 255], [0, 1, 255]);
    let (beats, vm) = run_dialogs(before, vec![vec![0xC9, 0], vec![0xC8, 2], vec![0x40, 0, 0]], vec![vec![0xF3, 0xE7, 0xF4, 0xFF]]);
    complete(&vm);
    let window = beats.iter().find_map(|beat| match &beat.then { Then::Window(window) => Some(window), _ => None }).unwrap();
    assert_eq!(window.message.page_lines(), vec![vec!["Tifa".to_string(), "Barret".to_string()]]);
    let unknown = StoryState::at(33).into_native_party().unwrap();
    let (_, vm) = run_dialogs(unknown, vec![vec![0x40, 0, 0]], vec![vec![0xF3, 0xFF]]);
    assert!(vm.stats().unresolved > 0);
    assert!(vm.has_invalid_message());
}

#[test]
fn only_reviewed_native_naming_returns_apply_field_sync() {
    let before = state([0, 1, 255], [1, 255, 255]);
    let section = section(vec![vec![0x49, 0, 6, 0]], vec![]);
    let script = FieldScript::parse(&section).unwrap();
    let offset = script.span(0, 1).unwrap().start;
    let mut vm = Vm::new(SceneVars::new(before.clone(), &[]), 7, Limits::default());
    vm.allow_default_name_sites(vec![(offset, 0)]);
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Finished, .. }));
    complete(&vm);
    let after = vm.current_persistent_story().unwrap();
    assert_eq!([9, 10, 11].map(|address| after.var(3, address)), [1, 0, 255]);
    assert_eq!(after.native_party(), before.native_party());
    assert!(matches!(vm.native_journal().unwrap().events(), [NativeStoryEvent::Party(effect)] if effect.operation() == NativePartyOp::FieldSync));
    let unreviewed = run(before.clone(), vec![vec![0x49, 0, 6, 0]]);
    assert!(unreviewed.stats().ignored.contains_key("MENU"));
    assert_eq!(unreviewed.current_persistent_story(), Some(&before));
}

#[test]
fn native_rebase_clears_acknowledged_journal_but_preserves_scratch_and_next_instruction() {
    let before = state([0, 1, 255], [0, 1, 255]);
    let section = section(vec![set(5, 40, 77), vec![0xC9, 0], vec![0x72, 8, 0], vec![0x70, 0, 72, 1], getpc(0, 41), vec![0xC8, 2]], vec![]);
    let script = FieldScript::parse(&section).unwrap();
    let mut vm = Vm::new(SceneVars::new(before, &[]), 7, Limits::default());
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Battle, .. }));
    let acknowledged = vm.current_persistent_story().unwrap().clone();
    vm.resume_battle(&acknowledged, SceneResources { gil: Some(9) });
    assert!(vm.native_journal().unwrap().events().is_empty());
    assert_eq!(vm.writes().get(&(5, 40)), Some(&77));
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Finished, .. }));
    complete(&vm);
    assert_eq!(vm.writes().get(&(5, 41)), Some(&255));
    assert_eq!(vm.current_persistent_story().unwrap().native_party().unwrap().known_main(), Some([2, 1, 255]));
    assert_eq!(vm.native_journal().unwrap().before(), &acknowledged);
    assert_eq!(vm.native_journal().unwrap().events().len(), 1);
}

#[test]
fn representation_changing_internal_rebase_is_sticky_and_exposes_no_native_afterimage() {
    let section = section(vec![set(1, 50, 9)], vec![]);
    let script = FieldScript::parse(&section).unwrap();
    let mut vm = Vm::new(SceneVars::new(state([0, 255, 255], [0, 255, 255]), &[]), 7, Limits::default());
    vm.resume_battle(&StoryState::at(33).into_extended().unwrap(), SceneResources::default());
    vm.take_stats();
    vm.begin(vec![Step::Await(ScriptRef::new(0, 1))]);
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Broken, .. }));
    assert!(vm.writes().is_empty());
    assert!(vm.native_journal().is_none());
}
