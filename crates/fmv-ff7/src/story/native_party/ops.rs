//! Atomic, game-native party operations over a version-4 snapshot. This is an evaluator,
//! not scene admission: it does not enable registration, remove execution gates, select a
//! PHS menu result, initialize an old save, change field models, or publish the native
//! `g_PartyUpdatedByFieldScript` notification. Consumers must handle those presentation
//! and lifecycle effects separately when execution is enabled.
//!
//! Source: PSX reconstruction revision d4db3de3c3aaf61269e048cf3f6a458996290de0,
//! <https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L5517-L5848>.
//! The `ADD_PARTY_MEMBER` macro assigns availability only for a written nonempty raw slot:
//! <https://github.com/Xeeynamo/ff7-decomp/blob/d4db3de3c3aaf61269e048cf3f6a458996290de0/src/field/field_opcodes.c#L17-L27>.
//! Duplicate Add and absent Remove still synchronize main slots (5527–5531,5579).
//! PartyReplace removes every matching raw member and assigns all final slots (5636–5673).
//! PartyCompare/Remove/Add execute padded 255 entries, including unchanged empty-slot
//! assignments (5677–5754). Availability removal clears raw matches without syncing main
//! (5805–5826); locks affect only their selected bit (5830–5848).
//!
//! Supported operands are ordinary character IDs 0..=8, and Set's 254/255 sentinels. This is
//! a port support restriction, not a claim that native unchecked-u8 shifts validate IDs:
//! flashback IDs 9/10 are deferred. All 16 stored mask bits remain intact. Knowledge checking
//! is deliberately conservative: tuple operations require the full main tuple, and every
//! operation that preserves/compares raw slots requires all three raw bytes. A Set without
//! 255 can overwrite unknown raw slots. No unrelated PHS bit needs to be known.

