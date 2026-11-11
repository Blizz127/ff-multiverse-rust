use super::*;
use crate::script::party::{sync_from_main, transition, PartyError};

fn state(raw: Option<[u8; 3]>, main: Option<[u8; 3]>) -> StoryState {
    let mut state = StoryState::at(69).into_native_party().unwrap();
    if let Some(raw) = raw {
        for (address, member) in [9, 10, 11].into_iter().zip(raw) { state.write_byte(3, address, member).unwrap(); }
    }
    if let Some(main) = main { state.native_party_mut().unwrap().write_main_slots(main, 7).unwrap(); }
    state
}

fn assert_after(effect: &NativePartyEffect, raw: [u8; 3], main: [u8; 3], assigned: NativePartyAssignments) {
    assert_eq!(effect.after().raw_slots(), raw.map(|member| (member, 255)));
    assert_eq!(effect.after().native_party().known_main(), Some(main));
    assert_eq!(effect.assignments(), assigned);
}

#[test]
fn tuple_operations_refuse_missing_main_knowledge_without_changing_the_source() {
    let mut input = state(Some([0, 1, 255]), None);
    input.native_party_mut().unwrap().write_main_slots([0, 0, 0], 1).unwrap();
    let before = input.encode();
    for op in [NativePartyOp::Add(2), NativePartyOp::Remove(1), NativePartyOp::Set([0, 2, 254]), NativePartyOp::FieldSync] {
        assert_eq!(evaluate_native_party(&input, op), Err(NativePartyOpError::UnknownMainSlot(1)), "{op:?}");
        assert_eq!(input.encode(), before);
    }
    let unknown = state(None, None);
    assert_eq!(evaluate_native_party(&unknown, NativePartyOp::Set([0, 2, 254])), Err(NativePartyOpError::UnknownMainSlot(0)));
}

#[test]
fn operations_that_preserve_or_compare_raw_slots_refuse_partial_bytes() {
    let mut input = state(None, Some([0, 1, 255]));
    input.write_byte(3, 9, 0).unwrap();
    input.write_masked(4, 10, 0, 1).unwrap();
    input.write_byte(3, 11, 255).unwrap();
    let before = input.encode();
    for op in [
        NativePartyOp::Add(2), NativePartyOp::Remove(1), NativePartyOp::Set([0, 255, 254]),
        NativePartyOp::Available { character: 2, available: false }, NativePartyOp::FieldSync,
    ] {
        assert_eq!(evaluate_native_party(&input, op), Err(NativePartyOpError::UnknownRawSlot { address: 10, known_mask: 1 }), "{op:?}");
        assert_eq!(input.encode(), before);
    }
}

#[test]
fn a_complete_set_initializes_unknown_raw_slots_but_does_not_guess_main_order() {
    let mut input = state(None, Some([0, 255, 255]));
    let before = input.encode();
    let effect = evaluate_native_party(&input, NativePartyOp::Set([0, 2, 254])).unwrap();
    assert_eq!(input.encode(), before);
    assert_eq!(effect.before().raw_slots(), [(0, 0); 3]);
    assert_after(&effect, [0, 2, 255], [0, 2, 255], NativePartyAssignments { raw_slots: 7, main_slots: 6, availability: 5, locks: 0 });
    assert_eq!(effect.after().native_party().availability_bits(), (5, 5));
    assert!(effect.apply_to(&mut input).unwrap());
    assert_eq!(NativePartyImage::capture(&input).unwrap(), *effect.after());
    let unknown = state(None, None);
    assert_eq!(evaluate_native_party(&unknown, NativePartyOp::Set([254; 3])), Err(NativePartyOpError::UnknownMainSlot(0)));
}

#[test]
fn availability_and_lock_assignments_need_no_tuple_knowledge() {
    let mut input = state(None, None);
    for op in [
        NativePartyOp::Available { character: 2, available: true },
        NativePartyOp::Lock { character: 0, locked: false },
        NativePartyOp::Lock { character: 2, locked: true },
    ] {
        let effect = evaluate_native_party(&input, op).unwrap();
        assert_eq!(effect.operation(), op);
        assert_eq!(effect.before().raw_slots(), [(0, 0); 3]);
        assert_eq!(effect.after().raw_slots(), [(0, 0); 3]);
        assert_eq!(effect.after().native_party().main_slots(), ([0; 3], 0));
        let expected = match op {
            NativePartyOp::Available { .. } => NativePartyAssignments { availability: 4, ..Default::default() },
            NativePartyOp::Lock { character, .. } => NativePartyAssignments { locks: 1 << character, ..Default::default() },
            _ => unreachable!(),
        };
        assert_eq!(effect.assignments(), expected);
        assert!(effect.apply_to(&mut input).unwrap());
    }
    let party = input.native_party().unwrap();
    assert_eq!(party.availability_bits(), (4, 4));
    assert_eq!(party.locking_bits(), (4, 5));
    assert_eq!(party.known_available(0), Ok(None));
    assert_eq!(party.known_locked(0), Ok(Some(false)));
}

