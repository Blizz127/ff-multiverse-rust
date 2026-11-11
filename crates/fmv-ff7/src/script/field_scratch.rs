//! A checked snapshot of FF7's field-local scratch memory, separate from story saves.
//!
//! The pinned PSX engine has one 256-byte `g_FieldMapVars` array. Selectors 5 and 6
//! alias it for byte access. Wide operations through 5 still read/write one unsigned
//! byte; through 6 they use two little-endian bytes. See `field_opcodes.c:727–735`,
//! `815–823`, `949–965`, and `1085–1101` in ff7-decomp revision
//! `d4db3de3c3aaf61269e048cf3f6a458996290de0`. Unsigned comparisons cast that same
//! signed read (`1747–1762`). Field entry explicitly clears all bytes before entity
//! initialization (`field_event.c:132–134`). These are source findings, not PC captures.
//!
//! Knowledge masks are a snapshot-validation policy: absent visit data does not prove
//! the native entry clear happened. Unknown bits have canonical zero values, and reads
//! refuse missing knowledge. A loaded image must already be canonical. Word access at
//! selector 6/address 255 is rejected before reading or writing; the native handler's
//! unchecked neighbour access is not reproduced. Interpreters opt into checked execution
//! through [`FieldScratchRun`]; no constructor infers a new field entry.

use std::ops::Range;

mod run;
pub use run::{FieldScratchOutcome, FieldScratchRun};

pub const FIELD_SCRATCH_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldScratchError {
    InvalidSelector(u8),
    OutOfBounds { address: usize, bytes: u8 },
    Unknown { address: usize, missing_mask: u8 },
    NonCanonical { address: usize, unknown_value_bits: u8 },
    StalePredecessor,
    InvalidOutcome,
}

impl std::fmt::Display for FieldScratchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSelector(selector) => write!(f, "selector {selector} is not FF7 field scratch (5 or 6)"),
            Self::OutOfBounds { address, bytes } => write!(f, "{bytes}-byte scratch access at {address} exceeds 256 bytes"),
            Self::Unknown { address, missing_mask } => write!(f, "scratch byte {address} has unknown bits {missing_mask:#04x}"),
            Self::NonCanonical { address, unknown_value_bits } => write!(f, "scratch byte {address} has nonzero unknown bits {unknown_value_bits:#04x}"),
            Self::StalePredecessor => write!(f, "scratch differs from the exact execution predecessor"),
            Self::InvalidOutcome => write!(f, "scratch outcome values or knowledge disagree with its assignments"),
        }
    }
}

impl std::error::Error for FieldScratchError {}

/// Values and bitwise knowledge for one field visit. No default/reset is implicit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldScratch {
    values: [u8; FIELD_SCRATCH_BYTES],
    known_masks: [u8; FIELD_SCRATCH_BYTES],
}

impl FieldScratch {
    /// A missing snapshot establishes no byte or bit, including zero-valued bits.
    pub const fn unknown() -> Self {
        Self { values: [0; FIELD_SCRATCH_BYTES], known_masks: [0; FIELD_SCRATCH_BYTES] }
    }

    /// Use only for an explicitly established new field entry, before its initializers.
    /// Scene boundaries, reconnects and battle returns do not establish this clear.
    pub const fn cleared_for_entry() -> Self {
        Self { values: [0; FIELD_SCRATCH_BYTES], known_masks: [u8::MAX; FIELD_SCRATCH_BYTES] }
    }

    /// Validate a loaded image. Unknown value bits must already be zero; malformed
    /// snapshots are rejected rather than silently normalized or promoted to known.
    pub fn from_parts(values: [u8; FIELD_SCRATCH_BYTES], known_masks: [u8; FIELD_SCRATCH_BYTES]) -> Result<Self, FieldScratchError> {
        for (address, (&value, &known)) in values.iter().zip(&known_masks).enumerate() {
            let unknown_value_bits = value & !known;
            if unknown_value_bits != 0 { return Err(FieldScratchError::NonCanonical { address, unknown_value_bits }); }
        }
        Ok(Self { values, known_masks })
    }

