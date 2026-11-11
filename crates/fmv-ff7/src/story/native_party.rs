//! Version 4 carries native party resources alongside the persistent field banks.
//!
//! Main party slots, PHS availability and PHS locks are separate native savemap fields;
//! the raw field slots remain solely in bank 3:9..11 (alias 4). No equality between the
//! two tuples, compactness, or implication from active membership to availability is imposed.
//! The pinned PSX layout and operations are documented at:
//! <https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/include/game.h#L546-L577>
//! and <https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L5515-L5848>.
//!
//! The canonical encoding is the version-3 bank header/records with version byte 4,
//! followed by exactly 12 bytes: main-known mask, three main slots, then availability,
//! availability-known mask, locks and locks-known mask (four little-endian u16s).
//! Unknown values are zero; known empty main slots are 255. Promotion never infers
//! these resources from the story moment or the raw field slots.

use super::{StoryState, StoryWriteError, MAX_BANK_BLOB_BYTES};

mod ops;
pub use ops::{evaluate_native_party, NativePartyAssignments, NativePartyEffect, NativePartyImage, NativePartyOp, NativePartyOpError};

pub const BLOB_VERSION_NATIVE_PARTY: u8 = 4;
pub const NATIVE_PARTY_BYTES: usize = 12;
/// All five fully known banks and the native-party suffix, within the 8 KiB wire limit.
pub const MAX_NATIVE_PARTY_BLOB_BYTES: usize = MAX_BANK_BLOB_BYTES + NATIVE_PARTY_BYTES;
const MAIN_MASK: u8 = 0b111;