#[test]
fn unavailable_clears_all_matching_raw_duplicates_without_touching_main_or_locks() {
    let mut input = state(Some([2, 0, 2]), Some([2, 0, 255]));
    input.native_party_mut().unwrap().write_locks_masked(4, 4);
    let effect = evaluate_native_party(&input, NativePartyOp::Available { character: 2, available: false }).unwrap();
    assert_after(&effect, [255, 0, 255], [2, 0, 255], NativePartyAssignments { raw_slots: 5, main_slots: 0, availability: 4, locks: 0 });
    assert_eq!(effect.after().native_party().availability_bits(), (0, 4));
    assert_eq!(effect.after().native_party().locking_bits(), (4, 4));
    effect.apply_to(&mut input).unwrap();
    assert_eq!(input.native_party().unwrap().known_main_slot(0), Ok(Some(2)), "an unavailable character can still occupy main until an explicit synchronization");
    let unknown_main = state(Some([2, 2, 255]), None);
    let effect = evaluate_native_party(&unknown_main, NativePartyOp::Available { character: 2, available: false }).unwrap();
    assert_eq!(effect.after().raw_slots(), [(255, 255); 3]);
    assert_eq!(effect.after().native_party().main_slots(), ([0; 3], 0));
    assert_eq!(effect.assignments().raw_slots, 3);
}

#[test]
fn duplicate_add_and_absent_remove_still_reconcile_main_without_raw_or_availability_assignments() {
    let input = state(Some([2, 255, 0]), Some([0, 1, 255]));
    let before = input.encode();
    for op in [NativePartyOp::Add(2), NativePartyOp::Remove(1)] {
        let effect = evaluate_native_party(&input, op).unwrap();
        assert_after(&effect, [2, 255, 0], [0, 2, 255], NativePartyAssignments { raw_slots: 0, main_slots: 6, availability: 0, locks: 0 });
        assert_eq!(effect.after().native_party().availability_bits(), (0, 0));
        assert!(effect.changed());
        assert_eq!(input.encode(), before);
    }
}

#[test]
fn padded_empty_markers_are_recorded_as_main_assignments_even_when_values_do_not_change() {
    let input = state(Some([0, 1, 255]), Some([0, 1, 255]));
    for op in [NativePartyOp::Add(1), NativePartyOp::Remove(2)] {
        let effect = evaluate_native_party(&input, op).unwrap();
        assert_after(&effect, [0, 1, 255], [0, 1, 255], NativePartyAssignments { raw_slots: 0, main_slots: 4, availability: 0, locks: 0 });
        assert!(!effect.changed(), "the journal still records the native assignment");
        assert!(!effect.apply_to(&mut input.clone()).unwrap());
    }
    let holes = state(Some([255, 0, 255]), Some([255, 0, 255]));
    let effect = evaluate_native_party(&holes, NativePartyOp::Remove(2)).unwrap();
    assert_eq!(effect.assignments().main_slots, 5, "PartyRemove executes all padded 255 entries, including both empty destinations");
}

#[test]
fn preserving_set_repairs_raw_duplicates_and_records_the_exact_native_assignments() {
    let input = state(Some([0, 0, 1]), Some([0, 1, 255]));
    let effect = evaluate_native_party(&input, NativePartyOp::Set([0, 255, 254])).unwrap();
    assert_after(&effect, [0, 1, 255], [0, 1, 255], NativePartyAssignments { raw_slots: 7, main_slots: 4, availability: 3, locks: 0 });
    assert_eq!(effect.after().native_party().availability_bits(), (3, 3));
    assert_eq!(effect.before().raw_slots(), [(0, 255), (0, 255), (1, 255)]);
    assert_eq!(transition([0, 0, 1], [0, 1, 255], PartyOp::Set([0, 255, 254])), Err(PartyError::InvalidSlots), "the legacy wrapper keeps its stricter raw-tuple policy");
}

