use super::*;
use crate::story::{compare_exchange_progression, AreaStory, BLOB_VERSION_BANKS};

fn blob(moment: u16, entries: &[[u8; 4]], party: [u8; NATIVE_PARTY_BYTES]) -> Vec<u8> {
    let mut bytes = vec![BLOB_VERSION_NATIVE_PARTY];
    bytes.extend(moment.to_le_bytes());
    bytes.extend(u16::try_from(entries.len()).unwrap().to_le_bytes());
    for entry in entries { bytes.extend(entry); }
    bytes.extend(party);
    bytes
}

fn empty_v4() -> StoryState {
    StoryState::decode(&blob(69, &[], NativePartyState::unknown().encode())).unwrap()
}

#[test]
fn unknown_slots_are_not_cloud_or_empty_and_indices_are_checked() {
    let mut party = NativePartyState::unknown();
    assert_eq!(party, NativePartyState::default());
    assert_eq!(party.main_slots(), ([0; 3], 0));
    assert_eq!(party.known_main(), None);
    for slot in 0..3 { assert_eq!(party.known_main_slot(slot), Ok(None)); }
    for slot in [3, 8, 255] {
        assert_eq!(party.known_main_slot(slot), Err(NativePartyError::SlotOutOfRange(slot)));
    }
    for bit in 0..16 {
        assert_eq!(party.known_available(bit), Ok(None));
        assert_eq!(party.known_locked(bit), Ok(None));
    }
    for bit in [16, 31, 255] {
        assert_eq!(party.known_available(bit), Err(NativePartyError::BitOutOfRange(bit)));
        assert_eq!(party.known_locked(bit), Err(NativePartyError::BitOutOfRange(bit)));
    }
    assert!(party.write_main_slots([255, 0, 0], 1).unwrap());
    assert_eq!(party.known_main_slot(0), Ok(Some(255)));
    assert_eq!(party.known_main_slot(1), Ok(None));
    assert!(party.write_main_slots([0, 0, 0], 2).unwrap(), "an explicit Cloud establishes knowledge even though its byte was zero");
    assert_eq!(party.known_main_slot(1), Ok(Some(0)));
    assert_eq!(party.known_main(), None);
    assert!(party.write_main_slots([0, 0, 255], 4).unwrap());
    assert_eq!(party.known_main(), Some([255, 0, 255]));
    assert!(!party.write_main_slots([255, 0, 255], 7).unwrap(), "an identical assignment changes nothing");
}

#[test]
fn slot_assignments_allow_holes_and_atomic_reorders_but_never_duplicate_members() {
    let mut party = NativePartyState::from_parts([0, 2, 3], 7, (0, 0), (0, 0)).unwrap();
    assert!(party.write_main_slots([3, 0, 2], 7).unwrap(), "validate the complete reorder, not its intermediate slots");
    assert_eq!(party.known_main(), Some([3, 0, 2]));
    assert!(party.write_main_slots([255, 255, 2], 7).unwrap(), "multiple empty slots are valid");
    assert!(party.write_main_slots([0, 255, 2], 7).unwrap(), "do not compact a hole");
    let before = party;
    for (values, assigned, error) in [
        ([2, 0, 0], 1, NativePartyError::DuplicateMember(2)),
        ([0, 8, 9], 6, NativePartyError::InvalidMember(9)),
        ([0, 254, 0], 2, NativePartyError::InvalidMember(254)),
        ([0, 0, 0], 8, NativePartyError::InvalidSlotMask(8)),
        ([0, 0, 0], 255, NativePartyError::InvalidSlotMask(255)),
    ] {
        assert_eq!(party.write_main_slots(values, assigned), Err(error));
        assert_eq!(party, before, "a refused multi-slot assignment leaves every field unchanged");
    }
    assert!(!party.write_main_slots([9, 254, 254], 0).unwrap(), "unassigned input bytes are not assignments");
    assert!(!party.write_main_slots([0, 254, 254], 1).unwrap());
    assert_eq!(party, before);
}