/// An authoritative roster query: malformed blobs and unknown native main slots refuse.
/// Empty bytes retain [`StoryState::decode`]'s explicitly valid legacy new-game meaning;
/// other legacy formats keep their existing moment table. A fully known empty v4 party
/// succeeds with an empty vector, distinct from an unknown party. Holes are filtered only
/// here, without reordering the remaining native slots or changing their stored positions.
pub fn checked_canon_party_of(progress: &[u8]) -> Option<Vec<u8>> {
    let state = StoryState::decode(progress)?;
    match state.native_party() {
        Some(party) => Some(party.known_main()?.into_iter().filter(|&member| member != 255).collect()),
        None => Some(super::canon_party(&state)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativePartyError {
    #[error("main party slot {0} is outside 0..3")]
    SlotOutOfRange(u8),
    #[error("PHS bit {0} is outside 0..16")]
    BitOutOfRange(u8),
    #[error("main-slot mask {0} contains bits outside the three slots")]
    InvalidSlotMask(u8),
    #[error("party member {0} is neither a supported character (0..=8) nor empty (255)")]
    InvalidMember(u8),
    #[error("known main slots repeat party member {0}")]
    DuplicateMember(u8),
    #[error("unknown party values must have a canonical zero representation")]
    NoncanonicalUnknown,
    #[error("native party resources require explicit version-4 promotion")]
    MissingResources,
    #[error("native party resources differ from the expected predecessor")]
    StaleResources,
    #[error("a native party replacement cannot discard established knowledge")]
    KnowledgeLoss,
}

/// The native resources outside the five field banks. Unknown is distinct from empty,
/// unavailable and unlocked. Fields are private so every public update keeps a valid image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NativePartyState {
    main: [u8; 3],
    main_known: u8,
    availability: u16,
    availability_known: u16,
    locks: u16,
    locks_known: u16,
}

impl NativePartyState {
    pub const fn unknown() -> Self {
        Self { main: [0; 3], main_known: 0, availability: 0, availability_known: 0, locks: 0, locks_known: 0 }
    }

    /// Construct an explicit image without normalizing malformed input. Tuple arguments
    /// are `(value, known_mask)`; unknown bits and unknown slot values must already be zero.
    pub fn from_parts(main: [u8; 3], main_known: u8, availability: (u16, u16), locks: (u16, u16)) -> Result<Self, NativePartyError> {
        let state = Self { main, main_known, availability: availability.0, availability_known: availability.1,
            locks: locks.0, locks_known: locks.1 };
        state.validate()?;
        Ok(state)
    }

    fn validate(&self) -> Result<(), NativePartyError> {
        if self.main_known & !MAIN_MASK != 0 { return Err(NativePartyError::InvalidSlotMask(self.main_known)); }
        let mut seen = 0u16;
        for (slot, &member) in self.main.iter().enumerate() {
            if self.main_known & (1 << slot) == 0 {
                if member != 0 { return Err(NativePartyError::NoncanonicalUnknown); }
            } else if member != 255 {
                if member > 8 { return Err(NativePartyError::InvalidMember(member)); }
                let bit = 1u16 << member;
                if seen & bit != 0 { return Err(NativePartyError::DuplicateMember(member)); }
                seen |= bit;
            }
        }
        if self.availability & !self.availability_known != 0 || self.locks & !self.locks_known != 0 {
            return Err(NativePartyError::NoncanonicalUnknown);
        }
        Ok(())
    }

    /// Main-slot values and the low-three-bit known mask. Unset slots contain zero, not a
    /// guessed Cloud; prefer the checked accessors when executing an instruction.
    pub fn main_slots(&self) -> ([u8; 3], u8) { (self.main, self.main_known) }

    /// All three main slots, preserving holes, only when every slot is authoritative.
    pub fn known_main(&self) -> Option<[u8; 3]> { (self.main_known == MAIN_MASK).then_some(self.main) }

    /// `Some(255)` is a known empty slot; `None` is unknown. An invalid slot is an error.
    pub fn known_main_slot(&self, slot: u8) -> Result<Option<u8>, NativePartyError> {
        let member = *self.main.get(usize::from(slot)).ok_or(NativePartyError::SlotOutOfRange(slot))?;
        Ok((self.main_known & (1 << slot) != 0).then_some(member))
    }

    pub fn availability_bits(&self) -> (u16, u16) { (self.availability, self.availability_known) }
    pub fn locking_bits(&self) -> (u16, u16) { (self.locks, self.locks_known) }

    pub fn known_available(&self, bit: u8) -> Result<Option<bool>, NativePartyError> {
        Self::known_bit(self.availability, self.availability_known, bit)
    }

    pub fn known_locked(&self, bit: u8) -> Result<Option<bool>, NativePartyError> {
        Self::known_bit(self.locks, self.locks_known, bit)
    }

    fn known_bit(value: u16, known: u16, bit: u8) -> Result<Option<bool>, NativePartyError> {
        let mask = 1u16.checked_shl(u32::from(bit)).ok_or(NativePartyError::BitOutOfRange(bit))?;
        Ok((known & mask != 0).then_some(value & mask != 0))
    }

    /// Apply actual slot assignments atomically. Unassigned inputs are ignored, existing
    /// knowledge is retained, and a complete reorder is validated as one operation.
    pub fn write_main_slots(&mut self, values: [u8; 3], assigned: u8) -> Result<bool, NativePartyError> {
        if assigned & !MAIN_MASK != 0 { return Err(NativePartyError::InvalidSlotMask(assigned)); }
        let mut next = *self;
        for (slot, (out, value)) in next.main.iter_mut().zip(values).enumerate() {
            if assigned & (1 << slot) != 0 { *out = value; }
        }
        next.main_known |= assigned;
        next.validate()?;
        let changed = next != *self;
        *self = next;
        Ok(changed)
    }

    /// An authored availability assignment establishes only its selected bits. It neither
    /// changes active slots nor assumes the engine's full mask is known.
    pub fn write_availability_masked(&mut self, value: u16, mask: u16) -> bool {
        let before = *self;
        self.availability = (self.availability & !mask) | (value & mask);
        self.availability_known |= mask;
        *self != before
    }

    /// An authored lock assignment is independent of availability and active membership.
    pub fn write_locks_masked(&mut self, value: u16, mask: u16) -> bool {
        let before = *self;
        self.locks = (self.locks & !mask) | (value & mask);
        self.locks_known |= mask;
        *self != before
    }

    pub(super) fn retains_knowledge(&self, old: &Self) -> bool {
        self.main_known & old.main_known == old.main_known
            && self.availability_known & old.availability_known == old.availability_known
            && self.locks_known & old.locks_known == old.locks_known
    }

    pub(super) fn decode(bytes: &[u8]) -> Option<Self> {
        let [known, a, b, c, v0, v1, k0, k1, l0, l1, m0, m1] = bytes else { return None };
        Self::from_parts([*a, *b, *c], *known,
            (u16::from_le_bytes([*v0, *v1]), u16::from_le_bytes([*k0, *k1])),
            (u16::from_le_bytes([*l0, *l1]), u16::from_le_bytes([*m0, *m1]))).ok()
    }

    pub(super) fn encode(self) -> [u8; NATIVE_PARTY_BYTES] {
        let [a, b, c] = self.main;
        let [v0, v1] = self.availability.to_le_bytes();
        let [k0, k1] = self.availability_known.to_le_bytes();
        let [l0, l1] = self.locks.to_le_bytes();
        let [m0, m1] = self.locks_known.to_le_bytes();
        [self.main_known, a, b, c, v0, v1, k0, k1, l0, l1, m0, m1]
    }

    /// Explicit port profile: reset main ordering/availability, with the opening MM BLK0
    /// assignment. Opaque original startup prevents claiming an arbitrary PC snapshot.
    pub(super) const fn first_control_profile() -> Self {
        Self { main: [0, 255, 255], main_known: MAIN_MASK, availability: 1,
            availability_known: u16::MAX, locks: 1, locks_known: 1 }
    }
}

impl StoryState {
    /// Opt into v4 without inferring resources from the moment, banks or a previous profile.
    /// Existing v4 resources are preserved; legacy bank promotion can still refuse conflicts.
    pub fn into_native_party(self) -> Result<Self, StoryWriteError> {
        let mut state = self.into_extended()?;
        state.native_party.get_or_insert_with(NativePartyState::unknown);
        Ok(state)
    }

    pub fn supports_native_party(&self) -> bool { self.native_party.is_some() }
    pub fn native_party(&self) -> Option<&NativePartyState> { self.native_party.as_ref() }
    pub fn native_party_mut(&mut self) -> Option<&mut NativePartyState> { self.native_party.as_mut() }

    /// Install a validated resource afterimage against its exact predecessor, without
    /// promoting an old save or losing knowledge. A durable write must still compare the
    /// complete story blob under its transaction lock ([`super::compare_exchange_progression`]).
    pub fn replace_native_party(&mut self, expected: &NativePartyState, replacement: NativePartyState) -> Result<bool, NativePartyError> {
        let current = self.native_party.as_mut().ok_or(NativePartyError::MissingResources)?;
        if *current != *expected { return Err(NativePartyError::StaleResources); }
        if !replacement.retains_knowledge(current) { return Err(NativePartyError::KnowledgeLoss); }
        let changed = *current != replacement;
        *current = replacement;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests;