#[test]
fn duplicate_raw_outputs_are_allowed_only_when_the_final_main_tuple_is_representable() {
    let input = state(Some([0, 255, 255]), Some([0, 255, 255]));
    let effect = evaluate_native_party(&input, NativePartyOp::Set([0, 0, 254])).unwrap();
    assert_after(&effect, [0, 0, 255], [0, 255, 255], NativePartyAssignments { raw_slots: 7, main_slots: 6, availability: 1, locks: 0 });
    assert_eq!(transition([0, 255, 255], [0, 255, 255], PartyOp::Set([0, 0, 254])), Err(PartyError::InvalidSlots));
    let missing_from_main = state(Some([1, 255, 255]), Some([1, 255, 255]));
    let before = missing_from_main.encode();
    assert_eq!(evaluate_native_party(&missing_from_main, NativePartyOp::Set([0, 0, 254])),
        Err(NativePartyOpError::Resource(NativePartyError::DuplicateMember(0))));
    assert_eq!(missing_from_main.encode(), before, "an invalid resulting main tuple produces no partial raw writes");
    assert_eq!(transition([1, 255, 255], [1, 255, 255], PartyOp::Set([0, 0, 254])), Err(PartyError::InvalidSlots));
}

#[test]
fn field_sync_preserves_raw_order_and_duplicates_without_assigning_phs_or_main() {
    for (raw, main, expected, mask) in [
        ([2, 255, 0], [0, 1, 255], [1, 255, 0], 3),
        ([1, 0, 255], [0, 1, 255], [1, 0, 255], 4),
        ([0, 0, 255], [0, 1, 255], [0, 0, 1], 4),
    ] {
        let input = state(Some(raw), Some(main));
        let effect = evaluate_native_party(&input, NativePartyOp::FieldSync).unwrap();
        assert_after(&effect, expected, main, NativePartyAssignments { raw_slots: mask, main_slots: 0, availability: 0, locks: 0 });
        assert_eq!(effect.after().native_party(), effect.before().native_party());
    }
    assert_eq!(sync_from_main([0, 0, 255], [0, 1, 255]), Err(PartyError::InvalidSlots));
    for op in [PartyOp::Add(2), PartyOp::Remove(0)] {
        assert_eq!(transition([0, 0, 255], [0, 1, 255], op), Err(PartyError::InvalidSlots));
    }
}

#[test]
fn raw_and_main_holes_keep_their_independent_positions_across_operations() {
    let mut input = state(Some([2, 255, 0]), Some([0, 255, 2]));
    let remove = evaluate_native_party(&input, NativePartyOp::Remove(0)).unwrap();
    assert_after(&remove, [2, 255, 255], [255, 255, 2], NativePartyAssignments { raw_slots: 4, main_slots: 3, availability: 0, locks: 0 });
    remove.apply_to(&mut input).unwrap();
    let add = evaluate_native_party(&input, NativePartyOp::Add(1)).unwrap();
    assert_after(&add, [2, 1, 255], [1, 255, 2], NativePartyAssignments { raw_slots: 2, main_slots: 3, availability: 2, locks: 0 });
    assert_eq!(add.before(), remove.after());
}

#[test]
fn complete_sets_record_all_raw_assignments_and_availability_or_bits_even_when_unchanged() {
    for (slots, requested, main_mask, availability) in [
        ([0, 1, 2], [0, 1, 2], 0, 7),
        ([0, 1, 255], [0, 1, 254], 4, 3),
    ] {
        let mut input = state(Some(slots), Some(slots));
        input.native_party_mut().unwrap().write_availability_masked(availability, availability);
        let effect = evaluate_native_party(&input, NativePartyOp::Set(requested)).unwrap();
        assert_after(&effect, slots, slots, NativePartyAssignments { raw_slots: 7, main_slots: main_mask, availability, locks: 0 });
        assert!(!effect.changed());
        assert!(!effect.apply_to(&mut input).unwrap());
    }
}