    /// Canonical bytes for export, not an authoritative read of unknown bits.
    pub fn values(&self) -> &[u8; FIELD_SCRATCH_BYTES] { &self.values }

    pub fn known_masks(&self) -> &[u8; FIELD_SCRATCH_BYTES] { &self.known_masks }

    pub fn read_u8(&self, selector: u8, address: usize) -> Result<u8, FieldScratchError> {
        self.read_masked_u8(selector, address, u8::MAX)
    }

    /// Read only the requested bits, returning zero outside `mask`. Unknown unrequested
    /// bits do not invalidate a known-bit guard. Even mask zero validates the address.
    pub fn read_masked_u8(&self, selector: u8, address: usize, mask: u8) -> Result<u8, FieldScratchError> {
        access(selector, address, false)?;
        let error = FieldScratchError::OutOfBounds { address, bytes: 1 };
        let value = self.values.get(address).ok_or(error)?;
        let known = self.known_masks.get(address).ok_or(error)?;
        let missing_mask = mask & !known;
        if missing_mask != 0 { return Err(FieldScratchError::Unknown { address, missing_mask }); }
        Ok(value & mask)
    }

    /// Selector 5 zero-extends one byte; selector 6 reads a little-endian word.
    pub fn read_u16(&self, selector: u8, address: usize) -> Result<u16, FieldScratchError> {
        self.read_masked_u16(selector, address, u16::MAX)
    }

    /// Check the wide access bounds before reading only the selected physical bits. Selector
    /// 5's upper bits are constant zero and require no neighbouring byte or knowledge. A
    /// selector-6 word at 255 still refuses even when only its low bits are requested.
    pub fn read_masked_u16(&self, selector: u8, address: usize, mask: u16) -> Result<u16, FieldScratchError> {
        let range = access(selector, address, true)?;
        let error = FieldScratchError::OutOfBounds { address, bytes: (range.end - range.start) as u8 };
        let values = self.values.get(range.clone()).ok_or(error)?;
        let known = self.known_masks.get(range).ok_or(error)?;
        let mut word = [0u8; 2];
        for (offset, ((target, (&value, &known)), required)) in word.iter_mut().zip(values.iter().zip(known)).zip(mask.to_le_bytes()).enumerate() {
            let missing_mask = required & !known;
            if missing_mask != 0 { return Err(FieldScratchError::Unknown { address: address + offset, missing_mask }); }
            *target = value & required;
        }
        Ok(u16::from_le_bytes(word))
    }

    /// Selector 5 remains 0..=255; selector 6 reinterprets both bytes as signed.
    pub fn read_i16(&self, selector: u8, address: usize) -> Result<i16, FieldScratchError> {
        self.read_u16(selector, address).map(|value| value as i16)
    }

    /// Writes report whether values or knowledge changed. Writing zero establishes it.
    pub fn write_u8(&mut self, selector: u8, address: usize, value: u8) -> Result<bool, FieldScratchError> {
        self.write_masked_u8(selector, address, value, u8::MAX)
    }

    /// An authored masked assignment establishes exactly `mask`, preserves other bits,
    /// and deliberately ignores value bits outside that mask.
    pub fn write_masked_u8(&mut self, selector: u8, address: usize, value: u8, mask: u8) -> Result<bool, FieldScratchError> {
        access(selector, address, false)?;
        let error = FieldScratchError::OutOfBounds { address, bytes: 1 };
        let target = self.values.get_mut(address).ok_or(error)?;
        let known = self.known_masks.get_mut(address).ok_or(error)?;
        let next = (*target & !mask) | (value & mask);
        let next_known = *known | mask;
        let changed = (*target, *known) != (next, next_known);
        *target = next;
        *known = next_known;
        Ok(changed)
    }

