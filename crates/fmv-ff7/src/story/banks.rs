//! Persistent field banks with explicit knowledge. Version 3 is deliberately opt-in: reading an
//! older save neither invents its missing choices nor rewrites it. Callers establish values only
//! from authored writes or verified initialization, then save against the exact predecessor.
//!
//! Bank aliases follow the FF7 reverse-engineering memory map:
//! <https://ff7-mods.github.io/ff7-flat-wiki/FF7/Kernel/Memory_management.html>.
//! Temporary banks 5/6 and special read-only banks 8/9/10 are not persistent story data.

use super::{StoryState, BTreeMap, TRACKED_VARS, MOMENT_BANK, NativePartyState, BLOB_VERSION_NATIVE_PARTY, NATIVE_PARTY_BYTES};

pub const BLOB_VERSION_BANKS: u8 = 3;
/// Five 256-byte banks, excluding the two moment bytes carried in the header.
const MAX_BANK_BYTES: usize = 5 * 256 - 2;
/// `[3, moment:u16le, count:u16le, (bank,address,value,known_mask)*count]`.
/// Banks are canonical, keys strictly ascending, masks nonzero, unknown bits zero.
pub const MAX_BANK_BLOB_BYTES: usize = 5 + 4 * MAX_BANK_BYTES;