#[test]
fn apply_preserves_unrelated_banks_and_refuses_a_stale_party_image_atomically() {
    let original = state(Some([0, 1, 255]), Some([0, 1, 255]));
    let effect = evaluate_native_party(&original, NativePartyOp::Remove(1)).unwrap();
    let mut input = original.clone();
    input.write_byte(1, 4, 37).unwrap();
    input.write_masked(7, 20, 0x80, 0x80).unwrap();
    input.write_word(14, 100, 0x1234).unwrap();
    input.moment = 108;
    assert!(effect.apply_to(&mut input).unwrap(), "the helper compares party inputs; the durable writer separately compares the whole predecessor");
    assert_eq!(input.known_byte(1, 4), Some(37));
    assert_eq!(input.known_mask(7, 20), 0x80);
    assert_eq!(input.known_word(13, 100), Some(0x1234));
    assert_eq!(input.moment, 108);
    let before = input.encode();
    assert_eq!(effect.apply_to(&mut input), Err(NativePartyOpError::StalePredecessor));
    assert_eq!(input.encode(), before);
    assert_eq!(original.known_byte(3, 10), Some(1));
}

#[test]
fn apply_checks_raw_and_native_knownness_even_when_the_operation_does_not_read_those_values() {
    let original = state(None, None);
    let effect = evaluate_native_party(&original, NativePartyOp::Available { character: 2, available: true }).unwrap();
    let mut stale = Vec::new();
    let mut raw_zero = original.clone();
    raw_zero.write_masked(3, 10, 0, 1).unwrap();
    stale.push(raw_zero);
    let mut raw_set = original.clone();
    raw_set.write_masked(4, 10, 1, 1).unwrap();
    stale.push(raw_set);
    let mut main = original.clone();
    main.native_party_mut().unwrap().write_main_slots([0, 0, 0], 1).unwrap();
    stale.push(main);
    let mut availability = original.clone();
    availability.native_party_mut().unwrap().write_availability_masked(0, 1);
    stale.push(availability);
    let mut locks = original.clone();
    locks.native_party_mut().unwrap().write_locks_masked(0, 1);
    stale.push(locks);
    for mut input in stale {
        let before = input.encode();
        assert_eq!(effect.apply_to(&mut input), Err(NativePartyOpError::StalePredecessor));
        assert_eq!(input.encode(), before);
    }
}

#[test]
fn native_operations_never_promote_a_legacy_state() {
    for bytes in [vec![1, 69, 0], vec![2, 69, 0, 1, 3, 9, 2], vec![3, 69, 0, 0, 0]] {
        let mut input = StoryState::decode(&bytes).unwrap();
        for op in [NativePartyOp::Set([0, 2, 254]), NativePartyOp::Available { character: 2, available: true }, NativePartyOp::FieldSync] {
            assert_eq!(evaluate_native_party(&input, op), Err(NativePartyOpError::RequiresNativeParty));
        }
        let effect = evaluate_native_party(&state(None, None), NativePartyOp::Lock { character: 0, locked: true }).unwrap();
        assert_eq!(effect.apply_to(&mut input), Err(NativePartyOpError::RequiresNativeParty));
        assert_eq!(input.encode(), bytes);
    }
}

#[test]
fn ordinary_selectors_are_bounded_while_existing_high_phs_bits_survive() {
    let mut input = state(Some([0, 255, 255]), Some([0, 255, 255]));
    input.native_party_mut().unwrap().write_availability_masked(0xA000, 0xF000);
    input.native_party_mut().unwrap().write_locks_masked(0x5000, 0xF000);
    let before = input.encode();
    for character in [9, 10, 15, 16, 255] {
        for op in [
            NativePartyOp::Add(character), NativePartyOp::Remove(character),
            NativePartyOp::Available { character, available: false }, NativePartyOp::Available { character, available: true },
            NativePartyOp::Lock { character, locked: false }, NativePartyOp::Lock { character, locked: true },
        ] {
            assert_eq!(evaluate_native_party(&input, op), Err(NativePartyOpError::UnsupportedSelector(character)));
            assert_eq!(input.encode(), before);
        }
        if character != 255 {
            assert_eq!(evaluate_native_party(&input, NativePartyOp::Set([0, character, 254])), Err(NativePartyOpError::UnsupportedSelector(character)));
        }
    }
    for op in [NativePartyOp::Add(8), NativePartyOp::Lock { character: 8, locked: true }, NativePartyOp::Remove(8)] {
        evaluate_native_party(&input, op).unwrap().apply_to(&mut input).unwrap();
    }
    assert_eq!(input.native_party().unwrap().availability_bits(), (0xA100, 0xF100));
    assert_eq!(input.native_party().unwrap().locking_bits(), (0x5100, 0xF100));
}