    /// Selector 5 writes only the low byte; selector 6 establishes both bytes atomically.
    pub fn write_u16(&mut self, selector: u8, address: usize, value: u16) -> Result<bool, FieldScratchError> {
        let range = access(selector, address, true)?;
        let error = FieldScratchError::OutOfBounds { address, bytes: (range.end - range.start) as u8 };
        let targets = self.values.get_mut(range.clone()).ok_or(error)?;
        let masks = self.known_masks.get_mut(range).ok_or(error)?;
        let mut changed = false;
        for ((target, known), next) in targets.iter_mut().zip(masks).zip(value.to_le_bytes()) {
            changed |= *target != next || *known != u8::MAX;
            *target = next;
            *known = u8::MAX;
        }
        Ok(changed)
    }

    pub fn write_i16(&mut self, selector: u8, address: usize, value: i16) -> Result<bool, FieldScratchError> {
        self.write_u16(selector, address, value as u16)
    }
}

fn access(selector: u8, address: usize, wide: bool) -> Result<Range<usize>, FieldScratchError> {
    if !matches!(selector, 5 | 6) { return Err(FieldScratchError::InvalidSelector(selector)); }
    let bytes = if wide && selector == 6 { 2u8 } else { 1 };
    let end = address.checked_add(usize::from(bytes)).filter(|&end| end <= FIELD_SCRATCH_BYTES)
        .ok_or(FieldScratchError::OutOfBounds { address, bytes })?;
    Ok(address..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_entry_clear_and_loaded_known_zero_are_distinct() {
        let unknown = FieldScratch::unknown();
        let cleared = FieldScratch::cleared_for_entry();
        assert_ne!(unknown, cleared);
        assert_eq!(unknown.values(), cleared.values());
        assert_eq!(unknown.read_u8(5, 31), Err(FieldScratchError::Unknown { address: 31, missing_mask: 255 }));
        assert_eq!(cleared.read_u8(5, 31), Ok(0));
        assert_eq!(FieldScratch::from_parts(*cleared.values(), *cleared.known_masks()), Ok(cleared));
        assert_eq!(FieldScratch::from_parts(*unknown.values(), *unknown.known_masks()), Ok(unknown));
        let mut absent = FieldScratch::unknown();
        assert_eq!(absent.write_u8(5, 31, 0), Ok(true));
        assert_eq!(absent.write_u8(6, 31, 0), Ok(false));
        assert_eq!(absent.read_u8(6, 31), Ok(0));
        assert!(absent.read_u8(5, 30).is_err());
    }

    #[test]
    fn selectors_alias_but_wide_selector_five_stays_an_unsigned_byte() {
        let mut scratch = FieldScratch::cleared_for_entry();
        scratch.write_u16(6, 20, 0x80ff).unwrap();
        assert_eq!(scratch.read_u8(5, 20), Ok(255));
        assert_eq!(scratch.read_u8(6, 20), Ok(255));
        assert_eq!(scratch.read_i16(5, 20), Ok(255));
        assert_eq!(scratch.read_u16(5, 20), Ok(255));
        assert_eq!(scratch.read_i16(6, 20), Ok(-32513));
        scratch.write_i16(5, 20, -2).unwrap();
        assert_eq!(scratch.read_u16(6, 20), Ok(0x80fe));
        scratch.write_u8(6, 21, 0x7f).unwrap();
        assert_eq!(scratch.read_i16(6, 20), Ok(0x7ffe));
        scratch.write_u16(6, 21, 0x1234).unwrap();
        assert_eq!(scratch.read_u16(6, 20), Ok(0x34fe), "overlapping word writes share the same bytes");
        assert_eq!(scratch.read_u8(5, 22), Ok(0x12));
    }

    #[test]
    fn masked_assignments_preserve_unknown_neighbours_and_known_zeroes() {
        let mut scratch = FieldScratch::unknown();
        assert_eq!(scratch.write_masked_u8(5, 8, 0xff, 0x0f), Ok(true));
        assert_eq!(scratch.read_masked_u8(6, 8, 0x07), Ok(7));
        assert_eq!(scratch.read_u8(5, 8), Err(FieldScratchError::Unknown { address: 8, missing_mask: 0xf0 }));
        assert_eq!(scratch.write_masked_u8(6, 8, 0, 0xf0), Ok(true));
        assert_eq!(scratch.read_u8(5, 8), Ok(15));
        assert_eq!(scratch.read_u16(6, 8), Err(FieldScratchError::Unknown { address: 9, missing_mask: 255 }));
        scratch.write_i16(5, 8, -1).unwrap();
        assert_eq!(scratch.read_i16(5, 8), Ok(255));
        assert_eq!(scratch.known_masks()[9], 0, "a wide selector-5 write does not establish its neighbour");
        let before = scratch.clone();
        assert_eq!(scratch.write_masked_u8(6, 9, 255, 0), Ok(false));
        assert_eq!(scratch.read_masked_u8(5, 9, 0), Ok(0));
        assert_eq!(scratch, before);
        assert!(scratch.values().iter().zip(scratch.known_masks()).all(|(&value, &mask)| value & !mask == 0));
    }

    #[test]
    fn loaded_images_reject_noncanonical_unknown_bits_at_every_address() {
        for address in 0..FIELD_SCRATCH_BYTES {
            let mut values = [0; FIELD_SCRATCH_BYTES];
            let mut masks = [0; FIELD_SCRATCH_BYTES];
            values[address] = 0x81;
            masks[address] = 0x80;
            assert_eq!(FieldScratch::from_parts(values, masks), Err(FieldScratchError::NonCanonical { address, unknown_value_bits: 1 }));
            values[address] = 0x80;
            let scratch = FieldScratch::from_parts(values, masks).unwrap();
            assert_eq!(scratch.read_masked_u8(5, address, 0x80), Ok(0x80));
        }
    }

    #[test]
    fn final_byte_is_valid_except_for_a_selector_six_word_and_all_refusals_are_atomic() {
        let mut scratch = FieldScratch::unknown();
        scratch.write_u8(6, 255, 0xa5).unwrap();
        assert_eq!(scratch.read_i16(5, 255), Ok(165));
        scratch.write_u16(5, 255, 0x12ff).unwrap();
        assert_eq!(scratch.read_u8(6, 255), Ok(255));
        assert_eq!(scratch.read_u16(6, 255), Err(FieldScratchError::OutOfBounds { address: 255, bytes: 2 }));
        assert!(scratch.read_i16(6, 255).is_err());
        let before = scratch.clone();
        assert!(scratch.write_u16(6, 255, 0).is_err());
        assert!(scratch.write_i16(6, 255, -1).is_err());
        for address in [256, 257, usize::MAX] {
            for selector in [5, 6] {
                assert!(scratch.read_u8(selector, address).is_err());
                assert!(scratch.read_u16(selector, address).is_err());
                assert!(scratch.read_i16(selector, address).is_err());
                assert!(scratch.read_masked_u8(selector, address, 0).is_err());
                assert!(scratch.write_u8(selector, address, 0).is_err());
                assert!(scratch.write_u16(selector, address, 0).is_err());
                assert!(scratch.write_i16(selector, address, 0).is_err());
                assert!(scratch.write_masked_u8(selector, address, 255, 0).is_err());
                assert_eq!(scratch, before);
            }
        }
        for selector in (0..=u8::MAX).filter(|s| !matches!(s, 5 | 6)) {
            assert_eq!(scratch.read_u8(selector, 0), Err(FieldScratchError::InvalidSelector(selector)));
            assert!(scratch.read_u16(selector, 0).is_err());
            assert!(scratch.read_i16(selector, 0).is_err());
            assert!(scratch.read_masked_u8(selector, 0, 0).is_err());
            assert!(scratch.write_u8(selector, 0, 0).is_err());
            assert!(scratch.write_u16(selector, 0, 0).is_err());
            assert!(scratch.write_i16(selector, 0, 0).is_err());
            assert!(scratch.write_masked_u8(selector, 0, 0, 0).is_err());
            assert_eq!(scratch, before);
        }
    }
}