#[test]
fn constructing_an_image_refuses_invalid_or_noncanonical_values() {
    for member in [9, 127, 254] {
        assert_eq!(NativePartyState::from_parts([member, 0, 0], 1, (0, 0), (0, 0)),
            Err(NativePartyError::InvalidMember(member)));
    }
    assert_eq!(NativePartyState::from_parts([0; 3], 8, (0, 0), (0, 0)), Err(NativePartyError::InvalidSlotMask(8)));
    assert_eq!(NativePartyState::from_parts([2, 2, 255], 7, (0, 0), (0, 0)), Err(NativePartyError::DuplicateMember(2)));
    for (slots, known, availability, locks) in [
        ([0, 255, 0], 1, (0, 0), (0, 0)),
        ([0; 3], 0, (2, 0), (0, 0)),
        ([0; 3], 0, (0, 0), (0x8000, 0x7FFF)),
    ] {
        assert_eq!(NativePartyState::from_parts(slots, known, availability, locks), Err(NativePartyError::NoncanonicalUnknown));
    }
    assert!(NativePartyState::from_parts([8, 255, 255], 7, (0, u16::MAX), (u16::MAX, u16::MAX)).is_ok());
}

#[test]
fn availability_and_locks_establish_selected_bits_without_inventing_other_resources() {
    let mut party = NativePartyState::unknown();
    assert!(party.write_availability_masked(0, 1 << 2), "an authored zero establishes unavailable");
    assert_eq!(party.known_available(2), Ok(Some(false)));
    assert_eq!(party.known_locked(2), Ok(None));
    assert_eq!(party.known_main(), None);
    assert!(!party.write_availability_masked(u16::MAX, 0));
    assert!(party.write_availability_masked(u16::MAX, 1 << 15));
    assert_eq!(party.availability_bits(), (0x8000, 0x8004));
    assert_eq!(party.known_available(15), Ok(Some(true)), "retain the native high bit rather than imposing a nine-character mask");
    assert_eq!(party.known_available(1), Ok(None));
    assert!(party.write_locks_masked(1 << 2, (1 << 2) | (1 << 15)));
    assert_eq!(party.locking_bits(), (4, 0x8004));
    assert_eq!(party.known_locked(2), Ok(Some(true)), "a lock does not require availability");
    assert_eq!(party.known_locked(15), Ok(Some(false)));
    assert_eq!(party.availability_bits(), (0x8000, 0x8004));
    assert!(party.write_availability_masked(0, 1 << 15));
    assert!(party.write_locks_masked(0, 1 << 2));
    assert_eq!(party.availability_bits(), (0, 0x8004));
    assert_eq!(party.locking_bits(), (0, 0x8004));
    assert!(!party.write_availability_masked(0, 0x8004));
    assert!(!party.write_locks_masked(0, 0x8004));
}

#[test]
fn suffix_has_exact_order_and_little_endian_masks() {
    let party = NativePartyState::from_parts([0, 255, 8], 7, (0x8004, 0xC00C), (0x1201, 0x3203)).unwrap();
    let bytes = [7, 0, 255, 8, 4, 128, 12, 192, 1, 18, 3, 50];
    assert_eq!(party.encode(), bytes);
    assert_eq!(NativePartyState::decode(&bytes), Some(party));
    for end in 0..bytes.len() { assert_eq!(NativePartyState::decode(&bytes[..end]), None); }
    let mut extra = bytes.to_vec();
    extra.push(0);
    assert_eq!(NativePartyState::decode(&extra), None);
}

