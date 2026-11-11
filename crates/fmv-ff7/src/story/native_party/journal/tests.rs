use super::*;

fn state() -> StoryState {
    let mut state = StoryState::at(69).into_native_party().unwrap();
    for (address, member) in [(9, 0), (10, 1), (11, 255)] { state.write_byte(3, address, member).unwrap(); }
    state.native_party_mut().unwrap().write_main_slots([0, 1, 255], 7).unwrap();
    state
}

fn assignment(event: &NativeStoryEvent) -> &NativeStoryAssignment {
    match event { NativeStoryEvent::Assignment(write) => write, NativeStoryEvent::Party(_) => panic!("expected a persistent assignment") }
}

fn replay(journal: &NativeStoryJournal) -> StoryState {
    let mut actual = journal.before().clone();
    assert_eq!(journal.apply_to(&mut actual, journal.before()).unwrap(), journal.changed());
    assert_eq!(&actual, journal.after());
    actual
}

#[test]
fn interleaved_raw_write_party_operation_and_restore_cannot_be_replayed_as_a_final_map() {
    let before = state();
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    builder.write_byte(4, 10, 2).unwrap();
    let effect = builder.party(NativePartyOp::Add(0)).unwrap();
    builder.write_byte(3, 10, 1).unwrap();
    assert_eq!(builder.before(), &before);
    assert_eq!(builder.events().len(), 3, "the party effect is one event, without duplicate raw assignments");
    assert_eq!(assignment(&builder.events()[0]).key(), (3, 10));
    assert_eq!(assignment(&builder.events()[0]).value(), 2);
    assert_eq!(builder.events()[1], NativeStoryEvent::Party(effect));
    assert_eq!(assignment(&builder.events()[2]).before(), (2, 255));
    assert_eq!(assignment(&builder.events()[2]).after(), (1, 255));
    assert_eq!(builder.state().native_party().unwrap().known_main(), Some([0, 2, 255]));
    assert_eq!(builder.state().native_party().unwrap().availability_bits(), (0, 0), "a duplicate Add does not manufacture availability for the raw assignment");
    for address in 9..=11 { assert_eq!(builder.state().known_byte(3, address), before.known_byte(3, address)); }
    let journal = builder.finish();
    assert!(journal.changed() && journal.any_change());
    replay(&journal);
    assert_eq!(before.native_party().unwrap().known_main(), Some([0, 1, 255]), "building and replaying leave the captured source alone");
}

#[test]
fn ordered_add_and_remove_keep_availability_even_when_both_tuples_return_to_their_start() {
    let mut before = state();
    before.write_byte(3, 10, 255).unwrap();
    before.native_party_mut().unwrap().write_main_slots([0, 255, 255], 7).unwrap();
    before.native_party_mut().unwrap().write_availability_masked(1, 1);
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    let add = builder.party(NativePartyOp::Add(1)).unwrap();
    let remove = builder.party(NativePartyOp::Remove(1)).unwrap();
    assert_eq!(remove.before(), add.after());
    assert_eq!(builder.events(), &[NativeStoryEvent::Party(add), NativeStoryEvent::Party(remove)]);
    assert_eq!(builder.state().native_party().unwrap().known_main(), Some([0, 255, 255]));
    for address in 9..=11 { assert_eq!(builder.state().known_byte(3, address), before.known_byte(3, address)); }
    assert_eq!(builder.state().native_party().unwrap().availability_bits(), (3, 3));
    assert!(builder.changed() && builder.any_change());
    replay(&builder.finish());
}

#[test]
fn masked_assignments_and_alias_words_keep_both_sides_of_raw_party_boundaries() {
    let before = StoryState::at(69).into_native_party().unwrap();
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    builder.write_masked(4, 10, 0xF2, 0x0F).unwrap();
    builder.write_masked(3, 10, 0x0F, 0xF0).unwrap();
    builder.write_word(4, 8, 0x01AB).unwrap();
    builder.write_word(4, 11, 0xCDFF).unwrap();
    assert_eq!(builder.events().len(), 6);
    let writes: Vec<_> = builder.events().iter().map(assignment).collect();
    assert_eq!(writes.iter().map(|write| write.key()).collect::<Vec<_>>(), [(3, 10), (3, 10), (3, 8), (3, 9), (3, 11), (3, 12)]);
    assert_eq!((writes[0].value(), writes[0].mask(), writes[0].before(), writes[0].after()), (2, 15, (0, 0), (2, 15)));
    assert_eq!((writes[1].value(), writes[1].mask(), writes[1].before(), writes[1].after()), (0, 240, (2, 15), (2, 255)));
    assert_eq!(builder.state().known_word(3, 8), Some(0x01AB));
    assert_eq!(builder.state().known_word(4, 11), Some(0xCDFF));
    assert_eq!(builder.state().known_byte(3, 10), Some(2));
    assert_eq!(builder.state().known_mask(3, 7), 0);
    assert_eq!(builder.state().known_mask(3, 13), 0);
    assert_eq!(builder.state().native_party(), before.native_party(), "bank assignments alone do not initialize or synchronize main slots");
    replay(&builder.finish());
}

