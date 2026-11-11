use super::*;

fn replay(outcome: &FieldScratchOutcome) -> FieldScratch {
    let mut actual = outcome.before().clone();
    assert_eq!(outcome.apply_to(&mut actual, outcome.before()).unwrap(), outcome.changed());
    assert_eq!(&actual, outcome.after());
    actual
}

#[test]
fn constructing_a_run_does_not_turn_an_absent_snapshot_into_an_entry_clear() {
    let unknown = FieldScratch::unknown();
    let cleared = FieldScratch::cleared_for_entry();
    let unknown_run = FieldScratchRun::new(&unknown);
    let clear_run = FieldScratchRun::new(&cleared);
    assert_eq!(unknown_run.before(), &unknown);
    assert_eq!(unknown_run.current(), &unknown);
    assert_eq!(clear_run.current(), &cleared);
    assert_eq!(unknown_run.current().values(), clear_run.current().values());
    assert_ne!(unknown_run.current().known_masks(), clear_run.current().known_masks());
    assert!(unknown_run.assigned_masks().iter().all(|&mask| mask == 0));
    assert!(!unknown_run.changed() && !unknown_run.any_change());
    let empty = unknown_run.snapshot().unwrap();
    assert_eq!(replay(&empty), unknown);
    let mut authored_zero = FieldScratchRun::new(&unknown);
    assert_eq!(authored_zero.write_u8(5, 31, 0), Ok(true));
    assert_eq!(authored_zero.current().values(), unknown.values());
    assert!(authored_zero.changed() && authored_zero.any_change(), "establishing zero is a knowledge change");
    assert_eq!(replay(&authored_zero.finish().unwrap()).read_u8(5, 31), Ok(0));
    assert_eq!(clear_run.read_u8(5, 31), Ok(0));
    let error = FieldScratchError::Unknown { address: 31, missing_mask: 255 };
    assert_eq!(unknown_run.read_u8(5, 31), Err(error));
    assert_eq!(unknown_run.fault(), Some(error));
    assert_eq!(unknown_run.finish(), Err(error));
    assert_eq!(clear_run.fault(), None);
}

#[test]
fn masked_writes_establish_only_assigned_bits_and_accumulate_their_actual_masks() {
    let before = FieldScratch::unknown();
    let mut run = FieldScratchRun::new(&before);
    assert_eq!(run.write_masked_u8(5, 8, 0xF3, 0x0F), Ok(true));
    assert_eq!(run.current().values()[8], 3);
    assert_eq!(run.current().known_masks()[8], 15);
    assert_eq!(run.assigned_masks()[8], 15);
    assert_eq!(run.read_masked_u8(6, 8, 7), Ok(3));
    assert_eq!(run.write_masked_u8(6, 8, 0, 0xF0), Ok(true), "known zero is an authored change");
    assert_eq!(run.current().known_masks()[8], 255);
    assert_eq!(run.assigned_masks()[8], 255);
    assert_eq!(run.read_u8(5, 8), Ok(3));
    let snapshot = run.snapshot().unwrap();
    assert_eq!(run.write_masked_u8(5, 9, 255, 0), Ok(false));
    assert_eq!(run.snapshot().unwrap(), snapshot);
    for address in [7, 9] {
        assert_eq!(run.current().known_masks()[address], 0);
        assert_eq!(run.assigned_masks()[address], 0);
    }
    assert!(run.changed() && run.any_change());
    assert_eq!(replay(&run.finish().unwrap()).read_u8(6, 8), Ok(3));
}