#[test]
fn v4_round_trip_preserves_bank_aliases_partial_masks_holes_and_independent_main_slots() {
    let entries = [[1, 3, 50, 255], [3, 9, 2, 255], [7, 255, 0, 128], [13, 255, 128, 128]];
    let suffix = [7, 0, 255, 2, 0, 128, 4, 128, 128, 0, 129, 0];
    let bytes = blob(0x1234, &entries, suffix);
    let state = StoryState::decode(&bytes).expect("a canonical v4 bank image and native suffix");
    assert!(state.supports_extended() && state.supports_native_party());
    assert_eq!(state.moment, 0x1234);
    assert_eq!(state.known_byte(2, 3), Some(50));
    assert_eq!(state.known_byte(4, 9), Some(2));
    assert_eq!(state.known_bit(15, 255, 7), Some(false));
    assert_eq!(state.known_bit(14, 255, 7), Some(true));
    assert_eq!(state.known_mask(13, 254), 0);
    let party = state.native_party().unwrap();
    assert_eq!(party.known_main(), Some([0, 255, 2]));
    assert_eq!(party.availability_bits(), (0x8000, 0x8004));
    assert_eq!(party.locking_bits(), (0x0080, 0x0081));
    assert_eq!(state.encode(), bytes);
    assert_eq!(state.clone().into_native_party().unwrap(), state, "promotion is idempotent");
    assert_eq!(state.clone().into_extended().unwrap(), state, "bank helpers cannot downgrade native resources");
}

#[test]
fn v4_rejects_truncation_extra_bytes_and_every_noncanonical_suffix_form() {
    let good = blob(69, &[[3, 9, 0, 255]], NativePartyState::unknown().encode());
    for end in 1..good.len() {
        assert!(StoryState::decode(&good[..end]).is_none(), "truncated at {end}");
    }
    let mut extra = good.clone();
    extra.push(0);
    assert!(StoryState::decode(&extra).is_none());
    for suffix in [
        [8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],   // a fourth main-slot mask bit
        [0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],   // an unknown slot contains a value
        [1, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],   // an unsupported member
        [1, 254, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], // an opcode sentinel is not stored empty
        [3, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0],   // duplicate known members
        [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0],   // availability outside its known mask
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0], // lock outside its known mask
    ] {
        assert!(StoryState::decode(&blob(69, &[], suffix)).is_none(), "{suffix:?}");
    }
}

#[test]
fn v4_retains_the_strict_bank_record_validation() {
    for entries in [
        vec![[2, 3, 0, 255]],                     // noncanonical bank alias
        vec![[1, 0, 0, 255]],                     // moment belongs to the header
        vec![[1, 1, 0, 255]],
        vec![[5, 9, 0, 255]],                     // visit scratch is not persistent
        vec![[8, 9, 0, 255]],                     // special bank
        vec![[3, 9, 0, 0]],                       // no known bits
        vec![[3, 9, 2, 1]],                       // value outside known bits
        vec![[3, 9, 0, 255], [3, 9, 0, 255]],     // duplicate key
        vec![[7, 9, 0, 255], [3, 9, 0, 255]],     // unsorted keys
    ] {
        assert!(StoryState::decode(&blob(69, &entries, NativePartyState::unknown().encode())).is_none(), "{entries:?}");
    }
    let good = blob(69, &[[3, 9, 0, 255]], NativePartyState::unknown().encode());
    for count in [0u16, 2, 1279, u16::MAX] {
        let mut bad = good.clone();
        bad[3..5].copy_from_slice(&count.to_le_bytes());
        assert!(StoryState::decode(&bad).is_none(), "incorrect count {count}");
    }
}

#[test]
fn legacy_codecs_stay_byte_identical_and_native_promotion_does_not_infer_a_party() {
    for bytes in [
        vec![1, 69, 0],
        vec![2, 69, 0, 1, 3, 9, 2],
        vec![3, 69, 0, 3, 0, 3, 9, 0, 255, 3, 10, 2, 255, 3, 11, 255, 255],
    ] {
        let old = StoryState::decode(&bytes).unwrap();
        assert_eq!(old.encode(), bytes);
        assert!(!old.supports_native_party());
        assert_eq!(old.native_party(), None);
        let bank_image = old.clone().into_extended().unwrap().encode();
        let mut promoted = old.into_native_party().unwrap();
        assert_eq!(promoted.native_party(), Some(&NativePartyState::unknown()));
        assert_eq!(promoted.native_party_mut().unwrap().known_main(), None, "neither the moment nor complete raw slots establish main slots");
        let mut promoted_banks = promoted.encode();
        promoted_banks.truncate(promoted_banks.len() - NATIVE_PARTY_BYTES);
        promoted_banks[0] = BLOB_VERSION_BANKS;
        assert_eq!(promoted_banks, bank_image, "native promotion preserves every bank value and known mask");
        assert_eq!(StoryState::decode(&promoted.encode()), Some(promoted));
    }
}