#[test]
fn moment_word_and_byte_aliases_replay_into_the_header_in_instruction_order() {
    let mut before = state();
    before.moment = 0x1234;
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    builder.write_word(2, 0, 0xABCD).unwrap();
    builder.write_byte(1, 0, 0xEF).unwrap();
    assert_eq!(builder.events().len(), 3);
    let writes: Vec<_> = builder.events().iter().map(assignment).collect();
    assert_eq!((writes[0].key(), writes[0].before(), writes[0].after()), ((1, 0), (0x34, 255), (0xCD, 255)));
    assert_eq!((writes[1].key(), writes[1].before(), writes[1].after()), ((1, 1), (0x12, 255), (0xAB, 255)));
    assert_eq!((writes[2].key(), writes[2].before(), writes[2].after()), ((1, 0), (0xCD, 255), (0xEF, 255)));
    assert_eq!(builder.state().moment, 0xABEF);
    assert_eq!(builder.state().known_word(2, 0), Some(0xABEF));
    let journal = builder.finish();
    let actual = replay(&journal);
    let encoded = actual.encode();
    assert_eq!(&encoded[..3], &[4, 0xEF, 0xAB]);
    assert_eq!(StoryState::decode(&encoded), Some(actual));
    assert_eq!(before.moment, 0x1234);
}

#[test]
fn refused_word_or_bank_writes_leave_the_overlay_and_existing_events_unchanged() {
    let mut builder = NativeStoryBuilder::new(&state()).unwrap();
    builder.write_byte(1, 4, 30).unwrap();
    let before = builder.snapshot();
    assert_eq!(builder.write_word(4, 255, 0xABCD), Err(NativeStoryError::Bank(StoryWriteError::WordCrossesBank)));
    assert_eq!(builder.snapshot(), before, "no low-byte write at address255 escaped the failed word");
    for bank in [0, 5, 6, 8, 9, 10, 16, 255] {
        let error = Err(NativeStoryError::Bank(StoryWriteError::NonPersistentBank(bank)));
        assert_eq!(builder.write_byte(bank, 9, 1), error);
        assert_eq!(builder.write_word(bank, 8, 0x1234), error);
        assert_eq!(builder.write_masked(bank, 9, 0, 0), error, "a zero mask does not authorize a nonpersistent bank");
        assert_eq!(builder.snapshot(), before);
    }
}

#[test]
fn same_value_assignments_are_retained_but_zero_masks_are_not_events() {
    let mut before = state();
    before.write_byte(1, 4, 30).unwrap();
    before.write_word(13, 50, 0x1234).unwrap();
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    assert!(!builder.write_byte(2, 4, 30).unwrap());
    assert!(!builder.write_masked(1, 4, 255, 0).unwrap());
    assert_eq!(builder.events().len(), 1);
    assert!(!builder.write_word(14, 50, 0x1234).unwrap());
    let effect = builder.party(NativePartyOp::Add(0)).unwrap();
    assert!(!effect.changed());
    assert_eq!(builder.events().len(), 4);
    assert_eq!(assignment(&builder.events()[0]).key(), (1, 4));
    assert_eq!(assignment(&builder.events()[1]).key(), (13, 50));
    assert_eq!(assignment(&builder.events()[2]).key(), (13, 51));
    assert!(builder.events().iter().all(|event| !event.changed()));
    assert!(!builder.changed() && !builder.any_change());
    let journal = builder.finish();
    assert!(!journal.changed() && !journal.any_change());
    assert_eq!(replay(&journal), before);
}

#[test]
fn a_change_followed_by_restoration_has_events_and_any_change_but_no_net_change() {
    let mut before = state();
    before.write_byte(1, 4, 30).unwrap();
    before.native_party_mut().unwrap().write_locks_masked(0, 4);
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    builder.write_byte(1, 4, 40).unwrap();
    builder.write_byte(1, 4, 30).unwrap();
    builder.party(NativePartyOp::Lock { character: 2, locked: true }).unwrap();
    builder.party(NativePartyOp::Lock { character: 2, locked: false }).unwrap();
    assert_eq!(builder.events().len(), 4);
    assert!(builder.events().iter().all(NativeStoryEvent::changed));
    assert!(builder.any_change());
    assert!(!builder.changed());
    let journal = builder.finish();
    assert!(journal.any_change());
    assert!(!journal.changed());
    assert_eq!(replay(&journal), before);
}