#[test]
fn unchanged_authored_writes_keep_masks_without_claiming_any_change() {
    let before = FieldScratch::cleared_for_entry();
    let mut run = FieldScratchRun::new(&before);
    assert_eq!(run.write_masked_u8(5, 10, 0, 0x20), Ok(false));
    assert_eq!(run.write_u16(6, 20, 0), Ok(false));
    assert_eq!(run.write_u16(5, 22, 0xAB00), Ok(false), "selector5 assigns only the unchanged low byte");
    assert_eq!(run.assigned_masks()[10], 0x20);
    assert_eq!(&run.assigned_masks()[20..23], &[255; 3]);
    assert_eq!(run.assigned_masks().iter().filter(|&&mask| mask != 0).count(), 4);
    assert!(!run.changed() && !run.any_change());
    let outcome = run.finish().unwrap();
    assert!(!outcome.changed() && !outcome.any_change());
    assert_eq!(replay(&outcome), before);
}

#[test]
fn changing_then_restoring_a_byte_retains_any_change_without_a_net_change() {
    let before = FieldScratch::cleared_for_entry();
    let mut run = FieldScratchRun::new(&before);
    assert_eq!(run.write_u8(5, 31, 1), Ok(true));
    assert_eq!(run.write_u8(6, 31, 0), Ok(true));
    assert_eq!(run.current(), &before);
    assert_eq!(run.assigned_masks()[31], 255);
    assert!(run.any_change());
    assert!(!run.changed());
    let outcome = run.finish().unwrap();
    assert!(outcome.any_change());
    assert!(!outcome.changed());
    assert_eq!(replay(&outcome), before);
}

#[test]
fn selector_five_wide_access_zero_extends_and_truncates_without_touching_a_neighbour() {
    let mut run = FieldScratchRun::new(&FieldScratch::unknown());
    assert_eq!(run.write_u16(5, 20, 0x80FF), Ok(true));
    assert_eq!(run.read_u16(5, 20), Ok(255));
    assert_eq!(run.read_i16(5, 20), Ok(255));
    assert_eq!(run.read_u8(6, 20), Ok(255));
    assert_eq!(run.current().known_masks()[21], 0);
    assert_eq!(run.assigned_masks()[21], 0);
    assert_eq!(run.write_i16(5, 20, -2), Ok(true));
    assert_eq!(run.read_i16(5, 20), Ok(254));
    assert_eq!(run.write_u16(5, 255, 0xFF80), Ok(true));
    assert_eq!(run.read_i16(5, 255), Ok(128), "the last byte is a valid selector5 wide operand");
    assert_eq!(run.assigned_masks()[255], 255);
    assert_eq!(run.assigned_masks().iter().filter(|&&mask| mask != 0).count(), 2);
    replay(&run.finish().unwrap());
}

#[test]
fn selector_six_words_are_little_endian_signed_and_overlap_the_same_physical_bytes() {
    let mut run = FieldScratchRun::new(&FieldScratch::unknown());
    run.write_u16(6, 20, 0x80FF).unwrap();
    assert_eq!(run.read_i16(6, 20), Ok(-32513));
    run.write_u8(5, 21, 0x7F).unwrap();
    assert_eq!(run.read_i16(6, 20), Ok(32767));
    run.write_u16(6, 21, 0x1234).unwrap();
    assert_eq!(run.read_u16(6, 20), Ok(0x34FF));
    assert_eq!(run.read_u16(6, 21), Ok(0x1234));
    run.write_i16(6, 23, -2).unwrap();
    assert_eq!(run.read_i16(6, 23), Ok(-2));
    assert_eq!(&run.current().values()[20..25], &[255, 0x34, 0x12, 0xFE, 255]);
    assert_eq!(&run.assigned_masks()[20..25], &[255; 5]);
    assert_eq!(run.current().known_masks()[25], 0);
    replay(&run.finish().unwrap());
}