#[test]
fn native_promotion_cannot_bypass_conflicting_or_unsupported_legacy_banks() {
    for (bytes, error) in [
        (vec![2, 36, 0, 1, 5, 9, 0x82], StoryWriteError::NonPersistentBank(5)),
        (vec![2, 36, 0, 2, 1, 225, 1, 2, 225, 32], StoryWriteError::ConflictingLegacyAlias { bank: 1, address: 225 }),
    ] {
        let old = StoryState::decode(&bytes).unwrap();
        assert_eq!(old.clone().into_native_party(), Err(error));
        assert_eq!(old.encode(), bytes);
    }
}

#[test]
fn a_complete_v4_image_is_exactly_5129_bytes_and_fits_the_wire_limit() {
    let mut entries = Vec::new();
    for bank in [1u8, 3, 7, 11, 13] {
        for address in 0..=255u8 {
            if bank == 1 && address < 2 { continue; }
            entries.push([bank, address, address.wrapping_add(bank), 255]);
        }
    }
    let party = NativePartyState::from_parts([8, 255, 0], 7, (0x8001, u16::MAX), (0x0100, u16::MAX)).unwrap();
    let bytes = blob(0x1234, &entries, party.encode());
    assert_eq!(bytes.len(), 5129);
    assert_eq!(bytes.len(), MAX_NATIVE_PARTY_BLOB_BYTES);
    assert!(bytes.len() < 8192);
    let state = StoryState::decode(&bytes).unwrap();
    assert_eq!(state.native_party(), Some(&party));
    assert_eq!(state.known_word(14, 254), Some(u16::from_le_bytes([11, 12])));
    assert_eq!(state.encode(), bytes);
}

#[test]
fn raw_field_slots_and_main_slots_never_synchronize_implicitly() {
    let mut state = empty_v4();
    for (address, value) in [(9, 0), (10, 3), (11, 255)] { state.write_byte(3, address, value).unwrap(); }
    assert_eq!(state.native_party(), Some(&NativePartyState::unknown()));
    state.native_party_mut().unwrap().write_main_slots([2, 255, 0], 7).unwrap();
    assert_eq!([state.known_byte(4, 9), state.known_byte(4, 10), state.known_byte(4, 11)], [Some(0), Some(3), Some(255)]);
    let party = *state.native_party().unwrap();
    state.write_byte(4, 9, 8).unwrap();
    assert_eq!(state.native_party(), Some(&party));
    assert_eq!(party.availability_bits(), (0, 0), "active membership does not establish availability");
    assert_eq!(party.locking_bits(), (0, 0));
    assert_eq!(StoryState::decode(&state.encode()), Some(state));
}

#[test]
fn exact_replacement_allows_native_clears_and_reorders_but_rejects_stale_predecessors() {
    let mut state = empty_v4();
    *state.native_party_mut().unwrap() = NativePartyState::from_parts([0, 2, 3], 7, (0x800D, u16::MAX), (5, u16::MAX)).unwrap();
    state.write_byte(3, 66, 1).unwrap();
    let before = state.encode();
    let party = state.native_party_mut().unwrap();
    party.write_main_slots([3, 255, 0], 7).unwrap();
    party.write_availability_masked(0, u16::MAX);
    party.write_locks_masked(0, u16::MAX);
    state.write_byte(3, 66, 0).unwrap();
    let after = state.encode();
    assert_eq!(compare_exchange_progression(&before, &before, &after), Some(after.clone()));
    assert_eq!(compare_exchange_progression(&after, &before, &before), None);
    assert_eq!(compare_exchange_progression(&after, &after, &after), Some(after));
}

