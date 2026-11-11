//! Checked execution over an explicitly supplied field-scratch image. This layer does not
//! choose field-entry defaults, run initializers, accept trigger seeds, or establish a visit.
//! It records only authored assignments and refuses export after any failed read or write.

use super::{access, FieldScratch, FieldScratchError, FIELD_SCRATCH_BYTES};
use std::cell::Cell;

/// An immutable scratch afterimage, separate from persistent story/resource effects. Actual
/// assigned masks survive unchanged writes; `any_change` also remembers changes later undone.
/// Only a successful checked run can construct it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldScratchOutcome {
    before: FieldScratch,
    after: FieldScratch,
    assigned_masks: [u8; FIELD_SCRATCH_BYTES],
    any_change: bool,
}

impl FieldScratchOutcome {
    pub fn before(&self) -> &FieldScratch { &self.before }
    pub fn after(&self) -> &FieldScratch { &self.after }
    pub fn assigned_masks(&self) -> &[u8; FIELD_SCRATCH_BYTES] { &self.assigned_masks }
    pub fn changed(&self) -> bool { self.before != self.after }
    pub fn any_change(&self) -> bool { self.any_change }

    /// Apply only to the exact initial values and knowledge. A durable visit operation must
    /// additionally compare and commit its story, resources and visit metadata atomically;
    /// this in-memory helper is not a replacement for that transaction.
    pub fn apply_to(&self, state: &mut FieldScratch, expected: &FieldScratch) -> Result<bool, FieldScratchError> {
        if state != expected || expected != &self.before { return Err(FieldScratchError::StalePredecessor); }
        let valid = self.before.values().iter().zip(self.before.known_masks())
            .zip(self.after.values().iter().zip(self.after.known_masks())).zip(&self.assigned_masks)
            .all(|(((&before, &known_before), (&after, &known_after)), &assigned)| {
                known_after == (known_before | assigned) && (before ^ after) & !assigned == 0
            });
        let changed = self.changed();
        if !valid || (changed && !self.any_change)
            || (self.any_change && self.assigned_masks.iter().all(|&mask| mask == 0)) {
            return Err(FieldScratchError::InvalidOutcome);
        }
        *state = self.after.clone();
        Ok(changed)
    }
}

/// One opt-in checked execution segment. Constructing it never clears memory or converts
/// unknown bytes into zeroes. Reads take `&self`; a private first-fault cell lets interpreters
/// audit their existing immutable read paths without exposing mutable scratch storage.
///
/// Once faulted, every later read/write returns that first error and changes nothing. Neither
/// cloning, inspecting the current image, nor taking an outcome can repair the failed run.
/// Clone to stage a multiwrite instruction, then publish that staged run only on success.
#[derive(Debug, Clone)]
pub struct FieldScratchRun {
    before: FieldScratch,
    current: FieldScratch,
    assigned_masks: [u8; FIELD_SCRATCH_BYTES],
    any_change: bool,
    fault: Cell<Option<FieldScratchError>>,
}

impl FieldScratchRun {
    pub fn new(before: &FieldScratch) -> Self {
        Self { before: before.clone(), current: before.clone(), assigned_masks: [0; FIELD_SCRATCH_BYTES],
            any_change: false, fault: Cell::new(None) }
    }

    pub fn before(&self) -> &FieldScratch { &self.before }
    /// A diagnostic/read-only image. Instruction reads must use this run's checked methods
    /// so an unknown access cannot escape its fault audit.
    pub fn current(&self) -> &FieldScratch { &self.current }
    pub fn assigned_masks(&self) -> &[u8; FIELD_SCRATCH_BYTES] { &self.assigned_masks }
    pub fn changed(&self) -> bool { self.before != self.current }
    pub fn any_change(&self) -> bool { self.any_change }
    pub fn fault(&self) -> Option<FieldScratchError> { self.fault.get() }

    fn ready(&self) -> Result<(), FieldScratchError> {
        match self.fault() { Some(error) => Err(error), None => Ok(()) }
    }

    fn checked<T>(&self, result: Result<T, FieldScratchError>) -> Result<T, FieldScratchError> {
        if let Err(error) = &result {
            if self.fault.get().is_none() { self.fault.set(Some(*error)); }
        }
        result
    }

    pub fn read_u8(&self, selector: u8, address: usize) -> Result<u8, FieldScratchError> {
        self.read_masked_u8(selector, address, 255)
    }

    pub fn read_masked_u8(&self, selector: u8, address: usize, mask: u8) -> Result<u8, FieldScratchError> {
        self.ready()?;
        self.checked(self.current.read_masked_u8(selector, address, mask))
    }

    pub fn read_u16(&self, selector: u8, address: usize) -> Result<u16, FieldScratchError> {
        self.read_masked_u16(selector, address, u16::MAX)
    }

    pub fn read_masked_u16(&self, selector: u8, address: usize, mask: u16) -> Result<u16, FieldScratchError> {
        self.ready()?;
        self.checked(self.current.read_masked_u16(selector, address, mask))
    }

    pub fn read_i16(&self, selector: u8, address: usize) -> Result<i16, FieldScratchError> {
        self.read_u16(selector, address).map(|value| value as i16)
    }

    pub fn write_u8(&mut self, selector: u8, address: usize, value: u8) -> Result<bool, FieldScratchError> {
        self.write_masked_u8(selector, address, value, 255)
    }

    pub fn write_masked_u8(&mut self, selector: u8, address: usize, value: u8, mask: u8) -> Result<bool, FieldScratchError> {
        self.ready()?;
        // Validate and stage all state before publication, including the assignment mask.
        let range = self.checked(access(selector, address, false))?;
        let mut next = self.current.clone();
        let changed = self.checked(next.write_masked_u8(selector, address, value, mask))?;
        let mut assigned = self.assigned_masks;
        let slots = self.checked(assigned.get_mut(range).ok_or(FieldScratchError::OutOfBounds { address, bytes: 1 }))?;
        for slot in slots { *slot |= mask; }
        self.current = next;
        self.assigned_masks = assigned;
        self.any_change |= changed;
        Ok(changed)
    }

    pub fn write_u16(&mut self, selector: u8, address: usize, value: u16) -> Result<bool, FieldScratchError> {
        self.ready()?;
        let range = self.checked(access(selector, address, true))?;
        let bytes = (range.end - range.start) as u8;
        let mut next = self.current.clone();
        let changed = self.checked(next.write_u16(selector, address, value))?;
        let mut assigned = self.assigned_masks;
        let slots = self.checked(assigned.get_mut(range).ok_or(FieldScratchError::OutOfBounds { address, bytes }))?;
        slots.fill(255);
        self.current = next;
        self.assigned_masks = assigned;
        self.any_change |= changed;
        Ok(changed)
    }

    pub fn write_i16(&mut self, selector: u8, address: usize, value: i16) -> Result<bool, FieldScratchError> {
        self.write_u16(selector, address, value as u16)
    }

    /// Export only after a completely fault-free prefix. The returned snapshot does not
    /// change if the interpreter later continues; faults remain sticky in the live run.
    pub fn snapshot(&self) -> Result<FieldScratchOutcome, FieldScratchError> {
        self.ready()?;
        Ok(FieldScratchOutcome { before: self.before.clone(), after: self.current.clone(),
            assigned_masks: self.assigned_masks, any_change: self.any_change })
    }

    pub fn finish(self) -> Result<FieldScratchOutcome, FieldScratchError> {
        self.ready()?;
        Ok(FieldScratchOutcome { before: self.before, after: self.current,
            assigned_masks: self.assigned_masks, any_change: self.any_change })
    }
}

#[cfg(test)]
mod tests;