#[test]
fn masked_word_reads_require_only_selected_known_bits_and_report_the_first_missing_byte() {
    let mut before = FieldScratch::unknown();
    before.write_masked_u8(5, 30, 5, 0x0F).unwrap();
    before.write_masked_u8(6, 31, 0xA0, 0xF0).unwrap();
    let run = FieldScratchRun::new(&before);
    assert_eq!(run.read_masked_u16(6, 30, 0xF00F), Ok(0xA005));
    assert_eq!(run.read_masked_u16(6, 30, 0), Ok(0));
    assert_eq!(run.fault(), None);
    assert!(run.assigned_masks().iter().all(|&mask| mask == 0));
    let low = run.clone();
    assert_eq!(low.read_u16(6, 30), Err(FieldScratchError::Unknown { address: 30, missing_mask: 0xF0 }));
    let high = run.clone();
    assert_eq!(high.read_masked_u16(6, 30, 0x0100), Err(FieldScratchError::Unknown { address: 31, missing_mask: 1 }));
    assert_eq!(run.fault(), None, "a failed clone cannot poison the source run");
    assert_eq!(replay(&run.finish().unwrap()), before);
}

#[test]
fn selector_five_upper_word_bits_are_constant_zero_even_when_the_physical_byte_is_unknown() {
    let run = FieldScratchRun::new(&FieldScratch::unknown());
    assert_eq!(run.read_masked_u16(5, 255, 0xFF00), Ok(0));
    assert_eq!(run.read_masked_u16(5, 255, 0), Ok(0));
    assert_eq!(run.read_masked_u8(6, 255, 0), Ok(0));
    assert_eq!(run.current().known_masks()[255], 0);
    assert_eq!(run.fault(), None);
    let required_low = run.clone();
    assert_eq!(required_low.read_masked_u16(5, 255, 0xFF01), Err(FieldScratchError::Unknown { address: 255, missing_mask: 1 }));
    assert_eq!(run.fault(), None);
    assert!(!run.finish().unwrap().changed());
}

#[test]
fn selector_six_at_the_last_byte_refuses_before_filtering_even_a_zero_or_low_only_mask() {
    let before = FieldScratch::cleared_for_entry();
    for mask in [0, 1, 0x00FF, 0xFF00, u16::MAX] {
        let run = FieldScratchRun::new(&before);
        let error = FieldScratchError::OutOfBounds { address: 255, bytes: 2 };
        assert_eq!(run.read_masked_u16(6, 255, mask), Err(error), "mask={mask:#06x}");
        assert_eq!(run.read_u8(5, 0), Err(error), "the first bounds fault remains sticky");
        assert_eq!(run.current(), &before);
        assert!(run.assigned_masks().iter().all(|&assigned| assigned == 0));
        assert_eq!(run.snapshot(), Err(error));
        assert_eq!(run.finish(), Err(error));
    }
}

#[test]
fn a_failed_word_write_is_atomic_for_values_knowledge_and_assignment_masks() {
    let mut base = FieldScratchRun::new(&FieldScratch::unknown());
    base.write_masked_u8(5, 255, 1, 1).unwrap();
    let before = base.current().clone();
    let masks = *base.assigned_masks();
    for (selector, address, bytes) in [(6, 255, 2), (6, 256, 2), (5, 256, 1), (6, usize::MAX, 2)] {
        let mut run = base.clone();
        let error = FieldScratchError::OutOfBounds { address, bytes };
        assert_eq!(run.write_u16(selector, address, 0xABCD), Err(error));
        assert_eq!(run.current(), &before);
        assert_eq!(run.assigned_masks(), &masks);
        assert_eq!((run.changed(), run.any_change()), (base.changed(), base.any_change()));
        assert_eq!(run.fault(), Some(error));
        assert_eq!(run.write_i16(5, 0, -1), Err(error));
        assert_eq!(run.current(), &before);
        assert_eq!(run.assigned_masks(), &masks);
        assert_eq!(run.finish(), Err(error));
    }
    assert_eq!(base.fault(), None);
    replay(&base.finish().unwrap());
}