use super::NativePartyState;
use crate::script::party::{native_transition, reconcile_slots};
use crate::story::{NativePartyError, PartyOp, StoryState, StoryWriteError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePartyOp {
    Add(u8),
    Remove(u8),
    Set([u8; 3]),
    Available { character: u8, available: bool },
    Lock { character: u8, locked: bool },
    /// Reviewed native FIELD_Init boundary; never an arbitrary scene/window reset.
    FieldSync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativePartyOpError {
    #[error("native party operations require an explicit version-4 state")]
    RequiresNativeParty,
    #[error("character selector {0} is outside the supported ordinary IDs 0..=8")]
    UnsupportedSelector(u8),
    #[error("main party slot {0} is not known")]
    UnknownMainSlot(u8),
    #[error("raw party bank 3 address {address} is not fully known (mask {known_mask})")]
    UnknownRawSlot { address: u8, known_mask: u8 },
    #[error("the native tuple operation cannot be represented")]
    InvalidTransition,
    #[error(transparent)]
    Resource(#[from] NativePartyError),
    #[error(transparent)]
    Bank(#[from] StoryWriteError),
    #[error("party or raw-slot knowledge differs from the operation's exact predecessor")]
    StalePredecessor,
    #[error("the operation's assignments do not produce its exact afterimage")]
    InconsistentEffect,
}

/// Complete party input/output, including partial knowledge of the raw field bytes.
/// This is small enough to retain per operation; unrelated story bytes are not duplicated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePartyImage {
    native: NativePartyState,
    raw: [(u8, u8); 3],
}

impl NativePartyImage {
    pub fn capture(state: &StoryState) -> Result<Self, NativePartyOpError> {
        let native = *state.native_party().ok_or(NativePartyOpError::RequiresNativeParty)?;
        let raw = [9, 10, 11].map(|address| (state.var(3, address), state.known_mask(3, address)));
        Ok(Self { native, raw })
    }

    pub fn native_party(&self) -> &NativePartyState { &self.native }
    /// `(value, known_mask)` for bank 3:9,10,11, in that order. No raw/main equality is implied.
    pub fn raw_slots(&self) -> [(u8, u8); 3] { self.raw }

    fn known_raw(&self) -> Result<[u8; 3], NativePartyOpError> {
        let mut slots = [0; 3];
        for (slot, (out, &(value, known_mask))) in slots.iter_mut().zip(&self.raw).enumerate() {
            if known_mask != 255 {
                return Err(NativePartyOpError::UnknownRawSlot { address: 9 + slot as u8, known_mask });
            }
            *out = value;
        }
        Ok(slots)
    }

    fn known_main(&self) -> Result<[u8; 3], NativePartyOpError> {
        let mut slots = [0; 3];
        for (slot, out) in slots.iter_mut().enumerate() {
            *out = self.native.known_main_slot(slot as u8)?.ok_or(NativePartyOpError::UnknownMainSlot(slot as u8))?;
        }
        Ok(slots)
    }
}

/// Actual native assignments, including writes that preserve a value. The two slot masks
/// use their low three bits; PHS masks retain their independent 16-bit domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NativePartyAssignments {
    pub raw_slots: u8,
    pub main_slots: u8,
    pub availability: u16,
    pub locks: u16,
}

/// A semantic journal entry. Only evaluation constructs it. Callers retain these in order,
/// even when `changed()` is false; no network or stored record can fabricate an effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePartyEffect {
    op: NativePartyOp,
    before: NativePartyImage,
    after: NativePartyImage,
    assigned: NativePartyAssignments,
}

impl NativePartyEffect {
    pub fn operation(&self) -> NativePartyOp { self.op }
    pub fn before(&self) -> &NativePartyImage { &self.before }
    pub fn after(&self) -> &NativePartyImage { &self.after }
    pub fn assignments(&self) -> NativePartyAssignments { self.assigned }
    pub fn changed(&self) -> bool { self.before != self.after }

    /// Apply against the exact party/raw predecessor, atomically on a clone. Unrelated
    /// banks are preserved. Durable publication must additionally CAS the complete story
    /// under its transaction lock; this helper does not weaken that outer precondition.
    pub fn apply_to(&self, state: &mut StoryState) -> Result<bool, NativePartyOpError> {
        if NativePartyImage::capture(state)? != self.before { return Err(NativePartyOpError::StalePredecessor); }
        let mut next = state.clone();
        for (slot, &(value, known)) in self.after.raw.iter().enumerate() {
            if self.assigned.raw_slots & (1 << slot) != 0 {
                if known != 255 { return Err(NativePartyOpError::InconsistentEffect); }
                next.write_masked(3, 9 + slot as u8, value, 255)?;
            }
        }
        next.replace_native_party(&self.before.native, self.after.native)?;
        if NativePartyImage::capture(&next)? != self.after { return Err(NativePartyOpError::InconsistentEffect); }
        let changed = next != *state;
        *state = next;
        Ok(changed)
    }
}

fn slot_mask(assigned: [bool; 3]) -> u8 {
    assigned.into_iter().enumerate().fold(0, |mask, (slot, written)| mask | if written { 1 << slot } else { 0 })
}

fn selector(character: u8) -> Result<(), NativePartyOpError> {
    if character <= 8 { Ok(()) } else { Err(NativePartyOpError::UnsupportedSelector(character)) }
}

/// Evaluate exactly one operation without mutating its input. Unknown required inputs,
/// unsupported operands or an unrepresentable final main tuple return no partial effect.
/// Operands must already be resolved and audited by the calling script interpreter.
pub fn evaluate_native_party(state: &StoryState, op: NativePartyOp) -> Result<NativePartyEffect, NativePartyOpError> {
    let before = NativePartyImage::capture(state)?;
    let mut after = before;
    let mut assigned = NativePartyAssignments::default();
    match op {
        NativePartyOp::Add(character) | NativePartyOp::Remove(character) => {
            selector(character)?;
        }
        NativePartyOp::Set(slots) => {
            for member in slots { if !matches!(member, 254 | 255) { selector(member)?; } }
        }
        NativePartyOp::Available { character, .. } | NativePartyOp::Lock { character, .. } => selector(character)?,
        NativePartyOp::FieldSync => {},
    }
    match op {
        NativePartyOp::Add(_) | NativePartyOp::Remove(_) | NativePartyOp::Set(_) => {
            let main = before.known_main()?;
            let overwrite = matches!(op, NativePartyOp::Set(slots) if !slots.contains(&255));
            // The ignored raw values of a complete overwrite are never marked known or
            // recorded as assignments. Its final native writes alone establish knowledge.
            let raw = if overwrite { before.raw.map(|(value, _)| value) } else { before.known_raw()? };
            let native_op = match op {
                NativePartyOp::Add(character) => PartyOp::Add(character),
                NativePartyOp::Remove(character) => PartyOp::Remove(character),
                NativePartyOp::Set(slots) => PartyOp::Set(slots),
                _ => return Err(NativePartyOpError::InvalidTransition),
            };
            let change = native_transition(raw, main, native_op).map_err(|_| NativePartyOpError::InvalidTransition)?;
            assigned.raw_slots = slot_mask(change.raw_assigned);
            assigned.main_slots = slot_mask(change.main_assigned);
            after.native.write_main_slots(change.main, assigned.main_slots)?;
            for ((out, member), written) in after.raw.iter_mut().zip(change.bank).zip(change.raw_assigned) {
                if written { *out = (member, 255); }
            }
            if matches!(op, NativePartyOp::Add(_) | NativePartyOp::Set(_)) {
                for (member, written) in change.bank.into_iter().zip(change.raw_assigned) {
                    if written && member != 255 {
                        selector(member)?;
                        assigned.availability |= 1u16 << member;
                    }
                }
                after.native.write_availability_masked(assigned.availability, assigned.availability);
            }
        }
        NativePartyOp::Available { character, available } => {
            if !available {
                let raw = before.known_raw()?;
                for (slot, (out, member)) in after.raw.iter_mut().zip(raw).enumerate() {
                    if member == character { *out = (255, 255); assigned.raw_slots |= 1 << slot; }
                }
            }
            assigned.availability = 1u16 << character;
            after.native.write_availability_masked(if available { assigned.availability } else { 0 }, assigned.availability);
        }
        NativePartyOp::Lock { character, locked } => {
            assigned.locks = 1u16 << character;
            after.native.write_locks_masked(if locked { assigned.locks } else { 0 }, assigned.locks);
        }
        NativePartyOp::FieldSync => {
            let (raw, written) = reconcile_slots(before.known_raw()?, before.known_main()?);
            assigned.raw_slots = slot_mask(written);
            for ((out, member), written) in after.raw.iter_mut().zip(raw).zip(written) {
                if written { *out = (member, 255); }
            }
        }
    }
    Ok(NativePartyEffect { op, before, after, assigned })
}

#[cfg(test)]
mod tests;