#[test]
fn exact_replacement_rejects_loss_of_any_native_knowledge_or_schema_downgrade() {
    let mut old = empty_v4();
    *old.native_party_mut().unwrap() = NativePartyState::from_parts([0, 2, 255], 7, (5, u16::MAX), (1, u16::MAX)).unwrap();
    let before = old.encode();
    for party in [
        NativePartyState::from_parts([0, 2, 255], 6, (5, u16::MAX), (1, u16::MAX)).unwrap(),
        NativePartyState::from_parts([0, 2, 255], 7, (5, !2u16), (1, u16::MAX)).unwrap(),
        NativePartyState::from_parts([0, 2, 255], 7, (5, u16::MAX), (1, !2u16)).unwrap(),
    ] {
        let mut lossy = old.clone();
        *lossy.native_party_mut().unwrap() = party;
        assert_eq!(compare_exchange_progression(&before, &before, &lossy.encode()), None, "knowledge of zero is still knowledge");
    }
    for downgraded in [vec![1, 69, 0], vec![2, 69, 0, 1, 3, 66, 1], vec![3, 69, 0, 0, 0]] {
        assert_eq!(compare_exchange_progression(&before, &before, &downgraded), None);
        let unknown = empty_v4().encode();
        assert_eq!(compare_exchange_progression(&unknown, &unknown, &downgraded), None, "v4 cannot silently downgrade even when every native value is unknown");
    }
    old.write_masked(7, 20, 0, 0x80).unwrap();
    let with_bank_knowledge = old.encode();
    assert_eq!(compare_exchange_progression(&with_bank_knowledge, &with_bank_knowledge, &before), None, "the native suffix cannot excuse losing bank knowledge");
}

#[test]
fn exact_replacement_accepts_explicit_legacy_promotion_without_guessing_native_values() {
    for before in [vec![1, 69, 0], vec![2, 69, 0, 1, 3, 9, 2], vec![3, 69, 0, 0, 0]] {
        let next = StoryState::decode(&before).unwrap().into_native_party().unwrap();
        assert_eq!(next.native_party(), Some(&NativePartyState::unknown()));
        let after = next.encode();
        assert_eq!(compare_exchange_progression(&before, &before, &after), Some(after));
    }
}

#[test]
fn checked_native_replacement_is_atomic_and_never_promotes_an_old_state() {
    let expected = NativePartyState::from_parts([0, 2, 255], 7, (5, u16::MAX), (1, u16::MAX)).unwrap();
    let replacement = NativePartyState::from_parts([2, 255, 0], 7, (0, u16::MAX), (0, u16::MAX)).unwrap();
    let mut state = empty_v4();
    state.write_byte(3, 9, 3).unwrap();
    *state.native_party_mut().unwrap() = expected;
    let before = state.encode();
    assert_eq!(state.replace_native_party(&NativePartyState::unknown(), replacement), Err(NativePartyError::StaleResources));
    assert_eq!(state.encode(), before);
    assert_eq!(state.replace_native_party(&expected, NativePartyState::unknown()), Err(NativePartyError::KnowledgeLoss));
    assert_eq!(state.encode(), before);
    assert_eq!(state.replace_native_party(&expected, replacement), Ok(true));
    assert_eq!(state.native_party(), Some(&replacement));
    assert_eq!(state.known_byte(3, 9), Some(3), "native replacement does not synchronize the raw slots");
    let after = state.encode();
    assert_eq!(state.replace_native_party(&expected, expected), Err(NativePartyError::StaleResources));
    assert_eq!(state.encode(), after);
    assert_eq!(state.replace_native_party(&replacement, replacement), Ok(false));
    for bytes in [vec![1, 69, 0], vec![2, 69, 0, 1, 3, 9, 2], vec![3, 69, 0, 0, 0]] {
        let mut old = StoryState::decode(&bytes).unwrap();
        assert_eq!(old.replace_native_party(&NativePartyState::unknown(), replacement), Err(NativePartyError::MissingResources));
        assert_eq!(old.encode(), bytes, "a missing resource is not permission to promote or guess it");
    }
}