#[test]
fn the_first_unknown_read_poisons_later_known_accesses_writes_clones_and_exports() {
    let mut before = FieldScratch::unknown();
    before.write_u8(5, 30, 1).unwrap();
    let mut run = FieldScratchRun::new(&before);
    run.write_u8(5, 31, 0).unwrap();
    let current = run.current().clone();
    let masks = *run.assigned_masks();
    let error = FieldScratchError::Unknown { address: 32, missing_mask: 255 };
    assert_eq!(run.read_u8(5, 32), Err(error));
    assert_eq!(run.read_u16(6, 30), Err(error), "even a now-completely-known word cannot hide the earlier error");
    assert_eq!(run.write_u8(5, 32, 0), Err(error), "a later write cannot repair the failed read");
    assert_eq!(run.write_masked_u8(5, 32, 0, 0), Err(error));
    assert_eq!(run.read_u8(0, 0), Err(error), "the original error wins over a later selector error");
    assert_eq!(run.write_u16(6, 255, 0), Err(error));
    assert_eq!(run.current(), &current);
    assert_eq!(run.assigned_masks(), &masks);
    let mut clone = run.clone();
    assert_eq!(clone.fault(), Some(error));
    assert_eq!(clone.write_u8(5, 30, 2), Err(error));
    assert_eq!(clone.current(), &current);
    assert_eq!(clone.snapshot(), Err(error));
    assert_eq!(clone.finish(), Err(error));
    assert_eq!(run.snapshot(), Err(error));
    assert_eq!(run.finish(), Err(error));
}

#[test]
fn an_invalid_selector_is_a_sticky_read_or_write_fault_even_with_a_zero_mask() {
    let before = FieldScratch::cleared_for_entry();
    for selector in [0, 4, 7, 255] {
        let error = FieldScratchError::InvalidSelector(selector);
        let mut write_run = FieldScratchRun::new(&before);
        assert_eq!(write_run.write_masked_u8(selector, 0, 0, 0), Err(error));
        assert_eq!(write_run.write_u8(5, 0, 99), Err(error));
        assert_eq!(write_run.current(), &before);
        assert!(write_run.assigned_masks().iter().all(|&mask| mask == 0));
        assert_eq!(write_run.finish(), Err(error));
        let read_run = FieldScratchRun::new(&before);
        assert_eq!(read_run.read_masked_u16(selector, 0, 0), Err(error));
        assert_eq!(read_run.read_i16(6, 0), Err(error));
        assert_eq!(read_run.clone().finish(), Err(error));
        assert_eq!(read_run.snapshot(), Err(error));
    }
}

#[test]
fn successful_outcomes_require_state_expected_and_before_to_match_values_and_knownness() {
    let mut before = FieldScratch::unknown();
    before.write_u8(5, 10, 1).unwrap();
    let mut run = FieldScratchRun::new(&before);
    run.write_u8(5, 20, 2).unwrap();
    let outcome = run.finish().unwrap();
    let mut changed_value = before.clone();
    changed_value.write_u8(5, 10, 2).unwrap();
    let mut changed_knowledge = before.clone();
    changed_knowledge.write_masked_u8(6, 30, 0, 1).unwrap();
    for altered in [changed_value, changed_knowledge] {
        for (mut actual, expected) in [
            (altered.clone(), before.clone()),
            (before.clone(), altered.clone()),
            (altered.clone(), altered.clone()),
        ] {
            let unchanged = actual.clone();
            assert_eq!(outcome.apply_to(&mut actual, &expected), Err(FieldScratchError::StalePredecessor));
            assert_eq!(actual, unchanged);
        }
    }
    let after = replay(&outcome);
    assert_eq!(after.read_u8(6, 10), Ok(1));
    assert_eq!(after.read_u8(6, 20), Ok(2));
    assert_eq!(before.known_masks()[20], 0);
}