#[test]
fn authoring_a_known_zero_is_a_change_even_when_the_stored_value_byte_stays_zero() {
    let before = state();
    assert_eq!(before.known_mask(7, 40), 0);
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    assert!(builder.write_masked(15, 40, 0, 0x20).unwrap());
    let write = assignment(&builder.events()[0]);
    assert_eq!((write.key(), write.value(), write.mask(), write.before(), write.after()), ((7, 40), 0, 0x20, (0, 0), (0, 0x20)));
    assert!(write.changed() && builder.changed() && builder.any_change());
    assert_eq!(builder.state().known_bit(7, 40, 5), Some(false));
    assert_eq!(builder.state().known_bit(7, 40, 4), None);
    replay(&builder.finish());
}

#[test]
fn replay_requires_state_expected_and_captured_before_to_match_every_bank_and_known_mask() {
    let mut before = state();
    before.write_byte(1, 4, 30).unwrap();
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    builder.write_byte(3, 66, 1).unwrap();
    let journal = builder.finish();
    let mut different_value = before.clone();
    different_value.write_byte(1, 4, 31).unwrap();
    let mut different_knowledge = before.clone();
    different_knowledge.write_masked(7, 80, 0, 1).unwrap();
    let mut different_moment = before.clone();
    different_moment.moment += 1;
    for changed in [different_value, different_knowledge, different_moment] {
        for (mut actual, expected) in [
            (changed.clone(), before.clone()),
            (before.clone(), changed.clone()),
            (changed.clone(), changed.clone()),
        ] {
            let unchanged = actual.encode();
            assert_eq!(journal.apply_to(&mut actual, &expected), Err(NativeStoryError::StalePredecessor));
            assert_eq!(actual.encode(), unchanged);
        }
    }
    assert_eq!(replay(&journal).known_byte(1, 4), Some(30));
}

#[test]
fn a_stale_party_event_after_a_valid_assignment_fails_without_publishing_the_first_write() {
    let before = state();
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    builder.write_byte(1, 4, 37).unwrap();
    let mut journal = builder.finish();
    let mut other_party = before.clone();
    other_party.write_byte(3, 10, 2).unwrap();
    let stale = evaluate_native_party(&other_party, NativePartyOp::Add(0)).unwrap();
    journal.events.push(NativeStoryEvent::Party(stale));
    let mut actual = before.clone();
    assert_eq!(journal.apply_to(&mut actual, &before), Err(NativeStoryError::Party(NativePartyOpError::StalePredecessor)));
    assert_eq!(actual, before, "the first replayed assignment was staged, not published");
}

#[test]
fn malformed_assignment_records_fail_atomically_including_after_byte_validation() {
    let before = state();
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    builder.write_byte(1, 4, 37).unwrap();
    builder.write_masked(3, 66, 1, 1).unwrap();
    let good = builder.finish();
    let write = *assignment(&good.events()[1]);
    for bad in [
        NativeStoryAssignment { bank: 4, ..write },
        NativeStoryAssignment { mask: 0, ..write },
        NativeStoryAssignment { value: 2, ..write },
        NativeStoryAssignment { before: (0, 255), ..write },
        NativeStoryAssignment { after: (0, 1), ..write },
    ] {
        let mut journal = good.clone();
        journal.events[1] = NativeStoryEvent::Assignment(bad);
        let mut actual = before.clone();
        assert_eq!(journal.apply_to(&mut actual, &before), Err(NativeStoryError::InconsistentAssignment));
        assert_eq!(actual, before);
    }
}

#[test]
fn replay_refuses_an_altered_afterimage_or_missing_event_without_partial_publication() {
    let before = state();
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    builder.write_byte(1, 4, 37).unwrap();
    let good = builder.finish();
    let mut changed_afterimage = good.clone();
    changed_afterimage.after.write_masked(11, 80, 0, 1).unwrap();
    let mut missing_event = good;
    missing_event.events.clear();
    for journal in [changed_afterimage, missing_event] {
        let mut actual = before.clone();
        assert_eq!(journal.apply_to(&mut actual, &before), Err(NativeStoryError::InconsistentAfterimage));
        assert_eq!(actual, before);
    }
}