#[test]
fn checked_party_refuses_malformed_or_unknown_native_state_and_preserves_empty_holes_and_order() {
    for bad in [vec![255], vec![3, 36, 0, 1, 0], vec![4, 36, 0, 0, 0]] {
        assert_eq!(checked_canon_party_of(&bad), None, "invalid data cannot supply a fallback party");
    }
    let mut state = empty_v4();
    state.moment = 35;
    assert_eq!(checked_canon_party_of(&state.encode()), None, "the old table cannot fill unknown native slots");
    state.native_party_mut().unwrap().write_main_slots([0, 0, 0], 1).unwrap();
    assert_eq!(checked_canon_party_of(&state.encode()), None, "one known member does not establish the remaining slots");
    state.native_party_mut().unwrap().write_main_slots([255; 3], 7).unwrap();
    assert_eq!(checked_canon_party_of(&state.encode()), Some(vec![]), "known empty is distinct from unreadable or unknown");
    state.native_party_mut().unwrap().write_main_slots([2, 255, 0], 7).unwrap();
    for moment in [0, 35, 36, 108, u16::MAX] {
        state.moment = moment;
        assert_eq!(checked_canon_party_of(&state.encode()), Some(vec![2, 0]), "stored Tifa then Cloud at moment {moment}");
    }
}

#[test]
fn checked_party_keeps_the_explicit_legacy_table_and_empty_blob_policy() {
    assert_eq!(checked_canon_party_of(&[]), Some(vec![0]), "empty is an explicitly supported legacy new-game blob");
    for (moment, expected) in [(0u16, vec![0]), (10, vec![0]), (11, vec![0, 1]), (35, vec![0, 1]), (36, vec![0]), (108, vec![0])] {
        let [lo, hi] = moment.to_le_bytes();
        for bytes in [
            vec![1, lo, hi],
            vec![2, lo, hi, 1, 3, 9, 2],
            vec![3, lo, hi, 3, 0, 3, 9, 2, 255, 3, 10, 255, 255, 3, 11, 0, 255],
        ] {
            assert_eq!(checked_canon_party_of(&bytes), Some(expected.clone()), "legacy raw slots do not replace the table: {bytes:?}");
        }
    }
}

#[test]
fn phase_keys_distinguish_native_knownness_values_masks_and_slot_order() {
    let area = AreaStory::default();
    let base = empty_v4();
    let mut cases = vec![base.clone()];
    for party in [
        NativePartyState::from_parts([0, 0, 0], 1, (0, 0), (0, 0)).unwrap(),
        NativePartyState::from_parts([255, 0, 0], 1, (0, 0), (0, 0)).unwrap(),
        NativePartyState::from_parts([0, 0, 0], 2, (0, 0), (0, 0)).unwrap(),
        NativePartyState::from_parts([0, 2, 255], 7, (0, 0), (0, 0)).unwrap(),
        NativePartyState::from_parts([2, 0, 255], 7, (0, 0), (0, 0)).unwrap(),
        NativePartyState::from_parts([0; 3], 0, (0, 1), (0, 0)).unwrap(),
        NativePartyState::from_parts([0; 3], 0, (1, 1), (0, 0)).unwrap(),
        NativePartyState::from_parts([0; 3], 0, (0, 0x8000), (0, 0)).unwrap(),
        NativePartyState::from_parts([0; 3], 0, (0, 0), (0, 1)).unwrap(),
        NativePartyState::from_parts([0; 3], 0, (0, 0), (1, 1)).unwrap(),
    ] {
        let mut state = base.clone();
        *state.native_party_mut().unwrap() = party;
        cases.push(state);
    }
    let keys: Vec<_> = cases.iter().map(|state| area.phase_key(&state.encode()).expect("a valid party phase")).collect();
    for (index, key) in keys.iter().enumerate() {
        for (other_index, other) in keys[..index].iter().enumerate() {
            assert_ne!(key, other, "native states {index} and {other_index} must not share a phase");
        }
    }
    assert_ne!(area.phase_key(&[3, 69, 0, 0, 0]), Some(keys[0].clone()), "the native resource schema is part of the phase identity");
}