#[test]
fn inconsistent_private_outcomes_refuse_knowledge_loss_unassigned_changes_and_false_change_claims_atomically() {
    let before = FieldScratch::cleared_for_entry();
    let mut run = FieldScratchRun::new(&before);
    run.write_u8(5, 31, 1).unwrap();
    let good = run.finish().unwrap();
    let mut knowledge_loss = good.clone();
    let mut values = *knowledge_loss.after.values();
    let mut known = *knowledge_loss.after.known_masks();
    values[31] = 0;
    known[31] = 0;
    knowledge_loss.after = FieldScratch::from_parts(values, known).unwrap();
    let mut unassigned_change = good.clone();
    unassigned_change.after.write_u8(5, 32, 1).unwrap();
    let mut missing_mask = good.clone();
    missing_mask.assigned_masks[31] = 0;
    let mut denied_change = good;
    denied_change.any_change = false;
    let mut added_knowledge = FieldScratchRun::new(&FieldScratch::unknown()).finish().unwrap();
    added_knowledge.after.write_masked_u8(5, 30, 0, 1).unwrap();
    let mut invented_change = FieldScratchRun::new(&before).finish().unwrap();
    invented_change.any_change = true;
    for outcome in [knowledge_loss, unassigned_change, missing_mask, denied_change, added_knowledge, invented_change] {
        let expected = outcome.before().clone();
        let mut actual = expected.clone();
        assert_eq!(outcome.apply_to(&mut actual, &expected), Err(FieldScratchError::InvalidOutcome));
        assert_eq!(actual, expected);
    }
}

#[test]
fn successful_snapshots_stay_immutable_after_more_writes_or_a_later_fault() {
    let before = FieldScratch::unknown();
    let mut run = FieldScratchRun::new(&before);
    run.write_u8(5, 31, 1).unwrap();
    let checkpoint = run.snapshot().unwrap();
    let captured = checkpoint.clone();
    run.write_u8(6, 30, 2).unwrap();
    assert_eq!(checkpoint, captured);
    assert_eq!(checkpoint.after().known_masks()[30], 0);
    assert_eq!(checkpoint.assigned_masks()[30], 0);
    assert_eq!(run.current().read_u8(5, 30), Ok(2));
    let error = FieldScratchError::Unknown { address: 40, missing_mask: 255 };
    assert_eq!(run.read_u8(5, 40), Err(error));
    assert_eq!(run.finish(), Err(error));
    let accepted = replay(&checkpoint);
    let rebased = FieldScratchRun::new(&accepted);
    assert_eq!(rebased.before(), &accepted);
    assert!(rebased.assigned_masks().iter().all(|&mask| mask == 0));
    assert!(!rebased.changed() && !rebased.any_change());
    assert_eq!(replay(&rebased.finish().unwrap()), accepted);
}

#[test]
fn cloned_multiwrites_read_their_current_overlay_and_a_failed_clone_cannot_poison_the_original() {
    let mut before = FieldScratch::unknown();
    for (address, value) in [(10, 1), (11, 2), (12, 3)] { before.write_u8(5, address, value).unwrap(); }
    let mut run = FieldScratchRun::new(&before);
    let mut staged = run.clone();
    for (source, destination) in [(10, 11), (11, 12), (12, 13)] {
        let value = staged.read_u8(5, source).unwrap();
        staged.write_u8(6, destination, value).unwrap();
    }
    assert_eq!(run.current(), &before);
    assert!(run.assigned_masks().iter().all(|&mask| mask == 0));
    for address in 10..=13 { assert_eq!(staged.read_u8(5, address), Ok(1)); }
    assert_eq!(&staged.assigned_masks()[11..14], &[255; 3]);
    let successful_stage = staged.snapshot().unwrap();
    let mut failed = staged.clone();
    failed.write_u8(5, 14, 9).unwrap();
    let error = FieldScratchError::Unknown { address: 15, missing_mask: 255 };
    assert_eq!(failed.read_u8(6, 15), Err(error));
    assert_eq!(failed.write_u8(5, 15, 0), Err(error));
    assert_eq!(failed.finish(), Err(error));
    assert_eq!(staged.snapshot().unwrap(), successful_stage);
    assert_eq!(run.fault(), None);
    run = staged;
    assert_eq!(run.current().known_masks()[14], 0, "the failed candidate was never published");
    let after = replay(&run.finish().unwrap());
    assert_eq!(after.read_u8(5, 13), Ok(1));
    assert_eq!(after.known_masks()[14], 0);
}
