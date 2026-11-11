//! Ordered persistent assignments and native party effects for one v4 execution segment.
//!
//! A final byte map cannot reconstruct an interleaved raw-party write, party operation, and
//! restoration of that raw byte: the main party may have changed even when the raw bytes did
//! not. This journal therefore owns the complete authored persistent overlay. Scratch bytes,
//! trigger seeds and presentation inputs remain the interpreter's responsibility; they are not
//! durable assignments and must not be inserted here as initialization.
//!
//! The builder accepts resolved, audited instruction results in execution order. Native party
//! effects already include their raw assignments: consumers may mirror these into inspection
//! maps, but must not journal those assignments a second time. Final maps are summaries only
//! for v4; replaying this journal is the sole application path. It does not enable any interpreter
//! or registration gate, serialize effects, or replace the caller's locked durable full-story CAS.

use super::{evaluate_native_party, NativePartyEffect, NativePartyOp, NativePartyOpError};
use crate::story::{canonical_persistent_bank, StoryState, StoryWriteError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativeStoryError {
    #[error("a native story journal requires an explicit version-4 state")]
    RequiresNativeParty,
    #[error(transparent)]
    Bank(#[from] StoryWriteError),
    #[error(transparent)]
    Party(#[from] NativePartyOpError),
    #[error("the story differs from the journal's exact predecessor")]
    StalePredecessor,
    #[error("a persistent assignment does not match its exact before/after bytes")]
    InconsistentAssignment,
    #[error("the ordered effects do not produce the journal's exact afterimage")]
    InconsistentAfterimage,
}

/// One actual canonical byte assignment, including assignments that leave its value and
/// knowledge unchanged. Only the builder constructs this checked record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeStoryAssignment {
    bank: u8,
    address: u8,
    value: u8,
    mask: u8,
    before: (u8, u8),
    after: (u8, u8),
}

impl NativeStoryAssignment {
    pub fn key(&self) -> (u8, u8) { (self.bank, self.address) }
    /// Assigned bits only; bits outside `mask()` are zero, not inferred knowledge.
    pub fn value(&self) -> u8 { self.value }
    pub fn mask(&self) -> u8 { self.mask }
    /// `(value, known_mask)` before this assignment.
    pub fn before(&self) -> (u8, u8) { self.before }
    /// `(value, known_mask)` after this assignment.
    pub fn after(&self) -> (u8, u8) { self.after }
    pub fn changed(&self) -> bool { self.before != self.after }

    fn apply_to(&self, state: &mut StoryState) -> Result<(), NativeStoryError> {
        if canonical_persistent_bank(self.bank) != Some(self.bank) || self.mask == 0
            || self.value & !self.mask != 0 || byte(state, self.bank, self.address) != self.before {
            return Err(NativeStoryError::InconsistentAssignment);
        }
        state.write_masked(self.bank, self.address, self.value, self.mask)?;
        if byte(state, self.bank, self.address) != self.after {
            return Err(NativeStoryError::InconsistentAssignment);
        }
        Ok(())
    }
}

/// Events stay in execution order. Party effects include their own raw-byte writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeStoryEvent {
    Assignment(NativeStoryAssignment),
    Party(NativePartyEffect),
}

impl NativeStoryEvent {
    /// Whether this event changes values or knowledge, even if a later event restores them.
    pub fn changed(&self) -> bool {
        match self { Self::Assignment(write) => write.changed(), Self::Party(effect) => effect.changed() }
    }

    fn apply_to(&self, state: &mut StoryState) -> Result<(), NativeStoryError> {
        match self {
            Self::Assignment(write) => write.apply_to(state),
            Self::Party(effect) => { effect.apply_to(state)?; Ok(()) }
        }
    }
}

/// An immutable execution result. It cannot be assembled from untrusted final maps or
/// caller-supplied effect records; only a builder creates its private ordered contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeStoryJournal {
    before: StoryState,
    after: StoryState,
    events: Vec<NativeStoryEvent>,
}

impl NativeStoryJournal {
    pub fn before(&self) -> &StoryState { &self.before }
    pub fn after(&self) -> &StoryState { &self.after }
    pub fn events(&self) -> &[NativeStoryEvent] { &self.events }
    /// Net persistent value or knowledge change, distinct from whether assignments occurred.
    pub fn changed(&self) -> bool { self.before != self.after }
    /// Any event changed values or knowledge, including a temporary change later restored.
    pub fn any_change(&self) -> bool { self.events.iter().any(NativeStoryEvent::changed) }