/// The physical persistent byte bank for a script's bank selector. The selectors in each pair
/// address the same bytes. Storage APIs specify their width explicitly; a script interpreter
/// must resolve its original selector's access width before canonicalizing the byte address.
pub fn canonical_persistent_bank(bank: u8) -> Option<u8> {
    match bank {
        1 | 2 => Some(1),
        3 | 4 => Some(3),
        7 | 15 => Some(7),
        11 | 12 => Some(11),
        13 | 14 => Some(13),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct KnownByte {
    pub value: u8,
    pub mask: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StoryWriteError {
    #[error("bank {0} is not a persistent field bank")]
    NonPersistentBank(u8),
    #[error("legacy bank aliases disagree at bank {bank}, address {address}")]
    ConflictingLegacyAlias { bank: u8, address: u8 },
    #[error("a word at address 255 crosses the bank boundary")]
    WordCrossesBank,
    #[error("numeric input at bank {bank}, address {address} is not completely known")]
    UnknownValue { bank: u8, address: u8 },
    #[error("numeric update is outside its declared bounds")]
    OutOfRange,
}

impl StoryState {
    /// Whether this state carries known-bit persistent banks (versions 3 and 4).
    pub fn supports_extended(&self) -> bool { self.banks.is_some() }

    /// The bits known at this address. Zero means unknown, not an authored zero.
    /// Legacy states retain their previous raw-bank interpretation until explicitly promoted.
    pub fn known_mask(&self, bank: u8, address: u8) -> u8 {
        if self.supports_extended() { return self.bank_byte(bank, address).mask; }
        if bank == MOMENT_BANK && address < 2 { return 0xFF; }
        super::tracked_var(bank, address).map_or(0, |t| t.bits)
    }

    pub(super) fn bank_byte(&self, bank: u8, address: u8) -> KnownByte {
        let Some(bank) = canonical_persistent_bank(bank) else { return KnownByte::default() };
        if bank == 1 && address < 2 {
            let [lo, hi] = self.moment.to_le_bytes();
            return KnownByte { value: if address == 0 { lo } else { hi }, mask: 0xFF };
        }
        self.banks.as_ref().and_then(|b| b.get(&(bank, address))).copied().unwrap_or_default()
    }

    /// Promote without inventing defaults. Existing tracked masks remain known; other legacy set
    /// bits remain set, with their zero bits unknown. Unsupported or conflicting legacy entries
    /// refuse promotion rather than silently discarding data. This does not write a save.
    pub fn into_extended(mut self) -> Result<Self, StoryWriteError> {
        if self.supports_extended() { return Ok(self); }
        let mut banks = BTreeMap::new();
        for tracked in TRACKED_VARS {
            banks.insert((tracked.bank, tracked.address), KnownByte { value: self.var(tracked.bank, tracked.address) & tracked.bits, mask: tracked.bits });
        }
        for (&(raw, address), &value) in &self.vars {
            let bank = canonical_persistent_bank(raw).ok_or(StoryWriteError::NonPersistentBank(raw))?;
            if bank == 1 && address < 2 {
                let [lo, hi] = self.moment.to_le_bytes();
                if value != if address == 0 { lo } else { hi } {
                    return Err(StoryWriteError::ConflictingLegacyAlias { bank, address });
                }
                continue;
            }
            let byte = banks.entry((bank, address)).or_default();
            // Legacy aliases may be distinct keys. Never choose between conflicting known bits.
            if (byte.value ^ value) & byte.mask & value != 0 {
                return Err(StoryWriteError::ConflictingLegacyAlias { bank, address });
            }
            byte.value |= value;
            byte.mask |= value;
        }
        self.vars.clear();
        self.banks = Some(banks);
        Ok(self)
    }

    /// Apply an authored masked assignment, preserving all other values and their knowledge.
    /// A write may establish previously unknown bits. Promotion and the write are atomic on error.
    /// No persistent bank is initialized merely because another byte was written.
    pub fn write_masked(&mut self, bank: u8, address: u8, value: u8, mask: u8) -> Result<bool, StoryWriteError> {
        let bank = canonical_persistent_bank(bank).ok_or(StoryWriteError::NonPersistentBank(bank))?;
        if mask == 0 { return Ok(false); }
        let mut next = self.clone().into_extended()?;
        if bank == 1 && address < 2 {
            let mut moment = next.moment.to_le_bytes();
            let [lo, hi] = &mut moment;
            let byte = if address == 0 { lo } else { hi };
            *byte = (*byte & !mask) | (value & mask);
            next.moment = u16::from_le_bytes(moment);
        } else if let Some(banks) = &mut next.banks {
            let byte = banks.entry((bank, address)).or_default();
            byte.value = (byte.value & !mask) | (value & mask);
            byte.mask |= mask;
        }
        let changed = *self != next;
        *self = next;
        Ok(changed)
    }

    /// Assign a whole byte, including an authored zero, without reading its previous value.
    pub fn write_byte(&mut self, bank: u8, address: u8, value: u8) -> Result<bool, StoryWriteError> {
        self.write_masked(bank, address, value, 0xFF)
    }

    /// Assign an unaligned little-endian word atomically. A word cannot wrap into address zero.
    pub fn write_word(&mut self, bank: u8, address: u8, value: u16) -> Result<bool, StoryWriteError> {
        let high = address.checked_add(1).ok_or(StoryWriteError::WordCrossesBank)?;
        let mut next = self.clone();
        let [lo, hi] = value.to_le_bytes();
        next.write_byte(bank, address, lo)?;
        next.write_byte(bank, high, hi)?;
        let changed = *self != next;
        *self = next;
        Ok(changed)
    }

    /// Ordered bounded numeric change (e.g. an affection update). The caller supplies bounds
    /// established from the relevant opcode; unknown inputs and out-of-range results are refused.
    /// This does not choose wrapping or saturation semantics on behalf of an opcode interpreter.
    pub fn add_byte_checked(&mut self, bank: u8, address: u8, delta: i32, min: u8, max: u8) -> Result<bool, StoryWriteError> {
        let value = self.known_byte(bank, address).ok_or(StoryWriteError::UnknownValue { bank, address })?;
        let value = i32::from(value).checked_add(delta).filter(|v| (i32::from(min)..=i32::from(max)).contains(v))
            .ok_or(StoryWriteError::OutOfRange)?;
        self.write_byte(bank, address, value as u8)
    }
}

#[allow(clippy::chunks_exact_to_as_chunks)]
pub(super) fn decode(blob: &[u8]) -> Option<StoryState> {
    let [version @ (BLOB_VERSION_BANKS | BLOB_VERSION_NATIVE_PARTY), lo, hi, count_lo, count_hi, rest @ ..] = blob else { return None };
    let count = usize::from(u16::from_le_bytes([*count_lo, *count_hi]));
    let bank_len = count.checked_mul(4)?;
    let suffix_len = if *version == BLOB_VERSION_NATIVE_PARTY { NATIVE_PARTY_BYTES } else { 0 };
    if count > MAX_BANK_BYTES || rest.len() != bank_len.checked_add(suffix_len)? { return None; }
    let native_party = if *version == BLOB_VERSION_NATIVE_PARTY {
        Some(NativePartyState::decode(rest.get(bank_len..)?)?)
    } else { None };
    let mut banks = BTreeMap::new();
    let mut previous = None;
    for entry in rest.get(..bank_len)?.chunks_exact(4) {
        let [bank, address, value, mask] = entry else { return None };
        let key = (*bank, *address);
        if canonical_persistent_bank(*bank) != Some(*bank) || (*bank == 1 && *address < 2)
            || *mask == 0 || value & !mask != 0 || previous.is_some_and(|p| p >= key) { return None; }
        banks.insert(key, KnownByte { value: *value, mask: *mask });
        previous = Some(key);
    }
    Some(StoryState { moment: u16::from_le_bytes([*lo, *hi]), vars: BTreeMap::new(), banks: Some(banks), native_party })
}

pub(super) fn encode(moment: u16, banks: &BTreeMap<(u8, u8), KnownByte>, native_party: Option<NativePartyState>) -> Vec<u8> {
    let mut bytes = vec![if native_party.is_some() { BLOB_VERSION_NATIVE_PARTY } else { BLOB_VERSION_BANKS }];
    bytes.extend(moment.to_le_bytes());
    bytes.extend((banks.len() as u16).to_le_bytes());
    for (&(bank, address), b) in banks { bytes.extend([bank, address, b.value, b.mask]); }
    if let Some(party) = native_party { bytes.extend(party.encode()); }
    bytes
}

/// Validate a story replacement against its exact expected predecessor. This is a pure check;
/// the store must perform it under the same lock/transaction as its write and scene receipt.
/// Unlike legacy union, this preserves clears and numeric assignments and rejects stale saves.
pub fn compare_exchange_progression(stored: &[u8], expected: &[u8], advanced: &[u8]) -> Option<Vec<u8>> {
    if stored != expected { return None; }
    let old = StoryState::decode(stored)?;
    let new = StoryState::decode(advanced)?;
    if old.supports_extended() && !new.supports_extended() { return None; }
    if let Some(old_party) = old.native_party() {
        if !new.native_party()?.retains_knowledge(old_party) { return None; }
    }
    if new.supports_extended() {
        let old = old.into_extended().ok()?;
        // A transaction may change a known value, but may not forget one. This also prevents a
        // stale legacy writer from presenting a newly promoted partial snapshot as a replacement.
        for (&(bank, address), byte) in old.banks.as_ref()? {
            if new.known_mask(bank, address) & byte.mask != byte.mask { return None; }
        }
    }
    Some(advanced.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::story::{merge_progression, AreaStory};

    #[test]
    fn old_saves_round_trip_without_inventing_unrecorded_choices() {
        for blob in [vec![1, 36, 0], vec![2, 27, 0, 1, 1, 225, 33]] {
            let old = StoryState::decode(&blob).unwrap();
            assert_eq!(old.encode(), blob);
            let state = old.into_extended().unwrap();
            assert_eq!(state.known_bit(3, 66, 0), None, "the flower decision was never recorded");
            assert_eq!(state.known_byte(1, 4), None, "affection is not invented from the moment");
            assert_eq!(state.known_mask(1, 225), 0xA1, "only the legacy tracked bits are known");
            assert_eq!(StoryState::decode(&state.encode()), Some(state));
        }
        // Legacy arbitrary future/unknown entries round-trip unchanged; they cannot be thrown
        // away or silently promoted into persistent banks when their meaning is unsupported.
        let blob = [2, 36, 0, 1, 5, 9, 0x82];
        let old = StoryState::decode(&blob).unwrap();
        assert_eq!(old.clone().into_extended(), Err(StoryWriteError::NonPersistentBank(5)));
        assert_eq!(old.encode(), blob);
    }

    #[test]
    fn legacy_alias_conflicts_refuse_promotion_and_do_not_mutate_the_state() {
        for blob in [
            vec![2, 36, 0, 2, 1, 225, 1, 2, 225, 32], // legacy rescue bit known clear, alias sets it
            vec![2, 36, 0, 1, 1, 0, 37],             // raw byte alias contradicts header moment
        ] {
            let mut state = StoryState::decode(&blob).unwrap();
            assert!(matches!(state.write_byte(3, 66, 1), Err(StoryWriteError::ConflictingLegacyAlias { .. })));
            assert_eq!(state.encode(), blob, "a refused promotion leaves every old byte intact");
        }
        let state = StoryState::decode(&[2, 36, 0, 2, 3, 66, 1, 4, 66, 2]).unwrap().into_extended().unwrap();
        assert_eq!((state.var(3, 66), state.known_mask(4, 66)), (3, 3), "untracked set bits survive both aliases");
        assert_eq!(state.known_bit(3, 66, 2), None, "untracked clear bits remain unknown");
    }

    #[test]
    fn a_known_zero_is_distinct_from_unknown_and_masked_writes_preserve_other_bits() {
        let mut state = StoryState::at(36);
        assert!(state.write_masked(3, 66, 0, 1).unwrap());
        assert_eq!(state.known_bit(3, 66, 0), Some(false));
        assert_eq!(state.known_bit(3, 66, 1), None);
        assert_eq!(state.known_byte(3, 66), None);
        state.write_masked(4, 66, 0x82, 0x82).unwrap();
        assert_eq!(state.known_mask(3, 66), 0x83);
        assert_eq!(state.var(4, 66), 0x82);
        state.write_masked(3, 66, 0, 2).unwrap();
        assert_eq!(state.var(4, 66), 0x80);
        assert_eq!(StoryState::decode(&state.encode()), Some(state.clone()));
        state.write_byte(3, 66, 0).unwrap();
        assert_eq!(state.known_byte(4, 66), Some(0));
        assert_eq!(state.known_byte(3, 67), None);
    }

    #[test]
    fn overlapping_bank_views_and_unaligned_words_share_bytes_without_wrapping() {
        let mut state = StoryState::at(0x1234).into_extended().unwrap();
        assert_eq!(state.known_word(1, 0), Some(0x1234));
        assert_eq!(state.known_word(2, 0), Some(0x1234));
        state.write_byte(1, 1, 0xAB).unwrap();
        assert_eq!(state.moment, 0xAB34);
        for (byte_bank, word_bank) in [(1, 2), (3, 4), (7, 15), (11, 12), (13, 14)] {
            state.write_word(word_bank, 101, 0xFEDC).unwrap();
            assert_eq!(state.known_byte(byte_bank, 101), Some(0xDC));
            assert_eq!(state.known_byte(byte_bank, 102), Some(0xFE));
            state.write_byte(byte_bank, 102, 0x12).unwrap();
            assert_eq!(state.known_word(word_bank, 101), Some(0x12DC));
        }
        let before = state.clone();
        assert_eq!(state.write_word(3, 255, 0x1234), Err(StoryWriteError::WordCrossesBank));
        for bank in [0, 5, 6, 8, 9, 10, 16, 255] {
            assert_eq!(state.write_byte(bank, 10, 42), Err(StoryWriteError::NonPersistentBank(bank)));
        }
        assert_eq!(state, before, "invalid writes are atomic");
    }

    #[test]
    fn numeric_updates_are_ordered_not_bit_unions_and_require_known_input() {
        let mut state = StoryState::at(36);
        assert!(matches!(state.add_byte_checked(1, 4, 1, 0, 100), Err(StoryWriteError::UnknownValue { .. })));
        state.write_byte(1, 4, 30).unwrap();
        state.add_byte_checked(1, 4, 5, 0, 100).unwrap();
        state.add_byte_checked(1, 4, -7, 0, 100).unwrap();
        assert_eq!(state.known_byte(1, 4), Some(28));
        let before = state.clone();
        for delta in [-29, 73, i32::MAX] {
            assert_eq!(state.add_byte_checked(1, 4, delta, 0, 100), Err(StoryWriteError::OutOfRange));
            assert_eq!(state, before);
        }
    }

    #[test]
    fn full_bank_encoding_is_bounded_canonical_and_round_trips_every_byte() {
        let mut state = StoryState::at(0x1234).into_extended().unwrap();
        for bank in [1, 3, 7, 11, 13] {
            for address in 0..=255u8 {
                state.write_byte(bank, address, address.wrapping_add(bank)).unwrap();
            }
        }
        let blob = state.encode();
        assert_eq!(blob.len(), MAX_BANK_BLOB_BYTES);
        assert_eq!(StoryState::decode(&blob), Some(state));
        assert_eq!(StoryState::decode(&blob).unwrap().encode(), blob);
    }

    #[test]
    fn malformed_or_ambiguous_version_three_blobs_are_refused() {
        for bad in [
            vec![3, 1, 0],                           // missing count
            vec![3, 1, 0, 1, 0],                     // missing entry
            vec![3, 1, 0, 0, 0, 3, 66, 0, 1],       // extra entry
            vec![3, 1, 0, 1, 0, 4, 66, 0, 1],       // alias is not canonical
            vec![3, 1, 0, 1, 0, 3, 66, 0, 0],       // no known bits
            vec![3, 1, 0, 1, 0, 3, 66, 2, 1],       // value outside known mask
            vec![3, 1, 0, 1, 0, 1, 1, 0, 255],      // moment is in header
            vec![3, 1, 0, 1, 0, 5, 0, 0, 255],      // temporary bank
            vec![3, 1, 0, 2, 0, 3, 66, 0, 1, 3, 66, 0, 1], // duplicate
            vec![3, 1, 0, 2, 0, 3, 67, 0, 1, 3, 66, 0, 1], // unsorted
            vec![3, 1, 0, 255, 255],                 // excessive count
            vec![4, 1, 0, 0, 0],                    // future schema
        ] { assert_eq!(StoryState::decode(&bad), None, "{bad:?}"); }
        assert!(StoryState::decode(&[3, 1, 0, 0, 0]).is_some(), "a v3 state may know only its moment");
    }

    #[test]
    fn exact_base_replacement_preserves_clears_and_rejects_stale_or_lossy_updates() {
        let mut state = StoryState::at(36);
        state.write_byte(3, 66, 3).unwrap();
        state.write_byte(1, 4, 30).unwrap();
        let before = state.encode();
        assert_eq!(compare_exchange_progression(&[1, 36, 0], &[1, 36, 0], &before), Some(before.clone()), "explicit legacy upgrade");
        state.write_masked(3, 66, 0, 1).unwrap();
        state.add_byte_checked(1, 4, -1, 0, 100).unwrap();
        let after = state.encode();
        assert_eq!(compare_exchange_progression(&before, &before, &after), Some(after.clone()));
        assert_eq!(compare_exchange_progression(&after, &before, &before), None, "stale predecessor");
        assert_eq!(compare_exchange_progression(&after, &after, &[1, 39, 0]), None, "no downgrade");
        assert_eq!(compare_exchange_progression(&after, &after, &[3, 39, 0, 0, 0]), None, "no forgetting");
        assert_eq!(compare_exchange_progression(&after, &after, &[4, 39, 0]), None, "future version");
        assert_eq!(merge_progression(&before, &after), before, "a legacy union cannot save ordered state");
        assert_eq!(merge_progression(&[1, 36, 0], &after), [1, 36, 0], "no implicit legacy migration");
    }

    #[test]
    fn phase_keys_include_knownness_values_and_exact_moment() {
        use crate::script::{FieldScript, synth::SynthScript};
        use crate::field::Walkmesh;
        // IFUB bank1[0] == 36 can distinguish two moments that the legacy bank2/word scanner
        // puts in one interval. Extended phases must therefore retain the exact moment.
        let bytes = SynthScript::default().entity("dir", vec![(0, vec![0x14, 0x10, 0, 36, 0, 2, 0, 0])]).build();
        let script = FieldScript::parse(&bytes).unwrap();
        let area = AreaStory::scan(&script, &Walkmesh { triangles: vec![], access: vec![] });
        assert!(area.edges().is_empty());
        let mut unknown = StoryState::at(36).into_extended().unwrap();
        let a = area.phase_key(&unknown.encode());
        unknown.moment = 37;
        assert_ne!(a, area.phase_key(&unknown.encode()), "byte aliases may distinguish moments in one legacy interval");
        unknown.moment = 36;
        unknown.write_masked(3, 66, 0, 1).unwrap();
        let b = area.phase_key(&unknown.encode());
        assert_ne!(a, b, "unknown is not known zero");
        unknown.write_masked(3, 66, 1, 1).unwrap();
        assert_ne!(b, area.phase_key(&unknown.encode()), "clear differs from set");
    }
}