#[test]
fn ordered_add_then_remove_keeps_the_authored_availability_assignment() {
    let mut input = state(Some([0, 255, 255]), Some([0, 255, 255]));
    input.native_party_mut().unwrap().write_availability_masked(1, 1);
    let add = evaluate_native_party(&input, NativePartyOp::Add(1)).unwrap();
    add.apply_to(&mut input).unwrap();
    let remove = evaluate_native_party(&input, NativePartyOp::Remove(1)).unwrap();
    assert_eq!(remove.before(), add.after());
    assert_eq!(add.assignments().availability, 2);
    assert_eq!(remove.assignments().availability, 0);
    remove.apply_to(&mut input).unwrap();
    assert_eq!(input.native_party().unwrap().known_main(), Some([0, 255, 255]));
    assert_eq!(NativePartyImage::capture(&input).unwrap().raw_slots(), [(0, 255), (255, 255), (255, 255)]);
    assert_eq!(input.native_party().unwrap().availability_bits(), (3, 3), "the final roster alone cannot reconstruct ordered side effects");
}

// The old value-only algorithm, before native padded-255 assignment accounting was factored
// out. This deliberately does not call native_transition or reconcile_slots: it checks values
// and legacy acceptance independently, while the focused tests above check assignment masks.
fn simplified_transition(mut raw: [u8; 3], mut main: [u8; 3], op: PartyOp) -> Option<([u8; 3], [u8; 3])> {
    match op {
        PartyOp::Add(member) => {
            if !raw.contains(&member) { let slot = raw.iter().position(|&m| m == 255).unwrap_or(2); raw[slot] = member; }
        }
        PartyOp::Remove(member) => {
            if let Some(slot) = raw.iter().position(|&m| m == member) { raw[slot] = 255; }
        }
        PartyOp::Set(mut requested) => {
            for member in &mut raw {
                if requested.iter().any(|&m| m != 255 && m == *member) { *member = 255; }
            }
            for member in raw.into_iter().filter(|&m| m != 255) {
                if let Some(slot) = requested.iter_mut().find(|m| **m == 255) { *slot = member; }
            }
            raw = requested.map(|m| if m == 254 { 255 } else { m });
        }
    }
    if !unique(&raw) { return None; }
    for member in &mut main { if !raw.contains(member) { *member = 255; } }
    for member in raw.into_iter().filter(|&m| m != 255) {
        if !main.contains(&member) {
            if let Some(slot) = main.iter_mut().find(|m| **m == 255) { *slot = member; }
        }
    }
    Some((raw, main))
}

fn unique(slots: &[u8; 3]) -> bool {
    slots.iter().enumerate().all(|(index, member)| *member == 255 || !slots[..index].contains(member))
}

#[test]
fn small_exhaustive_legacy_compatibility_preserves_values_without_weakening_duplicate_refusal() {
    let mut tuples = Vec::new();
    for a in [0, 1, 2, 255] { for b in [0, 1, 2, 255] { for c in [0, 1, 2, 255] {
        if unique(&[a, b, c]) { tuples.push([a, b, c]); }
    } } }
    assert_eq!(tuples.len(), 34);
    let mut ops = Vec::new();
    for member in 0..3 { ops.extend([PartyOp::Add(member), PartyOp::Remove(member)]); }
    for slots in [[254; 3], [255; 3], [0, 1, 254], [2, 255, 254], [255, 0, 254], [2, 1, 0], [0, 0, 254]] {
        ops.push(PartyOp::Set(slots));
    }
    for &raw in &tuples { for &main in &tuples {
        let input = state(Some(raw), Some(main));
        for &op in &ops {
            let expected = simplified_transition(raw, main, op);
            let legacy = transition(raw, main, op);
            assert_eq!(legacy.as_ref().ok().map(|change| (change.bank, change.main)), expected, "raw={raw:?} main={main:?} op={op:?}");
            let Some((after_raw, after_main)) = expected else {
                assert_eq!(legacy, Err(PartyError::InvalidSlots));
                continue;
            };
            let op = match op { PartyOp::Add(m) => NativePartyOp::Add(m), PartyOp::Remove(m) => NativePartyOp::Remove(m), PartyOp::Set(m) => NativePartyOp::Set(m) };
            let native = evaluate_native_party(&input, op).unwrap();
            assert_eq!(native.after().raw_slots(), after_raw.map(|member| (member, 255)), "{raw:?} {main:?} {op:?}");
            assert_eq!(native.after().native_party().known_main(), Some(after_main), "{raw:?} {main:?} {op:?}");
        }
    } }
}