    /// Validate and replay on a clone before publishing. Both supplied snapshots must equal
    /// the complete captured predecessor, including unrelated banks and all known masks.
    /// A durable writer must still check that predecessor against storage under its lock.
    pub fn apply_to(&self, state: &mut StoryState, expected: &StoryState) -> Result<bool, NativeStoryError> {
        if !state.supports_native_party() || !expected.supports_native_party() || !self.before.supports_native_party() {
            return Err(NativeStoryError::RequiresNativeParty);
        }
        if state != expected || expected != &self.before { return Err(NativeStoryError::StalePredecessor); }
        let mut next = expected.clone();
        for event in &self.events { event.apply_to(&mut next)?; }
        if next != self.after { return Err(NativeStoryError::InconsistentAfterimage); }
        let changed = next != *state;
        *state = next;
        Ok(changed)
    }
}

/// The single current persistent overlay during interpretation. All updates are checked
/// and atomic; there is deliberately no mutable access to the state or the event sequence.
/// Clone to stage a multiwrite instruction such as GTPYE, whose sequential source reads
/// must observe earlier destination writes. Publish the staged builder only on success.
#[derive(Debug, Clone)]
pub struct NativeStoryBuilder {
    before: StoryState,
    state: StoryState,
    events: Vec<NativeStoryEvent>,
}

impl NativeStoryBuilder {
    pub fn new(before: &StoryState) -> Result<Self, NativeStoryError> {
        if !before.supports_native_party() { return Err(NativeStoryError::RequiresNativeParty); }
        Ok(Self { before: before.clone(), state: before.clone(), events: Vec::new() })
    }

    pub fn before(&self) -> &StoryState { &self.before }
    pub fn state(&self) -> &StoryState { &self.state }
    pub fn events(&self) -> &[NativeStoryEvent] { &self.events }
    pub fn changed(&self) -> bool { self.before != self.state }
    pub fn any_change(&self) -> bool { self.events.iter().any(NativeStoryEvent::changed) }

    /// Append a resolved masked assignment. A zero mask is not an assignment. Otherwise
    /// the event is retained even when this returns false (no value or knowledge changed).
    pub fn write_masked(&mut self, bank: u8, address: u8, value: u8, mask: u8) -> Result<bool, NativeStoryError> {
        let bank = canonical_persistent_bank(bank).ok_or(StoryWriteError::NonPersistentBank(bank))?;
        if mask == 0 { return Ok(false); }
        let mut next = self.state.clone();
        let event = assign(&mut next, bank, address, value, mask)?;
        let changed = next != self.state;
        self.state = next;
        self.events.push(NativeStoryEvent::Assignment(event));
        Ok(changed)
    }

    pub fn write_byte(&mut self, bank: u8, address: u8, value: u8) -> Result<bool, NativeStoryError> {
        self.write_masked(bank, address, value, 255)
    }

    /// Assign an unaligned little-endian word as two ordered physical-byte events. Both
    /// halves are validated and staged before either is published, including bank aliases
    /// and words straddling the raw party tuple (for example bank 4 at address 8 or 11).
    pub fn write_word(&mut self, bank: u8, address: u8, value: u16) -> Result<bool, NativeStoryError> {
        let bank = canonical_persistent_bank(bank).ok_or(StoryWriteError::NonPersistentBank(bank))?;
        let high = address.checked_add(1).ok_or(StoryWriteError::WordCrossesBank)?;
        let [lo, hi] = value.to_le_bytes();
        let mut next = self.state.clone();
        let low = assign(&mut next, bank, address, lo, 255)?;
        let high = assign(&mut next, bank, high, hi, 255)?;
        let changed = next != self.state;
        self.state = next;
        self.events.extend([NativeStoryEvent::Assignment(low), NativeStoryEvent::Assignment(high)]);
        Ok(changed)
    }

    /// Evaluate against the current overlay, then append one semantic event. Its internal
    /// raw assignments must not also be passed to `write_byte` by the interpreter.
    pub fn party(&mut self, op: NativePartyOp) -> Result<NativePartyEffect, NativeStoryError> {
        let effect = evaluate_native_party(&self.state, op)?;
        let mut next = self.state.clone();
        effect.apply_to(&mut next)?;
        self.state = next;
        self.events.push(NativeStoryEvent::Party(effect));
        Ok(effect)
    }

    /// Capture an immutable boundary, without consuming the live interpreter state.
    pub fn snapshot(&self) -> NativeStoryJournal {
        NativeStoryJournal { before: self.before.clone(), after: self.state.clone(), events: self.events.clone() }
    }

    pub fn finish(self) -> NativeStoryJournal {
        NativeStoryJournal { before: self.before, after: self.state, events: self.events }
    }
}

fn byte(state: &StoryState, bank: u8, address: u8) -> (u8, u8) {
    (state.var(bank, address), state.known_mask(bank, address))
}

fn assign(state: &mut StoryState, bank: u8, address: u8, value: u8, mask: u8) -> Result<NativeStoryAssignment, NativeStoryError> {
    let before = byte(state, bank, address);
    state.write_masked(bank, address, value, mask)?;
    Ok(NativeStoryAssignment { bank, address, value: value & mask, mask, before, after: byte(state, bank, address) })
}

#[cfg(test)]
mod tests;