#[test]
fn a_cloned_multiwrite_reads_its_current_overlay_for_overlapping_gtpye_destinations() {
    let before = state();
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    let mut staged = builder.clone();
    // GTPYE-style sequential reads from raw9,10,11 into raw10,11 and adjacent byte12.
    // The second and third reads see earlier destination writes, not a captured tuple.
    for (source, destination) in [(9, 10), (10, 11), (11, 12)] {
        let value = staged.state().known_byte(3, source).unwrap();
        staged.write_byte(4, destination, value).unwrap();
    }
    assert_eq!(builder.state(), &before);
    assert!(builder.events().is_empty());
    for address in 9..=12 { assert_eq!(staged.state().known_byte(3, address), Some(0)); }
    assert_eq!(staged.state().native_party(), before.native_party());
    let staged_before_failure = staged.snapshot();
    let mut failed_instruction = staged.clone();
    failed_instruction.write_byte(7, 20, 99).unwrap();
    assert_eq!(failed_instruction.write_byte(5, 20, 1), Err(NativeStoryError::Bank(StoryWriteError::NonPersistentBank(5))));
    assert_eq!(staged.snapshot(), staged_before_failure, "discarding a failed staged instruction preserves the original builder");
    builder = staged;
    assert_eq!(builder.events().len(), 3);
    assert_eq!(builder.state().known_mask(7, 20), 0);
    replay(&builder.finish());
}

#[test]
fn a_failed_party_operation_leaves_prior_assignments_and_the_overlay_intact() {
    let before = StoryState::at(69).into_native_party().unwrap();
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    builder.write_byte(3, 9, 0).unwrap();
    let snapshot = builder.snapshot();
    assert_eq!(builder.party(NativePartyOp::Add(1)), Err(NativeStoryError::Party(NativePartyOpError::UnknownMainSlot(0))));
    assert_eq!(builder.snapshot(), snapshot);
    assert_eq!(builder.party(NativePartyOp::Lock { character: 16, locked: true }),
        Err(NativeStoryError::Party(NativePartyOpError::UnsupportedSelector(16))));
    assert_eq!(builder.snapshot(), snapshot);
}

#[test]
fn snapshots_are_immutable_and_a_post_battle_rebase_starts_a_new_empty_segment() {
    let before = state();
    let mut builder = NativeStoryBuilder::new(&before).unwrap();
    builder.write_byte(1, 4, 37).unwrap();
    let checkpoint = builder.snapshot();
    let captured = checkpoint.clone();
    builder.party(NativePartyOp::Add(2)).unwrap();
    assert_eq!(checkpoint, captured);
    assert_eq!(checkpoint.events().len(), 1);
    assert_eq!(checkpoint.after().native_party().unwrap().known_main(), Some([0, 1, 255]));
    let final_snapshot = builder.snapshot();
    assert_eq!(builder.finish(), final_snapshot);
    let mut after_battle = replay(&checkpoint);
    after_battle.write_byte(1, 80, 95).unwrap();
    let rebased = NativeStoryBuilder::new(&after_battle).unwrap();
    assert_eq!(rebased.before(), &after_battle);
    assert_eq!(rebased.state(), &after_battle);
    assert!(rebased.events().is_empty());
    assert!(!rebased.changed() && !rebased.any_change());
    let journal = rebased.finish();
    assert!(journal.events().is_empty());
    assert_eq!(replay(&journal), after_battle);
    let mut stale = checkpoint.before().clone();
    assert_eq!(journal.apply_to(&mut stale, checkpoint.before()), Err(NativeStoryError::StalePredecessor));
}

#[test]
fn legacy_builders_and_replay_inputs_require_explicit_v4_promotion() {
    let journal = NativeStoryBuilder::new(&state()).unwrap().finish();
    for bytes in [vec![1, 69, 0], vec![2, 69, 0, 1, 3, 9, 2], vec![3, 69, 0, 0, 0]] {
        let old = StoryState::decode(&bytes).unwrap();
        assert!(matches!(NativeStoryBuilder::new(&old), Err(NativeStoryError::RequiresNativeParty)));
        let mut actual = old.clone();
        assert_eq!(journal.apply_to(&mut actual, &old), Err(NativeStoryError::RequiresNativeParty));
        assert_eq!(actual.encode(), bytes);
        let mut native = journal.before().clone();
        assert_eq!(journal.apply_to(&mut native, &old), Err(NativeStoryError::RequiresNativeParty));
        assert_eq!(&native, journal.before());
    }
}
